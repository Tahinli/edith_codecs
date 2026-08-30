//! Tile payload writer (spec 5.11), for the one block coding the encoder can
//! produce today: a key frame whose every superblock is a single 64x64
//! DC-predicted block with no residual.
//!
//! That frame decodes to a flat mid-grey picture — every sample is the value a
//! DC prediction with no neighbours produces — which is what makes it a usable
//! gate: any desync between this writer and a real decoder shows up as a
//! decode failure or a sample that is not mid-grey, with no metric in the way.
//! It is the skeleton the block modes, transform sizes and coefficients hang
//! off as they arrive.

use std::sync::LazyLock;

use ec_core::{Error, Result};

use crate::cdf;
use crate::cdf_state::{Cdfs, MvComponentCdfs, TxbSet, TxbTables};
use crate::msac::SymbolEncoder;
use crate::mvstack::{MiGrid, MiInfo, find_mv_stack, single_ref_ctx};

/// round-4 av1-truesize debugging aid: prints `msg()` to stderr when the
/// `EC_RNG` environment variable is set, mirroring the `EC_PART`/`EC_TOK`
/// trace `/tmp/libaom-src`'s debug `aomdec` build already emits under the
/// same variable, so the two traces line up symbol for symbol. Checked once
/// per process, so unset (the default) costs one atomic load per call and no
/// allocation. Only the real tile writer below calls this -- the mode/rate
/// search's own trial encoders never do -- so the trace does not need the
/// throwaway-encoder gating a whole-encoder trace would.
fn ec_rng_trace(msg: impl FnOnce() -> String) {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2); // 2 = unknown, 1 = on, 0 = off
    let on = match ON.load(Ordering::Relaxed) {
        2 => {
            let on = u8::from(std::env::var_os("EC_RNG").is_some());
            ON.store(on, Ordering::Relaxed);
            on
        }
        v => v,
    };
    if on == 1 {
        eprintln!("{}", msg());
    }
}

/// `PARTITION_NONE` (spec 6.10.4): the whole block, undivided.
const PARTITION_NONE: usize = 0;
/// `PARTITION_SPLIT` (spec 6.10.4): the block cut into four quadrants.
const PARTITION_SPLIT: usize = 3;

/// The partition types whose probability mass the split-or-horizontal flag of
/// a superblock hanging off the bottom of the frame gathers (spec 9.3,
/// `partition_gather_vert_alike`): the flag says split, so everything vertical
/// or split lands on it.
const VERT_ALIKE: [usize; 6] = [2, PARTITION_SPLIT, 4, 6, 7, 9];

/// The same for a superblock hanging off the right-hand edge
/// (`partition_gather_horz_alike`).
const HORZ_ALIKE: [usize; 6] = [1, PARTITION_SPLIT, 4, 5, 6, 8];

/// Mode-info units across a 32x32 block.
pub(crate) const BLOCK_MI: u32 = SB_MI / 2;

/// Mode-info units across a 16x16 block, the smallest this crate's key-frame
/// writer codes.
pub(crate) const SUB_MI: u32 = BLOCK_MI / 2;

/// spec `decode_partition`'s `hasRows`/`hasCols` (5.11.4), recomputed at
/// whichever block size is asking: a block of `side_mi` mode-info units at mi
/// position `pos` may be coded unsplit (its own `PARTITION_NONE`) only when
/// this is true in both directions -- the frame's *true* size can put the
/// boundary inside a superblock, a 32x32 quadrant or (since this writer's
/// smallest block is 16x16) even a leaf, and each level must ask again with
/// its own half, not just once at the superblock.
pub(crate) fn has_half(pos: u32, side_mi: u32, bound: u32) -> bool {
    pos + side_mi / 2 < bound
}

/// The number of 32x32 block columns/rows a frame whose *true* (unpadded)
/// size gives `mi_cols`/`mi_rows` is coded over: every block whose mi origin
/// (`col * BLOCK_MI`) is inside the true bound is coded, so this is a
/// ceiling, not the exact division a block-aligned frame would give. Shared
/// by the tile writer's own iteration and by [`crate::encode`]'s block/
/// superblock generation, which must agree with it exactly.
pub(crate) fn block_grid(mi_cols: u32, mi_rows: u32) -> (u32, u32) {
    (mi_cols.div_ceil(BLOCK_MI), mi_rows.div_ceil(BLOCK_MI))
}
/// `DC_PRED` (spec 6.10.2), as both the luma and the chroma mode.
const DC_PRED: usize = 0;

/// `V_PRED` (spec 6.10.2), the first of the eight directional intra modes and
/// so the first mode that carries an angle delta.
const V_PRED: usize = 1;

/// `H_PRED`, which predicts every row from the column to the block's left.
#[cfg(test)]
const H_PRED: usize = 2;

/// The last directional mode, `D67_PRED`.
const D67_PRED: usize = 8;

/// The number of intra modes a key frame's luma block chooses from,
/// `INTRA_MODES` (spec 3).
const INTRA_MODES: usize = 13;

/// `Intra_Mode_Context` (spec 9.3): the five-way class each intra mode puts
/// its neighbours in when they pick the CDF for the next block's mode.
pub(crate) const INTRA_MODE_CTX: [usize; INTRA_MODES] = [0, 1, 2, 3, 4, 4, 4, 4, 3, 0, 1, 2, 0];

/// The symbol an angle delta of zero codes as: the alphabet runs from -3 to
/// +3, so `MAX_ANGLE_DELTA` is the middle of it.
const ANGLE_DELTA_ZERO: usize = 3;
/// Side of a superblock in 4x4 mode-info units when 128x128 superblocks are off.
const SB_MI: u32 = 16;

/// `NUM_BASE_LEVELS` (spec 3): levels above this carry a base-range tail.
const NUM_BASE_LEVELS: i32 = 2;
/// Where DCT_DCT sits in the spec's `Tx_Type_Intra_Inv_Set2`, the set a 16x16
/// intra luma transform picks its type from.
const TX_TYPE_DCT_DCT_SET2: usize = 1;
/// `COEFF_BASE_RANGE` (spec 3): how far the base-range tail reaches before the
/// Golomb tail takes over.
const COEFF_BASE_RANGE: i32 = 12;
/// `BR_CDF_SIZE - 1` (spec 3): the largest increment one base-range symbol
/// carries.
const BR_STEP: i32 = 3;
/// The largest level the base and base-range syntax carry between them.
/// Anything above it is written as a Golomb tail on top of this.
const MAX_BR_LEVEL: i32 = NUM_BASE_LEVELS + COEFF_BASE_RANGE;
/// The largest level this writer codes. The Golomb tail is a length in unary
/// followed by that many bits, and the spec's decoder reads at most twenty of
/// each, so a level a decoder cannot read back is refused rather than written.
const MAX_LEVEL: i32 = MAX_BR_LEVEL + (1 << 19);

/// The coefficient q-context (spec 8.3.2's `Get_Qctx`, `Default_..._Cdf`'s
/// leading index) a frame's `base_q_idx` picks its default CDFs from.
/// [`crate::cdf`] carries all four, one constant set per context.
fn q_ctx_of(base_q_idx: u8) -> usize {
    match base_q_idx {
        0..=20 => 0,
        21..=60 => 1,
        61..=120 => 2,
        _ => 3,
    }
}

/// Both writers here code whole superblocks only: a partial one forces the
/// partition syntax down the block tree, which they do not code yet.
/// The probability the CDF gives one symbol on its own.
fn element_prob(cdf: &[u16], element: usize) -> u16 {
    cdf[element] - if element > 0 { cdf[element - 1] } else { 0 }
}

/// The two-symbol CDF a partial superblock's partition flag is coded with: the
/// mass of the listed partition types becomes the probability of a split, and
/// the rest is the one partition the frame edge still allows.
fn gather(cdf: &[u16], elements: [usize; 6]) -> [u16; 3] {
    let split: u16 = elements.iter().map(|&e| element_prob(cdf, e)).sum();
    [32768 - split, 32768, 0]
}

fn check_superblocks(mi_cols: u32, mi_rows: u32) -> Result<()> {
    if mi_cols == 0
        || mi_rows == 0
        || !mi_cols.is_multiple_of(SB_MI)
        || !mi_rows.is_multiple_of(SB_MI)
    {
        return Err(Error::unsupported(
            "AV1 tile",
            "a key frame is written only for frames that are a whole number \
             of 64x64 superblocks",
        ));
    }
    Ok(())
}

/// Writes the payload of a one-tile key frame in which every superblock is a
/// skipped 64x64 DC-predicted block.
///
/// `mi_cols` and `mi_rows` are the frame's dimensions in 4x4 mode-info units,
/// as the frame header carries them.
///
/// # Errors
/// Returns an error when the frame is not a whole number of 64x64 superblocks:
/// a partial superblock forces the partition syntax down the block tree, which
/// this writer does not code yet.
pub fn flat_key_frame_tile(mi_cols: u32, mi_rows: u32) -> Result<Vec<u8>> {
    check_superblocks(mi_cols, mi_rows)?;
    let (sb_cols, sb_rows) = (mi_cols / SB_MI, mi_rows / SB_MI);

    let mut enc = SymbolEncoder::new();
    for r in 0..sb_rows {
        for c in 0..sb_cols {
            // decode_partition (spec 5.11.4). Every neighbour is a 64x64 block,
            // whose stored partition context has a zero bit at this block
            // size, so the context is 0 wherever the block sits.
            enc.symbol_fixed(PARTITION_NONE, &cdf::PARTITION_W64[0]);

            // intra_frame_mode_info (spec 5.11.16). Segmentation, delta q,
            // delta lf, palette, filter intra and intrabc are all off in the
            // frame header, and a skipped block codes no CDEF index, so the
            // block is three symbols: the skip flag and the two modes.
            let skip_ctx = usize::from(r > 0) + usize::from(c > 0);
            enc.symbol_fixed(1, &cdf::SKIP[skip_ctx]);
            // Both neighbours are DC-predicted, and an unavailable neighbour
            // counts as DC too, so both mode contexts are 0.
            enc.symbol_fixed(DC_PRED, &cdf::KF_Y_MODE[0][0]);
            // Chroma from luma is only offered up to 32x32, so the CFL-free
            // table is the one a 64x64 block reads.
            enc.symbol_fixed(DC_PRED, &cdf::UV_MODE_NO_CFL[DC_PRED]);

            // read_block_tx_size codes nothing while the frame's tx_mode is
            // TX_MODE_LARGEST, and a skipped block has no residual, so the
            // block ends here.
        }
    }
    Ok(enc.finish())
}

/// Writes the payload of a one-tile key frame in which every superblock is a
/// 64x64 DC-predicted block carrying one luma DC coefficient of `dc_level` and
/// no chroma residual.
///
/// `dc_level` is a quantised level, not a sample value: the decoder multiplies
/// it by the frame's DC quantiser and inverse-transforms it over the whole
/// block, so the picture it makes is a flat grey some distance either side of
/// the mid-grey a zero level gives. `base_q_idx` is the frame header's, and
/// picks the coefficient CDFs.
///
/// # Errors
/// As [`dc_key_frame_tile_levels`], which this is the one-level case of.
pub fn dc_key_frame_tile(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    dc_level: i32,
) -> Result<Vec<u8>> {
    check_superblocks(mi_cols, mi_rows)?;
    let blocks = ((mi_cols / SB_MI) * (mi_rows / SB_MI)) as usize;
    dc_key_frame_tile_levels(mi_cols, mi_rows, base_q_idx, &vec![dc_level; blocks])
}

/// Writes the payload of a one-tile key frame carrying one luma DC coefficient
/// per superblock, `levels` giving them in the raster order the superblocks are
/// coded in.
///
/// Each superblock decodes to a flat block of its own grey, so a frame written
/// here is a grid of greys the caller chooses — which is what makes the sign
/// context observable: a block's sign context is read off its coded neighbours,
/// and a frame whose levels differ in sign exercises the three-way split that a
/// single-sign frame cannot reach.
///
/// # Errors
/// Returns an error when the frame is not a whole number of 64x64 superblocks,
/// when `levels` does not carry exactly one level per superblock, when a level
/// is outside the range the base and base-range syntax carry (`-14..=14`
/// without zero).
pub fn dc_key_frame_tile_levels(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    levels: &[i32],
) -> Result<Vec<u8>> {
    check_superblocks(mi_cols, mi_rows)?;
    let (sb_cols, sb_rows) = (mi_cols / SB_MI, mi_rows / SB_MI);
    check_levels(levels, (sb_cols * sb_rows) as usize)?;
    let q_ctx = q_ctx_of(base_q_idx);
    let txb_skip_luma_64 = crate::cdf_state::pick(
        q_ctx,
        cdf::TXB_SKIP_LUMA_64_Q0,
        cdf::TXB_SKIP_LUMA_64_Q1,
        cdf::TXB_SKIP_LUMA_64,
        cdf::TXB_SKIP_LUMA_64_Q3,
    );
    let base_eob_luma_64_dc = crate::cdf_state::pick(
        q_ctx,
        cdf::COEFF_BASE_EOB_LUMA_64_Q0[0],
        cdf::COEFF_BASE_EOB_LUMA_64_Q1[0],
        cdf::COEFF_BASE_EOB_LUMA_64[0],
        cdf::COEFF_BASE_EOB_LUMA_64_Q3[0],
    );
    let txb_skip_chroma_32_none = crate::cdf_state::pick(
        q_ctx,
        cdf::TXB_SKIP_CHROMA_32_Q0[0],
        cdf::TXB_SKIP_CHROMA_32_Q1[0],
        cdf::TXB_SKIP_CHROMA_32[0],
        cdf::TXB_SKIP_CHROMA_32_Q3[0],
    );

    // The sign of the DC each coded block left behind, for the two neighbours
    // the sign context is read from: one row of them above, and the block to
    // the left, which is dropped at the start of every superblock row the way a
    // decoder clears its left context there.
    let mut above: Vec<Option<bool>> = vec![None; sb_cols as usize];
    let mut enc = SymbolEncoder::new();
    for r in 0..sb_rows {
        let mut left: Option<bool> = None;
        for c in 0..sb_cols {
            let dc_level = levels[(r * sb_cols + c) as usize];
            let negative = dc_level < 0;

            enc.symbol_fixed(PARTITION_NONE, &cdf::PARTITION_W64[0]);

            // Nothing is skipped now, so every neighbour's skip flag is 0 and
            // the skip context stays 0 across the frame.
            enc.symbol_fixed(0, &cdf::SKIP[0]);
            enc.symbol_fixed(DC_PRED, &cdf::KF_Y_MODE[0][0]);
            enc.symbol_fixed(DC_PRED, &cdf::UV_MODE_NO_CFL[DC_PRED]);

            write_dc_coeffs(
                &mut enc,
                dc_level,
                dc_sign_ctx(dc_vote(above[c as usize]) + dc_vote(left)),
                q_ctx,
                &txb_skip_luma_64,
                &base_eob_luma_64_dc,
            );

            // Both chroma transform blocks are all-zero. Their planes carry no
            // coded coefficient anywhere in the frame, so the neighbour halves
            // of their context stay 0 and only the offset for a transform block
            // that covers its whole plane block is left: context 7.
            enc.symbol_fixed(1, &txb_skip_chroma_32_none);
            enc.symbol_fixed(1, &txb_skip_chroma_32_none);

            above[c as usize] = Some(negative);
            left = Some(negative);
        }
    }
    Ok(enc.finish())
}

/// Writes the payload of a one-tile key frame in which every superblock is
/// split into four 32x32 DC-predicted blocks, each carrying one luma DC
/// coefficient and no chroma residual.
///
/// `levels` gives one level per 32x32 block in raster order across the frame,
/// so the grid is twice as wide and twice as tall as the superblock grid. The
/// blocks are coded in the z-order a superblock's split walks, but every
/// block's neighbours above and to its left are coded before it either way, so
/// the picture reads in raster order.
///
/// # Errors
/// As [`dc_key_frame_tile_levels`], with `levels` sized for the 32x32 grid.
pub fn split_dc_key_frame_tile(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    levels: &[i32],
) -> Result<Vec<u8>> {
    check_superblocks(mi_cols, mi_rows)?;
    let (sb_cols, sb_rows) = (mi_cols / SB_MI, mi_rows / SB_MI);
    let (cols, rows) = (sb_cols * 2, sb_rows * 2);
    check_levels(levels, (cols * rows) as usize)?;
    let q_ctx = q_ctx_of(base_q_idx);
    let txb_skip_luma_32 = crate::cdf_state::pick(
        q_ctx,
        cdf::TXB_SKIP_LUMA_32_Q0,
        cdf::TXB_SKIP_LUMA_32_Q1,
        cdf::TXB_SKIP_LUMA_32,
        cdf::TXB_SKIP_LUMA_32_Q3,
    );
    let base_eob_luma_32 = crate::cdf_state::pick(
        q_ctx,
        cdf::COEFF_BASE_EOB_LUMA_32_Q0[0],
        cdf::COEFF_BASE_EOB_LUMA_32_Q1[0],
        cdf::COEFF_BASE_EOB_LUMA_32[0],
        cdf::COEFF_BASE_EOB_LUMA_32_Q3[0],
    );
    let txb_skip_chroma_16_0 = crate::cdf_state::pick(
        q_ctx,
        cdf::TXB_SKIP_CHROMA_16_Q0[0],
        cdf::TXB_SKIP_CHROMA_16_Q1[0],
        cdf::TXB_SKIP_CHROMA_16[0],
        cdf::TXB_SKIP_CHROMA_16_Q3[0],
    );

    let mut above: Vec<Option<bool>> = vec![None; cols as usize];
    let mut left: Vec<Option<bool>> = vec![None; rows as usize];

    let mut enc = SymbolEncoder::new();
    for sb_r in 0..sb_rows {
        // A decoder clears its left context at the start of every superblock
        // row, and so does the partition context the 64x64 symbol reads.
        left.iter_mut().for_each(|l| *l = None);
        for sb_c in 0..sb_cols {
            // The partition context of a 64x64 block reads the bit its
            // neighbours' block size sets at this depth: a 32x32 neighbour sets
            // it, and an uncoded one leaves it clear, so the context is just
            // which neighbours exist. The 32x32 blocks below read a bit their
            // own size leaves clear, so their context is 0 throughout.
            // Every superblock here is split, so an existing neighbour always
            // sets that bit.
            let ctx = 2 * usize::from(sb_c > 0) + usize::from(sb_r > 0);
            enc.symbol_fixed(PARTITION_SPLIT, &cdf::PARTITION_W64[ctx]);

            for quadrant in 0..4 {
                let (r, c) = (sb_r * 2 + quadrant / 2, sb_c * 2 + quadrant % 2);
                let dc_level = levels[(r * cols + c) as usize];
                let negative = dc_level < 0;

                enc.symbol_fixed(PARTITION_NONE, &cdf::PARTITION_W32[0]);
                enc.symbol_fixed(0, &cdf::SKIP[0]);
                enc.symbol_fixed(DC_PRED, &cdf::KF_Y_MODE[0][0]);
                // Chroma from luma is offered up to 32x32, so the block reads
                // the wider table even though it does not take the mode.
                enc.symbol_fixed(DC_PRED, &cdf::UV_MODE_CFL[DC_PRED]);

                write_dc_coeffs(
                    &mut enc,
                    dc_level,
                    dc_sign_ctx(dc_vote(above[c as usize]) + dc_vote(left[r as usize])),
                    q_ctx,
                    &txb_skip_luma_32,
                    &base_eob_luma_32,
                );
                enc.symbol_fixed(1, &txb_skip_chroma_16_0);
                enc.symbol_fixed(1, &txb_skip_chroma_16_0);

                above[c as usize] = Some(negative);
                left[r as usize] = Some(negative);
            }
        }
    }
    Ok(enc.finish())
}

/// One quantised coefficient of a transform block.
///
/// `row` and `col` are its position in the transform it belongs to — 32x32 for
/// luma, 16x16 for each chroma plane — not in the picture: a coefficient at
/// row 0 varies along the picture's width and one at column 0 along its
/// height, and the coefficient at the origin is the DC the block's average
/// sample value rides on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coeff {
    /// The coefficient's row in the transform.
    pub row: u8,
    /// The coefficient's column in the transform.
    pub col: u8,
    /// Its quantised level, which the base and base-range syntax carry for
    /// magnitudes up to [`MAX_LEVEL`].
    pub level: i32,
}

/// The coefficients of one coded block: a 32x32 luma transform and, at 4:2:0,
/// the 16x16 transform each chroma plane covers the same area with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockCoeffs {
    /// The luma transform's coefficients.
    pub luma: Vec<Coeff>,
    /// The U plane's.
    pub u: Vec<Coeff>,
    /// The V plane's.
    pub v: Vec<Coeff>,
    /// The luma intra mode the block is predicted with, one of the thirteen
    /// modes a key frame codes (`DC_PRED` is zero, which is what
    /// [`Default`] and a plain coefficient list give). Chroma stays on
    /// `DC_PRED`.
    pub mode: u8,
    /// Whether the block carries no residual at all (spec `skip`). An intra
    /// block may still be skipped; `false` (the [`Default`]) codes whatever
    /// `luma`/`u`/`v` carry.
    pub skip: bool,
    /// The block's inter mode and motion vector, or `None` for an intra
    /// block. Only [`sb_coeff_inter_frame_tile`] codes `is_inter`; the key
    /// frame writers never read this field.
    pub inter: Option<InterInfo>,
    /// The 8x8 leaves a straddling 16x16 block is split into (lane-av1-rect),
    /// each with its own 8x8 luma transform and 4x4 chroma transforms, in
    /// raster order among the leaves that are inside the true frame. `Some`
    /// overrides this `BlockCoeffs`'s own `luma`/`u`/`v`/`mode`, which are left
    /// at their defaults and unused, the same way a `Whole` [`Superblock`]'s
    /// per-quadrant fields are unused.
    pub eight: Option<Vec<BlockCoeffs>>,
}

/// One inter mode [`sb_coeff_inter_frame_tile`]'s blocks may take (spec
/// 5.11.24 `read_inter_mode`), reduced to the two branches its symbol chain
/// needs to prove out: the stack's own top candidate outright, or a coded
/// residual against it. `NEARMV`/`GLOBALMV` are never written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterMode {
    /// `NEARESTMV`: takes `MvStack::nearest_mv` outright, no residual, no DRL.
    NearestMv,
    /// `NEWMV`: codes `mv` as a residual against `MvStack::pred_mv`, through
    /// a DRL index this writer always leaves at zero.
    NewMv,
}

/// An inter-coded block's mode and motion vector, `mv` in the spec's 1/8-pel
/// `(row, col)` units. For [`InterMode::NearestMv`] this writer ignores `mv`
/// and codes the stack's own candidate instead — the decoder derives it, so
/// nothing here can disagree with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterInfo {
    /// The mode this block is coded with.
    pub mode: InterMode,
    /// The motion vector a [`InterMode::NewMv`] block's residual targets.
    pub mv: (i32, i32),
}

impl From<Vec<Coeff>> for BlockCoeffs {
    /// A block that codes luma only.
    fn from(luma: Vec<Coeff>) -> Self {
        Self {
            luma,
            ..Self::default()
        }
    }
}

/// Side of the luma transform the coefficient writer codes.
const TX32: usize = 32;
/// Side of the larger of the two blocks a superblock splits into, in samples.
const BLOCK: usize = 32;
/// Side of the chroma transform beside it at 4:2:0, and of the luma transform
/// of a 16x16 block.
const TX16: usize = 16;
/// Side of the chroma transform of a 16x16 block at 4:2:0, and of the luma
/// transform of an 8x8 leaf under a straddling 16x16 (lane-av1-rect).
const TX8: usize = 8;
/// Side of the chroma transform of an 8x8 leaf at 4:2:0 (lane-av1-rect).
const TX4: usize = 4;
/// Side of a superblock, in samples, which is the size a block outside the
/// tile reads as.
const SB: usize = 64;
/// Side of the smallest block the writer codes, in samples, which is the grid
/// the neighbour bookkeeping is kept on.
const SUB: usize = 16;
/// Side of a 4x4 mode-info unit, in samples: the granularity libaom's above
/// and left entropy-context arrays are actually kept on (spec
/// `get_txb_ctx`/`av1_set_entropy_contexts`), finer than [`SUB`].
const MI: usize = 4;

/// What one coded block leaves behind for the blocks that read it as a
/// neighbour: whether it coded anything at all, and the sign of its DC.
#[derive(Clone, Copy, Default)]
struct Neighbour {
    /// Whether the plane's transform block carried a coefficient.
    coded: bool,
    /// The sign of its DC, absent when the DC itself is zero.
    dc: Option<bool>,
}

/// Rejects a frame with no mode-info grid at all.
///
/// `mi_cols`/`mi_rows` are the frame's *true* (unpadded) size in 4x4 units
/// (spec `compute_image_size`), not necessarily a multiple of the 32x32 block
/// grid: a block whose origin sits at or past this bound is not coded (spec's
/// `decode_partition` never visits it), and one that straddles the bound is
/// coded whole, its samples coming from the padded planes.
fn check_blocks(mi_cols: u32, mi_rows: u32) -> Result<()> {
    if mi_cols == 0 || mi_rows == 0 {
        return Err(Error::unsupported(
            "AV1 tile",
            "a coefficient key frame needs a nonzero mode-info grid",
        ));
    }
    Ok(())
}

/// The coefficients of one superblock: either one 64x64 block covering it, or
/// the four 32x32 blocks it is split into.
///
/// A superblock at the right-hand or bottom edge of the frame may be half
/// outside it, and such a superblock cannot be left whole — the spec has no
/// partition that keeps a block outside the frame — so it must be
/// [`Split`](Superblock::Split), and carries only the quadrants that are
/// inside, in raster order among themselves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Superblock {
    /// One 64x64 block, whose luma transform is 64x64 — of which only the
    /// top-left 32x32 carries coefficients — and whose chroma transforms are
    /// 32x32 each at 4:2:0.
    Whole(BlockCoeffs),
    /// The 32x32 quadrants the superblock is split into, each either one
    /// block or four 16x16 blocks of its own.
    Split(Vec<Quadrant>),
}

/// One 32x32 quadrant of a split superblock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Quadrant {
    /// One 32x32 block, with a 32x32 luma transform and a 16x16 transform per
    /// chroma plane.
    Whole(BlockCoeffs),
    /// The four 16x16 blocks it is split into, in raster order, each with a
    /// 16x16 luma transform and an 8x8 transform per chroma plane.
    Split(Vec<BlockCoeffs>),
}

impl Quadrant {
    /// The blocks it carries, in the order they are coded.
    pub(crate) fn blocks(&self) -> &[BlockCoeffs] {
        match self {
            Quadrant::Whole(block) => std::slice::from_ref(block),
            Quadrant::Split(blocks) => blocks.as_slice(),
        }
    }
}

/// What the blocks above and to the left of the one being coded left behind,
/// kept on the 16x16 grid the smallest block sits on.
struct Neighbours {
    /// Per plane, whether the neighbour coded anything and the sign of its DC.
    above: Vec<[Neighbour; 3]>,
    /// The same, down the left edge.
    left: Vec<[Neighbour; 3]>,
    /// The neighbour's luma intra mode, which picks the CDF the next block's
    /// mode is coded with.
    above_mode: Vec<usize>,
    /// The same, down the left edge.
    left_mode: Vec<usize>,
    /// The side of the neighbour block, in samples, which is what the
    /// partition symbol's context reads.
    above_side: Vec<usize>,
    /// The same, down the left edge.
    left_side: Vec<usize>,
    /// The same as `above_side`/`left_side`, but kept at the finer mi (4x4)
    /// granularity [`Self::above`]/[`Self::left`] are, rather than [`SUB`]:
    /// two 8x8 leaves of one straddling 16x16 block (lane-av1-rect) share a
    /// single [`SUB`]-grid cell, so a coarse array cannot tell the second
    /// leaf's partition symbol that the first one was coded at 8x8 (finer
    /// than the 16x16 it sits in) -- exactly what its `above`/`left` context
    /// needs to read (spec 9.3's `AbovePartitionContext`/`LeftPartitionContext`,
    /// which libaom keeps per mi unit for this reason).
    above_side_mi: Vec<usize>,
    /// The same, down the left edge.
    left_side_mi: Vec<usize>,
    /// Whether the neighbour carried no residual, for [`sb_coeff_inter_frame_tile`]'s
    /// skip context. Unused (and left at its default) by the key frame writers.
    above_skip: Vec<bool>,
    /// The same, down the left edge.
    left_skip: Vec<bool>,
    /// Whether the neighbour was coded inter, for the `is_inter` context.
    /// Unused by the key frame writers.
    above_inter: Vec<bool>,
    /// The same, down the left edge.
    left_inter: Vec<bool>,
    /// The frame's true (unpadded) width and height, in 4x4 mode-info units,
    /// which is what clamps [`Self::above`]/[`Self::left`] at the edge (spec
    /// `av1_set_entropy_contexts`): a block whose row or column run spills
    /// past this bound leaves its trailing 4x4 units at their default,
    /// mid-cell if need be.
    mi_cols: usize,
    mi_rows: usize,
}

/// What one plane's neighbours leave for a block, gathered across every cell
/// the block spans -- which is what the decoder's own derivation reads
/// (spec 5.11.39), and what a single cell only stands in for while every block
/// is the same size.
#[derive(Clone, Copy, Default)]
struct Around {
    /// Whether any transform block above this one carried a coefficient.
    above_coded: bool,
    /// The same, to the left.
    left_coded: bool,
    /// The running vote of the neighbours' DC signs: negative DCs count down,
    /// positive ones up, and a neighbour with no DC does not count.
    dc_vote: i32,
}

impl Neighbours {
    /// The state a tile starts from: a block outside it reads as `DC_PRED`,
    /// as having coded nothing, and as unsplit. `cols`/`rows` are in [`SUB`]
    /// units; `mi_cols`/`mi_rows` are the frame's true (unpadded) size in 4x4
    /// mode-info units.
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
        }
    }

    /// Clears the left edge, which a tile starts every superblock row with.
    fn start_row(&mut self) {
        self.left.iter_mut().for_each(|l| *l = Default::default());
        self.left_mode.iter_mut().for_each(|m| *m = DC_PRED);
        self.left_side.iter_mut().for_each(|s| *s = SB);
        self.left_side_mi.iter_mut().for_each(|s| *s = SB);
        self.left_skip.iter_mut().for_each(|s| *s = false);
        self.left_inter.iter_mut().for_each(|i| *i = false);
    }

    /// Writes one coded block into every 16x16 column and row it covers, and,
    /// on the finer 4x4 grid libaom's entropy context arrays are actually
    /// kept on, into every unit up to the true frame edge -- the units past
    /// it are left at their default (uncoded), even mid-16x16-cell (spec
    /// `av1_set_entropy_contexts`, which clamps to `blocks_wide`/`blocks_high`
    /// derived from the true `mi_cols`/`mi_rows`, not from this block's own
    /// side).
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
    /// (r, c): an 8x8 leaf under a straddling 16x16 (lane-av1-rect) sits at a
    /// mi offset [`Self::record`]'s SUB-unit `at` cannot name, but the
    /// `above`/`left` arrays it writes into are already sized to the full mi
    /// grid, so no resizing is needed to write into them at this
    /// finer-than-SUB granularity.
    fn record_mi(&mut self, at_mi: (usize, usize), side: usize, grids: &[Vec<i32>; 3]) {
        let (mi_r, mi_c) = at_mi;
        let states: [Neighbour; 3] = std::array::from_fn(|plane| neighbour_state(&grids[plane]));
        let side_mi = side / MI;
        for cell in 0..side_mi {
            self.left_side_mi[mi_r + cell] = side;
            self.above_side_mi[mi_c + cell] = side;
        }
        // libaom rounds the luma edge up to the plane's own 4x4 unit before
        // clamping a subsampled plane (`ROUND_POWER_OF_TWO(max_blocks_high,
        // subsampling_y)` in av1_write_intra_coeffs_mb, encodetxb.c:456-459):
        // a chroma 4x4 unit straddling the true luma edge is still whole in
        // chroma's own halved grid, so it stays valid one luma-mi row/col
        // past where luma's own edge falls when that edge is odd.
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

    /// Writes one inter-frame block's skip flag and inter/intra state into
    /// every 16x16 column and row it covers, the same span [`Self::record`]
    /// fills for the coefficient and mode state.
    fn record_inter(&mut self, at: (usize, usize), side: usize, skip: bool, is_inter: bool) {
        let (r, c) = at;
        for cell in 0..side / SUB {
            self.above_skip[c + cell] = skip;
            self.left_skip[r + cell] = skip;
            self.above_inter[c + cell] = is_inter;
            self.left_inter[r + cell] = is_inter;
        }
    }

    /// The context of a block's partition symbol (spec 9.3): whether the
    /// blocks above it and to its left were split finer than it is.
    /// The gathered state of the blocks above and to the left of one block,
    /// per plane.
    fn around(&self, (r, c): (usize, usize), side: usize) -> [Around; 3] {
        self.around_mi((r * (SUB / MI), c * (SUB / MI)), side)
    }

    /// [`Self::around`] taking the block's position directly in 4x4 mode-info
    /// units, for the same reason [`Self::record_mi`] does.
    fn around_mi(&self, (mi_r, mi_c): (usize, usize), side: usize) -> [Around; 3] {
        let side_mi = side / MI;
        std::array::from_fn(|plane| {
            let mut around = Around::default();
            for cell in 0..side_mi {
                let (above, left) = (
                    &self.above[mi_c + cell][plane],
                    &self.left[mi_r + cell][plane],
                );
                around.above_coded |= above.coded;
                around.left_coded |= left.coded;
                around.dc_vote += dc_vote(above.dc) + dc_vote(left.dc);
            }
            around
        })
    }

    fn partition_ctx(&self, at: (usize, usize), side: usize) -> usize {
        // Delegates to the mi-precise reader (same pattern as `around` /
        // `around_mi`): `above_side`/`left_side` are only ever advanced in
        // whole-[`SUB`] steps by [`Self::record`], so a leaf8's `record_mi`
        // (lane-av1-rect) -- which only touches the finer mi arrays -- leaves
        // them stale for the *next* sibling's own partition symbol, reading a
        // 16x16-slot side that already split into 8x8s underneath it.
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
}

/// Writes the payload of a one-tile key frame built from `superblocks`, each
/// either one 64x64 block or the 32x32 blocks it splits into.
///
/// `superblocks` gives one entry per superblock in raster order across the
/// frame. Every block is DC-predicted unless its [`BlockCoeffs::mode`] says
/// otherwise, and carries the coefficients its lists give; the coefficients
/// may sit anywhere in their transform, so a block is a picture rather than a
/// flat grey.
///
/// # Errors
/// Returns an error when the frame is not a whole number of 32x32 blocks, when
/// `superblocks` does not carry exactly one entry per superblock or an entry
/// does not carry one block per quadrant inside the frame, when a superblock
/// that is half outside the frame is left whole, when a block names an intra
/// mode a key frame does not code, when a coefficient sits outside its
/// transform, repeats a position, carries a zero level or one wider than the
/// Golomb tail reaches.
pub fn sb_coeff_key_frame_tile(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    superblocks: &[Superblock],
) -> Result<Vec<u8>> {
    check_blocks(mi_cols, mi_rows)?;
    let (cols, rows) = block_grid(mi_cols, mi_rows);
    let (sb_cols, sb_rows) = (cols.div_ceil(2), rows.div_ceil(2));
    if superblocks.len() != (sb_cols * sb_rows) as usize {
        return Err(Error::unsupported(
            "AV1 tile",
            "a coefficient key frame needs one entry per superblock",
        ));
    }
    let mut coded: Vec<&BlockCoeffs> = Vec::new();
    for superblock in superblocks {
        match superblock {
            Superblock::Whole(block) => coded.push(block),
            Superblock::Split(quadrants) => {
                for block in quadrants.iter().flat_map(Quadrant::blocks) {
                    match &block.eight {
                        Some(leaves) => coded.extend(leaves.iter()),
                        None => coded.push(block),
                    }
                }
            }
        }
    }
    if let Some(bad) = coded
        .into_iter()
        .find(|b| usize::from(b.mode) >= INTRA_MODES)
    {
        return Err(Error::unsupported(
            "AV1 tile",
            format!(
                "intra mode {} is not one of the thirteen a key frame codes",
                bad.mode
            ),
        ));
    }

    let sub_planes = [TxbSet::Luma16, TxbSet::Chroma8, TxbSet::Chroma8];
    let split_planes = [TxbSet::Luma32, TxbSet::Chroma16, TxbSet::Chroma16];
    let whole_planes = [TxbSet::Luma64, TxbSet::Chroma32, TxbSet::Chroma32];
    let scans = [
        default_scan(TX32),
        default_scan(TX16),
        default_scan(TX8),
        default_scan(TX4),
    ];
    // The blocks above and to the left, on the 16x16 grid. The left edge is
    // reset at every superblock row because a tile starts each row with no
    // left neighbour.
    let mut neighbours = Neighbours::new(
        cols as usize * 2,
        rows as usize * 2,
        mi_cols as usize,
        mi_rows as usize,
    );

    // The tile adapts every non-literal CDF it writes, exactly as the decoder
    // adapts the ones it reads, so the frame header leaves `disable_cdf_update`
    // off.
    let mut cdfs = Cdfs::new(q_ctx_of(base_q_idx));
    let mut enc = SymbolEncoder::new();
    for sb_r in 0..sb_rows {
        neighbours.start_row();
        for sb_c in 0..sb_cols {
            let at = (sb_r as usize * 4, sb_c as usize * 4);
            let ctx = neighbours.partition_ctx(at, SB);
            ec_rng_trace(|| {
                format!(
                    "EC_PART mi_row={} mi_col={} bsize=12 ctx={} tell={}",
                    at.0 * 4,
                    at.1 * 4,
                    ctx,
                    enc.tell()
                )
            });
            // A superblock whose bottom or right half is outside the frame
            // cannot be left unsplit, so the decoder reads a flag instead of
            // the partition symbol — and reads nothing at all when both halves
            // are outside, where the split is the only partition left.
            let (has_cols, has_rows) = (
                sb_c * SB_MI + SB_MI / 2 < mi_cols,
                sb_r * SB_MI + SB_MI / 2 < mi_rows,
            );
            // The quadrants of this superblock that are inside the frame, as
            // positions in the 32x32 block grid.
            let quadrant_positions: Vec<(usize, usize)> = (0..4)
                .map(|q| (sb_r * 2 + q / 2, sb_c * 2 + q % 2))
                .filter(|&(r, c)| r < rows && c < cols)
                .map(|(r, c)| (r as usize, c as usize))
                .collect();

            match &superblocks[(sb_r * sb_cols + sb_c) as usize] {
                Superblock::Whole(block) => {
                    if !has_cols || !has_rows {
                        return Err(Error::unsupported(
                            "AV1 tile",
                            "a superblock that is half outside the frame cannot be left whole",
                        ));
                    }
                    enc.symbol(PARTITION_NONE, &mut cdfs.partition_w64[ctx]);
                    let grids = [
                        level_grid(&block.luma, TX32)?,
                        level_grid(&block.u, TX32)?,
                        level_grid(&block.v, TX32)?,
                    ];
                    write_block(
                        &mut enc,
                        &mut cdfs,
                        &mut neighbours,
                        block,
                        at,
                        SB,
                        &whole_planes,
                        &grids,
                        [&scans[0], &scans[0], &scans[0]],
                        // A 64x64 block is too big to be offered chroma from
                        // luma, so its chroma mode reads the table without it.
                        false,
                    );
                    ec_rng_trace(|| {
                        format!(
                            "EC_TOK mi_row={} mi_col={} tell={}",
                            at.0 * 4,
                            at.1 * 4,
                            enc.tell()
                        )
                    });
                }
                Superblock::Split(quadrants) => {
                    match (has_cols, has_rows) {
                        (true, true) => enc.symbol(PARTITION_SPLIT, &mut cdfs.partition_w64[ctx]),
                        // The gathered CDF an edge superblock reads is built
                        // for the read and thrown away: the decoder never
                        // stores it back, so nothing adapts here.
                        (true, false) => {
                            enc.symbol_fixed(1, &gather(&cdfs.partition_w64[ctx], VERT_ALIKE));
                        }
                        (false, true) => {
                            enc.symbol_fixed(1, &gather(&cdfs.partition_w64[ctx], HORZ_ALIKE));
                        }
                        (false, false) => {}
                    }
                    if quadrants.len() != quadrant_positions.len() {
                        return Err(Error::unsupported(
                            "AV1 tile",
                            "a split superblock needs one block per quadrant inside the frame",
                        ));
                    }
                    for (quadrant, (r, c)) in quadrants.iter().zip(quadrant_positions) {
                        let at = (r * 2, c * 2);
                        let ctx = neighbours.partition_ctx(at, BLOCK);
                        // Recomputed at this 32x32 block's own half (spec
                        // `decode_partition`, called again at every size, not
                        // just once for the superblock): the true frame edge
                        // can fall inside this quadrant even when the
                        // superblock it sits in was itself whole or safely
                        // split above.
                        let (has_cols32, has_rows32) = (
                            has_half(c as u32 * BLOCK_MI, BLOCK_MI, mi_cols),
                            has_half(r as u32 * BLOCK_MI, BLOCK_MI, mi_rows),
                        );
                        ec_rng_trace(|| {
                            format!(
                                "EC_PART mi_row={} mi_col={} bsize=9 ctx={} tell={}",
                                at.0 * 4,
                                at.1 * 4,
                                ctx,
                                enc.tell()
                            )
                        });
                        match quadrant {
                            Quadrant::Whole(block) => {
                                if !has_cols32 || !has_rows32 {
                                    return Err(Error::unsupported(
                                        "AV1 tile",
                                        "a 32x32 block that is half outside the true frame \
                                         cannot be left whole",
                                    ));
                                }
                                enc.symbol(PARTITION_NONE, &mut cdfs.partition_w32[ctx]);
                                let grids = [
                                    level_grid(&block.luma, TX32)?,
                                    level_grid(&block.u, TX16)?,
                                    level_grid(&block.v, TX16)?,
                                ];
                                write_block(
                                    &mut enc,
                                    &mut cdfs,
                                    &mut neighbours,
                                    block,
                                    at,
                                    BLOCK,
                                    &split_planes,
                                    &grids,
                                    [&scans[0], &scans[1], &scans[1]],
                                    true,
                                );
                                ec_rng_trace(|| {
                                    format!(
                                        "EC_TOK mi_row={} mi_col={} tell={}",
                                        at.0 * 4,
                                        at.1 * 4,
                                        enc.tell()
                                    )
                                });
                            }
                            Quadrant::Split(blocks) => {
                                // The 16x16 sub-blocks this quadrant's split
                                // carries: only those whose own mi origin is
                                // inside the true frame (spec `decode_partition`'s
                                // `r >= MiRows || c >= MiCols` early return),
                                // which need not be all four when the true
                                // edge falls inside this quadrant.
                                let sub_positions: Vec<(usize, usize)> = (0..4)
                                    .map(|i| (r * 2 + i / 2, c * 2 + i % 2))
                                    .filter(|&(sr, sc)| {
                                        (sr as u32) * SUB_MI < mi_rows
                                            && (sc as u32) * SUB_MI < mi_cols
                                    })
                                    .collect();
                                if blocks.len() != sub_positions.len() {
                                    return Err(Error::unsupported(
                                        "AV1 tile",
                                        "a split 32x32 block needs one 16x16 entry per \
                                         sub-block inside the true frame",
                                    ));
                                }
                                // Same three-way spec signaling as the
                                // superblock level above, recomputed at this
                                // block's own half: a full alphabet symbol
                                // only when both halves are inside, a single
                                // gathered bit when just one is, and nothing
                                // at all (SPLIT is inferred) when neither is.
                                match (has_cols32, has_rows32) {
                                    (true, true) => {
                                        enc.symbol(PARTITION_SPLIT, &mut cdfs.partition_w32[ctx]);
                                    }
                                    (true, false) => {
                                        enc.symbol_fixed(
                                            1,
                                            &gather(&cdfs.partition_w32[ctx], VERT_ALIKE),
                                        );
                                    }
                                    (false, true) => {
                                        enc.symbol_fixed(
                                            1,
                                            &gather(&cdfs.partition_w32[ctx], HORZ_ALIKE),
                                        );
                                    }
                                    (false, false) => {}
                                }
                                for (block, (sr, sc)) in blocks.iter().zip(sub_positions) {
                                    // A 16x16 leaf's own hasRows/hasCols,
                                    // recomputed at this leaf's own half
                                    // (same three-way signaling as the 32x32
                                    // and 64x64 levels above).
                                    let (has_cols16, has_rows16) = (
                                        has_half(sc as u32 * SUB_MI, SUB_MI, mi_cols),
                                        has_half(sr as u32 * SUB_MI, SUB_MI, mi_rows),
                                    );
                                    let at = (sr, sc);
                                    let ctx = neighbours.partition_ctx(at, SUB);
                                    ec_rng_trace(|| {
                                        format!(
                                            "EC_PART mi_row={} mi_col={} bsize=6 ctx={} tell={}",
                                            at.0 * 4,
                                            at.1 * 4,
                                            ctx,
                                            enc.tell()
                                        )
                                    });
                                    if has_cols16 && has_rows16 {
                                        enc.symbol(PARTITION_NONE, &mut cdfs.partition_w16[ctx]);
                                        let grids = [
                                            level_grid(&block.luma, TX16)?,
                                            level_grid(&block.u, TX8)?,
                                            level_grid(&block.v, TX8)?,
                                        ];
                                        write_block(
                                            &mut enc,
                                            &mut cdfs,
                                            &mut neighbours,
                                            block,
                                            at,
                                            SUB,
                                            &sub_planes,
                                            &grids,
                                            [&scans[1], &scans[2], &scans[2]],
                                            true,
                                        );
                                        ec_rng_trace(|| {
                                            format!(
                                                "EC_TOK mi_row={} mi_col={} tell={}",
                                                at.0 * 4,
                                                at.1 * 4,
                                                enc.tell()
                                            )
                                        });
                                        continue;
                                    }
                                    // The true edge falls inside this 16x16
                                    // leaf itself (mod-32==8 target sizes):
                                    // one axis only, per r6's charter -- an
                                    // 8x8 leaf never itself straddles, so the
                                    // block splits cleanly along whichever
                                    // axis is short and only the in-frame
                                    // 8x8s are coded, each its own leaf.
                                    if !has_cols16 && !has_rows16 {
                                        return Err(Error::unsupported(
                                            "AV1 tile",
                                            "a 16x16 block whose true edge cuts through both \
                                             axes needs a rectangular transform this writer \
                                             does not code yet",
                                        ));
                                    }
                                    if has_cols16 {
                                        enc.symbol_fixed(
                                            1,
                                            &gather(&cdfs.partition_w16[ctx], VERT_ALIKE),
                                        );
                                    } else {
                                        enc.symbol_fixed(
                                            1,
                                            &gather(&cdfs.partition_w16[ctx], HORZ_ALIKE),
                                        );
                                    }
                                    let leaves = block.eight.as_ref().ok_or_else(|| {
                                        Error::unsupported(
                                            "AV1 tile",
                                            "a 16x16 block the true frame edge cuts through \
                                             needs its `eight` leaves populated",
                                        )
                                    })?;
                                    let (mi_row0, mi_col0) =
                                        (sr as u32 * SUB_MI, sc as u32 * SUB_MI);
                                    let leaf_positions: Vec<(u32, u32)> = (0..4)
                                        .map(|i| (mi_row0 + (i / 2) * 2, mi_col0 + (i % 2) * 2))
                                        .filter(|&(mr, mc)| mr < mi_rows && mc < mi_cols)
                                        .collect();
                                    if leaves.len() != leaf_positions.len() {
                                        return Err(Error::unsupported(
                                            "AV1 tile",
                                            "a straddling 16x16 block needs one `eight` entry \
                                             per 8x8 leaf inside the true frame",
                                        ));
                                    }
                                    // r11: the enclosing 16x16 slot's
                                    // above_mode/left_mode arrays are too
                                    // coarse for a second leaf whose true
                                    // above (or left) neighbour is the FIRST
                                    // leaf -- track it here and hand it to
                                    // write_leaf8 as a context override.
                                    let mut prev_leaf: Option<((usize, usize), usize)> = None;
                                    for (leaf, (mr, mc)) in leaves.iter().zip(leaf_positions) {
                                        let leaf_mi = (mr as usize, mc as usize);
                                        // r8: read at mi granularity, not the
                                        // enclosing 16x16 slot -- the first
                                        // leaf's `record_mi` call below
                                        // updates `above_side_mi`/
                                        // `left_side_mi` at this leaf's own
                                        // mi position, which the second
                                        // leaf's ctx lookup then sees.
                                        let leaf_ctx = neighbours.partition_ctx_mi(leaf_mi, 8);
                                        ec_rng_trace(|| {
                                            format!(
                                                "EC_PART mi_row={} mi_col={} bsize=3 ctx={} tell={}",
                                                leaf_mi.0,
                                                leaf_mi.1,
                                                leaf_ctx,
                                                enc.tell()
                                            )
                                        });
                                        enc.symbol(
                                            PARTITION_NONE,
                                            &mut cdfs.partition_w8[leaf_ctx],
                                        );
                                        let grids = [
                                            level_grid(&leaf.luma, TX8)?,
                                            level_grid(&leaf.u, TX4)?,
                                            level_grid(&leaf.v, TX4)?,
                                        ];
                                        let leaf_mode = write_leaf8(
                                            &mut enc,
                                            &mut cdfs,
                                            &mut neighbours,
                                            leaf,
                                            at,
                                            leaf_mi,
                                            &grids,
                                            [&scans[2], &scans[3], &scans[3]],
                                            prev_leaf,
                                        );
                                        prev_leaf = Some((leaf_mi, leaf_mode));
                                        ec_rng_trace(|| {
                                            format!(
                                                "EC_TOK mi_row={} mi_col={} tell={}",
                                                leaf_mi.0,
                                                leaf_mi.1,
                                                enc.tell()
                                            )
                                        });
                                    }
                                    // `record()`'s `above_mode`/`left_mode`
                                    // write is a no-op at an 8x8 leaf's own
                                    // side (`side / SUB == 0`), so the next
                                    // 16x16 quadrant beyond this straddling
                                    // one would otherwise see whatever stale
                                    // mode sat here before it: force the
                                    // write once the whole quadrant's leaves
                                    // are done, from the last (bottom/right-
                                    // most) leaf -- lane-av1-rect r15: doing
                                    // this *inside* `write_leaf8`, once per
                                    // leaf, let the first leaf's write
                                    // clobber the true external neighbour a
                                    // second leaf of the *same* quadrant
                                    // still needed to read on its
                                    // non-adjacency axis.
                                    if let Some((_, mode)) = prev_leaf {
                                        neighbours.above_mode[at.1] = mode;
                                        neighbours.left_mode[at.0] = mode;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(enc.finish())
}

/// Writes everything a key frame's block carries before its coefficients: the
/// skip flag, its luma intra mode against the CDF its neighbours' modes pick,
/// the angle a directional mode is steered by, and its chroma mode. Hands back
/// the luma mode, which is what the blocks beside it read.
fn write_intra_mode(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    block: &BlockCoeffs,
    above_mode: usize,
    left_mode: usize,
    cfl: bool,
) -> usize {
    let mode = usize::from(block.mode);
    enc.symbol(0, &mut cdfs.skip[0]);
    enc.symbol(
        mode,
        &mut cdfs.kf_y_mode[INTRA_MODE_CTX[above_mode]][INTRA_MODE_CTX[left_mode]],
    );
    ec_rng_trace(|| format!("EC_YMODE mode={mode} tell={} rng={}", enc.tell(), enc.rng()));
    if (V_PRED..=D67_PRED).contains(&mode) {
        enc.symbol(ANGLE_DELTA_ZERO, &mut cdfs.angle_delta[mode - V_PRED]);
    }
    // A block small enough to be offered chroma from luma reads the wider
    // table even when it does not take the mode.
    if cfl {
        enc.symbol(DC_PRED, &mut cdfs.uv_mode_cfl[mode]);
    } else {
        enc.symbol(DC_PRED, &mut cdfs.uv_mode_no_cfl[mode]);
    }
    mode
}

/// Writes one coded block: its mode, its three transform blocks, and what it
/// leaves behind for the blocks beside it. Its partition symbol is already
/// written, because only the caller knows what tree led here.
#[allow(clippy::too_many_arguments)]
fn write_block(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    block: &BlockCoeffs,
    at: (usize, usize),
    side: usize,
    planes: &[TxbSet; 3],
    grids: &[Vec<i32>; 3],
    scans: [&Vec<u16>; 3],
    cfl: bool,
) {
    let (r, c) = at;
    let mode = write_intra_mode(
        enc,
        cdfs,
        block,
        neighbours.above_mode[c],
        neighbours.left_mode[r],
        cfl,
    );
    write_block_planes(
        enc,
        cdfs,
        planes,
        grids,
        &scans,
        &neighbours.around(at, side),
        mode,
    );
    neighbours.record(at, side, mode, grids);
}

/// Writes one 8x8 leaf of a straddling 16x16 block (lane-av1-rect): its own
/// luma transform and 4x4 chroma transforms, coded exactly like
/// [`write_block`] but reading its intra-mode context from the *enclosing*
/// 16x16 slot -- `outer_at`, in [`SUB`]-grid units -- rather than from its own
/// finer position, since [`Neighbours`]'s `above_mode`/`left_mode` arrays stay
/// at [`SUB`] (16-sample) granularity. `leaf_mi` is this leaf's own position
/// in 4x4 mode-info units, which is what its coefficient context (finer than
/// [`SUB`]) is kept and read at.
#[allow(clippy::too_many_arguments)]
fn write_leaf8(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    block: &BlockCoeffs,
    outer_at: (usize, usize),
    leaf_mi: (usize, usize),
    grids: &[Vec<i32>; 3],
    scans: [&Vec<u16>; 3],
    prev_leaf: Option<((usize, usize), usize)>,
) -> usize {
    let (r, c) = outer_at;
    let mut above_mode = neighbours.above_mode[c];
    let mut left_mode = neighbours.left_mode[r];
    // The previous leaf sits directly above (same column, two mi rows up)
    // or directly to the left (same row, two mi cols over) of this one --
    // in either case its just-written mode, not the enclosing 16x16 slot's
    // stale neighbour, is what a decoder reads as this leaf's context.
    if let Some(((pr, pc), pmode)) = prev_leaf {
        if pc == leaf_mi.1 && leaf_mi.0 == pr + 2 {
            above_mode = pmode;
        } else if pr == leaf_mi.0 && leaf_mi.1 == pc + 2 {
            left_mode = pmode;
        }
    }
    // An 8x8 leaf is well within `is_cfl_allowed`'s <=32x32 bound (spec
    // 5.11.5), so it reads the CFL-allowed `uv_mode_cfl` CDF -- like every
    // other `write_block` caller at 16x16 and up -- not the narrower
    // no-CFL one: r12 lane-av1-rect, this leaf's own `cfl: false` was the
    // true first divergence (a differently-sized alphabet under the same
    // DC_PRED decision desyncs the coder even though the decoded mode is
    // unchanged).
    let mode = write_intra_mode(enc, cdfs, block, above_mode, left_mode, true);
    let planes = [TxbSet::Luma8, TxbSet::Chroma4, TxbSet::Chroma4];
    write_block_planes(
        enc,
        cdfs,
        &planes,
        grids,
        &scans,
        &neighbours.around_mi(leaf_mi, 8),
        mode,
    );
    neighbours.record_mi(leaf_mi, 8, grids);
    mode
}

/// Writes the three transform blocks of one coded block, in the order a
/// decoder reads them.
#[allow(clippy::too_many_arguments)]
fn write_block_planes(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    planes: &[TxbSet; 3],
    grids: &[Vec<i32>; 3],
    scans: &[&Vec<u16>; 3],
    around: &[Around; 3],
    mode: usize,
) {
    for (plane, (grid, scan)) in grids.iter().zip(scans.iter()).enumerate() {
        // Luma's transform covers its whole block, which fixes the all-zero
        // flag's context at zero; a chroma transform reads whether its
        // neighbours coded anything, on top of the offset the chroma tables
        // start at.
        let skip_ctx = if plane == 0 {
            0
        } else {
            usize::from(around[plane].above_coded) + usize::from(around[plane].left_coded)
        };
        write_coeffs(
            enc,
            &mut cdfs.txb(planes[plane], mode),
            grid,
            scan,
            skip_ctx,
            dc_sign_ctx(around[plane].dc_vote),
            Some(plane),
        );
        ec_rng_trace(|| {
            format!(
                "EC_PLANE plane={plane} nz={} skip_ctx={skip_ctx} tell={}",
                grid.iter().filter(|&&l| l != 0).count(),
                enc.tell()
            )
        });
    }
}

/// What a coded transform block leaves behind for the blocks that read it as a
/// neighbour.
fn neighbour_state(grid: &[i32]) -> Neighbour {
    Neighbour {
        coded: grid.iter().any(|&l| l != 0),
        dc: (grid[0] != 0).then_some(grid[0] < 0),
    }
}

/// Writes the payload of a one-tile key frame in which every superblock is
/// split into 32x32 blocks, each carrying the coefficients `blocks` gives it.
///
/// `blocks` gives one coefficient set per 32x32 block in raster order across
/// the frame. This is [`sb_coeff_key_frame_tile`] with every superblock split.
///
/// # Errors
/// As [`sb_coeff_key_frame_tile`], with `blocks` sized for the 32x32 grid.
pub fn split_coeff_key_frame_tile(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    blocks: &[BlockCoeffs],
) -> Result<Vec<u8>> {
    check_blocks(mi_cols, mi_rows)?;
    let (cols, rows) = block_grid(mi_cols, mi_rows);
    if blocks.len() != (cols * rows) as usize {
        return Err(Error::unsupported(
            "AV1 tile",
            "a coefficient key frame needs one coefficient set per 32x32 block",
        ));
    }
    let (sb_cols, sb_rows) = (cols.div_ceil(2), rows.div_ceil(2));
    let superblocks: Vec<Superblock> = (0..sb_rows)
        .flat_map(|sb_r| (0..sb_cols).map(move |sb_c| (sb_r, sb_c)))
        .map(|(sb_r, sb_c)| {
            Superblock::Split(
                (0..4)
                    .map(|q| (sb_r * 2 + q / 2, sb_c * 2 + q % 2))
                    .filter(|&(r, c)| r < rows && c < cols)
                    .map(|(r, c)| Quadrant::Whole(blocks[(r * cols + c) as usize].clone()))
                    .collect(),
            )
        })
        .collect();
    sb_coeff_key_frame_tile(mi_cols, mi_rows, base_q_idx, &superblocks)
}

/// Lays one plane's coefficient list out over its transform, rejecting the
/// positions and levels the writer does not code.
fn level_grid(coeffs: &[Coeff], side: usize) -> Result<Vec<i32>> {
    let mut grid = vec![0i32; side * side];
    for coeff in coeffs {
        if usize::from(coeff.row) >= side || usize::from(coeff.col) >= side {
            return Err(Error::unsupported(
                "AV1 tile",
                "a coefficient sits outside the transform of its plane",
            ));
        }
        if coeff.level == 0 || coeff.level.abs() > MAX_LEVEL {
            return Err(Error::unsupported(
                "AV1 tile",
                format!(
                    "coefficients are written for levels -{MAX_LEVEL}..={MAX_LEVEL} without zero"
                ),
            ));
        }
        let pos = usize::from(coeff.row) * side + usize::from(coeff.col);
        if grid[pos] != 0 {
            return Err(Error::unsupported(
                "AV1 tile",
                "two coefficients of one block share a position",
            ));
        }
        grid[pos] = coeff.level;
    }
    Ok(grid)
}

/// The default scan of a square transform (spec 8.4.2's `Default_Scan_NxN`),
/// as raster positions in the order they are coded.
///
/// The table is a rule rather than hundreds of pinned numbers: the scan walks
/// the anti-diagonals of the transform outwards from the origin, each diagonal
/// from its top-right end on odd diagonals and from its bottom-left end on
/// even ones.
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

/// What one 32x32 luma transform block's levels cost, in bits, priced through
/// the very CDFs [`write_coeffs`] will write them with -- end-of-block
/// position, base levels, base range, signs and the Golomb tail all included.
///
/// The mode search calls this before the block's neighbours exist, so the two
/// neighbour-derived contexts (the block-skip context and the DC sign context)
/// are taken as zero. Every other context inside the block is exact, because
/// those are read from the block's own levels.
///
/// The encoder codes 32x32 luma only -- every superblock it emits is split --
/// so one table is all the search needs; a second block size would need its
/// own entry point rather than a size argument, because the table and the scan
/// have to agree.
#[cfg(test)]
pub(crate) fn luma_32_coeff_bits(grid: &[i32]) -> f64 {
    coeff_bits(grid, TxbSet::Luma32)
}

/// What one transform block's levels cost through the CDFs of `set`, in bits.
///
/// The search runs before the tile is written, so the adapted state the block
/// will really be coded against does not exist yet: the price is taken against
/// the defaults every tile starts from, and against the contexts a block whose
/// neighbours coded nothing reads.
pub(crate) fn coeff_bits(grid: &[i32], set: TxbSet) -> f64 {
    /// Built once per size: the search prices thirteen modes for every block,
    /// and the scan is the same table every time.
    static SCANS: LazyLock<[Vec<u16>; 4]> = LazyLock::new(|| {
        [
            default_scan(TX4),
            default_scan(TX8),
            default_scan(TX16),
            default_scan(TX32),
        ]
    });
    let mut cdfs = Cdfs::new(2);
    let mut coding = cdfs.txb(set, DC_PRED);
    let scan = match coding.side {
        TX4 => &SCANS[0],
        TX8 => &SCANS[1],
        TX16 => &SCANS[2],
        _ => &SCANS[3],
    };
    let mut enc = SymbolEncoder::new();
    enc.reset_bits();
    write_coeffs(&mut enc, &mut coding, grid, scan, 0, 0, None);
    enc.bits()
}

/// What the partition symbol of a block of `side` samples costs, in bits, when
/// it says the block is split (or that it is not).
///
/// The price is taken at context zero — a block whose neighbours are no finer
/// than it is — because the search that reads it runs before the tile knows
/// what its neighbours will be.
pub(crate) fn partition_bits(side: usize, split: bool) -> f64 {
    let cdfs = Cdfs::new(2);
    let cdf: &[u16] = match side {
        SB => &cdfs.partition_w64[0],
        BLOCK => &cdfs.partition_w32[0],
        _ => &cdfs.partition_w16[0],
    };
    let symbol = if split {
        PARTITION_SPLIT
    } else {
        PARTITION_NONE
    };
    crate::encode::symbol_bits(cdf, symbol)
}

/// coeffs() for one plane's transform block of any coefficients the base and
/// base-range syntax reach (spec 5.11.39).
///
/// A 16x16 luma transform codes which transform type it is, because its set
/// holds more than one; 32x32 and 64x64 are DCT-only by spec 5.11.40. The
/// levels the contexts below are read from are the levels of
/// coefficients later in the scan, which a decoder walking the scan backwards
/// already has.
#[allow(clippy::too_many_arguments)]
fn write_coeffs(
    enc: &mut SymbolEncoder,
    coding: &mut TxbTables,
    grid: &[i32],
    scan: &[u16],
    skip_ctx: usize,
    sign_ctx: usize,
    plane: Option<usize>,
) {
    if let Some(plane) = plane {
        ec_rng_trace(|| format!("EC_PLANE plane={plane} tell_before={}", enc.tell()));
    }
    let side = coding.side;
    let eob = scan
        .iter()
        .rposition(|&pos| grid[pos as usize] != 0)
        .map_or(0, |i| i + 1);
    enc.symbol(usize::from(eob == 0), &mut coding.txb_skip[skip_ctx]);
    if plane.is_some() {
        ec_rng_trace(|| {
            format!(
                "EC_TXBSKIP ctx={skip_ctx} eob0={} tell={} rng={}",
                usize::from(eob == 0),
                enc.tell(),
                enc.rng()
            )
        });
    }
    if eob == 0 {
        return;
    }
    // A luma transform whose type set holds more than one type codes which it
    // is, right after the all-zero flag (spec 5.11.39). The writer only ever
    // uses DCT_DCT, which is index one of `Tx_Type_Intra_Inv_Set2`.
    if let Some(tx_type) = coding.tx_type.as_deref_mut() {
        enc.symbol(TX_TYPE_DCT_DCT_SET2, tx_type);
        if plane.is_some() {
            ec_rng_trace(|| format!("EC_TXTYPE tell={} rng={}", enc.tell(), enc.rng()));
        }
    }

    write_eob(enc, coding, eob, plane);

    for scan_idx in (0..eob).rev() {
        let pos = scan[scan_idx] as usize;
        let (row, col) = (pos / side, pos % side);
        let level = grid[pos].abs();
        if scan_idx == eob - 1 {
            let ctx = eob_coeff_ctx(scan_idx, side * side);
            let sym = (level.min(NUM_BASE_LEVELS + 1) - 1) as usize;
            enc.symbol(sym, &mut coding.base_eob[ctx]);
            if plane.is_some() {
                ec_rng_trace(|| {
                    format!(
                        "EC_BASEEOB scan_idx={scan_idx} ctx={ctx} level={} tell={}",
                        sym + 1,
                        enc.tell()
                    )
                });
            }
        } else {
            let ctx = base_ctx(grid, side, row, col);
            let sym = level.min(NUM_BASE_LEVELS + 1) as usize;
            enc.symbol(sym, &mut coding.base[ctx]);
            if plane.is_some() {
                ec_rng_trace(|| {
                    format!(
                        "EC_BASE scan_idx={scan_idx} ctx={ctx} level={sym} tell={}",
                        enc.tell()
                    )
                });
            }
        }
        if level > NUM_BASE_LEVELS {
            let ctx = br_ctx(grid, side, row, col);
            let mut remaining = level - (NUM_BASE_LEVELS + 1);
            let mut sent = 0;
            while sent < COEFF_BASE_RANGE {
                let k = remaining.min(BR_STEP);
                enc.symbol(k as usize, &mut coding.br[ctx]);
                if plane.is_some() {
                    ec_rng_trace(|| {
                        format!(
                            "EC_BR scan_idx={scan_idx} ctx={ctx} k={k} tell={}",
                            enc.tell()
                        )
                    });
                }
                if k < BR_STEP {
                    break;
                }
                remaining -= k;
                sent += BR_STEP;
            }
        }
    }

    // The signs come after the levels, in scan order, the DC's from a CDF and
    // the rest as raw bits (spec 5.11.39).
    for &pos in &scan[..eob] {
        let level = grid[pos as usize];
        if level == 0 {
            continue;
        }
        if pos == 0 {
            enc.symbol(usize::from(level < 0), &mut coding.dc_sign[sign_ctx]);
            if plane.is_some() {
                ec_rng_trace(|| {
                    format!(
                        "EC_DCSIGN ctx={sign_ctx} neg={} tell={}",
                        usize::from(level < 0),
                        enc.tell()
                    )
                });
            }
        } else {
            enc.literal(u32::from(level < 0), 1);
        }
        // A level the base and base-range syntax cannot reach carries the rest
        // of itself here, after its own sign (spec 5.11.39).
        if level.abs() > MAX_BR_LEVEL {
            write_golomb(enc, (level.abs() - MAX_BR_LEVEL - 1) as u32);
            if plane.is_some() {
                ec_rng_trace(|| format!("EC_GOLOMB tell={}", enc.tell()));
            }
        }
    }
    if let Some(plane) = plane {
        ec_rng_trace(|| format!("EC_PLANE plane={plane} eob={eob} tell_after={}", enc.tell()));
    }
}

/// The end-of-block position (spec 5.11.39): which group of scan positions the
/// last coded coefficient falls in, then its offset inside that group — the
/// offset's top bit from a CDF and the rest as raw bits.
fn write_eob(enc: &mut SymbolEncoder, coding: &mut TxbTables, eob: usize, plane: Option<usize>) {
    /// `Eob_Group_Start` (spec 5.11.39): the first scan position each group of
    /// end-of-block positions covers, indexed by the group's own number.
    const GROUP_START: [usize; 12] = [0, 1, 2, 3, 5, 9, 17, 33, 65, 129, 257, 513];
    /// `Eob_Offset_Bits` (spec 5.11.39): how wide each group's offset is.
    const OFFSET_BITS: [u32; 12] = [0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9];

    let group = GROUP_START
        .iter()
        .rposition(|&start| start <= eob)
        .expect("the first group starts at zero");
    // The groups are numbered from one, and how many of them the transform
    // reaches is the size of its own end-of-block alphabet.
    enc.symbol(group - 1, coding.eob_pt);
    if let Some(plane) = plane {
        ec_rng_trace(|| {
            format!(
                "EC_EOBPT plane={plane} eob_pt={group} tell={} rng={}",
                enc.tell(),
                enc.rng()
            )
        });
    }

    let bits = OFFSET_BITS[group];
    if bits > 0 {
        let offset = (eob - GROUP_START[group]) as u32;
        let top = (offset >> (bits - 1)) & 1;
        enc.symbol(top as usize, &mut coding.eob_extra[group - 3]);
        if let Some(plane) = plane {
            ec_rng_trace(|| {
                format!(
                    "EC_EOBEXTRA plane={plane} eob_ctx={} top={top} tell={}",
                    group - 3,
                    enc.tell()
                )
            });
        }
        if bits > 1 {
            enc.literal(offset & ((1 << (bits - 1)) - 1), bits - 1);
            if let Some(plane) = plane {
                ec_rng_trace(|| {
                    format!(
                        "EC_EOBBITS plane={plane} eob_extra={offset} tell={}",
                        enc.tell()
                    )
                });
            }
        }
    }
}

/// Writes the remainder of a level the base and base-range syntax could not
/// reach (spec 5.11.40): the value plus one, as its bit length in unary
/// followed by the value's own bits, most significant first.
pub(crate) fn write_golomb(enc: &mut SymbolEncoder, value: u32) {
    let x = value + 1;
    let length = 32 - x.leading_zeros();
    for _ in 0..length - 1 {
        enc.literal(0, 1);
    }
    for i in (0..length).rev() {
        enc.literal((x >> i) & 1, 1);
    }
}

/// The context of the level of the last coded coefficient (spec 8.3.2): how
/// far into the scan it sits, in quarters and eighths of the transform.
fn eob_coeff_ctx(scan_idx: usize, area: usize) -> usize {
    match scan_idx {
        0 => 0,
        i if i <= area / 8 => 1,
        i if i <= area / 4 => 2,
        _ => 3,
    }
}

/// The context of a coefficient's level (spec 8.3.2): the magnitudes of the
/// five neighbours below and to the right of it — the ones a decoder walking
/// the scan backwards already has — plus a term for where in the transform it
/// sits. The DC reads context 0 whatever its neighbours carry.
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

/// The context of a coefficient's base-range tail (spec 8.3.2): the magnitudes
/// of its three closest neighbours below and to the right, uncapped, and a
/// term separating the DC, the corner of the transform, and the rest.
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

/// A level at a position that may fall off the transform, where the levels a
/// context reads are zero.
fn neighbour(grid: &[i32], side: usize, row: usize, col: usize) -> i32 {
    if row >= side || col >= side {
        0
    } else {
        grid[row * side + col]
    }
}

/// Shared by both DC writers: one level per block, none of them zero.
fn check_levels(levels: &[i32], blocks: usize) -> Result<()> {
    if levels.len() != blocks {
        return Err(Error::unsupported(
            "AV1 tile",
            "a DC-only key frame needs one level per coded block",
        ));
    }
    if levels.iter().any(|&l| l == 0 || l.abs() > MAX_BR_LEVEL) {
        return Err(Error::unsupported(
            "AV1 tile",
            "a DC-only key frame is written for levels -14..=14 without zero; \
             wider levels need the Golomb tail",
        ));
    }
    Ok(())
}

/// coeffs() for a luma transform block whose only coefficient is the DC (spec
/// 5.11.39). The block size is the transform size, so the all-zero flag's
/// context is 0; an end-of-block of one is token 0 of the position alphabet
/// with no extra bits; the transform sizes here are all DCT-only, so no
/// transform type is coded; and the DC's own neighbours are zero, so its
/// magnitude contexts are 0 throughout.
fn write_dc_coeffs(
    enc: &mut SymbolEncoder,
    dc_level: i32,
    sign_ctx: usize,
    q_ctx: usize,
    txb_skip: &[u16],
    base_eob: &[u16],
) {
    use crate::cdf_state::pick;
    let level = dc_level.abs();
    let eob_pt = pick(
        q_ctx,
        cdf::EOB_PT_1024_LUMA_Q0,
        cdf::EOB_PT_1024_LUMA_Q1,
        cdf::EOB_PT_1024_LUMA,
        cdf::EOB_PT_1024_LUMA_Q3,
    );
    let br = pick(
        q_ctx,
        cdf::COEFF_BR_LUMA_32_Q0,
        cdf::COEFF_BR_LUMA_32_Q1,
        cdf::COEFF_BR_LUMA_32,
        cdf::COEFF_BR_LUMA_32_Q3,
    );
    enc.symbol_fixed(0, txb_skip);
    enc.symbol_fixed(0, &eob_pt);
    enc.symbol_fixed((level.min(NUM_BASE_LEVELS + 1) - 1) as usize, base_eob);
    if level > NUM_BASE_LEVELS {
        let mut remaining = level - (NUM_BASE_LEVELS + 1);
        let mut sent = 0;
        while sent < COEFF_BASE_RANGE {
            let k = remaining.min(BR_STEP);
            enc.symbol_fixed(k as usize, &br[0]);
            if k < BR_STEP {
                break;
            }
            remaining -= k;
            sent += BR_STEP;
        }
    }
    // The signs come after the levels, DC first (spec 5.11.39).
    enc.symbol_fixed(usize::from(dc_level < 0), &cdf::DC_SIGN_LUMA[sign_ctx]);
}

/// One 4x4 unit's vote in `Dc_Sign_Contexts` (spec 8.3.2): plus one for a
/// positive DC, minus one for a negative one, nothing for a unit whose block
/// carried no DC or that sits past the frame's true edge (spec
/// `av1_set_entropy_contexts`), which is why the vote is gathered per 4x4
/// unit and not per coded cell -- a unit past the edge does not vote even
/// when the rest of its cell does.
fn dc_vote(dc: Option<bool>) -> i32 {
    match dc {
        None => 0,
        Some(true) => -1,
        Some(false) => 1,
    }
}

/// Which of the three DC sign contexts a gathered vote picks.
fn dc_sign_ctx(vote: i32) -> usize {
    match vote.signum() {
        0 => 0,
        -1 => 1,
        _ => 2,
    }
}

/// The `is_inter` context (spec 5.11.16 via `av1_get_intra_inter_context`,
/// `pred_common.c`): both neighbours' intra/inter state when both are
/// available, one neighbour's when only one is, and zero at a tile's own
/// top-left corner.
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

/// `CLASS0_SIZE << (class + 2)` (spec 3), the magnitude an `MV_CLASS_n`
/// component's own bits start counting from; class zero starts at zero.
fn mv_class_base(class: usize) -> i32 {
    if class == 0 { 0 } else { 2i32 << (class + 2) }
}

/// The class a pre-offset magnitude `z` (`|diff| - 1`) falls in — the inverse
/// of `mv_class_base`'s doubling ranges, ported from libaom's
/// `av1_get_mv_class` (`mv.h`).
fn mv_class_of(z: i32) -> usize {
    let mut class = 0;
    while class < 10 && mv_class_base(class + 1) <= z {
        class += 1;
    }
    class
}

/// Writes one motion vector component's non-zero diff (spec 5.11.32
/// `read_mv_component`, run backwards): sign, class, then the class's own
/// bits. `allow_high_precision_mv` is off in every frame this writer codes,
/// so the eighth-pel bit is always inferred as one rather than coded — which
/// means only diffs whose eighth-pel bit really is one are representable;
/// anything else is refused rather than rounded, since rounding would silently
/// code a different vector than the caller asked for.
///
/// # Errors
/// Returns an error when `diff` needs the eighth-pel precision this writer's
/// frames do not carry.
fn write_mv_component(enc: &mut SymbolEncoder, c: &mut MvComponentCdfs, diff: i32) -> Result<()> {
    debug_assert_ne!(diff, 0);
    let sign = diff < 0;
    enc.symbol(usize::from(sign), &mut c.sign);
    let mag = diff.unsigned_abs() as i32;
    let z = mag - 1;
    if z & 1 == 0 {
        return Err(Error::unsupported(
            "AV1 tile",
            "a motion vector component needs eighth-pel precision, which \
             allow_high_precision_mv off does not carry",
        ));
    }
    let class = mv_class_of(z);
    enc.symbol(class, &mut c.class);
    let local = z - mv_class_base(class);
    if class == 0 {
        let bit = (local >> 3) & 1;
        let fr = (local >> 1) & 3;
        enc.symbol(bit as usize, &mut c.class0_bit);
        enc.symbol(fr as usize, &mut c.class0_fr[bit as usize]);
    } else {
        let d = local >> 3;
        let fr = (local >> 1) & 3;
        for i in 0..class {
            enc.symbol(((d >> i) & 1) as usize, &mut c.bit[i]);
        }
        enc.symbol(fr as usize, &mut c.fr);
    }
    // The eighth-pel bit itself is inferred, not coded (spec 5.11.32: "if
    // (allow_high_precision_mv) mv_class0_hp ... else mv_class0_hp = 1").
    Ok(())
}

/// Writes a motion vector as a residual against `pred` (spec 5.11.32
/// `read_mv`): the joint symbol naming which components differ, then each
/// differing component.
fn write_mv(
    enc: &mut SymbolEncoder,
    mv_comp: &mut [MvComponentCdfs; 2],
    mv_joint: &mut [u16; 5],
    mv: (i32, i32),
    pred: (i32, i32),
) -> Result<()> {
    let diff = (mv.0 - pred.0, mv.1 - pred.1);
    let joint = match (diff.0 != 0, diff.1 != 0) {
        (false, false) => 0, // MV_JOINT_ZERO
        (false, true) => 1,  // MV_JOINT_HNZVZ: column only
        (true, false) => 2,  // MV_JOINT_HZVNZ: row only
        (true, true) => 3,   // MV_JOINT_HNZVNZ
    };
    enc.symbol(joint, mv_joint);
    if diff.0 != 0 {
        write_mv_component(enc, &mut mv_comp[0], diff.0)?;
    }
    if diff.1 != 0 {
        write_mv_component(enc, &mut mv_comp[1], diff.1)?;
    }
    Ok(())
}

/// Writes the payload of a one-tile inter frame built from `blocks`, one
/// 32x32 block per entry in raster order across the frame — every superblock
/// is split into its four quadrants the way [`split_coeff_key_frame_tile`]
/// codes a key frame, because a mixed-size partition tree buys this writer's
/// gate nothing a flat grid does not already prove: the CDF state, the
/// neighbour contexts and the single-reference/MV-stack machinery below are
/// exactly as size-sensitive whether the tree recurses or not, and recursing
/// it would be undirected scaffolding — see `sb_coeff_key_frame_tile` for
/// where that tree already lives, ready to graft this block's mode reads
/// onto once a real partition search needs it.
///
/// A block whose [`BlockCoeffs::inter`] is `Some` is coded inter: `skip`,
/// then `is_inter`, then a single-reference chain that always names `LAST`
/// (spec 5.11.25 `single_ref_p1`/`p3`/`p4`, this crate's only reference so
/// far), then the two-mode `read_inter_mode` chain (spec 5.11.24) and, for
/// [`InterMode::NewMv`], a DRL index (always zero) and a coded motion vector
/// residual. A block whose `inter` is `None` codes intra, through the
/// *inter*-frame intra path (spec 5.11.16's `intra_frame_mode_info` is the
/// key frame writers' path; this is `inter_frame_mode_info`'s intra branch):
/// `Y_MODE` by size group rather than `KF_Y_MODE` by neighbour context, since
/// an inter frame's intra blocks do not read their neighbours' modes.
///
/// # Errors
/// Returns an error when the frame is not a whole number of 64x64
/// superblocks, when `blocks` does not carry exactly one entry per 32x32
/// block, when an intra block names a mode a key frame does not code, when a
/// coefficient sits outside its transform or is one the writer cannot code,
/// when a `NEWMV` block's motion vector needs eighth-pel precision.
pub fn sb_coeff_inter_frame_tile(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    blocks: &[Quadrant],
) -> Result<Vec<u8>> {
    check_blocks(mi_cols, mi_rows)?;
    // `block_grid`'s ceiling, not a plain division: a true frame size that is
    // not a whole number of 32x32 blocks (or of 64x64 superblocks) still has
    // one more block/superblock whose own origin is inside the true frame,
    // same as `sb_coeff_key_frame_tile`.
    let (cols, rows) = block_grid(mi_cols, mi_rows);
    if blocks.len() != (cols * rows) as usize {
        return Err(Error::unsupported(
            "AV1 tile",
            "an inter frame needs one entry per 32x32 block inside the true frame",
        ));
    }
    let coded: Vec<&BlockCoeffs> = blocks.iter().flat_map(Quadrant::blocks).collect();
    if let Some(bad) = coded
        .into_iter()
        .find(|b| b.inter.is_none() && usize::from(b.mode) >= INTRA_MODES)
    {
        return Err(Error::unsupported(
            "AV1 tile",
            format!(
                "intra mode {} is not one of the thirteen this writer codes",
                bad.mode
            ),
        ));
    }

    /// `Y_MODE`'s size group (spec `Size_Group`) for a 32x32 block, the only
    /// size this writer's inter branch codes.
    const SIZE_GROUP_32: usize = 3;
    /// `Ref_Frame_List`'s `LAST_FRAME` (spec 3): the only reference this
    /// writer's single-reference chain ever names.
    const LAST_FRAME: i8 = 1;

    let (sb_cols, sb_rows) = (cols.div_ceil(2), rows.div_ceil(2));
    let mut neighbours = Neighbours::new(
        cols as usize * 2,
        rows as usize * 2,
        mi_cols as usize,
        mi_rows as usize,
    );
    let mut grid = MiGrid::new(mi_cols as usize, mi_rows as usize);
    let mut cdfs = Cdfs::new(q_ctx_of(base_q_idx));
    let mut enc = SymbolEncoder::new();
    // An `is_inter` block's 32x32 luma transform reads a different
    // `tx_type` set than an intra block's (`get_tx_set`, spec 5.11.48; see
    // `TxbSet::Luma32Inter`'s doc comment); chroma's transform type is
    // derived from luma's, not coded, so it never differs between the two.
    let intra_planes = [TxbSet::Luma32, TxbSet::Chroma16, TxbSet::Chroma16];
    let inter_planes = [TxbSet::Luma32Inter, TxbSet::Chroma16, TxbSet::Chroma16];
    let scan32 = default_scan(TX32);
    let scan16 = default_scan(TX16);
    let scan8 = default_scan(TX8);
    let scan4 = default_scan(TX4);
    let zero_grids = [
        vec![0i32; TX32 * TX32],
        vec![0i32; TX16 * TX16],
        vec![0i32; TX16 * TX16],
    ];

    for sb_r in 0..sb_rows {
        neighbours.start_row();
        for sb_c in 0..sb_cols {
            let sb_at = (sb_r as usize * 4, sb_c as usize * 4);
            let sb_ctx = neighbours.partition_ctx(sb_at, SB);
            // spec `decode_partition`'s hasRows/hasCols (5.11.4): a superblock
            // whose bottom or right half falls outside the true frame cannot
            // be left whole, but this writer only ever splits a superblock
            // into its four 32x32 quadrants (never `PARTITION_NONE` at 64x64),
            // so the only question is which of the three partition symbols
            // that split takes — same three-way signaling as
            // `sb_coeff_key_frame_tile`'s superblock level.
            let (has_cols, has_rows) = (
                sb_c * SB_MI + SB_MI / 2 < mi_cols,
                sb_r * SB_MI + SB_MI / 2 < mi_rows,
            );
            match (has_cols, has_rows) {
                (true, true) => enc.symbol(PARTITION_SPLIT, &mut cdfs.partition_w64[sb_ctx]),
                (true, false) => {
                    enc.symbol_fixed(1, &gather(&cdfs.partition_w64[sb_ctx], VERT_ALIKE));
                }
                (false, true) => {
                    enc.symbol_fixed(1, &gather(&cdfs.partition_w64[sb_ctx], HORZ_ALIKE));
                }
                (false, false) => {}
            }

            for quadrant in 0..4 {
                let (r32, c32) = (sb_r * 2 + quadrant / 2, sb_c * 2 + quadrant % 2);
                // Only a quadrant whose own mi origin is inside the true
                // frame is coded at all (spec `decode_partition`'s
                // `r >= MiRows || c >= MiCols` early return), same filter as
                // `sb_coeff_key_frame_tile`'s `quadrant_positions`.
                if r32 >= rows || c32 >= cols {
                    continue;
                }
                let site = &blocks[(r32 * cols + c32) as usize];
                let at = (r32 as usize * 2, c32 as usize * 2);
                let ctx32 = neighbours.partition_ctx(at, BLOCK);
                // spec `decode_partition`'s hasRows/hasCols recomputed at this
                // quadrant's own half: a whole 32x32 block cannot be left
                // whole once its own half straddles the true edge, mirroring
                // `sb_coeff_key_frame_tile`'s "cannot be left whole" refusal.
                let (has_cols32, has_rows32) = (
                    has_half(c32 * BLOCK_MI, BLOCK_MI, mi_cols),
                    has_half(r32 * BLOCK_MI, BLOCK_MI, mi_rows),
                );
                let block = match site {
                    Quadrant::Whole(block) => {
                        if !has_cols32 || !has_rows32 {
                            return Err(Error::unsupported(
                                "AV1 tile",
                                "a 32x32 block that is half outside the true frame \
                                 cannot be left whole",
                            ));
                        }
                        enc.symbol(PARTITION_NONE, &mut cdfs.partition_w32[ctx32]);
                        block
                    }
                    Quadrant::Split(sub_blocks) => {
                        // Same filter as `sb_coeff_key_frame_tile`'s
                        // `sub_positions`: only the 16x16 leaves whose own mi
                        // origin is inside the true frame are coded.
                        let sub_positions: Vec<(usize, usize)> = (0..4)
                            .map(|i| (r32 as usize * 2 + i / 2, c32 as usize * 2 + i % 2))
                            .filter(|&(sr, sc)| {
                                (sr as u32) * SUB_MI < mi_rows && (sc as u32) * SUB_MI < mi_cols
                            })
                            .collect();
                        if sub_blocks.len() != sub_positions.len() {
                            return Err(Error::unsupported(
                                "AV1 tile",
                                "a split 32x32 inter-frame block needs one 16x16 entry \
                                 per sub-block inside the true frame",
                            ));
                        }
                        // Same three-way spec signaling as the superblock
                        // level above, recomputed at this quadrant's own half.
                        match (has_cols32, has_rows32) {
                            (true, true) => {
                                enc.symbol(PARTITION_SPLIT, &mut cdfs.partition_w32[ctx32]);
                            }
                            (true, false) => {
                                enc.symbol_fixed(
                                    1,
                                    &gather(&cdfs.partition_w32[ctx32], VERT_ALIKE),
                                );
                            }
                            (false, true) => {
                                enc.symbol_fixed(
                                    1,
                                    &gather(&cdfs.partition_w32[ctx32], HORZ_ALIKE),
                                );
                            }
                            (false, false) => {}
                        }
                        for (leaf, (sr, sc)) in sub_blocks.iter().zip(sub_positions) {
                            // A 16x16 leaf whose own half straddles the true
                            // frame edge on both axes needs a rectangular
                            // transform this writer does not code yet; one
                            // axis only is two (or, at a true corner, one)
                            // 8x8 leaves (lane-av1inter8), same split the key
                            // frame search takes at this geometry.
                            let (has_cols16, has_rows16) = (
                                has_half(sc as u32 * SUB_MI, SUB_MI, mi_cols),
                                has_half(sr as u32 * SUB_MI, SUB_MI, mi_rows),
                            );
                            if !has_cols16 && !has_rows16 {
                                return Err(Error::unsupported(
                                    "AV1 tile",
                                    "a 16x16 inter-frame block whose true edge cuts through \
                                     both axes needs a rectangular transform this writer \
                                     does not code yet",
                                ));
                            }
                            let at16 = (sr, sc);
                            if has_cols16 && has_rows16 {
                                let ctx16 = neighbours.partition_ctx(at16, SUB);
                                enc.symbol(PARTITION_NONE, &mut cdfs.partition_w16[ctx16]);
                                write_inter_frame_leaf(
                                    &mut enc,
                                    &mut cdfs,
                                    &mut neighbours,
                                    &mut grid,
                                    mi_cols,
                                    mi_rows,
                                    leaf,
                                    at16,
                                    &scan16,
                                    &scan8,
                                )?;
                            } else {
                                let ctx16 = neighbours.partition_ctx(at16, SUB);
                                if has_cols16 {
                                    enc.symbol_fixed(
                                        1,
                                        &gather(&cdfs.partition_w16[ctx16], VERT_ALIKE),
                                    );
                                } else {
                                    enc.symbol_fixed(
                                        1,
                                        &gather(&cdfs.partition_w16[ctx16], HORZ_ALIKE),
                                    );
                                }
                                let leaves = leaf.eight.as_ref().ok_or_else(|| {
                                    Error::unsupported(
                                        "AV1 tile",
                                        "a 16x16 inter-frame block the true frame edge cuts \
                                         through needs its `eight` leaves populated",
                                    )
                                })?;
                                let (mi_row0, mi_col0) = (sr as u32 * SUB_MI, sc as u32 * SUB_MI);
                                let leaf_positions: Vec<(u32, u32)> = (0..4)
                                    .map(|i| (mi_row0 + (i / 2) * 2, mi_col0 + (i % 2) * 2))
                                    .filter(|&(mr, mc)| mr < mi_rows && mc < mi_cols)
                                    .collect();
                                if leaves.len() != leaf_positions.len() {
                                    return Err(Error::unsupported(
                                        "AV1 tile",
                                        "a straddling 16x16 inter-frame block needs one \
                                         `eight` entry per 8x8 leaf inside the true frame",
                                    ));
                                }
                                let mut prev_leaf: Option<((usize, usize), bool, bool)> = None;
                                for (leaf8, (mr, mc)) in leaves.iter().zip(leaf_positions) {
                                    let leaf_mi = (mr as usize, mc as usize);
                                    let leaf_ctx = neighbours.partition_ctx_mi(leaf_mi, 8);
                                    enc.symbol(PARTITION_NONE, &mut cdfs.partition_w8[leaf_ctx]);
                                    let (skip, is_inter) = write_inter_frame_leaf8(
                                        &mut enc,
                                        &mut cdfs,
                                        &mut neighbours,
                                        &mut grid,
                                        mi_cols,
                                        mi_rows,
                                        leaf8,
                                        at16,
                                        leaf_mi,
                                        &scan8,
                                        &scan4,
                                        prev_leaf,
                                    )?;
                                    prev_leaf = Some((leaf_mi, skip, is_inter));
                                }
                                // Same write-back-once-from-the-last-leaf rule
                                // as the key frame search's `write_leaf8`
                                // caller (r15): the SUB-grid skip/inter arrays
                                // are otherwise left stale for the next
                                // 16x16 slot.
                                if let Some((_, skip, is_inter)) = prev_leaf {
                                    neighbours.record_inter(at16, SUB, skip, is_inter);
                                }
                            }
                        }
                        continue;
                    }
                };

                let (r, c) = at;
                let has_above = r32 > 0;
                let has_left = c32 > 0;
                let skip_ctx =
                    usize::from(neighbours.above_skip[c]) + usize::from(neighbours.left_skip[r]);
                enc.symbol(usize::from(block.skip), &mut cdfs.skip[skip_ctx]);

                let is_inter = block.inter.is_some();
                let (above_inter, left_inter) =
                    (neighbours.above_inter[c], neighbours.left_inter[r]);
                let ii_ctx = intra_inter_ctx(has_above, has_left, above_inter, left_inter);
                enc.symbol(usize::from(is_inter), &mut cdfs.intra_inter[ii_ctx]);

                let mode_for_tx;
                if let Some(info) = block.inter {
                    let sr_ctx = single_ref_ctx(above_inter || left_inter);
                    enc.symbol(0, &mut cdfs.single_ref[sr_ctx][0]); // p1: forward
                    enc.symbol(0, &mut cdfs.single_ref[sr_ctx][2]); // p3: LAST/LAST2
                    enc.symbol(0, &mut cdfs.single_ref[sr_ctx][3]); // p4: LAST

                    let (mi_row, mi_col) = (r32 as usize * 8, c32 as usize * 8);
                    let stack = find_mv_stack(
                        &grid,
                        mi_row,
                        mi_col,
                        8,
                        8,
                        LAST_FRAME,
                        mi_cols as usize,
                        mi_rows as usize,
                    );

                    let is_new_mv = matches!(info.mode, InterMode::NewMv);
                    if is_new_mv {
                        enc.symbol(0, &mut cdfs.new_mv[stack.new_mv_ctx]); // NEWMV
                    } else {
                        enc.symbol(1, &mut cdfs.new_mv[stack.new_mv_ctx]); // not new
                        enc.symbol(1, &mut cdfs.zero_mv[stack.zero_mv_ctx]); // not zero
                        enc.symbol(0, &mut cdfs.ref_mv[stack.ref_mv_ctx]); // NEARESTMV
                    }
                    // This writer always keeps the first DRL candidate, so the
                    // loop (spec 5.11.24 `read_drl_idx`) only ever runs its
                    // first, breaking iteration.
                    if is_new_mv && stack.entries.len() > 1 {
                        enc.symbol(0, &mut cdfs.drl_mode[stack.drl_ctx[0]]);
                    }
                    let mv = if is_new_mv {
                        write_mv(
                            &mut enc,
                            &mut cdfs.mv_comp,
                            &mut cdfs.mv_joint,
                            info.mv,
                            stack.pred_mv,
                        )?;
                        info.mv
                    } else {
                        stack.nearest_mv
                    };
                    grid.set(
                        mi_row,
                        mi_col,
                        MiInfo {
                            is_inter: true,
                            ref_frame: LAST_FRAME,
                            ref_frame1: None,
                            mv1: None,
                            mv,
                            is_new_mv,
                            size: 8,
                            size_h: 8,
                            is_global_mv0: false,
                            is_global_mv1: false,
                        },
                    );
                    for dr in 0..8 {
                        for dc in 0..8 {
                            if dr == 0 && dc == 0 {
                                continue;
                            }
                            grid.set(
                                mi_row + dr,
                                mi_col + dc,
                                MiInfo {
                                    is_inter: true,
                                    ref_frame: LAST_FRAME,
                                    ref_frame1: None,
                                    mv1: None,
                                    mv,
                                    is_new_mv,
                                    size: 8,
                                    size_h: 8,
                                    is_global_mv0: false,
                                    is_global_mv1: false,
                                },
                            );
                        }
                    }
                    mode_for_tx = 0;
                } else {
                    let mode = usize::from(block.mode);
                    enc.symbol(mode, &mut cdfs.y_mode[SIZE_GROUP_32]);
                    if (V_PRED..=D67_PRED).contains(&mode) {
                        enc.symbol(ANGLE_DELTA_ZERO, &mut cdfs.angle_delta[mode - V_PRED]);
                    }
                    enc.symbol(DC_PRED, &mut cdfs.uv_mode_cfl[mode]);
                    mode_for_tx = mode;
                    // Intra: no vote, but still a coded cell -- mvstack's
                    // extended-scan coverage must see it (module doc).
                    let (mi_row, mi_col) = (r32 as usize * 8, c32 as usize * 8);
                    for dr in 0..8 {
                        for dc in 0..8 {
                            grid.set(
                                mi_row + dr,
                                mi_col + dc,
                                MiInfo {
                                    is_inter: false,
                                    ref_frame: -1,
                                    ref_frame1: None,
                                    mv1: None,
                                    mv: (0, 0),
                                    is_new_mv: false,
                                    size: 8,
                                    size_h: 8,
                                    is_global_mv0: false,
                                    is_global_mv1: false,
                                },
                            );
                        }
                    }
                }

                if block.skip {
                    neighbours.record(at, BLOCK, mode_for_tx, &zero_grids);
                } else {
                    let grids = [
                        level_grid(&block.luma, TX32)?,
                        level_grid(&block.u, TX16)?,
                        level_grid(&block.v, TX16)?,
                    ];
                    write_block_planes(
                        &mut enc,
                        &mut cdfs,
                        if is_inter {
                            &inter_planes
                        } else {
                            &intra_planes
                        },
                        &grids,
                        &[&scan32, &scan16, &scan16],
                        &neighbours.around(at, BLOCK),
                        mode_for_tx,
                    );
                    neighbours.record(at, BLOCK, mode_for_tx, &grids);
                }
                neighbours.record_inter(at, BLOCK, block.skip, is_inter);
            }
        }
    }
    Ok(enc.finish())
}

/// Writes one 16x16 leaf a straddling 32x32 quadrant's [`Quadrant::Split`]
/// splits into: intra (spec `inter_frame_mode_info`'s intra branch, the only
/// one this writer used to code here) or real inter (`is_inter` coded
/// `true`), `NEARESTMV` only -- `NEWMV` at this size is this function's
/// caller's to never build (`Quadrant::Split`'s `BlockCoeffs.inter`) -- same
/// `single_ref`/mv-stack/DRL symbol chain as the whole-32x32 branch above,
/// just at this leaf's own 4x4-mi window (`bw4`/`bh4` of 4, not 8) and coded
/// through [`TxbSet::Luma16Inter`] rather than [`TxbSet::Luma32Inter`]. Its
/// own `PARTITION_NONE` symbol is already written, same contract as
/// [`write_block`].
#[allow(clippy::too_many_arguments)]
fn write_inter_frame_leaf(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    grid: &mut MiGrid,
    mi_cols: u32,
    mi_rows: u32,
    block: &BlockCoeffs,
    at: (usize, usize),
    scan16: &Vec<u16>,
    scan8: &Vec<u16>,
) -> Result<()> {
    if block.inter.is_none() && usize::from(block.mode) >= INTRA_MODES {
        return Err(Error::unsupported(
            "AV1 tile",
            format!(
                "intra mode {} is not one of the thirteen this writer codes",
                block.mode
            ),
        ));
    }
    /// `Y_MODE`'s size group (spec `Size_Group`, `common_data.h`'s
    /// `size_group_lookup[BLOCK_16X16]`) for this leaf's own size.
    const SIZE_GROUP_16: usize = 2;
    /// `Ref_Frame_List`'s `LAST_FRAME` (spec 3): the only reference this
    /// writer's single-reference chain ever names, same as the 32x32 branch.
    const LAST_FRAME: i8 = 1;

    let (r, c) = at;
    let skip_ctx = usize::from(neighbours.above_skip[c]) + usize::from(neighbours.left_skip[r]);
    enc.symbol(usize::from(block.skip), &mut cdfs.skip[skip_ctx]);

    let (has_above, has_left) = (r > 0, c > 0);
    let (above_inter, left_inter) = (neighbours.above_inter[c], neighbours.left_inter[r]);
    let ii_ctx = intra_inter_ctx(has_above, has_left, above_inter, left_inter);
    let is_inter = block.inter.is_some();
    enc.symbol(usize::from(is_inter), &mut cdfs.intra_inter[ii_ctx]);

    let mode_for_tx;
    if let Some(info) = block.inter {
        let sr_ctx = single_ref_ctx(above_inter || left_inter);
        enc.symbol(0, &mut cdfs.single_ref[sr_ctx][0]); // p1: forward
        enc.symbol(0, &mut cdfs.single_ref[sr_ctx][2]); // p3: LAST/LAST2
        enc.symbol(0, &mut cdfs.single_ref[sr_ctx][3]); // p4: LAST

        let (mi_row, mi_col) = (r * SUB_MI as usize, c * SUB_MI as usize);
        let stack = find_mv_stack(
            grid,
            mi_row,
            mi_col,
            SUB_MI as usize,
            SUB_MI as usize,
            LAST_FRAME,
            mi_cols as usize,
            mi_rows as usize,
        );

        let is_new_mv = matches!(info.mode, InterMode::NewMv);
        if is_new_mv {
            enc.symbol(0, &mut cdfs.new_mv[stack.new_mv_ctx]); // NEWMV
        } else {
            enc.symbol(1, &mut cdfs.new_mv[stack.new_mv_ctx]); // not new
            enc.symbol(1, &mut cdfs.zero_mv[stack.zero_mv_ctx]); // not zero
            enc.symbol(0, &mut cdfs.ref_mv[stack.ref_mv_ctx]); // NEARESTMV
        }
        if is_new_mv && stack.entries.len() > 1 {
            enc.symbol(0, &mut cdfs.drl_mode[stack.drl_ctx[0]]);
        }
        let mv = if is_new_mv {
            write_mv(
                enc,
                &mut cdfs.mv_comp,
                &mut cdfs.mv_joint,
                info.mv,
                stack.pred_mv,
            )?;
            info.mv
        } else {
            stack.nearest_mv
        };
        for dr in 0..SUB_MI as usize {
            for dc in 0..SUB_MI as usize {
                grid.set(
                    mi_row + dr,
                    mi_col + dc,
                    MiInfo {
                        is_inter: true,
                        ref_frame: LAST_FRAME,
                        ref_frame1: None,
                        mv1: None,
                        mv,
                        is_new_mv,
                        size: SUB_MI as usize,
                        size_h: SUB_MI as usize,
                        is_global_mv0: false,
                        is_global_mv1: false,
                    },
                );
            }
        }
        mode_for_tx = 0;
    } else {
        let mode = usize::from(block.mode);
        enc.symbol(mode, &mut cdfs.y_mode[SIZE_GROUP_16]);
        if (V_PRED..=D67_PRED).contains(&mode) {
            enc.symbol(ANGLE_DELTA_ZERO, &mut cdfs.angle_delta[mode - V_PRED]);
        }
        enc.symbol(DC_PRED, &mut cdfs.uv_mode_cfl[mode]);
        mode_for_tx = mode;
        // Intra: no vote, but still a coded cell -- mvstack's extended-scan
        // coverage must see it (module doc).
        let (mi_row, mi_col) = (r * SUB_MI as usize, c * SUB_MI as usize);
        for dr in 0..SUB_MI as usize {
            for dc in 0..SUB_MI as usize {
                grid.set(
                    mi_row + dr,
                    mi_col + dc,
                    MiInfo {
                        is_inter: false,
                        ref_frame: -1,
                        ref_frame1: None,
                        mv1: None,
                        mv: (0, 0),
                        is_new_mv: false,
                        size: SUB_MI as usize,
                        size_h: SUB_MI as usize,
                        is_global_mv0: false,
                        is_global_mv1: false,
                    },
                );
            }
        }
    }

    if block.skip {
        let zero_grids = [
            vec![0i32; TX16 * TX16],
            vec![0i32; TX8 * TX8],
            vec![0i32; TX8 * TX8],
        ];
        neighbours.record(at, SUB, mode_for_tx, &zero_grids);
    } else {
        let grids = [
            level_grid(&block.luma, TX16)?,
            level_grid(&block.u, TX8)?,
            level_grid(&block.v, TX8)?,
        ];
        write_block_planes(
            enc,
            cdfs,
            if is_inter {
                &[TxbSet::Luma16Inter, TxbSet::Chroma8, TxbSet::Chroma8]
            } else {
                &[TxbSet::Luma16, TxbSet::Chroma8, TxbSet::Chroma8]
            },
            &grids,
            &[scan16, scan8, scan8],
            &neighbours.around(at, SUB),
            mode_for_tx,
        );
        neighbours.record(at, SUB, mode_for_tx, &grids);
    }
    neighbours.record_inter(at, SUB, block.skip, is_inter);
    Ok(())
}

/// Writes one 8x8 leaf of a straddling 16x16 inter-frame block
/// (lane-av1inter8): its own skip flag, intra/inter choice and (when inter)
/// `NEARESTMV`/`NEWMV` chain, or (when intra) `Y_MODE`, then its own luma and
/// two chroma transform blocks -- coded exactly like
/// [`write_inter_frame_leaf`] but through [`TxbSet::Luma8Inter`]/
/// [`TxbSet::Chroma4`] and at this leaf's own 2x2-mi mv-stack window, reading
/// its skip/intra-inter context from the *enclosing* 16x16 slot's
/// [`Neighbours`] arrays (`outer_at`, in [`SUB`]-grid units) unless
/// `prev_leaf` names this straddling block's own first leaf as the true
/// mi-adjacent neighbour -- the same override [`write_leaf8`] applies to its
/// mode context, needed for the same reason: `Neighbours`' above/left arrays
/// only resolve to [`SUB`] granularity, too coarse for the second leaf of a
/// straddling 16x16 to see the first. `Y_MODE`'s own context (`Size_Group`)
/// is not neighbour-dependent, unlike the key frame path's `kf_y_mode`, so no
/// override is needed there. Hands back this leaf's own skip flag and
/// intra/inter choice, which is what the next leaf (or the caller's final
/// write-back, mirroring `write_leaf8`'s caller) reads.
#[allow(clippy::too_many_arguments)]
fn write_inter_frame_leaf8(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    grid: &mut MiGrid,
    mi_cols: u32,
    mi_rows: u32,
    block: &BlockCoeffs,
    outer_at: (usize, usize),
    leaf_mi: (usize, usize),
    scan8: &Vec<u16>,
    scan4: &Vec<u16>,
    prev_leaf: Option<((usize, usize), bool, bool)>,
) -> Result<(bool, bool)> {
    if block.inter.is_none() && usize::from(block.mode) >= INTRA_MODES {
        return Err(Error::unsupported(
            "AV1 tile",
            format!(
                "intra mode {} is not one of the thirteen this writer codes",
                block.mode
            ),
        ));
    }
    /// `Y_MODE`'s size group (spec `Size_Group`, `common_data.h`'s
    /// `size_group_lookup[BLOCK_8X8]`) for this leaf's own size.
    const SIZE_GROUP_8: usize = 1;
    /// `Ref_Frame_List`'s `LAST_FRAME` (spec 3), same as the 16x16/32x32
    /// branches.
    const LAST_FRAME: i8 = 1;

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
    enc.symbol(usize::from(block.skip), &mut cdfs.skip[skip_ctx]);

    let (has_above, has_left) = (leaf_mi.0 > 0, leaf_mi.1 > 0);
    let ii_ctx = intra_inter_ctx(has_above, has_left, above_inter, left_inter);
    let is_inter = block.inter.is_some();
    enc.symbol(usize::from(is_inter), &mut cdfs.intra_inter[ii_ctx]);

    let mode_for_tx;
    if let Some(info) = block.inter {
        let sr_ctx = single_ref_ctx(above_inter || left_inter);
        enc.symbol(0, &mut cdfs.single_ref[sr_ctx][0]); // p1: forward
        enc.symbol(0, &mut cdfs.single_ref[sr_ctx][2]); // p3: LAST/LAST2
        enc.symbol(0, &mut cdfs.single_ref[sr_ctx][3]); // p4: LAST

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

        let is_new_mv = matches!(info.mode, InterMode::NewMv);
        if is_new_mv {
            enc.symbol(0, &mut cdfs.new_mv[stack.new_mv_ctx]); // NEWMV
        } else {
            enc.symbol(1, &mut cdfs.new_mv[stack.new_mv_ctx]); // not new
            enc.symbol(1, &mut cdfs.zero_mv[stack.zero_mv_ctx]); // not zero
            enc.symbol(0, &mut cdfs.ref_mv[stack.ref_mv_ctx]); // NEARESTMV
        }
        if is_new_mv && stack.entries.len() > 1 {
            enc.symbol(0, &mut cdfs.drl_mode[stack.drl_ctx[0]]);
        }
        let mv = if is_new_mv {
            write_mv(
                enc,
                &mut cdfs.mv_comp,
                &mut cdfs.mv_joint,
                info.mv,
                stack.pred_mv,
            )?;
            info.mv
        } else {
            stack.nearest_mv
        };
        for dr in 0..2 {
            for dc in 0..2 {
                grid.set(
                    mi_row + dr,
                    mi_col + dc,
                    MiInfo {
                        is_inter: true,
                        ref_frame: LAST_FRAME,
                        ref_frame1: None,
                        mv1: None,
                        mv,
                        is_new_mv,
                        size: 2,
                        size_h: 2,
                        is_global_mv0: false,
                        is_global_mv1: false,
                    },
                );
            }
        }
        mode_for_tx = 0;
    } else {
        let mode = usize::from(block.mode);
        enc.symbol(mode, &mut cdfs.y_mode[SIZE_GROUP_8]);
        if (V_PRED..=D67_PRED).contains(&mode) {
            enc.symbol(ANGLE_DELTA_ZERO, &mut cdfs.angle_delta[mode - V_PRED]);
        }
        enc.symbol(DC_PRED, &mut cdfs.uv_mode_cfl[mode]);
        mode_for_tx = mode;
        // Intra: no vote, but still a coded cell -- mvstack's extended-scan
        // coverage must see it (module doc).
        let (mi_row, mi_col) = leaf_mi;
        for dr in 0..2 {
            for dc in 0..2 {
                grid.set(
                    mi_row + dr,
                    mi_col + dc,
                    MiInfo {
                        is_inter: false,
                        ref_frame: -1,
                        ref_frame1: None,
                        mv1: None,
                        mv: (0, 0),
                        is_new_mv: false,
                        size: 2,
                        size_h: 2,
                        is_global_mv0: false,
                        is_global_mv1: false,
                    },
                );
            }
        }
    }

    let planes = if is_inter {
        [TxbSet::Luma8Inter, TxbSet::Chroma4, TxbSet::Chroma4]
    } else {
        [TxbSet::Luma8, TxbSet::Chroma4, TxbSet::Chroma4]
    };
    if block.skip {
        let zero_grids = [
            vec![0i32; TX8 * TX8],
            vec![0i32; TX4 * TX4],
            vec![0i32; TX4 * TX4],
        ];
        neighbours.record_mi(leaf_mi, 8, &zero_grids);
    } else {
        let grids = [
            level_grid(&block.luma, TX8)?,
            level_grid(&block.u, TX4)?,
            level_grid(&block.v, TX4)?,
        ];
        write_block_planes(
            enc,
            cdfs,
            &planes,
            &grids,
            &[scan8, scan4, scan4],
            &neighbours.around_mi(leaf_mi, 8),
            mode_for_tx,
        );
        neighbours.record_mi(leaf_mi, 8, &grids);
    }
    Ok((block.skip, is_inter))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::frame_obu;
    use crate::obu::temporal_delimiter;
    use crate::sequence::sequence_header_obu;
    use ec_av1_syntax::sequence::SequenceHeader;
    use ec_av1_syntax::{
        FrameHeader, FrameType, LoopFilterParams, PRIMARY_REF_NONE, QuantizationParams, TileInfo,
        TxMode,
    };
    use std::io::Write;
    use std::process::{Command, Stdio};

    /// A 64x64 sequence with every tool this writer does not code turned off:
    /// 64x64 superblocks, no CDEF, no loop restoration, no superres, no filter
    /// intra, and no screen content tools (which is what keeps intra block copy
    /// and palette out of the block syntax).
    fn sequence_64() -> SequenceHeader {
        let mut seq = crate::sequence::tests::sample_1080p();
        seq.frame_width_bits = 7;
        seq.frame_height_bits = 7;
        seq.max_frame_width = 64;
        seq.max_frame_height = 64;
        seq.use_128x128_superblock = false;
        seq.enable_filter_intra = false;
        seq.enable_cdef = false;
        seq.enable_restoration = false;
        seq.enable_superres = false;
        seq.seq_force_screen_content_tools = 0;
        seq.seq_force_integer_mv = 0;
        seq
    }

    /// The key frame the tile above belongs to: one tile, quantised so nothing
    /// is lossless, one transform size per block, no in-loop filtering, and no
    /// CDF adaptation (the writer codes against the defaults).
    fn flat_key_frame() -> FrameHeader {
        FrameHeader {
            frame_type: FrameType::Key,
            frame_is_intra: true,
            show_frame: true,
            error_resilient_mode: true,
            disable_cdf_update: true,
            allow_screen_content_tools: false,
            force_integer_mv: true,
            refresh_frame_flags: 0xFF,
            primary_ref_frame: PRIMARY_REF_NONE,
            frame_width: 64,
            frame_height: 64,
            upscaled_width: 64,
            render_width: 64,
            render_height: 64,
            mi_cols: 16,
            mi_rows: 16,
            tile_info: TileInfo {
                uniform_spacing: true,
                cols: 1,
                rows: 1,
                cols_log2: 0,
                rows_log2: 0,
                mi_col_starts: vec![0, 16],
                mi_row_starts: vec![0, 16],
                context_update_tile_id: 0,
                tile_size_bytes: 1,
            },
            quantization: QuantizationParams {
                base_q_idx: 100,
                ..QuantizationParams::default()
            },
            loop_filter: LoopFilterParams::default(),
            tx_mode: TxMode::Largest,
            reduced_tx_set: false,
            ..FrameHeader::default()
        }
    }

    fn have_ffmpeg() -> bool {
        Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Decodes an AV1 OBU stream with ffmpeg and hands back the planes.
    fn ffmpeg_decode(stream: &[u8], w: usize, h: usize) -> Vec<u8> {
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
        assert_eq!(
            out.stdout.len(),
            w * h * 3 / 2,
            "expected one 4:2:0 frame, ffmpeg said: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    #[test]
    fn flat_key_frame_decodes_to_mid_grey() {
        if !have_ffmpeg() {
            eprintln!("SKIP flat_key_frame_decodes_to_mid_grey: no ffmpeg on PATH");
            return;
        }
        let seq = sequence_64();
        let header = flat_key_frame();
        let tile = flat_key_frame_tile(header.mi_cols, header.mi_rows).unwrap();

        let mut stream = temporal_delimiter();
        stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
        stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());

        let planes = ffmpeg_decode(&stream, 64, 64);
        // A DC prediction with no neighbour to average is the middle of the
        // range, and a skipped block adds no residual, so every sample of every
        // plane is 128 — one wrong symbol anywhere in the tile shows up here.
        for (i, &s) in planes.iter().enumerate() {
            assert_eq!(s, 128, "sample {i} of the decoded frame");
        }
    }

    /// The one q-context whose CDFs this crate carries; the decoded table
    /// below is pinned at this quantiser and nowhere else.
    const Q_IDX: u8 = 100;

    /// Encode a 64x64 key frame carrying `dc_level` and hand back its planes.
    fn decode_dc_frame(dc_level: i32) -> Vec<u8> {
        let seq = sequence_64();
        let mut header = flat_key_frame();
        header.quantization.base_q_idx = Q_IDX;
        let tile = dc_key_frame_tile(header.mi_cols, header.mi_rows, Q_IDX, dc_level).unwrap();
        let mut stream = temporal_delimiter();
        stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
        stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
        ffmpeg_decode(&stream, 64, 64)
    }

    /// What a reference decoder makes of each DC level at `base_q_idx` 100:
    /// the level times the DC quantiser, inverse-transformed over a whole
    /// 64x64 block — which spreads one coefficient over 4096 samples, so a
    /// level moves the picture by a sample or two, not by tens. These numbers
    /// are pinned from the decoder rather than derived: this crate has no
    /// inverse transform of its own yet to derive them with. What the test
    /// asserts around them — flat planes, untouched chroma, monotone in the
    /// level, and the sign going the right way — is derived, and it is what a
    /// desync in the coefficient syntax breaks first.
    const DECODED_AT_Q100: [(i32, u8); 28] = [
        (1, 128),
        (2, 128),
        (3, 129),
        (4, 129),
        (5, 129),
        (6, 129),
        (7, 129),
        (8, 129),
        (9, 130),
        (10, 130),
        (11, 130),
        (12, 130),
        (13, 130),
        (14, 131),
        (-1, 128),
        (-2, 128),
        (-3, 128),
        (-4, 127),
        (-5, 127),
        (-6, 127),
        (-7, 127),
        (-8, 127),
        (-9, 126),
        (-10, 126),
        (-11, 126),
        (-12, 126),
        (-13, 126),
        (-14, 126),
    ];

    #[test]
    fn a_dc_coefficient_moves_the_whole_block_off_mid_grey() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_dc_coefficient_moves_the_whole_block_off_mid_grey: no ffmpeg");
            return;
        }
        let mut seen = Vec::new();
        for (level, want) in DECODED_AT_Q100 {
            let planes = decode_dc_frame(level);
            let (luma, chroma) = planes.split_at(64 * 64);
            for (i, &sample) in luma.iter().enumerate() {
                assert_eq!(sample, want, "luma sample {i} at dc level {level}");
            }
            // The chroma transform blocks are all-zero, so both planes stay at
            // the prediction. A desync in the luma coefficient syntax lands
            // here first: the chroma flags are the next symbols after it.
            for (i, &sample) in chroma.iter().enumerate() {
                assert_eq!(sample, 128, "chroma sample {i} at dc level {level}");
            }
            seen.push((level, want));
        }
        // The picture moves off mid-grey the way the level says: up for a
        // positive level, down for a negative one, and never back towards it
        // as the level grows.
        for w in seen.windows(2) {
            let ((prev_level, prev), (level, value)) = (w[0], w[1]);
            if prev_level.signum() != level.signum() {
                continue;
            }
            if level > 0 {
                assert!(
                    value >= prev,
                    "level {level} decoded below level {prev_level}"
                );
                assert!(value >= 128, "a positive level darkened the picture");
            } else {
                assert!(
                    value <= prev,
                    "level {level} decoded above level {prev_level}"
                );
                assert!(value <= 128, "a negative level brightened the picture");
            }
        }
        assert!(
            DECODED_AT_Q100.iter().any(|&(_, v)| v > 128)
                && DECODED_AT_Q100.iter().any(|&(_, v)| v < 128),
            "the pinned table has to move the picture both ways"
        );
    }

    /// Encode a grid of 64x64 superblocks, each carrying its own DC level, and
    /// hand back the decoded planes.
    fn decode_level_grid(levels: &[i32], sb_cols: u32, sb_rows: u32) -> Vec<u8> {
        let (w, h) = (64 * sb_cols, 64 * sb_rows);
        let (seq, mut header) = frame_of(w, h);
        // The DC fixture writers code against the default CDFs and never
        // update them, so their frames say so.
        header.disable_cdf_update = true;
        let tile = dc_key_frame_tile_levels(header.mi_cols, header.mi_rows, Q_IDX, levels).unwrap();
        let mut stream = temporal_delimiter();
        stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
        stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
        ffmpeg_decode(&stream, w as usize, h as usize)
    }

    /// A sequence and key frame header for a `w` by `h` frame of whole
    /// superblocks.
    fn frame_of(w: u32, h: u32) -> (SequenceHeader, FrameHeader) {
        let mut seq = sequence_64();
        seq.max_frame_width = w;
        seq.max_frame_height = h;
        let mut header = flat_key_frame();
        // Every tile these headers carry is written by the adapting writer.
        header.disable_cdf_update = false;
        header.frame_width = w;
        header.frame_height = h;
        header.upscaled_width = w;
        header.render_width = w;
        header.render_height = h;
        header.mi_cols = w / 4;
        header.mi_rows = h / 4;
        header.tile_info.mi_col_starts = vec![0, header.mi_cols];
        header.tile_info.mi_row_starts = vec![0, header.mi_rows];
        header.quantization.base_q_idx = Q_IDX;
        (seq, header)
    }

    fn decoded_value(level: i32) -> u8 {
        DECODED_AT_Q100
            .iter()
            .find(|&&(l, _)| l == level)
            .map(|&(_, v)| v)
            .expect("the level is in the pinned table")
    }

    /// `dc_predict` (spec 7.11.2) for the flat case: a block whose neighbours
    /// are themselves flat predicts their average, and predicts mid-grey with
    /// no neighbour at all. Every block here is a whole 64x64 superblock, so
    /// the above row and the left column weigh the same.
    fn dc_prediction(above: Option<u8>, left: Option<u8>) -> u8 {
        match (above, left) {
            (None, None) => 128,
            (Some(a), None) => a,
            (None, Some(l)) => l,
            (Some(a), Some(l)) => ((u32::from(a) * 64 + u32::from(l) * 64 + 64) >> 7) as u8,
        }
    }

    /// What a DC level adds to the prediction: the pinned table is that sum
    /// against a mid-grey prediction, and the residual does not depend on what
    /// it is added to.
    fn dc_residual(level: i32) -> i32 {
        i32::from(decoded_value(level)) - 128
    }

    /// Every block reads its DC sign context off the coded blocks above and to
    /// its left, and the three ways that can land — no coded neighbour, the
    /// neighbours leaning one way, the neighbours cancelling — are only
    /// reachable in a frame whose levels differ in sign. Getting the context
    /// wrong desyncs the arithmetic decoder, so the check is that every block
    /// still decodes to the grey its own level asks for.
    #[test]
    fn each_superblock_decodes_to_the_grey_its_own_level_asks_for() {
        if !have_ffmpeg() {
            eprintln!("SKIP each_superblock_decodes_to_the_grey_its_own_level_asks_for: no ffmpeg");
            return;
        }
        // Read in raster order the two grids put a positive, a negative and a
        // cancelling sign context in front of the bottom-right block, and a
        // leaning-down one in front of the second grid's right and bottom
        // blocks.
        for levels in [[14, 3, -3, -14], [-14, -3, 3, 14]] {
            let planes = decode_level_grid(&levels, 2, 2);
            let (luma, chroma) = planes.split_at(128 * 128);
            // The blocks are not independent: a DC prediction reads the
            // reconstructed neighbours, so each block's grey is its
            // neighbours' average plus its own residual.
            let mut recon = [0u8; 4];
            for (block, level) in levels.iter().enumerate() {
                let (br, bc) = (block / 2, block % 2);
                let above = (br > 0).then(|| recon[block - 2]);
                let left = (bc > 0).then(|| recon[block - 1]);
                let want = (i32::from(dc_prediction(above, left)) + dc_residual(*level))
                    .clamp(0, 255) as u8;
                recon[block] = want;
                for y in 0..64 {
                    for x in 0..64 {
                        let i = (br * 64 + y) * 128 + bc * 64 + x;
                        assert_eq!(
                            luma[i], want,
                            "luma at ({x}, {y}) of the block carrying level {level} in {levels:?}"
                        );
                    }
                }
            }
            for (i, &sample) in chroma.iter().enumerate() {
                assert_eq!(sample, 128, "chroma sample {i} of {levels:?}");
            }
        }
    }

    /// What a DC level adds to a 32x32 block's prediction at `base_q_idx` 100,
    /// pinned from the decoder the way [`DECODED_AT_Q100`] is. A 32x32
    /// transform spreads its DC over a quarter of the samples a 64x64 one
    /// does, so the same level moves the picture further: level 14 is five
    /// sample values here against three there.
    const SPLIT_RESIDUAL_AT_Q100: [(i32, i32); 28] = [
        (1, 0),
        (2, 1),
        (3, 1),
        (4, 1),
        (5, 2),
        (6, 2),
        (7, 3),
        (8, 3),
        (9, 3),
        (10, 4),
        (11, 4),
        (12, 4),
        (13, 5),
        (14, 5),
        (-1, 0),
        (-2, -1),
        (-3, -1),
        (-4, -1),
        (-5, -2),
        (-6, -2),
        (-7, -2),
        (-8, -3),
        (-9, -3),
        (-10, -4),
        (-11, -4),
        (-12, -4),
        (-13, -5),
        (-14, -5),
    ];

    fn split_residual(level: i32) -> i32 {
        SPLIT_RESIDUAL_AT_Q100
            .iter()
            .find(|&&(l, _)| l == level)
            .map(|&(_, r)| r)
            .expect("the level is in the pinned table")
    }

    /// Splitting a superblock puts three more syntax elements in front of every
    /// block — the split partition itself, the 32x32 partition below it and a
    /// chroma mode from the wider table CFL-capable blocks read — and moves the
    /// coefficients onto the 32x32 CDFs. Getting any of them wrong desyncs the
    /// arithmetic decoder, and the check is that all sixteen blocks still land
    /// on the grey their own level and their neighbours ask for.
    #[test]
    fn a_split_superblock_decodes_each_quadrant_on_its_own_level() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_split_superblock_decodes_each_quadrant_on_its_own_level: no ffmpeg");
            return;
        }
        // Four by four 32x32 blocks over two by two superblocks, so the walk
        // crosses superblock boundaries in both directions, and signs that
        // alternate so the DC sign context lands on all three of its values.
        let levels: [i32; 16] = [14, -3, 5, -5, -7, 7, -14, 3, 2, -2, 9, -9, -11, 11, -1, 1];
        let planes = decode_split_grid(&levels, 2, 2);
        let (luma, chroma) = planes.split_at(128 * 128);

        let mut recon = [0i32; 16];
        for (block, level) in levels.iter().enumerate() {
            let (br, bc) = (block / 4, block % 4);
            let above = (br > 0).then(|| recon[block - 4] as u8);
            let left = (bc > 0).then(|| recon[block - 1] as u8);
            let want =
                (i32::from(dc_prediction(above, left)) + split_residual(*level)).clamp(0, 255);
            recon[block] = want;
            for y in 0..32 {
                for x in 0..32 {
                    let i = (br * 32 + y) * 128 + bc * 32 + x;
                    assert_eq!(
                        i32::from(luma[i]),
                        want,
                        "luma at ({x}, {y}) of the block carrying level {level} at row {br}, \
                         column {bc}"
                    );
                }
            }
        }
        for (i, &sample) in chroma.iter().enumerate() {
            assert_eq!(sample, 128, "chroma sample {i}");
        }
    }

    /// Encode a grid of 32x32 blocks, each carrying its own DC level.
    fn decode_split_grid(levels: &[i32], sb_cols: u32, sb_rows: u32) -> Vec<u8> {
        let (w, h) = (64 * sb_cols, 64 * sb_rows);
        let (seq, mut header) = frame_of(w, h);
        // The DC fixture writers code against the default CDFs and never
        // update them, so their frames say so.
        header.disable_cdf_update = true;
        let tile = split_dc_key_frame_tile(header.mi_cols, header.mi_rows, Q_IDX, levels).unwrap();
        let mut stream = temporal_delimiter();
        stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
        stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
        ffmpeg_decode(&stream, w as usize, h as usize)
    }

    #[test]
    fn a_level_grid_that_does_not_cover_the_frame_is_refused() {
        assert!(split_dc_key_frame_tile(16, 16, 100, &[3, 3, 3]).is_err());
        assert!(split_dc_key_frame_tile(16, 16, 100, &[3, 3, 3, 0]).is_err());
        assert!(dc_key_frame_tile_levels(32, 32, 100, &[3, 3]).is_err());
        assert!(dc_key_frame_tile_levels(32, 32, 100, &[3, 3, 3, 3, 3]).is_err());
        assert!(dc_key_frame_tile_levels(32, 32, 100, &[3, 3, 0, 3]).is_err());
    }

    #[test]
    fn dc_levels_the_base_syntax_cannot_carry_are_refused() {
        assert!(dc_key_frame_tile(16, 16, 100, 0).is_err());
        assert!(dc_key_frame_tile(16, 16, 100, 15).is_err());
        assert!(dc_key_frame_tile(16, 16, 100, -15).is_err());
    }

    #[test]
    fn partial_superblocks_are_refused() {
        assert!(flat_key_frame_tile(16, 20).is_err());
        assert!(flat_key_frame_tile(0, 16).is_err());
    }

    /// Encodes a 64x64 key frame whose four 32x32 blocks carry `blocks` and
    /// hands back the luma plane as rows of samples.
    fn decode_coeff_quadrants(blocks: &[Vec<Coeff>]) -> Vec<Vec<u8>> {
        let blocks: Vec<BlockCoeffs> = blocks.iter().cloned().map(BlockCoeffs::from).collect();
        let [luma, u, v] = decode_coeff_planes(&blocks);
        for (name, plane) in [("U", &u), ("V", &v)] {
            for (y, row) in plane.iter().enumerate() {
                for (x, &s) in row.iter().enumerate() {
                    assert_eq!(
                        s, 128,
                        "{name} sample ({x},{y}): no block codes a chroma coefficient, and a \
                         DC prediction with no coded neighbour is mid-grey"
                    );
                }
            }
        }
        luma
    }

    /// Encodes a 64x64 key frame whose four 32x32 blocks carry `blocks` and
    /// hands back its three planes as rows of samples: 64 rows of luma and, at
    /// 4:2:0, 32 rows of each chroma plane.
    fn decode_coeff_planes(blocks: &[BlockCoeffs]) -> [Vec<Vec<u8>>; 3] {
        let (seq, header) = frame_of(64, 64);
        let tile =
            split_coeff_key_frame_tile(header.mi_cols, header.mi_rows, Q_IDX, blocks).unwrap();
        let mut stream = temporal_delimiter();
        stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
        stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
        let planes = ffmpeg_decode(&stream, 64, 64);
        let (luma, chroma) = planes.split_at(64 * 64);
        let (u, v) = chroma.split_at(32 * 32);
        [
            luma.chunks_exact(64).map(<[u8]>::to_vec).collect(),
            u.chunks_exact(32).map(<[u8]>::to_vec).collect(),
            v.chunks_exact(32).map(<[u8]>::to_vec).collect(),
        ]
    }

    /// The top-left 32x32 block of a decoded frame, which is the only one whose
    /// prediction has no neighbour to lean on.
    fn top_left_block(rows: &[Vec<u8>]) -> Vec<Vec<u8>> {
        rows[..32].iter().map(|r| r[..32].to_vec()).collect()
    }

    /// A transform's basis functions all average to zero except the DC's, so a
    /// block carrying no DC decodes to samples that average to its prediction —
    /// mid-grey for the block with no neighbours. Rounding of the inverse
    /// transform moves the average by less than a sample.
    fn assert_mean_is_mid_grey(block: &[Vec<u8>]) {
        let sum: i32 = block.iter().flatten().map(|&s| i32::from(s)).sum();
        let mean = f64::from(sum) / (32.0 * 32.0);
        assert!(
            (mean - 128.0).abs() <= 1.0,
            "a block with no DC coefficient averages to its mid-grey prediction, got {mean}"
        );
    }

    /// A coefficient at row 0, column 1 selects the basis function that is flat
    /// down the block and a half-cycle of a cosine across it. Nothing about
    /// that shape is pinned here: every row of the block must be the same row,
    /// the samples must fall from left to right and actually move, and the
    /// block must still average to its prediction. A desync in the scan, the
    /// end-of-block position or the level contexts breaks one of those.
    #[test]
    fn a_coefficient_off_the_origin_selects_its_own_basis_function() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_coefficient_off_the_origin_selects_its_own_basis_function: no ffmpeg"
            );
            return;
        }
        let blocks = vec![
            vec![Coeff {
                row: 0,
                col: 1,
                level: 12,
            }],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ];
        let rows = decode_coeff_quadrants(&blocks);
        let block = top_left_block(&rows);

        for (y, row) in block.iter().enumerate() {
            assert_eq!(row, &block[0], "row {y} of a horizontal basis function");
        }
        assert!(
            block[0][0] > block[0][31],
            "a positive coefficient leans the block towards its left edge: {:?}",
            block[0]
        );
        for x in 1..32 {
            assert!(
                block[0][x] <= block[0][x - 1],
                "the half-cycle falls from left to right, broke at column {x}: {:?}",
                block[0]
            );
        }
        assert_mean_is_mid_grey(&block);

        // The same coefficient transposed selects the basis function that is
        // flat across the block and a half-cycle down it.
        let blocks = vec![
            vec![Coeff {
                row: 1,
                col: 0,
                level: 12,
            }],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ];
        let rows = decode_coeff_quadrants(&blocks);
        let block = top_left_block(&rows);
        for (y, row) in block.iter().enumerate() {
            for (x, &s) in row.iter().enumerate() {
                assert_eq!(s, row[0], "sample ({y},{x}) of a vertical basis function");
            }
        }
        assert!(
            block[0][0] > block[31][0],
            "a positive coefficient leans the block up"
        );
        for y in 1..32 {
            assert!(
                block[y][0] <= block[y - 1][0],
                "the half-cycle falls from top to bottom, broke at row {y}"
            );
        }
        assert_mean_is_mid_grey(&block);
    }

    /// A coefficient late in the scan pushes the end-of-block position into a
    /// group that carries an offset — the far corner of a 32x32 transform is
    /// the last of the eleven groups, whose offset is nine bits wide, one of
    /// them from a CDF and the rest raw. Every coefficient before it is coded
    /// too, so this also walks the base and base-range contexts over a block
    /// with neighbours that carry levels rather than zeros.
    #[test]
    fn a_coefficient_in_the_far_corner_reaches_the_last_end_of_block_group() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_coefficient_in_the_far_corner_reaches_the_last_end_of_block_group");
            return;
        }
        let block = vec![
            Coeff {
                row: 31,
                col: 31,
                level: 5,
            },
            Coeff {
                row: 5,
                col: 7,
                level: -9,
            },
            Coeff {
                row: 1,
                col: 1,
                level: 14,
            },
            Coeff {
                row: 0,
                col: 2,
                level: -3,
            },
            Coeff {
                row: 2,
                col: 0,
                level: 2,
            },
            Coeff {
                row: 1,
                col: 2,
                level: -1,
            },
        ];
        let rows = decode_coeff_quadrants(&[block, Vec::new(), Vec::new(), Vec::new()]);
        let block = top_left_block(&rows);
        assert_mean_is_mid_grey(&block);
        assert!(
            block.iter().flatten().any(|&s| s != block[0][0]),
            "six coefficients do not decode to a flat block"
        );
        // The blocks that carry nothing are flat: their prediction is whatever
        // their neighbours left, and they add no residual to it.
        for (name, ys, xs) in [("top right", 0..32, 32..64), ("bottom left", 32..64, 0..32)] {
            let first = rows[ys.start][xs.start];
            for y in ys {
                for x in xs.clone() {
                    assert_eq!(rows[y][x], first, "the {name} block carries no coefficient");
                }
            }
        }
    }

    /// The price the mode search pays for a block's levels has to be the price
    /// the writer charges for them, or the search is ranking modes by a
    /// fiction. Both halves are asked for the same grids under the same
    /// neutral contexts, and the priced bits are compared against the bytes
    /// the writer really spends.
    #[test]
    fn the_priced_coefficient_bits_match_the_bytes_written() {
        let scan = default_scan(TX32);
        // Three grids that reach different parts of the syntax: a lone DC, a
        // sparse spread of small levels with both signs, and levels past the
        // base-range alphabet so the Golomb tail runs.
        let mut grids = vec![vec![0i32; TX32 * TX32]; 3];
        grids[0][0] = -7;
        for (i, &pos) in scan.iter().enumerate().take(400) {
            if i % 7 == 0 {
                grids[1][pos as usize] = if i % 2 == 0 { 2 } else { -1 };
            }
        }
        grids[2][0] = 40;
        for (i, &pos) in scan.iter().enumerate().take(64) {
            grids[2][pos as usize] += i as i32 % 23 - 11;
        }

        // A single block is too short for the coder's own flush to average
        // out, so all three go through one encoder and one flush.
        let mut enc = SymbolEncoder::new();
        let mut priced = 0.0;
        for grid in &grids {
            write_coeffs(
                &mut enc,
                &mut Cdfs::new(2).txb(TxbSet::Luma32, 0),
                grid,
                &scan,
                0,
                0,
                None,
            );
            priced += luma_32_coeff_bits(grid);
        }
        let spent = enc.finish().len() as f64 * 8.0;
        assert!(
            (priced - spent).abs() / spent < 0.02,
            "priced {priced} bits, wrote {spent}"
        );
    }

    /// A rate term the search can read as constant is a rate term that ranks
    /// nothing, so the price is checked to move the way the levels move: an
    /// empty block is the cheapest thing there is, one coefficient costs more
    /// than none, and both spreading the coefficients and growing them costs
    /// more again.
    #[test]
    fn the_price_grows_with_the_levels() {
        let grid = |f: &dyn Fn(&mut Vec<i32>)| {
            let mut g = vec![0i32; TX32 * TX32];
            f(&mut g);
            luma_32_coeff_bits(&g)
        };
        let empty = grid(&|_| {});
        let one = grid(&|g| g[0] = 1);
        let bigger = grid(&|g| g[0] = 30);
        let spread = grid(&|g| {
            for i in 0..TX32 {
                g[i * TX32 + i] = 1;
            }
        });
        assert!(empty < one, "{empty} {one}");
        assert!(one < bigger, "{one} {bigger}");
        assert!(one < spread, "{one} {spread}");
        // An empty block is one symbol -- an expensive one, because a block
        // with no coefficients at all is the rarer of the two under the
        // neutral context -- and nothing else.
        assert!(
            empty < 6.0,
            "an empty block is one skip symbol, not {empty}"
        );
    }

    /// Neighbouring blocks that each carry coefficients read their DC sign
    /// context, their prediction and their own contexts off each other, so a
    /// frame whose four blocks all carry mixed-sign coefficients exercises what
    /// a single coded block cannot. The check is that it decodes at all — a
    /// desync anywhere fails the decode or the frame size — and that the block
    /// with no neighbours still averages to mid-grey.
    #[test]
    fn every_quadrant_carries_its_own_coefficients() {
        if !have_ffmpeg() {
            eprintln!("SKIP every_quadrant_carries_its_own_coefficients: no ffmpeg");
            return;
        }
        let blocks = vec![
            vec![
                Coeff {
                    row: 0,
                    col: 1,
                    level: 7,
                },
                Coeff {
                    row: 3,
                    col: 4,
                    level: -14,
                },
            ],
            vec![
                Coeff {
                    row: 0,
                    col: 0,
                    level: -6,
                },
                Coeff {
                    row: 2,
                    col: 2,
                    level: 9,
                },
            ],
            vec![
                Coeff {
                    row: 0,
                    col: 0,
                    level: 6,
                },
                Coeff {
                    row: 1,
                    col: 0,
                    level: -2,
                },
                Coeff {
                    row: 8,
                    col: 9,
                    level: 3,
                },
            ],
            vec![
                Coeff {
                    row: 0,
                    col: 0,
                    level: 11,
                },
                Coeff {
                    row: 16,
                    col: 1,
                    level: -4,
                },
            ],
        ];
        let rows = decode_coeff_quadrants(&blocks);
        assert_mean_is_mid_grey(&top_left_block(&rows));
        for (name, ys, xs) in [
            ("top right", 0..32, 32..64),
            ("bottom left", 32..64, 0..32),
            ("bottom right", 32..64, 32..64),
        ] {
            let block: Vec<Vec<u8>> = rows[ys].iter().map(|r| r[xs.clone()].to_vec()).collect();
            assert!(
                block.iter().flatten().any(|&s| s != block[0][0]),
                "the {name} block carries coefficients, so it is not flat"
            );
        }
    }

    /// The top-left 16x16 block of a decoded chroma plane, which is the only
    /// one whose prediction has no neighbour to lean on.
    fn top_left_chroma(rows: &[Vec<u8>]) -> Vec<Vec<u8>> {
        rows[..16].iter().map(|r| r[..16].to_vec()).collect()
    }

    /// The mean of a decoded block, which every basis function but the DC's
    /// leaves at the block's prediction.
    fn mean(block: &[Vec<u8>]) -> f64 {
        let sum: i32 = block.iter().flatten().map(|&s| i32::from(s)).sum();
        let count = block.iter().map(Vec::len).sum::<usize>() as f64;
        f64::from(sum) / count
    }

    /// A chroma plane's coefficients ride their own transform, their own CDFs
    /// and their own end-of-block alphabet, so a chroma basis function is the
    /// gate on all three: U carries the coefficient that is flat down the block
    /// and a half-cycle across it, V the one turned a quarter turn, and neither
    /// may reach the other plane or luma.
    #[test]
    fn each_chroma_plane_codes_its_own_basis_function() {
        if !have_ffmpeg() {
            eprintln!("SKIP each_chroma_plane_codes_its_own_basis_function: no ffmpeg");
            return;
        }
        let mut blocks = vec![BlockCoeffs::default(); 4];
        blocks[0].u = vec![Coeff {
            row: 0,
            col: 1,
            level: 12,
        }];
        blocks[0].v = vec![Coeff {
            row: 1,
            col: 0,
            level: 12,
        }];
        let [luma, u, v] = decode_coeff_planes(&blocks);

        for (y, row) in luma.iter().enumerate() {
            for (x, &s) in row.iter().enumerate() {
                assert_eq!(
                    s, 128,
                    "luma sample ({x},{y}): no block codes a luma coefficient, so the picture \
                     stays at its mid-grey prediction"
                );
            }
        }

        let u_block = top_left_chroma(&u);
        for (y, row) in u_block.iter().enumerate() {
            assert_eq!(
                row, &u_block[0],
                "row {y} of a horizontal chroma basis function"
            );
        }
        assert!(
            u_block[0][0] > u_block[0][15],
            "a positive coefficient leans the U block towards its left edge: {:?}",
            u_block[0]
        );

        let v_block = top_left_chroma(&v);
        for (x, &top) in v_block[0].iter().enumerate() {
            assert_eq!(
                top, v_block[0][0],
                "column {x} of a vertical chroma basis function"
            );
            let column: Vec<u8> = v_block.iter().map(|r| r[x]).collect();
            assert!(
                column[0] > column[15],
                "a positive coefficient leans the V column towards its top edge: {column:?}"
            );
        }

        for (name, block) in [("U", &u_block), ("V", &v_block)] {
            let mean = mean(block);
            assert!(
                (mean - 128.0).abs() <= 1.0,
                "the {name} block carries no DC, so it averages to its mid-grey prediction, \
                 got {mean}"
            );
        }
    }

    /// Every block coding chroma puts the chroma all-zero flag on the contexts
    /// a coded neighbour above and to the left select, and the DC sign on the
    /// contexts their signs select. A frame whose four blocks all carry chroma
    /// DCs of mixed sign walks those contexts; the picture that comes back must
    /// still lean the way each block's own DC asks.
    #[test]
    fn chroma_dc_signs_read_their_neighbours() {
        if !have_ffmpeg() {
            eprintln!("SKIP chroma_dc_signs_read_their_neighbours: no ffmpeg");
            return;
        }
        let dc = |level| {
            vec![Coeff {
                row: 0,
                col: 0,
                level,
            }]
        };
        let signs = [8, -8, -8, 8];
        let blocks: Vec<BlockCoeffs> = signs
            .iter()
            .map(|&level| BlockCoeffs {
                luma: Vec::new(),
                u: dc(level),
                v: dc(-level),
                ..BlockCoeffs::default()
            })
            .collect();
        let [luma, u, v] = decode_coeff_planes(&blocks);

        for (y, row) in luma.iter().enumerate() {
            for (x, &s) in row.iter().enumerate() {
                assert_eq!(s, 128, "luma sample ({x},{y}) with no luma coefficient");
            }
        }

        // A DC prediction leans on the neighbours a block has, so each block is
        // measured against the prediction it was given rather than mid-grey:
        // the only block with no neighbour is the first, and the rest are read
        // as a step away from the block above them and to their left.
        for (i, &level) in signs.iter().enumerate() {
            let (r, c) = (i / 2, i % 2);
            let block = |plane: &[Vec<u8>]| -> Vec<Vec<u8>> {
                plane[r * 16..r * 16 + 16]
                    .iter()
                    .map(|row| row[c * 16..c * 16 + 16].to_vec())
                    .collect()
            };
            for (name, plane, want) in [("U", &u, level), ("V", &v, -level)] {
                let block = block(plane);
                let mean = mean(&block);
                for (y, row) in block.iter().enumerate() {
                    assert_eq!(
                        row.iter().collect::<std::collections::HashSet<_>>().len(),
                        1,
                        "row {y} of a DC-only {name} block is flat"
                    );
                }
                if i == 0 {
                    assert!(
                        (want > 0) == (mean > 128.0),
                        "{name} block {i} carries a DC of {want}, so it must lean off its \
                         mid-grey prediction that way, got {mean}"
                    );
                }
            }
        }
        assert_ne!(u, v, "the two chroma planes carry opposite DCs");
    }

    /// The coefficient writer refuses what it cannot code rather than writing a
    /// stream a decoder walks off the end of.
    /// The 32x32 block at the given block row and column of a decoded plane.
    fn block_at(rows: &[Vec<u8>], block_row: usize, block_col: usize) -> Vec<Vec<u8>> {
        rows[block_row * 32..block_row * 32 + 32]
            .iter()
            .map(|r| r[block_col * 32..block_col * 32 + 32].to_vec())
            .collect()
    }

    /// Encodes a key frame of the given size, whose 32x32 blocks carry
    /// `blocks` in raster order, and hands back its luma plane as rows of
    /// The neighbour context a block reads is gathered across every cell it
    /// spans, not taken from the first one: a 32x32 block whose left-hand
    /// neighbour cells disagree still sees the coded one. Reading a single
    /// cell agreed with the decoder only while every block was the same size,
    /// and desynchronised the arithmetic decoder the moment a 16x16 one
    /// appeared beside a 32x32 one.
    #[test]
    fn a_block_gathers_the_cells_its_neighbours_cover() {
        let mut neighbours = Neighbours::new(4, 4, 64, 64);
        let quiet = [
            vec![0i32; TX16 * TX16],
            vec![0; TX8 * TX8],
            vec![0; TX8 * TX8],
        ];
        let mut loud = quiet.clone();
        loud[1][0] = -3;
        // Two 16x16 blocks above the right-hand 32x32 block of the row, of
        // which only the second codes anything.
        neighbours.record((0, 2), SUB, DC_PRED, &quiet);
        neighbours.record((0, 3), SUB, DC_PRED, &loud);
        let around = neighbours.around((2, 2), BLOCK);
        assert!(
            around[1].above_coded,
            "the coded cell has to reach the block below it"
        );
        assert_eq!(
            dc_sign_ctx(around[1].dc_vote),
            1,
            "the one negative DC above the block is what it votes"
        );
        // The same read one cell at a time misses it, which is the bug this
        // gate is here for. `above[8]` is the first 4x4 unit the quiet block
        // at `(0, 2)` wrote (2 SUB units * 4 4x4-units-per-SUB).
        assert!(!neighbours.above[8][1].coded);
    }

    /// samples.
    fn decode_luma_at(width: usize, height: usize, blocks: &[BlockCoeffs]) -> Vec<Vec<u8>> {
        let (seq, header) = frame_of(width as u32, height as u32);
        let tile =
            split_coeff_key_frame_tile(header.mi_cols, header.mi_rows, Q_IDX, blocks).unwrap();
        let mut stream = temporal_delimiter();
        stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
        stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
        let planes = ffmpeg_decode(&stream, width, height);
        planes[..width * height]
            .chunks_exact(width)
            .map(<[u8]>::to_vec)
            .collect()
    }

    /// Every row of a block is the same row, and that row falls from left to
    /// right — the shape of a coefficient at row 0, column 1.
    fn assert_falls_across(block: &[Vec<u8>], name: &str) {
        for row in block {
            assert_eq!(
                row, &block[0],
                "{name}: every row of the block is the same row"
            );
        }
        assert!(
            block[0][0] > block[0][31],
            "{name}: the row falls from left to right, got {} then {}",
            block[0][0],
            block[0][31]
        );
    }

    /// Every column of a block is constant across it and falls down it — the
    /// shape of a coefficient at row 1, column 0.
    fn assert_falls_down(block: &[Vec<u8>], name: &str) {
        for (y, row) in block.iter().enumerate() {
            assert!(
                row.iter().all(|&s| s == row[0]),
                "{name}: row {y} of the block is one constant"
            );
        }
        assert!(
            block[0][0] > block[31][0],
            "{name}: the column falls down the block, got {} then {}",
            block[0][0],
            block[31][0]
        );
    }

    /// The mean sample of a decoded block.
    fn mean_of(block: &[Vec<u8>]) -> f64 {
        let sum: i32 = block.iter().flatten().map(|&s| i32::from(s)).sum();
        f64::from(sum) / (block.len() * block[0].len()) as f64
    }

    /// The top-left block of a frame whose only coefficient is a luma DC of
    /// `level`.
    fn decode_dc_level_block(level: i32) -> Vec<Vec<u8>> {
        let blocks = vec![
            vec![Coeff {
                row: 0,
                col: 0,
                level,
            }],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ];
        top_left_block(&decode_coeff_quadrants(&blocks))
    }

    /// `read_golomb` (spec 5.11.40), written from the specification's
    /// pseudocode, and the level its caller builds from it: a level above the
    /// base-range tail is fifteen plus what the tail carries.
    fn read_golomb_level(dec: &mut crate::msac::tests::SymbolDecoder) -> u32 {
        let mut length = 0;
        while dec.literal(1) == 0 {
            length += 1;
            assert!(
                length < 20,
                "the tail's length prefix runs past twenty bits"
            );
        }
        let mut x = 1;
        for _ in 0..length {
            x = (x << 1) | dec.literal(1);
        }
        (MAX_BR_LEVEL + 1) as u32 + (x - 1)
    }

    /// The tail says how much of the level the base and base-range syntax could
    /// not carry, and the spec's own reader must hand back the level that was
    /// asked for — not one either side of it. A tail written a bit long, a bit
    /// short, or against the wrong base fails this at the first level it
    /// reaches.
    #[test]
    fn the_golomb_tail_reads_back_as_the_level_it_was_written_for() {
        for level in (MAX_BR_LEVEL + 1..=MAX_BR_LEVEL + 300)
            .chain([MAX_LEVEL - 1, MAX_LEVEL])
            .map(|l| l as u32)
        {
            let mut enc = SymbolEncoder::new();
            write_golomb(&mut enc, level - (MAX_BR_LEVEL + 1) as u32);
            // The tail is raw bits, so it needs no padding beyond what the
            // encoder's own flush writes.
            let data = enc.finish();
            let mut dec = crate::msac::tests::SymbolDecoder::new(&data);
            assert_eq!(
                read_golomb_level(&mut dec),
                level,
                "the tail of level {level}"
            );
        }
    }

    /// The base and base-range syntax reach level fourteen between them;
    /// anything above that carries the rest of itself as a Golomb tail, written
    /// after its own sign. A DC level moves the whole block off its mid-grey
    /// prediction by an amount that grows with the level, so a run of levels
    /// either side of the tail's threshold — spaced wide enough that the
    /// quantiser separates them — must decode to a run of steadily
    /// brighter blocks — and the negatives of the same levels to steadily
    /// darker ones. A tail written with the wrong length, in the wrong place in
    /// the syntax, or off by one desyncs the decoder outright.
    #[test]
    fn levels_above_the_base_range_tail_carry_a_golomb_tail() {
        if !have_ffmpeg() {
            eprintln!("SKIP levels_above_the_base_range_tail_carry_a_golomb_tail: no ffmpeg");
            return;
        }
        let mut brighter = Vec::new();
        let mut darker = Vec::new();
        for level in [10, 14, 18, 31, 60, 400] {
            brighter.push((level, mean_of(&decode_dc_level_block(level))));
            darker.push((level, mean_of(&decode_dc_level_block(-level))));
        }
        for pair in brighter.windows(2) {
            let [(low, dim), (high, bright)] = [pair[0], pair[1]];
            assert!(
                bright > dim,
                "level {high} decodes brighter than level {low}, got {bright} against {dim}"
            );
        }
        for pair in darker.windows(2) {
            let [(low, bright), (high, dim)] = [pair[0], pair[1]];
            assert!(
                dim < bright,
                "level -{high} decodes darker than level -{low}, got {dim} against {bright}"
            );
        }
        let (_, mid) = brighter[0];
        assert!(
            mid > 128.0 && darker[0].1 < 128.0,
            "a positive level brightens its block and a negative one darkens it, got {mid} and {}",
            darker[0].1
        );
    }

    /// A frame need not be a whole number of superblocks. A 96x64 frame is a
    /// superblock and a half across: the second superblock has no right-hand
    /// half, so its partition is a gathered flag rather than the full symbol,
    /// and the two blocks that would sit in that half are never coded at all.
    /// Each of the six blocks that do exist carries one of two basis functions,
    /// alternating, so a block written into the wrong place or a flag the
    /// decoder reads as a different number of bits shows up as a block with the
    /// other block's shape — or as a decode failure.
    fn decode_luma_sb(width: usize, height: usize, sbs: &[Superblock]) -> Vec<Vec<u8>> {
        let (seq, header) = frame_of(width as u32, height as u32);
        let tile = sb_coeff_key_frame_tile(header.mi_cols, header.mi_rows, Q_IDX, sbs).unwrap();
        let mut stream = temporal_delimiter();
        stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
        stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
        let planes = ffmpeg_decode(&stream, width, height);
        planes[..width * height]
            .chunks_exact(width)
            .map(<[u8]>::to_vec)
            .collect()
    }

    /// A superblock left whole carries one 64x64 transform, whose lowest
    /// horizontal basis function falls across the whole sixty-four samples in
    /// one stroke. Were the same superblock split, each half would restart the
    /// gradient at its own left edge and the picture would rise again in the
    /// middle, so the walk across the row is what tells the two apart — and
    /// the split superblock beside it proves the two partitions still agree on
    /// where the next one begins.
    #[test]
    fn a_whole_superblock_covers_all_four_of_its_quadrants() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_whole_superblock_covers_all_four_of_its_quadrants: no ffmpeg");
            return;
        }
        let across = BlockCoeffs::from(vec![Coeff {
            row: 0,
            col: 1,
            level: 20,
        }]);
        let down = BlockCoeffs::from(vec![Coeff {
            row: 1,
            col: 0,
            level: 20,
        }]);
        let sbs = [
            Superblock::Whole(across),
            Superblock::Split(vec![
                Quadrant::Whole(down.clone()),
                Quadrant::Whole(down.clone()),
                Quadrant::Whole(down.clone()),
                Quadrant::Whole(down),
            ]),
        ];
        let rows = decode_luma_sb(128, 64, &sbs);

        for (y, row) in rows.iter().enumerate() {
            assert_eq!(
                &row[..64],
                &rows[0][..64],
                "row {y} of the whole superblock repeats its first row"
            );
        }
        let first = &rows[0][..64];
        assert!(
            first.windows(2).all(|w| w[0] >= w[1]),
            "the whole superblock falls across its sixty-four samples without \
             restarting: {first:?}"
        );
        assert!(
            first[0] > first[63],
            "the whole superblock falls from left to right, got {} then {}",
            first[0],
            first[63]
        );
        for block_row in 0..2 {
            for block_col in 2..4 {
                let block = block_at(&rows, block_row, block_col);
                assert_falls_down(&block, &format!("quadrant ({block_row},{block_col})"));
            }
        }
    }

    /// A quadrant may be split again into four 16x16 blocks, each carrying its
    /// own 16x16 luma transform and 8x8 chroma transforms. A DC level per block
    /// makes each of the four a flat grey of its own, so the quadrant reads as
    /// four squares rather than one — which is what tells the extra split from
    /// a 32x32 block that merely carries the same coefficients.
    #[test]
    fn a_quadrant_splits_into_four_sixteens() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_quadrant_splits_into_four_sixteens: no ffmpeg");
            return;
        }
        let dc = |level: i32| {
            BlockCoeffs::from(vec![Coeff {
                row: 0,
                col: 0,
                level,
            }])
        };
        let levels = [14, 8, -8, -14];
        let flat = BlockCoeffs::from(Vec::new());
        let sbs = [Superblock::Split(vec![
            Quadrant::Split(levels.iter().map(|&l| dc(l)).collect()),
            Quadrant::Whole(flat.clone()),
            Quadrant::Whole(flat.clone()),
            Quadrant::Whole(flat),
        ])];
        let rows = decode_luma_sb(64, 64, &sbs);

        let mut greys = Vec::new();
        for i in 0..levels.len() {
            let (y0, x0) = ((i / 2) * 16, (i % 2) * 16);
            let grey = rows[y0][x0];
            for (y, row) in rows.iter().enumerate().skip(y0).take(16) {
                for (x, &sample) in row.iter().enumerate().skip(x0).take(16) {
                    assert_eq!(
                        sample, grey,
                        "the 16x16 block at ({x0},{y0}) is flat, but ({x},{y}) is not"
                    );
                }
            }
            greys.push(grey);
        }

        // Each block is predicted DC from the blocks the decoder has already
        // rebuilt, so what its own level does is move it off that prediction,
        // in the level's own direction.
        let g = |i: usize| i32::from(greys[i]);
        let predictions = [128, g(0), g(0), (g(1) + g(2) + 1) / 2];
        for (i, (&level, prediction)) in levels.iter().zip(predictions).enumerate() {
            assert_eq!(
                (g(i) - prediction).signum(),
                level.signum(),
                "the 16x16 block {i} carries level {level}, so it sits on the \
                 {} side of the {prediction} its neighbours predict, not at {}",
                if level > 0 { "bright" } else { "dark" },
                g(i)
            );
        }
        assert_eq!(
            greys.iter().collect::<std::collections::HashSet<_>>().len(),
            4,
            "the four blocks are four different greys, not one: {greys:?}"
        );
    }

    /// A superblock half outside the frame has no partition that keeps a block
    /// outside it, so it cannot be left whole.
    #[test]
    fn a_whole_superblock_at_the_frame_edge_is_refused() {
        let block = BlockCoeffs::from(vec![Coeff {
            row: 0,
            col: 0,
            level: 4,
        }]);
        let sbs = [
            Superblock::Split(vec![
                Quadrant::Whole(block.clone()),
                Quadrant::Whole(block.clone()),
                Quadrant::Whole(block.clone()),
                Quadrant::Whole(block.clone()),
            ]),
            Superblock::Whole(block.clone()),
        ];
        // Ninety-six samples across is three 32x32 blocks, so the second
        // superblock hangs half outside the frame.
        let err = sb_coeff_key_frame_tile(24, 16, Q_IDX, &sbs).unwrap_err();
        assert!(
            format!("{err}").contains("half outside the frame"),
            "got {err}"
        );
    }

    #[test]
    fn a_frame_that_is_not_a_whole_number_of_superblocks_codes_every_block() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_frame_that_is_not_a_whole_number_of_superblocks_codes_every_block: no \
                 ffmpeg"
            );
            return;
        }
        let blocks: Vec<BlockCoeffs> = (0..6)
            .map(|i| {
                let (row, col) = if i % 2 == 0 { (0, 1) } else { (1, 0) };
                BlockCoeffs::from(vec![Coeff {
                    row,
                    col,
                    level: 12,
                }])
            })
            .collect();
        let rows = decode_luma_at(96, 64, &blocks);
        for block_row in 0..2 {
            for block_col in 0..3 {
                let name = format!("block ({block_row},{block_col})");
                let block = block_at(&rows, block_row, block_col);
                if (block_row * 3 + block_col) % 2 == 0 {
                    assert_falls_across(&block, &name);
                } else {
                    assert_falls_down(&block, &name);
                }
            }
        }
    }

    /// The transpose of [`a_frame_that_is_not_a_whole_number_of_superblocks_codes_every_block`]:
    /// an odd number of 32x32 block *rows* (superblock hangs off the
    /// *bottom*, `has_cols=true, has_rows=false`) rather than an odd number of
    /// columns (hangs off the *right*, `has_cols=false, has_rows=true`) --
    /// the two halves of the `(has_cols, has_rows)` match in
    /// `sb_coeff_key_frame_tile` gather from different tables
    /// (`VERT_ALIKE`/`HORZ_ALIKE`), and only the right-hand-edge direction had
    /// a real-decoder test before this one.
    #[test]
    fn a_frame_that_hangs_off_the_bottom_codes_every_block() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_frame_that_hangs_off_the_bottom_codes_every_block: no ffmpeg");
            return;
        }
        let blocks: Vec<BlockCoeffs> = (0..6)
            .map(|i| {
                let (row, col) = if i % 2 == 0 { (0, 1) } else { (1, 0) };
                BlockCoeffs::from(vec![Coeff {
                    row,
                    col,
                    level: 12,
                }])
            })
            .collect();
        let rows = decode_luma_at(64, 96, &blocks);
        for block_row in 0..3 {
            for block_col in 0..2 {
                let name = format!("block ({block_row},{block_col})");
                let block = block_at(&rows, block_row, block_col);
                if (block_row * 2 + block_col) % 2 == 0 {
                    assert_falls_across(&block, &name);
                } else {
                    assert_falls_down(&block, &name);
                }
            }
        }
    }

    /// A frame whose blocks do not tile it is refused rather than written as a
    /// stream a decoder walks off the end of.
    #[test]
    fn a_frame_that_is_not_a_whole_number_of_blocks_is_refused() {
        let (_, header) = frame_of(80, 64);
        let blocks = vec![BlockCoeffs::default(); 4];
        let err = split_coeff_key_frame_tile(header.mi_cols, header.mi_rows, Q_IDX, &blocks)
            .expect_err("80 is not a whole number of 32x32 blocks");
        assert!(
            format!("{err}").contains("32x32"),
            "the refusal names the block size, got {err}"
        );
    }

    /// A directional intra mode says where its prediction comes from, and with
    /// no residual of its own a block must be exactly that prediction. The
    /// top-left block is given a basis function that is flat across it and
    /// falls down it; the block to its right is coded `H_PRED`, so every one of
    /// its rows must be the constant its left neighbour's rightmost column
    /// hands it, and the block below is coded `V_PRED`, so every one of its
    /// columns must be the constant the row above it ends on — a single flat
    /// value for the whole block. Nothing here pins the transform's shape or
    /// the mode's CDF: a mode written with the wrong context, an angle delta
    /// left out or a mode index off by one desyncs the symbol decoder and the
    /// two empty blocks stop mirroring their neighbours.
    #[test]
    fn directional_modes_predict_from_the_neighbours_they_name() {
        if !have_ffmpeg() {
            eprintln!("SKIP directional_modes_predict_from_the_neighbours_they_name: no ffmpeg");
            return;
        }
        let gradient = BlockCoeffs {
            luma: vec![Coeff {
                row: 1,
                col: 0,
                level: 12,
            }],
            ..BlockCoeffs::default()
        };
        let blocks = [
            gradient,
            BlockCoeffs {
                mode: H_PRED as u8,
                ..BlockCoeffs::default()
            },
            BlockCoeffs {
                mode: V_PRED as u8,
                ..BlockCoeffs::default()
            },
            BlockCoeffs::default(),
        ];
        let [luma, ..] = decode_coeff_planes(&blocks);

        let source = block_at(&luma, 0, 0);
        for (r, row) in source.iter().enumerate() {
            assert!(
                row.iter().all(|&s| s == row[0]),
                "the basis function at row 1, column 0 is flat across the block, row {r} is not:                  {row:?}"
            );
        }
        let profile: Vec<u8> = source.iter().map(|r| r[0]).collect();
        assert!(
            profile.windows(2).all(|w| w[0] >= w[1]) && profile[0] > profile[31],
            "the block must fall from top to bottom, got {profile:?}"
        );

        let horizontal = block_at(&luma, 0, 1);
        for (r, row) in horizontal.iter().enumerate() {
            assert!(
                row.iter().all(|&s| s == profile[r]),
                "an H_PRED block repeats the column to its left, row {r} should be all {} but is                  {row:?}",
                profile[r]
            );
        }

        let vertical = block_at(&luma, 1, 0);
        let bottom = profile[31];
        assert!(
            vertical.iter().flatten().all(|&s| s == bottom),
            "a V_PRED block repeats the row above it, which is all {bottom} here, got rows \
             {:?} and {:?}",
            vertical[0],
            vertical[31]
        );
    }

    /// The mode index is checked before anything is written.
    #[test]
    fn an_intra_mode_outside_the_key_frame_set_is_refused() {
        let mut blocks = vec![BlockCoeffs::default(); 4];
        blocks[2].mode = INTRA_MODES as u8;
        let message = split_coeff_key_frame_tile(16, 16, Q_IDX, &blocks)
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("intra mode 13"),
            "the refusal must name the mode, got {message}"
        );
    }

    #[test]
    fn coefficients_the_writer_cannot_code_are_refused() {
        let empty = || vec![BlockCoeffs::default(); 4];
        let cases: [(&str, Vec<BlockCoeffs>); 6] = [
            ("one set short", vec![BlockCoeffs::default(); 3]),
            ("off the transform", {
                let mut b = empty();
                b[0].luma = vec![Coeff {
                    row: 32,
                    col: 0,
                    level: 1,
                }];
                b
            }),
            ("a zero level", {
                let mut b = empty();
                b[1].luma = vec![Coeff {
                    row: 0,
                    col: 0,
                    level: 0,
                }];
                b
            }),
            ("a level past the Golomb tail's reach", {
                let mut b = empty();
                b[2].luma = vec![Coeff {
                    row: 0,
                    col: 0,
                    level: MAX_LEVEL + 1,
                }];
                b
            }),
            ("two coefficients at one position", {
                let mut b = empty();
                b[3].luma = vec![
                    Coeff {
                        row: 4,
                        col: 4,
                        level: 1,
                    },
                    Coeff {
                        row: 4,
                        col: 4,
                        level: 2,
                    },
                ];
                b
            }),
            ("off the chroma transform", {
                let mut b = empty();
                b[0].u = vec![Coeff {
                    row: 0,
                    col: 16,
                    level: 1,
                }];
                b
            }),
        ];
        for (name, blocks) in cases {
            assert!(
                split_coeff_key_frame_tile(16, 16, Q_IDX, &blocks).is_err(),
                "{name} must be refused"
            );
        }
    }

    /// The default scan is written as the rule that generates it rather than a
    /// thousand pinned numbers, so the rule is checked against what a scan must
    /// be: every position exactly once, the origin first, and never a position
    /// before one it sits diagonally behind — which is what lets a decoder read
    /// a coefficient's context off the coefficients it has already decoded.
    #[test]
    fn the_default_scan_walks_every_position_outwards() {
        for side in [TX16, TX32] {
            let area = side * side;
            let scan = default_scan(side);
            assert_eq!(scan.len(), area);
            let mut seen = vec![false; area];
            for &pos in &scan {
                assert!(
                    !seen[pos as usize],
                    "position {pos} is scanned twice in {side}x{side}"
                );
                seen[pos as usize] = true;
            }
            assert_eq!(scan[0], 0, "the scan starts at the DC");

            let mut order = vec![0usize; area];
            for (i, &pos) in scan.iter().enumerate() {
                order[pos as usize] = i;
            }
            for row in 0..side {
                for col in 0..side {
                    for (dr, dc) in [(1, 0), (0, 1), (1, 1), (2, 0), (0, 2)] {
                        let (nr, nc) = (row + dr, col + dc);
                        if nr < side && nc < side {
                            assert!(
                                order[nr * side + nc] > order[row * side + col],
                                "({nr},{nc}) is a context neighbour of ({row},{col}), so it must \
                                 be scanned after it"
                            );
                        }
                    }
                }
            }
        }
    }

    /// The whole point of the inverse transform living in the encoder is that
    /// it predicts the decoder without asking one. The residual a DC-only
    /// 64x64 transform produces has already been pinned from a real decoder in
    /// [`DECODED_AT_Q100`], so that table is the gate: every level in it, both
    /// signs, has to come back out of `dequant_and_inverse` exactly.
    #[test]
    fn the_inverse_transform_reproduces_the_pinned_whole_superblock_residuals() {
        for (level, _) in DECODED_AT_Q100 {
            let mut levels = vec![0i32; 64 * 64];
            levels[0] = level;
            let residual = crate::transform::dequant_and_inverse(&levels, 64, 8, i32::from(Q_IDX));
            let want = dc_residual(level);
            assert!(
                residual.iter().all(|&r| r == want),
                "level {level}: want a flat {want}, got {}..{} (first {})",
                residual.iter().min().unwrap(),
                residual.iter().max().unwrap(),
                residual[0]
            );
        }
    }

    /// The same gate at the other transform size the encoder codes: a split
    /// superblock's 32x32 blocks, whose residuals are pinned in
    /// [`SPLIT_RESIDUAL_AT_Q100`]. The two sizes divide the dequantized
    /// coefficient by different denominators, so a size-blind dequantizer
    /// passes one table and fails the other.
    #[test]
    fn the_inverse_transform_reproduces_the_pinned_split_residuals() {
        for (level, want) in SPLIT_RESIDUAL_AT_Q100 {
            let mut levels = vec![0i32; 32 * 32];
            levels[0] = level;
            let residual = crate::transform::dequant_and_inverse(&levels, 32, 8, i32::from(Q_IDX));
            assert!(
                residual.iter().all(|&r| r == want),
                "level {level}: want a flat {want}, got {}..{} (first {})",
                residual.iter().min().unwrap(),
                residual.iter().max().unwrap(),
                residual[0]
            );
        }
    }

    /// A DC-only transform is flat, which is exactly the case that cannot tell
    /// a transposed or mis-permuted butterfly network from a correct one. An AC
    /// coefficient in the first row must vary along the row and stay constant
    /// down each column, and the one in the first column the other way round;
    /// a transposed network swaps the two.
    #[test]
    fn a_single_ac_coefficient_varies_along_its_own_axis() {
        for side in [4, 8, 16, 32, 64] {
            let mut horizontal = vec![0i32; side * side];
            horizontal[1] = 100;
            let h = crate::transform::dequant_and_inverse(&horizontal, side, 8, i32::from(Q_IDX));
            let mut vertical = vec![0i32; side * side];
            vertical[side] = 100;
            let v = crate::transform::dequant_and_inverse(&vertical, side, 8, i32::from(Q_IDX));
            for row in 0..side {
                for col in 0..side {
                    assert_eq!(
                        h[row * side + col],
                        h[col],
                        "{side}: a first-row coefficient is constant down column {col}"
                    );
                    assert_eq!(
                        v[row * side + col],
                        v[row * side],
                        "{side}: a first-column coefficient is constant along row {row}"
                    );
                }
            }
            // The two are each other's transpose. Only to within a rounding
            // step: the spec rounds halves up rather than away from zero, so a
            // basis function and its transpose can land a count apart where
            // the row and column passes round opposite ways.
            assert!(h[0] != h[side - 1], "{side}: the horizontal basis varies");
            for row in 0..side {
                for col in 0..side {
                    let (a, b) = (h[row * side + col], v[col * side + row]);
                    assert!(
                        (a - b).abs() <= 1,
                        "{side}: transpose at ({row},{col}): {a} vs {b}"
                    );
                }
            }
        }
    }

    /// Negating every level negates every residual, to within the one count
    /// the spec's round-halves-up leaves behind. A dequantizer that rounded a
    /// negative coefficient away from zero instead of toward it would shift a
    /// whole basis function, not a count of it, so the bound is what makes
    /// this a test rather than a restatement.
    #[test]
    fn negating_the_levels_negates_the_residual() {
        for side in [32, 64] {
            for level in [1, 2, 3, 5, 9, 14, 40, 100] {
                let mut levels = vec![0i32; side * side];
                levels[0] = level;
                levels[1] = -level;
                levels[side + 1] = level * 2;
                let pos = crate::transform::dequant_and_inverse(&levels, side, 8, i32::from(Q_IDX));
                let negated: Vec<i32> = levels.iter().map(|&l| -l).collect();
                let neg =
                    crate::transform::dequant_and_inverse(&negated, side, 8, i32::from(Q_IDX));
                let worst = pos
                    .iter()
                    .zip(&neg)
                    .map(|(&p, &n)| (p + n).abs())
                    .max()
                    .expect("the block is not empty");
                assert!(worst <= 1, "side {side} level {level}: off by {worst}");
            }
        }
    }

    /// The pinned tables above are DC-only, and a flat residual cannot tell a
    /// correct butterfly network from one that is merely correct at DC. This
    /// is the same claim against a real decoder and with real AC content: the
    /// top-left block of a key frame predicts a flat mid-grey with no
    /// neighbours to read, so what ffmpeg shows there is exactly 128 plus the
    /// residual the encoder's own inverse transform computes. Every sample has
    /// to agree, not most of them.
    #[test]
    fn the_inverse_transform_predicts_what_ffmpeg_decodes_for_a_32x32_block() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP the_inverse_transform_predicts_what_ffmpeg_decodes_for_a_32x32_block: no ffmpeg"
            );
            return;
        }
        // A DC, two low-frequency terms of each orientation, a diagonal and a
        // far corner, with both signs among them.
        let coeffs = [
            (0u8, 0u8, 9i32),
            (0, 1, -14),
            (1, 0, 7),
            (0, 3, 5),
            (3, 0, -3),
            (2, 2, 11),
            (7, 5, -6),
            (31, 31, 4),
        ];
        let block: BlockCoeffs = coeffs
            .iter()
            .map(|&(row, col, level)| Coeff { row, col, level })
            .collect::<Vec<_>>()
            .into();
        let rows = decode_luma_at(
            64,
            64,
            &[
                block,
                BlockCoeffs::default(),
                BlockCoeffs::default(),
                BlockCoeffs::default(),
            ],
        );

        let mut levels = vec![0i32; 32 * 32];
        for &(row, col, level) in &coeffs {
            levels[usize::from(row) * 32 + usize::from(col)] = level;
        }
        let residual = crate::transform::dequant_and_inverse(&levels, 32, 8, i32::from(Q_IDX));
        for row in 0..32 {
            for col in 0..32 {
                let want = (128 + residual[row * 32 + col]).clamp(0, 255);
                assert_eq!(
                    i32::from(rows[row][col]),
                    want,
                    "({row},{col}): ffmpeg decoded {} where the encoder predicted {want}",
                    rows[row][col]
                );
            }
        }
        // The block is not flat, so the agreement is about the transform and
        // not about a prediction both sides got trivially right.
        let (lo, hi) = (
            rows[..32].iter().flat_map(|r| &r[..32]).min().unwrap(),
            rows[..32].iter().flat_map(|r| &r[..32]).max().unwrap(),
        );
        assert!(hi - lo > 4, "the block carries real detail, got {lo}..{hi}");
    }

    /// The same against the smallest transform the writer codes, the 16x16 one
    /// a split quadrant carries. Its rounding is the one the split path rests
    /// on, and nothing else in the crate exercises it against a decoder.
    #[test]
    fn the_inverse_transform_predicts_what_ffmpeg_decodes_for_a_16x16_block() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP the_inverse_transform_predicts_what_ffmpeg_decodes_for_a_16x16_block: no ffmpeg"
            );
            return;
        }
        let coeffs = [
            (0u8, 0u8, 9i32),
            (0, 1, -14),
            (1, 0, 7),
            (0, 3, 5),
            (3, 0, -3),
            (2, 2, 11),
            (7, 5, -6),
            (15, 15, 4),
        ];
        let block: BlockCoeffs = coeffs
            .iter()
            .map(|&(row, col, level)| Coeff { row, col, level })
            .collect::<Vec<_>>()
            .into();
        let (seq, header) = frame_of(64, 64);
        let quadrants = vec![
            Quadrant::Split(vec![
                block,
                BlockCoeffs::default(),
                BlockCoeffs::default(),
                BlockCoeffs::default(),
            ]),
            Quadrant::Whole(BlockCoeffs::default()),
            Quadrant::Whole(BlockCoeffs::default()),
            Quadrant::Whole(BlockCoeffs::default()),
        ];
        let tile = sb_coeff_key_frame_tile(
            header.mi_cols,
            header.mi_rows,
            Q_IDX,
            &[Superblock::Split(quadrants)],
        )
        .unwrap();
        let mut stream = temporal_delimiter();
        stream.extend_from_slice(&sequence_header_obu(&seq).unwrap());
        stream.extend_from_slice(&frame_obu(&seq, &header, &tile).unwrap());
        let planes = ffmpeg_decode(&stream, 64, 64);

        let mut levels = vec![0i32; TX16 * TX16];
        for &(row, col, level) in &coeffs {
            levels[usize::from(row) * TX16 + usize::from(col)] = level;
        }
        let residual = crate::transform::dequant_and_inverse(&levels, TX16, 8, i32::from(Q_IDX));
        for row in 0..TX16 {
            for col in 0..TX16 {
                let want = (128 + residual[row * TX16 + col]).clamp(0, 255);
                assert_eq!(
                    i32::from(planes[row * 64 + col]),
                    want,
                    "({row},{col}): ffmpeg decoded {} where the encoder predicted {want}",
                    planes[row * 64 + col]
                );
            }
        }
    }

    /// The same against the other transform size, the 64x64 one a whole
    /// superblock carries. Only its top-left 32x32 can hold coefficients, and
    /// the spec zeroes the rest before the row transform — a 64-point network
    /// that read the missing half as anything else would disagree here.
    #[test]
    fn the_inverse_transform_predicts_what_ffmpeg_decodes_for_a_64x64_block() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP the_inverse_transform_predicts_what_ffmpeg_decodes_for_a_64x64_block: no ffmpeg"
            );
            return;
        }
        let coeffs = [
            (0u8, 0u8, 12i32),
            (0, 2, -9),
            (2, 0, 6),
            (1, 1, -5),
            (5, 9, 8),
            (31, 0, -4),
            (0, 31, 3),
        ];
        let block: BlockCoeffs = coeffs
            .iter()
            .map(|&(row, col, level)| Coeff { row, col, level })
            .collect::<Vec<_>>()
            .into();
        let rows = decode_luma_sb(64, 64, &[Superblock::Whole(block)]);

        let mut levels = vec![0i32; 64 * 64];
        for &(row, col, level) in &coeffs {
            levels[usize::from(row) * 64 + usize::from(col)] = level;
        }
        let residual = crate::transform::dequant_and_inverse(&levels, 64, 8, i32::from(Q_IDX));
        for row in 0..64 {
            for col in 0..64 {
                let want = (128 + residual[row * 64 + col]).clamp(0, 255);
                assert_eq!(
                    i32::from(rows[row][col]),
                    want,
                    "({row},{col}): ffmpeg decoded {} where the encoder predicted {want}",
                    rows[row][col]
                );
            }
        }
    }

    /// The AV1 transforms are normalized so that a quantized level means the
    /// same thing at every size: doubling the transform's side halves what a
    /// DC level is worth, which is the job the per-size row shift and the
    /// dequantizer's denominator split between them. The frame syntax only
    /// codes 32x32 and 64x64 transforms, so this is what holds the four
    /// smaller sizes' shifts to the spec's table — a shift wrong by one at any
    /// size doubles or halves that size's step alone.
    #[test]
    fn a_dc_level_is_worth_half_as_much_each_time_the_transform_doubles() {
        let mut previous: Option<i32> = None;
        for side in [4usize, 8, 16, 32, 64] {
            let mut levels = vec![0i32; side * side];
            levels[0] = 100;
            let residual =
                crate::transform::dequant_and_inverse(&levels, side, 8, i32::from(Q_IDX));
            let dc = residual[0];
            assert!(
                residual.iter().all(|&r| r == dc),
                "side {side}: a DC coefficient reconstructs flat"
            );
            if let Some(previous) = previous {
                assert!(
                    (dc * 2 - previous).abs() <= 1,
                    "side {side}: {dc} is not half of the previous size's {previous}"
                );
            }
            previous = Some(dc);
        }
    }

    /// The encoder's own round trip, through a real bitstream.
    ///
    /// A picture is chosen, its residual against the DC prediction of 128 is
    /// forward-transformed and quantized, the levels are written into a tile,
    /// and ffmpeg decodes it. Two things have to hold: what comes back is
    /// exactly what the encoder's own inverse said it would be — sample for
    /// sample, which is what lets a rate-distortion loop trust its
    /// reconstruction — and it is within a quantizer step of the picture that
    /// was asked for, which is what makes the forward transform an encoder
    /// rather than a scrambler.
    #[test]
    fn a_quantized_picture_decodes_to_what_the_encoder_reconstructed() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_quantized_picture_decodes_to_what_the_encoder_reconstructed: no ffmpeg"
            );
            return;
        }
        // A gradient, a ripple and a corner patch: low frequencies, a mid one
        // and an edge, none of them a basis function of the transform.
        let mut residual = vec![0i32; 32 * 32];
        for row in 0..32usize {
            for col in 0..32usize {
                let gradient = row as f64 * 2.0 - 32.0;
                let ripple = 25.0
                    * (col as f64 * std::f64::consts::PI / 6.0).sin()
                    * (row as f64 * std::f64::consts::PI / 11.0).cos();
                let patch = if row >= 24 && col >= 20 { -40.0 } else { 0.0 };
                residual[row * 32 + col] = (gradient + ripple + patch).round() as i32;
            }
        }

        let levels =
            crate::transform::forward_and_quantize(&residual, 32, 8, i32::from(Q_IDX), 0.5);
        assert!(
            levels.iter().any(|&l| l != 0),
            "the picture quantized away entirely"
        );
        let coded: Vec<Coeff> = levels
            .iter()
            .enumerate()
            .filter(|&(_, &level)| level != 0)
            .map(|(i, &level)| Coeff {
                row: (i / 32) as u8,
                col: (i % 32) as u8,
                level,
            })
            .collect();

        let rows = decode_luma_at(
            64,
            64,
            &[
                coded.into(),
                BlockCoeffs::default(),
                BlockCoeffs::default(),
                BlockCoeffs::default(),
            ],
        );
        let reconstruction =
            crate::transform::dequant_and_inverse(&levels, 32, 8, i32::from(Q_IDX));

        let mut squared = 0.0f64;
        for row in 0..32 {
            for col in 0..32 {
                let want = (128 + reconstruction[row * 32 + col]).clamp(0, 255);
                assert_eq!(
                    i32::from(rows[row][col]),
                    want,
                    "sample ({row},{col}): the decoder and the encoder disagree"
                );
                let error = f64::from(rows[row][col]) - f64::from(128 + residual[row * 32 + col]);
                squared += error * error;
            }
        }
        let rmse = (squared / 1024.0).sqrt();
        // A quantizer step is q/8 in residual units. Rounding to nearest
        // spreads the error over a step, and a quarter of one is a bound this
        // picture clears with room to spare (2.2 of 14 as written) while a
        // forward transform that is merely energy-preserving does not: a
        // transposed column pass measures 13.3 here.
        let step = f64::from(crate::quant::ac_q(8, i32::from(Q_IDX))) / 8.0;
        assert!(
            rmse < step / 4.0,
            "the decoded picture is {rmse} off, a step being {step}"
        );
    }

    /// The inverse of [`write_mv_component`], mirroring [`SymbolDecoder`]
    /// against the spec's own `read_mv_component` pseudocode.
    fn decode_mv_component(
        dec: &mut crate::msac::tests::SymbolDecoder,
        c: &mut MvComponentCdfs,
    ) -> i32 {
        let sign = dec.symbol(&mut c.sign);
        let class = dec.symbol(&mut c.class);
        let local = if class == 0 {
            let bit = dec.symbol(&mut c.class0_bit);
            let fr = dec.symbol(&mut c.class0_fr[bit]);
            (bit << 3) | (fr << 1) | 1
        } else {
            let mut d = 0;
            for i in 0..class {
                d |= dec.symbol(&mut c.bit[i]) << i;
            }
            let fr = dec.symbol(&mut c.fr);
            (d << 3) | (fr << 1) | 1
        };
        let mag = mv_class_base(class) + local as i32 + 1;
        if sign == 1 { -mag } else { mag }
    }

    /// Decodes one superblock a [`sb_coeff_inter_frame_tile`] payload wrote,
    /// mirroring the writer's own context tracking (partition, skip,
    /// is_inter, and the MV stack) symbol for symbol against a fresh
    /// [`Cdfs`], so a desync between the two shows up as a wrong decoded
    /// value rather than a silent pass. Every block here is skipped, so no
    /// coefficient symbols are read. `wrong_y_mode` swaps in `KF_Y_MODE`'s
    /// (0, 0) row for an intra block's mode read, to show that reading the
    /// wrong table decodes the wrong mode rather than the one that was
    /// written.
    ///
    /// One decoded block's `(skip, is_inter, mode, mv)` — `mode` is `None`
    /// for an inter block, `mv` is `(0, 0)` for an intra one.
    type DecodedBlock = (bool, bool, Option<usize>, (i32, i32));

    /// Returns the number of symbols and literals read, and each block's
    /// decoded state.
    fn decode_inter_sb(
        data: &[u8],
        mi_cols: u32,
        mi_rows: u32,
        wrong_y_mode: bool,
    ) -> (usize, Vec<DecodedBlock>) {
        let mut dec = crate::msac::tests::SymbolDecoder::new(data);
        let mut cdfs = Cdfs::new(2);
        let mut grid = MiGrid::new(mi_cols as usize, mi_rows as usize);
        let mut above_skip = [false; 2];
        let mut left_skip = [false; 2];
        let mut above_inter = [false; 2];
        let mut left_inter = [false; 2];
        let mut count = 0usize;

        count += 1;
        dec.symbol(&mut cdfs.partition_w64[0]);

        let mut results = Vec::new();
        for quadrant in 0..4usize {
            let (r32, c32) = (quadrant / 2, quadrant % 2);
            count += 1;
            dec.symbol(&mut cdfs.partition_w32[0]);

            let (has_above, has_left) = (r32 > 0, c32 > 0);
            let skip_ctx = usize::from(above_skip[c32]) + usize::from(left_skip[r32]);
            count += 1;
            let skip = dec.symbol(&mut cdfs.skip[skip_ctx]) == 1;

            let ii_ctx = intra_inter_ctx(has_above, has_left, above_inter[c32], left_inter[r32]);
            count += 1;
            let is_inter = dec.symbol(&mut cdfs.intra_inter[ii_ctx]) == 1;

            let (mode, mv) = if is_inter {
                let sr_ctx = single_ref_ctx(above_inter[c32] || left_inter[r32]);
                count += 3;
                assert_eq!(
                    dec.symbol(&mut cdfs.single_ref[sr_ctx][0]),
                    0,
                    "single_ref p1"
                );
                assert_eq!(
                    dec.symbol(&mut cdfs.single_ref[sr_ctx][2]),
                    0,
                    "single_ref p3"
                );
                assert_eq!(
                    dec.symbol(&mut cdfs.single_ref[sr_ctx][3]),
                    0,
                    "single_ref p4"
                );

                let (mi_row, mi_col) = (r32 * 8, c32 * 8);
                let stack = find_mv_stack(
                    &grid,
                    mi_row,
                    mi_col,
                    8,
                    8,
                    1,
                    mi_cols as usize,
                    mi_rows as usize,
                );

                count += 1;
                let new_mv = dec.symbol(&mut cdfs.new_mv[stack.new_mv_ctx]) == 0;
                let (mv, is_new_mv) = if new_mv {
                    if stack.entries.len() > 1 {
                        count += 1;
                        dec.symbol(&mut cdfs.drl_mode[stack.drl_ctx[0]]);
                    }
                    count += 1;
                    let joint = dec.symbol(&mut cdfs.mv_joint);
                    let mut diff = (0, 0);
                    if joint == 2 || joint == 3 {
                        // sign, class, class0_bit, class0_fr for this test's
                        // small (class 0) components.
                        count += 4;
                        diff.0 = decode_mv_component(&mut dec, &mut cdfs.mv_comp[0]);
                    }
                    if joint == 1 || joint == 3 {
                        count += 4;
                        diff.1 = decode_mv_component(&mut dec, &mut cdfs.mv_comp[1]);
                    }
                    ((stack.pred_mv.0 + diff.0, stack.pred_mv.1 + diff.1), true)
                } else {
                    count += 2;
                    assert_eq!(
                        dec.symbol(&mut cdfs.zero_mv[stack.zero_mv_ctx]),
                        1,
                        "zero_mv"
                    );
                    assert_eq!(dec.symbol(&mut cdfs.ref_mv[stack.ref_mv_ctx]), 0, "ref_mv");
                    (stack.nearest_mv, false)
                };
                for dr in 0..8 {
                    for dc in 0..8 {
                        grid.set(
                            mi_row + dr,
                            mi_col + dc,
                            MiInfo {
                                is_inter: true,
                                ref_frame: 1,
                                ref_frame1: None,
                                mv1: None,
                                mv,
                                is_new_mv,
                                size: 8,
                                size_h: 8,
                                is_global_mv0: false,
                                is_global_mv1: false,
                            },
                        );
                    }
                }
                (None, mv)
            } else {
                count += 1;
                let m = if wrong_y_mode {
                    dec.symbol(&mut cdfs.kf_y_mode[0][0])
                } else {
                    dec.symbol(&mut cdfs.y_mode[3])
                };
                if (V_PRED..=D67_PRED).contains(&m) {
                    count += 1;
                    dec.symbol(&mut cdfs.angle_delta[m - V_PRED]);
                }
                count += 1;
                dec.symbol(&mut cdfs.uv_mode_cfl[m]);
                (Some(m), (0, 0))
            };

            above_skip[c32] = skip;
            left_skip[r32] = skip;
            above_inter[c32] = is_inter;
            left_inter[r32] = is_inter;
            results.push((skip, is_inter, mode, mv));
        }
        (count, results)
    }

    /// Every superblock here skips: no residual, so the only symbols are the
    /// mode chain, and the byte count and the symbol count both come out
    /// small enough to hand-check.
    #[test]
    fn an_all_skip_inter_superblock_is_small_and_reads_back() {
        let block = BlockCoeffs {
            inter: Some(InterInfo {
                mode: InterMode::NearestMv,
                mv: (0, 0),
            }),
            skip: true,
            ..BlockCoeffs::default()
        };
        let blocks = vec![
            Quadrant::Whole(block.clone()),
            Quadrant::Whole(block.clone()),
            Quadrant::Whole(block.clone()),
            Quadrant::Whole(block),
        ];
        let data = sb_coeff_inter_frame_tile(16, 16, 90, &blocks).unwrap();
        // Hand count: one partition_w64, then per block partition_w32 + skip +
        // is_inter + 3 single_ref + new_mv + zero_mv + ref_mv = 9, times four.
        assert!(
            data.len() <= 16,
            "an all-skip NEARESTMV superblock took {} bytes",
            data.len()
        );

        let (count, results) = decode_inter_sb(&data, 16, 16, false);
        assert_eq!(count, 1 + 4 * 9, "symbol count against the hand count");
        for (skip, is_inter, mode, mv) in results {
            assert!(skip);
            assert!(is_inter);
            assert_eq!(mode, None);
            // Every block's only inter neighbours carry (0, 0), so the stack's
            // nearest candidate is (0, 0) throughout.
            assert_eq!(mv, (0, 0));
        }
    }

    /// The fourth block codes NEWMV against a (0, 0) predictor with a coded
    /// column component: the symbol sequence a decoder reads back has to be
    /// exactly what [`write_mv`] wrote, not a residual that merely decodes
    /// to *some* valid vector.
    #[test]
    fn a_newmv_block_reads_back_the_exact_symbol_sequence_it_was_written_with() {
        let nearest = BlockCoeffs {
            inter: Some(InterInfo {
                mode: InterMode::NearestMv,
                mv: (0, 0),
            }),
            skip: true,
            ..BlockCoeffs::default()
        };
        let newmv = BlockCoeffs {
            inter: Some(InterInfo {
                mode: InterMode::NewMv,
                mv: (0, 2),
            }),
            skip: true,
            ..BlockCoeffs::default()
        };
        let blocks = vec![
            Quadrant::Whole(nearest.clone()),
            Quadrant::Whole(nearest.clone()),
            Quadrant::Whole(nearest),
            Quadrant::Whole(newmv),
        ];
        let data = sb_coeff_inter_frame_tile(16, 16, 90, &blocks).unwrap();

        let (count, results) = decode_inter_sb(&data, 16, 16, false);
        // Three NEARESTMV blocks at 9 symbols each, one NEWMV block at
        // partition + skip + is_inter + 3 single_ref + new_mv + mv_joint +
        // (sign, class, class0_bit, class0_fr for the one nonzero component)
        // = 12, plus the superblock's partition_w64.
        assert_eq!(count, 1 + 3 * 9 + 12, "symbol count against the hand count");
        let (skip, is_inter, mode, mv) = results[3];
        assert!(skip);
        assert!(is_inter);
        assert_eq!(mode, None);
        assert_eq!(
            mv,
            (0, 2),
            "the decoded motion vector must be exactly what was written"
        );
    }

    /// Decodes only the first 32x32 block's mode symbol of a superblock: the
    /// tile's own first block always reads partition, skip and is_inter at
    /// context zero, so this needs none of [`decode_inter_sb`]'s neighbour
    /// bookkeeping — which matters here because the mutation this feeds
    /// desyncs everything after the one symbol it is testing, and a decoder
    /// that kept reading past it would be asserting on garbage.
    fn decode_first_block_mode(data: &[u8], wrong_y_mode: bool) -> usize {
        let mut dec = crate::msac::tests::SymbolDecoder::new(data);
        let mut cdfs = Cdfs::new(2);
        dec.symbol(&mut cdfs.partition_w64[0]);
        dec.symbol(&mut cdfs.partition_w32[0]);
        dec.symbol(&mut cdfs.skip[0]);
        dec.symbol(&mut cdfs.intra_inter[0]);
        if wrong_y_mode {
            dec.symbol(&mut cdfs.kf_y_mode[0][0])
        } else {
            dec.symbol(&mut cdfs.y_mode[3])
        }
    }

    /// An inter frame's intra block reads its mode against `Y_MODE`'s size
    /// group, not `KF_Y_MODE`'s neighbour-context table a key frame's block
    /// reads. Reading the wrong table back — `KF_Y_MODE`'s `(0, 0)` row —
    /// decodes a different mode than the one [`sb_coeff_inter_frame_tile`]
    /// wrote, because the two tables carry different default probabilities
    /// even though a row of each is the same fourteen-entry width. (The swap
    /// this test's name warns of does not typecheck as a one-line
    /// substitution at the write site either — `cdfs.kf_y_mode[3]` is a
    /// `[[u16; 14]; 5]` row, not the single `[u16; 14]` `Y_MODE` indexing
    /// needs — so a mutation that got the *outer* shape wrong would be
    /// caught by `cargo check` before it ever reached a test; this test is
    /// what catches getting the *right* leaf shape from the *wrong* table.)
    #[test]
    fn an_intra_block_in_an_inter_frame_is_caught_reading_kf_y_mode() {
        let block = BlockCoeffs {
            mode: H_PRED as u8,
            skip: true,
            ..BlockCoeffs::default()
        };
        let blocks = vec![Quadrant::Whole(block); 4];
        let data = sb_coeff_inter_frame_tile(16, 16, 90, &blocks).unwrap();

        assert_eq!(
            decode_first_block_mode(&data, false),
            H_PRED,
            "the right table reads the mode back exactly"
        );
        assert_ne!(
            decode_first_block_mode(&data, true),
            H_PRED,
            "KF_Y_MODE's different probabilities must decode a different mode"
        );
    }
}
