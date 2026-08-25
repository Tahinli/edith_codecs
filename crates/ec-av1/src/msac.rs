//! AV1 multi-symbol arithmetic coder (spec section 8.2) — the writer side of
//! the decoder the specification defines, plus the probability adaptation of
//! spec section 8.3.
//!
//! AV1 specifies only the decoder: every tile payload is a range-coded string
//! whose meaning is whatever the decoding process in section 8.2 recovers from
//! it. [`SymbolEncoder`] is the inverse of that process — it narrows the same
//! `[low, low + range)` interval the decoder tracks and flushes the shortest
//! prefix that pins the interval down, so the decoder's `decode_symbol` returns
//! the symbols in the order they were written whatever bytes follow.
//!
//! CDFs are held in the specification's own orientation: for an `n`-symbol
//! alphabet the slice has `n + 1` entries, `cdf[i]` is the cumulative
//! probability of symbols `0..=i` scaled to `1 << 15` (so `cdf[n - 1]` is
//! always `1 << 15`), and `cdf[n]` is the adaptation counter of section 8.3.2.
//! Decoders that store the complement (`(1 << 15) - cdf[i]`) compute the same
//! intervals; the arithmetic below takes the complement where the spec does.

/// Probability precision dropped before the range multiply (spec `EC_PROB_SHIFT`).
const EC_PROB_SHIFT: u32 = 6;
/// Minimum interval width reserved for every symbol (spec `EC_MIN_PROB`).
const EC_MIN_PROB: u32 = 4;
/// Total probability mass of a CDF (spec `1 << 15`).
const CDF_TOP: u16 = 1 << 15;

/// A CDF for a two-symbol alphabet with fixed, equal probabilities — the
/// `L(n)` literal coder of spec 4.10.4 reads its bits through one of these.
const EQUIPROBABLE: [u16; 3] = [1 << 14, CDF_TOP, 0];

/// Writer for one AV1 symbol-coded string (one tile payload).
///
/// Symbols are written with [`SymbolEncoder::symbol`] (adapting the CDF as the
/// decoder will) or [`SymbolEncoder::symbol_fixed`]; raw bits go through
/// [`SymbolEncoder::literal`]. [`SymbolEncoder::finish`] flushes and returns
/// the payload bytes.
#[derive(Debug, Clone)]
pub struct SymbolEncoder {
    /// Low end of the current interval, kept shifted up so that the top of the
    /// active window sits at bit `cnt + 16`.
    low: u64,
    /// Width of the current interval, always renormalised into `[2^15, 2^16)`.
    rng: u32,
    /// Bits of headroom left in `low` before a byte has to be flushed; starts
    /// at `-9` so that the first flush happens once 9 bits have accumulated.
    cnt: i32,
    /// Flushed bytes before carry propagation: each entry can exceed `0xff` by
    /// the carry it owes the byte before it, which [`Self::finish`] adds in.
    precarry: Vec<u16>,
    /// What every symbol written so far cost against the CDF it was written
    /// with, in bits. The arithmetic coder spends `-log2(p)` on a symbol of
    /// probability `p` to within the rounding of its interval, so this is what
    /// a search may price a decision with without coding it twice.
    bits: f64,
}

impl Default for SymbolEncoder {
    fn default() -> SymbolEncoder {
        SymbolEncoder::new()
    }
}

impl SymbolEncoder {
    /// A new encoder holding the full interval, as the decoder's
    /// `init_symbol` process assumes.
    pub fn new() -> SymbolEncoder {
        SymbolEncoder {
            low: 0,
            rng: 0x8000,
            cnt: -9,
            precarry: Vec::new(),
            bits: 0.0,
        }
    }

    /// What the symbols written so far cost, in bits.
    pub fn bits(&self) -> f64 {
        self.bits
    }

    /// Forgets the cost accumulated so far, so that the next stretch of
    /// symbols can be priced on its own.
    pub fn reset_bits(&mut self) {
        self.bits = 0.0;
    }

    /// Writes `symbol` against `cdf` and adapts `cdf` exactly as the decoder
    /// will after reading it (spec 8.3.2).
    ///
    /// # Panics
    /// Panics if `symbol` is not a symbol of `cdf`'s alphabet, or if `cdf` is
    /// shorter than a two-symbol alphabet's three entries.
    pub fn symbol(&mut self, symbol: usize, cdf: &mut [u16]) {
        self.symbol_fixed(symbol, cdf);
        update_cdf(cdf, symbol);
    }

    /// Writes `symbol` against a CDF that does not adapt — the form the spec
    /// uses for the literal and equiprobable reads.
    ///
    /// # Panics
    /// Panics if `symbol` is not a symbol of `cdf`'s alphabet, or if `cdf` is
    /// shorter than a two-symbol alphabet's three entries.
    pub fn symbol_fixed(&mut self, symbol: usize, cdf: &[u16]) {
        assert!(cdf.len() >= 3, "a CDF covers at least two symbols");
        let nsyms = cdf.len() - 1;
        assert!(symbol < nsyms, "symbol {symbol} is outside the alphabet");

        // The decoder walks the alphabet computing the same two boundaries and
        // stops at the first symbol whose lower boundary the coded value has
        // fallen below, so the encoder only has to narrow to that symbol's
        // slice of the interval.
        let r = self.rng;
        let fh = u32::from(CDF_TOP - cdf[symbol]);
        let v = (((r >> 8) * (fh >> EC_PROB_SHIFT)) >> (7 - EC_PROB_SHIFT))
            + EC_MIN_PROB * (nsyms - 1 - symbol) as u32;
        let mut low = self.low;
        let rng = if symbol > 0 {
            let fl = u32::from(CDF_TOP - cdf[symbol - 1]);
            let u = (((r >> 8) * (fl >> EC_PROB_SHIFT)) >> (7 - EC_PROB_SHIFT))
                + EC_MIN_PROB * (nsyms - symbol) as u32;
            low += u64::from(r - u);
            u - v
        } else {
            // Symbol 0 owns the top of the interval, so only its width moves.
            r - v
        };
        // What this symbol really cost is how far it narrowed the interval,
        // which is not quite its nominal probability: the coder works the
        // range through an eight-bit multiply and hands every symbol above
        // this one a floor of `EC_MIN_PROB`. Pricing the narrowing rather than
        // the table entry is what makes the account match the bytes.
        self.bits -= (f64::from(rng) / f64::from(r)).log2();
        self.normalize(low, rng);
    }

    /// Writes `bits` raw bits of `value`, most significant first — the `L(n)`
    /// descriptor of spec 4.10.4, which is `bits` equiprobable symbols.
    ///
    /// # Panics
    /// Panics if `bits` exceeds 32.
    pub fn literal(&mut self, value: u32, bits: u32) {
        assert!(bits <= 32, "a literal is at most 32 bits");
        for i in (0..bits).rev() {
            let bit = (value >> i) & 1;
            self.symbol_fixed(bit as usize, &EQUIPROBABLE);
        }
    }

    /// Flushes the coder and returns the payload bytes.
    ///
    /// The flush writes the shortest value inside the final interval that stays
    /// inside it however the decoder pads the string, which is what lets a tile
    /// payload end on a byte boundary with no terminator.
    pub fn finish(mut self) -> Vec<u8> {
        // Round the interval's low end up to a value with 14 low zero bits,
        // then set bit 14: the result is inside the interval (the interval is
        // at least 2^15 wide before normalisation) and its tail bits are all
        // zero, so they need not be written at all.
        let m: u64 = 0x3fff;
        let mut e = ((self.low + m) & !m) | (m + 1);
        let mut c = self.cnt;
        let mut s = 10 + c;
        while s > 0 {
            self.precarry.push((e >> (c + 16)) as u16);
            e &= (1u64 << (c + 16)) - 1;
            s -= 8;
            c -= 8;
        }

        // Each flushed entry may have overflowed a byte; the overflow is a
        // carry into the byte written before it.
        let mut out = vec![0u8; self.precarry.len()];
        let mut carry = 0u32;
        for (byte, &pre) in out.iter_mut().zip(self.precarry.iter()).rev() {
            let v = u32::from(pre) + carry;
            *byte = v as u8;
            carry = v >> 8;
        }
        out
    }

    /// Renormalises the interval back into `[2^15, 2^16)`, flushing whole bytes
    /// of `low` once they can no longer change.
    fn normalize(&mut self, low: u64, rng: u32) {
        debug_assert!(rng > 0 && rng <= 0xffff);
        let d = 16 - (32 - rng.leading_zeros()) as i32;
        let mut c = self.cnt;
        let mut s = c + d;
        let mut low = low;
        if s >= 0 {
            // At least one byte at the top of `low` is settled: a later carry
            // can still reach it, which is why it is kept in the precarry
            // buffer as a `u16` rather than truncated to a byte here.
            c += 16;
            let mut m = (1u64 << c) - 1;
            if s >= 8 {
                self.precarry.push((low >> c) as u16);
                low &= m;
                c -= 8;
                m >>= 8;
            }
            self.precarry.push((low >> c) as u16);
            s = c + d - 24;
            low &= m;
        }
        self.low = low << d;
        self.rng = rng << d;
        self.cnt = s;
    }
}

/// The CDF adaptation of spec 8.3.2: every boundary moves a `rate`-dependent
/// fraction of the way towards the extreme the observed symbol implies, and the
/// counter in the last entry slows the rate down for the first 32 symbols.
fn update_cdf(cdf: &mut [u16], symbol: usize) {
    let nsyms = cdf.len() - 1;
    let count = cdf[nsyms];
    let rate = 3 + (nsyms >> 1).min(2) + (count >> 4) as usize;
    for (i, p) in cdf[..nsyms - 1].iter_mut().enumerate() {
        if i < symbol {
            *p -= *p >> rate;
        } else {
            *p += (CDF_TOP - *p) >> rate;
        }
    }
    cdf[nsyms] = count + 1 - (count >> 5);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The decoding process of spec 8.2, written from the specification's
    /// pseudocode so that the roundtrip tests measure the encoder against the
    /// decoder AV1 defines rather than against the encoder's own arithmetic.
    pub(crate) struct SymbolDecoder<'a> {
        data: &'a [u8],
        /// Next bit to read, as a bit offset into `data`.
        bit: usize,
        value: u32,
        range: u32,
        /// Bits left before the decoder starts padding with zeros (spec
        /// `SymbolMaxBits`), which goes negative at the end of the string.
        max_bits: i32,
    }

    impl<'a> SymbolDecoder<'a> {
        /// `init_symbol` (spec 8.2.2).
        pub(crate) fn new(data: &'a [u8]) -> SymbolDecoder<'a> {
            let mut d = SymbolDecoder {
                data,
                bit: 0,
                value: 0,
                range: 1 << 15,
                max_bits: 8 * data.len() as i32 - 15,
            };
            let num_bits = usize::min(data.len() * 8, 15) as u32;
            let buf = d.f(num_bits);
            d.value = ((1 << 15) - 1) ^ (buf << (15 - num_bits));
            d
        }

        /// `f(n)`: the next `n` bits, most significant first, zero past the end.
        fn f(&mut self, n: u32) -> u32 {
            let mut v = 0;
            for _ in 0..n {
                let byte = self.data.get(self.bit >> 3).copied().unwrap_or(0);
                v = (v << 1) | u32::from((byte >> (7 - (self.bit & 7))) & 1);
                self.bit += 1;
            }
            v
        }

        /// `decode_symbol` (spec 8.2.6), with the adaptation of 8.3.2.
        fn symbol(&mut self, cdf: &mut [u16]) -> usize {
            let s = self.symbol_fixed(cdf);
            update_cdf(cdf, s);
            s
        }

        fn symbol_fixed(&mut self, cdf: &[u16]) -> usize {
            let nsyms = cdf.len() - 1;
            let mut cur = self.range;
            let mut prev;
            let mut symbol = 0;
            loop {
                prev = cur;
                let f = u32::from(CDF_TOP - cdf[symbol]);
                cur = (((self.range >> 8) * (f >> EC_PROB_SHIFT)) >> (7 - EC_PROB_SHIFT))
                    + EC_MIN_PROB * (nsyms - 1 - symbol) as u32;
                if self.value >= cur {
                    break;
                }
                symbol += 1;
            }
            self.range = prev - cur;
            self.value -= cur;

            // Renormalise back into `[2^15, 2^16)`, the same shift the encoder
            // applied when it narrowed to this symbol.
            let bits = 16 - (32 - self.range.leading_zeros());
            self.range <<= bits;
            let num_bits = u32::min(bits, self.max_bits.max(0) as u32);
            let new_data = self.f(num_bits);
            let padded = new_data << (bits - num_bits);
            self.value = padded ^ (((self.value + 1) << bits) - 1);
            self.max_bits -= bits as i32;
            symbol
        }

        pub(crate) fn literal(&mut self, bits: u32) -> u32 {
            let mut v = 0;
            for _ in 0..bits {
                v = (v << 1) | self.symbol_fixed(&EQUIPROBABLE) as u32;
            }
            v
        }
    }

    /// A reproducible symbol stream: xorshift, so a failure names one seed.
    fn rng(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    fn flat_cdf(nsyms: usize) -> Vec<u16> {
        let mut cdf = vec![0u16; nsyms + 1];
        for (i, p) in cdf[..nsyms].iter_mut().enumerate() {
            *p = (((i + 1) * (1 << 15)) / nsyms) as u16;
        }
        cdf[nsyms] = 0;
        cdf
    }

    #[test]
    fn adaptive_symbols_roundtrip() {
        for nsyms in 2..=16usize {
            let mut state = 0x2545_f491_4f6c_dd1d ^ nsyms as u64;
            let symbols: Vec<usize> = (0..4000)
                .map(|_| (rng(&mut state) % nsyms as u64) as usize)
                .collect();

            let mut enc = SymbolEncoder::new();
            let mut cdf = flat_cdf(nsyms);
            for &s in &symbols {
                enc.symbol(s, &mut cdf);
            }
            let payload = enc.finish();

            let mut dec = SymbolDecoder::new(&payload);
            let mut cdf = flat_cdf(nsyms);
            for (i, &want) in symbols.iter().enumerate() {
                let got = dec.symbol(&mut cdf);
                assert_eq!(got, want, "alphabet {nsyms}, symbol {i}");
            }
        }
    }

    #[test]
    fn skewed_symbols_cost_less_than_a_bit() {
        // A 1-in-256 symbol against an adapting CDF must cost far under one bit
        // each: this is the property a range coder exists for, and it fails
        // loudly if the interval narrowing is wired backwards.
        let mut state = 0x9e37_79b9_7f4a_7c15;
        let symbols: Vec<usize> = (0..8000)
            .map(|_| usize::from(rng(&mut state).is_multiple_of(256)))
            .collect();
        let mut enc = SymbolEncoder::new();
        let mut cdf = flat_cdf(2);
        for &s in &symbols {
            enc.symbol(s, &mut cdf);
        }
        let payload = enc.finish();
        assert!(
            payload.len() < 8000 / 8 / 4,
            "8000 skewed symbols took {} bytes",
            payload.len()
        );

        let mut dec = SymbolDecoder::new(&payload);
        let mut cdf = flat_cdf(2);
        for (i, &want) in symbols.iter().enumerate() {
            assert_eq!(dec.symbol(&mut cdf), want, "symbol {i}");
        }
    }

    #[test]
    fn literals_roundtrip() {
        let mut state = 0x0123_4567_89ab_cdef;
        let values: Vec<(u32, u32)> = (0..2000)
            .map(|_| {
                let bits = 1 + (rng(&mut state) % 16) as u32;
                let value = (rng(&mut state) as u32) & ((1u32 << bits) - 1);
                (value, bits)
            })
            .collect();

        let mut enc = SymbolEncoder::new();
        for &(v, bits) in &values {
            enc.literal(v, bits);
        }
        let payload = enc.finish();
        // Equiprobable bits cannot be compressed, so the payload is one bit per
        // written bit plus the flush.
        let total: u32 = values.iter().map(|&(_, b)| b).sum();
        assert!(payload.len() as u32 <= total / 8 + 4);

        let mut dec = SymbolDecoder::new(&payload);
        for (i, &(want, bits)) in values.iter().enumerate() {
            assert_eq!(dec.literal(bits), want, "literal {i}");
        }
    }

    #[test]
    fn mixed_symbols_and_literals_roundtrip() {
        let mut state = 0xdead_beef_cafe_f00d;
        let mut enc = SymbolEncoder::new();
        let mut cdf3 = flat_cdf(3);
        let mut cdf8 = flat_cdf(8);
        let mut written = Vec::new();
        for _ in 0..3000 {
            match rng(&mut state) % 3 {
                0 => {
                    let s = (rng(&mut state) % 3) as usize;
                    enc.symbol(s, &mut cdf3);
                    written.push((0u8, s as u32));
                }
                1 => {
                    let s = (rng(&mut state) % 8) as usize;
                    enc.symbol(s, &mut cdf8);
                    written.push((1, s as u32));
                }
                _ => {
                    let v = (rng(&mut state) % 256) as u32;
                    enc.literal(v, 8);
                    written.push((2, v));
                }
            }
        }
        let payload = enc.finish();

        let mut dec = SymbolDecoder::new(&payload);
        let mut cdf3 = flat_cdf(3);
        let mut cdf8 = flat_cdf(8);
        for (i, &(kind, want)) in written.iter().enumerate() {
            let got = match kind {
                0 => dec.symbol(&mut cdf3) as u32,
                1 => dec.symbol(&mut cdf8) as u32,
                _ => dec.literal(8),
            };
            assert_eq!(got, want, "item {i} of kind {kind}");
        }
    }

    /// The bit account is what a search prices decisions with, so it has to
    /// agree with what the coder actually spends: over a long enough string the
    /// two are within the coder's own rounding, a fraction of a percent.
    #[test]
    fn the_bit_account_matches_the_bytes_written() {
        let mut enc = SymbolEncoder::new();
        let mut state: u32 = 0x1234_5678;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state
        };
        // A skewed CDF, so that the account has something to be wrong about:
        // an equiprobable string costs one bit a symbol however it is priced.
        let skewed: [u16; 3] = [29000, 1 << 15, 0];
        for _ in 0..20_000 {
            let symbol = usize::from(next() % 100 < 12);
            enc.symbol_fixed(symbol, &skewed);
        }
        let priced = enc.bits();
        let spent = (enc.finish().len() * 8) as f64;
        assert!(
            (priced - spent).abs() / spent < 0.01,
            "priced {priced:.0} bits, spent {spent:.0}"
        );
    }

    #[test]
    fn empty_payload_is_short() {
        assert!(SymbolEncoder::new().finish().len() <= 2);
    }
}
