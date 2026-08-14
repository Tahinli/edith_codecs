//! Canonical Huffman decoding through a two-level lookup table.
//!
//! A code is at most fifteen bits, so one flat 32768-entry table would decode
//! in a single load — and cost more to build than most Matroska blocks cost to
//! inflate. Instead the first `root_bits` bits index a small root table; the
//! few codes longer than that point at a sub-table indexed by their remaining
//! bits. Both levels are one indexed load, so no code is ever walked bit by bit.
//!
//! Codes are stored bit-reversed, because DEFLATE packs a code high bit first
//! into a stream that is read low bit first: reversing at build time turns
//! decoding into "peek `root_bits`, index".

use crate::Error;
use crate::bits::Bits;

/// Longest code RFC 1951 permits.
pub(crate) const MAX_BITS: u32 = 15;

/// Entry layout: `0` is "no code here"; otherwise bit 31 marks a sub-table
/// link (`sub_bits` in 24..28, offset in 0..24) and a leaf holds the symbol in
/// 4..20 with its code length in 0..4.
const SUB_FLAG: u32 = 1 << 31;

pub(crate) struct Table {
    root_bits: u32,
    root: Vec<u32>,
    subs: Vec<u32>,
}

impl Table {
    /// Build from one code length per symbol (`0` = symbol absent).
    ///
    /// `root_bits` is the first-level width; codes longer than it get a
    /// sub-table. An over-subscribed set is always corrupt. An incomplete one
    /// is corrupt too, except for the single-code case RFC 1951 allows for a
    /// distance tree that a stream never actually uses.
    pub(crate) fn new(lengths: &[u16], root_bits: u32) -> Result<Table, Error> {
        let mut count = [0u32; MAX_BITS as usize + 1];
        let mut max_len = 0;
        for &l in lengths {
            if l as u32 > MAX_BITS {
                return Err(Error::corrupt("Huffman code length above 15"));
            }
            if l > 0 {
                count[l as usize] += 1;
                max_len = max_len.max(l as u32);
            }
        }
        if max_len == 0 {
            // No symbols at all: legal to declare, corrupt to use.
            return Ok(Table {
                root_bits: 1,
                root: vec![0; 2],
                subs: Vec::new(),
            });
        }

        let mut left = 1i64;
        let mut total = 0u32;
        for &at_length in count.iter().skip(1) {
            left = (left << 1) - at_length as i64;
            if left < 0 {
                return Err(Error::corrupt("over-subscribed Huffman code"));
            }
            total += at_length;
        }
        if left > 0 && total > 1 {
            return Err(Error::corrupt("incomplete Huffman code"));
        }

        let mut next = [0u32; MAX_BITS as usize + 2];
        let mut code = 0u32;
        for l in 1..=MAX_BITS as usize {
            code = (code + count[l - 1]) << 1;
            next[l] = code;
        }
        let mut codes = vec![0u32; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l > 0 {
                codes[sym] = next[l as usize];
                next[l as usize] += 1;
            }
        }

        let root_bits = root_bits.min(max_len).max(1);
        let root_mask = (1u32 << root_bits) - 1;
        let mut root = vec![0u32; 1 << root_bits];
        let mut subs = Vec::new();

        // How wide each sub-table has to be, before anything is written into it.
        let mut sub_len = vec![0u32; 1 << root_bits];
        for (sym, &l) in lengths.iter().enumerate() {
            let l = l as u32;
            if l > root_bits {
                let prefix = (reverse(codes[sym], l) & root_mask) as usize;
                sub_len[prefix] = sub_len[prefix].max(l - root_bits);
            }
        }
        for (prefix, &bits) in sub_len.iter().enumerate() {
            if bits > 0 {
                let offset = subs.len() as u32;
                subs.resize(subs.len() + (1 << bits), 0);
                root[prefix] = SUB_FLAG | (bits << 24) | offset;
            }
        }

        for (sym, &l) in lengths.iter().enumerate() {
            let l = l as u32;
            if l == 0 {
                continue;
            }
            let rev = reverse(codes[sym], l);
            let leaf = ((sym as u32) << 4) | l;
            if l <= root_bits {
                let mut i = rev;
                while i < 1 << root_bits {
                    root[i as usize] = leaf;
                    i += 1 << l;
                }
            } else {
                let link = root[(rev & root_mask) as usize];
                let bits = (link >> 24) & 0xf;
                let offset = (link & 0x00ff_ffff) as usize;
                let mut i = rev >> root_bits;
                while i < 1 << bits {
                    subs[offset + i as usize] = leaf;
                    i += 1 << (l - root_bits);
                }
            }
        }

        Ok(Table {
            root_bits,
            root,
            subs,
        })
    }

    /// Decode one symbol, consuming exactly its code.
    #[inline]
    pub(crate) fn decode(&self, bits: &mut Bits) -> Result<u16, Error> {
        bits.refill();
        let mut entry = self.root[bits.peek(self.root_bits) as usize];
        if entry & SUB_FLAG != 0 {
            let width = (entry >> 24) & 0xf;
            let offset = (entry & 0x00ff_ffff) as usize;
            entry = self.subs[offset + bits.peek_at(self.root_bits, width) as usize];
        }
        if entry == 0 {
            return Err(Error::corrupt("Huffman code not in the tree"));
        }
        bits.consume(entry & 0xf)?;
        Ok((entry >> 4) as u16)
    }
}

/// The low `n` bits of `code`, reversed.
fn reverse(code: u32, n: u32) -> u32 {
    code.reverse_bits() >> (32 - n)
}
