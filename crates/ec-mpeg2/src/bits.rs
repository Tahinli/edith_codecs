//! MSB-first bit reader over a byte slice; reads past the end yield zeros and
//! are reported by [`Bits::overrun`].

use crate::tables::Vlc;

pub(crate) struct Bits<'a> {
    data: &'a [u8],
    /// Bit position from the start of `data`.
    pos: usize,
}

impl<'a> Bits<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Bits<'a> {
        Bits { data, pos: 0 }
    }

    /// The next 32 bits (zero padded past the end).
    #[inline(always)]
    pub(crate) fn peek32(&self) -> u32 {
        let byte = self.pos >> 3;
        let sh = (self.pos & 7) as u32;
        if let Some(chunk) = self.data.get(byte..byte + 8) {
            let mut b = [0u8; 8];
            b.copy_from_slice(chunk);
            ((u64::from_be_bytes(b) << sh) >> 32) as u32
        } else {
            let mut b = [0u8; 8];
            if byte < self.data.len() {
                let n = self.data.len() - byte;
                b[..n].copy_from_slice(&self.data[byte..]);
            }
            ((u64::from_be_bytes(b) << sh) >> 32) as u32
        }
    }

    /// Peek `n` bits, 1 <= n <= 32... (n = 0 returns 0).
    #[inline(always)]
    pub(crate) fn peek(&self, n: u32) -> u32 {
        if n == 0 { 0 } else { self.peek32() >> (32 - n) }
    }

    #[inline(always)]
    pub(crate) fn skip(&mut self, n: u32) {
        self.pos += n as usize;
    }

    #[inline(always)]
    pub(crate) fn read(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.pos += n as usize;
        v
    }

    #[inline(always)]
    pub(crate) fn read1(&mut self) -> bool {
        self.read(1) != 0
    }

    /// True once more bits were consumed than the slice holds.
    #[inline(always)]
    pub(crate) fn overrun(&self) -> bool {
        self.pos > self.data.len() * 8
    }

    /// Decode one symbol; `None` on an invalid prefix.
    #[inline(always)]
    pub(crate) fn vlc(&mut self, v: &Vlc) -> Option<u32> {
        let e = v.table[self.peek(v.bits) as usize];
        let len = e >> 16;
        if len == 0 {
            return None;
        }
        self.pos += len as usize;
        Some(e & 0xFFFF)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_across_bytes_and_pads_with_zeros() {
        let mut b = Bits::new(&[0b1010_0101, 0xFF]);
        assert_eq!(b.read(3), 0b101);
        assert_eq!(b.read(7), 0b0010111);
        assert!(!b.overrun());
        assert_eq!(b.read(10), 0b11_1111_0000);
        assert!(b.overrun());
    }
}
