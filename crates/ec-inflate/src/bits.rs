//! LSB-first bit buffer sized for table-driven Huffman decoding.
//!
//! DEFLATE reads its bits low-end first and its Huffman codes high-bit first,
//! so decoding wants to *look* at the next fifteen bits before knowing how many
//! of them belong to the code. [`ec_core::BitReaderLsb`] has no peek and refills
//! one bit at a time, so this is its own accumulator: up to 64 bits in a `u64`,
//! refilled a byte at a time, peeked without consuming.
//!
//! Past the end of the input the buffer reads as zeros; a code that would need
//! those zeros fails at [`Bits::consume`] with [`Error::Truncated`]. Nothing
//! here ever indexes out of bounds or panics on short input.

use crate::Error;

pub(crate) struct Bits<'a> {
    src: &'a [u8],
    /// Index of the next byte to pull into `buf`.
    pos: usize,
    /// The lowest `count` bits are real; everything above is zero padding.
    buf: u64,
    count: u32,
}

impl<'a> Bits<'a> {
    pub(crate) fn new(src: &'a [u8]) -> Bits<'a> {
        Bits {
            src,
            pos: 0,
            buf: 0,
            count: 0,
        }
    }

    /// Top the accumulator up to at least 57 bits, input permitting.
    ///
    /// Away from the end of the input that is one unaligned 64-bit load: the
    /// eight bytes are OR'd in above what is already there and the cursor moves
    /// by however many of them fit, which is the whole cost of a symbol for
    /// most symbols.
    #[inline]
    pub(crate) fn refill(&mut self) {
        if let Some(next) = self.src.get(self.pos..self.pos + 8) {
            let bytes = u64::from_le_bytes(next.try_into().expect("an eight byte slice"));
            self.buf |= bytes << self.count;
            self.pos += ((63 - self.count) >> 3) as usize;
            self.count |= 56;
            return;
        }
        while self.count <= 56 && self.pos < self.src.len() {
            self.buf |= (self.src[self.pos] as u64) << self.count;
            self.pos += 1;
            self.count += 8;
        }
    }

    /// The next `n` bits (`n <= 32`), zero-padded past end of input.
    #[inline]
    pub(crate) fn peek(&self, n: u32) -> u32 {
        self.peek_at(0, n)
    }

    /// The `n` bits sitting `skip` bits ahead (`skip + n <= 32`).
    #[inline]
    pub(crate) fn peek_at(&self, skip: u32, n: u32) -> u32 {
        ((self.buf >> skip) & ((1u64 << n) - 1)) as u32
    }

    /// Drop `n` bits already accounted for by a peek.
    #[inline]
    pub(crate) fn consume(&mut self, n: u32) -> Result<(), Error> {
        if n > self.count {
            return Err(Error::Truncated);
        }
        self.buf >>= n;
        self.count -= n;
        Ok(())
    }

    /// Read `n` bits (`n <= 32`) as a value.
    #[inline]
    pub(crate) fn take(&mut self, n: u32) -> Result<u32, Error> {
        if n == 0 {
            return Ok(0);
        }
        self.refill();
        let value = self.peek(n);
        self.consume(n)?;
        Ok(value)
    }

    /// Drop the partial byte and hand back the byte offset now current.
    ///
    /// The accumulator is emptied, so the caller can read whole bytes straight
    /// out of the source slice (stored blocks, the zlib trailer).
    pub(crate) fn align(&mut self) -> usize {
        let whole = (self.count / 8) as usize;
        self.buf = 0;
        self.count = 0;
        self.pos -= whole;
        self.pos
    }

    /// Move the byte cursor after a direct read from the source slice.
    pub(crate) fn seek(&mut self, pos: usize) {
        debug_assert_eq!(self.count, 0, "seek on an unaligned reader");
        self.pos = pos;
    }

    pub(crate) fn src(&self) -> &'a [u8] {
        self.src
    }
}
