//! MSB-first bit reader over a byte slice. Reads past the end return zeros;
//! callers check [`Br::left`] to detect over-reads.

#[derive(Clone)]
pub struct Br<'a> {
    d: &'a [u8],
    pos: usize,
    end: usize,
}

impl<'a> Br<'a> {
    pub fn new(d: &'a [u8]) -> Br<'a> {
        Br {
            d,
            pos: 0,
            end: d.len() * 8,
        }
    }

    #[inline]
    pub fn pos(&self) -> usize {
        self.pos
    }

    #[inline]
    pub fn set_pos(&mut self, p: usize) {
        self.pos = p;
    }

    #[inline]
    pub fn size_bits(&self) -> usize {
        self.end
    }

    /// Bits left; negative after an over-read.
    #[inline]
    pub fn left(&self) -> isize {
        self.end as isize - self.pos as isize
    }

    /// Peek `n` (0..=32) bits without consuming them.
    #[inline]
    pub fn peek(&self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let byte = self.pos >> 3;
        let sh = (self.pos & 7) as u32;
        let v: u64 = if byte + 8 <= self.d.len() {
            u64::from_be_bytes(self.d[byte..byte + 8].try_into().unwrap())
        } else {
            let mut b = [0u8; 8];
            if byte < self.d.len() {
                let n = self.d.len() - byte;
                b[..n].copy_from_slice(&self.d[byte..]);
            }
            u64::from_be_bytes(b)
        };
        ((v << sh) >> (64 - n)) as u32
    }

    #[inline]
    pub fn skip(&mut self, n: u32) {
        self.pos += n as usize;
    }

    #[inline]
    pub fn get(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.pos += n as usize;
        v
    }

    #[inline]
    pub fn bit(&mut self) -> u32 {
        self.get(1)
    }

    pub fn align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }

    /// Signed value from `n` bits as ffmpeg's `get_xbits`: a leading 1 means
    /// positive, otherwise `-(2^n - 1) + value`.
    #[inline]
    pub fn xbits(&mut self, n: u32) -> i32 {
        let v = self.get(n) as i32;
        if v >> (n - 1) != 0 {
            v
        } else {
            v - (1 << n) + 1
        }
    }
}
