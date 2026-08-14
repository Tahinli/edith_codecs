//! Prefix-code decoding for the spectral and scalefactor codebooks.
//!
//! Every codebook is a complete prefix code (Kraft sum exactly 1, asserted in
//! the tests), so a binary tree walk is total: no bit pattern falls off the
//! tree, and the only failure a stream can produce is running out of bits.

use ec_core::{BitReader, Error, Result};

/// A codebook as a walkable binary tree. `nodes[i]` holds the two children of
/// node `i`; a non-negative child is another node, a negative one is the leaf
/// `-(index + 1)`.
#[derive(Debug, Clone)]
pub struct Tree {
    nodes: Vec<[i32; 2]>,
}

impl Tree {
    /// Builds the tree from `(length, code)` by symbol index.
    ///
    /// # Panics
    /// If two symbols share a prefix, which would mean a corrupt table.
    pub fn new(codes: &[(u8, u32)]) -> Tree {
        let mut nodes = vec![[0i32; 2]];
        for (symbol, &(len, code)) in codes.iter().enumerate() {
            let mut at = 0usize;
            for depth in (0..len).rev() {
                let bit = ((code >> depth) & 1) as usize;
                let last = depth == 0;
                if last {
                    assert_eq!(nodes[at][bit], 0, "codebook prefix collision");
                    nodes[at][bit] = -(symbol as i32 + 1);
                } else if nodes[at][bit] == 0 {
                    nodes.push([0i32; 2]);
                    let next = nodes.len() as i32 - 1;
                    nodes[at][bit] = next;
                    at = next as usize;
                } else {
                    at = nodes[at][bit] as usize;
                }
            }
        }
        Tree { nodes }
    }

    /// Reads one symbol index.
    pub fn decode(&self, r: &mut BitReader<'_>) -> Result<usize> {
        let mut at = 0usize;
        loop {
            let bit = usize::from(r.read_bit()?);
            let next = self.nodes[at][bit];
            if next < 0 {
                return Ok((-next - 1) as usize);
            }
            if next == 0 {
                return Err(Error::corrupt("aac: undefined Huffman code"));
            }
            at = next as usize;
        }
    }
}

/// The eleven spectral trees plus the scalefactor tree, built once per decoder.
#[derive(Debug, Clone)]
pub struct Books {
    pub spectral: Vec<Tree>,
    pub scalefactor: Tree,
}

impl Books {
    pub fn new() -> Books {
        Books {
            spectral: crate::tables::CODEBOOKS
                .iter()
                .map(|cb| Tree::new(cb.codes))
                .collect(),
            scalefactor: Tree::new(crate::tables::SCALEFACTOR_CODES),
        }
    }
}

impl Default for Books {
    fn default() -> Books {
        Books::new()
    }
}
