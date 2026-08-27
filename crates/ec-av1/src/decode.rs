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

use ec_core::{Error, Result};

use crate::cdf;
use crate::cdf_state::{Cdfs, TxbSet, TxbTables};
use crate::encode::{Picture, Reach};
use crate::intra::predict;
use crate::msac::SymbolDecoder;
use crate::tile::{INTRA_MODE_CTX, block_grid, has_half};
use crate::transform::dequant_and_inverse;

const PARTITION_NONE: usize = 0;
const PARTITION_SPLIT: usize = 3;
const SB_MI: u32 = 16;
const BLOCK_MI: u32 = 8;
const SUB_MI: u32 = 4;
const MI: usize = 4;
const SB: usize = 64;
const BLOCK: usize = 32;
const SUB: usize = 16;
const TX32: usize = 32;
const TX16: usize = 16;
const TX8: usize = 8;
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
fn q_ctx_of(base_q_idx: u8) -> usize {
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

fn base_ctx(grid: &[i32], side: usize, row: usize, col: usize) -> usize {
    if row == 0 && col == 0 {
        return 0;
    }
    let mag: i32 = [(1, 0), (0, 1), (1, 1), (2, 0), (0, 2)]
        .iter()
        .map(|&(dr, dc)| neighbour(grid, side, row + dr, col + dc).abs().min(3))
        .sum();
    let offset = cdf::NZ_MAP_CTX_OFFSET_32[row.min(4)][col.min(4)] as usize;
    (((mag + 1) >> 1).min(4) as usize) + offset
}

fn br_ctx(grid: &[i32], side: usize, row: usize, col: usize) -> usize {
    let mag: i32 = [(1, 0), (0, 1), (1, 1)]
        .iter()
        .map(|&(dr, dc)| neighbour(grid, side, row + dr, col + dc).abs())
        .sum();
    let mag = (((mag + 1) >> 1).min(6)) as usize;
    if row == 0 && col == 0 {
        mag
    } else if row < 2 && col < 2 {
        mag + 7
    } else {
        mag + 14
    }
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
fn read_eob(dec: &mut SymbolDecoder, coding: &mut TxbTables) -> usize {
    const GROUP_START: [usize; 12] = [0, 1, 2, 3, 5, 9, 17, 33, 65, 129, 257, 513];
    const OFFSET_BITS: [u32; 12] = [0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9];

    let group = dec.symbol(coding.eob_pt) + 1;
    let bits = OFFSET_BITS[group];
    let mut offset = 0u32;
    if bits > 0 {
        let top = dec.symbol(&mut coding.eob_extra[group - 3]) as u32;
        offset = top << (bits - 1);
        if bits > 1 {
            offset |= dec.literal(bits - 1);
        }
    }
    GROUP_START[group] + offset as usize
}

/// One transform block's levels, the inverse of [`crate::tile`]'s
/// `write_coeffs`, returned as a `side * side` grid in raster order.
///
/// # Errors
/// Returns an error if the transform codes a type other than `DCT_DCT` — this
/// decoder reads only the intra key-frame path the encoder still writes,
/// which never selects another type.
fn read_coeffs(
    dec: &mut SymbolDecoder,
    coding: &mut TxbTables,
    scan: &[u16],
    skip_ctx: usize,
    sign_ctx: usize,
) -> Result<Vec<i32>> {
    let side = coding.side;
    let mut grid = vec![0i32; side * side];
    let all_zero = dec.symbol(&mut coding.txb_skip[skip_ctx]) == 1;
    if all_zero {
        return Ok(grid);
    }
    if let Some(tx_type) = coding.tx_type.as_deref_mut() {
        let t = dec.symbol(tx_type);
        if t != 1 {
            return Err(unsupported(
                "a transform type other than DCT_DCT (round 2: inter/other intra tx sets)",
            ));
        }
    }

    let eob = read_eob(dec, coding);
    let mut levels = vec![0i32; side * side];
    for scan_idx in (0..eob).rev() {
        let pos = scan[scan_idx] as usize;
        let (row, col) = (pos / side, pos % side);
        let level = if scan_idx == eob - 1 {
            let ctx = eob_coeff_ctx(scan_idx, side * side);
            dec.symbol(&mut coding.base_eob[ctx]) as i32 + 1
        } else {
            let ctx = base_ctx(&levels, side, row, col);
            dec.symbol(&mut coding.base[ctx]) as i32
        };
        let level = if level > NUM_BASE_LEVELS {
            let ctx = br_ctx(&levels, side, row, col);
            let mut level = level;
            let mut sent = 0;
            loop {
                let k = dec.symbol(&mut coding.br[ctx]) as i32;
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
            dec.symbol(&mut coding.dc_sign[sign_ctx]) == 1
        } else {
            dec.literal(1) == 1
        };
        let level = if level.abs_diff(0) as i32 > MAX_BR_LEVEL {
            level + read_golomb(dec)? as i32
        } else {
            level
        };
        grid[pos as usize] = if negative { -level } else { level };
    }
    Ok(grid)
}

/// What one coded block leaves behind for the blocks that read it as a
/// neighbour: whether it coded anything at all, and the sign of its DC —
/// [`crate::tile`]'s own private `Neighbour`.
#[derive(Clone, Copy, Default)]
struct Neighbour {
    coded: bool,
    dc: Option<bool>,
}

fn neighbour_state(grid: &[i32]) -> Neighbour {
    Neighbour {
        coded: grid.iter().any(|&l| l != 0),
        dc: (grid[0] != 0).then_some(grid[0] < 0),
    }
}

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
    mi_cols: usize,
    mi_rows: usize,
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
            mi_cols,
            mi_rows,
        }
    }

    fn start_row(&mut self) {
        self.left.iter_mut().for_each(|l| *l = Default::default());
        self.left_mode.iter_mut().for_each(|m| *m = DC_PRED);
        self.left_side.iter_mut().for_each(|s| *s = SB);
    }

    fn partition_ctx(&self, at: (usize, usize), side: usize) -> usize {
        let (r, c) = at;
        2 * usize::from(self.left_side[r] * 2 <= side) + usize::from(self.above_side[c] * 2 <= side)
    }

    /// The gathered coded/DC-sign state of the blocks above and to the left
    /// of a block `side` samples across, per plane.
    fn around(&self, (r, c): (usize, usize), side: usize) -> [(bool, bool, i32); 3] {
        let (mi_r, mi_c, side_mi) = (r * (SUB / MI), c * (SUB / MI), side / MI);
        std::array::from_fn(|plane| {
            let mut above_coded = false;
            let mut left_coded = false;
            let mut vote = 0;
            for cell in 0..side_mi {
                let above = &self.above[mi_c + cell][plane];
                let left = &self.left[mi_r + cell][plane];
                above_coded |= above.coded;
                left_coded |= left.coded;
                vote += dc_vote(above.dc) + dc_vote(left.dc);
            }
            (above_coded, left_coded, vote)
        })
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
        let states: [Neighbour; 3] = std::array::from_fn(|plane| neighbour_state(&grids[plane]));
        for cell in 0..side / SUB {
            self.above_mode[c + cell] = mode;
            self.left_mode[r + cell] = mode;
            self.above_side[c + cell] = side;
            self.left_side[r + cell] = side;
        }
        let (mi_r, mi_c, side_mi) = (r * (SUB / MI), c * (SUB / MI), side / MI);
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
}

/// Reads one coded block's skip flag, luma mode, angle delta (if the mode
/// carries one) and chroma mode, mirroring [`crate::tile`]'s `write_intra_mode`
/// exactly — including its skip context, which the real key-frame writer
/// always writes at a fixed context of zero (it never tracks a neighbour-based
/// one for intra blocks; that is the inter-frame writer's rule instead).
/// Refuses a nonzero angle delta or a chroma-from-luma mode: this decoder
/// reconstructs the thirteen luma modes at their base angle, DC-only chroma.
fn read_intra_mode(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    above_mode: usize,
    left_mode: usize,
    cfl: bool,
) -> Result<(bool, usize)> {
    let skip = dec.symbol(&mut cdfs.skip[0]) != 0;
    let mode =
        dec.symbol(&mut cdfs.kf_y_mode[INTRA_MODE_CTX[above_mode]][INTRA_MODE_CTX[left_mode]]);
    if (V_PRED..=D67_PRED).contains(&mode) {
        let angle = dec.symbol(&mut cdfs.angle_delta[mode - V_PRED]);
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
    if uv_mode != DC_PRED {
        return Err(unsupported("a chroma-from-luma mode (round 2)"));
    }
    Ok((skip, mode))
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
    fn reconstruct(
        &mut self,
        x: usize,
        y: usize,
        side: usize,
        mode: usize,
        reach: Reach,
        residual: &[i32],
    ) {
        let (above, left, corner) = self.edges(x, y, side, reach);
        let mut prediction = vec![0u8; side * side];
        predict(
            mode as u8,
            above.as_deref(),
            left.as_deref(),
            corner,
            side,
            &mut prediction,
        );
        for row in 0..side {
            for col in 0..side {
                let sample = (i32::from(prediction[row * side + col]) + residual[row * side + col])
                    .clamp(0, 255) as u8;
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
) -> Result<Vec<i32>> {
    let skip_ctx = if plane_idx == 0 {
        0
    } else {
        usize::from(around.0) + usize::from(around.1)
    };
    let mut coding = cdfs.txb(set, tx_mode);
    let grid = read_coeffs(dec, &mut coding, scan, skip_ctx, dc_sign_ctx(around.2))?;
    // A 64x64 luma block's transform covers the whole 64x64 area, but only its
    // top-left 32x32 of frequencies are coded (spec 5.11.40); the rest of the
    // dequantized grid stays zero, which `inverse_transform_2d`'s own `< 32`
    // guard also assumes.
    let levels = if tx_side == side {
        grid
    } else {
        let mut full = vec![0i32; tx_side * tx_side];
        for row in 0..side {
            full[row * tx_side..][..side].copy_from_slice(&grid[row * side..][..side]);
        }
        full
    };
    let residual = dequant_and_inverse(&levels, tx_side, 8, i32::from(base_q_idx));
    plane.reconstruct(x, y, tx_side, predict_mode, reach, &residual);
    Ok(if tx_side == side {
        residual_grid_placeholder(&levels, tx_side)
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
) -> Result<()> {
    let (r, c) = at;
    let (px, py) = (c * SUB, r * SUB);
    let (skip, mode) = read_intra_mode(
        dec,
        cdfs,
        neighbours.above_mode[c],
        neighbours.left_mode[r],
        cfl,
    )?;
    let reach = Reach::of(side, px, py, y.width, y.height);
    let (luma_grid, u_grid, v_grid);
    let (cpx, cpy) = (px / 2, py / 2);
    let chroma_side = side / 2;
    if skip {
        // A skipped block codes no residual syntax at all (spec 5.11.34):
        // straight prediction, on every plane.
        y.reconstruct(px, py, side, mode, reach, &vec![0i32; side * side]);
        u.reconstruct(
            cpx,
            cpy,
            chroma_side,
            DC_PRED,
            Reach::none(),
            &vec![0i32; chroma_side * chroma_side],
        );
        v.reconstruct(
            cpx,
            cpy,
            chroma_side,
            DC_PRED,
            Reach::none(),
            &vec![0i32; chroma_side * chroma_side],
        );
        luma_grid = vec![0i32; side * side];
        u_grid = vec![0i32; chroma_side * chroma_side];
        v_grid = vec![0i32; chroma_side * chroma_side];
    } else {
        let around = neighbours.around(at, side);
        luma_grid = read_plane(
            dec, cdfs, luma_set, scans.0, 0, around[0], mode, mode, reach, y, px, py, side,
            luma_tx, base_q_idx,
        )?;
        u_grid = read_plane(
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
        )?;
        v_grid = read_plane(
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
        )?;
    }
    neighbours.record(at, side, mode, &[luma_grid, u_grid, v_grid]);
    Ok(())
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
) -> Result<Picture> {
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

    let mut cdfs = Cdfs::new(q_ctx_of(base_q_idx));
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
                dec.symbol(&mut cdfs.partition_w64[ctx])
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
                            dec.symbol(&mut cdfs.partition_w32[ctx32])
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
                                    if !has_cols16 || !has_rows16 {
                                        return Err(unsupported(
                                            "a 16x16 block the true frame edge cuts through (round 2)",
                                        ));
                                    }
                                    let at16 = (sr, sc);
                                    let ctx16 = neighbours.partition_ctx(at16, SUB);
                                    let part16 = dec.symbol(&mut cdfs.partition_w16[ctx16]);
                                    if part16 != PARTITION_NONE {
                                        return Err(unsupported(
                                            "a partition below 16x16 (this encoder never writes one)",
                                        ));
                                    }
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
                                    )?;
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

    let (fw, fh) = (frame_width as usize, frame_height as usize);
    if fw == width && fh == height {
        return Ok(Picture {
            width,
            height,
            y: y.data,
            u: u.data,
            v: v.data,
        });
    }
    let crop = |plane: &PlaneBuf, w: usize, h: usize| -> Vec<u8> {
        let mut out = Vec::with_capacity(w * h);
        for row in 0..h {
            out.extend_from_slice(&plane.data[row * plane.width..][..w]);
        }
        out
    };
    Ok(Picture {
        width: fw,
        height: fh,
        y: crop(&y, fw, fh),
        u: crop(&u, fw / 2, fh / 2),
        v: crop(&v, fw / 2, fh / 2),
    })
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
        assert!(decode_key_frame_tile(&[0u8; 4], 0, 32, 32, 0, 128).is_err());
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
        let decoded =
            decode_key_frame_tile(&tile, mi_cols, mi_rows, base_q_idx, w as u32, h as u32).unwrap();
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
        let decoded = decode_key_frame_tile(&data, 16, 16, 40, 64, 64).unwrap();
        assert!(
            decoded.y.iter().all(|&s| s == 128),
            "a skipped DC_PRED block with no neighbours predicts flat mid-grey"
        );
    }
}
