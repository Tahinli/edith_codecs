//! Software decoder for this crate's own key-frame tile payloads (spec 5.11),
//! the reader half of [`crate::tile`]'s writer.
//!
//! `tile.rs` is another lane's territory this round (`lane-av1-rect` is
//! touching its partition search), so nothing here imports its private
//! helpers: the handful of `pub(crate)` items it already exposes
//! ([`crate::tile::block_grid`], [`crate::tile::has_half`],
//! [`crate::tile::INTRA_MODE_CTX`], [`crate::tile::write_golomb`]) are reused,
//! and everything else (the partition/coefficient context arithmetic, the
//! scan table) is a from-spec reimplementation kept in step with the writer by
//! the gate tests below rather than by sharing code with it.
//!
//! # Scope (round 1)
//! Key frames only, and only the shapes the encoder actually still needs to
//! prove itself against: every coded block is `DC_PRED` (this crate's mode
//! search names other intra modes, but a decoder for those needs the
//! directional-prediction edge-reach plumbing [`crate::intra::predict`]
//! already carries; wiring it up is round 2), never skipped, and every
//! superblock is a whole number of 64x64 units (a frame whose true edge falls
//! inside one, forcing the gathered-CDF partial-superblock path, is round 2
//! too). Anything outside that refuses with [`Error::unsupported`] rather than
//! silently miscoding.

use ec_av1_syntax::{CdefParams, LoopFilterParams};
use ec_core::{Error, Result};

use crate::cdf;
use crate::cdf_state::{Cdfs, MvComponentCdfs, TxbSet, TxbTables};
use crate::encode::{Picture, Reach};
use crate::intra::predict;
use crate::mc;
use crate::msac::SymbolDecoder;
use crate::mvstack::{MiGrid, MiInfo, find_mv_stack, single_ref_ctx};
use crate::tile::{INTRA_MODE_CTX, block_grid, has_half};
use crate::transform::{TxType, dequant_and_inverse_typed};

const PARTITION_NONE: usize = 0;
const PARTITION_SPLIT: usize = 3;
const SB_MI: u32 = 16;
const BLOCK_MI: u32 = 8;

/// How many `use_filter_intra` symbols this decoder has read as `1`, across
/// every call in the process -- the cheap counter [`filter_intra_hits`] gate
/// tests read (before/after, not the absolute value) to prove a stream
/// actually exercised the filter-intra predictor rather than silently
/// skipping it.
static FILTER_INTRA_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Current value of [`FILTER_INTRA_HITS`].
pub(crate) fn filter_intra_hits() -> usize {
    FILTER_INTRA_HITS.load(std::sync::atomic::Ordering::Relaxed)
}

/// How many 4-pixel deblocking edge groups [`edge_params`] has actually
/// selected for filtering, across every call in the process -- the same
/// before/after counter pattern as [`FILTER_INTRA_HITS`], proving a stream
/// exercised `apply_deblock` rather than every edge landing on level 0.
static DEBLOCK_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Current value of [`DEBLOCK_HITS`].
pub(crate) fn deblock_hits() -> usize {
    DEBLOCK_HITS.load(std::sync::atomic::Ordering::Relaxed)
}

/// How many [`read_tx_size`] reads resolved a `tx_depth` strictly less than
/// the block's own side, across every call in the process -- the same
/// before/after counter pattern as [`FILTER_INTRA_HITS`], proving a stream
/// actually exercised a *split* transform under `TxMode::Select` rather than
/// every block coincidentally resolving `depth=0` (indistinguishable from
/// `TxMode::Largest` pixel-wise).
static TX_DEPTH_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Current value of [`TX_DEPTH_HITS`].
pub(crate) fn tx_depth_hits() -> usize {
    TX_DEPTH_HITS.load(std::sync::atomic::Ordering::Relaxed)
}
const SUB_MI: u32 = 4;
const MI: usize = 4;
const SB: usize = 64;
const BLOCK: usize = 32;
const SUB: usize = 16;
const TX32: usize = 32;
const TX16: usize = 16;
const TX8: usize = 8;
const TX4: usize = 4;
const DC_PRED: usize = 0;
/// `V_PRED` (spec 6.10.2): the first directional mode, and so the first that
/// carries an angle delta — [`crate::tile`]'s own private copy of the same
/// constant.
const V_PRED: usize = 1;
/// The last directional mode, `D67_PRED`.
const D67_PRED: usize = 8;
/// The symbol an angle delta of zero codes as (spec 9.3): the alphabet runs
/// from -3 to +3, so this is its middle — [`crate::tile`]'s writer never
/// codes anything else, so this decoder never reads anything else either.
const ANGLE_DELTA_ZERO: usize = 3;
const NUM_BASE_LEVELS: i32 = 2;
const COEFF_BASE_RANGE: i32 = 12;
const BR_STEP: i32 = 3;
const MAX_BR_LEVEL: i32 = NUM_BASE_LEVELS + COEFF_BASE_RANGE;

/// `partition_gather_vert_alike`/`_horz_alike` (spec 9.3): the partition types
/// whose probability mass a superblock or block half outside the frame
/// gathers into its one remaining flag — [`crate::tile`]'s own private copy,
/// duplicated here (that module is another lane's territory this round) and
/// kept in step with it by the byte-exact round-trip tests below rather than
/// by sharing code.
const VERT_ALIKE: [usize; 6] = [2, PARTITION_SPLIT, 4, 6, 7, 9];
const HORZ_ALIKE: [usize; 6] = [1, PARTITION_SPLIT, 4, 5, 6, 8];

fn element_prob(cdf: &[u16], element: usize) -> u16 {
    cdf[element] - if element > 0 { cdf[element - 1] } else { 0 }
}

/// The two-symbol CDF a partial superblock/block's forced-split flag is read
/// with: the mass of `elements` becomes the probability of the one partition
/// the frame edge still allows, mirroring [`crate::tile`]'s own `gather`
/// (the encoder's non-adapting counterpart to this read).
fn gather(cdf: &[u16], elements: [usize; 6]) -> [u16; 3] {
    let split: u16 = elements.iter().map(|&e| element_prob(cdf, e)).sum();
    [32768 - split, 32768, 0]
}

fn unsupported(what: impl Into<String>) -> Error {
    Error::unsupported("AV1 tile", what.into())
}

/// The coefficient q-context a frame's `base_q_idx` picks its default CDFs
/// from (spec `Get_Qctx`), mirroring [`crate::tile`]'s private copy of the
/// same rule.
pub(crate) fn q_ctx_of(base_q_idx: u8) -> usize {
    match base_q_idx {
        0..=20 => 0,
        21..=60 => 1,
        61..=120 => 2,
        _ => 3,
    }
}

/// `Default_Scan_NxN` (spec 8.4.2), the same anti-diagonal walk
/// [`crate::tile`]'s writer generates its scan tables with.
fn default_scan(side: usize) -> Vec<u16> {
    let mut scan = Vec::with_capacity(side * side);
    for d in 0..(2 * side - 1) {
        let lo = d.saturating_sub(side - 1);
        let hi = d.min(side - 1);
        let diagonal = (lo..=hi).map(|row| (row * side + (d - row)) as u16);
        if d % 2 == 0 {
            scan.extend(diagonal.rev());
        } else {
            scan.extend(diagonal);
        }
    }
    scan
}

/// `Mrow_Scan`/`Mcol_Scan` (spec 8.4.2, libaom `scan.c`): the two
/// class-specific scans a `V_DCT`/`H_DCT` transform block reads coefficients
/// in, instead of the default zigzag. libaom's own tables are keyed by a
/// *column-major* raster (`get_txb_bhl`'s `col = pos / height`) over its
/// physically column-major level buffer, but they decompose to the exact
/// same (row, col) walk as a flat row-major traversal of *our* row-major
/// `grid`/`levels` (`pos = row * side + col`) once you read them that way:
/// `Mrow_Scan` visits row 0 across every column, then row 1, ... — precisely
/// `0..side*side` in our own indexing — and `Mcol_Scan` is that walk
/// transposed (column 0 down every row, then column 1, ...).
fn class_scan_table(side: usize, class: TxClass) -> Vec<u16> {
    match class {
        TxClass::Vert => (0..(side * side) as u16).collect(),
        TxClass::Horiz => (0..side)
            .flat_map(|col| (0..side).map(move |row| (row * side + col) as u16))
            .collect(),
        TxClass::TwoD => unreachable!("class_scan is only for V_DCT/H_DCT"),
    }
}

fn eob_coeff_ctx(scan_idx: usize, area: usize) -> usize {
    match scan_idx {
        0 => 0,
        i if i <= area / 8 => 1,
        i if i <= area / 4 => 2,
        _ => 3,
    }
}

fn neighbour(grid: &[i32], side: usize, row: usize, col: usize) -> i32 {
    if row >= side || col >= side {
        0
    } else {
        grid[row * side + col]
    }
}

/// spec 5.11.39's three `TxClass`es (`tx_type_to_class`, libaom
/// `txb_common.h`): everything but the two lone-axis-identity types this
/// decoder reads (`V_DCT`/`H_DCT`) is `TX_CLASS_2D`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TxClass {
    TwoD,
    Horiz,
    Vert,
}

impl TxClass {
    fn of(tx_type: TxType) -> Self {
        match tx_type {
            TxType::VDct => Self::Vert,
            TxType::HDct => Self::Horiz,
            _ => Self::TwoD,
        }
    }
}

/// `nz_map_ctx_offset_1d`: `TX_CLASS_HORIZ`/`TX_CLASS_VERT` share one 32-entry
/// table (indexed by the position along the transform's *coded* axis --
/// `col` for horiz, `row` for vert) that places their contexts past the 2D
/// table's own 26 rows (`SIG_COEF_CONTEXTS_2D`), 16 rows total
/// (`SIG_COEF_CONTEXTS_1D`) split 26/31/36.
fn nz_map_ctx_offset_1d(i: usize) -> usize {
    match i {
        0 => 26,
        1 => 31,
        _ => 36,
    }
}

fn base_ctx(grid: &[i32], side: usize, row: usize, col: usize, class: TxClass) -> usize {
    if row == 0 && col == 0 {
        return 0;
    }
    let offsets: [(usize, usize); 5] = match class {
        TxClass::TwoD => [(1, 0), (0, 1), (1, 1), (2, 0), (0, 2)],
        TxClass::Horiz => [(1, 0), (0, 1), (0, 2), (0, 3), (0, 4)],
        TxClass::Vert => [(1, 0), (0, 1), (2, 0), (3, 0), (4, 0)],
    };
    let mag: i32 = offsets
        .iter()
        .map(|&(dr, dc)| neighbour(grid, side, row + dr, col + dc).abs().min(3))
        .sum();
    let ctx = ((mag + 1) >> 1).min(4) as usize;
    match class {
        TxClass::TwoD => ctx + cdf::NZ_MAP_CTX_OFFSET_32[row.min(4)][col.min(4)] as usize,
        TxClass::Horiz => ctx + nz_map_ctx_offset_1d(col.min(31)),
        TxClass::Vert => ctx + nz_map_ctx_offset_1d(row.min(31)),
    }
}

fn br_ctx(grid: &[i32], side: usize, row: usize, col: usize, class: TxClass) -> usize {
    let extra = match class {
        TxClass::TwoD => neighbour(grid, side, row + 1, col + 1).abs(),
        TxClass::Horiz => neighbour(grid, side, row, col + 2).abs(),
        TxClass::Vert => neighbour(grid, side, row + 2, col).abs(),
    };
    let mag = neighbour(grid, side, row + 1, col).abs()
        + neighbour(grid, side, row, col + 1).abs()
        + extra;
    let mag = (((mag + 1) >> 1).min(6)) as usize;
    if row == 0 && col == 0 {
        return mag;
    }
    let near_origin = match class {
        TxClass::TwoD => row < 2 && col < 2,
        TxClass::Horiz => col == 0,
        TxClass::Vert => row == 0,
    };
    if near_origin { mag + 7 } else { mag + 14 }
}

fn dc_vote(dc: Option<bool>) -> i32 {
    match dc {
        None => 0,
        Some(true) => -1,
        Some(false) => 1,
    }
}

fn dc_sign_ctx(vote: i32) -> usize {
    match vote.signum() {
        0 => 0,
        -1 => 1,
        _ => 2,
    }
}

/// The remainder of a level the base and base-range syntax could not reach
/// (spec 5.11.40): its bit length in unary, then that many of its own bits,
/// most significant first — the exact inverse of [`crate::tile::write_golomb`].
fn read_golomb(dec: &mut SymbolDecoder) -> Result<u32> {
    let mut length = 1u32;
    while dec.literal(1) == 0 {
        length += 1;
        if length > 20 {
            return Err(unsupported("a Golomb tail longer than this decoder reads"));
        }
    }
    let mut x = 1u32;
    for _ in 1..length {
        x = (x << 1) | dec.literal(1);
    }
    Ok(x - 1)
}

/// `decode_symbol`'s end-of-block position (spec 5.11.39): the inverse of
/// [`crate::tile`]'s `write_eob`.
fn read_eob(dec: &mut SymbolDecoder, coding: &mut TxbTables, class: TxClass) -> usize {
    const GROUP_START: [usize; 12] = [0, 1, 2, 3, 5, 9, 17, 33, 65, 129, 257, 513];
    const OFFSET_BITS: [u32; 12] = [0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9];

    let trace = std::env::var_os("EC_AV1_TRACE").is_some();
    // libaom's `eob_flag_cdf*` tables carry a second dimension the 2D scan
    // never touches: `TX_CLASS_HORIZ`/`TX_CLASS_VERT` (`V_DCT`/`H_DCT`) adapt
    // a wholly separate CDF from every other tx_type (`decodetxb.c`'s
    // `eob_multi_ctx`). Missing that split desynced the very first
    // `V_DCT`/`H_DCT` TU this decoder ever produced (lane-av1tx4 r5, caught
    // against a real aomdec trace).
    let eob_pt: &mut [u16] = match (class, coding.eob_pt_class1.as_deref_mut()) {
        (TxClass::TwoD, _) | (_, None) => coding.eob_pt,
        (_, Some(class1)) => class1,
    };
    let group = dec.symbol(eob_pt) + 1;
    if trace {
        eprintln!("TRACE eob_pt value={group}");
    }
    let bits = OFFSET_BITS[group];
    let mut offset = 0u32;
    if bits > 0 {
        let top = dec.symbol(&mut coding.eob_extra[group - 3]) as u32;
        if trace {
            eprintln!("TRACE eob_extra ctx={} value={top}", group - 3);
        }
        offset = top << (bits - 1);
        if bits > 1 {
            offset |= dec.literal(bits - 1);
        }
    }
    GROUP_START[group] + offset as usize
}

/// One transform block's levels, the inverse of [`crate::tile`]'s
/// `write_coeffs`, returned as a `side * side` grid in raster order, along
/// with the transform type that grid was coded under (`DCT_DCT` when the
/// block's `TxbSet` carries no `tx_type` symbol at all, e.g. 32-point and
/// 64-point transforms, which never code anything else).
///
/// # Errors
/// Returns an error if the `tx_type` symbol decodes to a value outside its
/// CDF's own set (a corrupt or unsupported bitstream) — every value the
/// intra `Tx_Type_Intra_Inv_Set2` and inter `Tx_Type_Inter_Inv_Set3` CDFs can
/// produce dispatches to a real inverse transform.
fn read_coeffs(
    dec: &mut SymbolDecoder,
    coding: &mut TxbTables,
    scan: &[u16],
    skip_ctx: usize,
    sign_ctx: usize,
) -> Result<(Vec<i32>, TxType)> {
    let trace = std::env::var_os("EC_AV1_TRACE").is_some();
    let side = coding.side;
    let mut grid = vec![0i32; side * side];
    let all_zero = dec.symbol(&mut coding.txb_skip[skip_ctx]) == 1;
    if trace {
        eprintln!("TRACE all_zero ctx={skip_ctx} value={}", all_zero as i32);
    }
    if all_zero {
        return Ok((grid, TxType::DctDct));
    }
    let mut tx_type = TxType::DctDct;
    if let Some(tx_type_cdf) = coding.tx_type.as_deref_mut() {
        // The CDF row's own width names its set: 7 symbols (8 slots) is
        // `TX_SET_INTRA_1`, 5 (6 slots) the reduced `TX_SET_INTRA_2` (the
        // inter sets we read share set 2's symbol order).
        let set1 = tx_type_cdf.len() == 8;
        let t = dec.symbol(tx_type_cdf);
        if trace {
            eprintln!("TRACE tx_type value={t} set1={set1}");
        }
        tx_type = if set1 {
            TxType::from_symbol_set1(t)
        } else {
            TxType::from_symbol(t)
        }
        .ok_or_else(|| unsupported(format!("a tx_type symbol outside its CDF's own set: {t}")))?;
    }

    let class = TxClass::of(tx_type);
    let eob = read_eob(dec, coding, class);
    if trace {
        eprintln!("TRACE eob value={eob}");
    }
    let class_scan;
    let scan: &[u16] = if class == TxClass::TwoD {
        scan
    } else {
        class_scan = class_scan_table(side, class);
        &class_scan
    };
    let mut levels = vec![0i32; side * side];
    for scan_idx in (0..eob).rev() {
        let pos = scan[scan_idx] as usize;
        let (row, col) = (pos / side, pos % side);
        let level = if scan_idx == eob - 1 {
            let ctx = eob_coeff_ctx(scan_idx, side * side);
            let v = dec.symbol(&mut coding.base_eob[ctx]) as i32 + 1;
            if trace {
                eprintln!(
                    "TRACE base_eob scan_idx={scan_idx} pos={pos} row={row} col={col} ctx={ctx} value={v}"
                );
            }
            v
        } else {
            let ctx = base_ctx(&levels, side, row, col, class);
            let v = dec.symbol(&mut coding.base[ctx]) as i32;
            if trace {
                eprintln!(
                    "TRACE base scan_idx={scan_idx} pos={pos} row={row} col={col} ctx={ctx} value={v}"
                );
            }
            v
        };
        let level = if level > NUM_BASE_LEVELS {
            let ctx = br_ctx(&levels, side, row, col, class);
            let mut level = level;
            let mut sent = 0;
            loop {
                let k = dec.symbol(&mut coding.br[ctx]) as i32;
                if trace {
                    eprintln!(
                        "TRACE br scan_idx={scan_idx} pos={pos} row={row} col={col} ctx={ctx} value={k}"
                    );
                }
                level += k;
                sent += BR_STEP;
                if k < BR_STEP || sent >= COEFF_BASE_RANGE {
                    break;
                }
            }
            level
        } else {
            level
        };
        levels[pos] = level;
    }

    for &pos in &scan[..eob] {
        let level = levels[pos as usize];
        if level == 0 {
            continue;
        }
        let negative = if pos == 0 {
            let v = dec.symbol(&mut coding.dc_sign[sign_ctx]) == 1;
            if trace {
                eprintln!("TRACE dc_sign ctx={sign_ctx} value={}", v as i32);
            }
            v
        } else {
            dec.literal(1) == 1
        };
        let level = if level.abs_diff(0) as i32 > MAX_BR_LEVEL {
            let g = read_golomb(dec)?;
            if trace {
                eprintln!("TRACE golomb pos={pos} value={g}");
            }
            level + g as i32
        } else {
            level
        };
        grid[pos as usize] = if negative { -level } else { level };
    }
    Ok((grid, tx_type))
}

/// What one coded block leaves behind for the blocks that read it as a
/// neighbour: whether it coded anything at all, and the sign of its DC —
/// [`crate::tile`]'s own private `Neighbour`.
#[derive(Clone, Copy, Default)]
struct Neighbour {
    /// The transform unit's cumulative coefficient level (spec `cul_level`,
    /// `decodetxb.c`'s `read_coeffs_txb`): the sum of every coded level's
    /// magnitude, clamped to `COEFF_CONTEXT_MASK` (7). A plain "coded or not"
    /// boolean (`level != 0`) is enough for every context this decoder read
    /// until now, but the *luma* `txb_skip_ctx` of a transform unit smaller
    /// than its own block (`TxMode::Select`, spec `get_txb_ctx_general`)
    /// needs the actual magnitude tier of its above/left neighbours, not
    /// just whether they coded anything.
    level: u8,
    dc: Option<bool>,
}

fn neighbour_state(grid: &[i32]) -> Neighbour {
    let cul: u32 = grid.iter().map(|&l| l.unsigned_abs()).sum();
    Neighbour {
        level: cul.min(7) as u8,
        dc: (grid[0] != 0).then_some(grid[0] < 0),
    }
}

/// spec `get_txb_ctx_general`'s `skip_contexts[top][left]` table (plane 0,
/// transform unit smaller than the block it sits in): `top`/`left` are the
/// above/left neighbours' magnitude tiers, each already clamped to 4.
const SKIP_CONTEXTS: [[usize; 5]; 5] = [
    [1, 2, 2, 2, 3],
    [2, 4, 4, 4, 5],
    [2, 4, 4, 4, 5],
    [2, 4, 4, 4, 5],
    [3, 5, 5, 5, 6],
];

/// The neighbour state a block's contexts are read from — a duplicate of
/// [`crate::tile`]'s own private `Neighbours` (that module is another lane's
/// territory this round), kept at the same two granularities the real writer
/// tracks: `above_mode`/`left_mode`/`above_side`/`left_side` on the 16x16
/// (`SUB`) grid the partition/mode symbols read, and `above`/`left`'s
/// per-plane coded/DC-sign state on the finer 4x4 (`MI`) grid the coefficient
/// contexts read (spec `av1_set_entropy_contexts`) — which is what lets a
/// chroma transform straddling the true, odd-sized frame edge see its
/// neighbour's state clamped at its own subsampled edge rather than luma's.
struct Neighbours {
    above: Vec<[Neighbour; 3]>,
    left: Vec<[Neighbour; 3]>,
    above_mode: Vec<usize>,
    left_mode: Vec<usize>,
    above_side: Vec<usize>,
    left_side: Vec<usize>,
    /// The same as `above_side`/`left_side`, but kept at the finer mi (4x4)
    /// granularity `above`/`left` are, rather than [`SUB`] -- [`crate::tile`]'s
    /// `above_side_mi`/`left_side_mi`: two 8x8 leaves of one straddling 16x16
    /// block (lane-av1-rect) share a single [`SUB`]-grid cell, so a coarse
    /// array cannot tell the second leaf's partition symbol that the first
    /// one was coded at 8x8.
    above_side_mi: Vec<usize>,
    left_side_mi: Vec<usize>,
    /// Whether the block above/left of this column/row was coded `skip` --
    /// an inter frame's own `skip` context (spec `SkipContext`), which the
    /// key-frame writer never tracks (its skip context is always zero); a
    /// duplicate of [`crate::tile`]'s private `Neighbours::above_skip`/
    /// `left_skip`.
    above_skip: Vec<bool>,
    left_skip: Vec<bool>,
    /// Whether the block above/left was coded inter -- the `is_inter`
    /// context (spec `av1_get_intra_inter_context`), a duplicate of
    /// [`crate::tile`]'s private `Neighbours::above_inter`/`left_inter`.
    above_inter: Vec<bool>,
    left_inter: Vec<bool>,
    mi_cols: usize,
    mi_rows: usize,
    /// Per-8x8 (2x2 mi cells) `skip_txfm` flag over the whole padded frame,
    /// spec `av1_cdef_compute_sb_list`'s `is_8x8_block_skip`: CDEF (spec
    /// 7.15) never filters an 8x8 luma block whose covering coded block read
    /// `skip` -- one flag per 4x4 mi cell (finer than needed, but matches the
    /// grain [`Self::record_mi`] already writes at), read back at 8x8
    /// granularity by [`apply_cdef`] since `skip` is constant across every
    /// mi cell one coded block covers.
    skip_grid: Vec<bool>,
    skip_grid_cols_mi: usize,
    /// Per-4x4-mi luma transform width in pixels (8/16/32/64) -- this decoder
    /// never splits a coded block's transform below its own size (round 2:
    /// `TxMode::Select`/`tx_depth` refused), so a block's own side *is* its
    /// transform size, spec 7.14's `get_transform_size`/`get_filter_level`
    /// edge-length lookup (`apply_deblock`).
    tx_grid: Vec<u8>,
    /// Per-4x4-mi `is_inter` flag -- the loop filter's ref/mode delta lookup
    /// (spec 7.14.4 `get_filter_level`; every inter block this decoder
    /// produces is a single `LAST_FRAME` reference with a non-`GLOBALMV`
    /// mode, so `is_inter` alone picks the right `ref_deltas`/`mode_deltas`
    /// entry without a separate ref/mode grid).
    inter_grid: Vec<bool>,
}

impl Neighbours {
    /// `cols`/`rows` are in [`SUB`] units; `mi_cols`/`mi_rows` are the frame's
    /// true (unpadded) size in 4x4 mode-info units.
    fn new(cols: usize, rows: usize, mi_cols: usize, mi_rows: usize) -> Self {
        Self {
            above: vec![[Neighbour::default(); 3]; cols * (SUB / MI)],
            left: vec![[Neighbour::default(); 3]; rows * (SUB / MI)],
            above_mode: vec![DC_PRED; cols],
            left_mode: vec![DC_PRED; rows],
            above_side: vec![SB; cols],
            left_side: vec![SB; rows],
            above_side_mi: vec![SB; cols * (SUB / MI)],
            left_side_mi: vec![SB; rows * (SUB / MI)],
            above_skip: vec![false; cols],
            left_skip: vec![false; rows],
            above_inter: vec![false; cols],
            left_inter: vec![false; rows],
            mi_cols,
            mi_rows,
            skip_grid: vec![false; cols * (SUB / MI) * rows * (SUB / MI)],
            skip_grid_cols_mi: cols * (SUB / MI),
            tx_grid: vec![0u8; cols * (SUB / MI) * rows * (SUB / MI)],
            inter_grid: vec![false; cols * (SUB / MI) * rows * (SUB / MI)],
        }
    }

    /// Marks every 4x4 mi cell a just-decoded coded block covers with its own
    /// transform width (in pixels) and `is_inter` flag -- the loop filter's
    /// per-edge lookup (`apply_deblock`), filled at the same call sites and
    /// units as [`Self::fill_skip_grid`].
    fn fill_lf_grid(&mut self, at_mi: (usize, usize), side_mi: usize, tx_px: u8, is_inter: bool) {
        let (mi_r, mi_c) = at_mi;
        for rr in 0..side_mi {
            for cc in 0..side_mi {
                let idx = (mi_r + rr) * self.skip_grid_cols_mi + (mi_c + cc);
                if let Some(cell) = self.tx_grid.get_mut(idx) {
                    *cell = tx_px;
                }
                if let Some(cell) = self.inter_grid.get_mut(idx) {
                    *cell = is_inter;
                }
            }
        }
    }

    /// Marks every 4x4 mi cell a just-decoded coded block covers with its
    /// `skip` flag -- `at_mi`/`side_mi` are the block's own position and
    /// width/height in 4x4 mode-info units (mirroring [`Self::record_mi`]'s
    /// own units, not [`Self::record`]'s [`SUB`]-grid ones).
    fn fill_skip_grid(&mut self, at_mi: (usize, usize), side_mi: usize, skip: bool) {
        let (mi_r, mi_c) = at_mi;
        for rr in 0..side_mi {
            for cc in 0..side_mi {
                if let Some(cell) = self
                    .skip_grid
                    .get_mut((mi_r + rr) * self.skip_grid_cols_mi + (mi_c + cc))
                {
                    *cell = skip;
                }
            }
        }
    }

    /// Whether the 8x8 luma block at mi position `(mi_r, mi_c)` (2x2 mi
    /// cells) was `skip_txfm` -- [`apply_cdef`]'s dlist membership test.
    /// Spec/libaom `is_8x8_block_skip` requires *every* one of the four 4x4
    /// mi cells the 8x8 covers to be skip (a smaller-than-8x8 partition can
    /// straddle the region with a mix of skip/non-skip blocks).
    /// A single 4x4 mi cell's own `skip` flag, unaggregated (unlike
    /// [`Self::is_skip_txfm`]'s 8x8 all-four-cells rule) -- what the loop
    /// filter's `curr_skipped`/`pv_skip_txfm` (spec 7.14.2) want, since a
    /// filtered edge's two sides are each exactly one coded block wide.
    fn skip_at(&self, mi_r: usize, mi_c: usize) -> bool {
        self.skip_grid
            .get(mi_r * self.skip_grid_cols_mi + mi_c)
            .copied()
            .unwrap_or(false)
    }

    fn is_skip_txfm(&self, mi_r: usize, mi_c: usize) -> bool {
        (0..2).all(|rr| {
            (0..2).all(|cc| {
                self.skip_grid
                    .get((mi_r + rr) * self.skip_grid_cols_mi + (mi_c + cc))
                    .copied()
                    .unwrap_or(false)
            })
        })
    }

    fn start_row(&mut self) {
        self.left.iter_mut().for_each(|l| *l = Default::default());
        self.left_mode.iter_mut().for_each(|m| *m = DC_PRED);
        self.left_side.iter_mut().for_each(|s| *s = SB);
        self.left_side_mi.iter_mut().for_each(|s| *s = SB);
        self.left_skip.iter_mut().for_each(|s| *s = false);
        self.left_inter.iter_mut().for_each(|i| *i = false);
    }

    /// Records a block's `skip`/`is_inter` state for the next block that
    /// reads it as a neighbour -- [`crate::tile`]'s `record_inter`.
    fn record_inter(&mut self, at: (usize, usize), side: usize, skip: bool, is_inter: bool) {
        let (r, c) = at;
        for cell in 0..side / SUB {
            self.above_skip[c + cell] = skip;
            self.left_skip[r + cell] = skip;
            self.above_inter[c + cell] = is_inter;
            self.left_inter[r + cell] = is_inter;
        }
    }

    /// The context of a block's partition symbol: delegates to the mi-precise
    /// reader (same pattern as `around`/`around_mi`) -- `above_side`/
    /// `left_side` are only ever advanced in whole-[`SUB`] steps by
    /// [`Self::record`], so a leaf8's `record_mi` (lane-av1-rect) -- which
    /// only touches the finer mi arrays -- leaves them stale for the *next*
    /// sibling's own partition symbol.
    fn partition_ctx(&self, at: (usize, usize), side: usize) -> usize {
        let (r, c) = at;
        self.partition_ctx_mi((r * (SUB / MI), c * (SUB / MI)), side)
    }

    /// [`Self::partition_ctx`] at mi granularity, for an 8x8 leaf of a
    /// straddling 16x16 block: reads the finer `above_side_mi`/`left_side_mi`
    /// arrays so the second leaf sees the first leaf's own 8x8 side rather
    /// than the enclosing 16x16 slot's stale, shared state.
    fn partition_ctx_mi(&self, (mi_r, mi_c): (usize, usize), side: usize) -> usize {
        2 * usize::from(self.left_side_mi[mi_r] * 2 <= side)
            + usize::from(self.above_side_mi[mi_c] * 2 <= side)
    }

    /// The gathered coded/DC-sign state of the blocks above and to the left
    /// of a block `side` samples across, per plane.
    fn around(&self, (r, c): (usize, usize), side: usize) -> [(bool, bool, i32); 3] {
        self.around_mi((r * (SUB / MI), c * (SUB / MI)), side)
    }

    /// [`Self::around`] taking the block's position directly in 4x4 mode-info
    /// units, for the same reason [`Self::record_mi`] does.
    fn around_mi(&self, (mi_r, mi_c): (usize, usize), side: usize) -> [(bool, bool, i32); 3] {
        let side_mi = side / MI;
        std::array::from_fn(|plane| {
            let mut above_coded = false;
            let mut left_coded = false;
            let mut vote = 0;
            for cell in 0..side_mi {
                let above = &self.above[mi_c + cell][plane];
                let left = &self.left[mi_r + cell][plane];
                above_coded |= above.level != 0;
                left_coded |= left.level != 0;
                vote += dc_vote(above.dc) + dc_vote(left.dc);
            }
            (above_coded, left_coded, vote)
        })
    }

    /// The luma `txb_skip_ctx` of a transform unit smaller than its own block
    /// (spec `get_txb_ctx_general`, plane 0, `plane_bsize != tx_size`): the
    /// above/left neighbours' magnitude tiers, OR-reduced over the unit's own
    /// span and each clamped to 4, indexed into [`SKIP_CONTEXTS`]. Unlike
    /// [`Self::around_mi`]'s plain coded/uncoded bit, this needs the real
    /// cumulative level -- a lone-TU block never calls this (its `txb_skip_ctx`
    /// is always 0, spec's `plane_bsize == tx_size` branch).
    fn luma_skip_ctx(&self, (mi_r, mi_c): (usize, usize), side_mi: usize) -> usize {
        let mut top = 0u8;
        let mut left = 0u8;
        for cell in 0..side_mi {
            top |= self.above[mi_c + cell][0].level;
            left |= self.left[mi_r + cell][0].level;
        }
        let ctx = SKIP_CONTEXTS[(top as usize).min(4)][(left as usize).min(4)];
        if std::env::var_os("EC_AV1_TRACE").is_some() {
            eprintln!(
                "TRACE luma_skip_ctx mi=({mi_r},{mi_c}) side_mi={side_mi} top={top} left={left} ctx={ctx}"
            );
        }
        ctx
    }

    /// Writes one coded block into every cell it covers, on both grids: the
    /// mode/side arrays up to `side`'s own width, and the coefficient-context
    /// arrays up to the true frame edge — the units past it are left at their
    /// default (uncoded), even mid-cell, exactly as [`crate::tile`]'s own
    /// `record` leaves them (spec `av1_set_entropy_contexts`, which clamps to
    /// `blocks_wide`/`blocks_high` derived from the true `mi_cols`/`mi_rows`,
    /// not from this block's own side).
    fn record(&mut self, at: (usize, usize), side: usize, mode: usize, grids: &[Vec<i32>; 3]) {
        let (r, c) = at;
        for cell in 0..side / SUB {
            self.above_mode[c + cell] = mode;
            self.left_mode[r + cell] = mode;
            self.above_side[c + cell] = side;
            self.left_side[r + cell] = side;
        }
        self.record_mi((r * (SUB / MI), c * (SUB / MI)), side, grids);
    }

    /// The coefficient-context half of [`Self::record`], taking the block's
    /// position directly in 4x4 mode-info units rather than [`SUB`]-grid
    /// `(r, c)`: an 8x8 leaf under a straddling 16x16 (lane-av1-rect) sits at
    /// a mi offset [`Self::record`]'s SUB-unit `at` cannot name.
    fn record_mi(&mut self, at_mi: (usize, usize), side: usize, grids: &[Vec<i32>; 3]) {
        let (mi_r, mi_c) = at_mi;
        let states: [Neighbour; 3] = std::array::from_fn(|plane| neighbour_state(&grids[plane]));
        let side_mi = side / MI;
        for cell in 0..side_mi {
            self.left_side_mi[mi_r + cell] = side;
            self.above_side_mi[mi_c + cell] = side;
        }
        // A chroma 4x4 unit straddling the true luma edge is still whole in
        // chroma's own halved grid, so libaom rounds the luma bound up to the
        // plane's own 4x4 unit before clamping a subsampled plane
        // (`av1_write_intra_coeffs_mb`, encodetxb.c).
        let round_up_even = |n: usize| n.div_ceil(2) * 2;
        let bound_h = [
            self.mi_rows,
            round_up_even(self.mi_rows),
            round_up_even(self.mi_rows),
        ];
        let bound_w = [
            self.mi_cols,
            round_up_even(self.mi_cols),
            round_up_even(self.mi_cols),
        ];
        for cell in 0..side_mi {
            self.left[mi_r + cell] = std::array::from_fn(|plane| {
                if cell < side_mi.min(bound_h[plane].saturating_sub(mi_r)) {
                    states[plane]
                } else {
                    Default::default()
                }
            });
            self.above[mi_c + cell] = std::array::from_fn(|plane| {
                if cell < side_mi.min(bound_w[plane].saturating_sub(mi_c)) {
                    states[plane]
                } else {
                    Default::default()
                }
            });
        }
    }

    /// [`Self::record_mi`]'s plane-0 half only, for one luma transform unit
    /// inside a coded block whose luma transform is smaller than its own
    /// side (`TxMode::Select`): the real per-transform-unit coefficient
    /// context (spec `AboveLevelContext`/`LeftLevelContext`), unlike the
    /// coarse per-block `tx_grid`/`fill_lf_grid` state
    /// [`Self::record_split_luma`] still writes once for the whole block.
    /// Leaves `above_side_mi`/`left_side_mi` and the chroma planes alone --
    /// the caller's own [`Self::record_split_luma`] call sets those once,
    /// at the block's own true side, not this TU's.
    fn record_mi_luma(&mut self, (mi_r, mi_c): (usize, usize), tx_px: usize, grid: &[i32]) {
        let state = neighbour_state(grid);
        let side_mi = tx_px / MI;
        for cell in 0..side_mi {
            if mi_r + cell < self.mi_rows {
                self.left[mi_r + cell][0] = state;
            }
            if mi_c + cell < self.mi_cols {
                self.above[mi_c + cell][0] = state;
            }
        }
    }

    /// [`Self::record`]'s mode/side and chroma-context bookkeeping, for a
    /// block whose luma was decoded as several transform units through
    /// [`Self::record_mi_luma`] rather than [`Self::record`]'s own single
    /// whole-block luma write -- everything [`Self::record`] does except
    /// plane 0, which is already correct per-TU by the time this runs.
    fn record_split_luma(
        &mut self,
        at: (usize, usize),
        side: usize,
        mode: usize,
        chroma_grids: [&[i32]; 2],
    ) {
        let (r, c) = at;
        for cell in 0..side / SUB {
            self.above_mode[c + cell] = mode;
            self.left_mode[r + cell] = mode;
            self.above_side[c + cell] = side;
            self.left_side[r + cell] = side;
        }
        let (mi_r, mi_c) = (r * (SUB / MI), c * (SUB / MI));
        let side_mi = side / MI;
        for cell in 0..side_mi {
            self.left_side_mi[mi_r + cell] = side;
            self.above_side_mi[mi_c + cell] = side;
        }
        let round_up_even = |n: usize| n.div_ceil(2) * 2;
        let (bound_h, bound_w) = (round_up_even(self.mi_rows), round_up_even(self.mi_cols));
        for (plane_idx, grid) in chroma_grids.into_iter().enumerate() {
            let plane = plane_idx + 1;
            let state = neighbour_state(grid);
            for cell in 0..side_mi {
                if cell < side_mi.min(bound_h.saturating_sub(mi_r)) {
                    self.left[mi_r + cell][plane] = state;
                }
                if cell < side_mi.min(bound_w.saturating_sub(mi_c)) {
                    self.above[mi_c + cell][plane] = state;
                }
            }
        }
    }
}

/// Reads one coded block's skip flag, luma mode, angle delta (if the mode
/// carries one) and chroma mode, mirroring [`crate::tile`]'s `write_intra_mode`
/// exactly — including its skip context, which the real key-frame writer
/// always writes at a fixed context of zero (it never tracks a neighbour-based
/// one for intra blocks; that is the inter-frame writer's rule instead).
/// Refuses a nonzero angle delta: this decoder reconstructs the thirteen
/// luma modes at their base angle only. Chroma is DC-only or, where CFL is
/// offered (`cfl`, `is_cfl_allowed`, spec 5.11.5), chroma-from-luma -- the
/// third element carries that mode's `(alpha_q3_u, alpha_q3_v)`, `None` for
/// plain DC chroma.
/// A block-size class index into [`cdf::FILTER_INTRA`]/`Cdfs::filter_intra`,
/// or `None` when `side` is past `av1_filter_intra_allowed_bsize`'s <=32
/// bound (spec `filter_intra_mode_info` never reads a symbol there).
fn filter_intra_size_class(side: usize) -> Option<usize> {
    match side {
        4 => Some(0),
        8 => Some(1),
        16 => Some(2),
        32 => Some(3),
        _ => None,
    }
}

fn read_intra_mode(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    above_mode: usize,
    left_mode: usize,
    cfl: bool,
    side: usize,
    enable_filter_intra: bool,
) -> Result<(bool, usize, Option<(i32, i32)>, Option<usize>)> {
    let trace = std::env::var_os("EC_AV1_TRACE").is_some();
    let skip = dec.symbol(&mut cdfs.skip[0]) != 0;
    if trace {
        eprintln!("TRACE skip value={}", skip as i32);
    }
    let above_ctx = INTRA_MODE_CTX[above_mode];
    let left_ctx = INTRA_MODE_CTX[left_mode];
    let mode = dec.symbol(&mut cdfs.kf_y_mode[above_ctx][left_ctx]);
    if trace {
        eprintln!("TRACE y_mode ctx=({above_ctx},{left_ctx}) value={mode}");
    }
    if (V_PRED..=D67_PRED).contains(&mode) {
        let angle = dec.symbol(&mut cdfs.angle_delta[mode - V_PRED]);
        if trace {
            eprintln!("TRACE angle_delta value={angle}");
        }
        if angle != ANGLE_DELTA_ZERO {
            return Err(unsupported(
                "a nonzero angle delta (this encoder never writes one)",
            ));
        }
    }
    let uv_mode = if cfl {
        dec.symbol(&mut cdfs.uv_mode_cfl[mode])
    } else {
        dec.symbol(&mut cdfs.uv_mode_no_cfl[mode])
    };
    if trace {
        eprintln!("TRACE uv_mode cfl={cfl} y_mode={mode} value={uv_mode}");
    }
    let alpha = if uv_mode == DC_PRED {
        None
    } else if cfl && uv_mode == UV_CFL_PRED {
        Some(read_cfl_alphas(dec, cdfs))
    } else {
        return Err(unsupported("a directional chroma mode (round 2)"));
    };
    // `read_filter_intra_mode_info` (spec 5.11.14, libaom decodemv.c): a
    // `use_filter_intra` symbol this decoder never read at all until
    // lane-av1real r10 -- silently skipping it left every bit past a
    // DC_PRED block at <=32x32 read one symbol short of libaom for the rest
    // of the tile (the pinned CfL stream's actual bug: partition/skip/mode
    // matched bit-exact, then `eob_pt` read 5 instead of 6, because this
    // symbol's bits were never consumed).
    let mut filter_intra = None;
    if mode == DC_PRED
        && enable_filter_intra
        && let Some(class) = filter_intra_size_class(side)
    {
        let use_filter_intra = dec.symbol(&mut cdfs.filter_intra[class]) != 0;
        if trace {
            eprintln!(
                "TRACE use_filter_intra side={side} value={}",
                use_filter_intra as i32
            );
        }
        if use_filter_intra {
            FILTER_INTRA_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let fi_mode = dec.symbol(&mut cdfs.filter_intra_mode);
            if trace {
                eprintln!("TRACE filter_intra_mode value={fi_mode}");
            }
            filter_intra = Some(fi_mode);
        }
    }
    Ok((skip, mode, alpha, filter_intra))
}

/// `UV_CFL_PRED` (spec 6.10.19's chroma intra mode enum): the fourteenth
/// entry of [`crate::cdf::UV_MODE_CFL`], one past the thirteen ordinary
/// intra modes.
const UV_CFL_PRED: usize = 13;

/// `read_cfl_alphas` (libaom decodemv.c): the joint sign symbol (8-ary, spec
/// 5.11.45's `cfl_alpha_signs`), then each plane's alpha magnitude symbol
/// (16-ary, `cfl_alpha_u`/`cfl_alpha_v`) where its sign is nonzero -- context
/// indices `CFL_CONTEXT_U`/`CFL_CONTEXT_V` (libaom cfl.h) derived from the
/// joint sign the same way. Returns each plane's final signed `alpha_q3`
/// (`cfl_idx_to_alpha`, cfl.c): magnitude `idx + 1`, sign-zero collapsing to 0.
fn read_cfl_alphas(dec: &mut SymbolDecoder, cdfs: &mut Cdfs) -> (i32, i32) {
    let joint_sign = dec.symbol(&mut cdfs.cfl_sign) as i32;
    let sign_u = ((joint_sign + 1) * 11) >> 5;
    let sign_v = (joint_sign + 1) - 3 * sign_u;
    let alpha_u = if sign_u == 0 {
        0
    } else {
        let ctx_u = (joint_sign + 1 - 3) as usize;
        let mag = dec.symbol(&mut cdfs.cfl_alpha[ctx_u]) as i32 + 1;
        if sign_u == 2 { mag } else { -mag }
    };
    let alpha_v = if sign_v == 0 {
        0
    } else {
        let ctx_v = (sign_v * 3 + sign_u - 3) as usize;
        let mag = dec.symbol(&mut cdfs.cfl_alpha[ctx_v]) as i32 + 1;
        if sign_v == 2 { mag } else { -mag }
    };
    (alpha_u, alpha_v)
}

/// `cfl_luma_subsampling_420_lbd_c` + `subtract_average_c` (libaom cfl.c):
/// the reconstructed luma samples under a `side`-square block, subsampled
/// 4:2:0 to Q3 (`(a+b+c+d) << 1`, each 2x2 group), then block-average
/// subtracted (`round_offset = num_pel/2`, right-shifted by `log2(num_pel)`)
/// to give the AC values [`cfl_scaled`] scales by alpha.
fn cfl_ac_q3(y: &PlaneBuf, px: usize, py: usize, side: usize) -> Vec<i32> {
    let cside = side / 2;
    let mut ac = vec![0i32; cside * cside];
    let mut sum = 0i32;
    for cy in 0..cside {
        for cx in 0..cside {
            let (lx, ly) = (px + cx * 2, py + cy * 2);
            let q3 = (i32::from(y.data[ly * y.width + lx])
                + i32::from(y.data[ly * y.width + lx + 1])
                + i32::from(y.data[(ly + 1) * y.width + lx])
                + i32::from(y.data[(ly + 1) * y.width + lx + 1]))
                << 1;
            ac[cy * cside + cx] = q3;
            sum += q3;
        }
    }
    let num_pel = (cside * cside) as i32;
    let avg = (sum + num_pel / 2) >> num_pel.trailing_zeros();
    ac.iter_mut().for_each(|v| *v -= avg);
    ac
}

/// `get_scaled_luma_q0` (libaom cfl.h): `alpha_q3 * ac_q3`, rounded from Q6
/// back to whole samples with `ROUND_POWER_OF_TWO_SIGNED` (round-to-nearest,
/// ties away from zero on the shifted-out sign).
fn cfl_scaled(alpha_q3: i32, ac_q3: i32) -> i32 {
    let v = alpha_q3 * ac_q3;
    if v >= 0 {
        (v + 32) >> 6
    } else {
        -((-v + 32) >> 6)
    }
}

/// One plane's reconstruction buffer, `width * height` samples, and the
/// prediction/residual reads and writes into it — the decode-side twin of
/// [`crate::encode`]'s private `Plane`, whose `edges` this mirrors exactly
/// (own-edge and reach clamps both), passing what it gathers straight to
/// [`crate::intra::predict`] instead of hand-rolling `DC_PRED` alone.
struct PlaneBuf {
    data: Vec<u8>,
    width: usize,
    height: usize,
    /// The frame's true, decodable extent in this plane's own units — past
    /// this, samples are the padded coding surface's invented tail, never a
    /// real decoder's edge or reach reads.
    true_width: usize,
    true_height: usize,
}

impl PlaneBuf {
    fn edges(
        &self,
        x: usize,
        y: usize,
        side: usize,
        reach: Reach,
    ) -> (Option<Vec<u8>>, Option<Vec<u8>>, Option<u8>) {
        let own_across = x + side.min(self.true_width.saturating_sub(x));
        let across = if reach.above_right {
            own_across + side.min(self.true_width.saturating_sub(own_across))
        } else {
            own_across
        }
        .min(self.width);
        let own_down = y + side.min(self.true_height.saturating_sub(y));
        let down = if reach.below_left {
            own_down + side.min(self.true_height.saturating_sub(own_down))
        } else {
            own_down
        }
        .min(self.height);
        let above = (y > 0 && across > x)
            .then(|| self.data[(y - 1) * self.width + x..][..across - x].to_vec());
        let left = (x > 0 && down > y).then(|| {
            (y..down)
                .map(|row| self.data[row * self.width + x - 1])
                .collect::<Vec<_>>()
        });
        let corner = (x > 0 && y > 0).then(|| self.data[(y - 1) * self.width + x - 1]);
        (above, left, corner)
    }

    /// Predicts under `mode` then adds `residual` (side*side, raster),
    /// writing the clamped reconstruction back into the plane at `(x, y)`.
    /// `cfl`, when `Some((alpha_q3, ac_q3))` (a `UV_CFL_PRED` chroma block),
    /// nudges the prediction by [`cfl_scaled`]'s per-sample amount before the
    /// residual is added — `av1_cfl_predict_block`'s own clip-then-add-residual
    /// order (cfl.c).
    fn reconstruct(
        &mut self,
        x: usize,
        y: usize,
        side: usize,
        mode: usize,
        reach: Reach,
        residual: &[i32],
        cfl: Option<(i32, &[i32])>,
        filter_intra: Option<usize>,
    ) {
        let (above, left, corner) = self.edges(x, y, side, reach);
        let mut prediction = vec![0u8; side * side];
        if let Some(fi_mode) = filter_intra {
            crate::intra::predict_filter_intra(
                fi_mode,
                above.as_deref(),
                left.as_deref(),
                corner,
                side,
                &mut prediction,
            );
        } else {
            predict(
                mode as u8,
                above.as_deref(),
                left.as_deref(),
                corner,
                side,
                &mut prediction,
            );
        }
        for row in 0..side {
            for col in 0..side {
                let idx = row * side + col;
                let mut base = i32::from(prediction[idx]);
                if let Some((alpha_q3, ac_q3)) = cfl {
                    base = (base + cfl_scaled(alpha_q3, ac_q3[idx])).clamp(0, 255);
                }
                let sample = (base + residual[idx]).clamp(0, 255) as u8;
                self.data[(y + row) * self.width + x + col] = sample;
            }
        }
    }
}

/// Reads one plane's transform block, reconstructs it into `plane` at
/// `(x, y)`/`side`, and records what it leaves behind in `neighbours`.
#[allow(clippy::too_many_arguments)]
fn read_plane(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    set: TxbSet,
    scan: &[u16],
    plane_idx: usize,
    around: (bool, bool, i32),
    // The block's own luma mode — [`crate::tile::write_block_planes`] passes
    // it for every plane's `txb` lookup (it is what an intra transform type's
    // CDF is indexed by, spec 9.3), never the plane's own predicted mode.
    tx_mode: usize,
    predict_mode: usize,
    reach: Reach,
    plane: &mut PlaneBuf,
    x: usize,
    y: usize,
    side: usize,
    tx_side: usize,
    base_q_idx: u8,
    cfl: Option<(i32, &[i32])>,
    filter_intra: Option<usize>,
    // Only meaningful for `plane_idx == 0`: `Some` when this transform unit is
    // smaller than its own block (spec `get_txb_ctx_general`'s
    // `plane_bsize != tx_size` branch), carrying the already-looked-up
    // `SKIP_CONTEXTS` value; `None` everywhere else (a lone luma TU, or any
    // chroma plane), where `txb_skip_ctx` is 0 or the usual OR-of-neighbours
    // formula below.
    luma_skip_ctx: Option<usize>,
) -> Result<Vec<i32>> {
    let skip_ctx = if plane_idx == 0 {
        luma_skip_ctx.unwrap_or(0)
    } else {
        usize::from(around.0) + usize::from(around.1)
    };
    let mut coding = cdfs.txb(set, tx_mode);
    let (grid, tx_type) = read_coeffs(dec, &mut coding, scan, skip_ctx, dc_sign_ctx(around.2))?;
    // A 64x64 luma block's transform covers the whole 64x64 area, but only its
    // top-left 32x32 of frequencies are coded (spec 5.11.40); the rest of the
    // dequantized grid stays zero, which `inverse_transform_2d`'s own `< 32`
    // guard also assumes.
    let levels = if tx_side == side {
        grid
    } else {
        // `grid` is `tx_side x tx_side` (the coded corner); `dequant_and_inverse`
        // wants the true `side x side` transform, with dqDenom (spec 7.12.3)
        // keyed by that true size, not the coded corner's -- so the corner
        // goes into the top-left of a `side`-sized grid, not the reverse.
        let mut full = vec![0i32; side * side];
        for row in 0..tx_side {
            full[row * side..][..tx_side].copy_from_slice(&grid[row * tx_side..][..tx_side]);
        }
        full
    };
    let residual = dequant_and_inverse_typed(&levels, side, 8, i32::from(base_q_idx), tx_type);
    if std::env::var_os("EC_AV1_TRACE").is_some() {
        eprintln!(
            "TRACE dequant plane={plane_idx} base_q_idx={base_q_idx} levels[0..4]={:?} residual[0..4]={:?}",
            &levels[..4.min(levels.len())],
            &residual[..4.min(residual.len())]
        );
    }
    plane.reconstruct(
        x,
        y,
        side,
        predict_mode,
        reach,
        &residual,
        cfl,
        filter_intra,
    );
    Ok(if tx_side == side {
        residual_grid_placeholder(&levels, side)
    } else {
        levels
    })
}

/// `record`'s neighbour state only reads whether *the coded grid* (not the
/// residual) has nonzero entries, so the levels this function already built
/// are what it wants back; this helper exists only to name that clearly at
/// the one call site where `tx_side == side`.
fn residual_grid_placeholder(levels: &[i32], _tx_side: usize) -> Vec<i32> {
    levels.to_vec()
}

/// Decodes one whole-block coded superblock/quadrant/leaf: its mode, then its
/// three planes, reconstructing them into `y`/`u`/`v` and updating
/// `neighbours`.
#[allow(clippy::too_many_arguments)]
fn decode_block(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    at: (usize, usize),
    side: usize,
    luma_set: TxbSet,
    chroma_set: TxbSet,
    luma_tx: usize,
    chroma_tx: usize,
    scans: (&[u16], &[u16]),
    cfl: bool,
    y: &mut PlaneBuf,
    u: &mut PlaneBuf,
    v: &mut PlaneBuf,
    base_q_idx: u8,
    enable_filter_intra: bool,
    scan32: &[u16],
    scan16: &[u16],
    scan8: &[u16],
    scan4: &[u16],
    tx_select: bool,
    reduced_tx_set: bool,
) -> Result<()> {
    let (r, c) = at;
    let (px, py) = (c * SUB, r * SUB);
    let (skip, mode, alpha, filter_intra) = read_intra_mode(
        dec,
        cdfs,
        neighbours.above_mode[c],
        neighbours.left_mode[r],
        cfl,
        side,
        enable_filter_intra,
    )?;
    let (mi_r, mi_c) = (r * (SUB / MI), c * (SUB / MI));
    // `TxMode::Select`'s `tx_depth` (spec 5.11.16): read for every intra
    // block, skipped or not. `logical_tx` is what the loop filter's edge
    // lookup and the next block's own `tx_size_context` want (the *nominal*
    // transform size -- 64 even for the 64x64 corner-scanned case, spec
    // 5.11.40); `coeff_tx_side` is the actual coded coefficient grid's side,
    // which only differs from it at that one corner case.
    let (logical_tx, coeff_tx_side) = if tx_select {
        let resolved = read_tx_size(dec, cdfs, neighbours, (mi_r, mi_c), side);
        (resolved, if resolved == 64 { 32 } else { resolved })
    } else {
        (side, luma_tx)
    };
    let luma_set = if tx_select {
        txbset_for(logical_tx, reduced_tx_set)
    } else {
        luma_set
    };
    let luma_scan = scan_for(coeff_tx_side, scan32, scan16, scan8, scan4);
    let reach = Reach::of(side, px, py, y.width, y.height);
    let (cpx, cpy) = (px / 2, py / 2);
    let chroma_side = side / 2;
    if skip {
        // A skipped block codes no residual syntax at all (spec 5.11.34):
        // straight prediction, on every plane.
        y.reconstruct(
            px,
            py,
            side,
            mode,
            reach,
            &vec![0i32; side * side],
            None,
            filter_intra,
        );
        let ac = alpha.map(|_| cfl_ac_q3(y, px, py, side));
        u.reconstruct(
            cpx,
            cpy,
            chroma_side,
            DC_PRED,
            Reach::none(),
            &vec![0i32; chroma_side * chroma_side],
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
            None,
        );
        v.reconstruct(
            cpx,
            cpy,
            chroma_side,
            DC_PRED,
            Reach::none(),
            &vec![0i32; chroma_side * chroma_side],
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
            None,
        );
        let luma_grid = vec![0i32; side * side];
        let u_grid = vec![0i32; chroma_side * chroma_side];
        let v_grid = vec![0i32; chroma_side * chroma_side];
        neighbours.record(at, side, mode, &[luma_grid, u_grid, v_grid]);
    } else if !tx_select || logical_tx == side {
        // A single luma transform unit -- the old path, unchanged (including
        // the 64x64 corner case, `logical_tx == side == 64` with
        // `coeff_tx_side == 32`).
        let around = neighbours.around(at, side);
        let luma_grid = read_plane(
            dec,
            cdfs,
            luma_set,
            luma_scan,
            0,
            around[0],
            mode,
            mode,
            reach,
            y,
            px,
            py,
            side,
            coeff_tx_side,
            base_q_idx,
            None,
            filter_intra,
            None,
        )?;
        let ac = alpha.map(|_| cfl_ac_q3(y, px, py, side));
        let u_grid = read_plane(
            dec,
            cdfs,
            chroma_set,
            scans.1,
            1,
            around[1],
            mode,
            DC_PRED,
            Reach::none(),
            u,
            cpx,
            cpy,
            chroma_side,
            chroma_tx,
            base_q_idx,
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
            None,
            None,
        )?;
        let v_grid = read_plane(
            dec,
            cdfs,
            chroma_set,
            scans.1,
            2,
            around[2],
            mode,
            DC_PRED,
            Reach::none(),
            v,
            cpx,
            cpy,
            chroma_side,
            chroma_tx,
            base_q_idx,
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
            None,
            None,
        )?;
        neighbours.record(at, side, mode, &[luma_grid, u_grid, v_grid]);
    } else {
        // Several luma transform units, raster order (spec
        // `transform_block`'s loop): the block's resolved transform is
        // genuinely smaller than its own side, so each tile of it predicts
        // and codes independently, its own context read fresh off whatever
        // the earlier tiles in this same block already wrote -- the same
        // per-TU pattern [`decode_leaf8`] already runs for an 8x8 leaf.
        let n_axis = side / logical_tx;
        for tu_row in 0..n_axis {
            for tu_col in 0..n_axis {
                let tu_mi = (
                    mi_r + tu_row * (logical_tx / MI),
                    mi_c + tu_col * (logical_tx / MI),
                );
                let tu_px = px + tu_col * logical_tx;
                let tu_py = py + tu_row * logical_tx;
                let tu_around = neighbours.around_mi(tu_mi, logical_tx)[0];
                let tu_reach = Reach::of(logical_tx, tu_px, tu_py, y.width, y.height);
                // This transform unit's own bsize (`logical_tx`) is smaller
                // than the block it sits in (`side`), so `txb_skip_ctx` is
                // the neighbour-magnitude table, not the lone-TU 0 (spec
                // `get_txb_ctx_general`'s `plane_bsize != tx_size` branch).
                let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, logical_tx / MI);
                let tu_grid = read_plane(
                    dec,
                    cdfs,
                    luma_set,
                    luma_scan,
                    0,
                    tu_around,
                    mode,
                    mode,
                    tu_reach,
                    y,
                    tu_px,
                    tu_py,
                    logical_tx,
                    logical_tx,
                    base_q_idx,
                    None,
                    filter_intra,
                    Some(tu_skip_ctx),
                )?;
                if std::env::var_os("EC_AV1_TRACE").is_some() {
                    eprintln!(
                        "TRACE tu_bitpos mi=({},{}) tell={}",
                        tu_mi.0,
                        tu_mi.1,
                        dec.debug_bitpos()
                    );
                }
                neighbours.record_mi_luma(tu_mi, logical_tx, &tu_grid);
            }
        }
        let ac = alpha.map(|_| cfl_ac_q3(y, px, py, side));
        let chroma_around = neighbours.around(at, side);
        let u_grid = read_plane(
            dec,
            cdfs,
            chroma_set,
            scans.1,
            1,
            chroma_around[1],
            mode,
            DC_PRED,
            Reach::none(),
            u,
            cpx,
            cpy,
            chroma_side,
            chroma_tx,
            base_q_idx,
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
            None,
            None,
        )?;
        let v_grid = read_plane(
            dec,
            cdfs,
            chroma_set,
            scans.1,
            2,
            chroma_around[2],
            mode,
            DC_PRED,
            Reach::none(),
            v,
            cpx,
            cpy,
            chroma_side,
            chroma_tx,
            base_q_idx,
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
            None,
            None,
        )?;
        neighbours.record_split_luma(at, side, mode, [&u_grid, &v_grid]);
    }
    neighbours.fill_skip_grid((r * (SUB / MI), c * (SUB / MI)), side / MI, skip);
    neighbours.fill_lf_grid(
        (r * (SUB / MI), c * (SUB / MI)),
        side / MI,
        logical_tx as u8,
        false,
    );
    Ok(())
}

/// Decodes one 8x8 leaf of a straddling 16x16 block (lane-av1-rect), mirroring
/// [`crate::tile::write_leaf8`]: its intra-mode context comes from the
/// *enclosing* 16x16 slot's `above_mode`/`left_mode` (`outer_at`, [`SUB`]-grid),
/// overridden on whichever axis `prev_leaf` sits along -- the previous leaf's
/// just-decoded mode, not the enclosing slot's stale one, is the true
/// neighbour there. `leaf_mi` is this leaf's own position in 4x4 mode-info
/// units, which is what its coefficient context (finer than [`SUB`]) is read
/// at. Returns the mode this leaf decoded, for the caller's own
/// `prev_leaf`/final mode-writeback bookkeeping.
#[allow(clippy::too_many_arguments)]
fn decode_leaf8(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    outer_at: (usize, usize),
    leaf_mi: (usize, usize),
    scans: (&[u16], &[u16]),
    prev_leaf: Option<((usize, usize), usize)>,
    y: &mut PlaneBuf,
    u: &mut PlaneBuf,
    v: &mut PlaneBuf,
    base_q_idx: u8,
    enable_filter_intra: bool,
    tx_select: bool,
    reduced_tx_set: bool,
) -> Result<usize> {
    let (r, c) = outer_at;
    let mut above_mode = neighbours.above_mode[c];
    let mut left_mode = neighbours.left_mode[r];
    if let Some(((pr, pc), pmode)) = prev_leaf {
        if pc == leaf_mi.1 && leaf_mi.0 == pr + 2 {
            above_mode = pmode;
        } else if pr == leaf_mi.0 && leaf_mi.1 == pc + 2 {
            left_mode = pmode;
        }
    }
    // An 8x8 leaf is well within `is_cfl_allowed`'s <=32x32 bound (spec
    // 5.11.5), so it reads the CFL-allowed `uv_mode_cfl` CDF, like every other
    // `decode_block` caller at 16x16 and up.
    let (skip, mode, alpha, filter_intra) = read_intra_mode(
        dec,
        cdfs,
        above_mode,
        left_mode,
        true,
        8,
        enable_filter_intra,
    )?;
    // `TxMode::Select`'s `tx_depth` at an 8x8 leaf (spec 5.11.16): the only
    // depths an 8x8 block offers are `TX8` (depth 0) and `TX4` (depth 1,
    // which splits the leaf's own 8x8 prediction into a 2x2 grid of 4x4
    // transform units, same raster-order per-TU pattern `decode_block`'s
    // multi-TU branch runs).
    let resolved = if tx_select {
        read_tx_size(dec, cdfs, neighbours, leaf_mi, 8)
    } else {
        8
    };
    let (px, py) = (leaf_mi.1 * MI, leaf_mi.0 * MI);
    let reach = Reach::of(8, px, py, y.width, y.height);
    let (cpx, cpy) = (px / 2, py / 2);
    let (luma_grid, u_grid, v_grid);
    if skip {
        y.reconstruct(px, py, 8, mode, reach, &vec![0i32; 64], None, filter_intra);
        let ac = alpha.map(|_| cfl_ac_q3(y, px, py, 8));
        u.reconstruct(
            cpx,
            cpy,
            4,
            DC_PRED,
            Reach::none(),
            &vec![0i32; 16],
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
            None,
        );
        v.reconstruct(
            cpx,
            cpy,
            4,
            DC_PRED,
            Reach::none(),
            &vec![0i32; 16],
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
            None,
        );
        luma_grid = vec![0i32; 64];
        u_grid = vec![0i32; 16];
        v_grid = vec![0i32; 16];
    } else if resolved != 4 {
        let around = neighbours.around_mi(leaf_mi, 8);
        luma_grid = read_plane(
            dec,
            cdfs,
            if reduced_tx_set {
                TxbSet::Luma8
            } else {
                TxbSet::Luma8Set1
            },
            scans.0,
            0,
            around[0],
            mode,
            mode,
            reach,
            y,
            px,
            py,
            8,
            TX8,
            base_q_idx,
            None,
            filter_intra,
            None,
        )?;
        let ac = alpha.map(|_| cfl_ac_q3(y, px, py, 8));
        u_grid = read_plane(
            dec,
            cdfs,
            TxbSet::Chroma4,
            scans.1,
            1,
            around[1],
            mode,
            DC_PRED,
            Reach::none(),
            u,
            cpx,
            cpy,
            4,
            TX4,
            base_q_idx,
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
            None,
            None,
        )?;
        v_grid = read_plane(
            dec,
            cdfs,
            TxbSet::Chroma4,
            scans.1,
            2,
            around[2],
            mode,
            DC_PRED,
            Reach::none(),
            v,
            cpx,
            cpy,
            4,
            TX4,
            base_q_idx,
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
            None,
            None,
        )?;
    } else {
        // `tx_depth` resolved this leaf's luma to `TX4`: a 2x2 raster-order
        // grid of 4x4 transform units, each predicted from the leaf's own
        // 8x8 `mode` and reading its own fresh context off the earlier
        // units in this same leaf (the same per-TU pattern `decode_block`'s
        // multi-TU branch runs at bigger blocks).
        luma_grid = vec![0i32; 64];
        for tu_row in 0..2 {
            for tu_col in 0..2 {
                let tu_mi = (leaf_mi.0 + tu_row, leaf_mi.1 + tu_col);
                let tu_px = px + tu_col * 4;
                let tu_py = py + tu_row * 4;
                let tu_around = neighbours.around_mi(tu_mi, 4)[0];
                let tu_reach = Reach::of(4, tu_px, tu_py, y.width, y.height);
                let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, 1);
                let tu_grid = read_plane(
                    dec,
                    cdfs,
                    if reduced_tx_set {
                        TxbSet::Luma4
                    } else {
                        TxbSet::Luma4Set1
                    },
                    scans.1,
                    0,
                    tu_around,
                    mode,
                    mode,
                    tu_reach,
                    y,
                    tu_px,
                    tu_py,
                    4,
                    4,
                    base_q_idx,
                    None,
                    filter_intra,
                    Some(tu_skip_ctx),
                )?;
                if std::env::var_os("EC_AV1_TRACE").is_some() {
                    eprintln!(
                        "TRACE tu_bitpos mi=({},{}) tell={}",
                        tu_mi.0,
                        tu_mi.1,
                        dec.debug_bitpos()
                    );
                }
                neighbours.record_mi_luma(tu_mi, 4, &tu_grid);
            }
        }
        let ac = alpha.map(|_| cfl_ac_q3(y, px, py, 8));
        let chroma_around = neighbours.around_mi(leaf_mi, 8);
        u_grid = read_plane(
            dec,
            cdfs,
            TxbSet::Chroma4,
            scans.1,
            1,
            chroma_around[1],
            mode,
            DC_PRED,
            Reach::none(),
            u,
            cpx,
            cpy,
            4,
            TX4,
            base_q_idx,
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
            None,
            None,
        )?;
        v_grid = read_plane(
            dec,
            cdfs,
            TxbSet::Chroma4,
            scans.1,
            2,
            chroma_around[2],
            mode,
            DC_PRED,
            Reach::none(),
            v,
            cpx,
            cpy,
            4,
            TX4,
            base_q_idx,
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
            None,
            None,
        )?;
        // `record_split_luma` expects a `record`-style `(r, c)` position in
        // `SUB`-grid units and rescales it by `SUB / MI`; `leaf_mi` is
        // already in mi units, so calling it here double-scaled the
        // position. Plane 0 is already correct per-TU from the
        // `record_mi_luma` calls above; this inlines `record_mi`'s tail
        // (spec `get_txb_ctx_general`'s neighbour-magnitude bookkeeping) for
        // the two chroma planes only, at `leaf_mi` directly, side_mi=2 (this
        // leaf's own 8x8 side over `MI`).
        let side_mi = 2;
        for cell in 0..side_mi {
            neighbours.left_side_mi[leaf_mi.0 + cell] = 8;
            neighbours.above_side_mi[leaf_mi.1 + cell] = 8;
        }
        let round_up_even = |n: usize| n.div_ceil(2) * 2;
        let bound_h = round_up_even(neighbours.mi_rows);
        let bound_w = round_up_even(neighbours.mi_cols);
        let u_state = neighbour_state(&u_grid);
        let v_state = neighbour_state(&v_grid);
        for cell in 0..side_mi {
            if cell < side_mi.min(bound_h.saturating_sub(leaf_mi.0)) {
                neighbours.left[leaf_mi.0 + cell][1] = u_state;
                neighbours.left[leaf_mi.0 + cell][2] = v_state;
            }
            if cell < side_mi.min(bound_w.saturating_sub(leaf_mi.1)) {
                neighbours.above[leaf_mi.1 + cell][1] = u_state;
                neighbours.above[leaf_mi.1 + cell][2] = v_state;
            }
        }
        neighbours.fill_skip_grid(leaf_mi, 2, skip);
        neighbours.fill_lf_grid(leaf_mi, 2, 4, false);
        return Ok(mode);
    }
    neighbours.record_mi(leaf_mi, 8, &[luma_grid, u_grid, v_grid]);
    neighbours.fill_skip_grid(leaf_mi, 2, skip);
    neighbours.fill_lf_grid(leaf_mi, 2, 8, false);
    Ok(mode)
}

/// Spec 7.15.3's `Cdef_Directions`: `(row, col)` offsets for the two primary
/// taps of each of the eight directions; the secondary taps at `dir+2`/`dir-2`
/// (mod 8) reuse the same table.
const CDEF_DIRECTIONS: [[(i32, i32); 2]; 8] = [
    [(-1, 1), (-2, 2)],
    [(0, 1), (-1, 2)],
    [(0, 1), (0, 2)],
    [(0, 1), (1, 2)],
    [(1, 1), (2, 2)],
    [(1, 0), (2, 1)],
    [(1, 0), (2, 0)],
    [(1, 0), (2, -1)],
];
/// A neighbour tap outside the frame's true extent -- `constrain` reads this
/// as "so far away it never contributes" (spec/libaom `CDEF_VERY_LARGE`).
const CDEF_VERY_LARGE: i32 = 0x4000;
const CDEF_PRI_TAPS: [[i32; 2]; 2] = [[4, 2], [3, 3]];
const CDEF_SEC_TAPS: [i32; 2] = [2, 1];

fn cdef_msb(x: i32) -> i32 {
    31 - x.leading_zeros() as i32
}

/// Spec 7.15.3's `constrain`: how much of a neighbour's difference from the
/// centre sample a filter tap is allowed to contribute.
fn cdef_constrain(diff: i32, threshold: i32, damping: i32) -> i32 {
    if threshold == 0 {
        return 0;
    }
    let shift = (damping - cdef_msb(threshold)).max(0);
    diff.signum() * (threshold - (diff.abs() >> shift)).clamp(0, diff.abs())
}

/// Spec 7.15.3's direction search (libaom `cdef_find_dir_c`): the dominant
/// direction of an 8x8 luma window and the variance gap between it and its
/// orthogonal, `sample(row, col)` reading that window's own pixels.
fn cdef_find_dir(sample: impl Fn(usize, usize) -> i32) -> (usize, i32) {
    const DIV_TABLE: [i64; 9] = [0, 840, 420, 280, 210, 168, 140, 120, 105];
    let mut partial = [[0i64; 15]; 8];
    for i in 0..8i64 {
        for j in 0..8i64 {
            let x = i64::from(sample(i as usize, j as usize)) - 128;
            partial[0][(i + j) as usize] += x;
            partial[1][(i + j / 2) as usize] += x;
            partial[2][i as usize] += x;
            partial[3][(3 + i - j / 2) as usize] += x;
            partial[4][(7 + i - j) as usize] += x;
            partial[5][(3 - i / 2 + j) as usize] += x;
            partial[6][j as usize] += x;
            partial[7][(i / 2 + j) as usize] += x;
        }
    }
    let mut cost = [0i64; 8];
    for i in 0..8usize {
        cost[2] += partial[2][i] * partial[2][i];
        cost[6] += partial[6][i] * partial[6][i];
    }
    cost[2] *= DIV_TABLE[8];
    cost[6] *= DIV_TABLE[8];
    for i in 0..7usize {
        cost[0] += (partial[0][i] * partial[0][i] + partial[0][14 - i] * partial[0][14 - i])
            * DIV_TABLE[i + 1];
        cost[4] += (partial[4][i] * partial[4][i] + partial[4][14 - i] * partial[4][14 - i])
            * DIV_TABLE[i + 1];
    }
    cost[0] += partial[0][7] * partial[0][7] * DIV_TABLE[8];
    cost[4] += partial[4][7] * partial[4][7] * DIV_TABLE[8];
    for i in (1..8).step_by(2) {
        for j in 0..5usize {
            cost[i] += partial[i][3 + j] * partial[i][3 + j];
        }
        cost[i] *= DIV_TABLE[8];
        for j in 0..3usize {
            cost[i] += (partial[i][j] * partial[i][j] + partial[i][10 - j] * partial[i][10 - j])
                * DIV_TABLE[2 * j + 2];
        }
    }
    let mut best_cost = 0i64;
    let mut best_dir = 0usize;
    for (i, &c) in cost.iter().enumerate() {
        if c > best_cost {
            best_cost = c;
            best_dir = i;
        }
    }
    let var = (best_cost - cost[(best_dir + 4) & 7]) >> 10;
    (best_dir, var as i32)
}

/// Spec 7.15.2's per-8x8 strength adjustment: a strongly directional 8x8
/// (high variance gap) gets more deringing than a flat or non-directional
/// one, only ever applied to the luma primary strength.
fn cdef_adjust_strength(strength: i32, var: i32) -> i32 {
    if var == 0 {
        return 0;
    }
    let i = if var >> 6 != 0 {
        cdef_msb(var >> 6).min(12)
    } else {
        0
    };
    (strength * (4 + i) + 8) >> 4
}

/// Spec 7.15.3's `cdef_filter_block`: filters one block (8x8 luma, 4x4 4:2:0
/// chroma) in place, `sample(row, col)` reading the *unfiltered* frame (a
/// previously-filtered neighbour must never feed this block's own sum).
#[allow(clippy::too_many_arguments)]
fn cdef_filter_block(
    sample: impl Fn(i32, i32) -> i32,
    dst: &mut PlaneBuf,
    ox: usize,
    oy: usize,
    bw: usize,
    bh: usize,
    pri_strength: i32,
    sec_strength: i32,
    dir: usize,
    pri_damping: i32,
    sec_damping: i32,
    enable_primary: bool,
    enable_secondary: bool,
) {
    let clipping_required = enable_primary && enable_secondary;
    let pri_taps = CDEF_PRI_TAPS[(pri_strength & 1) as usize];
    for i in 0..bh {
        for j in 0..bw {
            let x = sample(i as i32, j as i32);
            let mut sum = 0i32;
            let mut max = x;
            let mut min = x;
            for k in 0..2usize {
                if enable_primary {
                    let (tr, tc) = CDEF_DIRECTIONS[dir][k];
                    let p0 = sample(i as i32 + tr, j as i32 + tc);
                    let p1 = sample(i as i32 - tr, j as i32 - tc);
                    sum += pri_taps[k] * cdef_constrain(p0 - x, pri_strength, pri_damping);
                    sum += pri_taps[k] * cdef_constrain(p1 - x, pri_strength, pri_damping);
                    if clipping_required {
                        if p0 != CDEF_VERY_LARGE {
                            max = max.max(p0);
                        }
                        if p1 != CDEF_VERY_LARGE {
                            max = max.max(p1);
                        }
                        min = min.min(p0).min(p1);
                    }
                }
                if enable_secondary {
                    let (tr0, tc0) = CDEF_DIRECTIONS[(dir + 2) & 7][k];
                    let (tr1, tc1) = CDEF_DIRECTIONS[(dir + 6) & 7][k];
                    let s0 = sample(i as i32 + tr0, j as i32 + tc0);
                    let s1 = sample(i as i32 - tr0, j as i32 - tc0);
                    let s2 = sample(i as i32 + tr1, j as i32 + tc1);
                    let s3 = sample(i as i32 - tr1, j as i32 - tc1);
                    if clipping_required {
                        for s in [s0, s1, s2, s3] {
                            if s != CDEF_VERY_LARGE {
                                max = max.max(s);
                            }
                        }
                        min = min.min(s0).min(s1).min(s2).min(s3);
                    }
                    sum += CDEF_SEC_TAPS[k]
                        * (cdef_constrain(s0 - x, sec_strength, sec_damping)
                            + cdef_constrain(s1 - x, sec_strength, sec_damping)
                            + cdef_constrain(s2 - x, sec_strength, sec_damping)
                            + cdef_constrain(s3 - x, sec_strength, sec_damping));
                }
            }
            let mut y = x + ((8 + sum - i32::from(sum < 0)) >> 4);
            if clipping_required {
                y = y.clamp(min, max);
            }
            dst.data[(oy + i) * dst.width + (ox + j)] = y.clamp(0, 255) as u8;
        }
    }
}

/// Applies CDEF (spec 7.15) to the whole reconstructed frame, over its true
/// (unpadded) `mi_cols`/`mi_rows` extent, before it is cropped to its display
/// size. `cdef_bits == 0` is this crate's only supported case so far (see the
/// refusal in `stream.rs`), so every 64x64 filter block always uses index 0's
/// strengths -- no per-block `cdef_idx` selection is implemented yet.
/// Spec 7.14: the in-loop deblocking filter, applied once per frame after
/// tile reconstruction and before [`apply_cdef`]. Uniform-level path only --
/// `stream.rs` already refuses `delta_lf_present`/segmentation upstream, so
/// every block's level comes from [`LoopFilterParams::level`] plus the
/// `ref_deltas`/`mode_deltas` adjustment alone (spec 7.14.4's
/// `get_filter_level`, precomputed per-block here rather than cached in a
/// table the way libaom's `av1_loop_filter_frame_init` does).
///
/// Ported from libaom's `av1/common/av1_loopfilter.c` (edge/level decision)
/// and `aom_dsp/loopfilter.c` (the `aom_lpf_*` pixel kernels).
fn apply_deblock(
    y: &mut PlaneBuf,
    u: &mut PlaneBuf,
    v: &mut PlaneBuf,
    lf: &LoopFilterParams,
    n: &Neighbours,
) {
    if lf.level[0] != 0 || lf.level[1] != 0 {
        deblock_plane(y, 0, lf, n);
    }
    if lf.level[2] != 0 {
        deblock_plane(u, 1, lf, n);
    }
    if lf.level[3] != 0 {
        deblock_plane(v, 2, lf, n);
    }
}

/// Luma-mi-grid position (row or col) a plane-local pixel coordinate falls
/// in -- `Neighbours::tx_grid`/`inter_grid` are always indexed at the luma
/// 4x4-mi grain, even when `coord` is a chroma coordinate (2 chroma px per
/// mi unit under this decoder's assumed 4:2:0 subsampling).
fn plane_to_mi(chroma: bool, coord: usize) -> usize {
    if chroma { coord / 2 } else { coord / 4 }
}

/// This plane's own transform width in pixels at the covering block, spec
/// `get_transform_size`'s plane-aware lookup: `tx_grid` stores the luma
/// width directly; chroma halves it (this decoder's UV transform is always
/// half its coded block's luma transform, spec's `av1_get_max_uv_txsize`
/// under 4:2:0 and a block that never splits its own transform).
fn tx_px_at(n: &Neighbours, chroma: bool, mi_r: usize, mi_c: usize) -> u32 {
    let luma_tx = n
        .tx_grid
        .get(mi_r * n.skip_grid_cols_mi + mi_c)
        .copied()
        .unwrap_or(0) as u32;
    if chroma { luma_tx / 2 } else { luma_tx }
}

/// `get_tx_size_context` (spec 9.3's `TxDepthCtx`, libaom
/// `TXFM_CONTEXT`/`entropymode.c`): the sum of two boolean terms, each
/// present only when its neighbour exists inside the true frame -- whether
/// the block immediately above (at this block's own leftmost mi column) or
/// to the left (at its own topmost mi row) coded a transform at least as wide
/// as `own_side`.
fn tx_size_context(n: &Neighbours, (mi_r, mi_c): (usize, usize), own_side: usize) -> usize {
    let above = mi_r > 0 && tx_px_at(n, false, mi_r - 1, mi_c) as usize >= own_side;
    let left = mi_c > 0 && tx_px_at(n, false, mi_r, mi_c - 1) as usize >= own_side;
    usize::from(above) + usize::from(left)
}

/// `read_tx_size`/`read_selected_tx_size` (spec 5.11.16): under
/// `TxMode::Select`, an intra block's `tx_depth` symbol, resolved straight to
/// the transform's own side in pixels (`side >> depth`) -- this decoder's
/// four square categories (8/16/32/64) each halve at most twice, matching
/// libaom's own per-size depth cap (`cdf::TX_SIZE_CAT0`..`TX_SIZE_CAT3`).
fn read_tx_size(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    n: &Neighbours,
    at_mi: (usize, usize),
    side: usize,
) -> usize {
    let ctx = tx_size_context(n, at_mi, side);
    let depth = match side {
        8 => dec.symbol(&mut cdfs.tx_size_cat0[ctx]),
        16 => dec.symbol(&mut cdfs.tx_size_cat1[ctx]),
        32 => dec.symbol(&mut cdfs.tx_size_cat2[ctx]),
        64 => dec.symbol(&mut cdfs.tx_size_cat3[ctx]),
        _ => unreachable!("decode_block/decode_leaf8 only call this at 8/16/32/64"),
    };
    if depth != 0 {
        TX_DEPTH_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    side >> depth
}

/// The luma coefficient scan table for a resolved transform side -- 32/16/8/4,
/// the only four sizes a `TxMode::Select` block's coded coefficient grid
/// ([`read_tx_size`]'s resolved side, or 32 at the 64x64 corner case) can be.
fn scan_for<'a>(
    tx_px: usize,
    scan32: &'a [u16],
    scan16: &'a [u16],
    scan8: &'a [u16],
    scan4: &'a [u16],
) -> &'a [u16] {
    match tx_px {
        32 => scan32,
        16 => scan16,
        8 => scan8,
        4 => scan4,
        _ => unreachable!("read_tx_size never resolves the coefficient grid to anything else"),
    }
}

/// The [`TxbSet`] a resolved luma transform side reads its coefficient tables
/// from -- 64 is only ever the 64x64-block, depth-0, corner-scanned case
/// ([`TxbSet::Luma64`]); every other resolved side maps to its own matching
/// set. `get_tx_set` (spec 5.11.48) only splits on `reduced_tx_set` at
/// `TX_8X8`/`TX_4X4`: an intra `TX_16X16` always reads `TX_SET_INTRA_2`
/// regardless of the flag (`av1_get_ext_tx_set_type`'s
/// `tx_size_sqr == TX_16X16` branch ignores `use_reduced_set` once it is past
/// the `TX_32X32`/reduced-set checks), so only the 8x8 and 4x4 cases have a
/// second table ([`TxbSet::Luma8Set1`]/[`TxbSet::Luma4Set1`]).
fn txbset_for(tx_px: usize, reduced_tx_set: bool) -> TxbSet {
    match tx_px {
        64 => TxbSet::Luma64,
        32 => TxbSet::Luma32,
        16 => TxbSet::Luma16,
        8 if reduced_tx_set => TxbSet::Luma8,
        8 => TxbSet::Luma8Set1,
        4 if reduced_tx_set => TxbSet::Luma4,
        4 => TxbSet::Luma4Set1,
        _ => unreachable!("read_tx_size never resolves a block's own side to anything else"),
    }
}

/// `tx_size_wide_unit_log2`/`tx_size_high_unit_log2`: `log2(px / 4)`, the
/// index [`tx_dim_to_filter_length`] and the chroma 4-vs-6 choice key on.
fn unit_log2(px: u32) -> i32 {
    if px == 0 {
        0
    } else {
        px.trailing_zeros() as i32 - 2
    }
}

/// spec 7.14.4's `get_filter_level`, restricted to this decoder's shapes:
/// every inter block is a single `LAST_FRAME` reference with a non-`GLOBALMV`
/// mode, so `is_inter` alone selects `ref_deltas`/`mode_deltas` (design note
/// on [`Neighbours::inter_grid`]).
fn lf_level(lf: &LoopFilterParams, plane_idx: usize, dir: usize, is_inter: bool) -> i32 {
    let base = i32::from(if plane_idx == 0 {
        lf.level[dir]
    } else if plane_idx == 1 {
        lf.level[2]
    } else {
        lf.level[3]
    });
    if !lf.delta_enabled {
        return base;
    }
    let scale = 1i32 << (base >> 5);
    let ref_idx = usize::from(is_inter);
    let mut lvl = base + i32::from(lf.ref_deltas[ref_idx]) * scale;
    if is_inter {
        lvl += i32::from(lf.mode_deltas[1]) * scale;
    }
    lvl.clamp(0, 63)
}

/// Spec 7.14.2's `set_lpf_parameters`, restricted to the uniform-level,
/// no-segmentation, no-delta-LF path and to this decoder's own invariant
/// that a coded block's transform is always its own full size -- so a TU
/// edge is always also a PU edge, and the "both sides skipped, not a PU
/// edge" suppression (which only ever fires at a non-PU-edge TU boundary)
/// never applies here, letting this skip the `skip`/`skip_txfm` lookup
/// entirely. Returns `None` when this 4-pixel edge group is not filtered.
fn edge_params(
    lf: &LoopFilterParams,
    n: &Neighbours,
    plane_idx: usize,
    dir: usize,
    chroma: bool,
    x0: usize,
    y0: usize,
) -> Option<(u8, i32)> {
    let (mi_r, mi_c) = (plane_to_mi(chroma, y0), plane_to_mi(chroma, x0));
    let cur_tx = tx_px_at(n, chroma, mi_r, mi_c);
    if cur_tx == 0 || (if dir == 0 { x0 } else { y0 }) % cur_tx as usize != 0 {
        return None;
    }
    let cur_inter = n.inter_grid[mi_r * n.skip_grid_cols_mi + mi_c];
    let (pv_mi_r, pv_mi_c) = if dir == 0 {
        (mi_r, mi_c - 1)
    } else {
        (mi_r - 1, mi_c)
    };
    let pv_tx = tx_px_at(n, chroma, pv_mi_r, pv_mi_c);
    let pv_inter = n.inter_grid[pv_mi_r * n.skip_grid_cols_mi + pv_mi_c];

    let cur_level = lf_level(lf, plane_idx, dir, cur_inter);
    let pv_level = lf_level(lf, plane_idx, dir, pv_inter);
    if cur_level == 0 && pv_level == 0 {
        return None;
    }
    let dim = unit_log2(cur_tx).min(unit_log2(pv_tx)).clamp(0, 4) as usize;
    let len: u8 = if chroma {
        if dim == 0 { 4 } else { 6 }
    } else {
        [4, 8, 14, 14, 14][dim]
    };
    let level = if cur_level != 0 { cur_level } else { pv_level };
    DEBLOCK_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Some((len, level))
}

/// `update_sharpness`'s per-level `(blimit, limit, hev_thr)` triple.
fn deblock_thresholds(level: i32, sharpness: u8) -> (i32, i32, i32) {
    let sharpness = i32::from(sharpness);
    let shift = i32::from(sharpness > 0) + i32::from(sharpness > 4);
    let mut lim = level >> shift;
    if sharpness > 0 {
        lim = lim.min(9 - sharpness);
    }
    lim = lim.max(1);
    (2 * (level + 2) + lim, lim, level >> 4)
}

fn sclamp(v: i32) -> i32 {
    v.clamp(-128, 127)
}

fn round_pow2(v: i32, n: u32) -> i32 {
    (v + (1 << (n - 1))) >> n
}

/// `filter4`: the narrow 4-tap kernel, and every wider kernel's non-flat
/// fallback.
fn filter4(mask: bool, thresh: i32, p1: i32, p0: i32, q0: i32, q1: i32) -> [i32; 4] {
    if !mask {
        return [p1, p0, q0, q1];
    }
    let hev = (p1 - p0).abs() > thresh || (q1 - q0).abs() > thresh;
    let (ps1, ps0, qs0, qs1) = (p1 - 128, p0 - 128, q0 - 128, q1 - 128);
    let outer = if hev { sclamp(ps1 - qs1) } else { 0 };
    let filter = sclamp(outer + 3 * (qs0 - ps0));
    let filter1 = sclamp(filter + 4) >> 3;
    let filter2 = sclamp(filter + 3) >> 3;
    let oq0 = sclamp(qs0 - filter1) + 128;
    let op0 = sclamp(ps0 + filter2) + 128;
    let tap = if hev { 0 } else { round_pow2(filter1, 1) };
    let oq1 = sclamp(qs1 - tap) + 128;
    let op1 = sclamp(ps1 + tap) + 128;
    [op1, op0, oq0, oq1]
}

/// `filter6`: chroma's flat 5-tap smoothing (spec `[1,2,2,2,1]`) over
/// `filter4`'s narrow taps.
#[allow(clippy::too_many_arguments)]
fn filter6(
    mask: bool,
    thresh: i32,
    flat: bool,
    p2: i32,
    p1: i32,
    p0: i32,
    q0: i32,
    q1: i32,
    q2: i32,
) -> [i32; 4] {
    if flat && mask {
        [
            round_pow2(p2 * 3 + p1 * 2 + p0 * 2 + q0, 3),
            round_pow2(p2 + p1 * 2 + p0 * 2 + q0 * 2 + q1, 3),
            round_pow2(p1 + p0 * 2 + q0 * 2 + q1 * 2 + q2, 3),
            round_pow2(p0 + q0 * 2 + q1 * 2 + q2 * 3, 3),
        ]
    } else {
        filter4(mask, thresh, p1, p0, q0, q1)
    }
}

/// `filter8`: luma's flat 7-tap smoothing (spec `[1,1,1,2,1,1,1]`).
#[allow(clippy::too_many_arguments)]
fn filter8(
    mask: bool,
    thresh: i32,
    flat: bool,
    p3: i32,
    p2: i32,
    p1: i32,
    p0: i32,
    q0: i32,
    q1: i32,
    q2: i32,
    q3: i32,
) -> [i32; 6] {
    if flat && mask {
        [
            round_pow2(p3 + p3 + p3 + 2 * p2 + p1 + p0 + q0, 3),
            round_pow2(p3 + p3 + p2 + 2 * p1 + p0 + q0 + q1, 3),
            round_pow2(p3 + p2 + p1 + 2 * p0 + q0 + q1 + q2, 3),
            round_pow2(p2 + p1 + p0 + 2 * q0 + q1 + q2 + q3, 3),
            round_pow2(p1 + p0 + q0 + 2 * q1 + q2 + q3 + q3, 3),
            round_pow2(p0 + q0 + q1 + 2 * q2 + q3 + q3 + q3, 3),
        ]
    } else {
        let [op1, op0, oq0, oq1] = filter4(mask, thresh, p1, p0, q0, q1);
        [p2, op1, op0, oq0, oq1, q2]
    }
}

/// `filter14`: the wide 13-tap smoothing used at 16x16-and-larger transform
/// edges; falls back to [`filter8`] (leaving `op6`/`oq6` untouched either
/// way -- they are only ever taps here, never written).
#[allow(clippy::too_many_arguments)]
fn filter14(
    mask: bool,
    thresh: i32,
    flat: bool,
    flat2: bool,
    p6: i32,
    p5: i32,
    p4: i32,
    p3: i32,
    p2: i32,
    p1: i32,
    p0: i32,
    q0: i32,
    q1: i32,
    q2: i32,
    q3: i32,
    q4: i32,
    q5: i32,
    q6: i32,
) -> [i32; 12] {
    if flat2 && flat && mask {
        [
            round_pow2(p6 * 7 + p5 * 2 + p4 * 2 + p3 + p2 + p1 + p0 + q0, 4),
            round_pow2(
                p6 * 5 + p5 * 2 + p4 * 2 + p3 * 2 + p2 + p1 + p0 + q0 + q1,
                4,
            ),
            round_pow2(
                p6 * 4 + p5 + p4 * 2 + p3 * 2 + p2 * 2 + p1 + p0 + q0 + q1 + q2,
                4,
            ),
            round_pow2(
                p6 * 3 + p5 + p4 + p3 * 2 + p2 * 2 + p1 * 2 + p0 + q0 + q1 + q2 + q3,
                4,
            ),
            round_pow2(
                p6 * 2 + p5 + p4 + p3 + p2 * 2 + p1 * 2 + p0 * 2 + q0 + q1 + q2 + q3 + q4,
                4,
            ),
            round_pow2(
                p6 + p5 + p4 + p3 + p2 + p1 * 2 + p0 * 2 + q0 * 2 + q1 + q2 + q3 + q4 + q5,
                4,
            ),
            round_pow2(
                p5 + p4 + p3 + p2 + p1 + p0 * 2 + q0 * 2 + q1 * 2 + q2 + q3 + q4 + q5 + q6,
                4,
            ),
            round_pow2(
                p4 + p3 + p2 + p1 + p0 + q0 * 2 + q1 * 2 + q2 * 2 + q3 + q4 + q5 + q6 * 2,
                4,
            ),
            round_pow2(
                p3 + p2 + p1 + p0 + q0 + q1 * 2 + q2 * 2 + q3 * 2 + q4 + q5 + q6 * 3,
                4,
            ),
            round_pow2(
                p2 + p1 + p0 + q0 + q1 + q2 * 2 + q3 * 2 + q4 * 2 + q5 + q6 * 4,
                4,
            ),
            round_pow2(
                p1 + p0 + q0 + q1 + q2 + q3 * 2 + q4 * 2 + q5 * 2 + q6 * 5,
                4,
            ),
            round_pow2(p0 + q0 + q1 + q2 + q3 + q4 * 2 + q5 * 2 + q6 * 7, 4),
        ]
    } else {
        let [op2, op1, op0, oq0, oq1, oq2] =
            filter8(mask, thresh, flat, p3, p2, p1, p0, q0, q1, q2, q3);
        [p5, p4, p3, op2, op1, op0, oq0, oq1, oq2, q3, q4, q5]
    }
}

/// Filters one 4-pixel edge group: `base` is the plane-local byte offset of
/// the first (`q0`) sample past the edge, `outer_stride` advances to the
/// next of the 4 perpendicular samples, `tap_stride` steps from one tap to
/// the next along the edge's own filtering direction.
#[allow(clippy::too_many_arguments)]
fn filter_edge(
    data: &mut [u8],
    base: usize,
    outer_stride: isize,
    tap_stride: isize,
    len: u8,
    level: i32,
    sharpness: u8,
) {
    let (blimit, limit, hev_thr) = deblock_thresholds(level, sharpness);
    for i in 0..4isize {
        let center = (base as isize + i * outer_stride) as usize;
        let idx = |k: isize| -> usize { (center as isize + k * tap_stride) as usize };
        let px = |k: isize| -> i32 { i32::from(data[idx(k)]) };
        match len {
            4 => {
                let (p1, p0, q0, q1) = (px(-2), px(-1), px(0), px(1));
                let mask = (p1 - p0).abs() <= limit
                    && (q1 - q0).abs() <= limit
                    && (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 <= blimit;
                let [op1, op0, oq0, oq1] = filter4(mask, hev_thr, p1, p0, q0, q1);
                data[idx(-2)] = op1.clamp(0, 255) as u8;
                data[idx(-1)] = op0.clamp(0, 255) as u8;
                data[idx(0)] = oq0.clamp(0, 255) as u8;
                data[idx(1)] = oq1.clamp(0, 255) as u8;
            }
            6 => {
                let (p2, p1, p0, q0, q1, q2) = (px(-3), px(-2), px(-1), px(0), px(1), px(2));
                let mask = (p2 - p1).abs() <= limit
                    && (p1 - p0).abs() <= limit
                    && (q1 - q0).abs() <= limit
                    && (q2 - q1).abs() <= limit
                    && (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 <= blimit;
                let flat = (p1 - p0).abs() <= 1
                    && (q1 - q0).abs() <= 1
                    && (p2 - p0).abs() <= 1
                    && (q2 - q0).abs() <= 1;
                let [op1, op0, oq0, oq1] = filter6(mask, hev_thr, flat, p2, p1, p0, q0, q1, q2);
                data[idx(-2)] = op1.clamp(0, 255) as u8;
                data[idx(-1)] = op0.clamp(0, 255) as u8;
                data[idx(0)] = oq0.clamp(0, 255) as u8;
                data[idx(1)] = oq1.clamp(0, 255) as u8;
            }
            8 => {
                let (p3, p2, p1, p0, q0, q1, q2, q3) =
                    (px(-4), px(-3), px(-2), px(-1), px(0), px(1), px(2), px(3));
                let mask = (p3 - p2).abs() <= limit
                    && (p2 - p1).abs() <= limit
                    && (p1 - p0).abs() <= limit
                    && (q1 - q0).abs() <= limit
                    && (q2 - q1).abs() <= limit
                    && (q3 - q2).abs() <= limit
                    && (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 <= blimit;
                let flat = (p1 - p0).abs() <= 1
                    && (q1 - q0).abs() <= 1
                    && (p2 - p0).abs() <= 1
                    && (q2 - q0).abs() <= 1
                    && (p3 - p0).abs() <= 1
                    && (q3 - q0).abs() <= 1;
                let [op2, op1, op0, oq0, oq1, oq2] =
                    filter8(mask, hev_thr, flat, p3, p2, p1, p0, q0, q1, q2, q3);
                data[idx(-3)] = op2.clamp(0, 255) as u8;
                data[idx(-2)] = op1.clamp(0, 255) as u8;
                data[idx(-1)] = op0.clamp(0, 255) as u8;
                data[idx(0)] = oq0.clamp(0, 255) as u8;
                data[idx(1)] = oq1.clamp(0, 255) as u8;
                data[idx(2)] = oq2.clamp(0, 255) as u8;
            }
            _ => {
                let (p6, p5, p4, p3, p2, p1, p0) =
                    (px(-7), px(-6), px(-5), px(-4), px(-3), px(-2), px(-1));
                let (q0, q1, q2, q3, q4, q5, q6) =
                    (px(0), px(1), px(2), px(3), px(4), px(5), px(6));
                let mask = (p3 - p2).abs() <= limit
                    && (p2 - p1).abs() <= limit
                    && (p1 - p0).abs() <= limit
                    && (q1 - q0).abs() <= limit
                    && (q2 - q1).abs() <= limit
                    && (q3 - q2).abs() <= limit
                    && (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 <= blimit;
                let flat = (p1 - p0).abs() <= 1
                    && (q1 - q0).abs() <= 1
                    && (p2 - p0).abs() <= 1
                    && (q2 - q0).abs() <= 1
                    && (p3 - p0).abs() <= 1
                    && (q3 - q0).abs() <= 1;
                let flat2 = (p4 - p0).abs() <= 1
                    && (q4 - q0).abs() <= 1
                    && (p5 - p0).abs() <= 1
                    && (q5 - q0).abs() <= 1
                    && (p6 - p0).abs() <= 1
                    && (q6 - q0).abs() <= 1;
                let out = filter14(
                    mask, hev_thr, flat, flat2, p6, p5, p4, p3, p2, p1, p0, q0, q1, q2, q3, q4, q5,
                    q6,
                );
                for (k, v) in (-6..=5isize).zip(out) {
                    data[idx(k)] = v.clamp(0, 255) as u8;
                }
            }
        }
    }
}

/// One plane's worth of spec 7.14: every vertical edge across the plane,
/// then every horizontal edge (the order the spec and libaom both use).
fn deblock_plane(plane: &mut PlaneBuf, plane_idx: usize, lf: &LoopFilterParams, n: &Neighbours) {
    let chroma = plane_idx != 0;
    let (tw, th, stride) = (plane.true_width, plane.true_height, plane.width);
    let mut y0 = 0usize;
    while y0 < th {
        let mut x0 = 4usize;
        while x0 < tw {
            if let Some((len, level)) = edge_params(lf, n, plane_idx, 0, chroma, x0, y0) {
                filter_edge(
                    &mut plane.data,
                    y0 * stride + x0,
                    stride as isize,
                    1,
                    len,
                    level,
                    lf.sharpness,
                );
            }
            x0 += 4;
        }
        y0 += 4;
    }
    let mut x0 = 0usize;
    while x0 < tw {
        let mut y0 = 4usize;
        while y0 < th {
            if let Some((len, level)) = edge_params(lf, n, plane_idx, 1, chroma, x0, y0) {
                filter_edge(
                    &mut plane.data,
                    y0 * stride + x0,
                    1,
                    stride as isize,
                    len,
                    level,
                    lf.sharpness,
                );
            }
            y0 += 4;
        }
        x0 += 4;
    }
}

fn apply_cdef(
    y: &mut PlaneBuf,
    u: &mut PlaneBuf,
    v: &mut PlaneBuf,
    cdef: &CdefParams,
    skip_grid: &Neighbours,
) {
    if cdef.y_pri_strength[0] == 0
        && cdef.y_sec_strength[0] == 0
        && cdef.uv_pri_strength[0] == 0
        && cdef.uv_sec_strength[0] == 0
    {
        return;
    }
    let src_y = y.data.clone();
    let src_u = u.data.clone();
    let src_v = v.data.clone();
    let damping = i32::from(cdef.damping);
    let damping_uv = (damping - 1).max(0);
    let mut mi_r = 0usize;
    while mi_r < skip_grid.mi_rows {
        let mut mi_c = 0usize;
        while mi_c < skip_grid.mi_cols {
            if !skip_grid.is_skip_txfm(mi_r, mi_c) {
                let (ox, oy) = (mi_c * 4, mi_r * 4);
                let (y_stride, y_true_w, y_true_h) = (y.width, y.true_width, y.true_height);
                let sample_y = |r: i32, c: i32| -> i32 {
                    let (ny, nx) = (oy as i32 + r, ox as i32 + c);
                    if ny < 0 || nx < 0 || ny as usize >= y_true_h || nx as usize >= y_true_w {
                        CDEF_VERY_LARGE
                    } else {
                        i32::from(src_y[ny as usize * y_stride + nx as usize])
                    }
                };
                let (dir, var) = cdef_find_dir(|r, c| {
                    let (ny, nx) = ((oy + r).min(y_true_h - 1), (ox + c).min(y_true_w - 1));
                    i32::from(src_y[ny * y_stride + nx])
                });

                // libaom `av1_cdef_filter_fb` passes `pri_strength ? dir : 0`
                // -- each plane zeroes the shared direction when *its own*
                // frame-level primary strength is 0, independent of the
                // per-block adjusted `t` and of the other plane's strength.
                let y_dir = if cdef.y_pri_strength[0] != 0 { dir } else { 0 };
                let t = cdef_adjust_strength(i32::from(cdef.y_pri_strength[0]), var);
                let enable_primary = t != 0;
                let enable_secondary = cdef.y_sec_strength[0] != 0;
                if enable_primary || enable_secondary {
                    cdef_filter_block(
                        sample_y,
                        y,
                        ox,
                        oy,
                        8,
                        8,
                        t,
                        i32::from(cdef.y_sec_strength[0]),
                        y_dir,
                        damping,
                        damping,
                        enable_primary,
                        enable_secondary,
                    );
                }

                let t_uv = i32::from(cdef.uv_pri_strength[0]);
                let uv_dir = if t_uv != 0 { dir } else { 0 };
                let enable_primary_uv = t_uv != 0;
                let enable_secondary_uv = cdef.uv_sec_strength[0] != 0;
                if enable_primary_uv || enable_secondary_uv {
                    let (cox, coy) = (ox / 2, oy / 2);
                    for (plane, src_p) in [(&mut *u, &src_u), (&mut *v, &src_v)] {
                        let (tw, th) = (plane.true_width, plane.true_height);
                        let stride = plane.width;
                        let sample_uv = |r: i32, c: i32| -> i32 {
                            let (ny, nx) = (coy as i32 + r, cox as i32 + c);
                            if ny < 0 || nx < 0 || ny as usize >= th || nx as usize >= tw {
                                CDEF_VERY_LARGE
                            } else {
                                i32::from(src_p[ny as usize * stride + nx as usize])
                            }
                        };
                        cdef_filter_block(
                            sample_uv,
                            plane,
                            cox,
                            coy,
                            4,
                            4,
                            t_uv,
                            i32::from(cdef.uv_sec_strength[0]),
                            uv_dir,
                            damping_uv,
                            damping_uv,
                            enable_primary_uv,
                            enable_secondary_uv,
                        );
                    }
                }
            }
            mi_c += 2;
        }
        mi_r += 2;
    }
}

/// Decodes the payload [`crate::tile::sb_coeff_key_frame_tile`] writes,
/// returning the picture it reconstructs to.
///
/// `mi_cols`/`mi_rows` and `base_q_idx` are the frame header's, as parsed by
/// [`ec_av1_syntax`]. `frame_width`/`frame_height` are the header's own true
/// (render) size — a separate field from `mi_cols`/`mi_rows` (spec
/// `compute_image_size` derives one from the other, but not losslessly: a
/// width that is not a multiple of 8 samples leaves `mi_cols * 4` past it) —
/// what the reconstruction is cropped to on the way out, mirroring
/// [`crate::encode::crop_encoded`].
///
/// # Errors
/// Returns an error when a block's partition, mode, skip flag or transform
/// type is anything this decoder does not reconstruct (round 2: inter, tx
/// types other than `DCT_DCT`, non-CDF-gathered rectangular splits below
/// 16x16, a directional mode's angle delta other than zero, chroma-from-luma),
/// or when the tile payload runs out of the symbols this decode expects (a
/// genuinely foreign stream).
pub fn decode_key_frame_tile(
    data: &[u8],
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    frame_width: u32,
    frame_height: u32,
    enable_filter_intra: bool,
    cdef: &CdefParams,
    loop_filter: &LoopFilterParams,
    tx_select: bool,
    reduced_tx_set: bool,
) -> Result<Picture> {
    decode_key_frame_tile_with_cdfs(
        data,
        mi_cols,
        mi_rows,
        base_q_idx,
        frame_width,
        frame_height,
        enable_filter_intra,
        cdef,
        loop_filter,
        None,
        tx_select,
        reduced_tx_set,
    )
    .map(|(picture, _)| picture)
}

/// [`decode_key_frame_tile`], threading a cross-frame CDF forward (spec
/// 7.20's `load_cdfs`): `initial_cdfs` is the caller's forwarded state (from
/// a prior frame's saved reference slot) when set, or the spec 8.4 defaults
/// when `None` (the only case [`decode_key_frame_tile`] itself ever needs,
/// since a key frame's own header always forces `primary_ref_frame ==
/// PRIMARY_REF_NONE`). Returns the tile's own end-of-tile adapted table
/// alongside the picture, for the caller to save into whichever reference
/// slots this frame's `refresh_frame_flags` names.
pub(crate) fn decode_key_frame_tile_with_cdfs(
    data: &[u8],
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    frame_width: u32,
    frame_height: u32,
    enable_filter_intra: bool,
    cdef: &CdefParams,
    loop_filter: &LoopFilterParams,
    initial_cdfs: Option<Cdfs>,
    tx_select: bool,
    reduced_tx_set: bool,
) -> Result<(Picture, Cdfs)> {
    if mi_cols == 0 || mi_rows == 0 {
        return Err(unsupported("a frame with no mode-info grid"));
    }
    let (sb_cols, sb_rows) = (mi_cols.div_ceil(SB_MI), mi_rows.div_ceil(SB_MI));
    let (cols32, rows32) = block_grid(mi_cols, mi_rows);
    let (true_width, true_height) = ((mi_cols * 4) as usize, (mi_rows * 4) as usize);
    let (width, height) = (cols32 as usize * BLOCK, rows32 as usize * BLOCK);

    let mut y = PlaneBuf {
        data: vec![0u8; width * height],
        width,
        height,
        true_width,
        true_height,
    };
    let mut u = PlaneBuf {
        data: vec![0u8; width * height / 4],
        width: width / 2,
        height: height / 2,
        true_width: true_width / 2,
        true_height: true_height / 2,
    };
    let mut v = PlaneBuf {
        data: vec![0u8; width * height / 4],
        width: width / 2,
        height: height / 2,
        true_width: true_width / 2,
        true_height: true_height / 2,
    };

    let scan32 = default_scan(TX32);
    let scan16 = default_scan(TX16);
    let scan8 = default_scan(TX8);
    let scan4 = default_scan(TX4);

    let mut cdfs = initial_cdfs.unwrap_or_else(|| Cdfs::new(q_ctx_of(base_q_idx)));
    if std::env::var_os("EC_AV1_TRACE").is_some() {
        eprintln!(
            "TRACE key_tile_bytes len={} first8={:02x?} base_q_idx={base_q_idx} mi_cols={mi_cols} mi_rows={mi_rows}",
            data.len(),
            &data[..data.len().min(8)]
        );
    }
    let mut dec = SymbolDecoder::new(data);
    let mut neighbours = Neighbours::new(
        cols32 as usize * 2,
        rows32 as usize * 2,
        mi_cols as usize,
        mi_rows as usize,
    );

    for sb_r in 0..sb_rows {
        neighbours.start_row();
        for sb_c in 0..sb_cols {
            let at = (sb_r as usize * 4, sb_c as usize * 4);
            let ctx = neighbours.partition_ctx(at, SB);
            let (has_cols, has_rows) = (
                sb_c * SB_MI + SB_MI / 2 < mi_cols,
                sb_r * SB_MI + SB_MI / 2 < mi_rows,
            );
            // Recomputed at this superblock's own half (spec
            // `decode_partition`): a full alphabet symbol when both halves are
            // inside the true frame, a single gathered bit when just one is
            // (the superblock cannot be left whole, so this is forced split),
            // and nothing at all (SPLIT inferred, no bits) when neither is —
            // mirroring [`crate::tile::sb_coeff_key_frame_tile`]'s own
            // three-way write exactly.
            let part = if has_cols && has_rows {
                let p = dec.symbol(&mut cdfs.partition_w64[ctx]);
                if std::env::var_os("EC_AV1_TRACE").is_some() {
                    eprintln!("TRACE partition_w64 ctx={ctx} value={p}");
                }
                p
            } else {
                match (has_cols, has_rows) {
                    (true, false) => {
                        dec.symbol_fixed(&gather(&cdfs.partition_w64[ctx], VERT_ALIKE));
                    }
                    (false, true) => {
                        dec.symbol_fixed(&gather(&cdfs.partition_w64[ctx], HORZ_ALIKE));
                    }
                    _ => {}
                }
                PARTITION_SPLIT
            };
            match part {
                PARTITION_NONE => {
                    decode_block(
                        &mut dec,
                        &mut cdfs,
                        &mut neighbours,
                        at,
                        SB,
                        TxbSet::Luma64,
                        TxbSet::Chroma32,
                        TX32,
                        TX32,
                        (&scan32, &scan32),
                        false,
                        &mut y,
                        &mut u,
                        &mut v,
                        base_q_idx,
                        enable_filter_intra,
                        &scan32,
                        &scan16,
                        &scan8,
                        &scan4,
                        tx_select,
                        reduced_tx_set,
                    )?;
                }
                PARTITION_SPLIT => {
                    for q in 0..4 {
                        let (r32, c32) = (sb_r * 2 + q / 2, sb_c * 2 + q % 2);
                        if r32 >= rows32 || c32 >= cols32 {
                            continue;
                        }
                        let at32 = (r32 as usize * 2, c32 as usize * 2);
                        let ctx32 = neighbours.partition_ctx(at32, BLOCK);
                        let (has_cols32, has_rows32) = (
                            has_half(c32 * BLOCK_MI, BLOCK_MI, mi_cols),
                            has_half(r32 * BLOCK_MI, BLOCK_MI, mi_rows),
                        );
                        let part32 = if has_cols32 && has_rows32 {
                            let p = dec.symbol(&mut cdfs.partition_w32[ctx32]);
                            if std::env::var_os("EC_AV1_TRACE").is_some() {
                                eprintln!("TRACE partition_w32 ctx={ctx32} value={p}");
                            }
                            p
                        } else {
                            match (has_cols32, has_rows32) {
                                (true, false) => {
                                    dec.symbol_fixed(&gather(
                                        &cdfs.partition_w32[ctx32],
                                        VERT_ALIKE,
                                    ));
                                }
                                (false, true) => {
                                    dec.symbol_fixed(&gather(
                                        &cdfs.partition_w32[ctx32],
                                        HORZ_ALIKE,
                                    ));
                                }
                                _ => {}
                            }
                            PARTITION_SPLIT
                        };
                        match part32 {
                            PARTITION_NONE => {
                                decode_block(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    at32,
                                    BLOCK,
                                    TxbSet::Luma32,
                                    TxbSet::Chroma16,
                                    TX32,
                                    TX16,
                                    (&scan32, &scan16),
                                    true,
                                    &mut y,
                                    &mut u,
                                    &mut v,
                                    base_q_idx,
                                    enable_filter_intra,
                                    &scan32,
                                    &scan16,
                                    &scan8,
                                    &scan4,
                                    tx_select,
                                    reduced_tx_set,
                                )?;
                            }
                            PARTITION_SPLIT => {
                                for sub in 0..4 {
                                    let (sr, sc) =
                                        (r32 as usize * 2 + sub / 2, c32 as usize * 2 + sub % 2);
                                    if (sr as u32) * SUB_MI >= mi_rows
                                        || (sc as u32) * SUB_MI >= mi_cols
                                    {
                                        continue;
                                    }
                                    let (has_cols16, has_rows16) = (
                                        has_half(sc as u32 * SUB_MI, SUB_MI, mi_cols),
                                        has_half(sr as u32 * SUB_MI, SUB_MI, mi_rows),
                                    );
                                    let at16 = (sr, sc);
                                    let ctx16 = neighbours.partition_ctx(at16, SUB);
                                    if has_cols16 && has_rows16 {
                                        let part16 = dec.symbol(&mut cdfs.partition_w16[ctx16]);
                                        if std::env::var_os("EC_AV1_TRACE").is_some() {
                                            eprintln!(
                                                "TRACE partition_w16 mi=({},{}) ctx={ctx16} value={part16}",
                                                at16.0, at16.1
                                            );
                                        }
                                        if part16 == PARTITION_NONE {
                                            decode_block(
                                                &mut dec,
                                                &mut cdfs,
                                                &mut neighbours,
                                                at16,
                                                SUB,
                                                TxbSet::Luma16,
                                                TxbSet::Chroma8,
                                                TX16,
                                                TX8,
                                                (&scan16, &scan8),
                                                true,
                                                &mut y,
                                                &mut u,
                                                &mut v,
                                                base_q_idx,
                                                enable_filter_intra,
                                                &scan32,
                                                &scan16,
                                                &scan8,
                                                &scan4,
                                                tx_select,
                                                reduced_tx_set,
                                            )?;
                                            continue;
                                        }
                                        if part16 != PARTITION_SPLIT {
                                            return Err(unsupported(
                                                "a partition below 16x16 other than a clean split (this encoder never writes one)",
                                            ));
                                        }
                                        // A real (non-straddle) SPLIT of a
                                        // whole 16x16 into four 8x8 leaves --
                                        // the same recursion the straddle
                                        // path below already runs when the
                                        // true frame edge forces it, just
                                        // over all four positions instead of
                                        // the one or two the edge leaves.
                                        let (mi_row0, mi_col0) =
                                            (sr as u32 * SUB_MI, sc as u32 * SUB_MI);
                                        let mut prev_leaf: Option<((usize, usize), usize)> = None;
                                        for i in 0..4 {
                                            let (mr, mc) =
                                                (mi_row0 + (i / 2) * 2, mi_col0 + (i % 2) * 2);
                                            let leaf_mi = (mr as usize, mc as usize);
                                            let leaf_ctx = neighbours.partition_ctx_mi(leaf_mi, 8);
                                            let part8 =
                                                dec.symbol(&mut cdfs.partition_w8[leaf_ctx]);
                                            if part8 != PARTITION_NONE {
                                                return Err(unsupported(
                                                    "a partition below 8x8 (this encoder never writes one)",
                                                ));
                                            }
                                            let leaf_mode = decode_leaf8(
                                                &mut dec,
                                                &mut cdfs,
                                                &mut neighbours,
                                                at16,
                                                leaf_mi,
                                                (&scan8, &scan4),
                                                prev_leaf,
                                                &mut y,
                                                &mut u,
                                                &mut v,
                                                base_q_idx,
                                                enable_filter_intra,
                                                tx_select,
                                                reduced_tx_set,
                                            )?;
                                            prev_leaf = Some((leaf_mi, leaf_mode));
                                        }
                                        if let Some((_, mode)) = prev_leaf {
                                            neighbours.above_mode[sc] = mode;
                                            neighbours.left_mode[sr] = mode;
                                        }
                                        continue;
                                    }
                                    // The true edge falls inside this 16x16
                                    // leaf itself (mod-32==8 target sizes,
                                    // lane-av1-rect): one axis only -- an 8x8
                                    // leaf never itself straddles, so the
                                    // block splits cleanly along whichever
                                    // axis is short.
                                    if !has_cols16 && !has_rows16 {
                                        return Err(unsupported(
                                            "a 16x16 block whose true edge cuts through both \
                                             axes needs a rectangular transform this decoder \
                                             does not code yet",
                                        ));
                                    }
                                    if has_cols16 {
                                        dec.symbol_fixed(&gather(
                                            &cdfs.partition_w16[ctx16],
                                            VERT_ALIKE,
                                        ));
                                    } else {
                                        dec.symbol_fixed(&gather(
                                            &cdfs.partition_w16[ctx16],
                                            HORZ_ALIKE,
                                        ));
                                    }
                                    let (mi_row0, mi_col0) =
                                        (sr as u32 * SUB_MI, sc as u32 * SUB_MI);
                                    let leaf_positions: Vec<(u32, u32)> = (0..4)
                                        .map(|i| (mi_row0 + (i / 2) * 2, mi_col0 + (i % 2) * 2))
                                        .filter(|&(mr, mc)| mr < mi_rows && mc < mi_cols)
                                        .collect();
                                    let mut prev_leaf: Option<((usize, usize), usize)> = None;
                                    for (mr, mc) in leaf_positions {
                                        let leaf_mi = (mr as usize, mc as usize);
                                        let leaf_ctx = neighbours.partition_ctx_mi(leaf_mi, 8);
                                        let part8 = dec.symbol(&mut cdfs.partition_w8[leaf_ctx]);
                                        if part8 != PARTITION_NONE {
                                            return Err(unsupported(
                                                "a partition below 8x8 (this encoder never writes one)",
                                            ));
                                        }
                                        let leaf_mode = decode_leaf8(
                                            &mut dec,
                                            &mut cdfs,
                                            &mut neighbours,
                                            at16,
                                            leaf_mi,
                                            (&scan8, &scan4),
                                            prev_leaf,
                                            &mut y,
                                            &mut u,
                                            &mut v,
                                            base_q_idx,
                                            enable_filter_intra,
                                            tx_select,
                                            reduced_tx_set,
                                        )?;
                                        prev_leaf = Some((leaf_mi, leaf_mode));
                                    }
                                    // `record()`'s `above_mode`/`left_mode`
                                    // write is a no-op at an 8x8 leaf's own
                                    // side, so force the write once the whole
                                    // 16x16 slot's leaves are done, from the
                                    // last leaf (mirrors the writer's r15
                                    // fix).
                                    if let Some((_, mode)) = prev_leaf {
                                        neighbours.above_mode[sc] = mode;
                                        neighbours.left_mode[sr] = mode;
                                    }
                                }
                            }
                            _ => {
                                return Err(unsupported(
                                    "a partition type this encoder never writes",
                                ));
                            }
                        }
                    }
                }
                _ => {
                    return Err(unsupported("a partition type this encoder never writes"));
                }
            }
        }
    }

    apply_deblock(&mut y, &mut u, &mut v, loop_filter, &neighbours);
    apply_cdef(&mut y, &mut u, &mut v, cdef, &neighbours);

    let (fw, fh) = (frame_width as usize, frame_height as usize);
    if fw == width && fh == height {
        return Ok((
            Picture {
                width,
                height,
                y: y.data,
                u: u.data,
                v: v.data,
            },
            cdfs,
        ));
    }
    let crop = |plane: &PlaneBuf, w: usize, h: usize| -> Vec<u8> {
        let mut out = Vec::with_capacity(w * h);
        for row in 0..h {
            out.extend_from_slice(&plane.data[row * plane.width..][..w]);
        }
        out
    };
    Ok((
        Picture {
            width: fw,
            height: fh,
            y: crop(&y, fw, fh),
            u: crop(&u, fw / 2, fh / 2),
            v: crop(&v, fw / 2, fh / 2),
        },
        cdfs,
    ))
}

/// The `is_inter` context (spec 5.11.16 via `av1_get_intra_inter_context`),
/// duplicating [`crate::tile`]'s private copy of the same rule.
fn intra_inter_ctx(has_above: bool, has_left: bool, above_inter: bool, left_inter: bool) -> usize {
    match (has_above, has_left) {
        (true, true) => {
            let (above_intra, left_intra) = (!above_inter, !left_inter);
            if above_intra && left_intra {
                3
            } else {
                usize::from(above_intra || left_intra)
            }
        }
        (true, false) => 2 * usize::from(!above_inter),
        (false, true) => 2 * usize::from(!left_inter),
        (false, false) => 0,
    }
}

/// `CLASS0_SIZE << (class + 2)`, duplicating `crate::tile`'s private
/// `mv_class_base` (spec 3): the magnitude an `MV_CLASS_n` component's own
/// bits start counting from.
fn mv_class_base(class: usize) -> i32 {
    if class == 0 { 0 } else { 2i32 << (class + 2) }
}

/// One motion vector component's non-zero diff (spec 5.11.32
/// `read_mv_component`), the inverse of [`crate::tile`]'s private
/// `write_mv_component`.
fn read_mv_component(
    dec: &mut SymbolDecoder,
    c: &mut MvComponentCdfs,
    allow_high_precision_mv: bool,
) -> i32 {
    let sign = dec.symbol(&mut c.sign);
    let class = dec.symbol(&mut c.class);
    let local = if class == 0 {
        let bit = dec.symbol(&mut c.class0_bit);
        let fr = dec.symbol(&mut c.class0_fr[bit]);
        // spec 5.11.32 `mv_class0_hp`: read only when the frame allows
        // high-precision motion vectors, else implicitly 1 (the low bit
        // stays at half-pel precision) -- this crate's own writer always
        // leaves the flag off, but a foreign stream can set it per frame.
        let hp = if allow_high_precision_mv {
            dec.symbol(&mut c.class0_hp)
        } else {
            1
        };
        (bit << 3) | (fr << 1) | hp
    } else {
        let mut d = 0;
        for i in 0..class {
            d |= dec.symbol(&mut c.bit[i]) << i;
        }
        let fr = dec.symbol(&mut c.fr);
        let hp = if allow_high_precision_mv {
            dec.symbol(&mut c.hp)
        } else {
            1
        };
        (d << 3) | (fr << 1) | hp
    };
    let mag = mv_class_base(class) + local as i32 + 1;
    if sign == 1 { -mag } else { mag }
}

/// A motion vector coded as a residual against `pred` (spec 5.11.32
/// `read_mv`), the inverse of [`crate::tile`]'s private `write_mv`.
fn read_mv(
    dec: &mut SymbolDecoder,
    mv_comp: &mut [MvComponentCdfs; 2],
    mv_joint: &mut [u16; 5],
    pred: (i32, i32),
    allow_high_precision_mv: bool,
) -> (i32, i32) {
    let joint = dec.symbol(mv_joint);
    let mut diff = (0, 0);
    if joint == 2 || joint == 3 {
        diff.0 = read_mv_component(dec, &mut mv_comp[0], allow_high_precision_mv);
    }
    if joint == 1 || joint == 3 {
        diff.1 = read_mv_component(dec, &mut mv_comp[1], allow_high_precision_mv);
    }
    (pred.0 + diff.0, pred.1 + diff.1)
}

/// A 1/8-pel motion vector component converted to the 1/16-pel offset
/// [`mc::predict`] takes, duplicating [`crate::encode`]'s private `mv_to_q4`
/// (its own doc comment explains the `*2`/`*1` luma/chroma split).
fn mv_to_q4(pos: usize, mv_component: i32, luma: bool) -> i32 {
    (pos as i32) * 16 + mv_component * if luma { 2 } else { 1 }
}

impl PlaneBuf {
    /// Adds `residual` onto an already-computed `prediction` (motion
    /// compensation's output, rather than [`predict`]'s intra one), writing
    /// the clamped reconstruction into the plane at `(x, y)` -- the inter
    /// counterpart of [`PlaneBuf::reconstruct`].
    fn reconstruct_mc(
        &mut self,
        x: usize,
        y: usize,
        side: usize,
        prediction: &[u8],
        residual: &[i32],
    ) {
        for row in 0..side {
            for col in 0..side {
                let sample = (i32::from(prediction[row * side + col]) + residual[row * side + col])
                    .clamp(0, 255) as u8;
                self.data[(y + row) * self.width + x + col] = sample;
            }
        }
    }
}

/// Reads one inter-coded plane's transform block and reconstructs it by
/// adding its residual onto a motion-compensated `prediction` -- the inter
/// counterpart of [`read_plane`] (every inter transform this decoder reads is
/// `side`-square with no oversized-64 case, so there is no padded-grid
/// branch to mirror).
#[allow(clippy::too_many_arguments)]
fn read_inter_plane(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    set: TxbSet,
    scan: &[u16],
    plane_idx: usize,
    around: (bool, bool, i32),
    tx_mode: usize,
    plane: &mut PlaneBuf,
    x: usize,
    y: usize,
    side: usize,
    base_q_idx: u8,
    prediction: &[u8],
) -> Result<Vec<i32>> {
    let skip_ctx = if plane_idx == 0 {
        0
    } else {
        usize::from(around.0) + usize::from(around.1)
    };
    let mut coding = cdfs.txb(set, tx_mode);
    let (grid, tx_type) = read_coeffs(dec, &mut coding, scan, skip_ctx, dc_sign_ctx(around.2))?;
    let residual = dequant_and_inverse_typed(&grid, side, 8, i32::from(base_q_idx), tx_type);
    plane.reconstruct_mc(x, y, side, prediction, &residual);
    Ok(grid)
}

/// Decodes one square inter-frame block (32x32 whole or 16x16 leaf): skip,
/// `is_inter`, then either the single-reference/MV-stack/motion-vector chain
/// and motion-compensated reconstruction, or the inter frame's own intra
/// path (`Y_MODE` by size group, not `KF_Y_MODE` by neighbour), mirroring
/// [`crate::tile`]'s `sb_coeff_inter_frame_tile`'s whole-block branch and its
/// private `write_inter_frame_leaf` -- both share this one function here,
/// parameterised by `side`/the transform sets/scan tables/size group they
/// each use.
///
/// # Errors
/// Returns an error when a symbol names anything this decoder does not
/// reconstruct: a reference other than `LAST_FRAME`, `GLOBALMV` (round 3;
/// `NEARESTMV`/`NEARMV`/`NEWMV` are all reconstructed), or an intra
/// mode/tx-type outside what [`read_intra_mode`]/[`read_coeffs`] already
/// refuse.
#[allow(clippy::too_many_arguments)]
fn decode_inter_block(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    grid: &mut MiGrid,
    at: (usize, usize),
    side: usize,
    mi_cols: u32,
    mi_rows: u32,
    y: &mut PlaneBuf,
    u: &mut PlaneBuf,
    v: &mut PlaneBuf,
    ref_y: &PlaneBuf,
    ref_u: &PlaneBuf,
    ref_v: &PlaneBuf,
    base_q_idx: u8,
    luma_set_intra: TxbSet,
    luma_set_inter: TxbSet,
    chroma_set: TxbSet,
    luma_tx: usize,
    chroma_tx: usize,
    scan_luma: &[u16],
    scan_chroma: &[u16],
    size_group: usize,
    allow_high_precision_mv: bool,
) -> Result<()> {
    const LAST_FRAME: i8 = 1;

    let (r, c) = at;
    let (px, py) = (c * SUB, r * SUB);
    let (cpx, cpy) = (px / 2, py / 2);
    let chroma_side = side / 2;

    let skip_ctx = usize::from(neighbours.above_skip[c]) + usize::from(neighbours.left_skip[r]);
    let skip = dec.symbol(&mut cdfs.skip[skip_ctx]) == 1;

    let (has_above, has_left) = (r > 0, c > 0);
    let (above_inter, left_inter) = (neighbours.above_inter[c], neighbours.left_inter[r]);
    let ii_ctx = intra_inter_ctx(has_above, has_left, above_inter, left_inter);
    let is_inter = dec.symbol(&mut cdfs.intra_inter[ii_ctx]) == 1;

    let mode_for_tx;
    let (luma_grid, u_grid, v_grid);
    if is_inter {
        let sr_ctx = single_ref_ctx(above_inter || left_inter);
        let p1 = dec.symbol(&mut cdfs.single_ref[sr_ctx][0]);
        let p3 = dec.symbol(&mut cdfs.single_ref[sr_ctx][2]);
        let p4 = dec.symbol(&mut cdfs.single_ref[sr_ctx][3]);
        if (p1, p3, p4) != (0, 0, 0) {
            return Err(unsupported(
                "a reference frame other than LAST_FRAME (round 2)",
            ));
        }

        let (mi_row, mi_col) = (r * SUB_MI as usize, c * SUB_MI as usize);
        let bw4 = side / 4;
        let stack = find_mv_stack(
            grid,
            mi_row,
            mi_col,
            bw4,
            bw4,
            LAST_FRAME,
            mi_cols as usize,
            mi_rows as usize,
        );

        let not_new = dec.symbol(&mut cdfs.new_mv[stack.new_mv_ctx]) == 1;
        let (mv, is_new_mv) = if !not_new {
            if stack.entries.len() > 1 {
                dec.symbol(&mut cdfs.drl_mode[stack.drl_ctx[0]]);
            }
            (
                read_mv(
                    dec,
                    &mut cdfs.mv_comp,
                    &mut cdfs.mv_joint,
                    stack.pred_mv,
                    allow_high_precision_mv,
                ),
                true,
            )
        } else {
            let not_zero = dec.symbol(&mut cdfs.zero_mv[stack.zero_mv_ctx]) == 1;
            let mv = if !not_zero {
                // GLOBALMV: `gm_get_motion_vector` (spec 7.10.2.1) under the
                // `GmType::IDENTITY` this crate's `decode_stream` requires of
                // every inter frame's `LAST_FRAME` entry (any other warp
                // model is refused before the tile decoder is ever called).
                (0, 0)
            } else {
                let nearest = dec.symbol(&mut cdfs.ref_mv[stack.ref_mv_ctx]) == 0;
                if nearest {
                    stack.nearest_mv
                } else {
                    // NEARMV (spec 5.11.24's `read_drl_idx`, `RefMvIdx` starting
                    // at 1): read at most two more `drl_mode` bits, one per stack
                    // entry past the second, stopping at the first `0`.
                    // `drl_ctx[idx]` is the context between `entries[idx]` and
                    // `entries[idx + 1]` (`MvStack::drl_ctx`'s own doc), which is
                    // exactly the pair this index is choosing between.
                    let mut idx = 1usize;
                    while idx < 3 && stack.entries.len() > idx + 1 {
                        if dec.symbol(&mut cdfs.drl_mode[stack.drl_ctx[idx]]) == 0 {
                            break;
                        }
                        idx += 1;
                    }
                    stack.entries.get(idx).map_or(stack.near_mv, |e| e.mv)
                }
            };
            (mv, false)
        };
        if std::env::var_os("EC_AV1_TRACE").is_some() {
            eprintln!(
                "EC_TRACE mi_row={mi_row} mi_col={mi_col} skip={} is_inter=1 mv=({},{}) is_new_mv={is_new_mv} bsize={side}",
                skip as u8, mv.0, mv.1
            );
        }
        for dr in 0..bw4 {
            for dc in 0..bw4 {
                grid.set(
                    mi_row + dr,
                    mi_col + dc,
                    MiInfo {
                        is_inter: true,
                        ref_frame: LAST_FRAME,
                        mv,
                        is_new_mv,
                        size: bw4,
                    },
                );
            }
        }
        mode_for_tx = 0;

        let mut pred_y = vec![0u8; side * side];
        mc::predict(
            &ref_y.data,
            ref_y.width,
            ref_y.true_width,
            ref_y.true_height,
            mv_to_q4(px, mv.1, true),
            mv_to_q4(py, mv.0, true),
            side,
            side,
            &mut pred_y,
        );
        let mut pred_u = vec![0u8; chroma_side * chroma_side];
        mc::predict(
            &ref_u.data,
            ref_u.width,
            ref_u.true_width,
            ref_u.true_height,
            mv_to_q4(cpx, mv.1, false),
            mv_to_q4(cpy, mv.0, false),
            chroma_side,
            chroma_side,
            &mut pred_u,
        );
        let mut pred_v = vec![0u8; chroma_side * chroma_side];
        mc::predict(
            &ref_v.data,
            ref_v.width,
            ref_v.true_width,
            ref_v.true_height,
            mv_to_q4(cpx, mv.1, false),
            mv_to_q4(cpy, mv.0, false),
            chroma_side,
            chroma_side,
            &mut pred_v,
        );

        if skip {
            y.reconstruct_mc(px, py, side, &pred_y, &vec![0i32; side * side]);
            u.reconstruct_mc(
                cpx,
                cpy,
                chroma_side,
                &pred_u,
                &vec![0i32; chroma_side * chroma_side],
            );
            v.reconstruct_mc(
                cpx,
                cpy,
                chroma_side,
                &pred_v,
                &vec![0i32; chroma_side * chroma_side],
            );
            luma_grid = vec![0i32; side * side];
            u_grid = vec![0i32; chroma_side * chroma_side];
            v_grid = vec![0i32; chroma_side * chroma_side];
        } else {
            let around = neighbours.around(at, side);
            luma_grid = read_inter_plane(
                dec,
                cdfs,
                luma_set_inter,
                scan_luma,
                0,
                around[0],
                mode_for_tx,
                y,
                px,
                py,
                side,
                base_q_idx,
                &pred_y,
            )?;
            u_grid = read_inter_plane(
                dec,
                cdfs,
                chroma_set,
                scan_chroma,
                1,
                around[1],
                mode_for_tx,
                u,
                cpx,
                cpy,
                chroma_side,
                base_q_idx,
                &pred_u,
            )?;
            v_grid = read_inter_plane(
                dec,
                cdfs,
                chroma_set,
                scan_chroma,
                2,
                around[2],
                mode_for_tx,
                v,
                cpx,
                cpy,
                chroma_side,
                base_q_idx,
                &pred_v,
            )?;
        }
    } else {
        let mode = dec.symbol(&mut cdfs.y_mode[size_group]);
        if mode >= 13 {
            return Err(unsupported(
                "an intra mode this decoder does not code (round 2)",
            ));
        }
        if (V_PRED..=D67_PRED).contains(&mode) {
            let angle = dec.symbol(&mut cdfs.angle_delta[mode - V_PRED]);
            if angle != ANGLE_DELTA_ZERO {
                return Err(unsupported(
                    "a nonzero angle delta (this encoder never writes one)",
                ));
            }
        }
        let uv_mode = dec.symbol(&mut cdfs.uv_mode_cfl[mode]);
        let alpha = if uv_mode == DC_PRED {
            None
        } else if uv_mode == UV_CFL_PRED {
            Some(read_cfl_alphas(dec, cdfs))
        } else {
            return Err(unsupported("a directional chroma mode (round 2)"));
        };
        mode_for_tx = mode;
        let (mi_row, mi_col) = (r * SUB_MI as usize, c * SUB_MI as usize);
        for dr in 0..side / 4 {
            for dc in 0..side / 4 {
                grid.set(
                    mi_row + dr,
                    mi_col + dc,
                    MiInfo {
                        is_inter: false,
                        ref_frame: -1,
                        mv: (0, 0),
                        is_new_mv: false,
                        size: side / 4,
                    },
                );
            }
        }

        let reach = Reach::of(side, px, py, y.width, y.height);
        if skip {
            y.reconstruct(
                px,
                py,
                side,
                mode,
                reach,
                &vec![0i32; side * side],
                None,
                None,
            );
            let ac = alpha.map(|_| cfl_ac_q3(y, px, py, side));
            u.reconstruct(
                cpx,
                cpy,
                chroma_side,
                DC_PRED,
                Reach::none(),
                &vec![0i32; chroma_side * chroma_side],
                alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
                None,
            );
            v.reconstruct(
                cpx,
                cpy,
                chroma_side,
                DC_PRED,
                Reach::none(),
                &vec![0i32; chroma_side * chroma_side],
                alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
                None,
            );
            luma_grid = vec![0i32; side * side];
            u_grid = vec![0i32; chroma_side * chroma_side];
            v_grid = vec![0i32; chroma_side * chroma_side];
        } else {
            let around = neighbours.around(at, side);
            luma_grid = read_plane(
                dec,
                cdfs,
                luma_set_intra,
                scan_luma,
                0,
                around[0],
                mode,
                mode,
                reach,
                y,
                px,
                py,
                side,
                luma_tx,
                base_q_idx,
                None,
                None,
                None,
            )?;
            let ac = alpha.map(|_| cfl_ac_q3(y, px, py, side));
            u_grid = read_plane(
                dec,
                cdfs,
                chroma_set,
                scan_chroma,
                1,
                around[1],
                mode,
                DC_PRED,
                Reach::none(),
                u,
                cpx,
                cpy,
                chroma_side,
                chroma_tx,
                base_q_idx,
                alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
                None,
                None,
            )?;
            v_grid = read_plane(
                dec,
                cdfs,
                chroma_set,
                scan_chroma,
                2,
                around[2],
                mode,
                DC_PRED,
                Reach::none(),
                v,
                cpx,
                cpy,
                chroma_side,
                chroma_tx,
                base_q_idx,
                alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
                None,
                None,
            )?;
        }
    }
    neighbours.record(at, side, mode_for_tx, &[luma_grid, u_grid, v_grid]);
    neighbours.record_inter(at, side, skip, is_inter);
    neighbours.fill_skip_grid((r * (SUB / MI), c * (SUB / MI)), side / MI, skip);
    neighbours.fill_lf_grid(
        (r * (SUB / MI), c * (SUB / MI)),
        side / MI,
        side as u8,
        is_inter,
    );
    Ok(())
}

/// Decodes one 8x8 leaf of a straddling 16x16 inter-frame block
/// (lane-av1inter8), mirroring [`crate::tile::write_inter_frame_leaf8`]'s
/// exact symbol order: its own skip flag, intra/inter choice and (when
/// inter) `NEARESTMV`/`NEWMV` chain against this leaf's own 2x2-mi mv-stack
/// window, or (when intra) `Y_MODE`, then its own luma and two chroma
/// transform blocks through [`TxbSet::Luma8Inter`]/[`TxbSet::Chroma4`],
/// same as [`decode_leaf8`]'s coefficient layer but with
/// motion-compensated prediction instead of intra prediction when inter.
/// `outer_at` (in [`SUB`]-grid units) is the enclosing 16x16 slot's
/// skip/intra-inter context, overridden by `prev_leaf` exactly as
/// [`decode_leaf8`] overrides mode context: `Neighbours`' above/left arrays
/// only resolve to [`SUB`] granularity. Hands back this leaf's own skip flag
/// and intra/inter choice, for the next leaf or the caller's final
/// write-back.
#[allow(clippy::too_many_arguments)]
fn decode_inter_block8(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    grid: &mut MiGrid,
    mi_cols: u32,
    mi_rows: u32,
    outer_at: (usize, usize),
    leaf_mi: (usize, usize),
    y: &mut PlaneBuf,
    u: &mut PlaneBuf,
    v: &mut PlaneBuf,
    ref_y: &PlaneBuf,
    ref_u: &PlaneBuf,
    ref_v: &PlaneBuf,
    base_q_idx: u8,
    scan8: &[u16],
    scan4: &[u16],
    prev_leaf: Option<((usize, usize), bool, bool)>,
    allow_high_precision_mv: bool,
) -> Result<(bool, bool)> {
    const LAST_FRAME: i8 = 1;
    /// `Y_MODE`'s size group (`common_data.h`'s `size_group_lookup[BLOCK_8X8]`).
    const SIZE_GROUP_8: usize = 1;

    let (px, py) = (leaf_mi.1 * MI, leaf_mi.0 * MI);
    let (cpx, cpy) = (px / 2, py / 2);
    const SIDE: usize = 8;
    const CHROMA_SIDE: usize = 4;

    let (r, c) = outer_at;
    let mut above_skip = neighbours.above_skip[c];
    let mut left_skip = neighbours.left_skip[r];
    let mut above_inter = neighbours.above_inter[c];
    let mut left_inter = neighbours.left_inter[r];
    if let Some(((pr, pc), pskip, pinter)) = prev_leaf {
        if pc == leaf_mi.1 && leaf_mi.0 == pr + 2 {
            above_skip = pskip;
            above_inter = pinter;
        } else if pr == leaf_mi.0 && leaf_mi.1 == pc + 2 {
            left_skip = pskip;
            left_inter = pinter;
        }
    }
    let skip_ctx = usize::from(above_skip) + usize::from(left_skip);
    let skip = dec.symbol(&mut cdfs.skip[skip_ctx]) == 1;

    let (has_above, has_left) = (leaf_mi.0 > 0, leaf_mi.1 > 0);
    let ii_ctx = intra_inter_ctx(has_above, has_left, above_inter, left_inter);
    let is_inter = dec.symbol(&mut cdfs.intra_inter[ii_ctx]) == 1;

    let mode_for_tx;
    let (luma_grid, u_grid, v_grid);
    if is_inter {
        let sr_ctx = single_ref_ctx(above_inter || left_inter);
        let p1 = dec.symbol(&mut cdfs.single_ref[sr_ctx][0]);
        let p3 = dec.symbol(&mut cdfs.single_ref[sr_ctx][2]);
        let p4 = dec.symbol(&mut cdfs.single_ref[sr_ctx][3]);
        if (p1, p3, p4) != (0, 0, 0) {
            return Err(unsupported(
                "a reference frame other than LAST_FRAME (round 2)",
            ));
        }

        let (mi_row, mi_col) = leaf_mi;
        let stack = find_mv_stack(
            grid,
            mi_row,
            mi_col,
            2,
            2,
            LAST_FRAME,
            mi_cols as usize,
            mi_rows as usize,
        );

        let not_new = dec.symbol(&mut cdfs.new_mv[stack.new_mv_ctx]) == 1;
        let (mv, is_new_mv) = if !not_new {
            if stack.entries.len() > 1 {
                dec.symbol(&mut cdfs.drl_mode[stack.drl_ctx[0]]);
            }
            (
                read_mv(
                    dec,
                    &mut cdfs.mv_comp,
                    &mut cdfs.mv_joint,
                    stack.pred_mv,
                    allow_high_precision_mv,
                ),
                true,
            )
        } else {
            let not_zero = dec.symbol(&mut cdfs.zero_mv[stack.zero_mv_ctx]) == 1;
            if !not_zero {
                return Err(unsupported("GLOBALMV (round 3)"));
            }
            let nearest = dec.symbol(&mut cdfs.ref_mv[stack.ref_mv_ctx]) == 0;
            let mv = if nearest {
                stack.nearest_mv
            } else {
                let mut idx = 1usize;
                while idx < 3 && stack.entries.len() > idx + 1 {
                    if dec.symbol(&mut cdfs.drl_mode[stack.drl_ctx[idx]]) == 0 {
                        break;
                    }
                    idx += 1;
                }
                stack.entries.get(idx).map_or(stack.near_mv, |e| e.mv)
            };
            (mv, false)
        };
        for dr in 0..2 {
            for dc in 0..2 {
                grid.set(
                    mi_row + dr,
                    mi_col + dc,
                    MiInfo {
                        is_inter: true,
                        ref_frame: LAST_FRAME,
                        mv,
                        is_new_mv,
                        size: 2,
                    },
                );
            }
        }
        mode_for_tx = 0;

        let mut pred_y = vec![0u8; SIDE * SIDE];
        mc::predict(
            &ref_y.data,
            ref_y.width,
            ref_y.true_width,
            ref_y.true_height,
            mv_to_q4(px, mv.1, true),
            mv_to_q4(py, mv.0, true),
            SIDE,
            SIDE,
            &mut pred_y,
        );
        let mut pred_u = vec![0u8; CHROMA_SIDE * CHROMA_SIDE];
        mc::predict(
            &ref_u.data,
            ref_u.width,
            ref_u.true_width,
            ref_u.true_height,
            mv_to_q4(cpx, mv.1, false),
            mv_to_q4(cpy, mv.0, false),
            CHROMA_SIDE,
            CHROMA_SIDE,
            &mut pred_u,
        );
        let mut pred_v = vec![0u8; CHROMA_SIDE * CHROMA_SIDE];
        mc::predict(
            &ref_v.data,
            ref_v.width,
            ref_v.true_width,
            ref_v.true_height,
            mv_to_q4(cpx, mv.1, false),
            mv_to_q4(cpy, mv.0, false),
            CHROMA_SIDE,
            CHROMA_SIDE,
            &mut pred_v,
        );

        if skip {
            y.reconstruct_mc(px, py, SIDE, &pred_y, &vec![0i32; SIDE * SIDE]);
            u.reconstruct_mc(
                cpx,
                cpy,
                CHROMA_SIDE,
                &pred_u,
                &vec![0i32; CHROMA_SIDE * CHROMA_SIDE],
            );
            v.reconstruct_mc(
                cpx,
                cpy,
                CHROMA_SIDE,
                &pred_v,
                &vec![0i32; CHROMA_SIDE * CHROMA_SIDE],
            );
            luma_grid = vec![0i32; SIDE * SIDE];
            u_grid = vec![0i32; CHROMA_SIDE * CHROMA_SIDE];
            v_grid = vec![0i32; CHROMA_SIDE * CHROMA_SIDE];
        } else {
            let around = neighbours.around_mi(leaf_mi, 8);
            luma_grid = read_inter_plane(
                dec,
                cdfs,
                TxbSet::Luma8Inter,
                scan8,
                0,
                around[0],
                mode_for_tx,
                y,
                px,
                py,
                SIDE,
                base_q_idx,
                &pred_y,
            )?;
            u_grid = read_inter_plane(
                dec,
                cdfs,
                TxbSet::Chroma4,
                scan4,
                1,
                around[1],
                mode_for_tx,
                u,
                cpx,
                cpy,
                CHROMA_SIDE,
                base_q_idx,
                &pred_u,
            )?;
            v_grid = read_inter_plane(
                dec,
                cdfs,
                TxbSet::Chroma4,
                scan4,
                2,
                around[2],
                mode_for_tx,
                v,
                cpx,
                cpy,
                CHROMA_SIDE,
                base_q_idx,
                &pred_v,
            )?;
        }
    } else {
        let mode = dec.symbol(&mut cdfs.y_mode[SIZE_GROUP_8]);
        if mode >= 13 {
            return Err(unsupported(
                "an intra mode this decoder does not code (round 2)",
            ));
        }
        if (V_PRED..=D67_PRED).contains(&mode) {
            let angle = dec.symbol(&mut cdfs.angle_delta[mode - V_PRED]);
            if angle != ANGLE_DELTA_ZERO {
                return Err(unsupported(
                    "a nonzero angle delta (this encoder never writes one)",
                ));
            }
        }
        let uv_mode = dec.symbol(&mut cdfs.uv_mode_cfl[mode]);
        if uv_mode != DC_PRED {
            return Err(unsupported(
                "a non-DC chroma mode on an 8x8 inter-frame leaf (this encoder never writes one)",
            ));
        }
        mode_for_tx = mode;
        let (mi_row, mi_col) = leaf_mi;
        for dr in 0..2 {
            for dc in 0..2 {
                grid.set(
                    mi_row + dr,
                    mi_col + dc,
                    MiInfo {
                        is_inter: false,
                        ref_frame: -1,
                        mv: (0, 0),
                        is_new_mv: false,
                        size: 2,
                    },
                );
            }
        }

        let reach = Reach::of(SIDE, px, py, y.width, y.height);
        if skip {
            y.reconstruct(
                px,
                py,
                SIDE,
                mode,
                reach,
                &vec![0i32; SIDE * SIDE],
                None,
                None,
            );
            u.reconstruct(
                cpx,
                cpy,
                CHROMA_SIDE,
                DC_PRED,
                Reach::none(),
                &vec![0i32; CHROMA_SIDE * CHROMA_SIDE],
                None,
                None,
            );
            v.reconstruct(
                cpx,
                cpy,
                CHROMA_SIDE,
                DC_PRED,
                Reach::none(),
                &vec![0i32; CHROMA_SIDE * CHROMA_SIDE],
                None,
                None,
            );
            luma_grid = vec![0i32; SIDE * SIDE];
            u_grid = vec![0i32; CHROMA_SIDE * CHROMA_SIDE];
            v_grid = vec![0i32; CHROMA_SIDE * CHROMA_SIDE];
        } else {
            let around = neighbours.around_mi(leaf_mi, 8);
            luma_grid = read_plane(
                dec,
                cdfs,
                TxbSet::Luma8,
                scan8,
                0,
                around[0],
                mode,
                mode,
                reach,
                y,
                px,
                py,
                SIDE,
                TX8,
                base_q_idx,
                None,
                None,
                None,
            )?;
            u_grid = read_plane(
                dec,
                cdfs,
                TxbSet::Chroma4,
                scan4,
                1,
                around[1],
                mode,
                DC_PRED,
                Reach::none(),
                u,
                cpx,
                cpy,
                CHROMA_SIDE,
                TX4,
                base_q_idx,
                None,
                None,
                None,
            )?;
            v_grid = read_plane(
                dec,
                cdfs,
                TxbSet::Chroma4,
                scan4,
                2,
                around[2],
                mode,
                DC_PRED,
                Reach::none(),
                v,
                cpx,
                cpy,
                CHROMA_SIDE,
                TX4,
                base_q_idx,
                None,
                None,
                None,
            )?;
        }
    }
    neighbours.record_mi(leaf_mi, 8, &[luma_grid, u_grid, v_grid]);
    neighbours.fill_lf_grid(leaf_mi, 2, 8, is_inter);
    Ok((skip, is_inter))
}

/// Decodes the payload [`crate::tile::sb_coeff_inter_frame_tile`] writes,
/// against `reference` (the previous frame's own decoded picture, at this
/// frame's true size -- a decoder's single-slot DPB, matching what this
/// crate's encoder always predicts from), returning the picture it
/// reconstructs to. Mirrors [`decode_key_frame_tile`]'s own contract
/// (`mi_cols`/`mi_rows`/`base_q_idx` from the frame header,
/// `frame_width`/`frame_height` its true render size).
///
/// # Errors
/// Returns an error under the same conditions [`decode_inter_block`] does,
/// or when `reference` is not this frame's own true size, or when the tile
/// codes a 16x16 leaf whose own true edge cuts through it (round 2, same
/// refusal [`crate::tile`]'s writer gives its encoder).
pub fn decode_inter_frame_tile(
    data: &[u8],
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    frame_width: u32,
    frame_height: u32,
    reference: &Picture,
    cdef: &CdefParams,
    loop_filter: &LoopFilterParams,
    allow_high_precision_mv: bool,
) -> Result<Picture> {
    decode_inter_frame_tile_with_cdfs(
        data,
        mi_cols,
        mi_rows,
        base_q_idx,
        frame_width,
        frame_height,
        reference,
        cdef,
        loop_filter,
        None,
        allow_high_precision_mv,
    )
    .map(|(picture, _)| picture)
}

/// [`decode_inter_frame_tile`], threading a cross-frame CDF forward -- see
/// [`decode_key_frame_tile_with_cdfs`]'s doc for `initial_cdfs`/the returned
/// end-of-tile table. An inter frame's own header can name
/// `primary_ref_frame == PRIMARY_REF_NONE` too (an error-resilient stream),
/// so `None` is a real case here, not only a convenience default.
pub(crate) fn decode_inter_frame_tile_with_cdfs(
    data: &[u8],
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    frame_width: u32,
    frame_height: u32,
    reference: &Picture,
    cdef: &CdefParams,
    loop_filter: &LoopFilterParams,
    initial_cdfs: Option<Cdfs>,
    allow_high_precision_mv: bool,
) -> Result<(Picture, Cdfs)> {
    if mi_cols == 0 || mi_rows == 0 {
        return Err(unsupported("a frame with no mode-info grid"));
    }
    let (true_width, true_height) = ((mi_cols * 4) as usize, (mi_rows * 4) as usize);
    if reference.width != true_width || reference.height != true_height {
        return Err(unsupported(
            "a reference picture that is not this frame's own true size",
        ));
    }
    let (cols, rows) = block_grid(mi_cols, mi_rows);
    let (sb_cols, sb_rows) = (cols.div_ceil(2), rows.div_ceil(2));
    let (width, height) = (cols as usize * BLOCK, rows as usize * BLOCK);

    let ref_y = PlaneBuf {
        data: reference.y.clone(),
        width: reference.width,
        height: reference.height,
        true_width: reference.width,
        true_height: reference.height,
    };
    let ref_u = PlaneBuf {
        data: reference.u.clone(),
        width: reference.width / 2,
        height: reference.height / 2,
        true_width: reference.width / 2,
        true_height: reference.height / 2,
    };
    let ref_v = PlaneBuf {
        data: reference.v.clone(),
        width: reference.width / 2,
        height: reference.height / 2,
        true_width: reference.width / 2,
        true_height: reference.height / 2,
    };

    let mut y = PlaneBuf {
        data: vec![0u8; width * height],
        width,
        height,
        true_width,
        true_height,
    };
    let mut u = PlaneBuf {
        data: vec![0u8; width * height / 4],
        width: width / 2,
        height: height / 2,
        true_width: true_width / 2,
        true_height: true_height / 2,
    };
    let mut v = PlaneBuf {
        data: vec![0u8; width * height / 4],
        width: width / 2,
        height: height / 2,
        true_width: true_width / 2,
        true_height: true_height / 2,
    };

    let scan32 = default_scan(TX32);
    let scan16 = default_scan(TX16);
    let scan8 = default_scan(TX8);
    let scan4 = default_scan(TX4);

    let mut cdfs = initial_cdfs.unwrap_or_else(|| Cdfs::new(q_ctx_of(base_q_idx)));
    let mut dec = SymbolDecoder::new(data);
    let mut neighbours = Neighbours::new(
        cols as usize * 2,
        rows as usize * 2,
        mi_cols as usize,
        mi_rows as usize,
    );
    let mut grid = MiGrid::new(mi_cols as usize, mi_rows as usize);

    for sb_r in 0..sb_rows {
        neighbours.start_row();
        for sb_c in 0..sb_cols {
            let sb_at = (sb_r as usize * 4, sb_c as usize * 4);
            let sb_ctx = neighbours.partition_ctx(sb_at, SB);
            let (has_cols, has_rows) = (
                sb_c * SB_MI + SB_MI / 2 < mi_cols,
                sb_r * SB_MI + SB_MI / 2 < mi_rows,
            );
            match (has_cols, has_rows) {
                (true, true) => {
                    dec.symbol(&mut cdfs.partition_w64[sb_ctx]);
                }
                (true, false) => {
                    dec.symbol_fixed(&gather(&cdfs.partition_w64[sb_ctx], VERT_ALIKE));
                }
                (false, true) => {
                    dec.symbol_fixed(&gather(&cdfs.partition_w64[sb_ctx], HORZ_ALIKE));
                }
                (false, false) => {}
            }

            for quadrant in 0..4 {
                let (r32, c32) = (sb_r * 2 + quadrant / 2, sb_c * 2 + quadrant % 2);
                if r32 >= rows || c32 >= cols {
                    continue;
                }
                let at = (r32 as usize * 2, c32 as usize * 2);
                let ctx32 = neighbours.partition_ctx(at, BLOCK);
                let (has_cols32, has_rows32) = (
                    has_half(c32 * BLOCK_MI, BLOCK_MI, mi_cols),
                    has_half(r32 * BLOCK_MI, BLOCK_MI, mi_rows),
                );
                let part32 = if has_cols32 && has_rows32 {
                    dec.symbol(&mut cdfs.partition_w32[ctx32])
                } else {
                    match (has_cols32, has_rows32) {
                        (true, false) => {
                            dec.symbol_fixed(&gather(&cdfs.partition_w32[ctx32], VERT_ALIKE));
                        }
                        (false, true) => {
                            dec.symbol_fixed(&gather(&cdfs.partition_w32[ctx32], HORZ_ALIKE));
                        }
                        _ => {}
                    }
                    PARTITION_SPLIT
                };
                match part32 {
                    PARTITION_NONE => {
                        decode_inter_block(
                            &mut dec,
                            &mut cdfs,
                            &mut neighbours,
                            &mut grid,
                            at,
                            BLOCK,
                            mi_cols,
                            mi_rows,
                            &mut y,
                            &mut u,
                            &mut v,
                            &ref_y,
                            &ref_u,
                            &ref_v,
                            base_q_idx,
                            TxbSet::Luma32,
                            TxbSet::Luma32Inter,
                            TxbSet::Chroma16,
                            TX32,
                            TX16,
                            &scan32,
                            &scan16,
                            3,
                            allow_high_precision_mv,
                        )?;
                    }
                    PARTITION_SPLIT => {
                        for sub in 0..4 {
                            let (sr, sc) = (r32 as usize * 2 + sub / 2, c32 as usize * 2 + sub % 2);
                            if (sr as u32) * SUB_MI >= mi_rows || (sc as u32) * SUB_MI >= mi_cols {
                                continue;
                            }
                            let (has_cols16, has_rows16) = (
                                has_half(sc as u32 * SUB_MI, SUB_MI, mi_cols),
                                has_half(sr as u32 * SUB_MI, SUB_MI, mi_rows),
                            );
                            if !has_cols16 && !has_rows16 {
                                return Err(unsupported(
                                    "a 16x16 inter block whose true edge cuts through both \
                                     axes needs a rectangular transform this decoder does \
                                     not code yet",
                                ));
                            }
                            let at16 = (sr, sc);
                            if has_cols16 && has_rows16 {
                                let ctx16 = neighbours.partition_ctx(at16, SUB);
                                let part16 = dec.symbol(&mut cdfs.partition_w16[ctx16]);
                                if part16 != PARTITION_NONE {
                                    return Err(unsupported(
                                        "a partition below 16x16 (this encoder never writes one)",
                                    ));
                                }
                                decode_inter_block(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    &mut grid,
                                    at16,
                                    SUB,
                                    mi_cols,
                                    mi_rows,
                                    &mut y,
                                    &mut u,
                                    &mut v,
                                    &ref_y,
                                    &ref_u,
                                    &ref_v,
                                    base_q_idx,
                                    TxbSet::Luma16,
                                    TxbSet::Luma16Inter,
                                    TxbSet::Chroma8,
                                    TX16,
                                    TX8,
                                    &scan16,
                                    &scan8,
                                    2,
                                    allow_high_precision_mv,
                                )?;
                            } else {
                                let ctx16 = neighbours.partition_ctx(at16, SUB);
                                if has_cols16 {
                                    dec.symbol_fixed(&gather(
                                        &cdfs.partition_w16[ctx16],
                                        VERT_ALIKE,
                                    ));
                                } else {
                                    dec.symbol_fixed(&gather(
                                        &cdfs.partition_w16[ctx16],
                                        HORZ_ALIKE,
                                    ));
                                }
                                let (mi_row0, mi_col0) = (sr as u32 * SUB_MI, sc as u32 * SUB_MI);
                                let leaf_positions: Vec<(u32, u32)> = (0..4)
                                    .map(|i| (mi_row0 + (i / 2) * 2, mi_col0 + (i % 2) * 2))
                                    .filter(|&(mr, mc)| mr < mi_rows && mc < mi_cols)
                                    .collect();
                                let mut prev_leaf: Option<((usize, usize), bool, bool)> = None;
                                for (mr, mc) in leaf_positions {
                                    let leaf_mi = (mr as usize, mc as usize);
                                    let leaf_ctx = neighbours.partition_ctx_mi(leaf_mi, 8);
                                    let part8 = dec.symbol(&mut cdfs.partition_w8[leaf_ctx]);
                                    if part8 != PARTITION_NONE {
                                        return Err(unsupported(
                                            "a partition below 8x8 (this encoder never writes one)",
                                        ));
                                    }
                                    let (skip, is_inter) = decode_inter_block8(
                                        &mut dec,
                                        &mut cdfs,
                                        &mut neighbours,
                                        &mut grid,
                                        mi_cols,
                                        mi_rows,
                                        at16,
                                        leaf_mi,
                                        &mut y,
                                        &mut u,
                                        &mut v,
                                        &ref_y,
                                        &ref_u,
                                        &ref_v,
                                        base_q_idx,
                                        &scan8,
                                        &scan4,
                                        prev_leaf,
                                        allow_high_precision_mv,
                                    )?;
                                    prev_leaf = Some((leaf_mi, skip, is_inter));
                                }
                                if let Some((_, skip, is_inter)) = prev_leaf {
                                    neighbours.record_inter(at16, SUB, skip, is_inter);
                                }
                            }
                        }
                    }
                    _ => {
                        return Err(unsupported("a partition type this encoder never writes"));
                    }
                }
            }
        }
    }

    apply_deblock(&mut y, &mut u, &mut v, loop_filter, &neighbours);
    apply_cdef(&mut y, &mut u, &mut v, cdef, &neighbours);

    let (fw, fh) = (frame_width as usize, frame_height as usize);
    if fw == width && fh == height {
        return Ok((
            Picture {
                width,
                height,
                y: y.data,
                u: u.data,
                v: v.data,
            },
            cdfs,
        ));
    }
    let crop = |plane: &PlaneBuf, w: usize, h: usize| -> Vec<u8> {
        let mut out = Vec::with_capacity(w * h);
        for row in 0..h {
            out.extend_from_slice(&plane.data[row * plane.width..][..w]);
        }
        out
    };
    Ok((
        Picture {
            width: fw,
            height: fh,
            y: crop(&y, fw, fh),
            u: crop(&u, fw / 2, fh / 2),
            v: crop(&v, fw / 2, fh / 2),
        },
        cdfs,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // `crate::tile`'s `flat_key_frame_tile`/`dc_key_frame_tile_levels`/
    // `split_dc_key_frame_tile` are not exercised here: they are synthetic
    // single-purpose writers with their own skip-context rule (keyed to
    // superblock position, not the neighbour-tracked one every real block
    // uses), never reached by `encode_key_frame_with_modes` — decoding them
    // would need a second, throwaway context convention next to the real
    // one below. `sb_coeff_key_frame_tile`, what the real encoder writes, is
    // this decoder's one target, and the round-trip test below is against
    // that path with real quantised residual, not a synthetic all-DC frame.

    #[test]
    fn a_frame_with_no_mode_info_grid_is_refused() {
        assert!(
            decode_key_frame_tile(
                &[0u8; 4],
                0,
                32,
                32,
                0,
                128,
                false,
                &CdefParams::default(),
                &LoopFilterParams::default(),
                false,
                true,
            )
            .is_err()
        );
    }

    /// Encodes and decodes one picture with `modes`, asserting the decoder's
    /// planes are byte-exact against the encoder's own reconstruction.
    fn round_trips(w: usize, h: usize, modes: &[u8]) {
        use crate::encode::{Encoded, Picture as Pic};
        let mut source = vec![0u8; w * h];
        for row in 0..h {
            for col in 0..w {
                source[row * w + col] = ((row * 3 + col * 5) % 251) as u8;
            }
        }
        let picture = Pic {
            width: w,
            height: h,
            y: source,
            u: vec![128; w * h / 4],
            v: vec![128; w * h / 4],
        };
        let Encoded {
            tile,
            reconstruction,
            mi_cols,
            mi_rows,
            base_q_idx,
            ..
        }: Encoded = crate::encode::encode_key_frame_with_modes(&picture, 40, 0.0, modes).unwrap();
        let decoded = decode_key_frame_tile(
            &tile,
            mi_cols,
            mi_rows,
            base_q_idx,
            w as u32,
            h as u32,
            false,
            &CdefParams::default(),
            &LoopFilterParams::default(),
            false,
            true,
        )
        .unwrap();
        assert_eq!(decoded.width, w, "{w}x{h}: decoded width");
        assert_eq!(decoded.height, h, "{w}x{h}: decoded height");
        assert_eq!(decoded.y, reconstruction.y, "{w}x{h} luma mismatch");
        assert_eq!(decoded.u, reconstruction.u, "{w}x{h} chroma U mismatch");
        assert_eq!(decoded.v, reconstruction.v, "{w}x{h} chroma V mismatch");
    }

    #[test]
    fn real_yuv_round_trips_bit_exact_against_the_encoder_reconstruction() {
        for (w, h) in [(128usize, 128usize), (192, 128)] {
            round_trips(w, h, &[DC_PRED as u8]);
        }
    }

    /// Every one of the thirteen key-frame intra modes, at their base angle,
    /// on real (non-flat) content: the mode search picks whichever wins each
    /// block, so this exercises the directional edge/reach plumbing this
    /// decoder round adds, not just `DC_PRED`.
    #[test]
    fn every_intra_mode_round_trips_bit_exact_against_the_encoder_reconstruction() {
        for (w, h) in [(128usize, 128usize), (192, 128)] {
            round_trips(w, h, &crate::intra::KEY_FRAME_MODES);
        }
    }

    /// Sizes not a whole number of 64x64 superblocks (nor even of 32x32
    /// blocks, for 854x480): the gathered-CDF partial-superblock/32x32 path
    /// this decoder round adds.
    #[test]
    fn odd_sizes_round_trip_bit_exact_against_the_encoder_reconstruction() {
        for (w, h) in [(854usize, 480usize), (216, 96)] {
            round_trips(w, h, &crate::intra::KEY_FRAME_MODES);
        }
    }

    /// mod-32==8 sizes (lane-av1-rect): the true frame edge falls inside a
    /// 16x16 block itself, forcing the encoder's 8x8-leaf straddle path
    /// ([`crate::tile::write_leaf8`]) regardless of the RD-cost guard, since
    /// no whole 16x16/32x32 legally covers the true edge here -- this decoder
    /// round's leaf8 read path (`decode_leaf8`, gathered `partition_w16` split
    /// bit, `partition_w8` leaves).
    #[test]
    fn leaf8_straddle_sizes_round_trip_bit_exact_against_the_encoder_reconstruction() {
        for (w, h) in [(40usize, 32usize), (640, 360)] {
            round_trips(w, h, &crate::intra::KEY_FRAME_MODES);
        }
    }

    /// A skipped block codes no residual syntax at all (spec 5.11.34):
    /// straight prediction on every plane. The real key-frame writer never
    /// emits `skip = 1` ([`crate::tile::write_intra_mode`] hardcodes it to
    /// `0`), so this constructs the exact symbol sequence a writer that did
    /// emit it would produce, by hand, and asserts the decoded reconstruction
    /// is pure DC prediction (mid-grey, with no neighbours) rather than
    /// refusing.
    #[test]
    fn a_skipped_block_decodes_to_pure_prediction() {
        use crate::msac::SymbolEncoder;
        let mut cdfs = Cdfs::new(q_ctx_of(40));
        let mut enc = SymbolEncoder::new();
        // One whole, unsplit 64x64 superblock, coded skip = true.
        enc.symbol(PARTITION_NONE, &mut cdfs.partition_w64[0]);
        enc.symbol(1, &mut cdfs.skip[0]);
        enc.symbol(
            DC_PRED,
            &mut cdfs.kf_y_mode[INTRA_MODE_CTX[DC_PRED]][INTRA_MODE_CTX[DC_PRED]],
        );
        enc.symbol(DC_PRED, &mut cdfs.uv_mode_no_cfl[DC_PRED]);
        let data = enc.finish();
        let decoded = decode_key_frame_tile(
            &data,
            16,
            16,
            40,
            64,
            64,
            false,
            &CdefParams::default(),
            &LoopFilterParams::default(),
            false,
            true,
        )
        .unwrap();
        assert!(
            decoded.y.iter().all(|&s| s == 128),
            "a skipped DC_PRED block with no neighbours predicts flat mid-grey"
        );
    }

    // ffmpeg cross-oracle: an independent decoder (not this crate's own
    // encoder-side reconstruction) agrees with what this module decodes,
    // duplicating `crate::encode`'s own `#[cfg(test)]` helpers of the same
    // name (that module is another lane's territory this round, and the
    // helpers are `#[cfg(test)]`-private to it).
    use std::io::Write;
    use std::process::{Command, Stdio};

    fn have_ffmpeg() -> bool {
        Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Decodes one AV1 OBU stream with ffmpeg and hands back its one 4:2:0
    /// frame's planes, at ffmpeg's own (coded, block-padded) size.
    fn ffmpeg_decode(stream: &[u8], width: usize, height: usize) -> Picture {
        let mut child = Command::new("ffmpeg")
            .args([
                "-v", "error", "-f", "obu", "-i", "-", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("ffmpeg failed to start");
        child
            .stdin
            .take()
            .expect("ffmpeg stdin")
            .write_all(stream)
            .expect("writing the stream to ffmpeg");
        let out = child.wait_with_output().expect("ffmpeg failed to run");
        assert!(
            out.status.success(),
            "ffmpeg refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (luma, chroma) = (width * height, width * height / 4);
        assert_eq!(
            out.stdout.len(),
            luma + 2 * chroma,
            "expected one 4:2:0 frame"
        );
        Picture {
            width,
            height,
            y: out.stdout[..luma].to_vec(),
            u: out.stdout[luma..luma + chroma].to_vec(),
            v: out.stdout[luma + chroma..].to_vec(),
        }
    }

    /// Decodes `frames` concatenated 4:2:0 frames out of one AV1 OBU stream.
    fn ffmpeg_decode_sequence(
        stream: &[u8],
        width: usize,
        height: usize,
        frames: usize,
    ) -> Vec<Picture> {
        let mut child = Command::new("ffmpeg")
            .args([
                "-v", "error", "-f", "obu", "-i", "-", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("ffmpeg failed to start");
        child
            .stdin
            .take()
            .expect("ffmpeg stdin")
            .write_all(stream)
            .expect("writing the stream to ffmpeg");
        let out = child.wait_with_output().expect("ffmpeg failed to run");
        assert!(
            out.status.success(),
            "ffmpeg refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (luma, chroma) = (width * height, width * height / 4);
        let frame_bytes = luma + 2 * chroma;
        assert_eq!(
            out.stdout.len(),
            frame_bytes * frames,
            "expected {frames} 4:2:0 frames, ffmpeg said: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        (0..frames)
            .map(|i| {
                let base = i * frame_bytes;
                Picture {
                    width,
                    height,
                    y: out.stdout[base..base + luma].to_vec(),
                    u: out.stdout[base + luma..base + luma + chroma].to_vec(),
                    v: out.stdout[base + luma + chroma..base + frame_bytes].to_vec(),
                }
            })
            .collect()
    }

    /// `ffprobe`'s reported coded `width,height` for one OBU stream --
    /// duplicating `crate::encode`'s own helper (per-call temp naming: the
    /// test binary runs callers on parallel threads, and a shared name lets
    /// one test's cleanup race another's still-reading ffprobe --
    /// pid-keyed-temp-path class).
    fn ffprobe_size(stream: &[u8]) -> (u32, u32) {
        static PROBE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ec-av1-decode-probe-{}-{}.obu",
            std::process::id(),
            PROBE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, stream).expect("writing the probe stream");
        let out = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-f",
                "obu",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height",
                "-of",
                "csv=p=0",
            ])
            .arg(&path)
            .output()
            .expect("ffprobe failed to run");
        let _ = std::fs::remove_file(&path);
        assert!(
            out.status.success(),
            "ffprobe refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        let mut fields = text.trim().split(',');
        let width: u32 = fields.next().expect("ffprobe width").parse().unwrap();
        let height: u32 = fields.next().expect("ffprobe height").parse().unwrap();
        (width, height)
    }

    /// This module's own decoder against an independent one (ffmpeg), not
    /// just against this crate's encoder-side reconstruction: both decoders
    /// read the identical tile bytes, so agreement here rules out a bug this
    /// crate's own writer and reader could otherwise share.
    #[test]
    fn ffmpeg_and_this_decoder_agree_on_a_key_frame() {
        if !have_ffmpeg() {
            eprintln!("SKIP ffmpeg_and_this_decoder_agree_on_a_key_frame: no ffmpeg");
            return;
        }
        use crate::encode::encode_key_frame;
        for &(width, height) in &[(64usize, 64usize), (216, 96), (40, 32)] {
            let picture = round_trip_test_card(width, height);
            let encoded = encode_key_frame(&picture, 100, 0.5).unwrap();
            let (coded_w, coded_h) = ffprobe_size(&encoded.stream);
            let ffmpeg_decoded = ffmpeg_decode(&encoded.stream, coded_w as usize, coded_h as usize);
            let ours = decode_key_frame_tile(
                &encoded.tile,
                encoded.mi_cols,
                encoded.mi_rows,
                encoded.base_q_idx,
                coded_w,
                coded_h,
                false,
                &CdefParams::default(),
                &LoopFilterParams::default(),
                false,
                true,
            )
            .unwrap();
            assert_eq!(ours.y, ffmpeg_decoded.y, "{width}x{height}: luma vs ffmpeg");
            assert_eq!(ours.u, ffmpeg_decoded.u, "{width}x{height}: U vs ffmpeg");
            assert_eq!(ours.v, ffmpeg_decoded.v, "{width}x{height}: V vs ffmpeg");
        }
    }

    fn round_trip_test_card(width: usize, height: usize) -> crate::encode::Picture {
        let mut picture = crate::encode::Picture::grey(width, height);
        for row in 0..height {
            for col in 0..width {
                picture.y[row * width + col] = ((row * 7 + col * 11) % 251) as u8;
            }
        }
        for row in 0..height / 2 {
            for col in 0..width / 2 {
                let i = row * width / 2 + col;
                picture.u[i] = (100 + (col * 60 / (width / 2).max(1))) as u8;
                picture.v[i] = (200 - (row * 80 / (height / 2).max(1))) as u8;
            }
        }
        picture
    }

    fn panned_test_card(width: usize, height: usize, shift: i64) -> crate::encode::Picture {
        let mut picture = crate::encode::Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let sx = (x as i64 - shift).rem_euclid(width as i64) as f64;
                let gradient = sx * 200.0 / width as f64;
                picture.y[y * width + x] = (20.0 + gradient).clamp(0.0, 255.0) as u8;
            }
        }
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                let sx = (x as i64 - shift / 2).rem_euclid((width / 2) as i64) as usize;
                let i = y * width / 2 + x;
                picture.u[i] = (100 + (sx * 60 / (width / 2))) as u8;
                picture.v[i] = (200 - (y * 80 / (height / 2))) as u8;
            }
        }
        picture
    }

    /// A GOP (one key frame plus several inter frames, each panned so its
    /// blocks actually take `NEARESTMV`/`NEWMV` rather than an all-skip
    /// `(0, 0)` no-op) decodes bit-exact against the encoder's own
    /// reconstruction, frame by frame, chaining each decoded frame straight
    /// into the next as its reference -- exactly the single-slot DPB
    /// [`crate::encode::encode_sequence`] itself threads.
    fn gop_round_trips(width: usize, height: usize) {
        use crate::encode::encode_sequence;
        let pictures: Vec<_> = (0..4)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let encoded = encode_sequence(&pictures, 100, 0.5).unwrap();
        assert_eq!(encoded.frames.len(), 4);

        let key = &encoded.frames[0];
        let mut reference = decode_key_frame_tile(
            &key.tile,
            key.mi_cols,
            key.mi_rows,
            key.base_q_idx,
            width as u32,
            height as u32,
            false,
            &CdefParams::default(),
            &LoopFilterParams::default(),
            false,
            true,
        )
        .unwrap();
        assert_eq!(
            reference.y, key.reconstruction.y,
            "{width}x{height} frame 0 luma"
        );
        assert_eq!(
            reference.u, key.reconstruction.u,
            "{width}x{height} frame 0 U"
        );
        assert_eq!(
            reference.v, key.reconstruction.v,
            "{width}x{height} frame 0 V"
        );

        for (i, frame) in encoded.frames.iter().enumerate().skip(1) {
            let decoded = decode_inter_frame_tile(
                &frame.tile,
                frame.mi_cols,
                frame.mi_rows,
                frame.base_q_idx,
                width as u32,
                height as u32,
                &reference,
                &CdefParams::default(),
                &LoopFilterParams::default(),
                false,
            )
            .unwrap();
            assert_eq!(
                decoded.y, frame.reconstruction.y,
                "{width}x{height} frame {i} luma"
            );
            assert_eq!(
                decoded.u, frame.reconstruction.u,
                "{width}x{height} frame {i} U"
            );
            assert_eq!(
                decoded.v, frame.reconstruction.v,
                "{width}x{height} frame {i} V"
            );
            reference = decoded;
        }
    }

    #[test]
    fn a_gop_round_trips_bit_exact_against_the_encoder_reconstruction() {
        gop_round_trips(128, 64);
    }

    /// Same claim, at a size that is not a whole number of 32x32 blocks
    /// (216x96: 216/32 is not exact), so an inter frame's own 16x16-leaf
    /// split path is exercised too.
    #[test]
    fn an_odd_size_gop_round_trips_bit_exact_against_the_encoder_reconstruction() {
        gop_round_trips(216, 96);
    }

    /// The GOP cross-oracle: ffmpeg decodes the whole sequence the same way
    /// this module does, frame for frame.
    /// A NEARMV block (`not_new`, `not_zero`, `!nearest`) predicts from the
    /// MV stack's *second* candidate (`near_mv`), not its first
    /// (`nearest_mv`) -- a hand-built symbol stream drives
    /// [`decode_inter_block`] directly, since this crate's own encoder never
    /// writes NEARMV (round 3: it never chooses the mode).
    #[test]
    fn a_nearmv_block_predicts_from_the_stacks_second_candidate() {
        use crate::msac::SymbolEncoder;
        use crate::mvstack::{MiGrid, MiInfo, find_mv_stack, single_ref_ctx};

        const LAST_FRAME: i8 = 1;
        let (mi_cols, mi_rows) = (12u32, 12u32);
        let side = 16usize; // one 16x16 leaf block
        let (r, c) = (1usize, 1usize); // `at`, in SUB (16px) units
        let (mi_row, mi_col) = (r * SUB_MI as usize, c * SUB_MI as usize);
        let bw4 = side / 4;

        let mut grid = MiGrid::new(mi_cols as usize, mi_rows as usize);
        let (above_mv, left_mv) = ((4, 4), (8, 8));
        let neighbour = |mv| MiInfo {
            is_inter: true,
            ref_frame: LAST_FRAME,
            mv,
            is_new_mv: false,
            size: 1,
        };
        for col in mi_col..mi_col + bw4 {
            grid.set(mi_row - 1, col, neighbour(above_mv));
        }
        for row in mi_row..mi_row + bw4 {
            grid.set(row, mi_col - 1, neighbour(left_mv));
        }

        let stack = find_mv_stack(
            &grid,
            mi_row,
            mi_col,
            bw4,
            bw4,
            LAST_FRAME,
            mi_cols as usize,
            mi_rows as usize,
        );
        // Sanity on the test's own setup, not the code under test: two equal-
        // weight candidates keep scan order (above, then left -- module doc).
        assert_eq!(stack.entries.len(), 2);
        assert_eq!(stack.nearest_mv, above_mv);
        assert_eq!(stack.near_mv, left_mv);

        let (skip_ctx, ii_ctx, sr_ctx) = (
            0usize,
            intra_inter_ctx(true, true, false, false),
            single_ref_ctx(false),
        );

        let mut cdfs = Cdfs::new(q_ctx_of(100));
        let mut enc = SymbolEncoder::new();
        enc.symbol(1, &mut cdfs.skip[skip_ctx]); // skip
        enc.symbol(1, &mut cdfs.intra_inter[ii_ctx]); // is_inter
        enc.symbol(0, &mut cdfs.single_ref[sr_ctx][0]); // LAST_FRAME
        enc.symbol(0, &mut cdfs.single_ref[sr_ctx][2]);
        enc.symbol(0, &mut cdfs.single_ref[sr_ctx][3]);
        enc.symbol(1, &mut cdfs.new_mv[stack.new_mv_ctx]); // not_new
        enc.symbol(1, &mut cdfs.zero_mv[stack.zero_mv_ctx]); // not_zero
        enc.symbol(1, &mut cdfs.ref_mv[stack.ref_mv_ctx]); // !nearest -> NEARMV
        let data = enc.finish();

        let (width, height) = (64usize, 64usize);
        let ref_pattern: Vec<u8> = (0..width * height).map(|i| (i % 251) as u8).collect();
        let ref_plane = |scale: usize| PlaneBuf {
            data: ref_pattern
                .iter()
                .step_by(scale.max(1))
                .take((width / scale) * (height / scale))
                .copied()
                .collect(),
            width: width / scale,
            height: height / scale,
            true_width: width / scale,
            true_height: height / scale,
        };
        let ref_y = PlaneBuf {
            data: ref_pattern.clone(),
            width,
            height,
            true_width: width,
            true_height: height,
        };
        let ref_u = ref_plane(2);
        let ref_v = ref_plane(2);

        let mut y = PlaneBuf {
            data: vec![0u8; width * height],
            width,
            height,
            true_width: width,
            true_height: height,
        };
        let mut u = PlaneBuf {
            data: vec![0u8; (width / 2) * (height / 2)],
            width: width / 2,
            height: height / 2,
            true_width: width / 2,
            true_height: height / 2,
        };
        let mut v = PlaneBuf {
            data: vec![0u8; (width / 2) * (height / 2)],
            width: width / 2,
            height: height / 2,
            true_width: width / 2,
            true_height: height / 2,
        };

        let mut cdfs = Cdfs::new(q_ctx_of(100));
        let mut dec = SymbolDecoder::new(&data);
        let mut neighbours = Neighbours::new(3, 3, mi_cols as usize, mi_rows as usize);
        decode_inter_block(
            &mut dec,
            &mut cdfs,
            &mut neighbours,
            &mut grid,
            (r, c),
            side,
            mi_cols,
            mi_rows,
            &mut y,
            &mut u,
            &mut v,
            &ref_y,
            &ref_u,
            &ref_v,
            100,
            TxbSet::Luma16,
            TxbSet::Luma16Inter,
            TxbSet::Chroma8,
            TX16,
            TX8,
            &[],
            &[],
            0,
            false,
        )
        .unwrap();

        let (px, py) = (c * SUB, r * SUB);
        let mut want_near = vec![0u8; side * side];
        crate::mc::predict(
            &ref_y.data,
            ref_y.width,
            ref_y.true_width,
            ref_y.true_height,
            mv_to_q4(px, left_mv.1, true),
            mv_to_q4(py, left_mv.0, true),
            side,
            side,
            &mut want_near,
        );
        let mut want_nearest = vec![0u8; side * side];
        crate::mc::predict(
            &ref_y.data,
            ref_y.width,
            ref_y.true_width,
            ref_y.true_height,
            mv_to_q4(px, above_mv.1, true),
            mv_to_q4(py, above_mv.0, true),
            side,
            side,
            &mut want_nearest,
        );
        assert_ne!(
            want_near, want_nearest,
            "test setup: the two candidate MVs must predict differently"
        );

        let mut got = vec![0u8; side * side];
        for row in 0..side {
            got[row * side..(row + 1) * side].copy_from_slice(
                &y.data[(py + row) * y.width + px..(py + row) * y.width + px + side],
            );
        }
        assert_eq!(
            got, want_near,
            "NEARMV predicted from the wrong stack entry"
        );
    }

    #[test]
    fn ffmpeg_and_this_decoder_agree_on_a_gop() {
        if !have_ffmpeg() {
            eprintln!("SKIP ffmpeg_and_this_decoder_agree_on_a_gop: no ffmpeg");
            return;
        }
        use crate::encode::encode_sequence;
        let (width, height) = (128usize, 64usize);
        let pictures: Vec<_> = (0..3)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let encoded = encode_sequence(&pictures, 100, 0.5).unwrap();
        let (coded_w, coded_h) = ffprobe_size(&encoded.stream);
        let ffmpeg_frames =
            ffmpeg_decode_sequence(&encoded.stream, coded_w as usize, coded_h as usize, 3);

        let key = &encoded.frames[0];
        let mut reference = decode_key_frame_tile(
            &key.tile,
            key.mi_cols,
            key.mi_rows,
            key.base_q_idx,
            coded_w,
            coded_h,
            false,
            &CdefParams::default(),
            &LoopFilterParams::default(),
            false,
            true,
        )
        .unwrap();
        assert_eq!(reference.y, ffmpeg_frames[0].y, "frame 0 luma vs ffmpeg");

        for (i, frame) in encoded.frames.iter().enumerate().skip(1) {
            let decoded = decode_inter_frame_tile(
                &frame.tile,
                frame.mi_cols,
                frame.mi_rows,
                frame.base_q_idx,
                coded_w,
                coded_h,
                &reference,
                &CdefParams::default(),
                &LoopFilterParams::default(),
                false,
            )
            .unwrap();
            assert_eq!(decoded.y, ffmpeg_frames[i].y, "frame {i} luma vs ffmpeg");
            assert_eq!(decoded.u, ffmpeg_frames[i].u, "frame {i} U vs ffmpeg");
            assert_eq!(decoded.v, ffmpeg_frames[i].v, "frame {i} V vs ffmpeg");
            reference = decoded;
        }
    }
}
