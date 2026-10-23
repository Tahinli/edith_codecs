//! Huffman books of the DTS core, built once from the canonical-order
//! (symbol, length) pairs in [`crate::tables_data::HUFF_SRC`]: codes are
//! assigned in table order, each one starting where the previous one ended.

use crate::bits::Bits;
use crate::tables_data::HUFF_SRC;
use std::sync::OnceLock;

/// One book: a direct lookup of `1 << max_len` entries.
pub(crate) struct Vlc {
    max_len: u32,
    /// (symbol, code length); length 0 marks an unused code.
    table: Vec<(i16, u8)>,
}

impl Vlc {
    fn build(pairs: &[(u8, u8)], offset: i16) -> Vlc {
        let max_len = pairs.iter().map(|p| p.1 as u32).max().unwrap_or(1);
        let mut table = vec![(0i16, 0u8); 1usize << max_len];
        let mut code: u64 = 0;
        for &(sym, len) in pairs {
            let len = len as u32;
            let start = (code >> (32 - len)) as usize;
            let span = 1usize << (max_len - len);
            let first = start << (max_len - len);
            for e in table.iter_mut().skip(first).take(span) {
                *e = (sym as i16 + offset, len as u8);
            }
            code += 1u64 << (32 - len);
        }
        Vlc { max_len, table }
    }

    /// Decode one symbol. An unassigned prefix yields 0 and consumes the
    /// maximum code length, which a damaged stream can reach and a valid one
    /// cannot.
    #[inline]
    pub fn read(&self, r: &mut Bits<'_>) -> i32 {
        let idx = r.peek(self.max_len) as usize;
        let (sym, len) = self.table[idx];
        r.skip(if len == 0 { self.max_len as usize } else { len as usize });
        sym as i32
    }
}

pub(crate) struct Books {
    /// `[abits - 1][selector]`.
    pub quant_index: Vec<Vec<Vlc>>,
    pub bit_allocation: Vec<Vlc>,
    pub scale_factor: Vec<Vlc>,
    pub transition_mode: Vec<Vlc>,
}

const GROUPS: [usize; 10] = [1, 3, 3, 3, 3, 7, 7, 7, 7, 7];
const SIZES: [usize; 10] = [3, 5, 7, 9, 13, 17, 25, 33, 65, 129];
const OFFSETS: [i16; 10] = [-1, -2, -3, -4, -6, -8, -12, -16, -32, -64];

pub(crate) fn books() -> &'static Books {
    static BOOKS: OnceLock<Books> = OnceLock::new();
    BOOKS.get_or_init(|| {
        let mut at = 0usize;
        let mut take = |n: usize, off: i16| {
            let v = Vlc::build(&HUFF_SRC[at..at + n], off);
            at += n;
            v
        };
        let mut quant_index = Vec::new();
        for i in 0..10 {
            let mut g = Vec::new();
            for _ in 0..GROUPS[i] {
                g.push(take(SIZES[i], OFFSETS[i]));
            }
            quant_index.push(g);
        }
        let bit_allocation = (0..5).map(|_| take(12, 1)).collect();
        let scale_factor = (0..5).map(|_| take(129, -64)).collect();
        let transition_mode = (0..4).map(|_| take(4, 0)).collect();
        Books {
            quant_index,
            bit_allocation,
            scale_factor,
            transition_mode,
        }
    })
}
