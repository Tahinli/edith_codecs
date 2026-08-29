//! lane-wedge r3: `COMPOUND_WEDGE` mask codebook -- libaom
//! `av1/common/reconinter.c`'s `init_wedge_master_masks`/`init_wedge_masks`/
//! `get_wedge_mask_inplace`, ported for the ONLY block-size family this
//! decoder's masked-compound leaves reach: square 8x8/16x16/32x32
//! (`decode.rs`'s `wedge_bsize` match on `side`). Every reachable square
//! bsize uses the same libaom codebook row (`wedge_codebook_16_heqw`) and
//! the same `wedge_signflip_lookup` row, so only ONE codebook/signflip
//! table is ported (rect bsizes are not reachable yet -- charter scope).
//!
//! VERIFICATION (class shared-oracle-blindness): `lanes/wedge_dump.c` is a
//! standalone C reimplementation of the same libaom source, compiled and
//! run independently with `gcc` (not linked against this Rust code, not
//! linked against libaom) -- it catches translation bugs (indexing, shift
//! direction/sign, offset order) that a hand-transcribed-only table would
//! not. `checksum()` below reproduces its exact per-block checksum
//! formula; `wedge_codebook_matches_c_dump` asserts every (bsize, sign,
//! index) checksum against `lanes/wedge_dump.expected.txt`.

const MASK_MASTER_SIZE: usize = 64;
const WEDGE_WEIGHT_BITS: u32 = 6;
pub const MAX_WEDGE_TYPES: usize = 16;

#[derive(Clone, Copy)]
enum Dir {
    Horizontal = 0,
    Vertical = 1,
    Oblique27 = 2,
    Oblique63 = 3,
    Oblique117 = 4,
    Oblique153 = 5,
}
const N_DIR: usize = 6;

const WEDGE_MASTER_OBLIQUE_ODD: [u8; MASK_MASTER_SIZE] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 6,
    18, 37, 53, 60, 63, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
    64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
];
const WEDGE_MASTER_OBLIQUE_EVEN: [u8; MASK_MASTER_SIZE] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 4, 11,
    27, 46, 58, 62, 63, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
    64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
];
const WEDGE_MASTER_VERTICAL: [u8; MASK_MASTER_SIZE] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 7,
    21, 43, 57, 62, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
    64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
];

/// `wedge_codebook_16_heqw` (reconinter.c) -- (direction, x_offset, y_offset).
const HEQW: [(Dir, i32, i32); MAX_WEDGE_TYPES] = [
    (Dir::Oblique27, 4, 4),
    (Dir::Oblique63, 4, 4),
    (Dir::Oblique117, 4, 4),
    (Dir::Oblique153, 4, 4),
    (Dir::Horizontal, 4, 2),
    (Dir::Horizontal, 4, 6),
    (Dir::Vertical, 2, 4),
    (Dir::Vertical, 6, 4),
    (Dir::Oblique27, 4, 2),
    (Dir::Oblique27, 4, 6),
    (Dir::Oblique153, 4, 2),
    (Dir::Oblique153, 4, 6),
    (Dir::Oblique63, 2, 4),
    (Dir::Oblique63, 6, 4),
    (Dir::Oblique117, 2, 4),
    (Dir::Oblique117, 6, 4),
];
/// `wedge_signflip_lookup` row shared by `BLOCK_8X8`/`BLOCK_16X16`/`BLOCK_32X32`.
const SIGNFLIP: [u8; MAX_WEDGE_TYPES] = [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1, 1, 1, 0, 1];

/// `shift_copy` (reconinter.c) -- shift a `MASK_MASTER_SIZE`-wide row right
/// (`shift >= 0`) or left, replicating the edge sample into the gap.
fn shift_copy(src: &[u8; MASK_MASTER_SIZE], dst: &mut [u8], shift: i32) {
    let w = MASK_MASTER_SIZE;
    if shift >= 0 {
        let shift = shift as usize;
        dst[shift..w].copy_from_slice(&src[..w - shift]);
        dst[..shift].fill(src[0]);
    } else {
        let shift = (-shift) as usize;
        dst[..w - shift].copy_from_slice(&src[shift..]);
        dst[w - shift..].fill(src[w - 1]);
    }
}

/// `wedge_mask_obl[neg][direction]`, a flat `MASK_MASTER_SIZE`-square plane
/// per (neg, direction). Built once by [`init_wedge_master_masks`].
struct MasterMasks {
    /// `[neg][direction] -> MASK_MASTER_SIZE * MASK_MASTER_SIZE` bytes.
    obl: [[Vec<u8>; N_DIR]; 2],
}

/// `init_wedge_master_masks` (reconinter.c) ported verbatim.
fn init_wedge_master_masks() -> MasterMasks {
    let w = MASK_MASTER_SIZE;
    let stride = MASK_MASTER_SIZE;
    let mut obl: [[Vec<u8>; N_DIR]; 2] = Default::default();
    for neg in 0..2 {
        for d in 0..N_DIR {
            obl[neg][d] = vec![0u8; w * w];
        }
    }
    let mut shift = (w / 4) as i32;
    for i in (0..w).step_by(2) {
        {
            let row_start = i * stride;
            shift_copy(
                &WEDGE_MASTER_OBLIQUE_EVEN,
                &mut obl[0][Dir::Oblique63 as usize][row_start..row_start + w],
                shift,
            );
        }
        shift -= 1;
        {
            let row_start = (i + 1) * stride;
            shift_copy(
                &WEDGE_MASTER_OBLIQUE_ODD,
                &mut obl[0][Dir::Oblique63 as usize][row_start..row_start + w],
                shift,
            );
        }
        obl[0][Dir::Vertical as usize][i * stride..i * stride + w]
            .copy_from_slice(&WEDGE_MASTER_VERTICAL);
        obl[0][Dir::Vertical as usize][(i + 1) * stride..(i + 1) * stride + w]
            .copy_from_slice(&WEDGE_MASTER_VERTICAL);
    }
    for i in 0..w {
        for j in 0..w {
            let msk = obl[0][Dir::Oblique63 as usize][i * stride + j] as i32;
            obl[0][Dir::Oblique27 as usize][j * stride + i] = msk as u8;
            let flip = ((1i32 << WEDGE_WEIGHT_BITS) - msk) as u8;
            obl[0][Dir::Oblique117 as usize][i * stride + w - 1 - j] = flip;
            obl[0][Dir::Oblique153 as usize][(w - 1 - j) * stride + i] = flip;
            obl[1][Dir::Oblique63 as usize][i * stride + j] = flip;
            obl[1][Dir::Oblique27 as usize][j * stride + i] = flip;
            obl[1][Dir::Oblique117 as usize][i * stride + w - 1 - j] = msk as u8;
            obl[1][Dir::Oblique153 as usize][(w - 1 - j) * stride + i] = msk as u8;
            let mskx = obl[0][Dir::Vertical as usize][i * stride + j] as i32;
            obl[0][Dir::Horizontal as usize][j * stride + i] = mskx as u8;
            let flipx = ((1i32 << WEDGE_WEIGHT_BITS) - mskx) as u8;
            obl[1][Dir::Vertical as usize][i * stride + j] = flipx;
            obl[1][Dir::Horizontal as usize][j * stride + i] = flipx;
        }
    }
    MasterMasks { obl }
}

/// `get_wedge_mask_inplace` (reconinter.c): the `bw x bh` window of the
/// master plane for this `(wedge_index, neg, bw, bh)`, still in
/// `MASK_MASTER_SIZE`-stride coordinates (caller extracts the window).
fn wedge_window(master: &MasterMasks, wedge_index: usize, neg: usize, bw: usize, bh: usize) -> (usize, usize, usize) {
    let (dir, xo, yo) = HEQW[wedge_index];
    let wsignflip = SIGNFLIP[wedge_index] as usize;
    let woff = (xo as usize * bw) >> 3;
    let hoff = (yo as usize * bh) >> 3;
    let _ = &master.obl[neg ^ wsignflip][dir as usize];
    let row0 = MASK_MASTER_SIZE / 2 - hoff;
    let col0 = MASK_MASTER_SIZE / 2 - woff;
    (neg ^ wsignflip, dir as usize, row0 * MASK_MASTER_SIZE + col0)
}

/// Per-`(sign, wedge_index)` `bw x bh` mask, contiguous stride `bw`.
pub struct WedgeCodebook {
    /// `[sign][wedge_index] -> bw*bh` bytes, row stride `bw`.
    masks: [[Vec<u8>; MAX_WEDGE_TYPES]; 2],
    pub bw: usize,
    pub bh: usize,
}

impl WedgeCodebook {
    fn build(master: &MasterMasks, bw: usize, bh: usize) -> Self {
        let mut masks: [[Vec<u8>; MAX_WEDGE_TYPES]; 2] = Default::default();
        for sign in 0..2 {
            for idx in 0..MAX_WEDGE_TYPES {
                let (neg_idx, dir, off) = wedge_window(master, idx, sign, bw, bh);
                let plane = &master.obl[neg_idx][dir];
                let mut m = vec![0u8; bw * bh];
                for i in 0..bh {
                    let src = off + i * MASK_MASTER_SIZE;
                    m[i * bw..i * bw + bw].copy_from_slice(&plane[src..src + bw]);
                }
                masks[sign][idx] = m;
            }
        }
        WedgeCodebook { masks, bw, bh }
    }

    /// `wedge_sign` in `{0,1}` (spec `wedge_sign`), `wedge_index` in
    /// `0..MAX_WEDGE_TYPES`. Mask is luma-resolution, row stride `bw`.
    pub fn mask(&self, wedge_sign: usize, wedge_index: usize) -> &[u8] {
        &self.masks[wedge_sign][wedge_index]
    }
}

/// One codebook per reachable square leaf side (8/16/32), built once.
pub struct WedgeMasks {
    pub side8: WedgeCodebook,
    pub side16: WedgeCodebook,
    pub side32: WedgeCodebook,
}

impl WedgeMasks {
    fn new() -> Self {
        let master = init_wedge_master_masks();
        WedgeMasks {
            side8: WedgeCodebook::build(&master, 8, 8),
            side16: WedgeCodebook::build(&master, 16, 16),
            side32: WedgeCodebook::build(&master, 32, 32),
        }
    }

    /// `side` is the square leaf side (8/16/32); panics otherwise (same
    /// invariant `decode.rs`'s `wedge_bsize` match already enforces).
    pub fn codebook(&self, side: usize) -> &WedgeCodebook {
        match side {
            8 => &self.side8,
            16 => &self.side16,
            32 => &self.side32,
            _ => unreachable!("wedge codebook side is 8/16/32"),
        }
    }
}

static WEDGE_MASKS: std::sync::OnceLock<WedgeMasks> = std::sync::OnceLock::new();

pub fn wedge_masks() -> &'static WedgeMasks {
    WEDGE_MASKS.get_or_init(WedgeMasks::new)
}

/// Reproduces `lanes/wedge_dump.c`'s `dump_bsize` checksum formula exactly.
#[cfg(test)]
fn checksum(mask: &[u8], bw: usize, bh: usize) -> u64 {
    let mut sum: u64 = 0;
    for i in 0..bh {
        for j in 0..bw {
            sum += mask[i * bw + j] as u64 * (i * bw + j + 1) as u64;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    /// lane-wedge r3: checksum-verify every (bsize, sign, wedge_index) mask
    /// this decoder can produce against `lanes/wedge_dump.c`'s independent
    /// C computation (`lanes/wedge_dump.expected.txt`, generated by
    /// `gcc -O2 lanes/wedge_dump.c -o lanes/wedge_dump && ./lanes/wedge_dump`).
    #[test]
    fn wedge_codebook_matches_c_dump() {
        let expected_txt = std::fs::read_to_string(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../lanes/wedge_dump.expected.txt"),
        )
        .expect("run gcc -O2 lanes/wedge_dump.c -o lanes/wedge_dump && ./lanes/wedge_dump > lanes/wedge_dump.expected.txt first");
        let mut expected = std::collections::HashMap::new();
        for line in expected_txt.lines() {
            // "8x8 sign=0 idx=0 checksum=39141"
            let parts: Vec<&str> = line.split_whitespace().collect();
            let name = parts[0];
            let sign: usize = parts[1]["sign=".len()..].parse().unwrap();
            let idx: usize = parts[2]["idx=".len()..].parse().unwrap();
            let cs: u64 = parts[3]["checksum=".len()..].parse().unwrap();
            expected.insert((name, sign, idx), cs);
        }
        let masks = wedge_masks();
        let mut checked = 0;
        for (name, side) in [("8x8", 8usize), ("16x16", 16), ("32x32", 32)] {
            let cb = masks.codebook(side);
            for sign in 0..2 {
                for idx in 0..MAX_WEDGE_TYPES {
                    let got = checksum(cb.mask(sign, idx), side, side);
                    let want = *expected.get(&(name, sign, idx)).unwrap_or_else(|| {
                        panic!("missing expected entry for {name} sign={sign} idx={idx}")
                    });
                    assert_eq!(
                        got, want,
                        "{name} sign={sign} idx={idx}: rust checksum {got} != C dump {want}"
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 3 * 2 * MAX_WEDGE_TYPES, "checked every (bsize,sign,index) triple");
    }
}
