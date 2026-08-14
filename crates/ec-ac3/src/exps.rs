//! Exponent decoding (A/52 §7.1.3).
//!
//! Exponents travel as 7-bit groups of three differential values, each of which
//! covers one, two or four mantissas depending on the strategy. A group whose
//! mapped value exceeds 124, or a running exponent outside 0..=24, is a corrupt
//! stream rather than a clamp: the bit allocation that follows would otherwise
//! read the rest of the block at the wrong offsets and call it audio.

use ec_core::{BitReader, Error, Result};

/// Exponent strategy (Tables 7.4, 7.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Strategy {
    /// Keep the previous block's exponents for this channel.
    #[default]
    Reuse,
    /// One exponent per mantissa.
    D15,
    /// One exponent per pair.
    D25,
    /// One exponent per quad.
    D45,
}

impl Strategy {
    /// From the 2-bit `chexpstr`/`cplexpstr` code.
    pub fn from_code(code: u32) -> Strategy {
        match code & 3 {
            0 => Strategy::Reuse,
            1 => Strategy::D15,
            2 => Strategy::D25,
            _ => Strategy::D45,
        }
    }

    /// Mantissas covered by one differential exponent: 1, 2 or 4.
    pub fn group_size(self) -> usize {
        match self {
            Strategy::Reuse => 0,
            Strategy::D15 => 1,
            Strategy::D25 => 2,
            Strategy::D45 => 4,
        }
    }

    /// Number of 7-bit groups for a full-bandwidth or coupled channel whose
    /// mantissas end at `endmant` (§7.1.3), not counting the absolute exponent.
    pub fn fbw_groups(self, endmant: usize) -> usize {
        match self {
            Strategy::Reuse => 0,
            Strategy::D15 => endmant.saturating_sub(1) / 3,
            Strategy::D25 => (endmant + 2) / 6,
            Strategy::D45 => (endmant + 8) / 12,
        }
    }

    /// Number of 7-bit groups for the coupling channel over `[start, end)`.
    pub fn coupling_groups(self, start: usize, end: usize) -> usize {
        let span = end.saturating_sub(start);
        match self {
            Strategy::Reuse => 0,
            Strategy::D15 => span / 3,
            Strategy::D25 => span / 6,
            Strategy::D45 => span / 12,
        }
    }
}

/// Read `ngrps` groups and expand them into `exps`, starting at `first_bin`.
///
/// `previous` is the absolute exponent the first differential is applied to:
/// `exps[ch][0]` for a full-bandwidth or LFE channel (which the caller has
/// already stored), or `cplabsexp << 1` for the coupling channel.
pub fn decode(
    r: &mut BitReader<'_>,
    strategy: Strategy,
    ngrps: usize,
    previous: u8,
    first_bin: usize,
    exps: &mut [u8],
) -> Result<()> {
    let group_size = strategy.group_size();
    if group_size == 0 {
        return Ok(());
    }
    let mut prev = i32::from(previous);
    let mut bin = first_bin;
    for _ in 0..ngrps {
        let gexp = r.read_bits(7)?;
        if gexp > 124 {
            return Err(Error::corrupt(format!(
                "AC-3 exponents: grouped value {gexp} > 124"
            )));
        }
        for mapped in [gexp / 25, (gexp % 25) / 5, gexp % 5] {
            prev += mapped as i32 - 2;
            if !(0..=24).contains(&prev) {
                return Err(Error::corrupt(format!(
                    "AC-3 exponents: absolute exponent {prev} outside 0..=24"
                )));
            }
            for _ in 0..group_size {
                if bin >= exps.len() {
                    // The last group of a D25/D45 set can overrun the channel's
                    // own bandwidth by design; those values are not used.
                    break;
                }
                exps[bin] = prev as u8;
                bin += 1;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_core::BitWriter;

    #[test]
    fn d15_expands_one_exponent_per_mantissa() {
        // Two groups: deltas (+2, 0, -1) and (0, +1, +1) from an absolute 5.
        let mut w = BitWriter::new();
        w.write_bits(4 * 25 + 2 * 5 + 1, 7);
        w.write_bits(2 * 25 + 3 * 5 + 3, 7);
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let mut exps = [0u8; 16];
        exps[0] = 5;
        decode(&mut r, Strategy::D15, 2, 5, 1, &mut exps).unwrap();
        assert_eq!(&exps[..7], &[5, 7, 7, 6, 6, 7, 8]);
    }

    #[test]
    fn d45_repeats_each_exponent_four_times() {
        let mut w = BitWriter::new();
        w.write_bits(3 * 25 + 2 * 5 + 2, 7); // +1, 0, 0
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let mut exps = [0u8; 16];
        exps[0] = 10;
        decode(&mut r, Strategy::D45, 1, 10, 1, &mut exps).unwrap();
        assert_eq!(
            &exps[..13],
            &[10, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11]
        );
    }

    #[test]
    fn out_of_range_exponents_are_corrupt_not_clamped() {
        let mut w = BitWriter::new();
        w.write_bits(4 * 25 + 4 * 5 + 4, 7); // +2, +2, +2 from 23 => 29
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let mut exps = [0u8; 16];
        assert!(matches!(
            decode(&mut r, Strategy::D15, 1, 23, 1, &mut exps),
            Err(Error::Corrupt { .. })
        ));
        // And a truncated group is NeedMore, never a panic.
        let mut r = BitReader::new(&[0u8; 0]);
        assert!(matches!(
            decode(&mut r, Strategy::D15, 1, 5, 1, &mut exps),
            Err(Error::NeedMore)
        ));
    }

    #[test]
    fn group_counts_match_the_standards_formulas() {
        // §7.1.3 worked cases: a full-bandwidth channel ending at 253.
        assert_eq!(Strategy::D15.fbw_groups(253), 84);
        assert_eq!(Strategy::D25.fbw_groups(253), 42);
        assert_eq!(Strategy::D45.fbw_groups(253), 21);
        assert_eq!(Strategy::D15.fbw_groups(7), 2);
        assert_eq!(Strategy::D25.coupling_groups(37, 253), 36);
    }
}
