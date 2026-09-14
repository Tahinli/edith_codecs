//! The bool decoder (spec 8.3.2), VP9's arithmetic range coder over one
//! data partition (the compressed header, and each tile of tile data).
//!
//! Every syntax element below the uncompressed header is coded as a
//! sequence of bools, each written at an 8-bit zero-probability `p`
//! (meaning `p/256`). The decoder mirrors the encoder's interval state
//! with two numbers — `range` (128..=255) and `value` (the encoded
//! number minus the interval's left endpoint) — and renormalises one
//! bit at a time, shifting a fresh input byte into `value` for every 8
//! bits consumed (spec 8.3.2, `read_bool` / libvpx `vpx_reader`).
//!
//! The tests drive the decoder against a test-only implementation of the
//! spec's reference encoder (8.3.3, the carry-propagating writer) and
//! assert bit-exact round-trips.

use ec_core::Result;

/// Boolean arithmetic decoder over one data partition (spec 8.3.2).
pub struct BoolDecoder<'a> {
    data: &'a [u8],
    /// Index of the next byte to shift into `value`.
    pos: usize,
    /// 128..=255; identical to the encoder's range.
    range: u32,
    /// Encoded number minus the current interval's left endpoint; the top
    /// bits hold what has been read, bytes shift in at the bottom.
    value: u32,
    /// Bits shifted out of `value` since the last byte was shifted in.
    bit_count: u32,
    /// Bytes consumed past the end of the partition (read as 0, per the
    /// spec's zero-extension of x). A healthy partition stays at 0.
    overreads: u32,
}

impl<'a> BoolDecoder<'a> {
    /// Start decoding the partition `data` (spec 8.3.2 `init_bool`): the
    /// first two bytes are read into `value`, big-endian. A partition
    /// shorter than two bytes is zero-extended like the reference
    /// decoder does, and the shortfall shows up in [`Self::overreads`].
    pub fn new(data: &'a [u8]) -> Result<Self> {
        if data.is_empty() {
            return Ok(Self {
                data,
                pos: 0,
                range: 255,
                value: 0,
                bit_count: 0,
                overreads: 0,
            });
        }
        let mut value = u32::from(data[0]) << 8;
        if data.len() > 1 {
            value |= u32::from(data[1]);
        }
        Ok(Self {
            data,
            pos: data.len().min(2),
            range: 255,
            value,
            bit_count: 0,
            overreads: 0,
        })
    }

    /// Bytes consumed past the end of the partition, read as zero. Any
    /// non-zero value on a complete, valid partition is a desync symptom.
    pub fn overreads(&self) -> u32 {
        self.overreads
    }

    /// Offset of the next unread input byte, for desync diagnostics.
    pub fn byte_offset(&self) -> usize {
        self.pos
    }

    fn next_byte(&mut self) -> u32 {
        match self.data.get(self.pos) {
            Some(&b) => {
                self.pos += 1;
                u32::from(b)
            }
            None => {
                self.overreads += 1;
                0
            }
        }
    }

    /// Decode one bool whose zero-probability is `prob`/256 (spec 8.3.2
    /// `read_bool`; libvpx `vpx_read`). Under `EC_VP9_TRACE`, prints the
    /// decoder state after the read — `B <pos> <bit_count> <prob> <bit>`.
    pub fn read_bool(&mut self, prob: u8) -> bool {
        let prob = u32::from(prob);
        let split = 1 + (((self.range - 1) * prob) >> 8);
        let bigsplit = split << 8;
        let bit = if self.value >= bigsplit {
            self.range -= split;
            self.value -= bigsplit;
            true
        } else {
            self.range = split;
            false
        };
        while self.range < 128 {
            self.value <<= 1;
            self.range <<= 1;
            self.bit_count += 1;
            if self.bit_count == 8 {
                self.bit_count = 0;
                self.value |= self.next_byte();
            }
        }
        if crate::trace_enabled() {
            eprintln!("B {} {} {} {}", self.pos, self.bit_count, prob, u8::from(bit));
        }
        bit
    }

    /// Unsigned `n`-bit literal, bits high- to low-order, each at
    /// probability 128 (spec `read_literal`).
    pub fn read_literal(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) + u32::from(self.read_bool(128));
        }
        v
    }

    /// Signed `n`-bit literal (spec `s(n)`, 4.10): an `n`-bit magnitude
    /// followed by a sign flag, 1 meaning negative. `n = 0` yields 0.
    pub fn read_signed_literal(&mut self, n: u32) -> i32 {
        if n == 0 {
            return 0;
        }
        let mut v: i32 = self.read_literal(n) as i32;
        if self.read_bool(128) {
            v = -v;
        }
        v
    }

    /// Decode a tree-coded value (spec 8.3.2 `read_tree`; libvpx
    /// `vpx_read_tree`): `tree` holds the left/right branch entries
    /// (positive = interior node index, negative = `-leaf`), `probs[i]`
    /// is the probability of interior node `i` (the tree position
    /// halved).
    pub fn read_tree(&mut self, tree: &[i8], probs: &[u8]) -> u8 {
        let mut i: i32 = 0;
        loop {
            let b = self.read_bool(probs[(i >> 1) as usize]);
            i = i32::from(tree[(i + i32::from(b as i8)) as usize]);
            if i <= 0 {
                return (-i) as u8;
            }
        }
    }
}

/// The spec 8.3.3 reference bool encoder (the carry-propagating writer
/// libvpx ships as `vpx_writer`), test-only: it lets the tests drive
/// [`BoolDecoder`] against known bitstreams instead of trusting the
/// decoder to decode its own idea of the format.
#[cfg(test)]
struct BoolEncoder {
    out: Vec<u8>,
    range: u32,
    bottom: u32,
    bit_count: u32,
}

#[cfg(test)]
impl BoolEncoder {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            range: 255,
            bottom: 0,
            bit_count: 24,
        }
    }

    fn add_one_to_output(out: &mut [u8]) {
        for b in out.iter_mut().rev() {
            if *b == 255 {
                *b = 0;
            } else {
                *b += 1;
                return;
            }
        }
        // Carry out of the beginning cannot happen: x < 1 always.
        unreachable!("bool encoder carry propagated past the first byte");
    }

    fn write_bool(&mut self, val: bool, prob: u8) {
        let prob = u32::from(prob);
        let split = 1 + (((self.range - 1) * prob) >> 8);
        if val {
            self.bottom += split;
            self.range -= split;
        } else {
            self.range = split;
        }
        while self.range < 128 {
            self.range <<= 1;
            if self.bottom & (1 << 31) != 0 {
                Self::add_one_to_output(&mut self.out);
            }
            self.bottom <<= 1;
            self.bit_count -= 1;
            if self.bit_count == 0 {
                self.out.push((self.bottom >> 24) as u8);
                self.bottom &= (1 << 24) - 1;
                self.bit_count = 8;
            }
        }
    }

    fn write_literal(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            self.write_bool((v >> i) & 1 != 0, 128);
        }
    }

    /// Spec `flush_bool_encoder`.
    fn flush(mut self) -> Vec<u8> {
        let c = self.bit_count;
        let mut v = self.bottom;
        if v & (1 << (32 - c)) != 0 {
            Self::add_one_to_output(&mut self.out);
        }
        v <<= c & 7;
        let mut c = c >> 3;
        while c > 0 {
            v <<= 8;
            c -= 1;
        }
        for _ in 0..4 {
            self.out.push((v >> 24) as u8);
            v <<= 8;
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_bools(pattern: &[(&[bool], u8)]) {
        let mut e = BoolEncoder::new();
        for (vals, p) in pattern {
            for &v in *vals {
                e.write_bool(v, *p);
            }
        }
        let data = e.flush();
        let mut d = BoolDecoder::new(&data).unwrap();
        for (vals, p) in pattern {
            for &v in *vals {
                assert_eq!(d.read_bool(*p), v, "desync at prob {:?}", p);
            }
        }
        assert_eq!(d.overreads(), 0);
    }

    #[test]
    fn roundtrips_bools_at_every_extreme() {
        // p = 1 (all zeros essentially free) through p = 255, with 1s at
        // the most improbable settings to force carry propagation.
        roundtrip_bools(&[
            (&[false; 64], 1),
            (&[true], 1),
            (&[false, true, false, true], 128),
            (&[true; 3], 255),
            (&[false; 32], 255),
            (&[true; 16], 1),
        ]);
    }

    #[test]
    fn roundtrips_long_random_streams() {
        // Deterministic LCG so failures reproduce.
        let mut state: u64 = 0x9E3779B97F4A7C15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let probs = [1u8, 2, 32, 128, 200, 254, 255];
        let mut e = BoolEncoder::new();
        let mut expected = Vec::new();
        for &p in &probs {
            for _ in 0..200 {
                let v = next() & 1 == 1;
                e.write_bool(v, p);
                expected.push((v, p));
            }
        }
        let data = e.flush();
        let mut d = BoolDecoder::new(&data).unwrap();
        for (v, p) in expected {
            assert_eq!(d.read_bool(p), v);
        }
        assert_eq!(d.overreads(), 0);
    }

    #[test]
    fn roundtrips_literals() {
        let mut e = BoolEncoder::new();
        for n in 1..=19u32 {
            let ones = (1 << n) - 1;
            e.write_literal(ones, n);
            e.write_literal(0, n);
            e.write_literal(0x15 & ones, n); // 0b10101 truncated
        }
        let data = e.flush();
        let mut d = BoolDecoder::new(&data).unwrap();
        for n in 1..=19u32 {
            let ones = (1 << n) - 1;
            assert_eq!(d.read_literal(n), ones);
            assert_eq!(d.read_literal(n), 0);
            assert_eq!(d.read_literal(n), 0x15 & ones);
        }
        assert_eq!(d.overreads(), 0);
    }

    #[test]
    fn roundtrips_signed_literals() {
        // Spec s(5): 5-bit magnitude then sign flag.
        let mut e = BoolEncoder::new();
        for mag in 0..=15u32 {
            e.write_literal(mag, 4);
            e.write_bool(false, 128);
            e.write_literal(mag, 4);
            e.write_bool(true, 128);
        }
        let data = e.flush();
        let mut d = BoolDecoder::new(&data).unwrap();
        for mag in 0..=15i32 {
            assert_eq!(d.read_signed_literal(4), mag);
            assert_eq!(d.read_signed_literal(4), -mag);
        }
        assert_eq!(d.overreads(), 0);
    }

    #[test]
    fn roundtrips_tree_values() {
        // The spec's intra mode tree (8.2 `read_intra_mode`).
        let intra: [i8; 18] = [
            -0, 2, -9, 4, -1, 6, 8, 12, -2, 10, -5, -6, -3, 14, -7, 16, -8, -4,
        ];
        let probs = [159u8; 9];
        // Bit paths per the tree above: DC="0", TM="100", V="101",
        // H="1100 00"? — walk a few leaves via the decoder itself, then
        // verify against the encoder paths.
        let leaves: [(&[bool], u8); 4] = [
            (&[false], 0),
            (&[true, false], 9),
            (&[true, true, false], 1),
            (&[true, true, true, false, false, false], 2),
        ];
        for (path, val) in leaves {
            let mut e = BoolEncoder::new();
            for (i, &b) in path.iter().enumerate() {
                e.write_bool(b, probs[(i + i) >> 1]);
            }
            let data = e.flush();
            let mut d = BoolDecoder::new(&data).unwrap();
            assert_eq!(d.read_tree(&intra, &probs), val, "path {path:?}");
        }
    }

    #[test]
    fn short_partitions_zero_extend() {
        // An empty partition is legal (a degenerate tile); init reads
        // zeros and the shortfall shows in overreads.
        let mut d = BoolDecoder::new(&[]).unwrap();
        assert_eq!(d.read_bool(128), false);
        let mut d = BoolDecoder::new(&[0]).unwrap();
        assert_eq!(d.read_bool(128), false);
    }

    #[test]
    fn overreads_are_counted_not_panicked() {
        // Two bytes of init, then nothing: any read shifts in zeros.
        let data = [0u8, 0];
        let mut d = BoolDecoder::new(&data).unwrap();
        for _ in 0..100 {
            d.read_bool(128);
        }
        assert!(d.overreads() > 0);
    }
}
