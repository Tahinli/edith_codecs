//! Mantissa unpacking (A/52 §7.3), including the grouped quantizers and the
//! dither the standard puts in place of zero-bit mantissas.

use ec_core::{BitReader, Result};

use crate::tables::{QNTZTAB, QUANT_LEVELS, symmetric_level};

/// The pseudo-random source for `bap == 0` mantissas (§7.3.4).
///
/// The standard asks only for "any reasonably random sequence" scaled to a
/// uniform distribution between ±0.707, so this is the classic 16-bit LFSR the
/// AC-3 reference decoder uses. It is deterministic, which is what makes a
/// decode reproducible from one run to the next.
#[derive(Debug, Clone, Copy, Default)]
pub struct Dither(u32);

impl Dither {
    /// Next dither value, uniform in ±0.707.
    pub fn value(&mut self) -> f32 {
        self.uniform() * 0.707
    }

    /// Next value from a zero-mean, unit-variance source — what Annex E
    /// §3.6.4.2.4 calls `noise()`. A uniform distribution on `[-1, 1)` has
    /// variance 1/3, so it is scaled by `sqrt(3)`.
    pub fn unit_variance(&mut self) -> f32 {
        self.uniform() * 1.732_050_8
    }

    fn uniform(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(25173).wrapping_add(13849) & 0xffff;
        f32::from(self.0 as u16 as i16) * (1.0 / 32768.0)
    }
}

/// Reads one exponent set's mantissas, holding the two-and-three value groups
/// that baps 1, 2 and 4 pack into a single code word.
///
/// A reader belongs to one channel's run of mantissas: the standard restarts
/// grouping at each exponent set, so a partly consumed group is dropped when
/// the next channel begins.
#[derive(Debug, Clone, Copy, Default)]
pub struct Mantissas {
    group3: [f32; 3],
    left3: usize,
    group5: [f32; 3],
    left5: usize,
    group11: [f32; 2],
    left11: usize,
}

impl Mantissas {
    /// A reader with no group in flight.
    pub fn new() -> Mantissas {
        Mantissas::default()
    }

    /// One mantissa value in `[-1, 1)`, before the exponent is applied.
    ///
    /// `bap == 0` returns 0.0; the caller decides whether that bin gets dither,
    /// because in the coupled range the decision belongs to the channel rather
    /// than to the coupling channel the mantissa came from.
    pub fn read(&mut self, r: &mut BitReader<'_>, bap: u8) -> Result<f32> {
        Ok(match bap {
            0 => 0.0,
            1 => {
                if self.left3 == 0 {
                    let code = r.read_bits(5)?.min(26);
                    let levels = QUANT_LEVELS[1];
                    self.group3 = [
                        symmetric_level(levels, code / 9),
                        symmetric_level(levels, (code % 9) / 3),
                        symmetric_level(levels, code % 3),
                    ];
                    self.left3 = 3;
                }
                self.left3 -= 1;
                self.group3[2 - self.left3]
            }
            2 => {
                if self.left5 == 0 {
                    let code = r.read_bits(7)?.min(124);
                    let levels = QUANT_LEVELS[2];
                    self.group5 = [
                        symmetric_level(levels, code / 25),
                        symmetric_level(levels, (code % 25) / 5),
                        symmetric_level(levels, code % 5),
                    ];
                    self.left5 = 3;
                }
                self.left5 -= 1;
                self.group5[2 - self.left5]
            }
            4 => {
                if self.left11 == 0 {
                    let code = r.read_bits(7)?.min(120);
                    let levels = QUANT_LEVELS[4];
                    self.group11 = [
                        symmetric_level(levels, code / 11),
                        symmetric_level(levels, code % 11),
                    ];
                    self.left11 = 2;
                }
                self.left11 -= 1;
                self.group11[1 - self.left11]
            }
            3 | 5 => {
                let bits = QNTZTAB[bap as usize];
                let code = r.read_bits(bits)?;
                symmetric_level(QUANT_LEVELS[bap as usize], code)
            }
            _ => {
                // 6..=15: asymmetric two's complement, decimal point left of
                // the MSB (§7.3.2).
                let bits = QNTZTAB[(bap as usize).min(15)];
                let value = r.read_signed(bits)?;
                value as f32 / (1u32 << (bits - 1)) as f32
            }
        })
    }
}

/// `2^-exponent`, the right shift §7.3 applies to every mantissa.
pub fn scale(exponent: u8) -> f32 {
    // Exponents are 0..=24, so a table lookup would buy nothing over the
    // exponent arithmetic the compiler folds here.
    f32::from_bits(127u32.wrapping_sub(u32::from(exponent)) << 23)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_core::BitWriter;

    #[test]
    fn grouped_quantizers_unpack_three_and_two_at_a_time() {
        // One bap-1 group holding codes (2, 1, 0) => (+2/3, 0, -2/3), then one
        // bap-4 group holding (10, 0) => (+10/11, -10/11).
        let mut w = BitWriter::new();
        w.write_bits(2 * 9 + 3, 5);
        w.write_bits(10 * 11, 7);
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let mut m = Mantissas::new();
        let got: Vec<f32> = (0..3).map(|_| m.read(&mut r, 1).unwrap()).collect();
        assert!((got[0] - 2.0 / 3.0).abs() < 1e-6);
        assert!(got[1].abs() < 1e-6);
        assert!((got[2] + 2.0 / 3.0).abs() < 1e-6);
        let a = m.read(&mut r, 4).unwrap();
        let b = m.read(&mut r, 4).unwrap();
        assert!((a - 10.0 / 11.0).abs() < 1e-6);
        assert!((b + 10.0 / 11.0).abs() < 1e-6);
    }

    #[test]
    fn asymmetric_mantissas_span_minus_one_to_just_under_one() {
        let mut w = BitWriter::new();
        w.write_bits(0b100000, 6); // most negative 6-bit word
        w.write_bits(0b011111, 6); // most positive
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let mut m = Mantissas::new();
        assert!((m.read(&mut r, 7).unwrap() + 1.0).abs() < 1e-6);
        assert!((m.read(&mut r, 7).unwrap() - 31.0 / 32.0).abs() < 1e-6);
        assert_eq!(m.read(&mut r, 0).unwrap(), 0.0);
    }

    #[test]
    fn exponent_scale_is_a_power_of_two() {
        assert_eq!(scale(0), 1.0);
        assert_eq!(scale(1), 0.5);
        assert_eq!(scale(24), 1.0 / 16_777_216.0);
    }

    #[test]
    fn dither_stays_inside_its_stated_range_and_repeats() {
        let mut d = Dither::default();
        let first: Vec<f32> = (0..64).map(|_| d.value()).collect();
        assert!(first.iter().all(|v| v.abs() <= 0.707));
        assert!(first.iter().any(|v| v.abs() > 0.1));
        let mut d2 = Dither::default();
        assert_eq!(first, (0..64).map(|_| d2.value()).collect::<Vec<_>>());
    }
}
