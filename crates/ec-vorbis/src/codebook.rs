//! Codebooks: Huffman lengths in, entry numbers and VQ vectors out.
//!
//! Three things make a Vorbis codebook more than a Huffman table. The lengths
//! themselves come in three encodings (flat, sparse, ordered-by-length). The
//! codeword assignment is canonical but stated as "the next free codeword of
//! this length", which is the same thing read from the other end. And a book
//! may carry a *lookup*: entry numbers index a vector of quantised values,
//! either by taking `dimensions` digits of the entry number in base
//! `lookup_values` (type 1, the packing that makes a 2^24-vector book fit in a
//! few hundred bytes) or by indexing the list directly (type 2). Both are
//! expanded once at parse time, so decode is a tree walk and a slice.

use ec_core::{Error, Result};

use crate::bits::{Bits, BitsOut};

/// One parsed codebook.
#[derive(Debug, Clone)]
pub struct Codebook {
    /// Values per entry: 1 for a scalar book, more for a VQ book.
    pub dimensions: usize,
    /// Codeword tree, walked one bit at a time.
    tree: Vec<Node>,
    /// `entries * dimensions` values, empty when `lookup_type` is 0.
    values: Vec<f32>,
}

impl Codebook {
    /// Parse one codebook from a setup header (§3.2.1).
    pub fn parse(bits: &mut Bits) -> Result<Codebook> {
        if bits.read(24) != 0x0056_4342 {
            return Err(Error::corrupt("codebook sync pattern"));
        }
        let dimensions = bits.read(16) as usize;
        let entries = bits.read(24) as usize;
        if bits.eop() {
            return Err(Error::corrupt("codebook header truncated"));
        }
        if dimensions == 0 || entries == 0 {
            return Err(Error::corrupt("empty codebook"));
        }
        // A book cannot be larger than the packet that describes it: every
        // length costs at least one bit, so this is the OOM guard on a header
        // that claims 2^24 entries in twenty bytes.
        if entries as u64 > bits.remaining() + 64 {
            return Err(Error::corrupt("codebook claims more entries than bits"));
        }

        let mut lengths = vec![0u8; entries];
        if bits.bit() {
            // Ordered: lengths are non-decreasing, stated as run lengths.
            let mut entry = 0usize;
            let mut length = bits.read(5) + 1;
            while entry < entries {
                if length > 32 {
                    return Err(Error::corrupt("ordered codebook overran 32 bits"));
                }
                let bits_needed = ilog((entries - entry) as u32);
                let run = bits.read(bits_needed) as usize;
                if entry + run > entries {
                    return Err(Error::corrupt("ordered codebook run past the end"));
                }
                lengths[entry..entry + run].fill(length as u8);
                entry += run;
                length += 1;
                if bits.eop() {
                    return Err(Error::corrupt("ordered codebook truncated"));
                }
            }
        } else {
            let sparse = bits.bit();
            for length in lengths.iter_mut() {
                if sparse && !bits.bit() {
                    continue;
                }
                *length = (bits.read(5) + 1) as u8;
            }
            if bits.eop() {
                return Err(Error::corrupt("codebook lengths truncated"));
            }
        }

        let (tree, _) = assign_codes(&lengths)?;

        let lookup_type = bits.read(4);
        let values = match lookup_type {
            0 => Vec::new(),
            1 | 2 => {
                let minimum = bits.float32();
                let delta = bits.float32();
                let value_bits = bits.read(4) + 1;
                let sequence_p = bits.bit();
                let lookup_values = match lookup_type {
                    1 => lookup1_values(entries, dimensions),
                    _ => entries * dimensions,
                };
                if bits.eop() || lookup_values as u64 > bits.remaining() + 64 {
                    return Err(Error::corrupt("codebook lookup truncated"));
                }
                let multiplicands: Vec<f32> = (0..lookup_values)
                    .map(|_| bits.read(value_bits) as f32)
                    .collect();
                expand_lookup(
                    lookup_type,
                    entries,
                    dimensions,
                    minimum,
                    delta,
                    sequence_p,
                    &multiplicands,
                )?
            }
            other => {
                return Err(Error::unsupported(
                    format!("codebook lookup type {other}"),
                    "Vorbis I defines 0, 1 and 2",
                ));
            }
        };
        if bits.eop() {
            return Err(Error::corrupt("codebook truncated"));
        }

        Ok(Codebook {
            dimensions,
            tree,
            values,
        })
    }

    /// True when entries carry VQ values, i.e. the book can fill a residue or
    /// floor-0 vector rather than only naming a number.
    pub fn has_values(&self) -> bool {
        !self.values.is_empty()
    }

    /// Read one codeword; [`None`] at end of packet or on an undefined path.
    pub fn decode_scalar(&self, bits: &mut Bits) -> Option<u32> {
        let mut node = 0usize;
        loop {
            let child = self.tree[node].child[usize::from(bits.bit())];
            if bits.eop() {
                return None;
            }
            match child {
                0 => return None,
                c if c < 0 => return Some((-c - 1) as u32),
                c => node = c as usize,
            }
        }
    }

    /// Read one codeword and answer its value vector; [`None`] as above.
    pub fn decode_vector(&self, bits: &mut Bits) -> Option<&[f32]> {
        let entry = self.decode_scalar(bits)? as usize;
        self.vector(entry)
    }

    /// The value vector of `entry`, when this book has values.
    pub fn vector(&self, entry: usize) -> Option<&[f32]> {
        let start = entry.checked_mul(self.dimensions)?;
        self.values.get(start..start + self.dimensions)
    }
}

/// Greatest `m` with `m^dimensions <= entries` (§9.2.3), computed without
/// `powf` so the boundary case never lands on the wrong side.
pub fn lookup1_values(entries: usize, dimensions: usize) -> usize {
    let mut value = 1usize;
    loop {
        let mut product = 1usize;
        let mut overflow = false;
        for _ in 0..dimensions {
            match product.checked_mul(value + 1) {
                Some(p) if p <= entries => product = p,
                _ => {
                    overflow = true;
                    break;
                }
            }
        }
        if overflow {
            return value;
        }
        value += 1;
    }
}

/// Turn multiplicands into one value vector per entry (§3.2.1 "VQ lookup").
fn expand_lookup(
    lookup_type: u32,
    entries: usize,
    dimensions: usize,
    minimum: f32,
    delta: f32,
    sequence_p: bool,
    multiplicands: &[f32],
) -> Result<Vec<f32>> {
    let total = entries
        .checked_mul(dimensions)
        .filter(|n| *n <= 1 << 24)
        .ok_or_else(|| Error::corrupt("codebook value table too large"))?;
    let mut values = vec![0.0f32; total];
    let lookup_values = multiplicands.len();
    for entry in 0..entries {
        let mut last = 0.0f32;
        let mut divisor = 1usize;
        for i in 0..dimensions {
            let offset = match lookup_type {
                1 => (entry / divisor) % lookup_values.max(1),
                _ => entry * dimensions + i,
            };
            let value = multiplicands.get(offset).copied().unwrap_or(0.0) * delta + minimum + last;
            values[entry * dimensions + i] = value;
            if sequence_p {
                last = value;
            }
            divisor = divisor.saturating_mul(lookup_values.max(1));
        }
    }
    Ok(values)
}

/// `ilog` of §9.2.1: the position of the highest set bit, zero for zero.
pub fn ilog(mut value: u32) -> u32 {
    let mut bits = 0;
    while value > 0 {
        bits += 1;
        value >>= 1;
    }
    bits
}

/// Codeword assignment (§3.2.1) as a walkable tree, plus the codeword each
/// entry got.
///
/// The rule is *not* the classic sorted-canonical one: entries are walked in
/// their own order and each takes the first codeword of its length that is
/// still free — so a book whose lengths are not already non-decreasing gets a
/// different assignment than sorting by length would give. (A Laplacian residue
/// book, short codes in the middle and long ones at both ends, is exactly that
/// case; getting it wrong desynchronises every packet while still consuming a
/// plausible number of bits.)
///
/// An under-populated tree is accepted rather than refused: single-entry books
/// are legal, and streams in the wild (the Xiph `one-entry-codebook` vector
/// among them) lean on the same leniency. What is refused is an *over*-full
/// tree, where an entry has no codeword left to take.
fn assign_codes(lengths: &[u8]) -> Result<(Vec<Node>, Vec<u32>)> {
    let mut nodes = vec![Node::default()];
    let mut codes = vec![0u32; lengths.len()];
    for (entry, &length) in lengths.iter().enumerate() {
        if length == 0 {
            continue;
        }
        if length > 32 {
            return Err(Error::corrupt("codebook codeword longer than 32 bits"));
        }
        let mut node = 0usize;
        let mut code = 0u32;
        let mut path: Vec<usize> = Vec::with_capacity(usize::from(length));
        for step in 0..length {
            // Depth still to go *below* the branch about to be taken.
            let below = length - step - 1;
            let branch = (0..2)
                .find(|&bit| free_depth(nodes[node].child[bit], &nodes) <= below)
                .ok_or_else(|| Error::corrupt("codebook is over-full"))?;
            code = (code << 1) | branch as u32;
            path.push(node);
            match below {
                0 => nodes[node].child[branch] = -((entry as i32) + 1),
                _ => {
                    if nodes[node].child[branch] == 0 {
                        nodes.push(Node::default());
                        nodes[node].child[branch] = (nodes.len() - 1) as i32;
                    }
                    node = nodes[node].child[branch] as usize;
                }
            }
        }
        // Free depths shrink from the leaf back up to the root.
        for &node in path.iter().rev() {
            let left = free_depth(nodes[node].child[0], &nodes);
            let right = free_depth(nodes[node].child[1], &nodes);
            nodes[node].free = left.min(right).saturating_add(1);
        }
        codes[entry] = code;
    }
    Ok((nodes, codes))
}

/// One node of the codeword tree.
#[derive(Debug, Clone, Copy)]
pub struct Node {
    /// Per branch: 0 free, positive a child index, negative the leaf
    /// `-(entry + 1)`.
    child: [i32; 2],
    /// Shallowest free slot in this subtree, 255 when there is none.
    free: u8,
}

impl Default for Node {
    fn default() -> Node {
        // A fresh node has two free branches, so a codeword can end one step
        // below it.
        Node {
            child: [0, 0],
            free: 1,
        }
    }
}

/// How far below `slot` the shallowest free slot sits; 255 when full.
fn free_depth(slot: i32, nodes: &[Node]) -> u8 {
    match slot {
        0 => 0,
        s if s < 0 => 255,
        s => nodes[s as usize].free,
    }
}

/// A codebook the encoder designed, kept in the shape the setup-header writer
/// and the packet writer both need: lengths to state, codewords to emit.
#[derive(Debug, Clone)]
pub struct CodebookSpec {
    /// Values per entry.
    pub dimensions: usize,
    /// Codeword length per entry, all non-zero (no sparse books written).
    pub lengths: Vec<u8>,
    /// Codeword per entry, MSB-first in `lengths[i]` bits.
    pub codes: Vec<u32>,
    /// Quantised values per entry when the book carries a lookup, empty for a
    /// pure symbol book.
    pub values: Vec<f32>,
}

impl CodebookSpec {
    /// Build a book over `weights` (one per entry, a relative frequency).
    ///
    /// Huffman over the weights, then the canonical assignment the decoder
    /// rebuilds from lengths alone. Every entry gets a codeword — a zero weight
    /// becomes a long one rather than an absent one, because the encoder must
    /// always have *something* to emit for a symbol it can produce.
    pub fn huffman(weights: &[f64], values: Vec<f32>) -> CodebookSpec {
        let lengths = huffman_lengths(weights);
        // The decoder's own rule, run forwards: same order, same choices.
        let (_, codes) = assign_codes(&lengths).expect("a Kraft-complete book always assigns");
        CodebookSpec {
            dimensions: 1,
            lengths,
            codes,
            values,
        }
    }

    /// Number of entries.
    pub fn entries(&self) -> usize {
        self.lengths.len()
    }

    /// Emit the codeword for `entry`.
    pub fn write(&self, out: &mut BitsOut, entry: usize) {
        let length = u32::from(self.lengths[entry]);
        let code = self.codes[entry];
        // The stream is LSB-first but a codeword reads MSB-first, so the bits
        // go out one at a time from the top.
        for depth in (0..length).rev() {
            out.bit((code >> depth) & 1 != 0);
        }
    }
}

/// Huffman code lengths for `weights`, capped at 24 bits.
///
/// The cap is enforced by flattening: a weight small enough to earn a 25-bit
/// codeword is raised until it does not. With at most a few hundred entries and
/// weights within a few decades of each other the cap never binds, and when it
/// does the cost is a fraction of a bit on the rarest symbol.
fn huffman_lengths(weights: &[f64]) -> Vec<u8> {
    let n = weights.len();
    assert!(n > 0, "a codebook needs at least one entry");
    if n == 1 {
        return vec![1];
    }
    let floor = weights.iter().cloned().fold(0.0f64, f64::max) / 8_388_608.0;
    let mut nodes: Vec<(f64, usize)> = weights
        .iter()
        .enumerate()
        .map(|(i, &w)| (w.max(floor).max(f64::MIN_POSITIVE), i))
        .collect();
    // Leaves first, then merged nodes, tracked as a forest over `parent`.
    let mut parent = vec![usize::MAX; n];
    let mut heap: Vec<(f64, usize)> = nodes.clone();
    heap.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let mut merged: Vec<(f64, usize)> = Vec::new();
    let mut next_id = n;
    while heap.len() + merged.len() > 1 {
        let mut take = || -> (f64, usize) {
            let from_heap = heap.last().map(|x| x.0);
            let from_merged = merged.first().map(|x| x.0);
            match (from_heap, from_merged) {
                (Some(a), Some(b)) if a <= b => heap.pop().unwrap(),
                (Some(_), None) => heap.pop().unwrap(),
                _ => merged.remove(0),
            }
        };
        let (wa, a) = take();
        let (wb, b) = take();
        parent.push(usize::MAX);
        parent[a] = next_id;
        parent[b] = next_id;
        merged.push((wa + wb, next_id));
        next_id += 1;
    }
    nodes.clear();

    let mut lengths = vec![0u8; n];
    for (i, length) in lengths.iter_mut().enumerate() {
        let mut node = i;
        let mut depth = 0u32;
        while parent[node] != usize::MAX {
            node = parent[node];
            depth += 1;
        }
        *length = depth.clamp(1, 24) as u8;
    }
    // Clamping can leave the tree over-full; rebalance by lengthening the
    // shallowest entries until Kraft's sum is back at one.
    balance(&mut lengths);
    lengths
}

/// Push a length vector to exactly Kraft-complete, so the decoder's canonical
/// rebuild has no ambiguity and no hole.
fn balance(lengths: &mut [u8]) {
    let kraft = |lengths: &[u8]| -> f64 {
        lengths
            .iter()
            .map(|&l| 2f64.powi(-i32::from(l)))
            .sum::<f64>()
    };
    // Over-full: lengthen the shortest codes.
    while kraft(lengths) > 1.0 + 1e-12 {
        let (index, _) = lengths
            .iter()
            .enumerate()
            .min_by_key(|(_, l)| **l)
            .expect("non-empty");
        lengths[index] += 1;
    }
    // Under-full: shorten the longest, which only ever helps the rare symbols.
    loop {
        let sum = kraft(lengths);
        let (index, &longest) = lengths
            .iter()
            .enumerate()
            .max_by_key(|(_, l)| **l)
            .expect("non-empty");
        if longest <= 1 || sum + 2f64.powi(-i32::from(longest)) > 1.0 + 1e-12 {
            break;
        }
        lengths[index] -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bits::BitsOut;

    #[test]
    fn lookup1_values_matches_the_definition() {
        assert_eq!(lookup1_values(1, 1), 1);
        assert_eq!(lookup1_values(64, 2), 8);
        assert_eq!(lookup1_values(63, 2), 7);
        assert_eq!(lookup1_values(1000, 3), 10);
    }

    #[test]
    fn a_designed_book_decodes_back_through_the_parsed_tree() {
        // Skewed weights so the lengths differ, which is what exercises the
        // canonical assignment on both sides.
        let weights: Vec<f64> = (0..17).map(|i| 1.0 / (1.0 + f64::from(i))).collect();
        let spec = CodebookSpec::huffman(&weights, Vec::new());
        assert_eq!(spec.entries(), 17);
        let kraft: f64 = spec.lengths.iter().map(|&l| 2f64.powi(-i32::from(l))).sum();
        assert!((kraft - 1.0).abs() < 1e-9, "kraft sum {kraft}");

        let mut out = BitsOut::new();
        for entry in 0..spec.entries() {
            spec.write(&mut out, entry);
        }
        let bytes = out.finish();

        let (tree, _) = assign_codes(&spec.lengths).expect("tree");
        let book = Codebook {
            dimensions: 1,
            tree,
            values: Vec::new(),
        };
        let mut bits = Bits::new(&bytes);
        for entry in 0..spec.entries() {
            assert_eq!(book.decode_scalar(&mut bits), Some(entry as u32));
        }
    }
}
