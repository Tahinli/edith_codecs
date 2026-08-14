//! Packet-local bit access with the Vorbis end-of-packet convention.
//!
//! Reading past the end of a Vorbis packet is not an error. The spec's decode
//! loops read until the packet runs out and take zeroes after that, which is
//! how a truncated final packet still decodes to the audio it does hold. So
//! this reader answers zero past the end and raises a sticky flag; the callers
//! that must not act on a zero — Huffman decode, header parse — ask [`Bits::eop`].

use ec_core::{BitReaderLsb, BitWriterLsb};

/// LSB-first reader over one packet.
pub struct Bits<'a> {
    reader: BitReaderLsb<'a>,
    eop: bool,
}

impl<'a> Bits<'a> {
    /// A reader positioned at the first bit of `data`.
    pub fn new(data: &'a [u8]) -> Bits<'a> {
        Bits {
            reader: BitReaderLsb::new(data),
            eop: false,
        }
    }

    /// `n` bits (`n <= 32`), zero once the packet is exhausted.
    pub fn read(&mut self, n: u32) -> u32 {
        match self.reader.read_bits(n) {
            Ok(v) => v,
            Err(_) => {
                self.eop = true;
                0
            }
        }
    }

    /// One bit, false once the packet is exhausted.
    pub fn bit(&mut self) -> bool {
        self.read(1) != 0
    }

    /// True once a read has run past the end of the packet.
    pub fn eop(&self) -> bool {
        self.eop
    }

    /// Bits left before the end of the packet.
    pub fn remaining(&self) -> u64 {
        self.reader.bits_remaining()
    }

    /// The 32-bit float of Vorbis I §9.2.2 — a 21-bit mantissa, a sign and a
    /// biased exponent, and *not* IEEE 754: it is decoded with integer
    /// arithmetic so every decoder agrees on the last bit.
    pub fn float32(&mut self) -> f32 {
        let x = self.read(32);
        float32_unpack(x)
    }
}

/// Vorbis I §9.2.2 float unpack, split out because the encoder packs the
/// inverse of it.
pub fn float32_unpack(x: u32) -> f32 {
    let mantissa = i64::from(x & 0x001f_ffff);
    let sign = x & 0x8000_0000 != 0;
    let exponent = ((x & 0x7fe0_0000) >> 21) as i32;
    let mantissa = if sign { -mantissa } else { mantissa };
    // `exp2` rather than `powi`: the exponent field spans 2^-788..2^235, which
    // only stays exact as a power of two.
    (mantissa as f64 * f64::from(exponent - 788).exp2()) as f32
}

/// The inverse of [`float32_unpack`] for a value an encoder wants to state
/// exactly: mantissa in `[-2^21, 2^21)` scaled by a power of two.
pub fn float32_pack(value: f32) -> u32 {
    if value == 0.0 || !value.is_finite() {
        return 0;
    }
    let sign = value < 0.0;
    let mut mantissa = f64::from(value.abs());
    let mut exponent = 0i32;
    // Normalise into [2^20, 2^21) so the 21-bit mantissa field is fully used.
    while mantissa >= 2_097_152.0 {
        mantissa /= 2.0;
        exponent += 1;
    }
    while mantissa < 1_048_576.0 {
        mantissa *= 2.0;
        exponent -= 1;
    }
    let mantissa = mantissa.round() as u32 & 0x001f_ffff;
    let exponent = (exponent + 788).clamp(0, 0x3ff) as u32;
    (u32::from(sign) << 31) | (exponent << 21) | mantissa
}

/// LSB-first writer over one packet, the mirror of [`Bits`].
#[derive(Default)]
pub struct BitsOut {
    writer: BitWriterLsb,
}

impl BitsOut {
    /// An empty packet.
    pub fn new() -> BitsOut {
        BitsOut::default()
    }

    /// Write the low `n` bits of `value`.
    pub fn write(&mut self, value: u32, n: u32) {
        self.writer.write_bits(value, n);
    }

    /// Write one bit.
    pub fn bit(&mut self, bit: bool) {
        self.writer.write_bit(bit);
    }

    /// Bits written so far.
    pub fn len(&self) -> u64 {
        self.writer.bit_len()
    }

    /// The packet, zero-padded to a byte.
    pub fn finish(self) -> Vec<u8> {
        self.writer.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float32_round_trips_through_the_spec_format() {
        for value in [1.0f32, 0.5, -2.25, 1e-6, 1024.0, -0.0009765625] {
            let packed = float32_pack(value);
            let back = float32_unpack(packed);
            assert!(
                (back - value).abs() <= value.abs() * 1e-6,
                "{value} -> {packed:#x} -> {back}"
            );
        }
        assert_eq!(float32_unpack(0), 0.0);
        // A read past the end is zero with the flag raised, not an error.
        let mut bits = Bits::new(&[0xff]);
        assert_eq!(bits.read(8), 0xff);
        assert_eq!(bits.read(1), 0);
        assert!(bits.eop());
    }
}
