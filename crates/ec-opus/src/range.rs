//! The Opus range decoder (RFC 6716, Section 4.1).
//!
//! Every symbol in an Opus frame — SILK and CELT alike — comes out of this
//! one object, so its arithmetic is exact integer arithmetic on `u32`, not an
//! approximation that happens to agree most of the time. A single ULP of drift
//! here desynchronises the whole frame, which is why the RFC test vectors ship
//! the encoder's final `rng` value per packet: [`RangeDecoder::range`] against
//! that value is a bit-exact check of the entire decode path.
//!
//! Two streams share the frame: range coder bytes read forward from the start,
//! and CELT "raw bits" read backward from the end. They are allowed to overlap
//! (Section 4.1.4) — reading past the end of either simply yields zero bits,
//! so a truncated or malformed frame decodes to noise rather than panicking.

/// Number of bits in the code value, `EC_CODE_BITS` in the reference.
const CODE_BITS: u32 = 32;
/// The renormalisation threshold: `rng` is kept above `2**23`.
const CODE_TOP: u32 = 1 << (CODE_BITS - 8 - 1); // 2**23

/// `ilog(x)`: one plus the index of the most significant set bit, `0` for `0`.
#[inline]
fn ilog(x: u32) -> u32 {
    32 - x.leading_zeros()
}

/// A range decoder over one Opus frame.
#[derive(Clone, Debug)]
pub struct RangeDecoder<'a> {
    buf: &'a [u8],
    /// Next byte the range coder reads, from the front.
    offs: usize,
    /// Bytes already consumed by the raw-bit reader, from the back.
    end_offs: usize,
    /// Bits buffered by the raw-bit reader (LSB first).
    end_window: u32,
    end_bits: u32,
    /// Difference between the high end of the range and the coded value.
    val: u32,
    /// Size of the current range.
    rng: u32,
    /// The bit left over from the last byte read by the range coder.
    rem: u32,
    /// Whole bits consumed so far, including what is buffered in `rng`.
    nbits_total: u32,
}

impl<'a> RangeDecoder<'a> {
    /// Starts decoding `buf`, an Opus frame body (Section 4.1.1).
    pub fn new(buf: &'a [u8]) -> RangeDecoder<'a> {
        let b0 = buf.first().copied().unwrap_or(0) as u32;
        let mut dec = RangeDecoder {
            buf,
            offs: 1.min(buf.len()),
            end_offs: 0,
            end_window: 0,
            end_bits: 0,
            val: 127 - (b0 >> 1),
            rng: 128,
            rem: b0 & 1,
            // 9 here becomes 33 once the initial renormalisation runs.
            nbits_total: 9,
        };
        dec.normalize();
        dec
    }

    /// The encoder's `rng` after the same symbols — the test-vector oracle.
    pub fn range(&self) -> u32 {
        self.rng
    }

    /// Bytes in the frame.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// True for a zero-length frame (DTX).
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    #[inline]
    fn next_byte(&mut self) -> u32 {
        let b = self.buf.get(self.offs).copied().unwrap_or(0) as u32;
        // Past the end the decoder keeps feeding zeros (Section 4.1.2.1).
        self.offs = (self.offs + 1).min(self.buf.len() + 1);
        b
    }

    #[inline]
    fn normalize(&mut self) {
        while self.rng <= CODE_TOP {
            self.nbits_total += 8;
            self.rng <<= 8;
            let b = self.next_byte();
            let sym = (self.rem << 7) | (b >> 1);
            self.rem = b & 1;
            self.val = ((self.val << 8) + (255 - sym)) & 0x7FFF_FFFF;
        }
    }

    /// `ec_decode()`: the frequency `fs` of the symbol about to be decoded.
    ///
    /// Must be followed by [`RangeDecoder::update`] with that symbol's
    /// `(fl, fh, ft)` before any other decode call.
    #[inline]
    pub fn decode(&mut self, ft: u32) -> u32 {
        let r = self.rng / ft;
        ft - (self.val / r + 1).min(ft)
    }

    /// `ec_decode_bin()`: [`RangeDecoder::decode`] with `ft = 1<<ftb`.
    #[inline]
    pub fn decode_bin(&mut self, ftb: u32) -> u32 {
        let r = self.rng >> ftb;
        (1 << ftb) - (self.val / r + 1).min(1 << ftb)
    }

    /// `ec_dec_update()`: consumes the symbol spanning `[fl, fh)` of `ft`.
    #[inline]
    pub fn update(&mut self, fl: u32, fh: u32, ft: u32) {
        let r = self.rng / ft;
        let s = r * (ft - fh);
        self.val -= s;
        self.rng = if fl > 0 { r * (fh - fl) } else { self.rng - s };
        self.normalize();
    }

    /// `ec_dec_bit_logp()`: one bit whose probability of being 1 is
    /// `2**-logp`.
    #[inline]
    pub fn dec_bit_logp(&mut self, logp: u32) -> bool {
        let s = self.rng >> logp;
        let bit = self.val < s;
        if !bit {
            self.val -= s;
            self.rng -= s;
        } else {
            self.rng = s;
        }
        self.normalize();
        bit
    }

    /// `ec_dec_icdf()`: one symbol from an inverse-CDF table, `ft = 1<<ftb`.
    ///
    /// `icdf[k]` holds `(1<<ftb) - fh[k]` and the table ends with `0`.
    #[inline]
    pub fn dec_icdf(&mut self, icdf: &[u8], ftb: u32) -> usize {
        let r = self.rng >> ftb;
        let mut k = 0usize;
        let mut t = self.rng;
        let mut s = r * icdf[0] as u32;
        while self.val < s {
            t = s;
            k += 1;
            s = r * icdf[k] as u32;
        }
        self.val -= s;
        self.rng = t - s;
        self.normalize();
        k
    }

    /// Decodes a symbol from a PDF given as frequency counts (the form the
    /// RFC tables are written in), summing to `1<<ftb`.
    #[inline]
    pub fn dec_pdf(&mut self, pdf: &[u16], ftb: u32) -> usize {
        let fs = self.decode_bin(ftb);
        let mut fl = 0u32;
        let mut k = 0usize;
        loop {
            let fh = fl + pdf[k] as u32;
            if fs < fh {
                self.update(fl, fh, 1 << ftb);
                return k;
            }
            fl = fh;
            k += 1;
        }
    }

    /// `ec_dec_bits()`: `bits` raw bits, read backwards from the end of the
    /// frame (Section 4.1.4).
    pub fn dec_bits(&mut self, bits: u32) -> u32 {
        if bits == 0 {
            return 0;
        }
        while self.end_bits < bits {
            let b = if self.end_offs < self.buf.len() {
                self.end_offs += 1;
                self.buf[self.buf.len() - self.end_offs] as u32
            } else {
                0
            };
            self.end_window |= b << self.end_bits;
            self.end_bits += 8;
        }
        let ret = self.end_window & ((1u32 << bits) - 1);
        self.end_window >>= bits;
        self.end_bits -= bits;
        self.nbits_total += bits;
        ret
    }

    /// `ec_dec_uint()`: one of `ft` equiprobable values, `0..ft`
    /// (Section 4.1.5). `ft` must be at least 1.
    pub fn dec_uint(&mut self, ft: u32) -> u32 {
        debug_assert!(ft > 1);
        let ftb = ilog(ft - 1);
        if ftb <= 8 {
            let t = self.decode(ft);
            self.update(t, t + 1, ft);
            t
        } else {
            let ftb = ftb - 8;
            let ft1 = ((ft - 1) >> ftb) + 1;
            let t = self.decode(ft1);
            self.update(t, t + 1, ft1);
            let t = (t << ftb) | self.dec_bits(ftb);
            // A corrupt frame can code out of range; saturate rather than
            // hand a caller an out-of-bounds index (Section 4.1.5).
            t.min(ft - 1)
        }
    }

    /// Shrinks the frame to `len` bytes, which is what the redundancy framing
    /// does when it reserves the tail of a packet for a second CELT frame
    /// (RFC 6716, Section 4.5.1). Bytes already read are unaffected.
    pub fn shrink(&mut self, len: usize) {
        if len <= self.buf.len() {
            self.buf = &self.buf[..len];
        }
    }

    /// Declares the rest of the frame consumed, so [`RangeDecoder::tell`]
    /// reports the whole frame. CELT does this for a silent frame, whose
    /// remaining bits carry nothing (RFC 6716, Section 4.3).
    pub fn skip_to_end(&mut self) {
        self.nbits_total = (self.buf.len() as u32) * 8 + ilog(self.rng);
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
}

/// The Opus range *encoder* (RFC 6716, Section 5.1) — the exact mirror of
/// [`RangeDecoder`], over a fixed-size buffer.
///
/// Range-coded bytes grow from the front of the buffer, raw bits from the
/// back, exactly as the decoder reads them; the two meeting is a bust, which
/// [`RangeEncoder::error`] reports rather than corrupting either stream. The
/// buffer is owned and reused across frames — [`RangeEncoder::reset`] costs a
/// `fill(0)` of the frame, so a steady-state encoder never allocates here.
#[derive(Clone, Debug)]
pub struct RangeEncoder {
    buf: Vec<u8>,
    /// Logical frame size: the decoder will be handed `buf[..storage]`.
    storage: usize,
    /// Next byte the range coder writes, from the front.
    offs: usize,
    /// Bytes already written by the raw-bit writer, from the back.
    end_offs: usize,
    /// Bits buffered by the raw-bit writer (LSB first).
    end_window: u32,
    nend_bits: u32,
    /// Whole bits written so far, including what is buffered in `rng`.
    nbits_total: u32,
    /// Low end of the current range.
    val: u32,
    /// Size of the current range.
    rng: u32,
    /// A buffered byte awaiting carry propagation, `-1` before the first.
    rem: i32,
    /// Count of buffered `0xFF` bytes that a carry would turn into `0x00`.
    ext: u32,
    error: bool,
}

impl RangeEncoder {
    /// An encoder with no buffer; call [`RangeEncoder::reset`] before use.
    pub fn new() -> RangeEncoder {
        RangeEncoder {
            buf: Vec::new(),
            storage: 0,
            offs: 0,
            end_offs: 0,
            end_window: 0,
            nend_bits: 0,
            nbits_total: CODE_BITS + 1,
            val: 0,
            rng: 1 << (CODE_BITS - 1),
            rem: -1,
            ext: 0,
            error: false,
        }
    }

    /// Starts a fresh frame of exactly `size` bytes.
    pub fn reset(&mut self, size: usize) {
        if self.buf.len() < size {
            self.buf.resize(size, 0);
        }
        self.buf[..size].fill(0);
        self.storage = size;
        self.offs = 0;
        self.end_offs = 0;
        self.end_window = 0;
        self.nend_bits = 0;
        self.nbits_total = CODE_BITS + 1;
        self.val = 0;
        self.rng = 1 << (CODE_BITS - 1);
        self.rem = -1;
        self.ext = 0;
        self.error = false;
    }

    /// The encoder's current range state — what the decoder's
    /// [`RangeDecoder::range`] must equal after the same symbols.
    pub fn range(&self) -> u32 {
        self.rng
    }

    /// True when the frame busted its budget; the frame is unusable.
    pub fn error(&self) -> bool {
        self.error
    }

    /// The frame size in bytes.
    pub fn storage(&self) -> usize {
        self.storage
    }

    /// Bytes the range coder has finalised so far (`ec_range_bytes`).
    pub fn range_bytes(&self) -> usize {
        self.offs
    }

    /// The finished frame; call only after [`RangeEncoder::done`].
    pub fn data(&self) -> &[u8] {
        &self.buf[..self.storage]
    }

    fn write_byte(&mut self, value: u32) {
        if self.offs + self.end_offs >= self.storage {
            self.error = true;
            return;
        }
        self.buf[self.offs] = value as u8;
        self.offs += 1;
    }

    fn write_byte_at_end(&mut self, value: u32) {
        if self.offs + self.end_offs >= self.storage {
            self.error = true;
            return;
        }
        self.end_offs += 1;
        self.buf[self.storage - self.end_offs] = value as u8;
    }

    fn carry_out(&mut self, c: i32) {
        if c != 0xFF {
            let carry = c >> 8;
            if self.rem >= 0 {
                self.write_byte((self.rem + carry) as u32);
            }
            while self.ext > 0 {
                self.write_byte(((0xFF + carry) & 0xFF) as u32);
                self.ext -= 1;
            }
            self.rem = c & 0xFF;
        } else {
            self.ext += 1;
        }
    }

    fn normalize(&mut self) {
        while self.rng <= CODE_TOP {
            self.carry_out((self.val >> 23) as i32);
            self.val = (self.val << 8) & 0x7FFF_FFFF;
            self.rng <<= 8;
            self.nbits_total += 8;
        }
    }

    /// `ec_encode()`: one symbol spanning `[fl, fh)` of `ft`.
    #[inline]
    pub fn encode(&mut self, fl: u32, fh: u32, ft: u32) {
        let r = self.rng / ft;
        if fl > 0 {
            self.val = self.val.wrapping_add(self.rng - r * (ft - fl));
            self.rng = r * (fh - fl);
        } else {
            self.rng -= r * (ft - fh);
        }
        self.normalize();
    }

    /// `ec_encode_bin()`: [`RangeEncoder::encode`] with `ft = 1<<bits`.
    #[inline]
    pub fn encode_bin(&mut self, fl: u32, fh: u32, bits: u32) {
        let r = self.rng >> bits;
        if fl > 0 {
            self.val = self.val.wrapping_add(self.rng - r * ((1 << bits) - fl));
            self.rng = r * (fh - fl);
        } else {
            self.rng -= r * ((1 << bits) - fh);
        }
        self.normalize();
    }

    /// `ec_enc_bit_logp()`: one bit whose probability of being 1 is `2**-logp`.
    #[inline]
    pub fn enc_bit_logp(&mut self, bit: bool, logp: u32) {
        let r = self.rng;
        let l = self.val;
        let s = r >> logp;
        let r = r - s;
        if bit {
            self.val = l.wrapping_add(r);
        }
        self.rng = if bit { s } else { r };
        self.normalize();
    }

    /// `ec_enc_icdf()`: symbol `k` of an inverse-CDF table, `ft = 1<<ftb`.
    #[inline]
    pub fn enc_icdf(&mut self, k: usize, icdf: &[u8], ftb: u32) {
        let r = self.rng >> ftb;
        if k > 0 {
            self.val = self.val.wrapping_add(self.rng - r * icdf[k - 1] as u32);
            self.rng = r * (icdf[k - 1] as u32 - icdf[k] as u32);
        } else {
            self.rng -= r * icdf[k] as u32;
        }
        self.normalize();
    }

    /// `ec_enc_uint()`: `value` as one of `ft` equiprobable values
    /// (Section 5.1.4). `ft` must be at least 2.
    pub fn enc_uint(&mut self, value: u32, ft: u32) {
        debug_assert!(ft > 1);
        let ft = ft - 1;
        let ftb = ilog(ft);
        if ftb > 8 {
            let ftb = ftb - 8;
            let ft1 = (ft >> ftb) + 1;
            let hi = value >> ftb;
            self.encode(hi, hi + 1, ft1);
            self.enc_bits(value & ((1u32 << ftb) - 1), ftb);
        } else {
            self.encode(value, value + 1, ft + 1);
        }
    }

    /// `ec_enc_bits()`: `bits` raw bits, written backwards from the end of the
    /// frame.
    pub fn enc_bits(&mut self, fl: u32, bits: u32) {
        debug_assert!(bits > 0);
        let mut window = self.end_window;
        let mut used = self.nend_bits;
        if used + bits > 32 {
            while used >= 8 {
                self.write_byte_at_end(window & 0xFF);
                window >>= 8;
                used -= 8;
            }
        }
        window |= fl << used;
        used += bits;
        self.end_window = window;
        self.nend_bits = used;
        self.nbits_total += bits;
    }

    /// `ec_enc_shrink()`: reduces the frame to `size` bytes, moving raw bits
    /// already written so they still end the frame. Only legal while
    /// `range_bytes()` plus the raw-bit bytes fit in `size`.
    pub fn shrink(&mut self, size: usize) {
        debug_assert!(self.offs + self.end_offs <= size);
        self.buf.copy_within(
            self.storage - self.end_offs..self.storage,
            size - self.end_offs,
        );
        self.storage = size;
        // The abandoned tail must be zero for the "rest is zeros" guarantee.
        if size > self.offs + self.end_offs {
            self.buf[self.offs..size - self.end_offs].fill(0);
        }
    }

    /// `ec_tell()`: a conservative upper bound on whole bits used so far.
    #[inline]
    pub fn tell(&self) -> u32 {
        self.nbits_total - ilog(self.rng)
    }

    /// `ec_tell_frac()`: the same bound in 1/8th bits.
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

    /// Declares the rest of the frame used, so [`RangeEncoder::tell`] reports
    /// the whole frame — the encoder side of the decoder's `skip_to_end`,
    /// which is what a silent CELT frame does with its remaining bits.
    pub fn pad_to_end(&mut self) {
        self.nbits_total = (self.storage as u32) * 8 + ilog(self.rng);
    }

    /// `ec_enc_done()`: terminates the frame. Everything between the range
    /// bytes and the raw bits is left as zeros, which the decoder reads as
    /// zeros — a CBR frame is therefore always exactly `storage` bytes.
    pub fn done(&mut self) {
        // Output the value in [val, val+rng) with the most trailing zeros.
        let mut l = CODE_BITS as i32 - ilog(self.rng) as i32;
        let mut msk = 0x7FFF_FFFFu32 >> l;
        let mut end = self.val.wrapping_add(msk) & !msk;
        if (end | msk) as u64 >= self.val as u64 + self.rng as u64 {
            l += 1;
            msk >>= 1;
            end = self.val.wrapping_add(msk) & !msk;
        }
        while l > 0 {
            self.carry_out((end >> 23) as i32);
            end = (end << 8) & 0x7FFF_FFFF;
            l -= 8;
        }
        if self.rem >= 0 || self.ext > 0 {
            self.carry_out(0);
        }
        let mut window = self.end_window;
        let mut used = self.nend_bits;
        while used >= 8 {
            self.write_byte_at_end(window & 0xFF);
            window >>= 8;
            used -= 8;
        }
        if !self.error && used > 0 {
            // Anything not written is already zero (reset cleared the frame).
            if self.end_offs >= self.storage {
                self.error = true;
            } else {
                let l = -l;
                if self.offs + self.end_offs >= self.storage && (l as u32) < used {
                    window &= (1u32 << l) - 1;
                    self.error = true;
                }
                self.buf[self.storage - self.end_offs - 1] |= window as u8;
            }
        }
    }
}

impl Default for RangeEncoder {
    fn default() -> Self {
        RangeEncoder::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_mixed_symbols() {
        // {4,4,4,4}/16 uniform context, as icdf and as raw PDF.
        const ICDF: &[u8] = &[12, 8, 4, 0];
        const PDF: &[u16] = &[4, 4, 4, 4];
        let syms: Vec<usize> = (0..200).map(|i| (i * 7 + i / 3) % 4).collect();
        let bits: Vec<bool> = (0..64).map(|i| i % 3 == 0).collect();
        let uints: Vec<u32> = vec![0, 1, 5, 1000, 65535, 70000, 3];

        let mut enc = RangeEncoder::new();
        enc.reset(512);
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
        enc.done();
        assert!(!enc.error());
        let frame = enc.data().to_vec();

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
    }

    /// The bit allocation is derived from `tell()`/`tell_frac()` on *both*
    /// sides, so the encoder's accounting must match the decoder's after every
    /// symbol — one bit of drift desynchronises the whole frame.
    #[test]
    fn encoder_and_decoder_tell_identically() {
        const ICDF: &[u8] = &[100, 60, 30, 10, 0];
        let mut enc = RangeEncoder::new();
        enc.reset(256);
        let mut log = Vec::new();
        let mut x = 7u32;
        for i in 0..300 {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            match i % 4 {
                0 => enc.enc_icdf((x % 5) as usize, ICDF, 7),
                1 => enc.enc_bit_logp(x & 1 == 0, (x % 6) + 1),
                2 => enc.enc_uint(x % 500, 700),
                _ => enc.enc_bits(x & 0x1F, 5),
            }
            log.push((i % 4, x, enc.tell(), enc.tell_frac(), enc.range()));
        }
        enc.done();
        assert!(!enc.error());
        let data = enc.data().to_vec();
        let mut dec = RangeDecoder::new(&data);
        for (kind, x, tell, tell_frac, rng) in log {
            match kind {
                0 => assert_eq!(dec.dec_icdf(ICDF, 7), (x % 5) as usize),
                1 => assert_eq!(dec.dec_bit_logp((x % 6) + 1), x & 1 == 0),
                2 => assert_eq!(dec.dec_uint(700), x % 500),
                _ => assert_eq!(dec.dec_bits(5), x & 0x1F),
            }
            assert_eq!(dec.tell(), tell, "tell drifted");
            assert_eq!(dec.tell_frac(), tell_frac, "tell_frac drifted");
            assert_eq!(dec.range(), rng, "range state drifted");
        }
    }

    /// A CBR frame stays exactly `storage` bytes: whatever the symbols leave
    /// unused decodes as zeros, which is what the encoder assumed.
    #[test]
    fn fixed_size_frame_decodes_past_the_coded_symbols() {
        let mut enc = RangeEncoder::new();
        enc.reset(40);
        enc.enc_icdf(1, &[100, 50, 0], 7);
        enc.enc_bits(0b101, 3);
        enc.done();
        assert!(!enc.error());
        assert_eq!(enc.data().len(), 40);
        let data = enc.data().to_vec();
        let mut dec = RangeDecoder::new(&data);
        assert_eq!(dec.dec_icdf(&[100, 50, 0], 7), 1);
        assert_eq!(dec.dec_bits(3), 0b101);
        // The rest of the frame reads as zero symbols, not garbage.
        for _ in 0..50 {
            let _ = dec.dec_bit_logp(4);
        }
    }

    /// `shrink` moves the raw-bit tail; symbols written before and after the
    /// shrink must both survive.
    #[test]
    fn shrink_preserves_both_streams() {
        let mut enc = RangeEncoder::new();
        enc.reset(200);
        enc.enc_uint(1234, 5000);
        enc.enc_bits(0xAB, 8);
        enc.shrink(50);
        enc.enc_uint(999, 5000);
        enc.enc_bits(0x5, 4);
        enc.done();
        assert!(!enc.error());
        assert_eq!(enc.data().len(), 50);
        let data = enc.data().to_vec();
        let mut dec = RangeDecoder::new(&data);
        assert_eq!(dec.dec_uint(5000), 1234);
        assert_eq!(dec.dec_bits(8), 0xAB);
        assert_eq!(dec.dec_uint(5000), 999);
        assert_eq!(dec.dec_bits(4), 0x5);
    }

    #[test]
    fn tell_agrees_with_tell_frac() {
        let data: Vec<u8> = (0..64u8)
            .map(|i| i.wrapping_mul(37).wrapping_add(11))
            .collect();
        let mut dec = RangeDecoder::new(&data);
        assert_eq!(dec.tell(), 1, "a fresh decoder reports the termination bit");
        for i in 0..100 {
            let _ = dec.dec_icdf(&[128, 0], 8);
            if i % 7 == 0 {
                let _ = dec.dec_bits(3);
            }
            assert_eq!(dec.tell(), dec.tell_frac().div_ceil(8));
        }
    }

    #[test]
    fn empty_frame_never_panics() {
        let mut dec = RangeDecoder::new(&[]);
        for _ in 0..50 {
            let _ = dec.dec_icdf(&[128, 0], 8);
            let _ = dec.dec_bits(8);
            let _ = dec.dec_uint(1 << 20);
        }
    }
}
