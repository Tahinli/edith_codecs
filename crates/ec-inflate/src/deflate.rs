//! DEFLATE compression (RFC 1951) and its zlib wrapper (RFC 1950), for encoders.
//!
//! The encoder is deliberately simple: greedy LZ77 over a 32 KiB window with a
//! bounded hash chain, emitted as one fixed-Huffman block. When stored blocks
//! would be smaller (incompressible input), the output uses stored blocks
//! instead, so it never grows past the input by more than the framing. The
//! ratio is below zlib's dynamic-Huffman output; every stream is valid DEFLATE
//! and round-trips through [`crate::inflate`].

const WINDOW: usize = 1 << 15;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
/// How many earlier positions with the same hash a match search examines.
const CHAIN: usize = 16;
const HASH_BITS: u32 = 15;
const STORED_MAX: usize = 65_535;

/// Length codes 257..=285: base length, then extra bits.
const LEN_BASE: [usize; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// Distance codes 0..=29: base distance, then extra bits.
const DIST_BASE: [usize; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Compress `input` to a raw DEFLATE stream (RFC 1951).
pub fn deflate_raw(input: &[u8]) -> Vec<u8> {
    let fixed = fixed_block(input);
    let stored_len = input.len() + 5 * input.len().div_ceil(STORED_MAX).max(1);
    if fixed.len() <= stored_len {
        fixed
    } else {
        stored_blocks(input)
    }
}

/// Compress `input` to a zlib stream (RFC 1950): header, DEFLATE data, Adler-32.
pub fn deflate_zlib(input: &[u8]) -> Vec<u8> {
    let body = deflate_raw(input);
    let mut out = Vec::with_capacity(body.len() + 6);
    // CMF 0x78 (deflate, 32 KiB window); FLG 0x9c (default level), 0x789c % 31 == 0.
    out.extend_from_slice(&[0x78, 0x9c]);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crate::adler32(input).to_be_bytes());
    out
}

/// LSB-first bit writer, as DEFLATE packs its fields.
struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    held: u32,
}

impl BitWriter {
    fn new(capacity: usize) -> Self {
        Self {
            out: Vec::with_capacity(capacity),
            acc: 0,
            held: 0,
        }
    }

    /// Append the low `bits` bits of `value`, least significant first.
    fn put(&mut self, value: u32, bits: u32) {
        self.acc |= u64::from(value) << self.held;
        self.held += bits;
        while self.held >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.held -= 8;
        }
    }

    /// Append a Huffman code, which DEFLATE writes most significant bit first.
    fn put_code(&mut self, code: u32, len: u32) {
        self.put(code.reverse_bits() >> (32 - len), len);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.held > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

/// One literal or length symbol under the fixed Huffman code (RFC 1951 3.2.6).
fn put_litlen(w: &mut BitWriter, sym: u32) {
    match sym {
        0..=143 => w.put_code(0x30 + sym, 8),
        144..=255 => w.put_code(0x190 + sym - 144, 9),
        256..=279 => w.put_code(sym - 256, 7),
        _ => w.put_code(0xc0 + sym - 280, 8),
    }
}

fn put_match(w: &mut BitWriter, len: usize, dist: usize) {
    let li = LEN_BASE.partition_point(|&b| b <= len) - 1;
    put_litlen(w, 257 + li as u32);
    w.put((len - LEN_BASE[li]) as u32, LEN_EXTRA[li]);
    let di = DIST_BASE.partition_point(|&b| b <= dist) - 1;
    w.put_code(di as u32, 5);
    w.put((dist - DIST_BASE[di]) as u32, DIST_EXTRA[di]);
}

fn hash(s: &[u8]) -> usize {
    let v = (u32::from(s[0]) << 16) | (u32::from(s[1]) << 8) | u32::from(s[2]);
    (v.wrapping_mul(2_654_435_761) >> (32 - HASH_BITS)) as usize
}

/// A single BFINAL fixed-Huffman block over the whole input.
fn fixed_block(input: &[u8]) -> Vec<u8> {
    let n = input.len();
    let mut w = BitWriter::new(n / 2 + 16);
    w.put(1, 1); // BFINAL
    w.put(1, 2); // BTYPE 01: fixed Huffman codes

    // head[h] and prev[] hold position + 1 (0 means none), so a chain is a
    // ring of the last WINDOW positions.
    let mut head = vec![0u32; 1 << HASH_BITS];
    let mut prev = vec![0u32; WINDOW];
    let insert = |head: &mut [u32], prev: &mut [u32], pos: usize| {
        let h = hash(&input[pos..]);
        prev[pos & (WINDOW - 1)] = head[h];
        head[h] = (pos + 1) as u32;
    };

    let mut i = 0;
    while i < n {
        let mut best_len = 0;
        let mut best_dist = 0;
        if i + MIN_MATCH <= n {
            let mut cand = head[hash(&input[i..])] as usize;
            let max = (n - i).min(MAX_MATCH);
            for _ in 0..CHAIN {
                if cand == 0 {
                    break;
                }
                let p = cand - 1;
                if i - p > WINDOW {
                    break;
                }
                let mut l = 0;
                while l < max && input[p + l] == input[i + l] {
                    l += 1;
                }
                if l > best_len {
                    best_len = l;
                    best_dist = i - p;
                    if l == max {
                        break;
                    }
                }
                cand = prev[p & (WINDOW - 1)] as usize;
            }
            insert(&mut head, &mut prev, i);
        }
        if best_len >= MIN_MATCH {
            put_match(&mut w, best_len, best_dist);
            for pos in i + 1..i + best_len {
                if pos + MIN_MATCH <= n {
                    insert(&mut head, &mut prev, pos);
                }
            }
            i += best_len;
        } else {
            put_litlen(&mut w, u32::from(input[i]));
            i += 1;
        }
    }
    put_litlen(&mut w, 256); // end of block
    w.finish()
}

/// Stored (BTYPE 00) blocks: byte-aligned header, LEN, NLEN, then the bytes.
fn stored_blocks(input: &[u8]) -> Vec<u8> {
    let chunks: Vec<&[u8]> = if input.is_empty() {
        vec![&[]]
    } else {
        input.chunks(STORED_MAX).collect()
    };
    let mut out = Vec::with_capacity(input.len() + 5 * chunks.len());
    let last = chunks.len() - 1;
    for (k, chunk) in chunks.iter().enumerate() {
        out.push(u8::from(k == last)); // BFINAL in bit 0, BTYPE 00, rest padding
        let len = chunk.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{inflate, inflate_zlib};

    fn pseudo_random(n: usize, seed: u32) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (s >> 16) as u8
            })
            .collect()
    }

    #[test]
    fn empty_and_tiny_inputs_round_trip() {
        for input in [&b""[..], b"a", b"ab", b"abc", b"abcabcabc"] {
            let z = deflate_zlib(input);
            assert_eq!(inflate_zlib(&z, usize::MAX).unwrap(), input);
            let raw = deflate_raw(input);
            assert_eq!(inflate(&raw, usize::MAX).unwrap(), input);
        }
    }

    #[test]
    fn repetitive_input_compresses_and_round_trips() {
        let input: Vec<u8> = (0..100_000u32).map(|i| (i % 7) as u8).collect();
        let z = deflate_zlib(&input);
        assert!(z.len() < input.len() / 20, "{} bytes", z.len());
        assert_eq!(inflate_zlib(&z, usize::MAX).unwrap(), input);
    }

    #[test]
    fn random_input_round_trips_within_framing_overhead() {
        let input = pseudo_random(200_000, 7);
        let raw = deflate_raw(&input);
        assert!(raw.len() <= input.len() + 5 * 4, "{} bytes", raw.len());
        assert_eq!(inflate(&raw, usize::MAX).unwrap(), input);
    }

    #[test]
    fn matches_reach_the_longest_and_furthest_codes() {
        // Long runs use length 258; a repeat 32 KiB back uses the last distance code.
        let mut input = pseudo_random(32_768, 3);
        let head = input[..1000].to_vec();
        input.extend_from_slice(&head);
        input.extend(std::iter::repeat_n(9u8, 1000));
        let z = deflate_zlib(&input);
        assert_eq!(inflate_zlib(&z, usize::MAX).unwrap(), input);
    }
}
