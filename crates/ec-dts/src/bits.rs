//! MSB-first bit reader that never panics: reads past the end return zeros
//! and set a flag the parser checks at the same points a C reader would.

pub(crate) struct Bits<'a> {
    buf: &'a [u8],
    pos: usize,
    total: usize,
    pub over: bool,
}

impl<'a> Bits<'a> {
    pub fn new(buf: &'a [u8]) -> Bits<'a> {
        Bits {
            buf,
            pos: 0,
            total: buf.len() * 8,
            over: false,
        }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Bits left; negative once a read went past the end.
    pub fn left(&self) -> isize {
        self.total as isize - self.pos as isize
    }

    /// Peek `n <= 32` bits without consuming them.
    #[inline]
    pub fn peek(&self, n: u32) -> u32 {
        debug_assert!(n <= 32);
        let byte = self.pos >> 3;
        let sh = (self.pos & 7) as u32;
        let mut v: u64 = 0;
        for i in 0..5 {
            v = (v << 8) | self.buf.get(byte + i).copied().unwrap_or(0) as u64;
        }
        let mask = if n == 32 { u32::MAX as u64 } else { (1u64 << n) - 1 };
        ((v >> (40 - sh - n)) & mask) as u32
    }

    #[inline]
    pub fn skip(&mut self, n: usize) {
        self.pos += n;
        if self.pos > self.total {
            self.over = true;
        }
    }

    #[inline]
    pub fn get(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.skip(n as usize);
        v
    }

    #[inline]
    pub fn get1(&mut self) -> u32 {
        self.get(1)
    }

    /// Signed `n`-bit value (two's complement), `1 <= n <= 24`.
    #[inline]
    pub fn get_signed(&mut self, n: u32) -> i32 {
        if n == 0 {
            return 0;
        }
        let v = self.get(n) as i32;
        (v << (32 - n)) >> (32 - n)
    }

    /// Seek to an absolute bit position. False when past the end.
    pub fn seek(&mut self, pos: usize) -> bool {
        if pos > self.total {
            self.over = true;
            return false;
        }
        self.pos = pos;
        true
    }
}
