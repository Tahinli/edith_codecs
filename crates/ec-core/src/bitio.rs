//! Bit-level readers and writers shared by every parser in the family.
//!
//! Two orders: MSB-first ([`BitReader`], [`BitWriter`]) for the MPEG/ITU
//! bitstreams and FLAC, LSB-first ([`BitReaderLsb`], [`BitWriterLsb`]) for the
//! Xiph formats. Readers never panic on truncated input — a short buffer is
//! [`Error::NeedMore`], which is also the streaming "feed me more" contract.

use crate::error::{Error, Result};

/// MSB-first bit reader over a borrowed buffer.
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    pos: u64,
}

impl<'a> BitReader<'a> {
    /// A reader positioned at the first bit of `data`.
    pub fn new(data: &'a [u8]) -> BitReader<'a> {
        BitReader { data, pos: 0 }
    }

    /// Bits consumed so far.
    pub fn bit_position(&self) -> u64 {
        self.pos
    }

    /// Bits left in the buffer.
    pub fn bits_remaining(&self) -> u64 {
        (self.data.len() as u64 * 8).saturating_sub(self.pos)
    }

    /// True when the next bit starts a byte.
    pub fn is_byte_aligned(&self) -> bool {
        self.pos.is_multiple_of(8)
    }

    /// Skip to the next byte boundary; no-op when already aligned.
    pub fn align_to_byte(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }

    /// Read `n` bits (`n <= 64`) into a `u64`.
    ///
    /// Panics if `n > 64` (a coding error, not an input error).
    pub fn read_bits64(&mut self, n: u32) -> Result<u64> {
        assert!(n <= 64, "read_bits64: n = {n} > 64");
        if self.bits_remaining() < n as u64 {
            return Err(Error::NeedMore);
        }
        let mut value = 0u64;
        let mut left = n;
        while left > 0 {
            let byte = self.data[(self.pos >> 3) as usize];
            let bit_off = (self.pos & 7) as u32;
            let take = left.min(8 - bit_off);
            let chunk = (byte << bit_off) >> (8 - take);
            value = (value << take) | chunk as u64;
            self.pos += take as u64;
            left -= take;
        }
        Ok(value)
    }

    /// Read `n` bits (`n <= 32`) into a `u32`.
    pub fn read_bits(&mut self, n: u32) -> Result<u32> {
        assert!(n <= 32, "read_bits: n = {n} > 32");
        Ok(self.read_bits64(n)? as u32)
    }

    /// Read `n` bits as a two's-complement signed value (`1 <= n <= 32`).
    pub fn read_signed(&mut self, n: u32) -> Result<i32> {
        assert!((1..=32).contains(&n), "read_signed: n = {n} outside 1..=32");
        let raw = self.read_bits(n)?;
        Ok(((raw << (32 - n)) as i32) >> (32 - n))
    }

    /// Read one bit.
    pub fn read_bit(&mut self) -> Result<bool> {
        Ok(self.read_bits64(1)? != 0)
    }

    /// Read `n` bits without consuming them.
    pub fn peek_bits(&mut self, n: u32) -> Result<u32> {
        let saved = self.pos;
        let v = self.read_bits(n);
        self.pos = saved;
        v
    }

    /// Skip `n` bits, or [`Error::NeedMore`] if fewer remain.
    pub fn skip_bits(&mut self, n: u64) -> Result<()> {
        if self.bits_remaining() < n {
            return Err(Error::NeedMore);
        }
        self.pos += n;
        Ok(())
    }

    /// Borrow `n` whole bytes from the current position, which must be aligned.
    pub fn read_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if !self.is_byte_aligned() {
            return Err(Error::corrupt("read_bytes at a non-byte-aligned position"));
        }
        let start = (self.pos >> 3) as usize;
        let end = start.checked_add(n).ok_or(Error::NeedMore)?;
        let slice = self.data.get(start..end).ok_or(Error::NeedMore)?;
        self.pos += n as u64 * 8;
        Ok(slice)
    }

    /// Unsigned exp-Golomb `ue(v)`, as used by H.264 and H.265.
    ///
    /// The prefix is capped at 31 zero bits: 32 or more cannot encode a value
    /// in `u32` range, so it is a corrupt bitstream (and, on a fuzzer's input,
    /// the difference between an error and an unbounded loop).
    pub fn read_ue(&mut self) -> Result<u32> {
        let mut zeros = 0u32;
        while !self.read_bit()? {
            zeros += 1;
            if zeros > 31 {
                return Err(Error::corrupt("exp-Golomb prefix longer than 31 zero bits"));
            }
        }
        if zeros == 0 {
            return Ok(0);
        }
        let suffix = self.read_bits(zeros)?;
        Ok((1u32 << zeros) - 1 + suffix)
    }

    /// Signed exp-Golomb `se(v)`.
    pub fn read_se(&mut self) -> Result<i32> {
        let k = self.read_ue()? as i64;
        let v = if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) };
        i32::try_from(v).map_err(|_| Error::corrupt(format!("se(v) out of i32 range: {v}")))
    }
}

/// MSB-first bit writer.
#[derive(Debug, Clone, Default)]
pub struct BitWriter {
    buf: Vec<u8>,
    /// Bits already written into the last byte of `buf` (0 means byte aligned).
    used: u32,
}

impl BitWriter {
    /// An empty writer.
    pub fn new() -> BitWriter {
        BitWriter::default()
    }

    /// An empty writer with room for `bytes`.
    pub fn with_capacity(bytes: usize) -> BitWriter {
        BitWriter {
            buf: Vec::with_capacity(bytes),
            used: 0,
        }
    }

    /// Bits written so far.
    pub fn bit_len(&self) -> u64 {
        if self.used == 0 {
            self.buf.len() as u64 * 8
        } else {
            (self.buf.len() as u64 - 1) * 8 + self.used as u64
        }
    }

    /// True when the next bit would start a byte.
    pub fn is_byte_aligned(&self) -> bool {
        self.used == 0
    }

    /// Write the low `n` bits of `value` (`n <= 64`), most significant first.
    pub fn write_bits64(&mut self, value: u64, n: u32) {
        assert!(n <= 64, "write_bits64: n = {n} > 64");
        for i in (0..n).rev() {
            self.write_bit(value >> i & 1 == 1);
        }
    }

    /// Write the low `n` bits of `value` (`n <= 32`), most significant first.
    pub fn write_bits(&mut self, value: u32, n: u32) {
        assert!(n <= 32, "write_bits: n = {n} > 32");
        self.write_bits64(value as u64, n);
    }

    /// Write a two's-complement signed value in `n` bits (`1 <= n <= 32`).
    pub fn write_signed(&mut self, value: i32, n: u32) {
        assert!(
            (1..=32).contains(&n),
            "write_signed: n = {n} outside 1..=32"
        );
        self.write_bits((value as u32) & (u32::MAX >> (32 - n)), n);
    }

    /// Write one bit.
    pub fn write_bit(&mut self, bit: bool) {
        if self.used == 0 {
            self.buf.push(0);
        }
        if bit {
            let last = self.buf.len() - 1;
            self.buf[last] |= 0x80 >> self.used;
        }
        self.used = (self.used + 1) % 8;
    }

    /// Pad with zero bits up to the next byte boundary.
    pub fn align_to_byte(&mut self) {
        while self.used != 0 {
            self.write_bit(false);
        }
    }

    /// Unsigned exp-Golomb `ue(v)`.
    ///
    /// Panics on `u32::MAX`, which has no exp-Golomb form that reads back into
    /// `u32`; no codec in the family emits it.
    pub fn write_ue(&mut self, value: u32) {
        assert!(value != u32::MAX, "write_ue: u32::MAX is not encodable");
        let x = value as u64 + 1;
        let bits = 64 - x.leading_zeros();
        self.write_bits64(0, bits - 1);
        self.write_bits64(x, bits);
    }

    /// Signed exp-Golomb `se(v)`.
    pub fn write_se(&mut self, value: i32) {
        let k = if value > 0 {
            2 * value as i64 - 1
        } else {
            -2 * value as i64
        };
        let k = u32::try_from(k).expect("se(v) out of exp-Golomb range");
        self.write_ue(k);
    }

    /// Append whole bytes; the writer must be byte aligned.
    ///
    /// Panics when misaligned — call [`BitWriter::align_to_byte`] first.
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        assert!(self.is_byte_aligned(), "write_bytes while not byte aligned");
        self.buf.extend_from_slice(bytes);
    }

    /// The bytes written so far; a trailing partial byte is zero padded.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Consume the writer, returning its bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

/// LSB-first bit reader (Vorbis, and other Xiph-family bitstreams).
#[derive(Debug, Clone)]
pub struct BitReaderLsb<'a> {
    data: &'a [u8],
    pos: u64,
}

impl<'a> BitReaderLsb<'a> {
    /// A reader positioned at the first bit of `data`.
    pub fn new(data: &'a [u8]) -> BitReaderLsb<'a> {
        BitReaderLsb { data, pos: 0 }
    }

    /// Bits consumed so far.
    pub fn bit_position(&self) -> u64 {
        self.pos
    }

    /// Bits left in the buffer.
    pub fn bits_remaining(&self) -> u64 {
        (self.data.len() as u64 * 8).saturating_sub(self.pos)
    }

    /// True when the next bit starts a byte.
    pub fn is_byte_aligned(&self) -> bool {
        self.pos.is_multiple_of(8)
    }

    /// Skip to the next byte boundary; no-op when already aligned.
    pub fn align_to_byte(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }

    /// Read `n` bits (`n <= 64`), least significant bit first.
    pub fn read_bits64(&mut self, n: u32) -> Result<u64> {
        assert!(n <= 64, "read_bits64: n = {n} > 64");
        if self.bits_remaining() < n as u64 {
            return Err(Error::NeedMore);
        }
        let mut value = 0u64;
        let mut got = 0u32;
        while got < n {
            let byte = self.data[(self.pos >> 3) as usize];
            let bit_off = (self.pos & 7) as u32;
            let take = (n - got).min(8 - bit_off);
            let mask = ((1u16 << take) - 1) as u8;
            let chunk = (byte >> bit_off) & mask;
            value |= (chunk as u64) << got;
            self.pos += take as u64;
            got += take;
        }
        Ok(value)
    }

    /// Read `n` bits (`n <= 32`), least significant bit first.
    pub fn read_bits(&mut self, n: u32) -> Result<u32> {
        assert!(n <= 32, "read_bits: n = {n} > 32");
        Ok(self.read_bits64(n)? as u32)
    }

    /// Read one bit.
    pub fn read_bit(&mut self) -> Result<bool> {
        Ok(self.read_bits64(1)? != 0)
    }

    /// Skip `n` bits, or [`Error::NeedMore`] if fewer remain.
    pub fn skip_bits(&mut self, n: u64) -> Result<()> {
        if self.bits_remaining() < n {
            return Err(Error::NeedMore);
        }
        self.pos += n;
        Ok(())
    }
}

/// LSB-first bit writer.
#[derive(Debug, Clone, Default)]
pub struct BitWriterLsb {
    buf: Vec<u8>,
    used: u32,
}

impl BitWriterLsb {
    /// An empty writer.
    pub fn new() -> BitWriterLsb {
        BitWriterLsb::default()
    }

    /// Bits written so far.
    pub fn bit_len(&self) -> u64 {
        if self.used == 0 {
            self.buf.len() as u64 * 8
        } else {
            (self.buf.len() as u64 - 1) * 8 + self.used as u64
        }
    }

    /// True when the next bit would start a byte.
    pub fn is_byte_aligned(&self) -> bool {
        self.used == 0
    }

    /// Write the low `n` bits of `value` (`n <= 64`), least significant first.
    pub fn write_bits64(&mut self, value: u64, n: u32) {
        assert!(n <= 64, "write_bits64: n = {n} > 64");
        for i in 0..n {
            self.write_bit(value >> i & 1 == 1);
        }
    }

    /// Write the low `n` bits of `value` (`n <= 32`), least significant first.
    pub fn write_bits(&mut self, value: u32, n: u32) {
        assert!(n <= 32, "write_bits: n = {n} > 32");
        self.write_bits64(value as u64, n);
    }

    /// Write one bit.
    pub fn write_bit(&mut self, bit: bool) {
        if self.used == 0 {
            self.buf.push(0);
        }
        if bit {
            let last = self.buf.len() - 1;
            self.buf[last] |= 1 << self.used;
        }
        self.used = (self.used + 1) % 8;
    }

    /// Pad with zero bits up to the next byte boundary.
    pub fn align_to_byte(&mut self) {
        while self.used != 0 {
            self.write_bit(false);
        }
    }

    /// The bytes written so far; a trailing partial byte is zero padded.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Consume the writer, returning its bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msb_round_trip_mixed_widths() {
        let widths = [1u32, 2, 3, 5, 7, 8, 9, 13, 16, 17, 24, 31, 32];
        let mut w = BitWriter::new();
        let mut values = Vec::new();
        for (i, &n) in widths.iter().enumerate() {
            let v = if n == 32 {
                u32::MAX
            } else {
                ((0xDEAD_BEEFu64 >> i) as u32) & (u32::MAX >> (32 - n))
            };
            values.push(v);
            w.write_bits(v, n);
        }
        let bits = w.bit_len();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        for (&n, &v) in widths.iter().zip(&values) {
            assert_eq!(r.read_bits(n).unwrap(), v, "width {n}");
        }
        assert_eq!(r.bit_position(), bits);
    }

    #[test]
    fn msb_bit_order_is_most_significant_first() {
        let mut w = BitWriter::new();
        w.write_bits(0b101, 3);
        w.align_to_byte();
        assert_eq!(w.as_bytes(), &[0b1010_0000]);
        let mut r = BitReader::new(&[0b1010_0000]);
        assert!(r.read_bit().unwrap());
        assert!(!r.read_bit().unwrap());
        assert!(r.read_bit().unwrap());
        assert!(!r.is_byte_aligned());
        r.align_to_byte();
        assert!(r.is_byte_aligned());
        assert_eq!(r.bits_remaining(), 0);
    }

    #[test]
    fn exp_golomb_round_trip_including_edges() {
        // 0 is a single '1' bit; the top of the range is 31 zeros + suffix.
        let values = [
            0u32,
            1,
            2,
            3,
            255,
            256,
            65_535,
            (1 << 31) - 2,
            (1 << 31) - 1,
            u32::MAX - 2,
            u32::MAX - 1,
        ];
        for &v in &values {
            let mut w = BitWriter::new();
            w.write_ue(v);
            let bits = w.bit_len();
            let bytes = w.into_bytes();
            let mut r = BitReader::new(&bytes);
            assert_eq!(r.read_ue().unwrap(), v, "ue({v})");
            assert_eq!(r.bit_position(), bits, "ue({v}) length");
        }
        let mut w = BitWriter::new();
        w.write_ue(0);
        w.align_to_byte();
        assert_eq!(w.as_bytes(), &[0b1000_0000]);
    }

    #[test]
    fn signed_exp_golomb_round_trip() {
        for v in [
            0i32,
            1,
            -1,
            2,
            -2,
            1000,
            -1000,
            i32::MAX / 2,
            -(i32::MAX / 2),
        ] {
            let mut w = BitWriter::new();
            w.write_se(v);
            let bytes = w.into_bytes();
            assert_eq!(BitReader::new(&bytes).read_se().unwrap(), v, "se({v})");
        }
    }

    #[test]
    fn truncation_is_need_more_never_panic() {
        let mut r = BitReader::new(&[0xFF, 0xFF]);
        assert!(r.read_bits(16).is_ok());
        assert!(r.read_bits(1).unwrap_err().is_need_more());
        assert!(BitReader::new(&[]).read_bit().unwrap_err().is_need_more());
        assert!(
            BitReader::new(&[0x80])
                .read_bits(9)
                .unwrap_err()
                .is_need_more()
        );
        // A ue() whose suffix is cut off, and one with no terminating 1 bit.
        assert!(
            BitReader::new(&[0x01])
                .read_ue()
                .unwrap_err()
                .is_need_more()
        );
        assert!(
            BitReader::new(&[0x00, 0x00])
                .read_ue()
                .unwrap_err()
                .is_need_more()
        );
        // 32+ zero bits before the 1: corrupt, not an infinite scan.
        let long = [0u8, 0, 0, 0, 0x80];
        let err = BitReader::new(&long).read_ue().unwrap_err();
        assert!(matches!(err, Error::Corrupt { .. }), "{err}");
    }

    #[test]
    fn signed_fixed_width_and_bytes() {
        let mut w = BitWriter::new();
        w.write_signed(-3, 8);
        w.write_signed(7, 5);
        w.align_to_byte();
        w.write_bytes(&[0xAB, 0xCD]);
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_signed(8).unwrap(), -3);
        assert_eq!(r.read_signed(5).unwrap(), 7);
        r.align_to_byte();
        assert_eq!(r.read_bytes(2).unwrap(), &[0xAB, 0xCD]);
        assert!(r.read_bytes(1).unwrap_err().is_need_more());
        // Misaligned byte reads are refused rather than silently realigned.
        let mut r = BitReader::new(&bytes);
        r.read_bit().unwrap();
        assert!(matches!(r.read_bytes(1), Err(Error::Corrupt { .. })));
    }

    #[test]
    fn peek_and_skip_do_not_diverge() {
        let mut r = BitReader::new(&[0b1011_0010, 0xFF]);
        assert_eq!(r.peek_bits(4).unwrap(), 0b1011);
        assert_eq!(r.bit_position(), 0);
        r.skip_bits(4).unwrap();
        assert_eq!(r.read_bits(4).unwrap(), 0b0010);
        assert!(r.skip_bits(9).is_err());
        assert_eq!(r.bits_remaining(), 8);
    }

    #[test]
    fn lsb_round_trip_and_order() {
        let mut w = BitWriterLsb::new();
        w.write_bits(0b101, 3);
        w.align_to_byte();
        assert_eq!(w.as_bytes(), &[0b0000_0101]);

        let widths = [1u32, 4, 7, 11, 16, 32];
        let mut w = BitWriterLsb::new();
        let mut values = Vec::new();
        for (i, &n) in widths.iter().enumerate() {
            let v = ((0xCAFE_F00Du64 >> i) as u32) & (u32::MAX >> (32 - n));
            values.push(v);
            w.write_bits(v, n);
        }
        let bytes = w.into_bytes();
        let mut r = BitReaderLsb::new(&bytes);
        for (&n, &v) in widths.iter().zip(&values) {
            assert_eq!(r.read_bits(n).unwrap(), v, "lsb width {n}");
        }
        assert_eq!(r.bits_remaining(), 1); // 71 bits written, padded to 9 bytes
    }

    #[test]
    fn lsb_truncation_is_need_more() {
        let mut r = BitReaderLsb::new(&[0x0F]);
        assert_eq!(r.read_bits(4).unwrap(), 0x0F);
        assert!(r.read_bits(5).unwrap_err().is_need_more());
        assert!(r.skip_bits(5).is_err());
        r.align_to_byte();
        assert_eq!(r.bits_remaining(), 0);
    }
}
