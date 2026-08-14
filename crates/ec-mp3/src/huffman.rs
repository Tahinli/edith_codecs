//! Layer III Huffman coding: the table registry, a decode tree built once per
//! process, and the encoder's cost/write side.

use crate::huffman_tables::*;
use ec_core::bitio::{BitReader, BitWriter};
use ec_core::error::{Error, Result};
use std::sync::OnceLock;

/// One code table as the bitstream refers to it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Table {
    /// `(length, code)` indexed by `x * dim + y`.
    pub codes: &'static [(u8, u16)],
    /// Values per axis; `x` and `y` run `0..dim`, the top one being an escape
    /// when `linbits` is non-zero.
    pub dim: u16,
    /// Extra magnitude bits read when a value is `dim - 1`.
    pub linbits: u8,
}

/// The 32 big-value table slots. 4 and 14 are not assigned by the standard;
/// 16..=23 and 24..=31 share one code table each and differ only in `linbits`.
pub(crate) fn big_table(select: usize) -> Result<Table> {
    let t = |codes: &'static [(u8, u16)], dim: u16, linbits: u8| Table {
        codes,
        dim,
        linbits,
    };
    Ok(match select {
        0 => t(&[], 0, 0),
        1 => t(&T1, 2, 0),
        2 => t(&T2, 3, 0),
        3 => t(&T3, 3, 0),
        5 => t(&T5, 4, 0),
        6 => t(&T6, 4, 0),
        7 => t(&T7, 6, 0),
        8 => t(&T8, 6, 0),
        9 => t(&T9, 6, 0),
        10 => t(&T10, 8, 0),
        11 => t(&T11, 8, 0),
        12 => t(&T12, 8, 0),
        13 => t(&T13, 16, 0),
        15 => t(&T15, 16, 0),
        16..=23 => t(&T16, 16, LINBITS_16[select - 16]),
        24..=31 => t(&T24, 16, LINBITS_24[select - 24]),
        _ => {
            return Err(Error::corrupt(format!(
                "mp3: Huffman table {select} is not assigned"
            )));
        }
    })
}

const LINBITS_16: [u8; 8] = [1, 2, 3, 4, 6, 8, 10, 13];
const LINBITS_24: [u8; 8] = [4, 5, 6, 7, 8, 9, 11, 13];

/// The two count1 (quadruple) tables, selected by `count1table_select`.
pub(crate) fn count1_table(select: bool) -> &'static [(u8, u16)] {
    if select { &C1 } else { &C0 }
}

/// A decode tree: two child slots per node, a negative slot being
/// `-(value_index + 1)`.
type Tree = Vec<[i32; 2]>;

fn build_tree(codes: &[(u8, u16)]) -> Tree {
    let mut tree: Tree = vec![[0, 0]];
    for (index, &(len, code)) in codes.iter().enumerate() {
        let mut node = 0usize;
        let code = u32::from(code);
        for bit in (0..u32::from(len)).rev() {
            let branch = ((code >> bit) & 1) as usize;
            if bit == 0 {
                tree[node][branch] = -(index as i32 + 1);
            } else {
                if tree[node][branch] == 0 {
                    tree.push([0, 0]);
                    tree[node][branch] = (tree.len() - 1) as i32;
                }
                node = tree[node][branch] as usize;
            }
        }
    }
    tree
}

struct Trees {
    big: Vec<Option<Tree>>,
    count1: [Tree; 2],
}

fn trees() -> &'static Trees {
    static TREES: OnceLock<Trees> = OnceLock::new();
    TREES.get_or_init(|| {
        let mut big = Vec::with_capacity(32);
        for select in 0..32 {
            big.push(match big_table(select) {
                Ok(t) if !t.codes.is_empty() => Some(build_tree(t.codes)),
                _ => None,
            });
        }
        Trees {
            big,
            count1: [build_tree(&C0), build_tree(&C1)],
        }
    })
}

fn walk(tree: &Tree, reader: &mut BitReader<'_>) -> Result<usize> {
    let mut node = 0usize;
    loop {
        let branch = usize::from(reader.read_bit()?);
        let next = tree[node][branch];
        if next < 0 {
            return Ok((-next - 1) as usize);
        }
        if next == 0 {
            return Err(Error::corrupt("mp3: Huffman code not in table"));
        }
        node = next as usize;
    }
}

/// Decodes one big-value pair into `out`, sign bits and linbits included.
pub(crate) fn decode_pair(
    reader: &mut BitReader<'_>,
    select: usize,
    table: Table,
    out: &mut [f32; 2],
) -> Result<()> {
    let tree = trees().big[select]
        .as_ref()
        .ok_or_else(|| Error::corrupt(format!("mp3: Huffman table {select} is not assigned")))?;
    let index = walk(tree, reader)?;
    let values = [
        (index as u16 / table.dim) as u32,
        (index as u16 % table.dim) as u32,
    ];
    // Order matters and is per value, not per pair: escape bits then sign for
    // x, then the same for y. Grouping them costs the same number of bits,
    // which is why only the samples go wrong.
    for (slot, mut value) in out.iter_mut().zip(values) {
        if table.linbits > 0 && value == u32::from(table.dim) - 1 {
            value += reader.read_bits(u32::from(table.linbits))?;
        }
        *slot = if value == 0 {
            0.0
        } else if reader.read_bit()? {
            -(value as f32)
        } else {
            value as f32
        };
    }
    Ok(())
}

/// Decodes one count1 quadruple: four values of 0 or +-1.
pub(crate) fn decode_quad(
    reader: &mut BitReader<'_>,
    select: bool,
    out: &mut [f32; 4],
) -> Result<()> {
    let tree = &trees().count1[usize::from(select)];
    let index = walk(tree, reader)?;
    for (slot, shift) in out.iter_mut().zip([3, 2, 1, 0]) {
        let value = (index >> shift) & 1;
        *slot = if value == 0 {
            0.0
        } else if reader.read_bit()? {
            -1.0
        } else {
            1.0
        };
    }
    Ok(())
}

/// Bits one pair costs in `table`, or `None` when the pair is out of range.
pub(crate) fn pair_bits(table: Table, x: u32, y: u32) -> Option<u32> {
    let max = u32::from(table.dim) - 1;
    let escape = u32::from(table.linbits) > 0;
    let (ix, iy) = (x.min(max), y.min(max));
    if (x > max && !escape) || (y > max && !escape) {
        return None;
    }
    if escape && (x > max + (1 << table.linbits) - 1 || y > max + (1 << table.linbits) - 1) {
        return None;
    }
    let (len, _) = table.codes[(ix * u32::from(table.dim) + iy) as usize];
    if len == 0 && !(x == 0 && y == 0) {
        return None;
    }
    let mut bits = u32::from(len);
    for value in [x, y] {
        if value >= max && escape {
            bits += u32::from(table.linbits);
        }
        if value != 0 {
            bits += 1;
        }
    }
    Some(bits)
}

/// Writes one pair; the caller has already checked it fits the table.
pub(crate) fn write_pair(writer: &mut BitWriter, table: Table, x: i32, y: i32) {
    let max = u32::from(table.dim) - 1;
    let escape = table.linbits > 0;
    let (ax, ay) = (x.unsigned_abs(), y.unsigned_abs());
    let (ix, iy) = (ax.min(max), ay.min(max));
    let (len, code) = table.codes[(ix * u32::from(table.dim) + iy) as usize];
    writer.write_bits(u32::from(code), u32::from(len));
    for (value, abs) in [(x, ax), (y, ay)] {
        if abs >= max && escape {
            writer.write_bits(abs - max, u32::from(table.linbits));
        }
        if abs != 0 {
            writer.write_bit(value < 0);
        }
    }
}

/// Bits one count1 quadruple costs.
pub(crate) fn quad_bits(select: bool, quad: [i32; 4]) -> u32 {
    let index = quad
        .iter()
        .fold(0usize, |acc, v| acc * 2 + usize::from(*v != 0));
    let (len, _) = count1_table(select)[index];
    u32::from(len) + quad.iter().filter(|v| **v != 0).count() as u32
}

/// Writes one count1 quadruple.
pub(crate) fn write_quad(writer: &mut BitWriter, select: bool, quad: [i32; 4]) {
    let index = quad
        .iter()
        .fold(0usize, |acc, v| acc * 2 + usize::from(*v != 0));
    let (len, code) = count1_table(select)[index];
    writer.write_bits(u32::from(code), u32::from(len));
    for value in quad {
        if value != 0 {
            writer.write_bit(value < 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every table is a complete prefix code over its whole value grid: the
    /// property the measurement rig checked before these numbers were written,
    /// re-checked here so a hand edit cannot break it unnoticed.
    #[test]
    fn tables_are_complete_prefix_codes() {
        let mut seen = 0;
        for select in 0..32 {
            let Ok(table) = big_table(select) else {
                continue;
            };
            if table.codes.is_empty() {
                continue;
            }
            seen += 1;
            assert_eq!(
                table.codes.len(),
                usize::from(table.dim) * usize::from(table.dim)
            );
            let kraft: f64 = table
                .codes
                .iter()
                .map(|(len, _)| 2f64.powi(-i32::from(*len)))
                .sum();
            assert!(
                (kraft - 1.0).abs() < 1e-12,
                "table {select}: Kraft sum {kraft}"
            );
            let tree = build_tree(table.codes);
            assert_eq!(
                tree.iter().flatten().filter(|n| **n < 0).count(),
                table.codes.len()
            );
        }
        assert_eq!(seen, 29, "tables 0..31 minus the four unassigned slots");
        for select in [false, true] {
            let codes = count1_table(select);
            let kraft: f64 = codes
                .iter()
                .map(|(len, _)| 2f64.powi(-i32::from(*len)))
                .sum();
            assert!((kraft - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn pairs_round_trip_through_every_table() {
        for select in (1..32).filter(|s| big_table(*s).is_ok()) {
            let table = big_table(select).unwrap();
            if table.codes.is_empty() {
                continue;
            }
            let max = u32::from(table.dim) - 1;
            let mut writer = BitWriter::new();
            let mut wanted = Vec::new();
            for x in 0..=max {
                for y in 0..=max {
                    let (sx, sy) = (x as i32, -(y as i32));
                    write_pair(&mut writer, table, sx, sy);
                    wanted.push([sx as f32, sy as f32]);
                }
            }
            let bytes = writer.into_bytes();
            let mut reader = BitReader::new(&bytes);
            for want in wanted {
                let mut got = [0.0f32; 2];
                decode_pair(&mut reader, select, table, &mut got).unwrap();
                assert_eq!(got, want, "table {select}");
            }
        }
    }

    #[test]
    fn quads_round_trip() {
        for select in [false, true] {
            let mut writer = BitWriter::new();
            let mut wanted = Vec::new();
            for index in 0..16 {
                let quad = [
                    (index >> 3) & 1,
                    -((index >> 2) & 1),
                    (index >> 1) & 1,
                    -(index & 1),
                ];
                write_quad(&mut writer, select, quad);
                wanted.push(quad.map(|v| v as f32));
            }
            let bytes = writer.into_bytes();
            let mut reader = BitReader::new(&bytes);
            for want in wanted {
                let mut got = [0.0f32; 4];
                decode_quad(&mut reader, select, &mut got).unwrap();
                assert_eq!(got, want);
            }
        }
    }
}
