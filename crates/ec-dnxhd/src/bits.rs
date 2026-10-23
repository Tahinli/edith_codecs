//! MSB-first bit reader over a byte slice with a 64-bit cache. Reads past the
//! end return zeros, like the zero padding the reference decoder relies on.

pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Next byte to load into the cache.
    pos: usize,
    /// Bits `[63 .. 64 - bits]` of `cache` are valid and next in the stream.
    cache: u64,
    bits: u32,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let mut r = BitReader { data, pos: 0, cache: 0, bits: 0 };
        r.refill();
        r
    }

    /// Make at least 56 bits available.
    #[inline(always)]
    pub fn refill(&mut self) {
        if let Some(s) = self.data.get(self.pos..self.pos + 8) {
            let v = u64::from_be_bytes(s.try_into().unwrap());
            self.cache |= v >> self.bits;
            let take = (63 - self.bits) >> 3;
            self.pos += take as usize;
            self.bits += take * 8;
        } else {
            self.refill_slow();
        }
    }

    #[cold]
    fn refill_slow(&mut self) {
        while self.bits <= 56 {
            let b = self.data.get(self.pos).copied().unwrap_or(0);
            self.cache |= u64::from(b) << (56 - self.bits);
            self.pos += 1;
            self.bits += 8;
        }
    }

    /// Peek `n` (1..=32) bits; the caller has refilled.
    #[inline(always)]
    pub fn peek(&self, n: u32) -> u32 {
        (self.cache >> (64 - n)) as u32
    }

    /// Peek all 32 upper bits.
    #[inline(always)]
    pub fn peek32(&self) -> u32 {
        (self.cache >> 32) as u32
    }

    #[inline(always)]
    pub fn skip(&mut self, n: u32) {
        self.cache <<= n;
        self.bits -= n;
    }

    /// Read `n` (1..=32) bits; the caller has refilled.
    #[inline(always)]
    pub fn get(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.skip(n);
        v
    }

    /// True when the cursor has moved beyond the data.
    pub fn overrun(&self) -> bool {
        // Bits consumed = loaded - still cached.
        (self.pos as u64) * 8 - u64::from(self.bits) > self.data.len() as u64 * 8
    }
}
