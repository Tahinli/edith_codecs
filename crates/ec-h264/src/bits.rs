//! Slice-data bit cursor: a 64-bit-cached reader for the CAVLC hot loop.
//!
//! `ec_core::BitReader` is the family-wide, allocation-free contract type and
//! stays the parser for headers. Slice data is different: it is read one to
//! three bits at a time, millions of times per picture, so this cursor keeps
//! the next 32..64 bits left-aligned in a register and refills by whole
//! words. Truncation still surfaces as [`Error::NeedMore`]; the exp-Golomb
//! prefix cap still turns runaway zeros into [`Error::Corrupt`].

use ec_core::error::{Error, Result};

/// Register-cached MSB-first bit reader over a byte slice.
pub struct BitCursor<'a> {
    data: &'a [u8],
    /// Upcoming bits, left-aligned.
    cache: u64,
    /// Valid bit count in `cache`.
    cached: u32,
    /// Next byte of `data` to load into the cache.
    next_byte: usize,
    /// Index of the bit just past the `rbsp_stop_one_bit`, for
    /// `more_rbsp_data()`; `data.len() * 8` when no stop bit exists.
    stop_bit: u64,
}

impl<'a> BitCursor<'a> {
    /// A cursor over `data`, positioned `bit_pos` bits in.
    pub fn new(data: &'a [u8], bit_pos: u64) -> BitCursor<'a> {
        let stop_bit = match data.iter().rposition(|&b| b != 0) {
            Some(i) => i as u64 * 8 + (7 - data[i].trailing_zeros() as u64),
            None => data.len() as u64 * 8,
        };
        let mut c = BitCursor {
            data,
            cache: 0,
            cached: 0,
            next_byte: (bit_pos / 8) as usize,
            stop_bit,
        };
        c.refill();
        let sub = (bit_pos % 8) as u32;
        c.cache <<= sub;
        c.cached = c.cached.saturating_sub(sub);
        c
    }

    /// Bits consumed so far.
    #[inline]
    pub fn bit_position(&self) -> u64 {
        self.next_byte as u64 * 8 - u64::from(self.cached)
    }

    /// True while syntax elements remain before the RBSP stop bit
    /// (spec 7.2 `more_rbsp_data()`).
    #[inline]
    pub fn more_rbsp_data(&self) -> bool {
        self.bit_position() < self.stop_bit
    }

    #[inline]
    fn refill(&mut self) {
        while self.cached <= 56 {
            let Some(&b) = self.data.get(self.next_byte) else {
                break;
            };
            self.cache |= u64::from(b) << (56 - self.cached);
            self.cached += 8;
            self.next_byte += 1;
        }
    }

    /// Bits left in the stream.
    #[inline]
    pub fn bits_remaining(&self) -> u64 {
        (self.data.len() - self.next_byte) as u64 * 8 + u64::from(self.cached)
    }

    /// The next 16 bits left-aligned in a u32, zero-padded past the end.
    #[inline]
    pub fn peek16(&mut self) -> u32 {
        if self.cached < 16 {
            self.refill();
        }
        (self.cache >> 48) as u32 & 0xFFFF
    }

    /// Consume `n` bits (`n <= 16` after a `peek16`; general `n <= 32`).
    #[inline]
    pub fn skip(&mut self, n: u32) -> Result<()> {
        if self.cached < n {
            self.refill();
            if self.cached < n {
                return Err(Error::NeedMore);
            }
        }
        self.cache <<= n;
        self.cached -= n;
        Ok(())
    }

    /// Read one bit.
    #[inline]
    pub fn read_bit(&mut self) -> Result<bool> {
        if self.cached == 0 {
            self.refill();
            if self.cached == 0 {
                return Err(Error::NeedMore);
            }
        }
        let bit = self.cache >> 63 != 0;
        self.cache <<= 1;
        self.cached -= 1;
        Ok(bit)
    }

    /// Read `n` bits (`n <= 32`) MSB-first.
    #[inline]
    pub fn read_bits(&mut self, n: u32) -> Result<u32> {
        debug_assert!(n <= 32);
        if n == 0 {
            return Ok(0);
        }
        if self.cached < n {
            self.refill();
            if self.cached < n {
                return Err(Error::NeedMore);
            }
        }
        let v = (self.cache >> (64 - n)) as u32;
        self.cache <<= n;
        self.cached -= n;
        Ok(v)
    }

    /// Count leading zero bits up to and including the terminating one bit
    /// (level_prefix / exp-Golomb prefix). Capped at `cap` zeros.
    #[inline]
    pub fn read_prefix_zeros(&mut self, cap: u32) -> Result<u32> {
        let mut zeros = 0u32;
        loop {
            if self.cached == 0 {
                self.refill();
                if self.cached == 0 {
                    return Err(Error::NeedMore);
                }
            }
            let lz = (self.cache.leading_zeros()).min(self.cached);
            if lz < self.cached {
                // Found the 1 bit within the cache.
                zeros += lz;
                if zeros > cap {
                    return Err(Error::corrupt("prefix run of zeros too long"));
                }
                self.cache <<= lz + 1;
                self.cached -= lz + 1;
                return Ok(zeros);
            }
            zeros += lz;
            if zeros > cap {
                return Err(Error::corrupt("prefix run of zeros too long"));
            }
            self.cache = 0;
            self.cached = 0;
            self.refill();
            if self.cached == 0 && self.bits_remaining() == 0 {
                return Err(Error::NeedMore);
            }
        }
    }

    /// Unsigned exp-Golomb `ue(v)` (prefix capped at 31 zeros, as in
    /// `ec_core::BitReader::read_ue`).
    #[inline]
    pub fn read_ue(&mut self) -> Result<u32> {
        let zeros = self.read_prefix_zeros(31)?;
        if zeros == 0 {
            return Ok(0);
        }
        let suffix = self.read_bits(zeros)?;
        Ok((1u32 << zeros) - 1 + suffix)
    }

    /// Signed exp-Golomb `se(v)`.
    #[inline]
    pub fn read_se(&mut self) -> Result<i32> {
        let k = self.read_ue()? as i64;
        let v = if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) };
        i32::try_from(v).map_err(|_| Error::corrupt("se(v) out of i32 range"))
    }

    /// Skip to the next byte boundary (I_PCM alignment).
    #[inline]
    pub fn align_to_byte(&mut self) {
        let sub = (self.bit_position() % 8) as u32;
        if sub != 0 {
            let n = 8 - sub;
            self.cache <<= n;
            self.cached -= n.min(self.cached);
        }
    }

    /// Borrow `n` whole bytes at the current (byte-aligned) position.
    pub fn read_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if !self.bit_position().is_multiple_of(8) {
            return Err(Error::corrupt("read_bytes at a non-byte-aligned position"));
        }
        let start = (self.bit_position() / 8) as usize;
        let end = start.checked_add(n).ok_or(Error::NeedMore)?;
        let slice = self.data.get(start..end).ok_or(Error::NeedMore)?;
        // Drop the cache and reposition after the bytes.
        self.cache = 0;
        self.cached = 0;
        self.next_byte = end;
        self.refill();
        Ok(slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_core::BitWriter;

    #[test]
    fn agrees_with_core_bitreader() {
        let mut w = BitWriter::new();
        for v in [0u32, 1, 5, 127, 4096] {
            w.write_ue(v);
        }
        for v in [0i32, -3, 7, -128] {
            w.write_se(v);
        }
        w.write_bits(0b1011, 4);
        w.write_bit(true); // stop bit
        w.align_to_byte();
        let bytes = w.into_bytes();
        let mut c = BitCursor::new(&bytes, 0);
        for v in [0u32, 1, 5, 127, 4096] {
            assert_eq!(c.read_ue().unwrap(), v);
        }
        for v in [0i32, -3, 7, -128] {
            assert_eq!(c.read_se().unwrap(), v);
        }
        assert_eq!(c.read_bits(4).unwrap(), 0b1011);
        assert!(!c.more_rbsp_data());
    }

    #[test]
    fn peek16_pads_and_skip_tracks_position() {
        let data = [0xAB, 0xCD];
        let mut c = BitCursor::new(&data, 4);
        assert_eq!(c.bit_position(), 4);
        assert_eq!(c.peek16(), 0xBCD0);
        c.skip(8).unwrap();
        assert_eq!(c.bit_position(), 12);
        assert_eq!(c.peek16(), 0xD000);
        assert_eq!(c.read_bits(4).unwrap(), 0xD);
        assert!(c.read_bit().is_err());
    }

    #[test]
    fn pcm_bytes_and_alignment() {
        let data = [0b1000_0000, 0x11, 0x22, 0x33];
        let mut c = BitCursor::new(&data, 0);
        assert!(c.read_bit().unwrap());
        c.align_to_byte();
        assert_eq!(c.read_bytes(2).unwrap(), &[0x11, 0x22]);
        assert_eq!(c.read_bits(8).unwrap(), 0x33);
        assert!(c.read_bytes(1).is_err());
    }

    #[test]
    fn long_zero_runs_are_corrupt_or_need_more() {
        // 33 zero bits then a 1: exp-Golomb prefix over the cap.
        let data = [0, 0, 0, 0, 0b0100_0000];
        let mut c = BitCursor::new(&data, 0);
        assert!(matches!(c.read_ue(), Err(Error::Corrupt { .. })));
        // All zeros: NeedMore (no stop bit at all).
        let zeros = [0u8; 4];
        let mut c = BitCursor::new(&zeros, 0);
        assert!(matches!(
            c.read_ue(),
            Err(Error::NeedMore | Error::Corrupt { .. })
        ));
    }
}
