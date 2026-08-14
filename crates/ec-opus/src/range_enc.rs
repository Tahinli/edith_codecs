//! The Opus range *encoder* (RFC 6716, Section 5.1).
//!
//! The mirror image of [`crate::range::RangeDecoder`], and deliberately so: the
//! two carry the same `rng` through the same symbols, which is what lets the
//! encoder predict every budget decision the decoder will make. Both sides
//! compute their allocation from [`RangeEncoder::tell_frac`] /
//! `RangeDecoder::tell_frac`, so those two functions must agree to the 1/8th
//! bit or the streams desynchronise at the first band.
//!
//! One frame, one encoder: the buffer is sized up front (`storage`), the range
//! coder fills it from the front and the raw bits from the back, and
//! [`RangeEncoder::done`] resolves the collision in the middle the way Section
//! 5.1.5 requires — the range coder's bytes win, the raw bits are the ones that
//! get truncated, and [`RangeEncoder::error`] says whether that happened.

/// Number of bits in the code value.
const CODE_BITS: u32 = 32;
/// Renormalisation threshold: `rng` is kept above `2**23`.
const CODE_TOP: u32 = 1 << (CODE_BITS - 8 - 1);
/// Shift that puts the top byte of `low` in the low 8 bits.
const CODE_SHIFT: u32 = CODE_BITS - 8 - 1;
/// Bits buffered by the raw-bit writer before it spills a byte.
const WINDOW_SIZE: u32 = 32;

/// `ilog(x)`: one plus the index of the most significant set bit, `0` for `0`.
#[inline]
fn ilog(x: u32) -> u32 {
    32 - x.leading_zeros()
}

/// A range encoder writing one Opus frame of at most `storage` bytes.
#[derive(Clone, Debug)]
pub struct RangeEncoder {
    buf: Vec<u8>,
    /// Bytes written by the range coder, from the front.
    offs: usize,
    /// Bytes written by the raw-bit writer, from the back.
    end_offs: usize,
    /// Raw bits not yet spilled to a byte (LSB first).
    end_window: u32,
    nend_bits: u32,
    /// Low end of the current range.
    low: u32,
    /// Size of the current range.
    rng: u32,
    /// Byte waiting to be written, `-1` when there is none.
    rem: i32,
    /// Pending `0xFF` bytes a carry would propagate through.
    ext: u32,
    /// Whole bits used so far, including what is buffered in `rng`.
    nbits_total: u32,
    /// Set when the frame ran out of room; the bitstream is then truncated.
    error: bool,
}

impl RangeEncoder {
    /// An encoder over a frame of exactly `storage` bytes.
    pub fn new(storage: usize) -> RangeEncoder {
        RangeEncoder {
            buf: vec![0; storage],
            offs: 0,
            end_offs: 0,
            end_window: 0,
            nend_bits: 0,
            low: 0,
            rng: 0x8000_0000,
            rem: -1,
            ext: 0,
            // The termination bit, the same 33 the decoder starts from.
            nbits_total: CODE_BITS + 1,
            error: false,
        }
    }

    /// Bytes the frame may occupy.
    pub fn storage(&self) -> usize {
        self.buf.len()
    }

    /// The range coder state, which the decoder reproduces exactly.
    pub fn range(&self) -> u32 {
        self.rng
    }

    /// True once a symbol has been dropped for want of room.
    pub fn error(&self) -> bool {
        self.error
    }

    fn write_byte(&mut self, b: u8) {
        if self.offs + self.end_offs >= self.buf.len() {
            self.error = true;
            return;
        }
        self.buf[self.offs] = b;
        self.offs += 1;
    }

    fn write_byte_at_end(&mut self, b: u8) {
        if self.offs + self.end_offs >= self.buf.len() {
            self.error = true;
            return;
        }
        self.end_offs += 1;
        let n = self.buf.len();
        self.buf[n - self.end_offs] = b;
    }

    /// Outputs a byte, propagating a carry into the bytes already written.
    fn carry_out(&mut self, c: i32) {
        if c != 0xFF {
            let carry = c >> 8;
            if self.rem >= 0 {
                let v = (self.rem + carry) as u8;
                self.write_byte(v);
            }
            while self.ext > 0 {
                self.write_byte(((0xFF + carry) & 0xFF) as u8);
                self.ext -= 1;
            }
            self.rem = c & 0xFF;
        } else {
            self.ext += 1;
        }
    }

    #[inline]
    fn normalize(&mut self) {
        while self.rng <= CODE_TOP {
            self.carry_out((self.low >> CODE_SHIFT) as i32);
            self.low = (self.low << 8) & 0x7FFF_FFFF;
            self.rng <<= 8;
            self.nbits_total += 8;
        }
    }

    /// `ec_encode()`: the symbol spanning `[fl, fh)` of `ft`.
    #[inline]
    pub fn encode(&mut self, fl: u32, fh: u32, ft: u32) {
        let r = self.rng / ft;
        if fl > 0 {
            self.low = self.low.wrapping_add(self.rng - r * (ft - fl));
            self.rng = r * (fh - fl);
        } else {
            self.rng -= r * (ft - fh);
        }
        self.normalize();
    }

    /// `ec_encode_bin()`: [`RangeEncoder::encode`] with `ft = 1<<ftb`.
    #[inline]
    pub fn encode_bin(&mut self, fl: u32, fh: u32, ftb: u32) {
        let r = self.rng >> ftb;
        let ft = 1u32 << ftb;
        if fl > 0 {
            self.low = self.low.wrapping_add(self.rng - r * (ft - fl));
            self.rng = r * (fh - fl);
        } else {
            self.rng -= r * (ft - fh);
        }
        self.normalize();
    }

    /// `ec_enc_bit_logp()`: one bit whose probability of being 1 is `2**-logp`.
    #[inline]
    pub fn enc_bit_logp(&mut self, val: bool, logp: u32) {
        let s = self.rng >> logp;
        if val {
            self.low = self.low.wrapping_add(self.rng - s);
            self.rng = s;
        } else {
            self.rng -= s;
        }
        self.normalize();
    }

    /// `ec_enc_icdf()`: symbol `s` of an inverse-CDF table, `ft = 1<<ftb`.
    #[inline]
    pub fn enc_icdf(&mut self, s: usize, icdf: &[u8], ftb: u32) {
        let r = self.rng >> ftb;
        if s > 0 {
            self.low = self.low.wrapping_add(self.rng - r * icdf[s - 1] as u32);
            self.rng = r * (icdf[s - 1] as u32 - icdf[s] as u32);
        } else {
            self.rng -= r * icdf[s] as u32;
        }
        self.normalize();
    }

    /// `ec_enc_uint()`: one of `ft` equiprobable values (Section 5.1.4).
    pub fn enc_uint(&mut self, val: u32, ft: u32) {
        debug_assert!(ft > 1 && val < ft);
        let ftb = ilog(ft - 1);
        if ftb <= 8 {
            self.encode(val, val + 1, ft);
        } else {
            let ftb = ftb - 8;
            let hi = val >> ftb;
            self.encode(hi, hi + 1, ((ft - 1) >> ftb) + 1);
            self.enc_bits(val & ((1 << ftb) - 1), ftb);
        }
    }

    /// `ec_enc_bits()`: `bits` raw bits, written backwards from the end of the
    /// frame (Section 5.1.4).
    pub fn enc_bits(&mut self, val: u32, bits: u32) {
        if bits == 0 {
            return;
        }
        if self.nend_bits + bits > WINDOW_SIZE {
            while self.nend_bits >= 8 {
                let b = (self.end_window & 0xFF) as u8;
                self.write_byte_at_end(b);
                self.end_window >>= 8;
                self.nend_bits -= 8;
            }
        }
        self.end_window |= val << self.nend_bits;
        self.nend_bits += bits;
        self.nbits_total += bits;
    }

    /// `ec_tell()`: a conservative upper bound on whole bits used so far.
    #[inline]
    pub fn tell(&self) -> u32 {
        self.nbits_total - ilog(self.rng)
    }

    /// `ec_tell_frac()`: the same bound in 1/8th bits (Section 4.1.6.2).
    pub fn tell_frac(&self) -> u32 {
        let mut lg = ilog(self.rng);
        let mut r_q15 = self.rng >> (lg - 16);
        for _ in 0..3 {
            r_q15 = (r_q15 * r_q15) >> 15;
            let bit = r_q15 >> 16;
            lg = 2 * lg + bit;
            r_q15 >>= bit;
        }
        self.nbits_total * 8 - lg
    }

    /// Terminates the stream and returns the frame, `storage` bytes long.
    ///
    /// Section 5.1.5: the value written is the one in `[low, low+rng)` with the
    /// most trailing zeros, so that the raw bits packed from the other end may
    /// overwrite them wherever the two streams meet.
    pub fn done(mut self) -> Vec<u8> {
        let mut l = (CODE_BITS - ilog(self.rng)) as i32;
        let mut msk = 0x7FFF_FFFFu32 >> l;
        let mut end = self.low.wrapping_add(msk) & !msk;
        if (end | msk) as u64 >= self.low as u64 + self.rng as u64 {
            l += 1;
            msk >>= 1;
            end = self.low.wrapping_add(msk) & !msk;
        }
        while l > 0 {
            self.carry_out((end >> CODE_SHIFT) as i32);
            end = (end << 8) & 0x7FFF_FFFF;
            l -= 8;
        }
        if self.rem >= 0 || self.ext > 0 {
            self.carry_out(0);
        }
        // Spill the buffered raw bits, then merge whatever is left of them into
        // the last byte the range coder did not reach.
        let mut window = self.end_window;
        let mut used = self.nend_bits;
        while used >= 8 {
            let b = (window & 0xFF) as u8;
            self.write_byte_at_end(b);
            window >>= 8;
            used -= 8;
        }
        if !self.error {
            let n = self.buf.len();
            for b in &mut self.buf[self.offs..n - self.end_offs] {
                *b = 0;
            }
            if used > 0 {
                if self.end_offs >= n {
                    self.error = true;
                } else {
                    // `l` counts how many bits of the last range-coder byte are
                    // unused; anything beyond that would corrupt it.
                    let room = (-l) as u32;
                    if self.offs + self.end_offs >= n && room < used {
                        window &= (1 << room) - 1;
                        self.error = true;
                    }
                    self.buf[n - self.end_offs - 1] |= window as u8;
                }
            }
        }
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range::RangeDecoder;

    #[test]
    fn round_trips_every_symbol_shape() {
        const ICDF: &[u8] = &[12, 8, 4, 0];
        const PDF: &[u16] = &[4, 4, 4, 4];
        let syms: Vec<usize> = (0..200).map(|i| (i * 7 + i / 3) % 4).collect();
        let bits: Vec<bool> = (0..64).map(|i| i % 3 == 0).collect();
        let uints: Vec<u32> = vec![0, 1, 5, 1000, 65535, 70000, 3];

        let mut enc = RangeEncoder::new(512);
        for &s in &syms {
            enc.enc_icdf(s, ICDF, 4);
        }
        for (i, &b) in bits.iter().enumerate() {
            enc.enc_bit_logp(b, (i % 5) as u32 + 1);
        }
        for &u in &uints {
            enc.enc_uint(u, 1 << 17);
        }
        for &s in &syms {
            let fl = (s as u32) * 4;
            enc.encode(fl, fl + 4, 16);
        }
        enc.enc_bits(0b1011, 4);
        assert!(!enc.error());
        let rng_end = enc.range();
        let frame = enc.done();

        let mut dec = RangeDecoder::new(&frame);
        for &s in &syms {
            assert_eq!(dec.dec_icdf(ICDF, 4), s);
        }
        for (i, &b) in bits.iter().enumerate() {
            assert_eq!(dec.dec_bit_logp((i % 5) as u32 + 1), b);
        }
        for &u in &uints {
            assert_eq!(dec.dec_uint(1 << 17), u);
        }
        for &s in &syms {
            assert_eq!(dec.dec_pdf(PDF, 4), s);
        }
        assert_eq!(dec.dec_bits(4), 0b1011);
        // The shared state the whole allocation depends on.
        assert_eq!(dec.range(), rng_end, "encoder and decoder rng diverged");
    }

    #[test]
    fn tell_tracks_the_decoder_bit_for_bit() {
        let mut enc = RangeEncoder::new(256);
        let mut expect = Vec::new();
        for i in 0..100u32 {
            enc.enc_icdf((i % 2) as usize, &[128, 0], 8);
            if i % 7 == 0 {
                enc.enc_bits(i & 7, 3);
            }
            expect.push((enc.tell(), enc.tell_frac()));
        }
        let frame = enc.done();
        let mut dec = RangeDecoder::new(&frame);
        for (i, &(t, tf)) in expect.iter().enumerate() {
            let _ = dec.dec_icdf(&[128, 0], 8);
            if i % 7 == 0 {
                let _ = dec.dec_bits(3);
            }
            assert_eq!(dec.tell(), t, "tell at symbol {i}");
            assert_eq!(dec.tell_frac(), tf, "tell_frac at symbol {i}");
        }
    }

    #[test]
    fn a_full_buffer_is_an_error_not_a_panic() {
        let mut enc = RangeEncoder::new(4);
        for _ in 0..200 {
            enc.enc_icdf(1, &[128, 0], 8);
            enc.enc_bits(0xFF, 8);
        }
        assert!(enc.error(), "overrunning 4 bytes must be reported");
        assert_eq!(enc.done().len(), 4);
    }
}
