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

use ec_av1_syntax::{
    CdefParams, DeltaParams, LoopFilterParams, LoopRestorationParams,
    SEG_LVL_ALT_LF_Y_V, SEG_LVL_ALT_Q, SegmentationParams, TileInfo,
};
use ec_core::{Error, Result};

use crate::cdf;
use crate::cdf_state::{Cdfs, MvComponentCdfs, TxbSet, TxbTables};
use crate::encode::{Picture, Reach};
use crate::intra::predict;
use crate::mc;
use crate::msac::SymbolDecoder;
use crate::mvstack::{
    ALTREF_FRAME, ALTREF2_FRAME, BWDREF_FRAME, GOLDEN_FRAME, LAST_FRAME, LAST2_FRAME, LAST3_FRAME,
    MiGrid, MiInfo, NO_SIGN_BIAS, NeighbourRef, SignBiasTable, comp_reference_type_ctx,
    find_mv_stack_with_sign_bias, reference_mode_ctx,
    single_ref_p1_ctx, single_ref_p2_ctx, single_ref_p3_ctx, single_ref_p4_ctx, single_ref_p5_ctx,
    single_ref_p6_ctx, uni_comp_ref_p1_ctx,
};
use crate::tile::{INTRA_MODE_CTX, block_grid, has_half};
use crate::transform::{TxType, dequant_and_inverse_typed, dequant_and_inverse_typed_wh};

const PARTITION_NONE: usize = 0;
const PARTITION_HORZ: usize = 1;
const PARTITION_VERT: usize = 2;
const PARTITION_SPLIT: usize = 3;
const PARTITION_HORZ_A: usize = 4;
const PARTITION_HORZ_B: usize = 5;
const PARTITION_VERT_A: usize = 6;
const PARTITION_VERT_B: usize = 7;
const PARTITION_HORZ_4: usize = 8;
const PARTITION_VERT_4: usize = 9;
const SB_MI: u32 = 16;
const BLOCK_MI: u32 = 8;

// How many `use_filter_intra` symbols this decoder has read as `1`, across
// every call on the current thread -- the cheap counter [`filter_intra_hits`] gate
// tests read (before/after, not the absolute value) to prove a stream
// actually exercised the filter-intra predictor rather than silently
// skipping it.
thread_local! {
    static FILTER_INTRA_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`FILTER_INTRA_HITS`].
pub(crate) fn filter_intra_hits() -> usize {
    FILTER_INTRA_HITS.with(|c| c.get())
}

// lane-realworld r1: this frame's `cdef.bits` (spec 5.9.19), threaded into
// [`maybe_read_cdef_idx`] the same way [`ENABLE_EDGE_FILTER`] threads a
// frame-level flag through the recursive block decode -- `0` (the default)
// makes every call a true no-op, matching every existing gate stream's
// `cdef_bits == 0` behaviour exactly.
thread_local! {
    static CDEF_BITS: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}
// lane-realworld r8: the sequence header's `color_config.bit_depth`, set
// once per `decode_key_frame_tile_with_cdfs`/`decode_inter_frame_tile_with_cdfs`
// call the same way `CDEF_BITS` is above. `stream.rs`'s own refusal still
// blocks every non-8-bit stream from reaching a decode call, so this is `8`
// on every path that runs today -- threading it now (rather than the literal
// `8`) is stage 1 of widening past that refusal without re-deriving every
// clamp bound later. `PlaneBuf`'s sample storage stays `u16` regardless of
// depth (this decoder's own internal choice, matches dav1d/libaom).
thread_local! {
    static BIT_DEPTH: std::cell::Cell<u8> = const { std::cell::Cell::new(8) };
}
/// `(1 << bit_depth) - 1` — the reconstruction clamp bound for the current
/// stream's [`BIT_DEPTH`], replacing the crate-wide literal `255`.
pub(crate) fn sample_max() -> i32 {
    (1i32 << BIT_DEPTH.with(std::cell::Cell::get)) - 1
}
/// The current stream's bit depth, for callers (e.g. dequant) that need the
/// raw value rather than the derived clamp bound.
pub(crate) fn bit_depth() -> u8 {
    BIT_DEPTH.with(std::cell::Cell::get)
}
/// Sets [`BIT_DEPTH`] for the current thread, called once per frame from
/// [`crate::stream`]'s decode loop.
pub(crate) fn set_bit_depth(bit_depth: u8) {
    BIT_DEPTH.with(|c| c.set(bit_depth));
}
// Whether this superblock's single CDEF unit (this decoder has no 128x128
// SB, so the SB IS the CDEF unit) has already had its `cdef_idx` literal
// read -- reset to `false` at the top of each superblock in the two tile
// loops, spec `read_cdef`'s `cdef_transmitted[index]`.
thread_local! {
    static CDEF_TRANSMITTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
// Number of superblock columns in the current tile's grid, so
// [`maybe_read_cdef_idx`] can turn an `(mi_r, mi_c)` into a flat index into
// [`CDEF_IDX_GRID`].
thread_local! {
    static CDEF_SB_COLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
// One resolved `cdef_idx` per superblock, read by [`apply_cdef`] to select
// which of the header's `1 << cdef.bits` strength pairs applies to that
// superblock's samples.
thread_local! {
    static CDEF_IDX_GRID: std::cell::RefCell<Vec<u8>> = std::cell::RefCell::new(Vec::new());
}
// How many real (`cdef.bits > 0`) `cdef_idx` literals [`maybe_read_cdef_idx`]
// has read, across every call on the current thread -- the gate's proof that a
// `cdef_bits > 0` stream actually reached the new reader rather than every
// superblock coincidentally being all-skip.
thread_local! {
    static CDEF_IDX_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`CDEF_IDX_HITS`].
pub(crate) fn cdef_idx_hits() -> usize {
    CDEF_IDX_HITS.with(|c| c.get())
}

// lane-realworld r4: this frame's `delta.q_present` (spec 5.9.17) and its
// actual step (`1 << delta.q_res`, already the real multiplier, not the raw
// 2-bit field). `false`/`1` (the defaults) make [`maybe_read_delta_q`] a
// true no-op, matching every existing gate stream's `delta_q_present == 0`
// behaviour exactly, the same corner-cut [`CDEF_BITS`] takes above.
thread_local! {
    static DELTA_Q_PRESENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static DELTA_Q_RES: std::cell::Cell<i32> = const { std::cell::Cell::new(1) };
}
// The running quantizer index (spec `CurrentQIndex`, libaom
// `xd->current_base_qindex`) -- reset to the frame's own `base_q_idx` at the
// top of every tile (spec `decode_tile`), carried across superblocks within
// that tile, and mutated only by [`maybe_read_delta_q`]. Read back at the
// dequantization call sites instead of the frame-constant `base_q_idx`.
thread_local! {
    static CURRENT_Q_IDX: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
}
// This frame's own per-plane DC/AC quantizer-index deltas (lane-sbpart r11,
// spec 5.9.12/7.12.2, [`crate::quant::QuantDeltas`]) -- frame-constant (not
// per-superblock like [`CURRENT_Q_IDX`]), set once per frame decode and read
// back at every dequantization call site keyed on which plane it is doing.
thread_local! {
    static QUANT_DELTAS: std::cell::Cell<crate::quant::QuantDeltas> = const {
        std::cell::Cell::new(crate::quant::QuantDeltas {
            y_dc: 0,
            u_dc: 0,
            u_ac: 0,
            v_dc: 0,
            v_ac: 0,
        })
    };
}
// How many real `delta_q_present` symbol groups [`maybe_read_delta_q`] has
// read, across every call on the current thread -- the gate's proof that a
// `delta_q_present` stream actually reached the new reader.
thread_local! {
    static DELTA_Q_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`DELTA_Q_HITS`].
pub(crate) fn delta_q_hits() -> usize {
    DELTA_Q_HITS.with(|c| c.get())
}

/// This frame's [`QUANT_DELTAS`] `(dc, ac)` pair for plane `plane_idx` (0 =
/// Y, 1 = U, 2 = V) -- luma has no coded `ac` delta (spec 5.9.12 never reads
/// one), so it is always `0` here.
fn plane_q_delta(plane_idx: usize) -> (i32, i32) {
    let d = QUANT_DELTAS.with(|c| c.get());
    match plane_idx {
        0 => (d.y_dc, 0),
        1 => (d.u_dc, d.u_ac),
        _ => (d.v_dc, d.v_ac),
    }
}

/// `read_delta_qindex`/`read_delta_q_params` (spec 5.11.10, libaom
/// `decodemv.c:85-146,735-770`), called right after [`maybe_read_cdef_idx`]
/// at every block-decode call site (spec order `skip -> cdef -> delta_q`).
/// A no-op when `delta_q_present` is unset, or when this block is not at the
/// superblock's own top-left MI position (`b_row == 0 && b_col == 0` in
/// libaom, collapsed here to a position check since a partition tree's
/// top-left leaf is the only block that can ever land there), or when that
/// leaf both *is* the whole superblock and is `skip` (the one case libaom
/// itself skips the read, state carrying over unchanged).
fn maybe_read_delta_q(dec: &mut SymbolDecoder, cdfs: &mut Cdfs, mi_r: usize, mi_c: usize, is_whole_sb: bool, skip: bool) {
    if !DELTA_Q_PRESENT.with(|c| c.get()) {
        return;
    }
    if mi_r % SB_MI as usize != 0 || mi_c % SB_MI as usize != 0 {
        return;
    }
    if is_whole_sb && skip {
        return;
    }
    let mut abs = dec.symbol(&mut cdfs.delta_q) as i32;
    const DELTA_Q_SMALL: i32 = 3;
    if abs >= DELTA_Q_SMALL {
        let rem_bits = dec.literal(3) + 1;
        let thr = (1i32 << rem_bits) + 1;
        abs = dec.literal(rem_bits) as i32 + thr;
    }
    let sign_negative = if abs != 0 { dec.literal(1) != 0 } else { true };
    let reduced = if sign_negative { -abs } else { abs };
    DELTA_Q_HITS.with(|c| c.set(c.get() + 1));
    let res = DELTA_Q_RES.with(|c| c.get());
    CURRENT_Q_IDX.with(|c| c.set((c.get() + reduced * res).clamp(1, 255)));
}

// lane-realworld r5: this frame's `delta.lf_present`/`lf_res`/`lf_multi`
// (spec 5.9.18) -- same no-op-by-default corner-cut as `DELTA_Q_PRESENT`.
thread_local! {
    static DELTA_LF_PRESENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static DELTA_LF_RES: std::cell::Cell<i32> = const { std::cell::Cell::new(1) };
    static DELTA_LF_MULTI: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
// The running per-plane/direction loop-filter delta (spec `DeltaLF[i]`,
// `i` in `0..FRAME_LF_COUNT`) -- reset to `[0; 4]` at the top of every tile,
// mutated only by [`maybe_read_delta_lf`]. `!delta_lf_multi` mode only ever
// updates index 0 and every reader treats that as the value for all 4
// indices (spec `read_delta_lf_params`'s own single-cdf branch).
thread_local! {
    static CURRENT_DELTA_LF: std::cell::Cell<[i32; 4]> = const { std::cell::Cell::new([0; 4]) };
}
// How many real `delta_lf_present` symbol groups [`maybe_read_delta_lf`] has
// read -- the gate's proof the reader fired, mirroring [`DELTA_Q_HITS`].
thread_local! {
    static DELTA_LF_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`DELTA_LF_HITS`].
pub(crate) fn delta_lf_hits() -> usize {
    DELTA_LF_HITS.with(|c| c.get())
}

/// `read_delta_lflevel`/`read_delta_lf_params` (spec 5.11.11, libaom
/// `decodemv.c:99-146,735-770`) -- same shape and same call-site gating as
/// [`maybe_read_delta_q`] (spec order `delta_q -> delta_lf`), looped over
/// `FRAME_LF_COUNT = 4` planes/directions when `delta_lf_multi` is set, or
/// read once into index 0 otherwise.
fn maybe_read_delta_lf(dec: &mut SymbolDecoder, cdfs: &mut Cdfs, mi_r: usize, mi_c: usize, is_whole_sb: bool, skip: bool) {
    if !DELTA_LF_PRESENT.with(|c| c.get()) {
        return;
    }
    if mi_r % SB_MI as usize != 0 || mi_c % SB_MI as usize != 0 {
        return;
    }
    if is_whole_sb && skip {
        return;
    }
    let multi = DELTA_LF_MULTI.with(|c| c.get());
    let res = DELTA_LF_RES.with(|c| c.get());
    let count = if multi { 4 } else { 1 };
    let mut cur = CURRENT_DELTA_LF.with(|c| c.get());
    for i in 0..count {
        let mut abs = if multi {
            dec.symbol(&mut cdfs.delta_lf_multi[i]) as i32
        } else {
            dec.symbol(&mut cdfs.delta_lf) as i32
        };
        const DELTA_LF_SMALL: i32 = 3;
        if abs >= DELTA_LF_SMALL {
            let rem_bits = dec.literal(3) + 1;
            let thr = (1i32 << rem_bits) + 1;
            abs = dec.literal(rem_bits) as i32 + thr;
        }
        let sign_negative = if abs != 0 { dec.literal(1) != 0 } else { true };
        let reduced = if sign_negative { -abs } else { abs };
        cur[i] = (cur[i] + reduced * res).clamp(-63, 63);
        DELTA_LF_HITS.with(|c| c.set(c.get() + 1));
    }
    CURRENT_DELTA_LF.with(|c| c.set(cur));
}

/// `read_cdef` (spec 5.11.56, libaom `decodemv.c:read_cdef`), called right
/// after `skip` is known at every block-decode call site. A no-op at
/// `cdef.bits == 0` (0-bit literal, nothing to consume) -- and
/// `read_cdef_params` already forces `bits = 0` under `coded_lossless`/
/// `allow_intrabc`, so neither needs a separate check here. Reads once per
/// superblock, at the first non-skip block.
fn maybe_read_cdef_idx(dec: &mut SymbolDecoder, mi_r: usize, mi_c: usize, skip: bool) {
    let bits = CDEF_BITS.with(|c| c.get());
    if bits == 0 || skip || CDEF_TRANSMITTED.with(|c| c.get()) {
        return;
    }
    let idx = dec.literal(u32::from(bits)) as u8;
    CDEF_TRANSMITTED.with(|c| c.set(true));
    CDEF_IDX_HITS.with(|c| c.set(c.get() + 1));
    let (sb_r, sb_c) = (mi_r / SB_MI as usize, mi_c / SB_MI as usize);
    let sb_cols = CDEF_SB_COLS.with(|c| c.get());
    if sb_cols > 0 {
        CDEF_IDX_GRID.with(|g| {
            let mut g = g.borrow_mut();
            let i = sb_r * sb_cols + sb_c;
            if i < g.len() {
                g[i] = idx;
            }
        });
    }
}

// ---------------------------------------------------------------------------
// lane-seg: segmentation. Header (spec 5.9.14) is parsed by `ec-av1-syntax`;
// this block is the per-block `segment_id` reader (spec 5.11.7-5.11.9) plus
// the two consumers a real stream exercises: the per-segment quantizer index
// (spec 7.12.2 `get_qindex`) and the per-segment loop-filter level
// (spec 7.14.4). Shapes follow libaom `decodemv.c:read_segment_id`/
// `read_intra_segment_id`/`read_inter_segment_id` and
// `pred_common.h:av1_get_spatial_seg_pred` -- note libaom returns the SPATIAL
// PREDICTION (not 0) from `read_segment_id` when the block is `skip`, which
// is what the oracle every gate here compares against actually does.
thread_local! {
    static SEG: std::cell::RefCell<SegmentationParams> =
        std::cell::RefCell::new(SegmentationParams::default());
    /// `CurrentSegmentIds`: this frame's own map, `mi_rows * mi_cols` entries
    /// with stride `mi_cols` (NOT the padded `Neighbours` grid stride).
    static SEG_IDS: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    /// `PrevSegmentIds`: the primary reference frame's saved map, empty when
    /// this frame has no primary reference (spec `load_previous_segment_ids`).
    static PREV_SEG_IDS: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    /// `(mi_rows, mi_cols)` for both maps above.
    static SEG_MI_DIMS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
    /// `AboveSegPredContext` (per mi column) / `LeftSegPredContext` (per mi
    /// row) -- spec 5.11.9's own `seg_id_predicted` neighbour state, which
    /// libaom keeps on the neighbouring `mbmi->seg_id_predicted` instead.
    static ABOVE_SEG_PRED: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    static LEFT_SEG_PRED: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    /// The block currently being decoded -- read back by [`block_q_idx`].
    static CUR_SEGMENT_ID: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    /// The tile's own top-left mi position, for `AvailU`/`AvailL`.
    static SEG_TILE_ORIGIN: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
    /// How many real `segment_id` symbols were read, and the set of segment
    /// ids any block ended up with -- the gate's proof the feature fired.
    static SEG_ID_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SEG_IDS_SEEN: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    /// How many `seg_id_predicted` symbols were read (temporal update).
    static SEG_PRED_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Installs this frame's segmentation parameters and allocates its map, with
/// `prev` the primary reference frame's saved map (spec
/// `load_previous_segment_ids`; `None` for `PRIMARY_REF_NONE` or an empty
/// slot). Called once per frame, whether or not segmentation is enabled.
pub(crate) fn set_segmentation(
    seg: SegmentationParams,
    mi_rows: usize,
    mi_cols: usize,
    prev: Option<&[u8]>,
) {
    SEG.with(|s| *s.borrow_mut() = seg);
    SEG_MI_DIMS.with(|c| c.set((mi_rows, mi_cols)));
    SEG_IDS.with(|m| *m.borrow_mut() = vec![0u8; mi_rows * mi_cols]);
    PREV_SEG_IDS.with(|m| {
        let mut m = m.borrow_mut();
        m.clear();
        if let Some(p) = prev {
            if p.len() == mi_rows * mi_cols {
                m.extend_from_slice(p);
            }
        }
    });
    ABOVE_SEG_PRED.with(|m| *m.borrow_mut() = vec![0u8; mi_cols]);
    LEFT_SEG_PRED.with(|m| *m.borrow_mut() = vec![0u8; mi_rows]);
    CUR_SEGMENT_ID.with(|c| c.set(0));
}

/// This frame's decoded segment map, to be stored into whichever reference
/// slots `refresh_frame_flags` names (spec 7.20's `SavedSegmentIds`).
pub(crate) fn take_segment_ids() -> Vec<u8> {
    SEG_IDS.with(|m| m.borrow().clone())
}

/// How many `segment_id` symbols have been read (never reset per frame).
pub(crate) fn segment_id_hits() -> usize {
    SEG_ID_HITS.with(|c| c.get())
}

/// How many distinct segment ids any decoded block has carried.
pub(crate) fn segment_ids_seen() -> usize {
    SEG_IDS_SEEN.with(|c| c.get().count_ones() as usize)
}

/// How many `seg_id_predicted` symbols have been read (temporal update).
pub(crate) fn segment_pred_hits() -> usize {
    SEG_PRED_HITS.with(|c| c.get())
}

/// Resets the segmentation hit counters (gate setup).
pub(crate) fn reset_segment_hits() {
    SEG_ID_HITS.with(|c| c.set(0));
    SEG_IDS_SEEN.with(|c| c.set(0));
    SEG_PRED_HITS.with(|c| c.set(0));
}

/// `seg_feature_active_idx(segment_id, feature)` for the frame currently
/// being decoded, with its `FeatureData` value.
fn seg_feature(segment_id: usize, feature: usize) -> Option<i32> {
    SEG.with(|s| {
        let s = s.borrow();
        s.feature_active(segment_id, feature)
            .then(|| i32::from(s.feature_data[segment_id][feature]))
    })
}

/// `get_qindex(ignoreDeltaQ = 0, segment_id)` (spec 7.12.2, libaom
/// `av1_get_qindex`) for the block currently being decoded: the running
/// `CurrentQIndex` shifted by this block's segment's `SEG_LVL_ALT_Q` data.
/// Every dequant call site reads the block's quantizer through here.
fn block_q_idx() -> i32 {
    let base = CURRENT_Q_IDX.with(|c| c.get());
    let seg_id = CUR_SEGMENT_ID.with(|c| c.get()) as usize;
    match seg_feature(seg_id, SEG_LVL_ALT_Q) {
        Some(data) => (base + data).clamp(0, 255),
        None => base,
    }
}

/// The segment id recorded for one mi cell of the current frame's map.
fn segment_id_at(mi_r: usize, mi_c: usize) -> u8 {
    let (mi_rows, mi_cols) = SEG_MI_DIMS.with(|c| c.get());
    if mi_r >= mi_rows || mi_c >= mi_cols {
        return 0;
    }
    SEG_IDS.with(|m| m.borrow()[mi_r * mi_cols + mi_c])
}

/// `set_segment_id`: stamps `id` over every mi cell this block covers.
fn write_segment_id(mi_r: usize, mi_c: usize, w_mi: usize, h_mi: usize, id: u8) {
    let (mi_rows, mi_cols) = SEG_MI_DIMS.with(|c| c.get());
    let (x_mis, y_mis) = (w_mi.min(mi_cols.saturating_sub(mi_c)), h_mi.min(mi_rows.saturating_sub(mi_r)));
    SEG_IDS.with(|m| {
        let mut m = m.borrow_mut();
        for y in 0..y_mis {
            for x in 0..x_mis {
                m[(mi_r + y) * mi_cols + mi_c + x] = id;
            }
        }
    });
    CUR_SEGMENT_ID.with(|c| c.set(id));
    SEG_IDS_SEEN.with(|c| c.set(c.get() | 1 << id));
}

/// `get_predicted_segment_id` (libaom `dec_get_segment_id` over
/// `last_frame_seg_map`): the minimum previous-frame segment id over the
/// block's own mi footprint. `0` when there is no previous map.
fn predicted_segment_id(mi_r: usize, mi_c: usize, w_mi: usize, h_mi: usize) -> u8 {
    let (mi_rows, mi_cols) = SEG_MI_DIMS.with(|c| c.get());
    PREV_SEG_IDS.with(|m| {
        let m = m.borrow();
        if m.is_empty() {
            return 0;
        }
        let (x_mis, y_mis) = (w_mi.min(mi_cols.saturating_sub(mi_c)), h_mi.min(mi_rows.saturating_sub(mi_r)));
        let mut id = u8::MAX;
        for y in 0..y_mis {
            for x in 0..x_mis {
                id = id.min(m[(mi_r + y) * mi_cols + mi_c + x]);
            }
        }
        if id == u8::MAX { 0 } else { id }
    })
}

/// `av1_neg_deinterleave` (libaom `decodemv.c:258`).
fn neg_deinterleave(diff: i32, reference: i32, max: i32) -> i32 {
    if reference == 0 {
        return diff;
    }
    if reference >= max - 1 {
        return max - diff - 1;
    }
    let half = if 2 * reference < max {
        diff <= 2 * reference
    } else {
        diff <= 2 * (max - reference - 1)
    };
    if half {
        return if diff & 1 != 0 {
            reference + ((diff + 1) >> 1)
        } else {
            reference - (diff >> 1)
        };
    }
    if 2 * reference < max { diff } else { max - (diff + 1) }
}

/// `av1_get_spatial_seg_pred`: `(prediction, cdf_index)` from the current
/// frame's already-decoded above/left/above-left neighbours.
fn spatial_seg_pred(mi_r: usize, mi_c: usize) -> (i32, usize) {
    let (row0, col0) = SEG_TILE_ORIGIN.with(|c| c.get());
    let (avail_u, avail_l) = (mi_r > row0, mi_c > col0);
    let prev_ul = (avail_u && avail_l).then(|| i32::from(segment_id_at(mi_r - 1, mi_c - 1)));
    let prev_u = avail_u.then(|| i32::from(segment_id_at(mi_r - 1, mi_c)));
    let prev_l = avail_l.then(|| i32::from(segment_id_at(mi_r, mi_c - 1)));
    let ctx = match prev_ul {
        None => 0,
        Some(ul) if Some(ul) == prev_u && Some(ul) == prev_l => 2,
        Some(ul) if Some(ul) == prev_u || Some(ul) == prev_l || prev_u == prev_l => 1,
        Some(_) => 0,
    };
    let pred = match (prev_u, prev_l) {
        (None, l) => l.unwrap_or(0),
        (Some(u), None) => u,
        (Some(u), Some(l)) => {
            if prev_ul == Some(u) {
                u
            } else {
                l
            }
        }
    };
    (pred, ctx)
}

/// `read_segment_id` (libaom `decodemv.c:280`): the spatially predicted id
/// when `skip`, otherwise one `segment_id` symbol de-interleaved against
/// that prediction. Does NOT write the map -- callers do.
fn read_segment_id(dec: &mut SymbolDecoder, cdfs: &mut Cdfs, mi_r: usize, mi_c: usize, skip: bool) -> u8 {
    let (pred, ctx) = spatial_seg_pred(mi_r, mi_c);
    if skip {
        return pred as u8;
    }
    let coded = dec.symbol(&mut cdfs.segment_id[ctx]) as i32;
    SEG_ID_HITS.with(|c| c.set(c.get() + 1));
    let last_active = SEG.with(|s| i32::from(s.borrow().last_active_seg_id));
    neg_deinterleave(coded, pred, last_active + 1).clamp(0, last_active) as u8
}

/// `read_intra_segment_id` (spec 5.11.7): called twice per intra-frame block
/// -- once before `skip` when `SegIdPreSkip`, once after it otherwise (the
/// caller decides which by passing `pre_skip`).
fn intra_segment_id(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    mi_r: usize,
    mi_c: usize,
    w_mi: usize,
    h_mi: usize,
    skip: bool,
) {
    if !SEG.with(|s| s.borrow().enabled) {
        return;
    }
    let id = read_segment_id(dec, cdfs, mi_r, mi_c, skip);
    write_segment_id(mi_r, mi_c, w_mi, h_mi, id);
}

/// Whether this frame's intra blocks read their `segment_id` before `skip`
/// (`SegIdPreSkip`); `false` also when segmentation is off.
fn seg_id_pre_skip() -> bool {
    SEG.with(|s| {
        let s = s.borrow();
        s.enabled && s.seg_id_pre_skip
    })
}

/// `read_inter_segment_id(preskip)` (spec 5.11.9, libaom `decodemv.c:352`).
/// Called at both spec positions of an inter frame's block: `pre_skip = true`
/// before `skip_mode`/`skip`, and `pre_skip = false` after them.
fn inter_segment_id(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    mi_r: usize,
    mi_c: usize,
    w_mi: usize,
    h_mi: usize,
    skip: bool,
    pre_skip: bool,
) {
    let (enabled, update_map, temporal_update, pre_skip_frame) = SEG.with(|s| {
        let s = s.borrow();
        (s.enabled, s.update_map, s.temporal_update, s.seg_id_pre_skip)
    });
    if !enabled {
        return;
    }
    if !update_map {
        // The map is inherited wholesale from the previous frame.
        let id = predicted_segment_id(mi_r, mi_c, w_mi, h_mi);
        write_segment_id(mi_r, mi_c, w_mi, h_mi, id);
        return;
    }
    if pre_skip && !pre_skip_frame {
        return;
    }
    if !pre_skip && skip {
        // libaom: a skipped block never codes `seg_id_predicted`, and its
        // segment id is the spatial prediction.
        stamp_seg_pred(mi_r, mi_c, w_mi, h_mi, 0);
        let id = read_segment_id(dec, cdfs, mi_r, mi_c, true);
        write_segment_id(mi_r, mi_c, w_mi, h_mi, id);
        return;
    }
    let id = if temporal_update {
        let ctx = seg_pred_ctx(mi_r, mi_c);
        let predicted = dec.symbol(&mut cdfs.segment_pred[ctx]) != 0;
        SEG_PRED_HITS.with(|c| c.set(c.get() + 1));
        stamp_seg_pred(mi_r, mi_c, w_mi, h_mi, u8::from(predicted));
        if predicted {
            predicted_segment_id(mi_r, mi_c, w_mi, h_mi)
        } else {
            read_segment_id(dec, cdfs, mi_r, mi_c, false)
        }
    } else {
        read_segment_id(dec, cdfs, mi_r, mi_c, false)
    };
    write_segment_id(mi_r, mi_c, w_mi, h_mi, id);
}

/// `av1_get_pred_context_seg_id`: above + left `seg_id_predicted` flags.
fn seg_pred_ctx(mi_r: usize, mi_c: usize) -> usize {
    // libaom reads the flag off `above_mbmi`/`left_mbmi`, which are NULL (and
    // so contribute 0) outside the tile -- the availability check here is
    // what keeps a previous tile's stamps out of this tile's context.
    let (row0, col0) = SEG_TILE_ORIGIN.with(|c| c.get());
    let above = if mi_r > row0 {
        ABOVE_SEG_PRED.with(|m| m.borrow().get(mi_c).copied().unwrap_or(0))
    } else {
        0
    };
    let left = if mi_c > col0 {
        LEFT_SEG_PRED.with(|m| m.borrow().get(mi_r).copied().unwrap_or(0))
    } else {
        0
    };
    usize::from(above + left)
}

/// Spec 5.11.9's `AboveSegPredContext`/`LeftSegPredContext` update.
fn stamp_seg_pred(mi_r: usize, mi_c: usize, w_mi: usize, h_mi: usize, value: u8) {
    ABOVE_SEG_PRED.with(|m| {
        let mut m = m.borrow_mut();
        for i in 0..w_mi {
            if let Some(cell) = m.get_mut(mi_c + i) {
                *cell = value;
            }
        }
    });
    LEFT_SEG_PRED.with(|m| {
        let mut m = m.borrow_mut();
        for i in 0..h_mi {
            if let Some(cell) = m.get_mut(mi_r + i) {
                *cell = value;
            }
        }
    });
}

// How many key-frame luma blocks resolved `smooth_neighbor` (spec
// `get_intra_edge_filter_type`) to `true` -- lane-chroma r2's own before/after
// counter, proving a stream actually put a directional block next to a
// smooth-predicted neighbour rather than every block reading the (previously
// hardcoded `false`) edge-filter strength bucket unchanged.
thread_local! {
    static SMOOTH_LUMA_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`SMOOTH_LUMA_HITS`].
pub(crate) fn smooth_luma_hits() -> usize {
    SMOOTH_LUMA_HITS.with(|c| c.get())
}

thread_local! {
    /// lane-band46 r1: how many split-transform units answered their intra
    /// reach DIFFERENTLY under libaom's `row_off`/`col_off` rules than the
    /// standalone-block lookup this decoder used to apply to them (class
    /// reach-is-per-transform-unit). Every hit is a transform unit whose
    /// above-right / below-left reference samples used to be wrong.
    static SPLIT_TU_REACH_FIX_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`SPLIT_TU_REACH_FIX_HITS`].
pub(crate) fn split_tu_reach_fix_hits() -> usize {
    SPLIT_TU_REACH_FIX_HITS.with(|c| c.get())
}

/// libaom `has_top_right`/`has_bottom_left` for one transform unit of a split
/// transform ([`crate::encode::Reach::of_tu`]), counting how often that answer
/// differs from the standalone-block answer the call sites used before
/// lane-band46 r1 -- the counter a gate asserts on to prove a stream really
/// exercised the fix.
#[allow(clippy::too_many_arguments)]
fn tu_reach(
    bw: usize,
    bh: usize,
    tx: usize,
    row_off: usize,
    col_off: usize,
    px: usize,
    py: usize,
    width: usize,
    height: usize,
) -> Reach {
    let fixed = Reach::of_tu(bw, bh, tx, row_off, col_off, px, py, width, height);
    let standalone = Reach::of(tx, px + col_off * MI, py + row_off * MI, width, height);
    if fixed != standalone {
        SPLIT_TU_REACH_FIX_HITS.with(|c| c.set(c.get() + 1));
    }
    fixed
}

// How many `uv_mode` reads resolved to `SMOOTH_PRED..=PAETH_PRED` (9..=12),
// across every call on the current thread -- lane-chroma r3's own before/after
// counter, proving a stream actually exercised chroma's smooth/paeth
// predictor (the previously-refused round-2 gap) rather than every block
// landing on DC/CFL/directional.
thread_local! {
    static SMOOTH_UV_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`SMOOTH_UV_HITS`].
pub(crate) fn smooth_uv_hits() -> usize {
    SMOOTH_UV_HITS.with(|c| c.get())
}

// How many tiles [`decode_key_frame_tile_with_cdfs`] has actually decoded a
// full superblock walk over, across every call on the current thread --
// lane-tiles r2's own gate-blind-to-feature guard: a gate over a
// multi-tile stream must prove more than one tile actually fired, not just
// that the stream carried tile_info.cols > 1.
thread_local! {
    static TILE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}
/// Current value of [`TILE_HITS`].
pub(crate) fn tile_hits() -> usize {
    TILE_HITS.with(|c| c.get())
}

// How many 4-pixel deblocking edge groups [`edge_params`] has actually
// selected for filtering, across every call on the current thread -- the same
// before/after counter pattern as [`FILTER_INTRA_HITS`], proving a stream
// exercised `apply_deblock` rather than every edge landing on level 0.
thread_local! {
    static DEBLOCK_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}
/// lane-comppin r4: decode-order picture counter for `EC_AV1_PREFILT_DUMP`,
/// mirroring stream.rs's `pictures_decoded` so the two dumps index
/// identically against aomdec's own `ec_frame_idx` in decodeframe.c.
static PREFILT_PICTURE_IDX: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Current value of [`DEBLOCK_HITS`].
pub(crate) fn deblock_hits() -> usize {
    DEBLOCK_HITS.with(|c| c.get())
}

// How many [`read_tx_size`] reads resolved a `tx_depth` strictly less than
// the block's own side, across every call on the current thread -- the same
// before/after counter pattern as [`FILTER_INTRA_HITS`], proving a stream
// actually exercised a *split* transform under `TxMode::Select` rather than
// every block coincidentally resolving `depth=0` (indistinguishable from
// `TxMode::Largest` pixel-wise).
thread_local! {
    static TX_DEPTH_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

// How many plain SQUARE intra blocks (lane-sqchroma r1) resolved a luma
// transform smaller than their own side (`tx_depth != 0`) while coding a
// chroma prediction other than `DC_PRED` -- the pair the square path's chroma
// only exercises with aomenc's tx-size search left ON (its default), which
// every older recipe in this file pins off. Counted at the block, so a stream
// where RD happened to give every split-transform block a DC chroma mode
// reads as zero rather than silently passing for free.
thread_local! {
    static SQ_CHROMA_TX_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`SQ_CHROMA_TX_HITS`].
pub(crate) fn sq_chroma_tx_hits() -> usize {
    SQ_CHROMA_TX_HITS.with(|c| c.get())
}

/// Current value of [`TX_DEPTH_HITS`].
pub(crate) fn tx_depth_hits() -> usize {
    TX_DEPTH_HITS.with(|c| c.get())
}

// lane-txselect: how many `txfm_split` symbols an inter frame's var-tx tree
// read, and how many of those actually took the split, on the current thread
// -- the same before/after counter pattern as [`TX_DEPTH_HITS`], proving a
// stream exercised spec 5.11.17's `read_var_tx_size` rather than every block
// coincidentally coding one whole-block transform.
thread_local! {
    static TXFM_SPLIT_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TXFM_SPLIT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // This frame's own `tx_mode == TxMode::Select` and `reduced_tx_set` bits,
    // set once per inter tile by [`decode_inter_frame_tile_with_cdfs`] --
    // `decode_inter_block` already carries forty parameters and both are
    // frame-constant.
    static TX_SELECT_INTER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static REDUCED_TX_SET_INTER: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Current value of [`TXFM_SPLIT_READS`].
pub(crate) fn txfm_split_reads() -> usize {
    TXFM_SPLIT_READS.with(|c| c.get())
}

/// Current value of [`TXFM_SPLIT_HITS`].
pub(crate) fn txfm_split_hits() -> usize {
    TXFM_SPLIT_HITS.with(|c| c.get())
}

/// Sets the current thread's [`TX_SELECT_INTER`]/[`REDUCED_TX_SET_INTER`].
pub(crate) fn set_inter_tx_mode(tx_select: bool, reduced_tx_set: bool) {
    TX_SELECT_INTER.with(|c| c.set(tx_select));
    REDUCED_TX_SET_INTER.with(|c| c.set(reduced_tx_set));
}

// How many `partition_w16` reads (a 16x16 block's own partition symbol, key
// intra-frame path) resolved to `PARTITION_VERT_B` -- lane-rect16 r1: a
// plain default-settings aomenc run over lavfi mandelbrot hits this arm on
// frame 0's very first non-NONE/SPLIT `partition_w16` symbol (mi=(3,2)), not
// the plain HORZ/VERT this lane's charter assumed. Left rect (8x16, real
// `decode_block_rect`) + two 8x8 leaves stacked on the right (`decode_leaf8`,
// chained via `prev_leaf` exactly like the existing 16x16-SPLIT-into-4x8x8
// path above).
thread_local! {
    static VERT_B_INTRA_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`VERT_B_INTRA_HITS`].
pub(crate) fn vert_b_intra_hits() -> usize {
    VERT_B_INTRA_HITS.with(|c| c.get())
}

// How many `partition_w16` reads resolved to a plain `PARTITION_HORZ`/
// `PARTITION_VERT` (lane-rect16 r2): re-measuring after the VERT_B fix above
// showed mandelbrot's real first-hit blocker is this plain arm at mi=(0,7),
// not VERT_B (`refusal-names-a-correlate`; the VERT_B write-up's "frame 0's
// very first non-NONE/SPLIT symbol" claim was about decode ORDER within one
// superblock's z-order recursion, not raster mi position).
thread_local! {
    static HORZ_VERT_INTRA_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`HORZ_VERT_INTRA_HITS`].
pub(crate) fn horz_vert_intra_hits() -> usize {
    HORZ_VERT_INTRA_HITS.with(|c| c.get())
}

// How many HORZ/VERT intra strips resolved a SPLIT luma transform
// (`tx_depth != 0`) and decoded it per transform unit (lane-rectsplit r1) --
// the case every rect path here refused by name before, and the first
// refusal the user's Hunger Games extract (600s in) stops at.
thread_local! {
    static RECT_SPLIT_TX_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT_SPLIT_TX_HITS`].
pub(crate) fn rect_split_tx_hits() -> usize {
    RECT_SPLIT_TX_HITS.with(|c| c.get())
}

// The subset of [`RECT_SPLIT_TX_HITS`] that is BOTH superblock-level (one
// side 64) AND split past the first depth, i.e. tiled more than one unit in
// each axis -- the only shape with a transform unit that has neighbours on
// two sides inside its own strip (`row_off > 0 && col_off > 0`), and the one
// no gate fired before lane-rectsplit r2 (the shared counter above also
// counts 32x32-level and depth-1 strips, so it cannot prove this case).
thread_local! {
    static RECT_SPLIT_SB_INTERIOR_TU_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT_SPLIT_SB_INTERIOR_TU_HITS`].
pub(crate) fn rect_split_sb_interior_tu_hits() -> usize {
    RECT_SPLIT_SB_INTERIOR_TU_HITS.with(|c| c.get())
}

// How many HORZ/VERT intra strips actually predicted with filter intra
// (lane-rectsplit r1) -- the case `decode_block_rect` refused by name
// ("this decoder predicts square-only") until `predict_filter_intra` took
// `bw`x`bh`.
thread_local! {
    static FILTER_INTRA_RECT_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`FILTER_INTRA_RECT_HITS`].
pub(crate) fn filter_intra_rect_hits() -> usize {
    FILTER_INTRA_RECT_HITS.with(|c| c.get())
}

// As [`FILTER_INTRA_RECT_HITS`], counting only the strips BELOW 16x16
// (8x16/16x8 leaves of a 16x16 HORZ/VERT partition, lane-fistrip r1) -- the
// shapes whose `use_filter_intra` symbol had no CDF class before this round.
// A gate asserting the wider counter would be satisfied by a 32x16 strip and
// prove nothing about the new shapes.
thread_local! {
    static FILTER_INTRA_RECT_SUB16_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`FILTER_INTRA_RECT_SUB16_HITS`].
pub(crate) fn filter_intra_rect_sub16_hits() -> usize {
    FILTER_INTRA_RECT_SUB16_HITS.with(|c| c.get())
}

// How many [`read_coeffs`] reads resolved a `tx_type` outside `TX_CLASS_2D`
// (`V_DCT`/`H_DCT`, spec 5.11.39's `TxClass::Horiz`/`Vert`), across every
// call on the current thread -- the same before/after counter pattern as
// [`TX_DEPTH_HITS`], proving a stream actually exercised the class-split
// `eob_pt`/1D-neighbour context path rather than every block coincidentally
// landing on `DCT_DCT`.
thread_local! {
    static TX_CLASS1_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

// How many `uv_mode` reads resolved to a directional chroma mode (neither
// `DC_PRED` nor `UV_CFL_PRED`), across every call on the current thread -- the same
// before/after counter pattern as [`FILTER_INTRA_HITS`], proving a stream
// actually exercised chroma's own directional predictor.
thread_local! {
    static DIRECTIONAL_UV_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`DIRECTIONAL_UV_HITS`].
pub(crate) fn directional_uv_hits() -> usize {
    DIRECTIONAL_UV_HITS.with(|c| c.get())
}

// How many `angle_delta`/`angle_delta_uv` symbols this decoder has read as
// something other than [`ANGLE_DELTA_ZERO`], across every call in the
// process -- the same before/after counter pattern as [`FILTER_INTRA_HITS`],
// proving a stream actually exercised a nonzero angle delta.
thread_local! {
    static ANGLE_DELTA_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

// How many [`read_single_ref`] reads resolved to a reference other than
// `LAST_FRAME`, across every call on the current thread -- the same before/after
// counter pattern as [`FILTER_INTRA_HITS`], proving a stream actually
// exercised a non-`LAST_FRAME` reference (`GOLDEN_FRAME`, this decoder's
// only other supported one so far) rather than every block coincidentally
// resolving `LAST_FRAME`.
thread_local! {
    static NON_LAST_REF_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`NON_LAST_REF_HITS`].
pub(crate) fn non_last_ref_hits() -> usize {
    NON_LAST_REF_HITS.with(|c| c.get())
}

// Per-reference [`NON_LAST_REF_HITS`] breakdown (lane-av1refs): one counter
// per `MV_REFERENCE_FRAME` past `LAST_FRAME`, so a gate targeting one
// specific reference (`LAST2`/`LAST3`/`GOLDEN`/`BWDREF`/`ALTREF2`/`ALTREF`)
// can prove THAT reference fired rather than any non-`LAST_FRAME` one --
// `NON_LAST_REF_HITS` alone cannot tell a `LAST2_FRAME` gate from a
// `GOLDEN_FRAME` draw it did not ask for.
thread_local! {
    static LAST2_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}
thread_local! {
    static LAST3_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}
thread_local! {
    static GOLDEN_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}
thread_local! {
    static BWDREF_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}
thread_local! {
    static ALTREF2_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}
thread_local! {
    static ALTREF_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

// How many [`read_comp_mode`] reads resolved `COMPOUND_REFERENCE` (lane-av1comp)
// -- proves a `reference_select` stream actually reached a block that picked
// compound, not just that the header bit was set. Every such block is
// refused after this fires ([`decode_inter_block`]/[`decode_inter_block8`]),
// so this only ever counts attempted reads, never a completed decode.
thread_local! {
    static COMP_MODE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`COMP_MODE_HITS`].
pub(crate) fn comp_mode_hits() -> usize {
    COMP_MODE_HITS.with(|c| c.get())
}

// How many blocks [`read_intra_mode`] decoded a real (nonzero-size) palette-Y
// use for, across every call on the current thread -- the same before/after
// counter pattern as [`FILTER_INTRA_HITS`], proving a stream actually
// exercised palette reconstruction rather than only reading and refusing the
// `palette_y_mode` symbol.
thread_local! {
    static PALETTE_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`PALETTE_HITS`].
pub(crate) fn palette_hits() -> usize {
    PALETTE_HITS.with(|c| c.get())
}

// lane-palette2 r1: as [`PALETTE_HITS`], for a real (nonzero-size) chroma
// palette use -- proves a stream actually reconstructed UV palette pixels,
// not just read and refused the `palette_uv_mode` symbol.
thread_local! {
    static PALETTE_UV_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`PALETTE_UV_HITS`].
pub(crate) fn palette_uv_hits() -> usize {
    PALETTE_UV_HITS.with(|c| c.get())
}

// lane-palette2 r1: how many HORZ/VERT rect intra strips ([`read_intra_mode_rect`])
// decoded a real palette-Y use -- proves the rect strip's own palette syntax
// (not just the square path's) actually fired.
thread_local! {
    static PALETTE_RECT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`PALETTE_RECT_HITS`].
pub(crate) fn palette_rect_hits() -> usize {
    PALETTE_RECT_HITS.with(|c| c.get())
}

// lane-palette2 r1: how many palette-Y blocks decoded with a split luma
// transform (`tx_select && logical_tx != side`) -- proves the per-TU index-map
// slicing actually ran, not just that the whole-block path stayed exact.
thread_local! {
    static PALETTE_SPLIT_TX_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`PALETTE_SPLIT_TX_HITS`].
pub(crate) fn palette_split_tx_hits() -> usize {
    PALETTE_SPLIT_TX_HITS.with(|c| c.get())
}

thread_local! {
    /// A just-decoded palette-Y block's own predicted pixels
    /// (`side*side`, row-major, `colors[map[i]]`), consumed by
    /// [`PlaneBuf::reconstruct`]'s next call in place of `predict()` -- the
    /// same off-the-call-stack idiom as [`ENABLE_EDGE_FILTER`] (threading a
    /// per-block override through `read_plane`'s two dozen call sites is the
    /// alternative this avoids). Always empty again once that call consumes
    /// it (`.take()`), so a non-palette block's own `reconstruct` call never
    /// sees stale state.
    static PALETTE_PRED: std::cell::RefCell<Option<Vec<u16>>> =
        const { std::cell::RefCell::new(None) };
}

thread_local! {
    /// The current frame's `mi` grid, kept only while a frame header set
    /// `allow_intrabc` -- an intrabc block's DV predictor is the ordinary
    /// MV stack ([`crate::mvstack::find_mv_stack`]) built against
    /// `INTRA_FRAME`, which needs the neighbour state a key frame otherwise
    /// never keeps. `(grid, mi_cols, mi_rows)`; `None` on every frame that
    /// does not allow intrabc (so nothing is paid for it).
    ///
    /// corner-cut: only [`decode_block`] (the square path) records into it,
    /// so a frame mixing intrabc with rect/sub-8x8 leaves would predict off
    /// an incomplete grid. Ceiling: the DV *value* (never the parse) would
    /// be wrong there; upgrade = record from the rect/leaf paths too.
    static INTRABC_MI_GRID: std::cell::RefCell<Option<(crate::mvstack::MiGrid, usize, usize)>> =
        const { std::cell::RefCell::new(None) };
    /// The block vector [`read_intra_mode`] just decoded, handed to
    /// [`decode_block`] off the call stack (same idiom as [`PALETTE_PRED`],
    /// which the alternative -- a tenth tuple member through every caller --
    /// already avoids). Always `.take()`n by the block that reads it.
    static INTRABC_DV: std::cell::Cell<Option<(i32, i32)>> = const { std::cell::Cell::new(None) };
    /// How many blocks decoded `use_intrabc == 1` -- the gate's hit counter.
    static INTRABC_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`INTRABC_HITS`].
pub(crate) fn intrabc_hits() -> usize {
    INTRABC_HITS.with(|c| c.get())
}

/// Zeroes [`INTRABC_HITS`] (gate tests call this before a decode).
pub(crate) fn reset_intrabc_hits() {
    INTRABC_HITS.with(|c| c.set(0));
}

/// Records one coded block into [`INTRABC_MI_GRID`]: every block contributes
/// its size (libaom's `scan_row_mbmi` advances `processed_rows` off any
/// candidate's `bsize`, ref-match or not), an intrabc one also its DV as an
/// `INTRA_FRAME` candidate (libaom treats intrabc as `is_inter_block`).
fn record_intrabc_mi(mi_r: usize, mi_c: usize, n4: usize, dv: Option<(i32, i32)>) {
    INTRABC_MI_GRID.with(|g| {
        let mut g = g.borrow_mut();
        let Some((grid, _, _)) = g.as_mut() else {
            return;
        };
        let info = crate::mvstack::MiInfo {
            is_inter: dv.is_some(),
            ref_frame: 0,
            ref_frame1: None,
            mv: dv.unwrap_or((0, 0)),
            mv1: None,
            is_new_mv: dv.is_some(),
            is_global_mv0: false,
            is_global_mv1: false,
            size: n4,
            size_h: n4,
        };
        for r in mi_r..mi_r + n4 {
            for c in mi_c..mi_c + n4 {
                grid.set(r, c, info);
            }
        }
    });
}

// How many blocks decoded `skip_mode == 1` (lane-av1comp round 14) --
// proves a real `skip_mode_present` stream actually reached a block that
// picked it, not just that the frame header bit was set.
thread_local! {
    static SKIP_MODE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`SKIP_MODE_HITS`].
pub(crate) fn skip_mode_hits() -> usize {
    SKIP_MODE_HITS.with(|c| c.get())
}

// lane-motionmode: how many blocks actually decoded `obmc_selected == true`
// (as opposed to just reaching an eligible `read_motion_mode` symbol read)
// -- the gate's own proof that a real aomenc `--enable-obmc=1` stream
// reached [`obmc_blend`], not just the header bit.
thread_local! {
    static OBMC_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`OBMC_HITS`].
pub(crate) fn obmc_hits() -> usize {
    OBMC_HITS.with(|c| c.get())
}

// lane-scaledref r1: per-case firing counts for the scaled-reference gate
// (class `gate-blind-to-feature`) -- a pixel match under an UNSCALED
// reference would pass just as well and prove nothing, so the gate
// hard-asserts each combination this round lifts actually occurred:
// a compound block with a scaled tap, a warp block whose warp libaom
// suppresses because the reference is scaled, an OBMC block, an interintra
// block, and an 8x8 leaf.
thread_local! {
    static SCALED_COMPOUND_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SCALED_WARP_FALLBACK_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SCALED_OBMC_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SCALED_INTERINTRA_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SCALED_BLOCK8_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SCALED_WARP_SUPPRESSED_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static MIXED_SCALE_COMPOUND_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`MIXED_SCALE_COMPOUND_HITS`]: compound blocks with one
/// scaled and one unscaled tap.
pub(crate) fn mixed_scale_compound_hits() -> usize {
    MIXED_SCALE_COMPOUND_HITS.with(|c| c.get())
}

/// Current value of [`SCALED_WARP_SUPPRESSED_HITS`]: blocks that would have
/// read the 3-symbol `motion_mode_cdf` but read the 2-symbol `obmc_cdf`
/// instead because their reference is scaled (libaom `motion_mode_allowed`,
/// `blockd.h:1484`).
pub(crate) fn scaled_warp_suppressed_hits() -> usize {
    SCALED_WARP_SUPPRESSED_HITS.with(|c| c.get())
}

/// Current value of [`SCALED_COMPOUND_HITS`].
pub(crate) fn scaled_compound_hits() -> usize {
    SCALED_COMPOUND_HITS.with(|c| c.get())
}

/// Current value of [`SCALED_WARP_FALLBACK_HITS`].
pub(crate) fn scaled_warp_fallback_hits() -> usize {
    SCALED_WARP_FALLBACK_HITS.with(|c| c.get())
}

/// Current value of [`SCALED_OBMC_HITS`].
pub(crate) fn scaled_obmc_hits() -> usize {
    SCALED_OBMC_HITS.with(|c| c.get())
}

/// Current value of [`SCALED_INTERINTRA_HITS`].
pub(crate) fn scaled_interintra_hits() -> usize {
    SCALED_INTERINTRA_HITS.with(|c| c.get())
}

/// Current value of [`SCALED_BLOCK8_HITS`].
pub(crate) fn scaled_block8_hits() -> usize {
    SCALED_BLOCK8_HITS.with(|c| c.get())
}

// lane-motionmode round 3: same proof as [`OBMC_HITS`], narrowed to
// `decode_inter_block8`'s own 8x8-leaf `read_motion_mode` -- a subset of
// [`OBMC_HITS`] (both fire together on an 8x8 hit), lets the gate tell an
// 8x8-leaf OBMC block apart from a 16x16+ one.
thread_local! {
    static OBMC_HITS_8: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// lane-warp round 1: how many blocks resolved `motion_mode_allowed` to
// `WARPED_CAUSAL`-eligible (3-symbol read, `num_proj_ref >= 1` under
// `allow_warped_motion`) AND the symbol actually decoded to `WARPED_CAUSAL`
// -- the gate's proof that a fixture really exercises the alphabet this
// round changed, even though the block itself still refuses (warp
// estimation/filter not ported).
thread_local! {
    static WARP_SELECTED_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`WARP_SELECTED_HITS`].
pub(crate) fn warp_selected_hits() -> usize {
    WARP_SELECTED_HITS.with(|c| c.get())
}

// lane-cwarp r1: how many COMPOUND_REFERENCE blocks had at least one
// reference predicted through the per-ref GLOBAL warp (`av1_warp_plane`
// into the compound intermediate) rather than translationally -- the
// gate's proof that a `GLOBAL_GLOBALMV` block under a ROTZOOM/AFFINE model
// really reached [`crate::warp::warp_affine_compound`].
thread_local! {
    static COMPOUND_WARP_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`COMPOUND_WARP_HITS`].
pub(crate) fn compound_warp_hits() -> usize {
    COMPOUND_WARP_HITS.with(|c| c.get())
}

// lane-gmaffine r1: how many blocks built their prediction from a
// SIX-parameter (`AFFINE`) global-motion model through
// `crate::warp::global_warp_params` -- the gate's proof that a stream really
// carried an AFFINE `global_motion_params` AND that a block was predicted
// with it, not merely that the header parsed one.
thread_local! {
    static AFFINE_GM_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`AFFINE_GM_HITS`].
pub(crate) fn affine_gm_hits() -> usize {
    AFFINE_GM_HITS.with(|c| c.get())
}

// lane-gmaffine r1: 8x8-leaf-only counters -- how many `BLOCK_8X8` leaves
// coded `GLOBALMV` (`GLOBALMV_HITS_8`) and how many built a real
// `WARPED_CAUSAL` local-warp prediction there (`WARP_HITS_8`, incremented
// only once `find_projection` returned a usable model, unlike
// `WARP_SELECTED_HITS` which counts the symbol). Both are the 8x8 gates'
// proof that the leaf path itself fired, not the 16x16+ one.
thread_local! {
    static GLOBALMV_HITS_8: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static WARP_HITS_8: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`GLOBALMV_HITS_8`].
pub(crate) fn globalmv_hits_8() -> usize {
    GLOBALMV_HITS_8.with(|c| c.get())
}

/// Current value of [`WARP_HITS_8`].
pub(crate) fn warp_hits_8() -> usize {
    WARP_HITS_8.with(|c| c.get())
}

// How many blocks decoded `interintra == 1` with the non-wedge blended
// prediction applied (lane-interintra r1) -- the gate's proof that a
// stream actually exercised interintra.
thread_local! {
    static INTERINTRA_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`INTERINTRA_HITS`].
pub(crate) fn interintra_hits() -> usize {
    INTERINTRA_HITS.with(|c| c.get())
}

// How many blocks decoded `comp_group_idx == 1` (masked COMPOUND_REFERENCE,
// wedge or diffwtd) and had `compound_type`/`wedge_idx`/`wedge_sign`/
// `mask_type` consumed entropy-exact -- lane-maskcomp r1's proof that a
// gate fixture actually exercises this alphabet, even though the block
// still refuses (the mask blend itself is unported).
thread_local! {
    static MASKED_COMPOUND_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`MASKED_COMPOUND_HITS`].
pub(crate) fn masked_compound_hits() -> usize {
    MASKED_COMPOUND_HITS.with(|c| c.get())
}

// How many blocks decoded a real `COMPOUND_WEDGE` block (`comp_group_idx ==
// 1`, `compound_type == 0`) -- lane-wedge r3's proof a gate fixture
// actually exercised the wedge blend, not just the DIFFWTD half of
// `MASKED_COMPOUND_HITS`.
thread_local! {
    static WEDGE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`WEDGE_HITS`].
pub(crate) fn wedge_hits() -> usize {
    WEDGE_HITS.with(|c| c.get())
}

// How many blocks decoded a real wedge-INTERINTRA block
// (`interintra == 1 && use_wedge_interintra == 1`) -- lane-wii r2's proof
// a gate fixture exercised the wedge blend (fixed sign 0), not just the
// smooth arm counted by `INTERINTRA_HITS`.
thread_local! {
    static WII_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`WII_HITS`].
pub(crate) fn wii_hits() -> usize {
    WII_HITS.with(|c| c.get())
}

/// Current value of [`OBMC_HITS_8`].
pub(crate) fn obmc_hits_8() -> usize {
    OBMC_HITS_8.with(|c| c.get())
}

// lane-rectgate r1: how many 32x32 quadrants actually decoded a
// `PARTITION_HORZ_B` split (the one extended/ab partition this decoder's
// inter-frame tile loop decodes -- `PARTITION_HORZ`/`VERT`/`HORZ_A`/`VERT_A`/
// `VERT_B` all still fall into the generic "a partition type this encoder
// never writes" refusal below) -- the gate's proof that a free-partition
// aomenc stream actually exercised a non-`NONE`/`SPLIT` partition rather than
// coincidentally never selecting one.
thread_local! {
    static EXTENDED_PARTITION_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`EXTENDED_PARTITION_HITS`].
pub(crate) fn extended_partition_hits() -> usize {
    EXTENDED_PARTITION_HITS.with(|c| c.get())
}

// lane-partitions r1: how many 32x32 quadrants decoded a real
// `PARTITION_HORZ`/`PARTITION_VERT` (two true rectangular strips, unlike
// `PARTITION_HORZ_B`'s square-context stand-in) -- the free-partition
// gate's proof this round's arms actually fired.
thread_local! {
    static RECT_PARTITION_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT_PARTITION_HITS`].
pub(crate) fn rect_partition_hits() -> usize {
    RECT_PARTITION_HITS.with(|c| c.get())
}

// lane-inter8 r1: how many superblocks an inter tile coded as a single
// whole-SB `PARTITION_NONE` 64x64 inter block (the case aomenc picks for
// static content, previously a named refusal).
thread_local! {
    static INTER_SB_NONE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`INTER_SB_NONE_HITS`].
pub(crate) fn inter_sb_none_hits() -> usize {
    INTER_SB_NONE_HITS.with(|c| c.get())
}

// lane-inter8 r1: how many INTERIOR (not frame-edge-straddling) 16x16 inter
// blocks coded a real `PARTITION_SPLIT` into four 8x8 leaves -- the straddle
// path has always run that loop, so only this counter proves the newly-lifted
// alphabet value fired.
thread_local! {
    static INTER_SUB16_SPLIT_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`INTER_SUB16_SPLIT_HITS`].
pub(crate) fn inter_sub16_split_hits() -> usize {
    INTER_SUB16_SPLIT_HITS.with(|c| c.get())
}

// lane-rectwire r2: how many `PARTITION_HORZ`/`VERT` strips actually decoded
// real (non-skip) coefficients through [`read_coeffs_rect`] -- proves the
// rect coefficient reader itself fired, not just the strip-level partition
// symbol [`RECT_PARTITION_HITS`] already counts.
thread_local! {
    static RECT_COEFF_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT_COEFF_HITS`].
pub(crate) fn rect_coeff_hits() -> usize {
    RECT_COEFF_HITS.with(|c| c.get())
}

// lane-rectx r1: how many below-16x16 HORZ/VERT leaves ([`decode_leaf_rect`])
// decoded real (non-skip) TX_16X8/TX_8X16 coefficients -- the counter the
// charter's real-aomenc gate hard-asserts.
thread_local! {
    static RECT_LEAF_COEFF_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT_LEAF_COEFF_HITS`].
pub(crate) fn rect_leaf_coeff_hits() -> usize {
    RECT_LEAF_COEFF_HITS.with(|c| c.get())
}

// lane-rectx r5: how many times a NON-strip `kf_y_mode` reader
// ([`decode_block`], [`decode_block_rect`], [`decode_block_rect64`]) took an
// above/left mode from the mi-exact map that DIFFERS from its coarse 16x16
// slot -- i.e. how often the r5 defect would have picked the wrong CDF row.
thread_local! {
    static MODE_MI_OVERRIDE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`MODE_MI_OVERRIDE_HITS`].
pub(crate) fn mode_mi_override_hits() -> usize {
    MODE_MI_OVERRIDE_HITS.with(|c| c.get())
}

// lane-cfl r1: how many times a chroma intra-edge filter-type read
// (`get_filt_type`, libaom reconintra.c) took an above/left UV mode from the
// mi-exact map that DIFFERS from its coarse 16x16 slot -- i.e. how often the
// r1 defect would have filtered a chroma edge with the wrong strength.
thread_local! {
    static UV_MODE_MI_OVERRIDE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`UV_MODE_MI_OVERRIDE_HITS`].
pub(crate) fn uv_mode_mi_override_hits() -> usize {
    UV_MODE_MI_OVERRIDE_HITS.with(|c| c.get())
}

// lane-cfl r1 gate counters: `UV_CFL_PRED` blocks (every `cfl_alpha_signs`
// read) and chroma blocks with a NONZERO `angle_delta_uv` -- the two block
// shapes the r1 chroma defect showed up on.
thread_local! {
    static CFL_BLOCK_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static UV_ANGLE_DELTA_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`CFL_BLOCK_HITS`].
pub(crate) fn cfl_block_hits() -> usize {
    CFL_BLOCK_HITS.with(|c| c.get())
}

/// Current value of [`UV_ANGLE_DELTA_HITS`].
pub(crate) fn uv_angle_delta_hits() -> usize {
    UV_ANGLE_DELTA_HITS.with(|c| c.get())
}

// lane-sbpart r2: how many superblock-level `PARTITION_HORZ`/`PARTITION_VERT`
// blocks (two true 64x32/32x64 strips, [`decode_block_rect64`]) fired -- the
// gate's proof this round's arms actually reached a real block, not just
// that `partition_w64` read one of those two symbol values.
thread_local! {
    static SB_RECT_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`SB_RECT_HITS`].
pub(crate) fn sb_rect_hits() -> usize {
    SB_RECT_HITS.with(|c| c.get())
}

// lane-part32 r4: how many superblock-level AB blocks (`PARTITION_HORZ_A`/
// `_B`/`VERT_A`/`_B` at 64x64 -- two 32x32 squares plus one 64x32/32x64
// strip) fired. Real aomenc picks these at 64 even with
// `--enable-ab-partitions=0` (lane-sbpart r2's own measurement), so this is
// the counter the gate hard-asserts.
thread_local! {
    static SB_AB_HITS: std::cell::Cell<[usize; 4]> =
        const { std::cell::Cell::new([0; 4]) };
}

/// Current value of [`SB_AB_HITS`], summed over the four arms.
pub(crate) fn sb_ab_hits() -> usize {
    SB_AB_HITS.with(|c| c.get().iter().sum())
}

/// [`SB_AB_HITS`] split per arm, in `PARTITION_HORZ_A`, `_HORZ_B`,
/// `_VERT_A`, `_VERT_B` order -- what the gate hard-asserts arm by arm, so a
/// run where aomenc's RD only ever picked (say) `HORZ_A` cannot pass off as
/// proof of all four.
pub(crate) fn sb_ab_hits_by_arm() -> [usize; 4] {
    SB_AB_HITS.with(std::cell::Cell::get)
}

// lane-tx64x16: the two 1:4 superblock arms, counted per orientation --
// `PARTITION_HORZ_4` (four 64x16 strips) and `PARTITION_VERT_4` (four 16x64
// strips). Two counters, not one, because a gate that only proved "some 1:4
// strip fired" would pass on a stream that never coded the other axis, and
// every scan/context table this round added comes in a transposed pair.
thread_local! {
    static SB_RECT4_HORZ_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SB_RECT4_VERT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of `SB_RECT4_HORZ_HITS` (64x16 strips decoded).
pub(crate) fn sb_rect4_horz_hits() -> usize {
    SB_RECT4_HORZ_HITS.with(|c| c.get())
}

/// Current value of `SB_RECT4_VERT_HITS` (16x64 strips decoded).
pub(crate) fn sb_rect4_vert_hits() -> usize {
    SB_RECT4_VERT_HITS.with(|c| c.get())
}

// lane-tx64x16 r3: the 32x32-level 1:4 arms, counted per orientation --
// `PARTITION_HORZ_4` (four 32x8 strips) and `PARTITION_VERT_4` (four 8x32
// strips), the arms the Hunger Games mid-film extract stops at.
thread_local! {
    static RECT4_32_HORZ_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static RECT4_32_VERT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many 32x32-level 1:4 strips actually read coefficients (non-skip):
/// a gate that only counted strips would pass on an all-skip stream, which
/// exercises none of the new scan/context tables.
thread_local! {
    static RECT4_COEFF_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of `RECT4_COEFF_HITS`.
pub(crate) fn rect4_coeff_hits() -> usize {
    RECT4_COEFF_HITS.with(|c| c.get())
}

/// Current value of `RECT4_32_HORZ_HITS` (32x8 strips decoded).
pub(crate) fn rect4_32_horz_hits() -> usize {
    RECT4_32_HORZ_HITS.with(|c| c.get())
}

/// Current value of `RECT4_32_VERT_HITS` (8x32 strips decoded).
pub(crate) fn rect4_32_vert_hits() -> usize {
    RECT4_32_VERT_HITS.with(|c| c.get())
}

// lane-rect64q r1: how many of [`decode_block_rect64`]'s three per-plane
// dequant calls actually observed `CURRENT_Q_IDX != base_q_idx` -- proof the
// running-vs-stale-snapshot bug this round fixed is exercised by a gate, not
// just inert-compiled. Zero forever on a passing gate means that gate's
// stream never carries `delta_q_present=1` through a rect64 block.
thread_local! {
    static RECT64_QIDX_DRIFT_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT64_QIDX_DRIFT_HITS`].
pub(crate) fn rect64_qidx_drift_hits() -> usize {
    RECT64_QIDX_DRIFT_HITS.with(|c| c.get())
}

// lane-partab r1: how many 32x32 quadrants decoded an AB partition
// (PARTITION_HORZ_A / VERT_A / VERT_B -- two 16x16 squares plus one
// 16x32/32x16 strip). Like [`RECT_PARTITION_HITS`], this is the
// free-partition gate's proof this round's arms actually fired.
thread_local! {
    static PARTAB_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`PARTAB_HITS`].
pub(crate) fn partab_hits() -> usize {
    PARTAB_HITS.with(|c| c.get())
}

// lane-part32 r1: how many 32x32 quadrants decoded each INTRA AB/4-way arm,
// one counter per arm rather than one shared total -- the gate hard-asserts
// each of these individually nonzero so a recipe that only ever fires
// HORZ_A can't silently stand in for the whole set.
thread_local! {
    static INTRA_HORZ_A_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    static INTRA_HORZ_B_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    static INTRA_VERT_A_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    static INTRA_VERT_B_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`INTRA_HORZ_A_HITS`].
pub(crate) fn intra_horz_a_hits() -> usize {
    INTRA_HORZ_A_HITS.with(|c| c.get())
}
/// Current value of [`INTRA_HORZ_B_HITS`].
pub(crate) fn intra_horz_b_hits() -> usize {
    INTRA_HORZ_B_HITS.with(|c| c.get())
}
/// Current value of [`INTRA_VERT_A_HITS`].
pub(crate) fn intra_vert_a_hits() -> usize {
    INTRA_VERT_A_HITS.with(|c| c.get())
}
/// Current value of [`INTRA_VERT_B_HITS`].
pub(crate) fn intra_vert_b_hits() -> usize {
    INTRA_VERT_B_HITS.with(|c| c.get())
}

// How many [`read_inter_compound_mode`] reads actually happened
// (lane-av1comp) -- proves a `COMPOUND_REFERENCE` block reached its own
// `compound_mode` symbol (past `comp_mode`/`comp_ref` and the compound
// mvstack build), not just that `comp_mode` fired. Still refused right
// after (no MV assignment or MC wired), so this only counts attempted
// reads.
thread_local! {
    static COMPOUND_MODE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`COMPOUND_MODE_HITS`].
pub(crate) fn compound_mode_hits() -> usize {
    COMPOUND_MODE_HITS.with(|c| c.get())
}

/// How many query blocks folded in at least one temporal MV candidate
/// (spec 7.10.2.8's `add_tpl_ref_mv`) so far in this process -- the
/// temporal-MV gate's own firing counter (lane-av1tmvp).
///
/// # HANDOFF (lane-av1tmvp): not yet wired to a live stream
/// [`crate::mvstack::find_mv_stack_with_sign_bias`]'s only call site in
/// this file (`decode_inter_block`) still passes `tpl: None` -- this
/// counter compiles and the projection math it counts is unit-tested
/// ([`crate::motion_field`]), but nothing in [`crate::stream::decode_stream`]
/// builds a per-DPB-slot [`crate::motion_field::MotionField`] or calls
/// [`crate::motion_field::setup_motion_field`] yet, so this will read `0`
/// against any real stream today. See the module doc on
/// `crate::motion_field` and this crate's ledger for the remaining wiring.
pub(crate) fn tmv_hits() -> usize {
    crate::mvstack::tmv_hits()
}

/// Current value of [`crate::mc::inter_pred_hits`] -- inter predictions
/// produced on this thread, the 10-bit inter gate's firing proof.
pub(crate) fn inter_pred_hits() -> usize {
    crate::mc::inter_pred_hits()
}

/// Everything [`decode_inter_block`] needs to fold temporal MV candidates
/// (spec 7.10.2.8), threaded down from [`decode_inter_frame_tile_with_cdfs`]:
/// the current frame's own projected field ([`crate::motion_field::setup_motion_field`]'s
/// output, built by the caller when `header.use_ref_frame_mvs` is set) plus
/// the order-hint bookkeeping [`crate::motion_field::get_relative_dist`]
/// needs to turn a query block's own resolved `ref_frame` into
/// `TplArgs::cur_offset_0` (spec 7.9.3).
pub(crate) struct TplFrameArgs<'a> {
    pub field: &'a crate::motion_field::TplField,
    pub order_hint_bits: u32,
    pub order_hint: u32,
    /// `ref_order_hints[ref_frame - LAST_FRAME]` for `ref_frame` in
    /// `LAST_FRAME..=ALTREF_FRAME` — the OrderHint of the picture this
    /// frame's own `ref_frame_idx` names for each single reference.
    pub ref_order_hints: [u32; 7],
}

/// Builds this frame's own saved motion field (libaom `av1_copy_frame_mvs`,
/// spec 7.9's per-frame `MotionFieldMvs` storage step) from its fully
/// decoded `grid`: every 4x4 unit an inter block covers reads the same
/// `MiInfo` ([`decode_inter_block`]/`decode_inter_block8` write it
/// block-uniform), so sampling any one 4x4 unit inside each 8x8 cell is
/// exact — every block this crate ever codes is at least 8x8, so no 8x8
/// cell straddles two different blocks' values.
pub(crate) fn build_motion_field(
    grid: &MiGrid,
    mi_cols: usize,
    mi_rows: usize,
    order_hint: u32,
    ref_order_hints: [u32; 7],
) -> crate::motion_field::MotionField {
    let mut field =
        crate::motion_field::MotionField::new(mi_cols, mi_rows, order_hint, ref_order_hints);
    for row in 0..mi_rows {
        for col in 0..mi_cols {
            if let Some(info) = grid.get(row, col)
                && info.is_inter
                && info.ref_frame > 0
            {
                field.set(
                    row,
                    col,
                    crate::motion_field::SavedMv {
                        mv: info.mv,
                        ref_frame: info.ref_frame,
                    },
                );
            }
        }
    }
    field
}

/// Current value of the counter for `ref_frame` (`LAST2_FRAME`..=`ALTREF_FRAME`;
/// panics on `LAST_FRAME`/`INTRA_FRAME`/`NONE`, which have no counter here).
pub(crate) fn ref_hits(ref_frame: i8) -> usize {
    let c = match ref_frame {
        LAST2_FRAME => &LAST2_HITS,
        LAST3_FRAME => &LAST3_HITS,
        GOLDEN_FRAME => &GOLDEN_HITS,
        BWDREF_FRAME => &BWDREF_HITS,
        ALTREF2_FRAME => &ALTREF2_HITS,
        ALTREF_FRAME => &ALTREF_HITS,
        other => panic!("ref_hits: no per-reference counter for ref_frame {other}"),
    };
    c.with(|v| v.get())
}

thread_local! {
    /// The sequence header's `enable_intra_edge_filter`, set once per
    /// [`decode_key_frame_tile_with_cdfs`] call: [`PlaneBuf::reconstruct`]'s
    /// own read of it, kept off the call stack (see that function's own doc
    /// comment for why) rather than threaded through every `read_plane`/
    /// `decode_block`/`decode_leaf8` call between here and there.
    static ENABLE_EDGE_FILTER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Set [`ENABLE_EDGE_FILTER`] for this thread.
///
/// `decode_key_frame_tile_with_cdfs` sets it, but `decode_inter_frame_tile`
/// never did -- so an inter frame's intra blocks read whatever value the last
/// key frame decoded ON THIS THREAD left behind. Inside one stream that is
/// harmless (its own key frame sets it first), but a caller that decodes an
/// inter frame directly, or a test harness running several streams on one
/// thread, inherits a foreign sequence's bit: decode stops being a pure
/// function of the bitstream. `decode_stream` now calls this once per stream
/// from the sequence header, which covers every frame including inter ones.
pub(crate) fn set_enable_edge_filter(value: bool) {
    ENABLE_EDGE_FILTER.with(|f| f.set(value));
}

/// Current value of [`ANGLE_DELTA_HITS`].
pub(crate) fn angle_delta_hits() -> usize {
    ANGLE_DELTA_HITS.with(|c| c.get())
}

/// Current value of [`TX_CLASS1_HITS`].
pub(crate) fn tx_class1_hits() -> usize {
    TX_CLASS1_HITS.with(|c| c.get())
}

// How many `PARTITION_SPLIT` decisions below an 8x8 block (four independent
// `BLOCK_4X4` leaves, spec `decode_partition`'s `bSize < BLOCK_8X8` recursion
// bottom) this thread's decode actually reconstructed -- the same
// before/after counter pattern as [`TX_CLASS1_HITS`], proving a stream
// genuinely exercised the sub-8x8 leaf path rather than every attempt
// coincidentally landing on `PARTITION_NONE`.
thread_local! {
    static SUB8_SPLIT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`SUB8_SPLIT_HITS`].
pub(crate) fn sub8_split_hits() -> usize {
    SUB8_SPLIT_HITS.with(|c| c.get())
}

// How many 4x8 / 8x4 leaves (lane-tx4x8's `PARTITION_VERT`/`PARTITION_HORZ`
// of an 8x8 block) this thread's decode reconstructed with REAL coefficients
// -- a skipped leaf exercises the prediction and the mi bookkeeping but never
// the rect transform, the 32-position `eob_pt` or the rect scan, so the gate
// asserts on these two rather than a plain leaf count.
thread_local! {
    static TX4X8_CODED_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TX8X4_CODED_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Sub-8x8 rect leaves whose `tx_depth` symbol resolved to 1 -- the leaf's
    /// two 4x4 transform units, predicted one after the other. A separate
    /// counter from [`TX_DEPTH_HITS`] (which every square path also bumps) so
    /// the gate can prove THIS path fired.
    static RECT8_SPLIT_TX_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// 16x8/8x16 intra strips whose prediction actually read past the block's
    /// own width (above-right) or height (below-left) -- the samples
    /// `Reach::of_rect` decides on, and the only ones a wrong `has_tr_*`/
    /// `has_bl_*` table row can corrupt (lane-tx4x8 r3).
    static RECT_STRIP_REACH_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Every 16x8/8x16 intra strip predicted, reaching or not -- the
    /// denominator [`RECT_STRIP_REACH_HITS`] is a fraction of.
    static RECT_STRIP_PRED_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT8_SPLIT_TX_HITS`].
pub(crate) fn rect8_split_tx_hits() -> usize {
    RECT8_SPLIT_TX_HITS.with(|c| c.get())
}

/// Current value of [`RECT_STRIP_PRED_HITS`].
pub(crate) fn rect_strip_pred_hits() -> usize {
    RECT_STRIP_PRED_HITS.with(|c| c.get())
}

/// Current value of [`RECT_STRIP_REACH_HITS`].
pub(crate) fn rect_strip_reach_hits() -> usize {
    RECT_STRIP_REACH_HITS.with(|c| c.get())
}

/// Current value of [`TX4X8_CODED_HITS`].
pub(crate) fn tx4x8_coded_hits() -> usize {
    TX4X8_CODED_HITS.with(|c| c.get())
}

/// Current value of [`TX8X4_CODED_HITS`].
pub(crate) fn tx8x4_coded_hits() -> usize {
    TX8X4_CODED_HITS.with(|c| c.get())
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
const H_PRED: usize = 2;
/// `D157_PRED`: `fimode_to_intradir`'s target for `FILTER_D157_PRED` (spec
/// `Fimode_To_Intradir`, libaom `blockd.h`).
const D157_PRED: usize = 6;
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
    class_scan_table_wh(side, side, class)
}

/// [`class_scan_table`] widened to `(w, h)` (lane-tx4x8). Checked against
/// libaom's own `mrow_scan_4x8`/`mcol_scan_4x8`/`mrow_scan_8x4`/
/// `mcol_scan_8x4` (`scan.c`) converted out of their column-major encoding
/// by `(p % h) * w + p / h`, the same conversion [`SCAN_16X8`] was
/// transcribed with -- `mrow` is the plain row-major raster in our indexing
/// and `mcol` its transpose, at rect sizes exactly as at square ones.
fn class_scan_table_wh(w: usize, h: usize, class: TxClass) -> Vec<u16> {
    match class {
        TxClass::Vert => (0..(w * h) as u16).collect(),
        TxClass::Horiz => (0..w)
            .flat_map(|col| (0..h).map(move |row| (row * w + col) as u16))
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
            // `TX_CLASS_VERT`/`TX_CLASS_HORIZ` (`txb_common.h`'s `tx_type_to_class`)
            // key on axis alone -- FLIPADST is still ADST for this purpose.
            TxType::VDct | TxType::VAdst | TxType::VFlipAdst => Self::Vert,
            TxType::HDct | TxType::HAdst | TxType::HFlipAdst => Self::Horiz,
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

fn base_ctx(
    grid: &[i32],
    side: usize,
    row: usize,
    col: usize,
    class: TxClass,
    rect_shape: Option<(usize, usize)>,
) -> usize {
    // libaom's `get_nz_map_ctx_from_stats` only special-cases the DC
    // coefficient to ctx 0 for `TX_CLASS_2D` (`(tx_class | coeff_idx) == 0`,
    // txb_common.h) -- `V_DCT`/`H_DCT` (class `Horiz`/`Vert`) still run the DC
    // position through the full 1D neighbour sum and offset table. Skipping
    // that for every class desynced the last (DC) coefficient of a
    // `TX_CLASS_HORIZ` block (lane-av1tx4 final round: real ctx 28 read as
    // 0, which then read one extra `BR` symbol at the max and forced a
    // spurious third Golomb call).
    if class == TxClass::TwoD && row == 0 && col == 0 {
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
        // libaom's CDF *set* for a truncated 64-wide/tall corner resolves
        // through `get_txsize_entropy_ctx` to the square `TX_64X64` (this
        // decoder's `TxbSet::Luma64`), but `get_nz_map_ctx_from_stats`
        // separately indexes `av1_nz_map_ctx_offset` by the raw, un-adjusted
        // `tx_size` -- `TX_32X64`/`TX_64X32` for a superblock-level HORZ/VERT
        // strip, which own a genuinely different table from the square one
        // (lane-sbpart r8 root cause: `(row=1, col=0)` reads ctx 2 under the
        // square table vs the real ctx 13 the rect table gives).
        TxClass::TwoD => {
            // lane-rectsplit r4: apply libaom's GENERATING RULE
            // (`txb_common.h:199-209`) directly instead of indexing the
            // transcribed 5x5 tables. `av1_nz_map_ctx_offset[tx_size]` is a
            // FLAT array read at `coeff_idx`, and `coeff_idx` is COLUMN-major
            // (libaom decomposes it as `col = coeff_idx >> bhl`, `bhl` the
            // ADJUSTED height's log2, 5 for both `TX_64X32` and `TX_32X64`),
            // so 32 consecutive entries are one COLUMN: the [`cdf`]
            // transcriptions are `[col][row]` and this read had them as
            // `[row][col]` -- the transpose, i.e. the other shape's rule with
            // 11 and 16 swapped. Same values as [`base_ctx_rect`] now, stated
            // once; `base_ctx_rect_offsets_match_the_transcribed_tables_over_the_whole_domain`
            // pins the rule against the tables over their whole 5x5 domain.
            if let Some((w, h)) = rect_shape {
                if std::env::var_os("EC_NZOFF_DUMP").is_some() {
                    eprintln!("NZOFF side={side} shape={w}x{h} row={row} col={col}");
                }
                if w < h && row < 2 {
                    return ctx + 11;
                }
                if w > h && col < 2 {
                    return ctx + 16;
                }
            }
            ctx + cdf::NZ_MAP_CTX_OFFSET_32[row.min(4)][col.min(4)] as usize
        }
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

/// `Default_Scan_32x16` (spec 8.4.2, libaom `scan.c`'s `default_scan_32x16`):
/// transposed from libaom's column-major buffer into this decoder's row-major
/// `pos = row * 32 + col` (lane-rectwire, verified as a bijection of `0..512`
/// against the real table). Not the square zigzag formula generalized to
/// unequal `w`/`h` -- checked and that does NOT reproduce this table.
const SCAN_32X16: [u16; 512] = [
    0, 32, 1, 64, 33, 2, 96, 65, 34, 3, 128, 97, 66, 35, 4, 160, 129, 98, 67, 36, 5, 192, 161,
    130, 99, 68, 37, 6, 224, 193, 162, 131, 100, 69, 38, 7, 256, 225, 194, 163, 132, 101, 70, 39,
    8, 288, 257, 226, 195, 164, 133, 102, 71, 40, 9, 320, 289, 258, 227, 196, 165, 134, 103, 72,
    41, 10, 352, 321, 290, 259, 228, 197, 166, 135, 104, 73, 42, 11, 384, 353, 322, 291, 260, 229,
    198, 167, 136, 105, 74, 43, 12, 416, 385, 354, 323, 292, 261, 230, 199, 168, 137, 106, 75, 44,
    13, 448, 417, 386, 355, 324, 293, 262, 231, 200, 169, 138, 107, 76, 45, 14, 480, 449, 418,
    387, 356, 325, 294, 263, 232, 201, 170, 139, 108, 77, 46, 15, 481, 450, 419, 388, 357, 326,
    295, 264, 233, 202, 171, 140, 109, 78, 47, 16, 482, 451, 420, 389, 358, 327, 296, 265, 234,
    203, 172, 141, 110, 79, 48, 17, 483, 452, 421, 390, 359, 328, 297, 266, 235, 204, 173, 142,
    111, 80, 49, 18, 484, 453, 422, 391, 360, 329, 298, 267, 236, 205, 174, 143, 112, 81, 50, 19,
    485, 454, 423, 392, 361, 330, 299, 268, 237, 206, 175, 144, 113, 82, 51, 20, 486, 455, 424,
    393, 362, 331, 300, 269, 238, 207, 176, 145, 114, 83, 52, 21, 487, 456, 425, 394, 363, 332,
    301, 270, 239, 208, 177, 146, 115, 84, 53, 22, 488, 457, 426, 395, 364, 333, 302, 271, 240,
    209, 178, 147, 116, 85, 54, 23, 489, 458, 427, 396, 365, 334, 303, 272, 241, 210, 179, 148,
    117, 86, 55, 24, 490, 459, 428, 397, 366, 335, 304, 273, 242, 211, 180, 149, 118, 87, 56, 25,
    491, 460, 429, 398, 367, 336, 305, 274, 243, 212, 181, 150, 119, 88, 57, 26, 492, 461, 430,
    399, 368, 337, 306, 275, 244, 213, 182, 151, 120, 89, 58, 27, 493, 462, 431, 400, 369, 338,
    307, 276, 245, 214, 183, 152, 121, 90, 59, 28, 494, 463, 432, 401, 370, 339, 308, 277, 246,
    215, 184, 153, 122, 91, 60, 29, 495, 464, 433, 402, 371, 340, 309, 278, 247, 216, 185, 154,
    123, 92, 61, 30, 496, 465, 434, 403, 372, 341, 310, 279, 248, 217, 186, 155, 124, 93, 62, 31,
    497, 466, 435, 404, 373, 342, 311, 280, 249, 218, 187, 156, 125, 94, 63, 498, 467, 436, 405,
    374, 343, 312, 281, 250, 219, 188, 157, 126, 95, 499, 468, 437, 406, 375, 344, 313, 282, 251,
    220, 189, 158, 127, 500, 469, 438, 407, 376, 345, 314, 283, 252, 221, 190, 159, 501, 470, 439,
    408, 377, 346, 315, 284, 253, 222, 191, 502, 471, 440, 409, 378, 347, 316, 285, 254, 223, 503,
    472, 441, 410, 379, 348, 317, 286, 255, 504, 473, 442, 411, 380, 349, 318, 287, 505, 474, 443,
    412, 381, 350, 319, 506, 475, 444, 413, 382, 351, 507, 476, 445, 414, 383, 508, 477, 446, 415,
    509, 478, 447, 510, 479, 511,
];

/// `Default_Scan_16x32`, this decoder's own row-major transcription (see
/// [`SCAN_32X16`]'s doc comment).
const SCAN_16X32: [u16; 512] = [
    0, 1, 16, 2, 17, 32, 3, 18, 33, 48, 4, 19, 34, 49, 64, 5, 20, 35, 50, 65, 80, 6, 21, 36, 51,
    66, 81, 96, 7, 22, 37, 52, 67, 82, 97, 112, 8, 23, 38, 53, 68, 83, 98, 113, 128, 9, 24, 39, 54,
    69, 84, 99, 114, 129, 144, 10, 25, 40, 55, 70, 85, 100, 115, 130, 145, 160, 11, 26, 41, 56, 71,
    86, 101, 116, 131, 146, 161, 176, 12, 27, 42, 57, 72, 87, 102, 117, 132, 147, 162, 177, 192,
    13, 28, 43, 58, 73, 88, 103, 118, 133, 148, 163, 178, 193, 208, 14, 29, 44, 59, 74, 89, 104,
    119, 134, 149, 164, 179, 194, 209, 224, 15, 30, 45, 60, 75, 90, 105, 120, 135, 150, 165, 180,
    195, 210, 225, 240, 31, 46, 61, 76, 91, 106, 121, 136, 151, 166, 181, 196, 211, 226, 241, 256,
    47, 62, 77, 92, 107, 122, 137, 152, 167, 182, 197, 212, 227, 242, 257, 272, 63, 78, 93, 108,
    123, 138, 153, 168, 183, 198, 213, 228, 243, 258, 273, 288, 79, 94, 109, 124, 139, 154, 169,
    184, 199, 214, 229, 244, 259, 274, 289, 304, 95, 110, 125, 140, 155, 170, 185, 200, 215, 230,
    245, 260, 275, 290, 305, 320, 111, 126, 141, 156, 171, 186, 201, 216, 231, 246, 261, 276, 291,
    306, 321, 336, 127, 142, 157, 172, 187, 202, 217, 232, 247, 262, 277, 292, 307, 322, 337, 352,
    143, 158, 173, 188, 203, 218, 233, 248, 263, 278, 293, 308, 323, 338, 353, 368, 159, 174, 189,
    204, 219, 234, 249, 264, 279, 294, 309, 324, 339, 354, 369, 384, 175, 190, 205, 220, 235, 250,
    265, 280, 295, 310, 325, 340, 355, 370, 385, 400, 191, 206, 221, 236, 251, 266, 281, 296, 311,
    326, 341, 356, 371, 386, 401, 416, 207, 222, 237, 252, 267, 282, 297, 312, 327, 342, 357, 372,
    387, 402, 417, 432, 223, 238, 253, 268, 283, 298, 313, 328, 343, 358, 373, 388, 403, 418, 433,
    448, 239, 254, 269, 284, 299, 314, 329, 344, 359, 374, 389, 404, 419, 434, 449, 464, 255, 270,
    285, 300, 315, 330, 345, 360, 375, 390, 405, 420, 435, 450, 465, 480, 271, 286, 301, 316, 331,
    346, 361, 376, 391, 406, 421, 436, 451, 466, 481, 496, 287, 302, 317, 332, 347, 362, 377, 392,
    407, 422, 437, 452, 467, 482, 497, 303, 318, 333, 348, 363, 378, 393, 408, 423, 438, 453, 468,
    483, 498, 319, 334, 349, 364, 379, 394, 409, 424, 439, 454, 469, 484, 499, 335, 350, 365, 380,
    395, 410, 425, 440, 455, 470, 485, 500, 351, 366, 381, 396, 411, 426, 441, 456, 471, 486, 501,
    367, 382, 397, 412, 427, 442, 457, 472, 487, 502, 383, 398, 413, 428, 443, 458, 473, 488, 503,
    399, 414, 429, 444, 459, 474, 489, 504, 415, 430, 445, 460, 475, 490, 505, 431, 446, 461, 476,
    491, 506, 447, 462, 477, 492, 507, 463, 478, 493, 508, 479, 494, 509, 495, 510, 511,
];

/// `Default_Scan_4x8` (libaom `scan.c`), this decoder's own row-major
/// transcription (see [`SCAN_32X16`]'s doc comment for the conversion, which
/// was re-verified this lane by regenerating [`SCAN_16X8`]/[`SCAN_8X16`] from
/// the same source).
const SCAN_4X8: [u16; 32] = [
    0, 1, 4, 2, 5, 8, 3, 6, 9, 12, 7, 10, 13, 16, 11, 14, 17, 20, 15, 18, 21, 24, 19, 22, 25, 28,
    23, 26, 29, 27, 30, 31,
];

/// `Default_Scan_8x4`, same source and conversion as [`SCAN_4X8`].
const SCAN_8X4: [u16; 32] = [
    0, 8, 1, 16, 9, 2, 24, 17, 10, 3, 25, 18, 11, 4, 26, 19, 12, 5, 27, 20, 13, 6, 28, 21, 14, 7,
    29, 22, 15, 30, 23, 31,
];

/// `Default_Scan_16x8`, this decoder's own row-major transcription (see
/// [`SCAN_32X16`]'s doc comment).
const SCAN_16X8: [u16; 128] = [
    0, 16, 1, 32, 17, 2, 48, 33, 18, 3, 64, 49, 34, 19, 4, 80, 65, 50, 35, 20, 5, 96, 81, 66, 51,
    36, 21, 6, 112, 97, 82, 67, 52, 37, 22, 7, 113, 98, 83, 68, 53, 38, 23, 8, 114, 99, 84, 69, 54,
    39, 24, 9, 115, 100, 85, 70, 55, 40, 25, 10, 116, 101, 86, 71, 56, 41, 26, 11, 117, 102, 87,
    72, 57, 42, 27, 12, 118, 103, 88, 73, 58, 43, 28, 13, 119, 104, 89, 74, 59, 44, 29, 14, 120,
    105, 90, 75, 60, 45, 30, 15, 121, 106, 91, 76, 61, 46, 31, 122, 107, 92, 77, 62, 47, 123, 108,
    93, 78, 63, 124, 109, 94, 79, 125, 110, 95, 126, 111, 127,
];

/// `Default_Scan_8x16`, this decoder's own row-major transcription (see
/// [`SCAN_32X16`]'s doc comment).
const SCAN_8X16: [u16; 128] = [
    0, 1, 8, 2, 9, 16, 3, 10, 17, 24, 4, 11, 18, 25, 32, 5, 12, 19, 26, 33, 40, 6, 13, 20, 27, 34,
    41, 48, 7, 14, 21, 28, 35, 42, 49, 56, 15, 22, 29, 36, 43, 50, 57, 64, 23, 30, 37, 44, 51, 58,
    65, 72, 31, 38, 45, 52, 59, 66, 73, 80, 39, 46, 53, 60, 67, 74, 81, 88, 47, 54, 61, 68, 75, 82,
    89, 96, 55, 62, 69, 76, 83, 90, 97, 104, 63, 70, 77, 84, 91, 98, 105, 112, 71, 78, 85, 92, 99,
    106, 113, 120, 79, 86, 93, 100, 107, 114, 121, 87, 94, 101, 108, 115, 122, 95, 102, 109, 116,
    123, 103, 110, 117, 124, 111, 118, 125, 119, 126, 127,
];

/// `Default_Scan_32x8`, this decoder's own row-major transcription (see
/// [`SCAN_32X16`]'s doc comment) -- the chroma plane of a 64x16
/// `PARTITION_HORZ_4` superblock strip (lane-tx64x16). libaom stores its
/// scans column-major (`p = col * height + row`), so a naive copy of
/// `default_scan_32x8` would silently install the TRANSPOSED 8x32 order
/// (class reference-layout-not-spec): `default_scan_16x8`'s literal bytes
/// are byte-identical to this decoder's [`SCAN_8X16`].
const SCAN_32X8: [u16; 256] = [
    0, 32, 1, 64, 33, 2, 96, 65, 34, 3, 128, 97, 66, 35, 4, 160, 129, 98, 67, 36, 5, 192, 161, 130,
    99, 68, 37, 6, 224, 193, 162, 131, 100, 69, 38, 7, 225, 194, 163, 132, 101, 70, 39, 8, 226, 195,
    164, 133, 102, 71, 40, 9, 227, 196, 165, 134, 103, 72, 41, 10, 228, 197, 166, 135, 104, 73, 42,
    11, 229, 198, 167, 136, 105, 74, 43, 12, 230, 199, 168, 137, 106, 75, 44, 13, 231, 200, 169,
    138, 107, 76, 45, 14, 232, 201, 170, 139, 108, 77, 46, 15, 233, 202, 171, 140, 109, 78, 47, 16,
    234, 203, 172, 141, 110, 79, 48, 17, 235, 204, 173, 142, 111, 80, 49, 18, 236, 205, 174, 143,
    112, 81, 50, 19, 237, 206, 175, 144, 113, 82, 51, 20, 238, 207, 176, 145, 114, 83, 52, 21, 239,
    208, 177, 146, 115, 84, 53, 22, 240, 209, 178, 147, 116, 85, 54, 23, 241, 210, 179, 148, 117,
    86, 55, 24, 242, 211, 180, 149, 118, 87, 56, 25, 243, 212, 181, 150, 119, 88, 57, 26, 244, 213,
    182, 151, 120, 89, 58, 27, 245, 214, 183, 152, 121, 90, 59, 28, 246, 215, 184, 153, 122, 91,
    60, 29, 247, 216, 185, 154, 123, 92, 61, 30, 248, 217, 186, 155, 124, 93, 62, 31, 249, 218, 187,
    156, 125, 94, 63, 250, 219, 188, 157, 126, 95, 251, 220, 189, 158, 127, 252, 221, 190, 159, 253,
    222, 191, 254, 223, 255,
];
/// `Default_Scan_8x32`, [`SCAN_32X8`]'s transpose (the chroma plane of a
/// 16x64 `PARTITION_VERT_4` strip).
const SCAN_8X32: [u16; 256] = [
    0, 1, 8, 2, 9, 16, 3, 10, 17, 24, 4, 11, 18, 25, 32, 5, 12, 19, 26, 33, 40, 6, 13, 20, 27, 34,
    41, 48, 7, 14, 21, 28, 35, 42, 49, 56, 15, 22, 29, 36, 43, 50, 57, 64, 23, 30, 37, 44, 51, 58,
    65, 72, 31, 38, 45, 52, 59, 66, 73, 80, 39, 46, 53, 60, 67, 74, 81, 88, 47, 54, 61, 68, 75, 82,
    89, 96, 55, 62, 69, 76, 83, 90, 97, 104, 63, 70, 77, 84, 91, 98, 105, 112, 71, 78, 85, 92, 99,
    106, 113, 120, 79, 86, 93, 100, 107, 114, 121, 128, 87, 94, 101, 108, 115, 122, 129, 136, 95,
    102, 109, 116, 123, 130, 137, 144, 103, 110, 117, 124, 131, 138, 145, 152, 111, 118, 125, 132,
    139, 146, 153, 160, 119, 126, 133, 140, 147, 154, 161, 168, 127, 134, 141, 148, 155, 162, 169,
    176, 135, 142, 149, 156, 163, 170, 177, 184, 143, 150, 157, 164, 171, 178, 185, 192, 151, 158,
    165, 172, 179, 186, 193, 200, 159, 166, 173, 180, 187, 194, 201, 208, 167, 174, 181, 188, 195,
    202, 209, 216, 175, 182, 189, 196, 203, 210, 217, 224, 183, 190, 197, 204, 211, 218, 225, 232,
    191, 198, 205, 212, 219, 226, 233, 240, 199, 206, 213, 220, 227, 234, 241, 248, 207, 214, 221,
    228, 235, 242, 249, 215, 222, 229, 236, 243, 250, 223, 230, 237, 244, 251, 231, 238, 245, 252,
    239, 246, 253, 247, 254, 255,
];
/// `Default_Scan_16x4`, this decoder's own row-major transcription of
/// libaom's column-major `default_scan_16x4` (`scan.c:66`) -- the CHROMA
/// plane of a 32x8 `PARTITION_HORZ_4` strip at the 32x32 level
/// (lane-tx64x16 r3). The transcription rule (`row * w + col` with
/// `row = p % h`, `col = p / h`) was validated by REGENERATING the existing
/// [`SCAN_16X8`] from `default_scan_16x8` byte for byte before these two
/// were emitted -- a naive copy installs the transpose (class
/// reference-layout-not-spec).
const SCAN_16X4: [u16; 64] = [
    0, 16, 1, 32, 17, 2, 48, 33, 18, 3, 49, 34, 19, 4, 50, 35, 20, 5, 51, 36, 21, 6, 52, 37, 22, 7,
    53, 38, 23, 8, 54, 39, 24, 9, 55, 40, 25, 10, 56, 41, 26, 11, 57, 42, 27, 12, 58, 43, 28, 13,
    59, 44, 29, 14, 60, 45, 30, 15, 61, 46, 31, 62, 47, 63,
];
/// `Default_Scan_4x16`, [`SCAN_16X4`]'s transpose (the chroma plane of an
/// 8x32 `PARTITION_VERT_4` strip).
const SCAN_4X16: [u16; 64] = [
    0, 1, 4, 2, 5, 8, 3, 6, 9, 12, 7, 10, 13, 16, 11, 14, 17, 20, 15, 18, 21, 24, 19, 22, 25, 28,
    23, 26, 29, 32, 27, 30, 33, 36, 31, 34, 37, 40, 35, 38, 41, 44, 39, 42, 45, 48, 43, 46, 49, 52,
    47, 50, 53, 56, 51, 54, 57, 60, 55, 58, 61, 59, 62, 63,
];

/// [`neighbour`] with independent `w` (row stride)/`h` (row-bound) extents
/// (lane-rectwire, mirrors [`record_rect`](Neighbours::record_rect)'s
/// asymmetric-extent pattern).
fn neighbour_rect(grid: &[i32], w: usize, h: usize, row: usize, col: usize) -> i32 {
    if row >= h || col >= w {
        0
    } else {
        grid[row * w + col]
    }
}

/// [`base_ctx`]'s `TxClass::TwoD` arm widened to `(w, h)` (lane-rectwire):
/// the rect generalization is libaom's own comment in
/// `get_nz_map_ctx_from_stats` (`txb_common.h:189-224`), reduces to the exact
/// same [`cdf::NZ_MAP_CTX_OFFSET_32`] table when `w == h`. Never called for
/// `V_DCT`/`H_DCT` -- geometrically impossible at the two size pairs
/// [`decode_block_rect`] codes (see that function's own doc comment).
fn base_ctx_rect(
    grid: &[i32],
    w: usize,
    h: usize,
    row: usize,
    col: usize,
    class: TxClass,
) -> usize {
    // Same `(tx_class | coeff_idx) == 0` guard [`base_ctx`] documents: only
    // the 2D class short-circuits the DC position.
    if class == TxClass::TwoD && row == 0 && col == 0 {
        return 0;
    }
    let offsets: [(usize, usize); 5] = match class {
        TxClass::TwoD => [(1, 0), (0, 1), (1, 1), (2, 0), (0, 2)],
        TxClass::Horiz => [(1, 0), (0, 1), (0, 2), (0, 3), (0, 4)],
        TxClass::Vert => [(1, 0), (0, 1), (2, 0), (3, 0), (4, 0)],
    };
    let mag: i32 = offsets
        .iter()
        .map(|&(dr, dc)| neighbour_rect(grid, w, h, row + dr, col + dc).abs().min(3))
        .sum();
    let ctx = ((mag + 1) >> 1).min(4) as usize;
    match class {
        TxClass::TwoD => {
            if w < h && row < 2 {
                return ctx + 11;
            }
            if w > h && col < 2 {
                return ctx + 16;
            }
            ctx + cdf::NZ_MAP_CTX_OFFSET_32[row.min(4)][col.min(4)] as usize
        }
        // Shape-independent in libaom too (`get_nz_map_ctx_from_stats`'s
        // `TX_CLASS_HORIZ`/`VERT` arms index `nz_map_ctx_offset_1d` by the
        // position along one axis only).
        TxClass::Horiz => ctx + nz_map_ctx_offset_1d(col.min(31)),
        TxClass::Vert => ctx + nz_map_ctx_offset_1d(row.min(31)),
    }
}

/// [`br_ctx`]'s `TxClass::TwoD` arm widened to `(w, h)` (lane-rectwire): the
/// neighbour-offset math itself is shape-independent, only the boundary
/// clamp needs the real bound in each axis.
fn br_ctx_rect(
    grid: &[i32],
    w: usize,
    h: usize,
    row: usize,
    col: usize,
    class: TxClass,
) -> usize {
    let extra = match class {
        TxClass::TwoD => neighbour_rect(grid, w, h, row + 1, col + 1).abs(),
        TxClass::Horiz => neighbour_rect(grid, w, h, row, col + 2).abs(),
        TxClass::Vert => neighbour_rect(grid, w, h, row + 2, col).abs(),
    };
    let mag = neighbour_rect(grid, w, h, row + 1, col).abs()
        + neighbour_rect(grid, w, h, row, col + 1).abs()
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
    // lane-scaledref r1: this cap MASKS A REAL DEFECT and must not be lifted
    // on its own. Reading up to 32 leading zeros (dav1d's `len < 32`; libaom
    // itself calls a 21st bit a corrupt frame, decodetxb.c:30) is bit-identical
    // for every tail any encoder writes -- our own writer tops out at 19 zeros
    // (`tile::MAX_LEVEL == MAX_BR_LEVEL + (1 << 19)`) -- and was implemented
    // and round-trip tested here (commit ee1f980, reverted in 314ee08). It
    // turned `a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact`
    // RED: seed 67 stops here today, and with the cap raised it decodes to a
    // frame-0 LUMA MISMATCH instead. A key frame cannot legitimately carry a
    // level above 1<<19, so the long tail is the SYMPTOM of an earlier desync
    // in that intra rect64 stream -- class `refusal-hides-a-defect`. Lift this
    // together with that defect's fix, not before.
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
    if std::env::var_os("EC_AV1_EOBPT_CDF").is_some() {
        let (range, value) = dec.debug_state();
        eprintln!("EC_AV1_EOBPT_CDF {eob_pt:?} range={range} value={value}");
    }
    let group = dec.symbol(eob_pt) + 1;
    if trace {
        eprintln!("TRACE eob_pt value={group} rng={}", dec.debug_state().0);
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
/// `Intra_Mode_To_Tx_Type` (spec 9.3): chroma's transform type is never its
/// own coded symbol -- `TxbSet::Chroma*`'s `tx_type` slot is always `None`
/// (a chroma block shares no CDF row with `Luma16`'s mode-indexed one) -- so
/// libaom's own `intra_mode_to_tx_type` (`blockd.h`) derives it purely from
/// the plane's own predicted mode instead. Every entry this table can
/// produce is a member of both `TX_SET_INTRA_1` and `_2` (spec 9.3's two
/// intra sets differ only in `IDTX`/`V_DCT`/`H_DCT`, none of which this
/// table ever names), so unlike the CDF-coded luma path there is no
/// ext-tx-set membership check to fall back from here.
fn default_intra_tx_type(mode: u8) -> TxType {
    use crate::intra::{
        D45_PRED, D67_PRED, D113_PRED, D135_PRED, D157_PRED, D203_PRED, DC_PRED, H_PRED,
        PAETH_PRED, SMOOTH_H_PRED, SMOOTH_PRED, SMOOTH_V_PRED, V_PRED,
    };
    match mode {
        DC_PRED | D45_PRED => TxType::DctDct,
        V_PRED | D113_PRED | D67_PRED | SMOOTH_V_PRED => TxType::AdstDct,
        H_PRED | D157_PRED | D203_PRED | SMOOTH_H_PRED => TxType::DctAdst,
        D135_PRED | SMOOTH_PRED | PAETH_PRED => TxType::AdstAdst,
        other => panic!("intra mode {other} has no Intra_Mode_To_Tx_Type entry"),
    }
}

/// `is_smooth` (libaom `reconintra.c`): true for `SMOOTH_PRED..=SMOOTH_H_PRED`
/// (9..=11) — deliberately excludes `PAETH_PRED` (12), which the spec's own
/// `get_intra_edge_filter_type`/libaom `get_filt_type` never treat as smooth.
fn is_smooth_mode(mode: usize) -> bool {
    (crate::intra::SMOOTH_PRED as usize..=crate::intra::SMOOTH_H_PRED as usize).contains(&mode)
}

fn read_coeffs(
    dec: &mut SymbolDecoder,
    coding: &mut TxbTables,
    scan: &[u16],
    skip_ctx: usize,
    sign_ctx: usize,
    // The chroma default (spec `Intra_Mode_To_Tx_Type`, see
    // [`default_intra_tx_type`]) for the sizes where `coding.tx_type` codes
    // nothing at all; `DctDct` for every luma call and every chroma call at
    // a size the spec forces to `DCT_DCT` regardless of mode (32-point and
    // up, `EXT_TX_SET_DCTONLY`) -- the caller already folds that in.
    default_tx_type: TxType,
    // `Some((w, h))` with `w != h` when this read is a superblock-level
    // HORZ/VERT strip's truncated luma corner: the CDF *set* resolves square
    // (`side`/`coding` above), but `av1_nz_map_ctx_offset`'s position table
    // is indexed by the real, un-adjusted rectangular shape (lane-sbpart r8
    // root cause, see [`base_ctx`]). `None` everywhere else -- every other
    // caller's block genuinely is `side` x `side`.
    rect_shape: Option<(usize, usize)>,
) -> Result<(Vec<i32>, TxType)> {
    let trace = std::env::var_os("EC_AV1_TRACE").is_some();
    let side = coding.side;
    let mut grid = vec![0i32; side * side];
    if std::env::var_os("EC_AV1_EOBPT_CDF").is_some() {
        let (range, value) = dec.debug_state();
        eprintln!("EC_AV1_STATE_BEFORE_TXBSKIP range={range} value={value}");
    }
    let all_zero = dec.symbol(&mut coding.txb_skip[skip_ctx]) == 1;
    if std::env::var_os("EC_TRACE_COEFF").is_some() {
        let (rng, _) = dec.debug_state();
        eprintln!(
            "EC_COEFF_STEP tag=all_zero ctx={skip_ctx} all_zero={} rng={rng}",
            all_zero as i32
        );
    }
    if std::env::var_os("EC_AV1_TELL").is_some() {
        eprintln!(
            "TELL label=post_txb_skip ctx={skip_ctx} all_zero={} tell={} range={}",
            all_zero as u8, dec.debug_bitpos(), dec.debug_state().0
        );
    }
    if trace {
        eprintln!("TRACE all_zero ctx={skip_ctx} value={}", all_zero as i32);
    }
    if std::env::var_os("EC_AV1_EOBPT_CDF").is_some() {
        let (range, value) = dec.debug_state();
        eprintln!("EC_AV1_STATE_AFTER_TXBSKIP range={range} value={value}");
    }
    if all_zero {
        return Ok((grid, TxType::DctDct));
    }
    let mut tx_type = default_tx_type;
    if let Some(tx_type_cdf) = coding.tx_type.as_deref_mut() {
        // The CDF row's own width names its set (lane-cdffwd2: extended
        // past the original two-way intra split once wider `reduced_tx_set
        // == 0` inter sets joined the table): 17 slots (16 symbols) is
        // `EXT_TX_SET_ALL16`, 13 (12 symbols) `EXT_TX_SET_DTT9_IDTX_1DDCT`,
        // 8 (7 symbols) `TX_SET_INTRA_1`, everything else (6 slots, 5
        // symbols) the reduced `TX_SET_INTRA_2`/`TX_SET_INTER_3` two sets
        // share their symbol order with (`Tx_Type_Inter_Inv_Set3`'s two
        // members are a prefix of `Tx_Type_Intra_Inv_Set2`'s five).
        let len = tx_type_cdf.len();
        if std::env::var_os("EC_AV1_EOBPT_CDF").is_some() && len == 3 {
            eprintln!("EC_AV1_TXTYPE32_CDF {tx_type_cdf:?}");
        }
        let t = dec.symbol(tx_type_cdf);
        if std::env::var_os("EC_TRACE_COEFF").is_some() {
            let (rng, _) = dec.debug_state();
            eprintln!("EC_COEFF_STEP tag=tx_type rng={rng}");
        }
        if std::env::var_os("EC_AV1_EOBPT_CDF").is_some() {
            let (range, value) = dec.debug_state();
            eprintln!("EC_AV1_STATE_AFTER_TXTYPE range={range} value={value}");
        }
        if trace {
            eprintln!("TRACE tx_type value={t} len={len}");
        }
        tx_type = match len {
            17 => TxType::from_symbol_all16(t),
            13 => TxType::from_symbol_set2_12(t),
            8 => TxType::from_symbol_set1(t),
            _ => TxType::from_symbol(t),
        }
        .ok_or_else(|| unsupported(format!("a tx_type symbol outside its CDF's own set: {t}")))?;
    }

    let class = TxClass::of(tx_type);
    if class != TxClass::TwoD {
        TX_CLASS1_HITS.with(|c| c.set(c.get() + 1));
    }
    let eob = read_eob(dec, coding, class);
    if trace {
        eprintln!("TRACE eob value={eob}");
    }
    let ec_trace_coeff = std::env::var_os("EC_TRACE_COEFF").is_some();
    if ec_trace_coeff {
        let (rng, _) = dec.debug_state();
        let cls = match class { TxClass::TwoD => "2d", TxClass::Horiz => "horiz", TxClass::Vert => "vert" };
        eprintln!("EC_COEFF_STEP tag=eob eob={eob} tx={tx_type:?} class={cls} rng={rng}");
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
            let ctx = base_ctx(&levels, side, row, col, class, rect_shape);
            let v = dec.symbol(&mut coding.base[ctx]) as i32;
            if ec_trace_coeff {
                let (rng, _) = dec.debug_state();
                // `pos` in libaom's column-major `coeff_idx` convention, so
                // the ladder lines up with instrumented `aomdec`.
                let apos = col * side + row;
                eprintln!(
                    "EC_COEFF_STEP tag=base c={scan_idx} pos={apos} ctx={ctx} level={v} rng={rng}"
                );
            }
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
        if ec_trace_coeff && scan_idx == eob - 1 {
            let (rng, _) = dec.debug_state();
            eprintln!("EC_COEFF_STEP tag=base_eob level={level} rng={rng}");
        }
        levels[pos] = level;
    }
    if ec_trace_coeff {
        let (rng, _) = dec.debug_state();
        eprintln!("EC_COEFF_STEP tag=after_bases rng={rng}");
    }

    for (c, &pos) in scan[..eob].iter().enumerate() {
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
        if ec_trace_coeff {
            let (rng, _) = dec.debug_state();
            eprintln!("EC_COEFF_STEP tag=sign c={c} sign={} rng={rng}", negative as i32);
        }
        let level = if level.abs_diff(0) as i32 > MAX_BR_LEVEL {
            let g = read_golomb(dec)?;
            if trace {
                eprintln!("TRACE golomb pos={pos} value={g}");
            }
            level + g as i32
        } else {
            level
        };
        if ec_trace_coeff {
            let (rng, _) = dec.debug_state();
            eprintln!("EC_COEFF_STEP tag=post_golomb c={c} level={level} rng={rng}");
        }
        grid[pos as usize] = if negative { -level } else { level };
    }
    Ok((grid, tx_type))
}

/// [`read_coeffs`] widened to `(w, h)` (lane-rectwire; lane-rectx added the
/// `tx_type` read [`TxbSet::LumaRect16x8`] needs), restricted to
/// `TxClass::TwoD` -- the only class any rect size pair this decoder codes
/// can ever produce, since every `tx_type` this decoder's rect sets can read
/// (`EXT_TX_SET_DTT4_IDTX`'s five members) is itself 2D: refuses by name
/// rather than guess-decode if a non-2D class ever shows up here, since
/// neither `base_ctx_rect`/`br_ctx_rect` nor [`class_scan_table`] have a rect
/// form for those.
fn read_coeffs_rect(
    dec: &mut SymbolDecoder,
    coding: &mut TxbTables,
    scan: &[u16],
    w: usize,
    h: usize,
    skip_ctx: usize,
    sign_ctx: usize,
    default_tx_type: TxType,
) -> Result<(Vec<i32>, TxType)> {
    let rect_trace = std::env::var_os("EC_TRACE_COEFF").is_some();
    let mut grid = vec![0i32; w * h];
    let entry_rng = dec.debug_state().0;
    let all_zero = dec.symbol(&mut coding.txb_skip[skip_ctx]) == 1;
    if rect_trace {
        eprintln!(
            "EC_COEFF_STEP tag=all_zero plane=0 ctx={skip_ctx} all_zero={} rng={}",
            all_zero as i32,
            dec.debug_state().0
        );
    }
    if all_zero {
        return Ok((grid, TxType::DctDct));
    }
    // lane-tx4x8: a 4x8/8x4 luma TU DOES carry a `tx_type` symbol (its set is
    // `EXT_TX_SET_DTT4_IDTX(_1DDCT)`, `tx_size_sqr_up == TX_8X8`), including
    // the `V_DCT`/`H_DCT` members whose 1D tx class needs its own scan and
    // context offsets -- both of which now have rect forms
    // ([`class_scan_table_wh`], [`base_ctx_rect`]/[`br_ctx_rect`]'s `class`).
    // The rect sets that carry no symbol at all still pass `None` here.
    let mut tx_type = default_tx_type;
    if let Some(tx_type_cdf) = coding.tx_type.as_deref_mut() {
        let len = tx_type_cdf.len();
        let t = dec.symbol(tx_type_cdf);
        tx_type = match len {
            17 => TxType::from_symbol_all16(t),
            13 => TxType::from_symbol_set2_12(t),
            8 => TxType::from_symbol_set1(t),
            _ => TxType::from_symbol(t),
        }
        .ok_or_else(|| unsupported(format!("a tx_type symbol outside its CDF's own set: {t}")))?;
        if rect_trace {
            eprintln!("EC_COEFF_STEP tag=tx_type plane=0 rng={}", dec.debug_state().0);
        }
    }
    let class = TxClass::of(tx_type);
    if class != TxClass::TwoD {
        TX_CLASS1_HITS.with(|c| c.set(c.get() + 1));
    }
    let eob = read_eob(dec, coding, class);
    if rect_trace {
        eprintln!("EC_COEFF_STEP tag=eob eob={eob} rng={}", dec.debug_state().0);
    }
    let class_scan;
    let scan: &[u16] = if class == TxClass::TwoD {
        scan
    } else {
        class_scan = class_scan_table_wh(w, h, class);
        &class_scan
    };
    let mut levels = vec![0i32; w * h];
    for scan_idx in (0..eob).rev() {
        let pos = scan[scan_idx] as usize;
        let (row, col) = (pos / w, pos % w);
        // libaom's column-major `coeff_idx`, for ladder comparison.
        let apos = col * h + row;
        let level = if scan_idx == eob - 1 {
            let ctx = eob_coeff_ctx(scan_idx, w * h);
            let v = dec.symbol(&mut coding.base_eob[ctx]) as i32 + 1;
            if rect_trace {
                eprintln!(
                    "EC_COEFF_STEP tag=base_eob c={scan_idx} pos={pos} ctx={ctx} level={v} rng={}",
                    dec.debug_state().0
                );
            }
            v
        } else {
            let ctx = base_ctx_rect(&levels, w, h, row, col, class);
            let v = dec.symbol(&mut coding.base[ctx]) as i32;
            if rect_trace {
                eprintln!(
                    "EC_COEFF_STEP tag=base c={scan_idx} pos={pos} ctx={ctx} level={v} rng={}",
                    dec.debug_state().0
                );
            }
            v
        };
        let level = if level > NUM_BASE_LEVELS {
            let ctx = br_ctx_rect(&levels, w, h, row, col, class);
            let mut level = level;
            let mut sent = 0;
            loop {
                let k = dec.symbol(&mut coding.br[ctx]) as i32;
                if rect_trace {
                    eprintln!(
                        "EC_COEFF_STEP tag=br c={scan_idx} pos={pos} ctx={ctx} k={k} rng={}",
                        dec.debug_state().0
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
        if rect_trace {
            let (rng, _) = dec.debug_state();
            eprintln!("EC_COEFF_STEP tag=base c={scan_idx} pos={pos} level={level} rng={rng}");
        }
    }
    if rect_trace {
        let (rng, _) = dec.debug_state();
        eprintln!("EC_COEFF_STEP tag=after_bases rng={rng}");
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
        if rect_trace {
            let (rng, _) = dec.debug_state();
            eprintln!("EC_COEFF_STEP tag=sign_rect pos={pos} sign={} rng={rng}", u8::from(negative));
        }
        let level = if level.abs_diff(0) as i32 > MAX_BR_LEVEL {
            let g = read_golomb(dec)?;
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
    /// Chroma's own `uv_mode` companion to `above_mode`/`left_mode` (lane-chroma
    /// r3): a smooth/paeth `uv_mode`'s edge-filter strength (spec
    /// `get_intra_edge_filter_type`) reads the CHROMA neighbour's mode, not the
    /// luma one -- the same block can (and often does) code a different
    /// `uv_mode` than its own `mode`. Coarse [`SUB`]-grid like `above_mode`;
    /// [`Self::decode_leaf8`]'s per-leaf `prev_leaf` override (a real
    /// within-block neighbour finer than this grid) is luma-only -- a known,
    /// unexercised-by-gate corner-cut (every real-encoder gate in `stream.rs`
    /// uses `--min-partition-size=16`, so no leaf8 8x8 split ever reaches this).
    above_uv_mode: Vec<usize>,
    left_uv_mode: Vec<usize>,
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
    /// The luma `mode` of the last `BLOCK_4X4` leaf decoded at each mi column
    /// / row, with the mi position it was written at
    /// (`(usize::MAX, _)` = never written). `above_mode`/`left_mode` are on
    /// the coarse 16x16 grid, which cannot distinguish the two 4x4 leaves an
    /// 8x8 `PARTITION_SPLIT` puts in one mi column; libaom's intra-mode
    /// context reads `mi(row, col-1)`/`mi(row-1, col)` exactly, so a sub-8x8
    /// leaf whose neighbour is another split group needs that group's leaf 3
    /// (bottom-right), not the leaf the coarse slot happens to hold. Written
    /// and read only by [`decode_leaf_split4`]; the position guard makes it
    /// fall back to the coarse slot whenever the real neighbour was a block
    /// of 8x8 or larger (whose one mode is correct for every row it spans).
    sub8_mode_col: Vec<(usize, usize)>,
    sub8_mode_row: Vec<(usize, usize)>,
    /// The `uv_mode` twin of `sub8_mode_col`/`sub8_mode_row` (lane-sub8 r5):
    /// chroma's edge-filter type reads the CHROMA neighbour's `uv_mode`
    /// (`get_intra_edge_filter_type`, reconintra.c:974), and the coarse
    /// [`SUB`] slots cannot name it once a 16x16 is split into 8x8 leaves --
    /// the leaf above writes the same row slot the block to the left owns.
    uv_mode_col: Vec<(usize, usize)>,
    uv_mode_row: Vec<(usize, usize)>,
    /// Whether the block above/left of this column/row was coded `skip` --
    /// an inter frame's own `skip` context (spec `SkipContext`), which the
    /// key-frame writer never tracks (its skip context is always zero); a
    /// duplicate of [`crate::tile`]'s private `Neighbours::above_skip`/
    /// `left_skip`.
    above_skip: Vec<bool>,
    left_skip: Vec<bool>,
    /// libaom's `above_txfm_context`/`left_txfm_context` (`TXFM_CONTEXT`), at
    /// 4x4 mi granularity: the transform *width* (above) / *height* (left) in
    /// pixels last written over each unit by `set_txfm_ctxs`/
    /// `txfm_partition_update`, which is what an inter block's `txfm_split`
    /// symbol reads its context from (lane-txselect, spec 5.11.17).
    /// [`TXFM_CTX_INIT`] until a block writes one; a fresh full-height `left`
    /// array is exactly libaom's per-superblock-row reset, since each mi row
    /// is only ever visited inside one superblock row.
    above_txfm: Vec<u8>,
    left_txfm: Vec<u8>,
    /// Whether the block above/left of this column/row coded `skip_mode`
    /// (spec 5.11.23) -- `av1_get_skip_mode_context`'s own neighbour lookup,
    /// lane-av1comp round 14.
    above_skip_mode: Vec<bool>,
    left_skip_mode: Vec<bool>,
    /// Whether the block above/left was coded inter -- the `is_inter`
    /// context (spec `av1_get_intra_inter_context`), a duplicate of
    /// [`crate::tile`]'s private `Neighbours::above_inter`/`left_inter`.
    above_inter: Vec<bool>,
    left_inter: Vec<bool>,
    /// The actual reference frame (`1..=7`, spec's `MV_REFERENCE_FRAME`
    /// alphabet) the block above/left of this column/row coded, or `-1` for
    /// an intra neighbour/frame border -- the `single_ref_p1`..`p6` context
    /// functions (`mvstack::ref_ctx` family) need the real reference, not
    /// just "was it inter" (lane-av1refs: every inter block used to be
    /// `LAST_FRAME` by construction, so `above_inter`/`left_inter` alone was
    /// enough).
    above_ref: Vec<i8>,
    left_ref: Vec<i8>,
    /// The block above/left's *second* reference frame when it was coded
    /// compound (spec `RefFrames[1]`), or `None` for single-ref/intra --
    /// lane-av1comp: `mvstack::reference_mode_ctx`/`comp_reference_type_ctx`
    /// need a full [`crate::mvstack::NeighbourRef`], not just `above_ref`'s
    /// `i8`, once a compound `read_ref_frames` lands. Always `None` today
    /// (no decode path here ever produces a compound block yet).
    above_ref1: Vec<Option<i8>>,
    left_ref1: Vec<Option<i8>>,
    /// The precomputed `get_comp_group_idx_context`/`get_comp_index_context`
    /// neighbour contribution (spec 5.11.25, libaom `pred_common.h`):
    /// a compound neighbour's own `comp_group_idx`/`compound_idx` bit, or
    /// the "ref_frame[0] == ALTREF_FRAME" special case (`3`/`1`) for a
    /// single-ref one, or `0` for intra/unavailable -- lane-av1comp.
    above_comp_group_idx: Vec<u8>,
    left_comp_group_idx: Vec<u8>,
    above_compound_idx: Vec<u8>,
    left_compound_idx: Vec<u8>,
    /// `interp_filter[0]`/`[1]` (horizontal/vertical kernel index, 0..=2, or
    /// `3` = "no info": an intra neighbour or the frame's own border) of the
    /// block above/left of this column/row -- spec
    /// `av1_get_pred_context_switchable_interp`'s own neighbour lookup.
    above_filter: Vec<[u8; 2]>,
    left_filter: Vec<[u8; 2]>,
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
    /// The transform HEIGHT companion to [`Self::tx_grid`]'s width --
    /// lane-rect r2: TX_32X16/TX_16X32 strips deblock their horizontal
    /// edges by tx height (spec 7.14.2 `set_lpf_parameters` dir==VERT).
    tx_h_grid: Vec<u8>,
    /// Per-4x4-mi CHROMA transform width/height in pixels -- lane-tiny r4.
    /// A block's chroma transform is `av1_get_max_uv_txsize(bsize)`: the
    /// largest transform covering the subsampled BLOCK, capped at 32x32. It
    /// does NOT follow luma's `tx_depth` split, so deriving it as
    /// `tx_grid / 2` invented chroma transform edges the deblocker then
    /// filtered (32x16 frames, U plane, seeds 42/47). Written from the
    /// block's own mi span in [`Self::fill_lf_grid_rect`], the one place
    /// every caller routes through.
    uv_tx_grid: Vec<u8>,
    uv_tx_h_grid: Vec<u8>,
    /// Per-4x4-mi reference frame (`0` = intra, magnitude `1..=7` =
    /// `MV_REFERENCE_FRAME`, negative = that reference coded as `GLOBALMV` --
    /// `lf_level`'s `mode_lf_lut` row 0, lane-av1golden2)
    /// -- the loop filter's ref/mode delta lookup (spec 7.14.4
    /// `get_filter_level`, `loop_filter_ref_deltas[ref_frame]`; lane-av1refs:
    /// widened from a bare `is_inter` bool, which collapsed every non-`LAST`
    /// reference onto `LAST_FRAME`'s own delta).
    ref_grid: Vec<i8>,
    /// Per-4x4-mi snapshot of [`CURRENT_DELTA_LF`] at the moment this cell's
    /// block was decoded (spec `DeltaLF[i]`, `i` in `0..FRAME_LF_COUNT`,
    /// Y-vertical/Y-horizontal/U/V) -- lane-realworld r5: `lf_level`'s
    /// per-block additive term, mirroring [`Self::ref_grid`]'s own snapshot
    /// pattern. `!delta_lf_multi` mode broadcasts index 0 into all 4 slots
    /// at write time ([`Self::fill_lf_grid_rect`]) so a reader never needs
    /// to know which mode produced the value.
    delta_lf_grid: Vec<[i8; 4]>,
    /// The block above/left's own palette-Y size (0 = not a palette block)
    /// and its base colours -- `av1_get_palette_mode_ctx`/
    /// `av1_get_palette_cache` (pred_common.c)'s neighbour lookup, lane-palette
    /// r2. UV has no equivalent yet (stage 2, unreconstructed).
    above_palette_size: Vec<usize>,
    left_palette_size: Vec<usize>,
    above_palette_colors: Vec<[u16; 8]>,
    left_palette_colors: Vec<[u16; 8]>,
    /// As the four fields above, for the neighbour's own chroma (U-channel
    /// only -- `av1_get_palette_cache`'s plane-1 lookup, [`Self::palette_uv_cache`])
    /// palette size/colours (lane-palette2 r1).
    above_palette_uv_size: Vec<usize>,
    left_palette_uv_size: Vec<usize>,
    above_palette_uv_colors: Vec<[u16; 8]>,
    left_palette_uv_colors: Vec<[u16; 8]>,
    /// This tile's own top-left corner, in 4x4 mode-info units (spec
    /// `MiRowStart`/`MiColStart`) -- lane-tiles: every "is there really a
    /// neighbour" check compares against this instead of literal `0`, since a
    /// block at the tile's own top/left edge has no usable neighbour even
    /// though a decoded block sits there in the picture (spec `decode_tile`'s
    /// per-tile `clear_above_context`/`left_available`/`up_available`).
    /// `0` for a single-tile frame, matching every existing caller exactly.
    tile_row0_mi: usize,
    tile_col0_mi: usize,
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
            above_uv_mode: vec![DC_PRED; cols],
            left_uv_mode: vec![DC_PRED; rows],
            above_side: vec![SB; cols],
            left_side: vec![SB; rows],
            above_side_mi: vec![SB; cols * (SUB / MI)],
            left_side_mi: vec![SB; rows * (SUB / MI)],
            sub8_mode_col: vec![(usize::MAX, 0); cols * (SUB / MI)],
            sub8_mode_row: vec![(usize::MAX, 0); rows * (SUB / MI)],
            uv_mode_col: vec![(usize::MAX, 0); cols * (SUB / MI)],
            uv_mode_row: vec![(usize::MAX, 0); rows * (SUB / MI)],
            above_txfm: vec![TXFM_CTX_INIT; cols * (SUB / MI)],
            left_txfm: vec![TXFM_CTX_INIT; rows * (SUB / MI)],
            // lane-inter8 r2: mi(4px)-granular, not [`SUB`]-granular -- an
            // 8x8 inter leaf's left neighbour is the OTHER leaf row of the
            // 16x16 block beside it, which a SUB-grid slot cannot name
            // (class context-read-from-one-cell).
            above_skip: vec![false; cols * (SUB / MI)],
            left_skip: vec![false; rows * (SUB / MI)],
            above_skip_mode: vec![false; cols * (SUB / MI)],
            left_skip_mode: vec![false; rows * (SUB / MI)],
            above_inter: vec![false; cols * (SUB / MI)],
            left_inter: vec![false; rows * (SUB / MI)],
            above_ref: vec![-1; cols * (SUB / MI)],
            left_ref: vec![-1; rows * (SUB / MI)],
            above_ref1: vec![None; cols * (SUB / MI)],
            left_ref1: vec![None; rows * (SUB / MI)],
            above_comp_group_idx: vec![0; cols * (SUB / MI)],
            left_comp_group_idx: vec![0; rows * (SUB / MI)],
            above_compound_idx: vec![0; cols * (SUB / MI)],
            left_compound_idx: vec![0; rows * (SUB / MI)],
            above_filter: vec![[3u8; 2]; cols * (SUB / MI)],
            left_filter: vec![[3u8; 2]; rows * (SUB / MI)],
            mi_cols,
            mi_rows,
            skip_grid: vec![false; cols * (SUB / MI) * rows * (SUB / MI)],
            skip_grid_cols_mi: cols * (SUB / MI),
            tx_grid: vec![0u8; cols * (SUB / MI) * rows * (SUB / MI)],
            tx_h_grid: vec![0u8; cols * (SUB / MI) * rows * (SUB / MI)],
            uv_tx_grid: vec![0u8; cols * (SUB / MI) * rows * (SUB / MI)],
            uv_tx_h_grid: vec![0u8; cols * (SUB / MI) * rows * (SUB / MI)],
            ref_grid: vec![0i8; cols * (SUB / MI) * rows * (SUB / MI)],
            delta_lf_grid: vec![[0i8; 4]; cols * (SUB / MI) * rows * (SUB / MI)],
            above_palette_size: vec![0; cols],
            left_palette_size: vec![0; rows],
            above_palette_colors: vec![[0u16; 8]; cols],
            left_palette_colors: vec![[0u16; 8]; rows],
            above_palette_uv_size: vec![0; cols],
            left_palette_uv_size: vec![0; rows],
            above_palette_uv_colors: vec![[0u16; 8]; cols],
            left_palette_uv_colors: vec![[0u16; 8]; rows],
            tile_row0_mi: 0,
            tile_col0_mi: 0,
        }
    }

    /// spec `decode_tile`'s per-tile reset: `clear_above_context` over this
    /// tile's own column span (`col1_mi` exclusive), plus recording
    /// `(row0_mi, col0_mi)` as the tile's own top-left mi cell for every
    /// availability check below. Left context resets every superblock row
    /// regardless of tile ([`Self::start_row`], already called once per SB
    /// row by every caller) so it needs no tile-specific reset here.
    fn start_tile(&mut self, row0_mi: usize, col0_mi: usize, col1_mi: usize) {
        // lane-seg: `AvailU`/`AvailL` for the segment-id neighbour prediction
        // are tile-relative, same as every other neighbour lookup here.
        SEG_TILE_ORIGIN.with(|c| c.set((row0_mi, col0_mi)));
        self.tile_row0_mi = row0_mi;
        self.tile_col0_mi = col0_mi;
        let end = col1_mi.min(self.above.len());
        for i in col0_mi.min(end)..end {
            self.above[i] = Default::default();
            self.above_side_mi[i] = SB;
        }
        let (sc0, sc1) = (
            col0_mi / (SUB / MI),
            (col1_mi / (SUB / MI)).min(self.above_mode.len()),
        );
        for i in sc0.min(sc1)..sc1 {
            self.above_mode[i] = DC_PRED;
            self.above_side[i] = SB;
        }
        // lane-inter8 r2: the inter bands are mi-granular, so they reset over
        // the tile's own mi column span directly.
        let mend = col1_mi.min(self.above_skip.len());
        for i in col0_mi.min(mend)..mend {
            self.above_skip[i] = false;
            self.above_skip_mode[i] = false;
            self.above_inter[i] = false;
            self.above_ref[i] = -1;
            self.above_ref1[i] = None;
            self.above_comp_group_idx[i] = 0;
            self.above_compound_idx[i] = 0;
            self.above_filter[i] = [3u8; 2];
        }
        // lane-sub8 r6: the mi-granular column maps are per tile too -- a
        // stale `(row, mode)` from the tile to the LEFT names an mi row this
        // tile's first block would read as its own above neighbour.
        let mcol_end = col1_mi.min(self.sub8_mode_col.len());
        for i in col0_mi.min(mcol_end)..mcol_end {
            self.sub8_mode_col[i] = (usize::MAX, 0);
            self.uv_mode_col[i] = (usize::MAX, 0);
        }
    }

    /// Writes a just-decoded block's own palette-Y size/colours (`size == 0`
    /// for a non-palette block, clearing any stale neighbour state) into every
    /// [`SUB`]-grid cell it covers on both the above and left edges --
    /// mirrors [`Self::record_rect`]'s loop shape, square-only (this lane's
    /// call sites are all square blocks).
    fn record_palette_y(&mut self, at: (usize, usize), side: usize, size: usize, colors: [u16; 8]) {
        let (r, c) = at;
        for cell in 0..side / SUB {
            self.above_palette_size[c + cell] = size;
            self.above_palette_colors[c + cell] = colors;
        }
        for cell in 0..side / SUB {
            self.left_palette_size[r + cell] = size;
            self.left_palette_colors[r + cell] = colors;
        }
    }

    /// `av1_get_palette_mode_ctx` (pred_common.h:197): count of the above/left
    /// neighbours that are themselves a palette-Y block, plus
    /// `av1_get_palette_cache`'s own merged, ascending, deduplicated colour
    /// cache (pred_common.c:73) -- the above neighbour is excluded at a
    /// superblock's own top row (`r % 4 == 0` in [`SUB`]-grid units, 4 cells
    /// per 64px SB, `MIN_SB_SIZE_LOG2`), the left neighbour never is.
    fn palette_ctx_and_cache(&self, at: (usize, usize)) -> (usize, Vec<u16>) {
        let (r, c) = at;
        let above_ok = r % 4 != 0;
        let (above_n, above_colors) = if above_ok && self.above_palette_size[c] > 0 {
            (self.above_palette_size[c], self.above_palette_colors[c])
        } else {
            (0, [0u16; 8])
        };
        let (left_n, left_colors) = if self.left_palette_size[r] > 0 {
            (self.left_palette_size[r], self.left_palette_colors[r])
        } else {
            (0, [0u16; 8])
        };
        let ctx = usize::from(above_n > 0) + usize::from(left_n > 0);
        let mut cache = Vec::with_capacity(16);
        let push_dedup = |cache: &mut Vec<u16>, v: u16| {
            if cache.last() != Some(&v) {
                cache.push(v);
            }
        };
        let (mut ai, mut li) = (0usize, 0usize);
        while ai < above_n && li < left_n {
            let (va, vl) = (above_colors[ai], left_colors[li]);
            if vl < va {
                push_dedup(&mut cache, vl);
                li += 1;
            } else {
                push_dedup(&mut cache, va);
                ai += 1;
                if vl == va {
                    li += 1;
                }
            }
        }
        while ai < above_n {
            push_dedup(&mut cache, above_colors[ai]);
            ai += 1;
        }
        while li < left_n {
            push_dedup(&mut cache, left_colors[li]);
            li += 1;
        }
        (ctx, cache)
    }

    /// As [`Self::record_palette_y`], for a just-decoded block's own chroma
    /// (U-channel) palette size/colours -- `size == 0` clears stale state the
    /// same way (lane-palette2 r1).
    fn record_palette_uv(&mut self, at: (usize, usize), side: usize, size: usize, colors: [u16; 8]) {
        let (r, c) = at;
        for cell in 0..side / SUB {
            self.above_palette_uv_size[c + cell] = size;
            self.above_palette_uv_colors[c + cell] = colors;
        }
        for cell in 0..side / SUB {
            self.left_palette_uv_size[r + cell] = size;
            self.left_palette_uv_colors[r + cell] = colors;
        }
    }

    /// [`Self::palette_ctx_and_cache`]'s cache half, for the U channel
    /// (`av1_get_palette_cache(xd, 1, cache)`) -- `palette_uv_mode_ctx` needs
    /// no neighbour lookup at all (it is this block's own just-decided Y
    /// palette use, `read_palette_mode_info`'s `pmi->palette_size[0] > 0`),
    /// so only the cache is wanted here.
    fn palette_uv_cache(&self, at: (usize, usize)) -> Vec<u16> {
        let (r, c) = at;
        let above_ok = r % 4 != 0;
        let (above_n, above_colors) = if above_ok && self.above_palette_uv_size[c] > 0 {
            (self.above_palette_uv_size[c], self.above_palette_uv_colors[c])
        } else {
            (0, [0u16; 8])
        };
        let (left_n, left_colors) = if self.left_palette_uv_size[r] > 0 {
            (self.left_palette_uv_size[r], self.left_palette_uv_colors[r])
        } else {
            (0, [0u16; 8])
        };
        let mut cache = Vec::with_capacity(16);
        let push_dedup = |cache: &mut Vec<u16>, v: u16| {
            if cache.last() != Some(&v) {
                cache.push(v);
            }
        };
        let (mut ai, mut li) = (0usize, 0usize);
        while ai < above_n && li < left_n {
            let (va, vl) = (above_colors[ai], left_colors[li]);
            if vl < va {
                push_dedup(&mut cache, vl);
                li += 1;
            } else {
                push_dedup(&mut cache, va);
                ai += 1;
                if vl == va {
                    li += 1;
                }
            }
        }
        while ai < above_n {
            push_dedup(&mut cache, above_colors[ai]);
            ai += 1;
        }
        while li < left_n {
            push_dedup(&mut cache, left_colors[li]);
            li += 1;
        }
        cache
    }

    /// Marks every 4x4 mi cell a just-decoded coded block covers with its own
    /// transform width (in pixels) and `is_inter` flag -- the loop filter's
    /// per-edge lookup (`apply_deblock`), filled at the same call sites and
    /// units as [`Self::fill_skip_grid`].
    fn fill_lf_grid(&mut self, at_mi: (usize, usize), side_mi: usize, tx_px: u8, ref_frame: i8) {
        self.fill_lf_grid_rect(at_mi, side_mi, side_mi, tx_px, tx_px, ref_frame);
    }

    /// [`Self::fill_lf_grid`] with independent row/col extents -- lane-partitions
    /// r1: a true rectangular strip's mi span isn't `side_mi` square (`w_mi`
    /// != `h_mi`); `tx_px` still names a single scalar (this decoder's
    /// existing corner-cut, `Neighbours::tx_grid`'s own doc comment -- a
    /// rectangular strip's transform is `TX_32X16`/`TX_16X32`, not tracked
    /// per-axis, matching lane-warp r5's `HORZ_B` top-strip precedent).
    fn fill_lf_grid_rect(
        &mut self,
        at_mi: (usize, usize),
        w_mi: usize,
        h_mi: usize,
        tx_px: u8,
        tx_h_px: u8,
        ref_frame: i8,
    ) {
        let (mi_r, mi_c) = at_mi;
        // `av1_get_max_uv_txsize` under 4:2:0: the block's chroma extent is
        // half its luma one (min 4 px, the smallest transform), capped at
        // TX_32X32 -- independent of this block's luma `tx_depth`.
        let uv_tx_w = ((w_mi * MI / 2).max(4).min(32)) as u8;
        let uv_tx_h = ((h_mi * MI / 2).max(4).min(32)) as u8;
        let cur = CURRENT_DELTA_LF.with(|c| c.get());
        let snapshot: [i8; 4] = if DELTA_LF_MULTI.with(|c| c.get()) {
            std::array::from_fn(|i| cur[i].clamp(-63, 63) as i8)
        } else {
            [cur[0].clamp(-63, 63) as i8; 4]
        };
        for rr in 0..h_mi {
            for cc in 0..w_mi {
                let idx = (mi_r + rr) * self.skip_grid_cols_mi + (mi_c + cc);
                if let Some(cell) = self.tx_grid.get_mut(idx) {
                    *cell = tx_px;
                }
                if let Some(cell) = self.tx_h_grid.get_mut(idx) {
                    *cell = tx_h_px;
                }
                if let Some(cell) = self.uv_tx_grid.get_mut(idx) {
                    *cell = uv_tx_w;
                }
                if let Some(cell) = self.uv_tx_h_grid.get_mut(idx) {
                    *cell = uv_tx_h;
                }
                if let Some(cell) = self.ref_grid.get_mut(idx) {
                    *cell = ref_frame;
                }
                if let Some(cell) = self.delta_lf_grid.get_mut(idx) {
                    *cell = snapshot;
                }
            }
        }
    }

    /// Marks every 4x4 mi cell a just-decoded coded block covers with its
    /// `skip` flag -- `at_mi`/`side_mi` are the block's own position and
    /// width/height in 4x4 mode-info units (mirroring [`Self::record_mi`]'s
    /// own units, not [`Self::record`]'s [`SUB`]-grid ones).
    fn fill_skip_grid(&mut self, at_mi: (usize, usize), side_mi: usize, skip: bool) {
        self.fill_skip_grid_rect(at_mi, side_mi, side_mi, skip);
    }

    /// [`Self::fill_skip_grid`] with independent row/col extents (lane-partitions r1).
    fn fill_skip_grid_rect(&mut self, at_mi: (usize, usize), w_mi: usize, h_mi: usize, skip: bool) {
        let (mi_r, mi_c) = at_mi;
        for rr in 0..h_mi {
            for cc in 0..w_mi {
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

    /// The `skip_txfm` context of a block whose top-left mi cell is
    /// `(mi_r, mi_c)`: libaom `av1_get_skip_txfm_context`
    /// (`pred_common.h:175-181`) is `above_mi->skip_txfm + left_mi->skip_txfm`,
    /// with an absent neighbour contributing 0. The mi grid is per tile, so
    /// row/column 0 is a tile edge and has no neighbour on that axis.
    fn skip_txfm_ctx(&self, mi_r: usize, mi_c: usize) -> usize {
        usize::from(mi_r > self.tile_row0_mi && self.skip_at(mi_r - 1, mi_c))
            + usize::from(mi_c > self.tile_col0_mi && self.skip_at(mi_r, mi_c - 1))
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
        self.left_uv_mode.iter_mut().for_each(|m| *m = DC_PRED);
        self.left_side.iter_mut().for_each(|s| *s = SB);
        self.left_side_mi.iter_mut().for_each(|s| *s = SB);
        self.left_skip.iter_mut().for_each(|s| *s = false);
        self.left_skip_mode.iter_mut().for_each(|s| *s = false);
        self.left_inter.iter_mut().for_each(|i| *i = false);
        self.left_ref.iter_mut().for_each(|r| *r = -1);
        self.left_ref1.iter_mut().for_each(|r| *r = None);
        self.left_comp_group_idx.iter_mut().for_each(|r| *r = 0);
        self.left_compound_idx.iter_mut().for_each(|r| *r = 0);
        self.left_filter.iter_mut().for_each(|f| *f = [3u8; 2]);
        // lane-sub8 r6: mi-granular row maps reset with the rest of the left
        // context (a tile/SB-row boundary has no left neighbour).
        self.sub8_mode_row.iter_mut().for_each(|s| *s = (usize::MAX, 0));
        self.uv_mode_row.iter_mut().for_each(|s| *s = (usize::MAX, 0));
    }

    /// Records a block's `skip`/`is_inter`/`interp_filter` state for the next
    /// block that reads it as a neighbour -- [`crate::tile`]'s
    /// `record_inter`. `filter` is `[3, 3]` for an intra block (spec
    /// `get_ref_filter_type`'s "not this ref frame" case reads the same as
    /// "no neighbour"). `ref_frame` is `-1` for an intra block, else the
    /// `1..=7` reference it coded (lane-av1refs's `single_ref_p*_ctx` needs
    /// the real value, not just `is_inter`).
    fn record_inter(
        &mut self,
        at: (usize, usize),
        side: usize,
        skip: bool,
        is_inter: bool,
        ref_frame: i8,
        filter: [u8; 2],
        skip_mode: bool,
    ) {
        self.record_inter_rect(at, side, side, skip, is_inter, ref_frame, filter, skip_mode);
    }

    /// [`Self::record_inter`] with independent above (`w_sub`, [`SUB`]-grid
    /// columns) / left (`h_sub`, [`SUB`]-grid rows) extents -- lane-partitions
    /// r1: a true rectangular strip's above-context span (its own width) and
    /// left-context span (its own height) differ, unlike every caller before
    /// this round (class neighbour-votes-all-its-fields).
    #[allow(clippy::too_many_arguments)]
    fn record_inter_rect(
        &mut self,
        at: (usize, usize),
        w_sub: usize,
        h_sub: usize,
        skip: bool,
        is_inter: bool,
        ref_frame: i8,
        filter: [u8; 2],
        skip_mode: bool,
    ) {
        let (r, c) = at;
        self.record_inter_rect_mi(
            (r * (SUB / MI), c * (SUB / MI)),
            w_sub / MI,
            h_sub / MI,
            skip,
            is_inter,
            ref_frame,
            filter,
            skip_mode,
        );
    }

    /// [`Self::record_inter_rect`] in mi (4px) units -- lane-inter8 r2: an
    /// 8x8 leaf covers 2x2 mi, half a [`SUB`] cell in each direction, so the
    /// side bands are written (and read) per mi. Every [`SUB`]-unit caller
    /// routes through the wrapper above.
    #[allow(clippy::too_many_arguments)]
    fn record_inter_rect_mi(
        &mut self,
        at_mi: (usize, usize),
        w_mi: usize,
        h_mi: usize,
        skip: bool,
        is_inter: bool,
        ref_frame: i8,
        filter: [u8; 2],
        skip_mode: bool,
    ) {
        let (r, c) = at_mi;
        let altref = is_inter && ref_frame == crate::mvstack::ALTREF_FRAME;
        for cell in 0..w_mi {
            self.above_skip[c + cell] = skip;
            self.above_skip_mode[c + cell] = skip_mode;
            self.above_inter[c + cell] = is_inter;
            self.above_ref[c + cell] = ref_frame;
            // lane-av1comp: no caller of `record_inter` ever decodes a
            // compound block yet (see [`Self::above_ref1`]'s doc); the
            // parameter this becomes lands with `read_ref_frames`.
            self.above_ref1[c + cell] = None;
            // libaom `get_comp_group_idx_context`/`get_comp_index_context`'s
            // single-ref special case: `ref_frame[0] == ALTREF_FRAME` reads
            // as `3`/`1` even though this block has no second reference.
            // `record_compound_ctx` overwrites this with the real bit for an
            // actual compound block's cells, called right after this one.
            self.above_comp_group_idx[c + cell] = if altref { 3 } else { 0 };
            self.above_compound_idx[c + cell] = u8::from(altref);
            self.above_filter[c + cell] = filter;
        }
        for cell in 0..h_mi {
            self.left_skip[r + cell] = skip;
            self.left_skip_mode[r + cell] = skip_mode;
            self.left_inter[r + cell] = is_inter;
            self.left_ref[r + cell] = ref_frame;
            self.left_ref1[r + cell] = None;
            self.left_comp_group_idx[r + cell] = if altref { 3 } else { 0 };
            self.left_compound_idx[r + cell] = u8::from(altref);
            self.left_filter[r + cell] = filter;
        }
    }

    /// Overwrites [`Self::record_inter`]'s ALTREF-special-case default with
    /// a real compound block's own `comp_group_idx`/`compound_idx` bits
    /// (libaom `has_second_ref(mbmi)` branch of `get_comp_group_idx_context`/
    /// `get_comp_index_context`) -- called right after `record_inter` for
    /// the same `at`/`side`, only on the `COMPOUND_REFERENCE` path. Also
    /// stamps `above_ref1`/`left_ref1` with the block's real second
    /// reference, which `record_inter` always clears to `None` (lane-av1comp:
    /// this is the one caller that actually knows it) -- so the next block's
    /// [`NeighbourRef`](crate::mvstack::NeighbourRef) sees a real compound
    /// neighbour instead of `record_inter`'s single-ref default.
    fn record_compound_ctx(
        &mut self,
        at: (usize, usize),
        side: usize,
        ref1: i8,
        comp_group_idx: u8,
        compound_idx: u8,
    ) {
        self.record_compound_ctx_rect(at, side, side, ref1, comp_group_idx, compound_idx);
    }

    /// [`Self::record_compound_ctx`] with independent above/left extents,
    /// mirroring [`Self::record_inter_rect`] (lane-partitions r1).
    #[allow(clippy::too_many_arguments)]
    fn record_compound_ctx_rect(
        &mut self,
        at: (usize, usize),
        w_sub: usize,
        h_sub: usize,
        ref1: i8,
        comp_group_idx: u8,
        compound_idx: u8,
    ) {
        let (r, c) = at;
        self.record_compound_ctx_rect_mi(
            (r * (SUB / MI), c * (SUB / MI)),
            w_sub / MI,
            h_sub / MI,
            ref1,
            comp_group_idx,
            compound_idx,
        );
    }

    /// [`Self::record_compound_ctx_rect`] in mi units (lane-inter8 r2, see
    /// [`Self::record_inter_rect_mi`]).
    fn record_compound_ctx_rect_mi(
        &mut self,
        at_mi: (usize, usize),
        w_mi: usize,
        h_mi: usize,
        ref1: i8,
        comp_group_idx: u8,
        compound_idx: u8,
    ) {
        let (r, c) = at_mi;
        for cell in 0..w_mi {
            self.above_ref1[c + cell] = Some(ref1);
            self.above_comp_group_idx[c + cell] = comp_group_idx;
            self.above_compound_idx[c + cell] = compound_idx;
        }
        for cell in 0..h_mi {
            self.left_ref1[r + cell] = Some(ref1);
            self.left_comp_group_idx[r + cell] = comp_group_idx;
            self.left_compound_idx[r + cell] = compound_idx;
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

    /// [`Self::around`] with independent above (`w`)/left (`h`) extents, in
    /// pixels -- lane-rectwire, mirrors [`Self::record_rect`]'s asymmetric
    /// extent pattern for a true rectangular strip's own coefficient context.
    fn around_rect(&self, (r, c): (usize, usize), w: usize, h: usize) -> [(bool, bool, i32); 3] {
        self.around_mi_rect((r * (SUB / MI), c * (SUB / MI)), w, h)
    }

    /// [`Self::around_rect`] taking the block's position directly in 4x4
    /// mode-info units, mirroring [`Self::around_mi`].
    fn around_mi_rect(&self, (mi_r, mi_c): (usize, usize), w: usize, h: usize) -> [(bool, bool, i32); 3] {
        let (w_mi, h_mi) = (w / MI, h / MI);
        std::array::from_fn(|plane| {
            let mut above_coded = false;
            let mut left_coded = false;
            let mut vote = 0;
            for cell in 0..w_mi {
                let above = &self.above[mi_c + cell][plane];
                above_coded |= above.level != 0;
                vote += dc_vote(above.dc);
            }
            for cell in 0..h_mi {
                let left = &self.left[mi_r + cell][plane];
                left_coded |= left.level != 0;
                vote += dc_vote(left.dc);
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
    fn record(
        &mut self,
        at: (usize, usize),
        side: usize,
        mode: usize,
        uv_mode: usize,
        grids: &[Vec<i32>; 3],
    ) {
        self.record_rect(at, side, side, mode, uv_mode, grids);
    }

    /// [`Self::record`] with independent above (`w`)/left (`h`) extents, in
    /// pixels -- lane-partitions r1: `above_side`/`left_side` (read back by
    /// [`Self::partition_ctx`]) are the neighbour's own width/height per the
    /// spec's per-edge partition-context rule, so a true rectangular block
    /// naturally wants two different values here rather than one.
    /// Writes this block's luma `mode` into the mi-granular
    /// [`Self::sub8_mode_col`]/[`Self::sub8_mode_row`] maps, keyed by the mi
    /// position a *later* neighbour would read it from: the block's LAST mi
    /// row for every column it spans (what the block below reads via
    /// `mi(row-1, col)`) and its LAST mi column for every row (what the
    /// block to the right reads via `mi(row, col-1)`). Only sub-8x8 leaves
    /// read these back, and only when the recorded position is exactly their
    /// own neighbour -- see [`Self::sub8_mode_col`]'s doc.
    fn record_mode_mi(
        &mut self,
        mi_r: usize,
        mi_c: usize,
        mi_w: usize,
        mi_h: usize,
        mode: usize,
    ) {
        let (last_r, last_c) = (mi_r + mi_h - 1, mi_c + mi_w - 1);
        for cell in 0..mi_w {
            if let Some(slot) = self.sub8_mode_col.get_mut(mi_c + cell) {
                *slot = (last_r, mode);
            }
        }
        for cell in 0..mi_h {
            if let Some(slot) = self.sub8_mode_row.get_mut(mi_r + cell) {
                *slot = (last_c, mode);
            }
        }
    }

    /// The mi-exact above/left neighbour luma mode for a block whose top-left
    /// mi is `(mi_r, mi_c)`, or `None` when no block has recorded that exact
    /// mi position (then the coarse [`SUB`]-grid slot is right by
    /// construction). See [`Self::record_mode_mi`].
    fn mode_above_mi(&self, mi_r: usize, mi_c: usize) -> Option<usize> {
        match self.sub8_mode_col.get(mi_c) {
            Some(&(row, m)) if mi_r > self.tile_row0_mi && row == mi_r - 1 => Some(m),
            _ => None,
        }
    }

    fn mode_left_mi(&self, mi_r: usize, mi_c: usize) -> Option<usize> {
        match self.sub8_mode_row.get(mi_r) {
            Some(&(col, m)) if mi_c > self.tile_col0_mi && col == mi_c - 1 => Some(m),
            _ => None,
        }
    }

    /// [`Self::record_mode_mi`] for the block's `uv_mode`, into
    /// [`Self::uv_mode_col`]/[`Self::uv_mode_row`].
    fn record_uv_mode_mi(
        &mut self,
        mi_r: usize,
        mi_c: usize,
        mi_w: usize,
        mi_h: usize,
        uv_mode: usize,
    ) {
        let (last_r, last_c) = (mi_r + mi_h - 1, mi_c + mi_w - 1);
        for cell in 0..mi_w {
            if let Some(slot) = self.uv_mode_col.get_mut(mi_c + cell) {
                *slot = (last_r, uv_mode);
            }
        }
        for cell in 0..mi_h {
            if let Some(slot) = self.uv_mode_row.get_mut(mi_r + cell) {
                *slot = (last_c, uv_mode);
            }
        }
    }

    /// Whether either mi-exact chroma neighbour of the block at `(mi_r, mi_c)`
    /// coded a smooth `uv_mode` -- libaom's `intra_edge_filter_type` for
    /// planes 1/2. Falls back to the coarse [`SUB`] slot on whichever axis no
    /// block has recorded the exact neighbouring mi.
    fn smooth_uv_neighbour(&self, mi_r: usize, mi_c: usize, r: usize, c: usize) -> bool {
        let above = match self.uv_mode_col.get(mi_c) {
            Some(&(row, m)) if mi_r > self.tile_row0_mi && row == mi_r - 1 => m,
            _ => self.above_uv_mode[c],
        };
        let left = match self.uv_mode_row.get(mi_r) {
            Some(&(col, m)) if mi_c > self.tile_col0_mi && col == mi_c - 1 => m,
            _ => self.left_uv_mode[r],
        };
        // lane-cfl r1 counter: how often the mi-exact chroma neighbour differs
        // from the coarse [`SUB`] slot -- i.e. how often a chroma edge would
        // have been filtered at the wrong strength by the coarse map alone.
        if above != self.above_uv_mode[c] || left != self.left_uv_mode[r] {
            UV_MODE_MI_OVERRIDE_HITS.with(|h| h.set(h.get() + 1));
        }
        is_smooth_mode(above) || is_smooth_mode(left)
    }

    /// The above/left luma modes libaom's `above_mi`/`left_mi` really see for a
    /// block at coarse [`SUB`]-grid `(r, c)`. lane-rectx r5: the coarse
    /// `above_mode`/`left_mode` slots are written in 16x16 cells, so a
    /// sub-16x16 neighbour (a 16x8 rect leaf, an 8x8 split cell) leaves them
    /// holding an OLDER, larger block's mode -- it only ever recorded into the
    /// mi-exact map. Every `kf_y_mode` reader goes through here, not just the
    /// strip path ([`decode_leaf_rect`] took this override in r4; a 32x16 strip
    /// whose left neighbour was a 16x8 leaf still read the stale row and picked
    /// a different CDF for the same mode value).
    fn modes_above_left(&self, r: usize, c: usize) -> (usize, usize) {
        let (mi_r, mi_c) = (r * (SUB / MI), c * (SUB / MI));
        let above = self.mode_above_mi(mi_r, mi_c).unwrap_or(self.above_mode[c]);
        let left = self.mode_left_mi(mi_r, mi_c).unwrap_or(self.left_mode[r]);
        if above != self.above_mode[c] || left != self.left_mode[r] {
            MODE_MI_OVERRIDE_HITS.with(|h| h.set(h.get() + 1));
        }
        (above, left)
    }

    fn record_rect(
        &mut self,
        at: (usize, usize),
        w: usize,
        h: usize,
        mode: usize,
        uv_mode: usize,
        grids: &[Vec<i32>; 3],
    ) {
        let (r, c) = at;
        for cell in 0..w / SUB {
            self.above_mode[c + cell] = mode;
            self.above_uv_mode[c + cell] = uv_mode;
            self.above_side[c + cell] = w;
        }
        for cell in 0..h / SUB {
            self.left_mode[r + cell] = mode;
            self.left_uv_mode[r + cell] = uv_mode;
            self.left_side[r + cell] = h;
        }
        self.record_mode_mi(r * (SUB / MI), c * (SUB / MI), w / MI, h / MI, mode);
        self.record_uv_mode_mi(r * (SUB / MI), c * (SUB / MI), w / MI, h / MI, uv_mode);
        self.record_mi_rect((r * (SUB / MI), c * (SUB / MI)), w, h, grids);
    }

    /// The coefficient-context half of [`Self::record`], taking the block's
    /// position directly in 4x4 mode-info units rather than [`SUB`]-grid
    /// `(r, c)`: an 8x8 leaf under a straddling 16x16 (lane-av1-rect) sits at
    /// a mi offset [`Self::record`]'s SUB-unit `at` cannot name.
    fn record_mi(&mut self, at_mi: (usize, usize), side: usize, grids: &[Vec<i32>; 3]) {
        self.record_mi_rect(at_mi, side, side, grids);
    }

    /// [`Self::record_mi`] with independent above (`w`)/left (`h`) extents,
    /// in pixels (lane-partitions r1, mirroring [`Self::record_rect`]).
    fn record_mi_rect(
        &mut self,
        at_mi: (usize, usize),
        w: usize,
        h: usize,
        grids: &[Vec<i32>; 3],
    ) {
        let (mi_r, mi_c) = at_mi;
        let states: [Neighbour; 3] = std::array::from_fn(|plane| neighbour_state(&grids[plane]));
        let (w_mi, h_mi) = (w / MI, h / MI);
        for cell in 0..h_mi {
            self.left_side_mi[mi_r + cell] = h;
        }
        for cell in 0..w_mi {
            self.above_side_mi[mi_c + cell] = w;
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
        for cell in 0..h_mi {
            self.left[mi_r + cell] = std::array::from_fn(|plane| {
                if cell < h_mi.min(bound_h[plane].saturating_sub(mi_r)) {
                    states[plane]
                } else {
                    Default::default()
                }
            });
        }
        for cell in 0..w_mi {
            self.above[mi_c + cell] = std::array::from_fn(|plane| {
                if cell < w_mi.min(bound_w[plane].saturating_sub(mi_c)) {
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
        uv_mode: usize,
        chroma_grids: [&[i32]; 2],
    ) {
        self.record_split_luma_rect(at, side, side, mode, uv_mode, chroma_grids);
    }

    /// [`Self::record_split_luma`] with independent above (`w`)/left (`h`)
    /// extents in pixels (lane-rectsplit r1), the same relationship
    /// [`Self::record_rect`] has to [`Self::record`]: a HORZ/VERT strip whose
    /// luma transform split into several square units needs exactly this
    /// bookkeeping, at its own true rectangular extents.
    fn record_split_luma_rect(
        &mut self,
        at: (usize, usize),
        w: usize,
        h: usize,
        mode: usize,
        uv_mode: usize,
        chroma_grids: [&[i32]; 2],
    ) {
        let (r, c) = at;
        for cell in 0..w / SUB {
            self.above_mode[c + cell] = mode;
            self.above_uv_mode[c + cell] = uv_mode;
            self.above_side[c + cell] = w;
        }
        for cell in 0..h / SUB {
            self.left_mode[r + cell] = mode;
            self.left_uv_mode[r + cell] = uv_mode;
            self.left_side[r + cell] = h;
        }
        let (mi_r, mi_c) = (r * (SUB / MI), c * (SUB / MI));
        let (w_mi, h_mi) = (w / MI, h / MI);
        self.record_mode_mi(mi_r, mi_c, w_mi, h_mi, mode);
        self.record_uv_mode_mi(mi_r, mi_c, w_mi, h_mi, uv_mode);
        for cell in 0..h_mi {
            self.left_side_mi[mi_r + cell] = h;
        }
        for cell in 0..w_mi {
            self.above_side_mi[mi_c + cell] = w;
        }
        let round_up_even = |n: usize| n.div_ceil(2) * 2;
        let (bound_h, bound_w) = (round_up_even(self.mi_rows), round_up_even(self.mi_cols));
        for (plane_idx, grid) in chroma_grids.into_iter().enumerate() {
            let plane = plane_idx + 1;
            let state = neighbour_state(grid);
            for cell in 0..h_mi {
                if cell < h_mi.min(bound_h.saturating_sub(mi_r)) {
                    self.left[mi_r + cell][plane] = state;
                }
            }
            for cell in 0..w_mi {
                if cell < w_mi.min(bound_w.saturating_sub(mi_c)) {
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

/// [`filter_intra_size_class`] for a true `bw`x`bh` rect strip
/// (lane-intradisp r1): `av1_filter_intra_allowed_bsize` permits any bsize
/// with both dims <= 32, which includes `BLOCK_32X16`/`BLOCK_16X32` -- their
/// own distinct `cdf::FILTER_INTRA` rows, classes 4/5.
fn filter_intra_size_class_rect(bw: usize, bh: usize) -> Option<usize> {
    match (bw, bh) {
        (32, 16) => Some(4),
        (16, 32) => Some(5),
        // lane-rectx r3: `av1_filter_intra_allowed_bsize` is "both sides <=
        // 32", so a 16x8/8x16 strip reads the flag too -- returning `None`
        // here dropped that symbol on every DC_PRED strip of that size and
        // desynced the tile (caught against an aomdec EC_TRACE_COEFF ladder:
        // identical mode/uv_mode values, range 34808 vs the oracle's 40668).
        (16, 8) => Some(6),
        (8, 16) => Some(7),
        _ if bw == bh => filter_intra_size_class(bw),
        _ => None,
    }
}

/// `av1_allow_palette`'s size bound (blockd.h: `block_size_wide/high <=
/// MAX_PALETTE_BLOCK_WIDTH/HEIGHT` (64) and `sb_type >= BLOCK_8X8`) plus
/// `av1_get_palette_bsize_ctx` (pred_common.h: `num_pels_log2_lookup[bsize]
/// - num_pels_log2_lookup[BLOCK_8X8]`, i.e. `log2(bw*bh) - 6`) -- `None` past
/// the bound (never gates a palette read), `Some(bsize_ctx)` otherwise.
fn palette_bsize_ctx(side: usize) -> Option<usize> {
    palette_bsize_ctx_wh(side, side)
}

/// [`palette_bsize_ctx`], generalised to a true `bw`x`bh` rect strip
/// (lane-palette2 r1) -- the same `log2(bw*bh) - 6` formula, this decoder's
/// square call sites are just `bw == bh`.
fn palette_bsize_ctx_wh(bw: usize, bh: usize) -> Option<usize> {
    match (bw, bh) {
        (8, 8) => Some(0),
        (16, 16) => Some(2),
        (32, 16) | (16, 32) => Some(3),
        (32, 32) => Some(4),
        (64, 64) => Some(6),
        _ => None,
    }
}

/// A just-decoded palette-Y block's own size, base colours (only the first
/// `size` entries are real), and per-pixel colour-index map (`side*side`,
/// row-major, each entry `< size`).
struct PaletteY {
    size: usize,
    colors: [u16; 8],
    map: Vec<u8>,
}

/// As [`PaletteY`], for a just-decoded chroma palette block: one shared
/// per-pixel colour-index map (spec `av1_visit_palette`'s plane-1 pass is
/// shared between U and V, both co-located after 4:2:0 subsampling) plus
/// each plane's own base colours.
struct PaletteUv {
    size: usize,
    u_colors: [u16; 8],
    v_colors: [u16; 8],
    map: Vec<u8>,
}

/// `aom_ceil_log2` (aom_dsp/bitwriter_buffer.h-adjacent helper, used both by
/// `read_palette_colors_y`'s shrinking `bits` and `av1_read_uniform`'s
/// `get_unsigned_bits`, which is the same formula): smallest `n` with
/// `2^n >= x`, `0` for `x <= 1`.
fn ceil_log2(x: u32) -> u32 {
    if x < 2 { 0 } else { 32 - (x - 1).leading_zeros() }
}

/// `av1_read_uniform` (decoder.h:425): a uniform `0..n` value coded as
/// `l-1` raw bits plus a conditional extra bit, `l = get_unsigned_bits(n)`.
fn read_uniform(dec: &mut SymbolDecoder, n: usize) -> usize {
    let l = ceil_log2(n as u32);
    let m = (1usize << l) - n;
    let v = dec.literal(l - 1) as usize;
    if v < m {
        v
    } else {
        (v << 1) - m + dec.literal(1) as usize
    }
}

/// `merge_colors` (decodemv.c:462): interleaves the ascending `cached`
/// colours (already-known, from the above/left neighbours) with the
/// ascending `transmitted` ones (delta-coded off the wire) back into one
/// ascending list of `cached.len() + transmitted.len()` colours -- a tied
/// value picks the cached copy first, matching the C `<=`.
fn merge_colors(transmitted: &[u16], cached: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(transmitted.len() + cached.len());
    let (mut ci, mut ti) = (0, 0);
    while ci < cached.len() && ti < transmitted.len() {
        if cached[ci] <= transmitted[ti] {
            out.push(cached[ci]);
            ci += 1;
        } else {
            out.push(transmitted[ti]);
            ti += 1;
        }
    }
    out.extend_from_slice(&cached[ci..]);
    out.extend_from_slice(&transmitted[ti..]);
    out
}

/// `read_palette_colors_y` (decodemv.c:478), bit_depth fixed at 8 (this
/// decoder is 8-bit only): accepts up to `n` colours straight from `cache`
/// (one flag bit each, in cache order), then delta-codes the remainder
/// (first value raw, each following delta `>= 1` off a shrinking bit width),
/// and merges the two ascending lists back together.
fn read_palette_colors_y(dec: &mut SymbolDecoder, n: usize, cache: &[u16]) -> [u16; 8] {
    let mut cached = Vec::with_capacity(n);
    for &c in cache {
        if cached.len() >= n {
            break;
        }
        if dec.literal(1) == 1 {
            cached.push(c);
        }
    }
    let mut colors = [0u16; 8];
    let n_cached = cached.len();
    if n_cached < n {
        let mut transmitted = Vec::with_capacity(n - n_cached);
        let first = dec.literal(8) as u16;
        transmitted.push(first);
        if n_cached + 1 < n {
            let min_bits = 8u32 - 3;
            let mut bits = min_bits + dec.literal(2);
            let mut range = 255i32 - i32::from(first) - 1;
            for _ in (n_cached + 1)..n {
                let delta = dec.literal(bits) as i32 + 1;
                let prev = *transmitted.last().unwrap();
                let val = (i32::from(prev) + delta).clamp(0, 255) as u16;
                range -= i32::from(val) - i32::from(prev);
                bits = bits.min(ceil_log2(range.max(0) as u32));
                transmitted.push(val);
            }
        }
        let merged = merge_colors(&transmitted, &cached);
        colors[..n].copy_from_slice(&merged);
    } else {
        colors[..n].copy_from_slice(&cached[..n]);
    }
    colors
}

/// `read_palette_colors_uv` (decodemv.c:509), bit_depth fixed at 8: U reads
/// exactly [`read_palette_colors_y`]'s cache/delta scheme (against the U-only
/// cache, `range` starting at `255 - prev` with no `-1`, and each raw `delta`
/// unbiased by `+1` -- both differences from Y's own reader, matching the C
/// side by side). V never uses a cache: a leading bit picks either `n` raw
/// literals, or a first raw value plus `n-1` signed deltas that wrap mod 256
/// rather than clamp.
fn read_palette_colors_uv(dec: &mut SymbolDecoder, n: usize, cache: &[u16]) -> ([u16; 8], [u16; 8]) {
    let mut cached = Vec::with_capacity(n);
    for &c in cache {
        if cached.len() >= n {
            break;
        }
        if dec.literal(1) == 1 {
            cached.push(c);
        }
    }
    let mut u_colors = [0u16; 8];
    let n_cached = cached.len();
    if n_cached < n {
        let mut transmitted = Vec::with_capacity(n - n_cached);
        let first = dec.literal(8) as u16;
        transmitted.push(first);
        if n_cached + 1 < n {
            let min_bits = 8u32 - 3;
            let mut bits = min_bits + dec.literal(2);
            let mut range = 255i32 - i32::from(first);
            for _ in (n_cached + 1)..n {
                let delta = dec.literal(bits) as i32;
                let prev = *transmitted.last().unwrap();
                let val = (i32::from(prev) + delta).clamp(0, 255) as u16;
                range -= i32::from(val) - i32::from(prev);
                bits = bits.min(ceil_log2(range.max(0) as u32));
                transmitted.push(val);
            }
        }
        let merged = merge_colors(&transmitted, &cached);
        u_colors[..n].copy_from_slice(&merged);
    } else {
        u_colors[..n].copy_from_slice(&cached[..n]);
    }
    let mut v_colors = [0u16; 8];
    if dec.literal(1) == 1 {
        let min_bits_v = 8u32 - 4;
        let bits = min_bits_v + dec.literal(2);
        v_colors[0] = dec.literal(8) as u16;
        for i in 1..n {
            let mut delta = dec.literal(bits) as i32;
            if delta != 0 && dec.literal(1) == 1 {
                delta = -delta;
            }
            let mut val = i32::from(v_colors[i - 1]) + delta;
            if val < 0 {
                val += 256;
            }
            if val >= 256 {
                val -= 256;
            }
            v_colors[i] = val as u16;
        }
    } else {
        for i in 0..n {
            v_colors[i] = dec.literal(8) as u16;
        }
    }
    (u_colors, v_colors)
}

/// `av1_palette_color_index_context_lookup` (entropymode.c:889): maps a
/// `NUM_PALETTE_NEIGHBORS`-weighted score hash (`0..=8`) to the real
/// `0..=4` context, `-1` for hashes no real neighbour configuration produces.
const PALETTE_COLOR_INDEX_CONTEXT_LOOKUP: [i32; 9] = [-1, -1, 0, -1, -1, 4, 3, 2, 1];

/// `av1_get_palette_color_index_context` (entropymode.c:893): the colour-index
/// map's own per-pixel context and colour-order permutation, from the
/// left/up-left/up already-decoded neighbours (weights `[2, 1, 2]`, top-3
/// selection sort). `map`'s stride is `side` (this decoder never crops a
/// palette block's map to a narrower plane edge, [`decode_color_index_map`]'s
/// own doc). Returns `(ctx, color_order)`; `color_order[symbol]` is the
/// actual colour index to store.
fn palette_color_index_context(
    map: &[u8],
    side: usize,
    row: usize,
    col: usize,
    n: usize,
) -> (usize, [u8; 8]) {
    let left = if col >= 1 { Some(map[row * side + col - 1]) } else { None };
    let up_left = if col >= 1 && row >= 1 {
        Some(map[(row - 1) * side + col - 1])
    } else {
        None
    };
    let up = if row >= 1 { Some(map[(row - 1) * side + col]) } else { None };
    let mut scores = [0i32; 8];
    for (neighbour, weight) in [(left, 2), (up_left, 1), (up, 2)] {
        if let Some(v) = neighbour {
            scores[v as usize] += weight;
        }
    }
    let mut color_order = [0u8; 8];
    for i in 0..8 {
        color_order[i] = i as u8;
    }
    for i in 0..3 {
        let mut max = scores[i];
        let mut max_idx = i;
        for j in (i + 1)..n {
            if scores[j] > max {
                max = scores[j];
                max_idx = j;
            }
        }
        if max_idx != i {
            let max_score = scores[max_idx];
            let max_color = color_order[max_idx];
            for k in (i + 1..=max_idx).rev() {
                scores[k] = scores[k - 1];
                color_order[k] = color_order[k - 1];
            }
            scores[i] = max_score;
            color_order[i] = max_color;
        }
    }
    let hash = scores[0] + scores[1] * 2 + scores[2] * 2;
    let ctx = PALETTE_COLOR_INDEX_CONTEXT_LOOKUP[hash as usize];
    (ctx as usize, color_order)
}

/// `decode_color_map_tokens` (detokenize.c:25): the palette-Y colour-index
/// map's wavefront decode over a `side x side` grid -- this decoder's
/// palette blocks are always square and never straddle the frame's true
/// (possibly odd) edge without already allocating the full `side*side`
/// region (every other skip/residual path here does the same), so no
/// rows/cols-vs-plane_width/height cropping special case is needed, unlike
/// libaom's own non-block-aligned split.
fn decode_color_index_map(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    n: usize,
    side: usize,
    uv: bool,
) -> Vec<u8> {
    let trace = std::env::var_os("EC_AV1_TRACE").is_some();
    let mut map = vec![0u8; side * side];
    if trace {
        let (rng, _) = dec.debug_state();
        eprintln!("EC_PAL row=0 col=0 ctx=-1 n={n} rng={rng}");
    }
    map[0] = read_uniform(dec, n) as u8;
    if trace {
        let (rng, _) = dec.debug_state();
        eprintln!("EC_PAL_VAL row=0 col=0 color_idx={} rng={rng}", map[0]);
    }
    for i in 1..(2 * side - 1) {
        let j_hi = i.min(side - 1);
        let j_lo = i.saturating_sub(side - 1);
        for j in (j_lo..=j_hi).rev() {
            let row = i - j;
            let col = j;
            let (ctx, color_order) = palette_color_index_context(&map, side, row, col, n);
            if trace {
                let (rng, _) = dec.debug_state();
                eprintln!("EC_PAL row={row} col={col} ctx={ctx} n={n} rng={rng}");
            }
            let symbol = if uv {
                dec.symbol(&mut cdfs.palette_uv_color_index[n - 2][ctx][..=n])
            } else {
                dec.symbol(&mut cdfs.palette_y_color_index[n - 2][ctx][..=n])
            };
            map[row * side + col] = color_order[symbol];
            if trace {
                let (rng, _) = dec.debug_state();
                eprintln!("EC_PAL_VAL row={row} col={col} color_idx={symbol} rng={rng}");
            }
        }
    }
    map
}

/// [`read_intra_mode`] for a true `bw`x`bh` rect strip (lane-intradisp r1):
/// identical except the `use_filter_intra` size class comes from
/// [`filter_intra_size_class_rect`] instead of the square-only
/// [`filter_intra_size_class`] -- every other symbol this reads (`skip`,
/// `y_mode`, `angle_delta`, `uv_mode`, `cfl_alphas`) is indexed by mode or
/// neighbour state, never by block size.
#[allow(clippy::too_many_arguments)]
fn read_intra_mode_rect(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    above_mode: usize,
    left_mode: usize,
    cfl: bool,
    bw: usize,
    bh: usize,
    enable_filter_intra: bool,
    skip_ctx: usize,
    allow_screen_content_tools: bool,
    mi_r: usize,
    mi_c: usize,
) -> Result<(
    bool,
    usize,
    i32,
    usize,
    i32,
    Option<(i32, i32)>,
    Option<usize>,
)> {
    // lane-screen consumes palette/intrabc syntax in the SQUARE reader only
    // (`palette_bsize_ctx` is keyed on a single side). A rect strip in a
    // screen-content frame would therefore skip symbols the encoder wrote and
    // desync the tile, so refuse it by name rather than decode it wrong.
    if allow_screen_content_tools {
        return Err(unsupported(
            "a HORZ/VERT intra strip in a screen-content frame (palette syntax \
             is consumed for square blocks only)",
        ));
    }
    let ec_istep = std::env::var_os("EC_TRACE_MODE_STEP").is_some();
    macro_rules! istep {
        ($name:literal, $val:expr) => {
            if ec_istep {
                let (rng, _) = dec.debug_state();
                eprintln!(
                    "EC_ISTEP mi_row={mi_r} mi_col={mi_c} name={} val={} rng={rng}",
                    $name, $val
                );
            }
        };
    }
    // Same `intra_segment_id` placement as the square reader above.
    let (seg_w_mi, seg_h_mi) = (bw / 4, bh / 4);
    if seg_id_pre_skip() {
        intra_segment_id(dec, cdfs, mi_r, mi_c, seg_w_mi, seg_h_mi, false);
    }
    let skip = dec.symbol(&mut cdfs.skip[skip_ctx]) != 0;
    istep!("skip", skip as i32);
    if !seg_id_pre_skip() {
        intra_segment_id(dec, cdfs, mi_r, mi_c, seg_w_mi, seg_h_mi, skip);
    }
    maybe_read_cdef_idx(dec, mi_r, mi_c, skip);
    istep!("cdef", 0);
    // A HORZ/VERT rect strip is never the whole superblock (`bw`/`bh` never
    // both 64 here -- see this fn's own doc), so `is_whole_sb` is always
    // `false`.
    maybe_read_delta_q(dec, cdfs, mi_r, mi_c, false, skip);
    maybe_read_delta_lf(dec, cdfs, mi_r, mi_c, false, skip);
    istep!("dq", 0);
    let above_ctx = INTRA_MODE_CTX[above_mode];
    let left_ctx = INTRA_MODE_CTX[left_mode];
    let mode = dec.symbol(&mut cdfs.kf_y_mode[above_ctx][left_ctx]);
    istep!("mode", mode as i32);
    let angle_delta_y = if (V_PRED..=D67_PRED).contains(&mode) {
        read_angle_delta(dec, &mut cdfs.angle_delta[mode - V_PRED])
    } else {
        0
    };
    istep!("angle_y", angle_delta_y);
    let uv_mode = if cfl {
        dec.symbol(&mut cdfs.uv_mode_cfl[mode])
    } else {
        dec.symbol(&mut cdfs.uv_mode_no_cfl[mode])
    };
    istep!("uv_mode", uv_mode as i32);
    let alpha = if cfl && uv_mode == UV_CFL_PRED {
        Some(read_cfl_alphas(dec, cdfs))
    } else {
        None
    };
    if (9..=12).contains(&uv_mode) {
        SMOOTH_UV_HITS.with(|c| c.set(c.get() + 1));
    }
    let angle_delta_uv = if (V_PRED..=D67_PRED).contains(&uv_mode) {
        DIRECTIONAL_UV_HITS.with(|c| c.set(c.get() + 1));
        read_angle_delta(dec, &mut cdfs.angle_delta[uv_mode - V_PRED])
    } else {
        0
    };
    istep!("angle_uv", angle_delta_uv);
    if angle_delta_uv != 0 {
        UV_ANGLE_DELTA_HITS.with(|c| c.set(c.get() + 1));
    }
    let mut filter_intra = None;
    if std::env::var_os("EC_AV1_TRACE").is_some() {
        let (rng, _) = dec.debug_state();
        eprintln!(
            "TRACE_RECT_PREFI mode={mode} uv_mode={uv_mode} enable_filter_intra={} \
             class={:?} rng={rng}",
            enable_filter_intra as i32,
            filter_intra_size_class_rect(bw, bh)
        );
    }
    if mode == DC_PRED
        && enable_filter_intra
        && let Some(class) = filter_intra_size_class_rect(bw, bh)
    {
        let use_filter_intra = dec.symbol(&mut cdfs.filter_intra[class]) != 0;
        if std::env::var_os("EC_AV1_TRACE").is_some() {
            eprintln!("TRACE_RECT_USEFI value={}", use_filter_intra as i32);
        }
        if use_filter_intra {
            FILTER_INTRA_HITS.with(|c| c.set(c.get() + 1));
            let fi_mode = dec.symbol(&mut cdfs.filter_intra_mode);
            filter_intra = Some(fi_mode);
        }
    }
    Ok((
        skip,
        mode,
        angle_delta_y,
        uv_mode,
        angle_delta_uv,
        alpha,
        filter_intra,
    ))
}

/// [`tx_size_context`] for a true `bw`x`bh` rect strip (lane-intradisp r1):
/// `get_tx_size_context` (libaom `pred_common.h`) compares the neighbour
/// above against this block's own *width* and the neighbour to the left
/// against its own *height* -- for a square block those are the same value,
/// which is why the square-only version can get away with one `own_side`.
fn tx_size_context_rect(
    n: &Neighbours,
    (mi_r, mi_c): (usize, usize),
    own_w: usize,
    own_h: usize,
) -> usize {
    let above = mi_r > n.tile_row0_mi && tx_px_at(n, false, mi_r - 1, mi_c) as usize >= own_w;
    let left = mi_c > n.tile_col0_mi && tx_h_px_at(n, false, mi_r, mi_c - 1) as usize >= own_h;
    usize::from(above) + usize::from(left)
}

/// [`cfl_ac_q3`] for a true `bw`x`bh` rect strip (lane-intradisp r1).
fn cfl_ac_q3_rect(y: &PlaneBuf, px: usize, py: usize, bw: usize, bh: usize) -> Vec<i32> {
    let (cw, ch) = (bw / 2, bh / 2);
    let mut ac = vec![0i32; cw * ch];
    let mut sum = 0i32;
    for cy in 0..ch {
        for cx in 0..cw {
            let (lx, ly) = (px + cx * 2, py + cy * 2);
            let q3 = (i32::from(y.data[ly * y.width + lx])
                + i32::from(y.data[ly * y.width + lx + 1])
                + i32::from(y.data[(ly + 1) * y.width + lx])
                + i32::from(y.data[(ly + 1) * y.width + lx + 1]))
                << 1;
            ac[cy * cw + cx] = q3;
            sum += q3;
        }
    }
    let num_pel = (cw * ch) as i32;
    let avg = (sum + num_pel / 2) >> num_pel.trailing_zeros();
    ac.iter_mut().for_each(|v| *v -= avg);
    ac
}

/// Everything a rect strip already read out of the stream before its luma
/// transform units are decoded -- [`decode_rect_split`]'s own mode bundle
/// (nine values that would otherwise be nine more parameters).
struct RectStripModes {
    skip: bool,
    mode: usize,
    angle_delta_y: i32,
    uv_predict_mode: usize,
    angle_delta_uv: i32,
    alpha: Option<(i32, i32)>,
    filter_intra: Option<usize>,
    smooth_neighbor: bool,
    smooth_neighbor_uv: bool,
}

/// Decodes the planes of one `bw`x`bh` HORZ/VERT intra strip whose luma
/// transform is SPLIT (`tx_depth != 0`) -- lane-rectsplit r1, the case every
/// rect path here refused by name until now ("per-unit rect prediction is not
/// ported").
///
/// `depth_to_tx_size` (libaom `blockd.h`) walks `sub_tx_size_map` `depth`
/// times from the block's own `max_txsize_rect_lookup` entry, and for a 2:1
/// strip the very first step already lands on a SQUARE transform
/// (`sub_tx_size_map[TX_32X16] == TX_16X16`, `[TX_64X32] == TX_32X32`, and
/// every further step halves a square) -- so each unit is square and reads
/// through the ordinary square [`read_plane`]. What is genuinely rectangular
/// here is the *tiling* (`bw/tx` by `bh/tx` units, not `n` by `n` as
/// [`decode_block`]'s square split loop assumes) and the chroma plane, whose
/// own transform is `av1_get_max_uv_txsize` of the whole strip and is never
/// split by luma's depth.
///
/// Each unit predicts off whatever the earlier units of this same block
/// already reconstructed (spec 5.11.36 `predict_and_reconstruct_intra_block`
/// inside `av1_foreach_transformed_block`, raster order), which is exactly
/// what predicting the whole strip in one shot got wrong.
#[allow(clippy::too_many_arguments)]
fn decode_rect_split(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    at: (usize, usize),
    bw: usize,
    bh: usize,
    tx: usize,
    m: &RectStripModes,
    y: &mut PlaneBuf,
    u: &mut PlaneBuf,
    v: &mut PlaneBuf,
    base_q_idx: u8,
    reduced_tx_set: bool,
) -> Result<()> {
    let (r, c) = at;
    let (px, py) = (c * SUB, r * SUB);
    let (mi_r, mi_c) = (r * (SUB / MI), c * (SUB / MI));
    let (cpx, cpy) = (px / 2, py / 2);
    let (chroma_w, chroma_h) = (bw / 2, bh / 2);
    // The chroma transform is picked before any symbol is read, so an
    // unsupported chroma shape refuses before this strip desyncs the tile.
    let chroma: Option<(TxbSet, &[u16])> = match (chroma_w, chroma_h) {
        (16, 8) => Some((TxbSet::ChromaRect16x8, &SCAN_16X8[..])),
        (8, 16) => Some((TxbSet::ChromaRect16x8, &SCAN_8X16[..])),
        (32, 16) => Some((TxbSet::ChromaRect32x16, &SCAN_32X16[..])),
        (16, 32) => Some((TxbSet::ChromaRect32x16, &SCAN_16X32[..])),
        _ => None,
    };
    // lane-rectsplit r2 (verifier finding): with both callers wired
    // (`decode_block_rect`'s 32x16/16x32 and `decode_block_rect64`'s
    // 64x32/32x64) every shape that reaches here has a table above, so this
    // arm is UNREACHABLE today. It stays as the shape guard for the next
    // caller -- a new strip size would otherwise hit the `expect` below and
    // panic instead of refusing by name -- and so stays in
    // `refusal_inventory::REFUSALS`.
    if !m.skip && chroma.is_none() {
        return Err(unsupported(
            "a coded HORZ/VERT strip whose chroma transform has no rect coefficient tables here",
        ));
    }
    RECT_SPLIT_TX_HITS.with(|c| c.set(c.get() + 1));
    if bw.max(bh) == 64 && bw / tx > 1 && bh / tx > 1 {
        RECT_SPLIT_SB_INTERIOR_TU_HITS.with(|c| c.set(c.get() + 1));
    }
    // LUMA: `bw/tx` x `bh/tx` square transform units, raster order.
    let luma_set = txbset_for(tx, reduced_tx_set);
    let luma_scan = default_scan(tx);
    let zero_tu = vec![0i32; tx * tx];
    // The strip's OWN edge availability, the fallback for a transform unit
    // whose top-right/bottom-left pixels fall outside the strip.
    let block_reach = Reach::of_rect(bw, bh, px, py, y.width, y.height);
    for tu_row in 0..bh / tx {
        for tu_col in 0..bw / tx {
            let tu_mi = (mi_r + tu_row * (tx / MI), mi_c + tu_col * (tx / MI));
            let (tu_px, tu_py) = (px + tu_col * tx, py + tu_row * tx);
            let (col_off, row_off) = (tu_col * tx, tu_row * tx);
            // `has_top_right`/`has_bottom_left` (libaom `reconintra.c`) for a
            // transform unit INSIDE a block, which is a different rule from
            // the standalone-block table [`Reach::of`] applies -- and the
            // reason a depth-2 superblock strip came out one sample off
            // before this: `Reach::of(tx, ..)` answered as if the unit were
            // its own block at that superblock position.
            //  * top-right: if the unit's right neighbour is still inside the
            //    strip the pixels are already reconstructed (`col_off +
            //    tx_wide < plane_bw`); otherwise only the strip's own top row
            //    of units can reach past it, through the block-level answer.
            //  * bottom-left: a unit that is not in the strip's left column
            //    has none (`col_off > 0` returns 0); inside the strip
            //    (`row_off + tx_high < plane_bh`) they are the left
            //    neighbour's, already reconstructed; the last row falls back
            //    to the block-level answer.
            let tu_reach = Reach {
                above_right: if col_off + tx < bw {
                    true
                } else {
                    row_off == 0 && block_reach.above_right
                },
                below_left: if col_off > 0 {
                    false
                } else if row_off + tx < bh {
                    true
                } else {
                    block_reach.below_left
                },
            };
            if m.skip {
                y.reconstruct(
                    tu_px,
                    tu_py,
                    tx,
                    m.mode,
                    m.angle_delta_y,
                    tu_reach,
                    &zero_tu,
                    None,
                    m.filter_intra,
                    m.smooth_neighbor,
                );
                neighbours.record_mi_luma(tu_mi, tx, &zero_tu);
            } else {
                let tu_around = neighbours.around_mi(tu_mi, tx)[0];
                // This unit is smaller than the block it sits in, so
                // `txb_skip_ctx` is the neighbour-magnitude table, not the
                // lone-TU 0 (spec `get_txb_ctx_general`).
                let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, tx / MI);
                let tu_grid = read_plane(
                    dec,
                    cdfs,
                    luma_set,
                    &luma_scan,
                    0,
                    tu_around,
                    m.mode,
                    m.mode,
                    m.angle_delta_y,
                    tu_reach,
                    y,
                    tu_px,
                    tu_py,
                    tx,
                    tx,
                    base_q_idx,
                    None,
                    m.filter_intra,
                    Some(tu_skip_ctx),
                    m.smooth_neighbor,
                )?;
                neighbours.record_mi_luma(tu_mi, tx, &tu_grid);
            }
        }
    }
    // CHROMA: one un-split rect transform for the whole strip.
    let ac = m.alpha.map(|_| cfl_ac_q3_rect(y, px, py, bw, bh));
    let reach = block_reach;
    let (u_grid, v_grid) = if m.skip {
        let zero = vec![0i32; chroma_w * chroma_h];
        u.reconstruct_rect(
            cpx, cpy, chroma_w, chroma_h, m.uv_predict_mode, m.angle_delta_uv, reach, &zero,
            m.alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)), None, m.smooth_neighbor_uv,
        );
        v.reconstruct_rect(
            cpx, cpy, chroma_w, chroma_h, m.uv_predict_mode, m.angle_delta_uv, reach, &zero,
            m.alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)), None, m.smooth_neighbor_uv,
        );
        (zero.clone(), zero)
    } else {
        let (chroma_set, chroma_scan) = chroma.expect("refused above when None");
        // `av1_get_ext_tx_set_type`: a chroma transform whose square-up size
        // is TX_32X32 or bigger resolves to `EXT_TX_SET_DCTONLY` for intra,
        // so only the 16x8/8x16 pair takes the mode-indexed default.
        let default_tx = if chroma_w.max(chroma_h) >= 32 {
            TxType::DctDct
        } else {
            default_intra_tx_type(m.uv_predict_mode as u8)
        };
        let around = neighbours.around_rect(at, bw, bh);
        let mut grids: Vec<Vec<i32>> = Vec::with_capacity(2);
        for plane_idx in 1..=2 {
            let skip_ctx =
                usize::from(around[plane_idx].0) + usize::from(around[plane_idx].1);
            let mut coding = cdfs.txb(chroma_set, m.uv_predict_mode);
            let (levels, tx_type) = read_coeffs_rect(
                dec,
                &mut coding,
                chroma_scan,
                chroma_w,
                chroma_h,
                skip_ctx,
                dc_sign_ctx(around[plane_idx].2),
                default_tx,
            )?;
            // The same delta_q proof the unsplit rect64 path records
            // (lane-rectsplit r4: lifting the split-transform refusal moved
            // superblock strips onto this path, so only counting there would
            // leave the delta_q gate's counter silent for them).
            if bw.max(bh) == 64 && CURRENT_Q_IDX.with(|c| c.get()) != i32::from(base_q_idx) {
                RECT64_QIDX_DRIFT_HITS.with(|c| c.set(c.get() + 1));
            }
            let residual = dequant_and_inverse_typed_wh(
                &levels,
                chroma_w,
                chroma_h,
                crate::decode::bit_depth(),
                CURRENT_Q_IDX.with(|c| c.get()),
                plane_q_delta(plane_idx).0,
                plane_q_delta(plane_idx).1,
                tx_type,
            );
            let cfl = m
                .alpha
                .zip(ac.as_deref())
                .map(|((au, av), ac)| (if plane_idx == 1 { au } else { av }, ac));
            let plane = if plane_idx == 1 { &mut *u } else { &mut *v };
            plane.reconstruct_rect(
                cpx,
                cpy,
                chroma_w,
                chroma_h,
                m.uv_predict_mode,
                m.angle_delta_uv,
                reach,
                &residual,
                cfl,
                None,
                m.smooth_neighbor_uv,
            );
            grids.push(levels);
        }
        let mut it = grids.into_iter();
        (it.next().unwrap(), it.next().unwrap())
    };
    neighbours.record_split_luma_rect(at, bw, bh, m.mode, m.uv_predict_mode, [&u_grid, &v_grid]);
    neighbours.fill_skip_grid_rect((mi_r, mi_c), bw / MI, bh / MI, m.skip);
    neighbours.fill_lf_grid_rect((mi_r, mi_c), bw / MI, bh / MI, tx as u8, tx as u8, 0);
    if std::env::var_os("EC_AV1_TRACE").is_some() {
        let (rng, _) = dec.debug_state();
        eprintln!(
            "TRACE_RECT_SPLIT mi_row={mi_r} mi_col={mi_c} bw={bw} bh={bh} tx={tx} rng={rng}"
        );
    }
    Ok(())
}

/// Decodes one true `bw`x`bh` `PARTITION_HORZ`/`PARTITION_VERT` intra strip
/// (lane-intradisp r1, spec `decode_block` restricted to a 32x32 quadrant's
/// two rect children). Only the `skip` case is supported: `skip`/`y_mode`/
/// `uv_mode`/`filter_intra`/`tx_depth` are read exactly per spec either way
/// (so a skip strip elsewhere in the tile stays in sync), but this decoder
/// has no rectangular-transform coefficient reader, and a chroma plane here
/// is genuinely `TX_16X8`/`TX_8X16` (`av1_get_max_uv_txsize`, spec 5.11.16)
/// with no depth-based square-tiling escape the way luma's own resolved
/// `tx_depth` has (`sub_tx_size_map[TX_32X16]` == `TX_16X16`, square) --
/// so a coded (non-skip) strip is refused by name instead of guess-decoded.
#[allow(clippy::too_many_arguments)]
fn decode_block_rect(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    at: (usize, usize),
    bw: usize,
    bh: usize,
    y: &mut PlaneBuf,
    u: &mut PlaneBuf,
    v: &mut PlaneBuf,
    enable_filter_intra: bool,
    allow_screen_content_tools: bool,
    base_q_idx: u8,
    tx_select: bool,
    reduced_tx_set: bool,
) -> Result<()> {
    let (r, c) = at;
    let (px, py) = (c * SUB, r * SUB);
    let (nb_above_mode, nb_left_mode) = neighbours.modes_above_left(r, c);
    let (skip, mode, angle_delta_y, uv_mode, angle_delta_uv, alpha, filter_intra) =
        read_intra_mode_rect(
            dec,
            cdfs,
            nb_above_mode,
            nb_left_mode,
            true,
            bw,
            bh,
            enable_filter_intra,
            neighbours.skip_txfm_ctx(r * (SUB / MI), c * (SUB / MI)),
            allow_screen_content_tools,
            r * (SUB / MI),
            c * (SUB / MI),
        )?;
    let smooth_neighbor = is_smooth_mode(nb_above_mode) || is_smooth_mode(nb_left_mode);
    if smooth_neighbor {
        SMOOTH_LUMA_HITS.with(|c| c.set(c.get() + 1));
    }
    // lane-chroma r3: chroma's own edge-filter-strength neighbour check
    // (spec `get_intra_edge_filter_type`) reads the CHROMA neighbour's
    // `uv_mode`, not the luma one.
    let smooth_neighbor_uv =
        neighbours.smooth_uv_neighbour(r * (SUB / MI), c * (SUB / MI), r, c);
    // lane-rectsplit r1: `predict_filter_intra` now takes the block's own
    // `bw`x`bh` (`av1_filter_intra_predictor_c` walks its 4x2 patches over a
    // rectangle just as happily), so a `use_filter_intra` strip at this level
    // predicts instead of refusing. The below-16x16 leaf arm
    // (`decode_leaf_rect`) keeps the refusal: it has no gate of its own.
    if filter_intra.is_some() {
        FILTER_INTRA_RECT_HITS.with(|c| c.set(c.get() + 1));
    }
    let uv_predict_mode = if uv_mode == UV_CFL_PRED {
        DC_PRED
    } else {
        uv_mode
    };
    let (mi_r, mi_c) = (r * (SUB / MI), c * (SUB / MI));
    // The tx-depth symbol EXISTS only under TX_MODE_SELECT. Both square paths
    // gate their `read_tx_size` on `tx_select` for exactly this reason; this
    // one did not, so with `--enable-tx-size-search=0` -- which several gate
    // recipes use -- the decoder consumed a symbol the encoder never wrote and
    // desynced the tile from that point on. That is why both strips of the
    // pinned HORZ quadrant were wrong from their very first pixel, and why our
    // range after the read equalled the oracle's range for the whole block.
    let depth = if tx_select {
        let ctx = tx_size_context_rect(neighbours, (mi_r, mi_c), bw, bh);
        dec.symbol(&mut cdfs.tx_size_cat2[ctx])
    } else {
        0
    };
    if depth != 0 {
        TX_DEPTH_HITS.with(|c| c.set(c.get() + 1));
        // lane-rectsplit r1: a split transform is predicted and reconstructed
        // per transform unit, each unit taking its edges from the previous
        // unit's reconstruction inside this same strip (spec 5.11.36) --
        // [`decode_rect_split`]. `depth_to_tx_size` of a 2:1 strip is square
        // from the first step on, so `bw.min(bh) >> (depth - 1)` names it.
        let tx = bw.min(bh) >> (depth - 1);
        if std::env::var_os("EC_SBPART_DUMP64").is_some() {
            eprintln!(
                "DUMP64SPLIT mi_r={mi_r} mi_c={mi_c} px={px} py={py} bw={bw} bh={bh} \
                 depth={depth} tx={tx} mode={mode} uv={uv_predict_mode} skip={skip} \
                 angle_y={angle_delta_y}"
            );
        }
        let modes = RectStripModes {
            skip,
            mode,
            angle_delta_y,
            uv_predict_mode,
            angle_delta_uv,
            alpha,
            filter_intra,
            smooth_neighbor,
            smooth_neighbor_uv,
        };
        decode_rect_split(
            dec, cdfs, neighbours, at, bw, bh, tx, &modes, y, u, v, base_q_idx, reduced_tx_set,
        )?;
        RECT_PARTITION_HITS.with(|c| c.set(c.get() + 1));
        return Ok(());
    }
    let (tx_w, tx_h) = (bw, bh);
    let (cpx, cpy) = (px / 2, py / 2);
    let (chroma_w, chroma_h) = (bw / 2, bh / 2);
    let reach = Reach::of_rect(bw, bh, px, py, y.width, y.height);
    if std::env::var_os("EC_AV1_TRACE").is_some() {
        let (rng, _) = dec.debug_state();
        eprintln!(
            "TRACE_RECT_IMODE mi_row={mi_r} mi_col={mi_c} mode={mode} uv_mode={uv_mode} \
             skip={} tx={depth} rng={rng}",
            skip as i32
        );
    }
    if skip {
        y.reconstruct_rect(
            px,
            py,
            bw,
            bh,
            mode,
            angle_delta_y,
            reach,
            &vec![0i32; bw * bh],
            None,
            filter_intra,
            smooth_neighbor,
        );
        let ac = alpha.map(|_| cfl_ac_q3_rect(y, px, py, bw, bh));
        u.reconstruct_rect(
            cpx,
            cpy,
            chroma_w,
            chroma_h,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            &vec![0i32; chroma_w * chroma_h],
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
            None,
            smooth_neighbor_uv,
        );
        v.reconstruct_rect(
            cpx,
            cpy,
            chroma_w,
            chroma_h,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            &vec![0i32; chroma_w * chroma_h],
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
            None,
            smooth_neighbor_uv,
        );
        neighbours.record_rect(
            at,
            bw,
            bh,
            mode,
            uv_predict_mode,
            &[
                vec![0i32; bw * bh],
                vec![0i32; chroma_w * chroma_h],
                vec![0i32; chroma_w * chroma_h],
            ],
        );
    } else if (bw, bh) != (32, 16) && (bw, bh) != (16, 32) {
        // lane-rect16 r1: a real (non-skip) coefficient read at this size
        // (8x16/16x32 VERT_B's own left strip) is not ported -- it needs its
        // own `TxbSet`/`eob_pt` tables (a true 128-position luma group has no
        // `EOB_PT_128_LUMA` in this decoder yet, only the *chroma* one
        // `lane-rectwire` built for `ChromaRect16x8`) and, unlike the 32x32
        // sqr-up case `LumaRect32x16` safely skips, `get_ext_tx_set_type`
        // does NOT return `EXT_TX_SET_DCTONLY` at a 16x16 sqr-up under this
        // encoder's reduced_tx_set -- a real `tx_type` symbol may be coded
        // that no table here reads. Refused by name rather than guess-decoded
        // or silently desynced.
        return Err(unsupported(
            "a coded (non-skip) HORZ_B/VERT_B rect strip below 16x16 (this decoder ports only \
             the skip case at this size)",
        ));
    } else {
        // lane-rectwire r2: real coefficients. `get_txsize_entropy_ctx`
        // reduces both size pairs to their square-up CDF sets
        // (`TxbSet::LumaRect32x16`/`ChromaRect16x8`, see those variants' own
        // doc comments); the scan order and 2D context tables are the real
        // rect ones (`SCAN_32X16`/etc, `base_ctx_rect`/`br_ctx_rect`).
        let (luma_scan, chroma_scan): (&[u16], &[u16]) = if bw == 32 {
            (&SCAN_32X16, &SCAN_16X8)
        } else {
            (&SCAN_16X32, &SCAN_8X16)
        };
        // lane-rectwire r3: real coefficients desync a pixel-exact gate
        // (rectwire-flake-1.obu, seed 55) -- both 32x16 strips of frame 0's
        // mi(8,8) HORZ quadrant come out ~100% wrong from their very first
        // pixel (not a subtle context-table off-by-one), while the sibling
        // non-rect quadrants of the same frame match. Range-ladder bisection
        // against a real aomdec (EC_TRACE_COEFF/EC_TRACE_MODE) was started
        // but the intra key-frame path has no traced symbol between the
        // partition read and the first coefficient block (mode/skip/tx_depth
        // reads are all untraced in libaom's decodeframe.c intra path, unlike
        // the inter EC_TRACE_MODE this round added), so a same-length range
        // comparison wasn't reachable within budget -- see
        // lanes/rectwire-r3.report.md for what was ruled out (base_ctx_rect's
        // 5-neighbour offsets, u/v_skip_ctx's above+left formula, and
        // dc_sign_ctx's around-index all cross-checked byte-for-byte against
        // libaom and match). lane-rectwire r4: range-ladder against the
        // now-instrumented oracle (EC_TRACE_MODE's intra EC_IMODE/EC_IMODE_VAL)
        // narrowed the divergence past skip/y_mode/uv_mode (values AND range
        // both confirmed in sync -- rng=58692 on both sides right after
        // uv_mode, with `enable_filter_intra=0` on this frame so neither side
        // reads a filter_intra symbol) to the `tx_size_cat2` depth read
        // immediately after: ours moves rng 58692 -> 43570, which cannot be
        // reconciled with oracle's own EC_IMODE_VAL (rng=58692, taken *after*
        // its own tx_size read) unless oracle's tx symbol is a near-certainty
        // CDF that barely narrows the range -- i.e. this decoder's
        // `tx_size_context_rect` ctx or `cdfs.tx_size_cat2` CDF selection for
        // a 32x16/16x32 strip is the first diverging symbol, not the
        // coefficient-context math r3 already cleared. Not confirmed byte-
        // exact (would need a decodemv.c EC_TRACE_MODE patch specifically on
        // `read_tx_size`'s own rng, not attempted this round -- budget), so
        // r5 CONFIRMED the suspect and it was simpler than the CDF row: the
        // tx-depth symbol exists only under TX_MODE_SELECT, and this path read
        // it unconditionally where both square paths gate on `tx_select`. With
        // --enable-tx-size-search=0 the encoder writes no such symbol, so the
        // decoder consumed one that was never there.
        {
        let around = neighbours.around_rect(at, bw, bh);
        let mut luma_coding = cdfs.txb(TxbSet::LumaRect32x16, mode);
        let (luma_levels, luma_tx_type) = read_coeffs_rect(
            dec,
            &mut luma_coding,
            luma_scan,
            bw,
            bh,
            0,
            dc_sign_ctx(around[0].2),
            TxType::DctDct,
        )?;
        if std::env::var_os("EC_AV1_TRACE").is_some() {
            let (rng, _) = dec.debug_state();
            eprintln!("TRACE_RECT_COEFF plane=0 mi_row={mi_r} mi_col={mi_c} rng={rng}");
        }
        let luma_residual = dequant_and_inverse_typed_wh(
            &luma_levels,
            bw,
            bh,
            crate::decode::bit_depth(),
            block_q_idx(),
            plane_q_delta(0).0,
            plane_q_delta(0).1,
            luma_tx_type,
        );
        y.reconstruct_rect(
            px,
            py,
            bw,
            bh,
            mode,
            angle_delta_y,
            reach,
            &luma_residual,
            None,
            filter_intra,
            smooth_neighbor,
        );
        let ac = alpha.map(|_| cfl_ac_q3_rect(y, px, py, bw, bh));
        let u_default_tx = default_intra_tx_type(uv_predict_mode as u8);
        let u_skip_ctx = usize::from(around[1].0) + usize::from(around[1].1);
        let mut u_coding = cdfs.txb(TxbSet::ChromaRect16x8, uv_predict_mode);
        let (u_levels, u_tx_type) = read_coeffs_rect(
            dec,
            &mut u_coding,
            chroma_scan,
            chroma_w,
            chroma_h,
            u_skip_ctx,
            dc_sign_ctx(around[1].2),
            u_default_tx,
        )?;
        if std::env::var_os("EC_AV1_TRACE").is_some() {
            let (rng, _) = dec.debug_state();
            eprintln!("TRACE_RECT_COEFF plane=1 mi_row={mi_r} mi_col={mi_c} rng={rng}");
        }
        let u_residual = dequant_and_inverse_typed_wh(
            &u_levels,
            chroma_w,
            chroma_h,
            crate::decode::bit_depth(),
            block_q_idx(),
            plane_q_delta(1).0,
            plane_q_delta(1).1,
            u_tx_type,
        );
        u.reconstruct_rect(
            cpx,
            cpy,
            chroma_w,
            chroma_h,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            &u_residual,
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
            None,
            smooth_neighbor_uv,
        );
        let v_default_tx = default_intra_tx_type(uv_predict_mode as u8);
        let v_skip_ctx = usize::from(around[2].0) + usize::from(around[2].1);
        let mut v_coding = cdfs.txb(TxbSet::ChromaRect16x8, uv_predict_mode);
        let (v_levels, v_tx_type) = read_coeffs_rect(
            dec,
            &mut v_coding,
            chroma_scan,
            chroma_w,
            chroma_h,
            v_skip_ctx,
            dc_sign_ctx(around[2].2),
            v_default_tx,
        )?;
        if std::env::var_os("EC_AV1_TRACE").is_some() {
            let (rng, _) = dec.debug_state();
            eprintln!("TRACE_RECT_COEFF plane=2 mi_row={mi_r} mi_col={mi_c} rng={rng}");
        }
        let v_residual = dequant_and_inverse_typed_wh(
            &v_levels,
            chroma_w,
            chroma_h,
            crate::decode::bit_depth(),
            block_q_idx(),
            plane_q_delta(2).0,
            plane_q_delta(2).1,
            v_tx_type,
        );
        v.reconstruct_rect(
            cpx,
            cpy,
            chroma_w,
            chroma_h,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            &v_residual,
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
            None,
            smooth_neighbor_uv,
        );
        neighbours.record_rect(at, bw, bh, mode, uv_predict_mode, &[luma_levels, u_levels, v_levels]);
        RECT_COEFF_HITS.with(|c| c.set(c.get() + 1));
        }
    }
    neighbours.fill_skip_grid_rect((mi_r, mi_c), bw / MI, bh / MI, skip);
    neighbours.fill_lf_grid_rect((mi_r, mi_c), bw / MI, bh / MI, tx_w as u8, tx_h as u8, 0);
    RECT_PARTITION_HITS.with(|c| c.set(c.get() + 1));
    if std::env::var_os("EC_AV1_TRACE").is_some() {
        eprintln!("TRACE_RECT32_END mi_row={mi_r} mi_col={mi_c} bw={bw} bh={bh}");
    }
    Ok(())
}

/// Decodes one `bw`x`bh` rect strip of a plain 16x16-level `PARTITION_HORZ`/
/// `PARTITION_VERT` (lane-rect16 r2), sibling to [`decode_block_rect`] but
/// addressed by REAL mi coordinates rather than derived from an [`SUB`]-grid
/// `outer_at`, the same relationship [`decode_leaf8`] has to `decode_block`:
/// the second strip of a HORZ/VERT split sits offset by half the 16x16
/// parent, which `decode_block_rect`'s `at`-derived `px = c * SUB` can't
/// name. `prev_leaf` mirrors `decode_leaf8`'s own convention -- when the
/// previous strip is directly above (same column, `mi_h` rows up) or
/// directly left (same row, `mi_w` cols over), its mode overrides the coarse
/// per-16x16-cell `above_mode`/`left_mode` slot for THIS strip's own mode
/// context read, and bookkeeping goes through the `_mi`/`_rect` neighbour
/// writers (`record_mi_rect`, `fill_skip_grid_rect`, `fill_lf_grid_rect`) at
/// this strip's real position, never the coarse `record_rect` (which the
/// caller uses once, after both strips, exactly like the `VERT_B` arm next
/// to it). The non-skip case (lane-rectx) reads real `TX_16X8`/`TX_8X16`
/// luma coefficients (`TxbSet::LumaRect16x8`) and their `TX_8X4`/`TX_4X8`
/// chroma half (`TxbSet::ChromaRect8x4`), the two size classes one level
/// under [`decode_block_rect`]'s own 32x16/16x32 strip.
#[allow(clippy::too_many_arguments)]
fn decode_leaf_rect(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    outer_at: (usize, usize),
    leaf_mi: (usize, usize),
    bw: usize,
    bh: usize,
    prev_leaf: Option<((usize, usize), usize)>,
    y: &mut PlaneBuf,
    u: &mut PlaneBuf,
    v: &mut PlaneBuf,
    enable_filter_intra: bool,
    allow_screen_content_tools: bool,
    base_q_idx: u8,
    tx_select: bool,
    reduced_tx_set: bool,
) -> Result<usize> {
    let _ = base_q_idx;
    if std::env::var_os("EC_AV1_RECTX_TRACE").is_some() {
        eprintln!(
            "decode_leaf_rect: leaf_mi={leaf_mi:?} bw={bw} bh={bh} outer_at={outer_at:?} rng={}",
            dec.debug_state().0
        );
    }
    let (r, c) = outer_at;
    let mut above_mode = neighbours.above_mode[c];
    let mut left_mode = neighbours.left_mode[r];
    let (mi_w, mi_h) = (bw / MI, bh / MI);
    if let Some(((pr, pc), pmode)) = prev_leaf {
        if pc == leaf_mi.1 && leaf_mi.0 == pr + mi_h {
            above_mode = pmode;
        } else if pr == leaf_mi.0 && leaf_mi.1 == pc + mi_w {
            left_mode = pmode;
        }
    }
    // Same mi-exact override [`decode_leaf8`] takes: one coarse 16x16 slot
    // cannot hold the modes a split (or a pair of strips) leaves behind, so
    // when the map holds the block at exactly this strip's above/left mi it
    // is the neighbour libaom's `above_mi`/`left_mi` reads.
    if let Some(m) = neighbours.mode_above_mi(leaf_mi.0, leaf_mi.1) {
        above_mode = m;
    }
    if let Some(m) = neighbours.mode_left_mi(leaf_mi.0, leaf_mi.1) {
        left_mode = m;
    }
    let smooth_neighbor =
        is_smooth_mode(above_mode) || is_smooth_mode(left_mode);
    if smooth_neighbor {
        SMOOTH_LUMA_HITS.with(|c| c.set(c.get() + 1));
    }
    let (skip, mode, angle_delta_y, uv_mode, angle_delta_uv, alpha, filter_intra) =
        read_intra_mode_rect(
            dec,
            cdfs,
            above_mode,
            left_mode,
            true,
            bw,
            bh,
            enable_filter_intra,
            neighbours.skip_txfm_ctx(leaf_mi.0, leaf_mi.1),
            allow_screen_content_tools,
            leaf_mi.0,
            leaf_mi.1,
        )?;
    if std::env::var_os("EC_AV1_RECTX_TRACE").is_some() {
        eprintln!("  skip={skip} mode={mode} angle_delta_y={angle_delta_y} uv_mode={uv_mode}");
    }
    if filter_intra.is_some() {
        // lane-fistrip r1: `predict_filter_intra` already walks its 4x2
        // patches over a true `bw`x`bh` block (lane-rectsplit r1), so an
        // 8x16/16x8 strip predicts through the same call `reconstruct_rect`
        // below already makes; this refusal was square-only prose.
        FILTER_INTRA_RECT_HITS.with(|c| c.set(c.get() + 1));
        FILTER_INTRA_RECT_SUB16_HITS.with(|c| c.set(c.get() + 1));
    }
    let uv_predict_mode = if uv_mode == UV_CFL_PRED { DC_PRED } else { uv_mode };
    // lane-cfl r1, same defect as [`decode_leaf_8x8`]: chroma's intra-edge
    // filter type is the NEIGHBOUR's `uv_mode` (libaom `get_filt_type`), not
    // a constant `false`.
    let smooth_neighbor_uv = neighbours.smooth_uv_neighbour(leaf_mi.0, leaf_mi.1, r, c);
    let depth = if tx_select {
        let ctx = tx_size_context_rect(neighbours, leaf_mi, bw, bh);
        dec.symbol(&mut cdfs.tx_size_cat2[ctx])
    } else {
        0
    };
    if depth != 0 {
        return Err(unsupported(
            "a HORZ/VERT intra strip below 16x16 with a split transform (per-unit rect \
             prediction is not ported)",
        ));
    }
    let (px, py) = (leaf_mi.1 * MI, leaf_mi.0 * MI);
    let reach = Reach::of_rect(bw, bh, px, py, y.width, y.height);
    let (cpx, cpy) = (px / 2, py / 2);
    let (chroma_w, chroma_h) = (bw / 2, bh / 2);
    let (luma_levels, u_levels, v_levels);
    if skip {
        luma_levels = vec![0i32; bw * bh];
        u_levels = vec![0i32; chroma_w * chroma_h];
        v_levels = vec![0i32; chroma_w * chroma_h];
        y.reconstruct_rect(
            px, py, bw, bh, mode, angle_delta_y, reach, &luma_levels, None, filter_intra,
            smooth_neighbor,
        );
        let ac = alpha.map(|_| cfl_ac_q3_rect(y, px, py, bw, bh));
        u.reconstruct_rect(
            cpx, cpy, chroma_w, chroma_h, uv_predict_mode, angle_delta_uv, reach, &u_levels,
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)), None, smooth_neighbor_uv,
        );
        v.reconstruct_rect(
            cpx, cpy, chroma_w, chroma_h, uv_predict_mode, angle_delta_uv, reach, &v_levels,
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)), None, smooth_neighbor_uv,
        );
    } else {
        // lane-rectx: a genuine 16x8/8x16 luma transform (`TxbSet::LumaRect16x8`,
        // real `EOB_PT_128_LUMA` alphabet + the same mode-indexed `tx_type`
        // symbol the square `Luma16` set reads -- `get_ext_tx_set_type`'s
        // `use_reduced_set` branch returns `EXT_TX_SET_DTT4_IDTX` at
        // `tx_size_sqr_up == TX_16X16` regardless of the true, non-square
        // `tx_size`) and its chroma half (8x4/4x8, `TxbSet::ChromaRect8x4`).
        let (luma_scan, chroma_scan): (&[u16], &[u16]) = if bw == 16 {
            (&SCAN_16X8, &SCAN_8X4)
        } else {
            (&SCAN_8X16, &SCAN_4X8)
        };
        let around = neighbours.around_mi_rect(leaf_mi, bw, bh);
        let luma_set = if reduced_tx_set {
            TxbSet::LumaRect16x8
        } else {
            TxbSet::LumaRect16x8Set1
        };
        let mut luma_coding = cdfs.txb(luma_set, mode);
        let (l_levels, luma_tx_type) = read_coeffs_rect(
            dec, &mut luma_coding, luma_scan, bw, bh, 0, dc_sign_ctx(around[0].2), TxType::DctDct,
        )?;
        if std::env::var_os("EC_AV1_RECTX_TRACE").is_some() {
            let nz: Vec<(usize, i32)> = l_levels.iter().copied().enumerate().filter(|(_, v)| *v != 0).collect();
            eprintln!("  luma_tx_type={luma_tx_type:?} nz_levels={nz:?}");
        }
        luma_levels = l_levels;
        let luma_residual = dequant_and_inverse_typed_wh(
            &luma_levels, bw, bh, crate::decode::bit_depth(),
            CURRENT_Q_IDX.with(|c| c.get()), plane_q_delta(0).0, plane_q_delta(0).1, luma_tx_type,
        );
        y.reconstruct_rect(
            px, py, bw, bh, mode, angle_delta_y, reach, &luma_residual, None, filter_intra,
            smooth_neighbor,
        );
        let ac = alpha.map(|_| cfl_ac_q3_rect(y, px, py, bw, bh));
        let u_default_tx = default_intra_tx_type(uv_predict_mode as u8);
        let u_skip_ctx = usize::from(around[1].0) + usize::from(around[1].1);
        let mut u_coding = cdfs.txb(TxbSet::ChromaRect8x4, uv_predict_mode);
        let (u_l, u_tx_type) = read_coeffs_rect(
            dec, &mut u_coding, chroma_scan, chroma_w, chroma_h, u_skip_ctx,
            dc_sign_ctx(around[1].2), u_default_tx,
        )?;
        u_levels = u_l;
        let u_residual = dequant_and_inverse_typed_wh(
            &u_levels, chroma_w, chroma_h, crate::decode::bit_depth(),
            CURRENT_Q_IDX.with(|c| c.get()), plane_q_delta(1).0, plane_q_delta(1).1, u_tx_type,
        );
        u.reconstruct_rect(
            cpx, cpy, chroma_w, chroma_h, uv_predict_mode, angle_delta_uv, reach, &u_residual,
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)), None, smooth_neighbor_uv,
        );
        let v_default_tx = default_intra_tx_type(uv_predict_mode as u8);
        let v_skip_ctx = usize::from(around[2].0) + usize::from(around[2].1);
        let mut v_coding = cdfs.txb(TxbSet::ChromaRect8x4, uv_predict_mode);
        let (v_l, v_tx_type) = read_coeffs_rect(
            dec, &mut v_coding, chroma_scan, chroma_w, chroma_h, v_skip_ctx,
            dc_sign_ctx(around[2].2), v_default_tx,
        )?;
        v_levels = v_l;
        let v_residual = dequant_and_inverse_typed_wh(
            &v_levels, chroma_w, chroma_h, crate::decode::bit_depth(),
            CURRENT_Q_IDX.with(|c| c.get()), plane_q_delta(2).0, plane_q_delta(2).1, v_tx_type,
        );
        v.reconstruct_rect(
            cpx, cpy, chroma_w, chroma_h, uv_predict_mode, angle_delta_uv, reach, &v_residual,
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)), None, smooth_neighbor_uv,
        );
        RECT_LEAF_COEFF_HITS.with(|c| c.set(c.get() + 1));
    }
    neighbours.record_mi_rect(leaf_mi, bw, bh, &[luma_levels, u_levels, v_levels]);
    neighbours.fill_skip_grid_rect(leaf_mi, mi_w, mi_h, skip);
    neighbours.fill_lf_grid_rect(leaf_mi, mi_w, mi_h, bw as u8, bh as u8, 0);
    // A rect leaf spans mi_w x mi_h, so its mode reaches several columns and
    // rows -- record the whole span, not one cell.
    neighbours.record_mode_mi(leaf_mi.0, leaf_mi.1, mi_w, mi_h, mode);
    neighbours.record_uv_mode_mi(leaf_mi.0, leaf_mi.1, mi_w, mi_h, uv_predict_mode);
    Ok(mode)
}

/// Decodes one 32x8 / 8x32 strip of a 32x32-level `PARTITION_HORZ_4` /
/// `PARTITION_VERT_4` (lane-tx64x16 r3): [`decode_block_rect`]'s 4:1 sibling,
/// addressed by REAL mi coordinates for the same reason [`decode_leaf_rect`]
/// is -- strips 1 and 3 start 8 px into a 16-px [`SUB`] cell, which an
/// `at`-derived `px = c * SUB` cannot name. `prev_strip` carries the previous
/// strip's mode so this one's mode context sees its true above/left
/// neighbour inside the same 32x32 (`decode_leaf_rect`'s convention).
///
/// Tables: LUMA `TX_32X8`/`TX_8X32` averages to the 16x16 entropy context
/// ([`TxbSet::LumaRect32x8`], 256 positions) and scans with
/// [`SCAN_32X8`]/[`SCAN_8X32`]; CHROMA `TX_16X4`/`TX_4X16` averages to the
/// 8x8 one ([`TxbSet::Chroma8`], 64 positions) and scans with
/// [`SCAN_16X4`]/[`SCAN_4X16`]. Neither shape takes the rect sqrt2 scale --
/// `get_rect_tx_log_ratio` fires at ratio 1 only, and this is ratio 2
/// (`inverse_transform_2d_typed_wh`'s own `rect_scale`).
///
/// A split transform (`tx_depth != 0`) refuses by name: `depth_to_tx_size` of
/// a 4:1 strip is not simply `bw.min(bh) >> (depth - 1)` the way a 2:1
/// strip's is, so [`decode_rect_split`]'s per-unit walk cannot be reused
/// unexamined.
#[allow(clippy::too_many_arguments)]
fn decode_block_rect4(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    strip_mi: (usize, usize),
    bw: usize,
    bh: usize,
    prev_strip: Option<((usize, usize), usize)>,
    y: &mut PlaneBuf,
    u: &mut PlaneBuf,
    v: &mut PlaneBuf,
    enable_filter_intra: bool,
    allow_screen_content_tools: bool,
    base_q_idx: u8,
    tx_select: bool,
) -> Result<(usize, usize)> {
    let _ = base_q_idx;
    let (mi_r, mi_c) = strip_mi;
    let (r, c) = (mi_r / (SUB / MI), mi_c / (SUB / MI));
    let (mi_w, mi_h) = (bw / MI, bh / MI);
    let mut above_mode = neighbours.above_mode[c];
    let mut left_mode = neighbours.left_mode[r];
    if let Some(((pr, pc), pmode)) = prev_strip {
        if pc == mi_c && mi_r == pr + mi_h {
            above_mode = pmode;
        } else if pr == mi_r && mi_c == pc + mi_w {
            left_mode = pmode;
        }
    }
    let (skip, mode, angle_delta_y, uv_mode, angle_delta_uv, alpha, filter_intra) =
        read_intra_mode_rect(
            dec,
            cdfs,
            above_mode,
            left_mode,
            true,
            bw,
            bh,
            enable_filter_intra,
            neighbours.skip_txfm_ctx(mi_r, mi_c),
            allow_screen_content_tools,
            mi_r,
            mi_c,
        )?;
    let smooth_neighbor = is_smooth_mode(above_mode) || is_smooth_mode(left_mode);
    if smooth_neighbor {
        SMOOTH_LUMA_HITS.with(|c| c.set(c.get() + 1));
    }
    let smooth_neighbor_uv =
        neighbours.smooth_uv_neighbour(r * (SUB / MI), c * (SUB / MI), r, c);
    if filter_intra.is_some() {
        FILTER_INTRA_RECT_HITS.with(|c| c.set(c.get() + 1));
    }
    let uv_predict_mode = if uv_mode == UV_CFL_PRED {
        DC_PRED
    } else {
        uv_mode
    };
    let depth = if tx_select {
        let ctx = tx_size_context_rect(neighbours, (mi_r, mi_c), bw, bh);
        dec.symbol(&mut cdfs.tx_size_cat2[ctx])
    } else {
        0
    };
    if depth != 0 {
        // lane-tx64x16 r4: the depth is IN the message -- it says which port
        // the next round owes. depth 1 of a 4:1 strip is TX_16X8/TX_8X16
        // (`sub_tx_size_map[TX_32X8] == TX_16X8`), a RECTANGULAR transform
        // unit with no luma coefficient tables here; depth 2 is TX_8X8,
        // square, the shape `decode_rect_split` already walks. The film stops
        // at depth 2, synthetic band fixtures hit both.
        return Err(unsupported(format!(
            "a 32x32-level 1:4 strip with a split transform (per-unit 4:1 prediction is not \
             ported, depth={depth})"
        )));
    }
    // The coarse (16-px [`SUB`] cell) mode/side arrays cannot name a strip
    // that is 8 px tall, so each strip stamps every cell it touches and the
    // LAST strip wins -- which is the right answer for the block below/right
    // of the 32x32, the only reader outside it. Written after this strip's
    // own context read above, never before.
    for cell in 0..bw.div_ceil(SUB) {
        neighbours.above_mode[c + cell] = mode;
        neighbours.above_uv_mode[c + cell] = uv_predict_mode;
        neighbours.above_side[c + cell] = bw;
    }
    for cell in 0..bh.div_ceil(SUB) {
        neighbours.left_mode[r + cell] = mode;
        neighbours.left_uv_mode[r + cell] = uv_predict_mode;
        neighbours.left_side[r + cell] = bh;
    }
    let (px, py) = (mi_c * MI, mi_r * MI);
    let (cpx, cpy) = (px / 2, py / 2);
    let (chroma_w, chroma_h) = (bw / 2, bh / 2);
    let reach = Reach::of_rect(bw, bh, px, py, y.width, y.height);
    let (luma_scan, chroma_scan): (&[u16], &[u16]) = if bw > bh {
        (&SCAN_32X8, &SCAN_16X4)
    } else {
        (&SCAN_8X32, &SCAN_4X16)
    };
    let (luma_levels, u_levels, v_levels) = if skip {
        (
            vec![0i32; bw * bh],
            vec![0i32; chroma_w * chroma_h],
            vec![0i32; chroma_w * chroma_h],
        )
    } else {
        let around = neighbours.around_mi_rect((mi_r, mi_c), bw, bh);
        let mut luma_coding = cdfs.txb(TxbSet::LumaRect32x8, mode);
        let (luma_levels, luma_tx_type) = read_coeffs_rect(
            dec,
            &mut luma_coding,
            luma_scan,
            bw,
            bh,
            0,
            dc_sign_ctx(around[0].2),
            TxType::DctDct,
        )?;
        let mut planes = Vec::with_capacity(2);
        for plane in 1..3 {
            let default_tx = default_intra_tx_type(uv_predict_mode as u8);
            let skip_ctx = usize::from(around[plane].0) + usize::from(around[plane].1);
            let mut coding = cdfs.txb(TxbSet::Chroma8, uv_predict_mode);
            let (levels, tx_type) = read_coeffs_rect(
                dec,
                &mut coding,
                chroma_scan,
                chroma_w,
                chroma_h,
                skip_ctx,
                dc_sign_ctx(around[plane].2),
                default_tx,
            )?;
            planes.push((levels, tx_type));
        }
        let (v_levels, v_tx_type) = planes.pop().expect("plane 2");
        let (u_levels, u_tx_type) = planes.pop().expect("plane 1");
        RECT4_COEFF_HITS.with(|c| c.set(c.get() + 1));
        let luma_residual = dequant_and_inverse_typed_wh(
            &luma_levels,
            bw,
            bh,
            crate::decode::bit_depth(),
            block_q_idx(),
            plane_q_delta(0).0,
            plane_q_delta(0).1,
            luma_tx_type,
        );
        y.reconstruct_rect(
            px,
            py,
            bw,
            bh,
            mode,
            angle_delta_y,
            reach,
            &luma_residual,
            None,
            filter_intra,
            smooth_neighbor,
        );
        let ac = alpha.map(|_| cfl_ac_q3_rect(y, px, py, bw, bh));
        for (plane, buf, levels, tx_type) in [
            (1usize, &mut *u, &u_levels, u_tx_type),
            (2, &mut *v, &v_levels, v_tx_type),
        ] {
            let residual = dequant_and_inverse_typed_wh(
                levels,
                chroma_w,
                chroma_h,
                crate::decode::bit_depth(),
                block_q_idx(),
                plane_q_delta(plane).0,
                plane_q_delta(plane).1,
                tx_type,
            );
            buf.reconstruct_rect(
                cpx,
                cpy,
                chroma_w,
                chroma_h,
                uv_predict_mode,
                angle_delta_uv,
                reach,
                &residual,
                alpha
                    .zip(ac.as_deref())
                    .map(|((au, av), ac)| (if plane == 1 { au } else { av }, ac)),
                None,
                smooth_neighbor_uv,
            );
        }
        neighbours.record_mi_rect(
            (mi_r, mi_c),
            bw,
            bh,
            &[luma_levels, u_levels, v_levels],
        );
        neighbours.fill_skip_grid_rect((mi_r, mi_c), mi_w, mi_h, skip);
        neighbours.fill_lf_grid_rect((mi_r, mi_c), mi_w, mi_h, bw as u8, bh as u8, 0);
        if bw > bh {
            RECT4_32_HORZ_HITS.with(|c| c.set(c.get() + 1));
        } else {
            RECT4_32_VERT_HITS.with(|c| c.set(c.get() + 1));
        }
        return Ok((mode, uv_predict_mode));
    };
    y.reconstruct_rect(
        px,
        py,
        bw,
        bh,
        mode,
        angle_delta_y,
        reach,
        &luma_levels,
        None,
        filter_intra,
        smooth_neighbor,
    );
    let ac = alpha.map(|_| cfl_ac_q3_rect(y, px, py, bw, bh));
    u.reconstruct_rect(
        cpx,
        cpy,
        chroma_w,
        chroma_h,
        uv_predict_mode,
        angle_delta_uv,
        reach,
        &u_levels,
        alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
        None,
        smooth_neighbor_uv,
    );
    v.reconstruct_rect(
        cpx,
        cpy,
        chroma_w,
        chroma_h,
        uv_predict_mode,
        angle_delta_uv,
        reach,
        &v_levels,
        alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
        None,
        smooth_neighbor_uv,
    );
    neighbours.record_mi_rect((mi_r, mi_c), bw, bh, &[luma_levels, u_levels, v_levels]);
    neighbours.fill_skip_grid_rect((mi_r, mi_c), mi_w, mi_h, skip);
    neighbours.fill_lf_grid_rect((mi_r, mi_c), mi_w, mi_h, bw as u8, bh as u8, 0);
    if bw > bh {
        RECT4_32_HORZ_HITS.with(|c| c.set(c.get() + 1));
    } else {
        RECT4_32_VERT_HITS.with(|c| c.set(c.get() + 1));
    }
    Ok((mode, uv_predict_mode))
}

/// Decodes one true `bw`x`bh` superblock-level `PARTITION_HORZ`/
/// `PARTITION_VERT` strip (lane-sbpart r2, `bw, bh` one of `(64, 32)` /
/// `(32, 64)` -- [`decode_block_rect`]'s own sibling one level up). Unlike
/// that 32x16/16x32 strip, both dimensions here exceed the 32-coefficient
/// cap, so LUMA needs the same corner truncation [`decode_block`] already
/// does for a plain 64x64 square (spec 5.11.40): `get_txsize_entropy_ctx`
/// resolves TX_64X32/TX_32X64 to TX_64X64 regardless of orientation (see
/// this function's own charter/report), so both strips read a real 32x32
/// corner through [`read_coeffs`] with [`TxbSet::Luma64`]/[`SCAN_32`] and
/// embed it top-left of a zeroed `bw`x`bh` grid, mirroring
/// [`read_plane`]'s `tx_side != side` branch generalized to non-square.
/// CHROMA (TX_32X16/TX_16X32) resolves to TX_32X32 -- a real, *untruncated*
/// `bw/2`x`bh/2` transform via [`read_coeffs_rect`] with
/// [`TxbSet::ChromaRect32x16`] and the matching [`SCAN_32X16`]/
/// [`SCAN_16X32`], same as [`decode_block_rect`]'s own chroma path. Like
/// that function, only the single-transform-unit (`tx_depth == 0`) case is
/// supported: a split transform refuses by name rather than guess-decode.
#[allow(clippy::too_many_arguments)]
fn decode_block_rect64(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    at: (usize, usize),
    bw: usize,
    bh: usize,
    y: &mut PlaneBuf,
    u: &mut PlaneBuf,
    v: &mut PlaneBuf,
    enable_filter_intra: bool,
    allow_screen_content_tools: bool,
    base_q_idx: u8,
    tx_select: bool,
    reduced_tx_set: bool,
) -> Result<()> {
    let (r, c) = at;
    let (px, py) = (c * SUB, r * SUB);
    let (nb_above_mode, nb_left_mode) = neighbours.modes_above_left(r, c);
    let (skip, mode, angle_delta_y, uv_mode, angle_delta_uv, alpha, filter_intra) =
        read_intra_mode_rect(
            dec,
            cdfs,
            nb_above_mode,
            nb_left_mode,
            // `is_cfl_allowed` (spec 5.11.5) caps CFL at <=32x32; these
            // superblock-level HORZ/VERT strips are 64x32/32x64, so unlike
            // `decode_block_rect`'s own 32x16/16x32 strips (where `true` is
            // correct), CFL must never be offered here -- passing `true`
            // read `uv_mode` off the 14-symbol `uv_mode_cfl` CDF instead of
            // the real 13-symbol `uv_mode_no_cfl` one and desynced the tile
            // from this block's very first symbol (lane-sbpart r3 bisect).
            false,
            bw,
            bh,
            enable_filter_intra,
            neighbours.skip_txfm_ctx(r * (SUB / MI), c * (SUB / MI)),
            allow_screen_content_tools,
            r * (SUB / MI),
            c * (SUB / MI),
        )?;
    let smooth_neighbor = is_smooth_mode(nb_above_mode) || is_smooth_mode(nb_left_mode);
    if smooth_neighbor {
        SMOOTH_LUMA_HITS.with(|c| c.set(c.get() + 1));
    }
    let smooth_neighbor_uv =
        neighbours.smooth_uv_neighbour(r * (SUB / MI), c * (SUB / MI), r, c);
    if filter_intra.is_some() {
        // `filter_intra_size_class_rect` already returns `None` for both
        // `(64, 32)` and `(32, 64)` (`av1_filter_intra_allowed_bsize` caps at
        // 32 on both axes), so this symbol is never read at this level --
        // kept only as the same defensive refusal [`decode_block_rect`] has.
        return Err(unsupported(
            "filter intra on a superblock-level HORZ/VERT strip (never expected -- \
             av1_filter_intra_allowed_bsize caps at 32x32)",
        ));
    }
    let uv_predict_mode = if uv_mode == UV_CFL_PRED {
        DC_PRED
    } else {
        uv_mode
    };
    let (mi_r, mi_c) = (r * (SUB / MI), c * (SUB / MI));
    let depth = if tx_select {
        let ctx = tx_size_context_rect(neighbours, (mi_r, mi_c), bw, bh);
        // lane-rectsplit r1: `bsize_to_tx_size_cat` (libaom `decodemv.c`)
        // counts `sub_tx_size_map` steps from the block's own max rect
        // transform down to TX_4X4, minus one -- BLOCK_64X32's chain
        // (TX_64X32 -> TX_32X32 -> TX_16X16 -> TX_8X8 -> TX_4X4) is four
        // steps, so this level's category is 3 (the same one a 64x64 square
        // block uses), not the 2 a 32x16 strip uses. Reading the cat-2 CDF
        // here was a wrong-table read of a real symbol.
        dec.symbol(&mut cdfs.tx_size_cat3[ctx])
    } else {
        0
    };
    if depth != 0 {
        TX_DEPTH_HITS.with(|c| c.set(c.get() + 1));
        // lane-rectsplit r1/r4: the superblock-level strip splits its
        // transform through the very same per-unit path as its 32x32-level
        // sibling (`sub_tx_size_map[TX_64X32] == TX_32X32`, square from the
        // first step on, so `bw.min(bh) >> (depth - 1)` names the unit).
        let tx = bw.min(bh) >> (depth - 1);
        if std::env::var_os("EC_SBPART_DUMP64").is_some() {
            eprintln!(
                "DUMP64SPLIT mi_r={mi_r} mi_c={mi_c} px={px} py={py} bw={bw} bh={bh} \
                 depth={depth} tx={tx} mode={mode} uv={uv_predict_mode} skip={skip} \
                 angle_y={angle_delta_y}"
            );
        }
        let modes = RectStripModes {
            skip,
            mode,
            angle_delta_y,
            uv_predict_mode,
            angle_delta_uv,
            alpha,
            filter_intra,
            smooth_neighbor,
            smooth_neighbor_uv,
        };
        decode_rect_split(
            dec, cdfs, neighbours, at, bw, bh, tx, &modes, y, u, v, base_q_idx, reduced_tx_set,
        )?;
        RECT_PARTITION_HITS.with(|c| c.set(c.get() + 1));
        return Ok(());
    }
    let (tx_w, tx_h) = (bw, bh);
    let (cpx, cpy) = (px / 2, py / 2);
    let (chroma_w, chroma_h) = (bw / 2, bh / 2);
    let reach = Reach::of_rect(bw, bh, px, py, y.width, y.height);
    if skip {
        y.reconstruct_rect(
            px,
            py,
            bw,
            bh,
            mode,
            angle_delta_y,
            reach,
            &vec![0i32; bw * bh],
            None,
            filter_intra,
            smooth_neighbor,
        );
        let ac = alpha.map(|_| cfl_ac_q3_rect(y, px, py, bw, bh));
        u.reconstruct_rect(
            cpx,
            cpy,
            chroma_w,
            chroma_h,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            &vec![0i32; chroma_w * chroma_h],
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
            None,
            smooth_neighbor_uv,
        );
        v.reconstruct_rect(
            cpx,
            cpy,
            chroma_w,
            chroma_h,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            &vec![0i32; chroma_w * chroma_h],
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
            None,
            smooth_neighbor_uv,
        );
        neighbours.record_rect(
            at,
            bw,
            bh,
            mode,
            uv_predict_mode,
            &[
                vec![0i32; bw * bh],
                vec![0i32; chroma_w * chroma_h],
                vec![0i32; chroma_w * chroma_h],
            ],
        );
    } else {
        let around = neighbours.around_rect(at, bw, bh);
        // LUMA: the coded corner, embedded top-left of the true bw x bh grid
        // (see this function's own doc comment). A 64-length axis codes only
        // its low 32 coefficients, so the corner is `min(bw, 32)` x
        // `min(bh, 32)`: 32x32 under a 64x32/32x64 HORZ/VERT strip, and
        // 32x16/16x32 under a 64x16/16x64 1:4 strip (lane-tx64x16).
        let (luma_cw, luma_ch) = (bw.min(32), bh.min(32));
        let scan32 = default_scan(TX32);
        if std::env::var_os("EC_TRACE_COEFF").is_some() {
            let (rng, _) = dec.debug_state();
            eprintln!("EC_COEFF plane=0 row={mi_r} col={mi_c} tx_size={luma_cw}x{luma_ch} rng={rng}");
        }
        let (luma_corner, luma_tx_type) = if (luma_cw, luma_ch) == (32, 32) {
            let mut luma_coding = cdfs.txb(TxbSet::Luma64, mode);
            read_coeffs(
                dec,
                &mut luma_coding,
                &scan32,
                0,
                dc_sign_ctx(around[0].2),
                TxType::DctDct,
                Some((bw, bh)),
            )?
        } else {
            // `av1_get_adjusted_tx_size` (`blockd.h:1361`) maps TX_64X16 ->
            // TX_32X16 and TX_16X64 -> TX_16X32, and `av1_scan_orders`
            // (`scan.c`) gives both the very same `default_scan_32x16` /
            // `default_scan_16x32` the 2:1 sizes use ("Half of the
            // coefficients of tx64 at higher frequencies are set to zeros. So
            // tx32's scan order is used"). `get_txsize_entropy_ctx` is
            // (txsize_sqr + txsize_sqr_up + 1) >> 1 = (TX_16X16 + TX_64X64 +
            // 1) >> 1 = TX_32X32 and `txsize_log2_minus4[TX_64X16]` is 5 (a
            // 512-position eob group) -- BOTH identical to TX_32X16, so the
            // 2:1 CDF set `LumaRect32x16` is bit-exact here and no new table
            // is owed. `av1_nz_map_ctx_offset[TX_64X16]` is literally
            // `av1_nz_map_ctx_offset_32x16` (`txb_common.c:340`), which is
            // what `base_ctx_rect` computes at (32, 16).
            let luma_scan: &[u16] = if bw == 64 { &SCAN_32X16 } else { &SCAN_16X32 };
            let mut luma_coding = cdfs.txb(TxbSet::LumaRect32x16, mode);
            read_coeffs_rect(
                dec,
                &mut luma_coding,
                luma_scan,
                luma_cw,
                luma_ch,
                0,
                dc_sign_ctx(around[0].2),
                TxType::DctDct,
            )?
        };
        if std::env::var_os("EC_TRACE_COEFF").is_some() {
            let (rng, _) = dec.debug_state();
            eprintln!("EC_COEFF_VAL plane=0 row={mi_r} col={mi_c} rng={rng}");
        }
        let mut luma_levels = vec![0i32; bw * bh];
        for row in 0..luma_ch {
            luma_levels[row * bw..][..luma_cw]
                .copy_from_slice(&luma_corner[row * luma_cw..][..luma_cw]);
        }
        {
            let cur = block_q_idx();
            if cur != i32::from(base_q_idx) {
                RECT64_QIDX_DRIFT_HITS.with(|c| c.set(c.get() + 1));
            }
            if std::env::var_os("EC_AV1_TRACE").is_some() {
                eprintln!(
                    "TRACE rect64_dequant plane=0 base_q_idx={base_q_idx} current_q_idx={cur}"
                );
            }
        }
        let luma_residual = dequant_and_inverse_typed_wh(
            &luma_levels,
            bw,
            bh,
            crate::decode::bit_depth(),
            block_q_idx(),
            plane_q_delta(0).0,
            plane_q_delta(0).1,
            luma_tx_type,
        );
        y.reconstruct_rect(
            px,
            py,
            bw,
            bh,
            mode,
            angle_delta_y,
            reach,
            &luma_residual,
            None,
            filter_intra,
            smooth_neighbor,
        );
        if std::env::var_os("EC_SBPART_DUMP64").is_some() {
            let leftcol: Vec<u16> = if px > 0 {
                (py..py + bh).map(|row| y.data[row * y.width + px - 1]).collect()
            } else {
                vec![]
            };
            eprintln!(
                "DUMP64 mi_r={mi_r} mi_c={mi_c} px={px} py={py} bw={bw} bh={bh} mode={mode} \
                 skip={skip} eob_nonzero={} row0={:?} leftcol={:?}",
                luma_corner.iter().any(|&v| v != 0),
                &y.data[py * y.width + px..][..bw],
                leftcol,
            );
        }
        // CHROMA: a real, untruncated chroma_w x chroma_h transform -- no
        // corner crop needed (see doc comment).
        let ac = alpha.map(|_| cfl_ac_q3_rect(y, px, py, bw, bh));
        // The chroma plane is never truncated (both axes <= 32 after
        // subsampling). Under a 1:4 strip it is a true 32x8/8x32:
        // `get_txsize_entropy_ctx(TX_32X8)` = (TX_8X8 + TX_32X32 + 1) >> 1 =
        // TX_16X16 and `txsize_log2_minus4[TX_32X8]` = 4 (256 positions),
        // both identical to TX_16X16 -- so [`TxbSet::Chroma16`] is the exact
        // set, and only the scan is new (lane-tx64x16).
        let (chroma_set, chroma_scan): (TxbSet, &[u16]) = match (bw, bh) {
            (64, 16) => (TxbSet::Chroma16, &SCAN_32X8),
            (16, 64) => (TxbSet::Chroma16, &SCAN_8X32),
            (64, _) => (TxbSet::ChromaRect32x16, &SCAN_32X16),
            _ => (TxbSet::ChromaRect32x16, &SCAN_16X32),
        };
        let u_skip_ctx = usize::from(around[1].0) + usize::from(around[1].1);
        let mut u_coding = cdfs.txb(chroma_set, uv_predict_mode);
        if std::env::var_os("EC_TRACE_COEFF").is_some() {
            let (rng, _) = dec.debug_state();
            eprintln!("EC_COEFF plane=1 row={mi_r} col={mi_c} tx_size=rect32x16 rng={rng}");
        }
        let (u_levels, u_tx_type) = read_coeffs_rect(
            dec,
            &mut u_coding,
            chroma_scan,
            chroma_w,
            chroma_h,
            u_skip_ctx,
            dc_sign_ctx(around[1].2),
            TxType::DctDct,
        )?;
        if std::env::var_os("EC_TRACE_COEFF").is_some() {
            let (rng, _) = dec.debug_state();
            eprintln!("EC_COEFF_VAL plane=1 row={mi_r} col={mi_c} rng={rng}");
        }
        {
            let cur = block_q_idx();
            if cur != i32::from(base_q_idx) {
                RECT64_QIDX_DRIFT_HITS.with(|c| c.set(c.get() + 1));
            }
            if std::env::var_os("EC_AV1_TRACE").is_some() {
                eprintln!(
                    "TRACE rect64_dequant plane=1 base_q_idx={base_q_idx} current_q_idx={cur}"
                );
            }
        }
        let u_residual = dequant_and_inverse_typed_wh(
            &u_levels,
            chroma_w,
            chroma_h,
            crate::decode::bit_depth(),
            block_q_idx(),
            plane_q_delta(1).0,
            plane_q_delta(1).1,
            u_tx_type,
        );
        u.reconstruct_rect(
            cpx,
            cpy,
            chroma_w,
            chroma_h,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            &u_residual,
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
            None,
            smooth_neighbor_uv,
        );
        let v_skip_ctx = usize::from(around[2].0) + usize::from(around[2].1);
        let mut v_coding = cdfs.txb(chroma_set, uv_predict_mode);
        if std::env::var_os("EC_TRACE_COEFF").is_some() {
            let (rng, _) = dec.debug_state();
            eprintln!("EC_COEFF plane=2 row={mi_r} col={mi_c} tx_size=rect32x16 rng={rng}");
        }
        let (v_levels, v_tx_type) = read_coeffs_rect(
            dec,
            &mut v_coding,
            chroma_scan,
            chroma_w,
            chroma_h,
            v_skip_ctx,
            dc_sign_ctx(around[2].2),
            TxType::DctDct,
        )?;
        if std::env::var_os("EC_TRACE_COEFF").is_some() {
            let (rng, _) = dec.debug_state();
            eprintln!("EC_COEFF_VAL plane=2 row={mi_r} col={mi_c} rng={rng}");
        }
        {
            let cur = block_q_idx();
            if cur != i32::from(base_q_idx) {
                RECT64_QIDX_DRIFT_HITS.with(|c| c.set(c.get() + 1));
            }
            if std::env::var_os("EC_AV1_TRACE").is_some() {
                eprintln!(
                    "TRACE rect64_dequant plane=2 base_q_idx={base_q_idx} current_q_idx={cur}"
                );
            }
        }
        let v_residual = dequant_and_inverse_typed_wh(
            &v_levels,
            chroma_w,
            chroma_h,
            crate::decode::bit_depth(),
            block_q_idx(),
            plane_q_delta(2).0,
            plane_q_delta(2).1,
            v_tx_type,
        );
        v.reconstruct_rect(
            cpx,
            cpy,
            chroma_w,
            chroma_h,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            &v_residual,
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
            None,
            smooth_neighbor_uv,
        );
        neighbours.record_rect(at, bw, bh, mode, uv_predict_mode, &[luma_levels, u_levels, v_levels]);
        RECT_COEFF_HITS.with(|c| c.set(c.get() + 1));
    }
    neighbours.fill_skip_grid_rect((mi_r, mi_c), bw / MI, bh / MI, skip);
    neighbours.fill_lf_grid_rect((mi_r, mi_c), bw / MI, bh / MI, tx_w as u8, tx_h as u8, 0);
    SB_RECT_HITS.with(|c| c.set(c.get() + 1));
    match (bw, bh) {
        (64, 16) => SB_RECT4_HORZ_HITS.with(|c| c.set(c.get() + 1)),
        (16, 64) => SB_RECT4_VERT_HITS.with(|c| c.set(c.get() + 1)),
        _ => {}
    }
    if std::env::var_os("EC_AV1_TRACE").is_some() {
        let (rng, _) = dec.debug_state();
        eprintln!("TRACE_RECT64_END mi_row={mi_r} mi_col={mi_c} bw={bw} bh={bh} rng={rng}");
    }
    Ok(())
}

/// `read_angle_delta`/`Angle_Delta` (spec 5.11.42 + 9.3): the symbol minus
/// [`ANGLE_DELTA_ZERO`] gives the signed `-MAX_ANGLE_DELTA..=MAX_ANGLE_DELTA`
/// delta [`crate::intra::predict`] wants; bumps [`ANGLE_DELTA_HITS`] whenever
/// it lands away from zero, for gate tests to prove a real stream fired one.
fn read_angle_delta(dec: &mut SymbolDecoder, cdf: &mut [u16]) -> i32 {
    let symbol = dec.symbol(cdf);
    if symbol != ANGLE_DELTA_ZERO {
        ANGLE_DELTA_HITS.with(|c| c.set(c.get() + 1));
    }
    symbol as i32 - ANGLE_DELTA_ZERO as i32
}

#[allow(clippy::too_many_arguments)]
fn read_intra_mode(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    above_mode: usize,
    left_mode: usize,
    cfl: bool,
    side: usize,
    enable_filter_intra: bool,
    skip_ctx: usize,
    allow_screen_content_tools: bool,
    allow_intrabc: bool,
    // `Some((palette_mode_ctx, color_cache))` at the call sites this lane's
    // scope actually reconstructs a palette-Y block for (`av1_get_palette_mode_ctx`
    // and `av1_get_palette_cache`'s neighbour lookup, [`Neighbours::palette_ctx_and_cache`]);
    // `None` at the excluded call sites (the 8x8-leaf path), which still read
    // the `palette_y_mode` symbol at ctx 0 -- matching the bits a real
    // decoder reads either way -- but refuse by name the moment it fires,
    // exactly the old behaviour.
    palette: Option<(usize, &[u16])>,
    // The U-channel colour cache for a chroma palette read ([`Neighbours::palette_uv_cache`]),
    // `&[]` at the excluded call sites -- same corner-cut as `palette` above,
    // no `mode_ctx` needed (`palette_uv_mode_ctx` is this block's own just-read
    // `use_palette_y`, not a neighbour lookup).
    palette_uv_cache: &[u16],
    mi_r: usize,
    mi_c: usize,
) -> Result<(
    bool,
    usize,
    i32,
    usize,
    i32,
    Option<(i32, i32)>,
    Option<usize>,
    Option<PaletteY>,
    Option<PaletteUv>,
)> {
    let trace = std::env::var_os("EC_AV1_TRACE").is_some();
    // lane-tiny r2: EC_ISTEP-format trace (same "name=... val=... rng=..."
    // shape the oracle's own `ec_read_intra_frame_mode_info_impl` prints
    // under `EC_TRACE_MODE_STEP`) so a range ladder can be diffed line for
    // line against the instrumented aomdec, not just `tell()`.
    let ec_istep = std::env::var_os("EC_TRACE_MODE_STEP").is_some();
    macro_rules! istep {
        ($name:literal, $val:expr) => {
            if ec_istep {
                let (rng, _) = dec.debug_state();
                eprintln!(
                    "EC_ISTEP mi_row={mi_r} mi_col={mi_c} name={} val={} rng={rng}",
                    $name, $val
                );
            }
        };
    }
    // spec 5.11.6 `intra_frame_mode_info`: `intra_segment_id` is read BEFORE
    // `skip` when `SegIdPreSkip`, and after it otherwise.
    let (seg_w_mi, seg_h_mi) = (side / 4, side / 4);
    if seg_id_pre_skip() {
        intra_segment_id(dec, cdfs, mi_r, mi_c, seg_w_mi, seg_h_mi, false);
    }
    let skip = dec.symbol(&mut cdfs.skip[skip_ctx]) != 0;
    if trace {
        eprintln!("TRACE skip value={} rng={}", skip as i32, dec.debug_state().0);
    }
    istep!("skip", skip as i32);
    if !seg_id_pre_skip() {
        intra_segment_id(dec, cdfs, mi_r, mi_c, seg_w_mi, seg_h_mi, skip);
    }
    // spec order (see the comment below on `read_intrabc_info`): `skip`,
    // `segment_id`, `cdef`, `delta_q` -- `cdef` lands right here.
    maybe_read_cdef_idx(dec, mi_r, mi_c, skip);
    istep!("cdef", 0);
    maybe_read_delta_q(dec, cdfs, mi_r, mi_c, side == 64, skip);
    maybe_read_delta_lf(dec, cdfs, mi_r, mi_c, side == 64, skip);
    istep!("dq", 0);
    // `read_intrabc_info` (spec 5.11.13, libaom decodemv.c:693, called at
    // :811 right after `skip_txfm`/`segment_id`/`cdef`/`delta_q` and before
    // `mbmi->mode` is ever read): a `use_intrabc` symbol only present on an
    // intra frame whose header set `allow_intrabc`. Actual intra block copy
    // needs `assign_dv`'s own motion-vector-prediction machinery (`av1_find_mv_refs`,
    // `av1_find_ref_dv`) this decoder does not carry -- lane-screen scope
    // draws the line at consuming this one flag symbol and refusing by name
    // when it fires, so the arithmetic decoder still resyncs for a stream
    // that merely allows (but never uses) intrabc.
    if allow_intrabc {
        let use_intrabc = dec.symbol(&mut cdfs.intrabc) != 0;
        if trace {
            eprintln!("TRACE use_intrabc value={}", use_intrabc as i32);
        }
        if use_intrabc {
            let dv = read_intrabc_dv(dec, cdfs, mi_r, mi_c, side);
            if trace {
                eprintln!("TRACE intrabc dv={dv:?}");
            }
            INTRABC_DV.with(|c| c.set(Some(dv)));
            INTRABC_HITS.with(|c| c.set(c.get() + 1));
            // spec 5.11.13 / libaom `read_intra_frame_mode_info` returns the
            // moment `is_intrabc_block`: no y/uv mode, angle delta, palette,
            // CFL or filter-intra syntax follows. `YMode`/`UVMode` are forced
            // `DC_PRED` (which is also what the neighbour mode contexts and
            // the loop filter then see).
            return Ok((skip, DC_PRED, 0, DC_PRED, 0, None, None, None, None));
        }
    }
    let above_ctx = INTRA_MODE_CTX[above_mode];
    let left_ctx = INTRA_MODE_CTX[left_mode];
    let mode = dec.symbol(&mut cdfs.kf_y_mode[above_ctx][left_ctx]);
    if trace {
        eprintln!("TRACE y_mode ctx=({above_ctx},{left_ctx}) value={mode} rng={}", dec.debug_state().0);
    }
    istep!("mode", mode as i32);
    let angle_delta_y = if (V_PRED..=D67_PRED).contains(&mode) {
        let delta = read_angle_delta(dec, &mut cdfs.angle_delta[mode - V_PRED]);
        if trace {
            eprintln!(
                "TRACE angle_delta value={}",
                delta + ANGLE_DELTA_ZERO as i32
            );
        }
        delta
    } else {
        0
    };
    istep!("angle_y", angle_delta_y);
    let uv_mode = if cfl {
        dec.symbol(&mut cdfs.uv_mode_cfl[mode])
    } else {
        dec.symbol(&mut cdfs.uv_mode_no_cfl[mode])
    };
    if trace {
        eprintln!("TRACE uv_mode cfl={cfl} y_mode={mode} value={uv_mode} rng={}", dec.debug_state().0);
    }
    istep!("uv_mode", uv_mode as i32);
    let alpha = if cfl && uv_mode == UV_CFL_PRED {
        Some(read_cfl_alphas(dec, cdfs))
    } else {
        None
    };
    // `SMOOTH_PRED..PAETH_PRED` (9..=12) chroma is a separate round-2 gap
    // from this lane's directional chroma: a corner block with neither
    // `above` nor `left` (both `Edges::build`'s no-neighbour fallback,
    // 127/129) fed `SMOOTH_PRED` produces the wrong pixel there (worst delta
    // 78 against ffmpeg, traced 2026-08-27) -- an existing, un-lane-touched
    // bug in that fallback this lane's own scope does not cover. Keep it
    // refused here rather than ship a silently-wrong decode.
    if (9..=12).contains(&uv_mode) {
        SMOOTH_UV_HITS.with(|c| c.set(c.get() + 1));
    }
    // `get_uv_mode` (spec 9.3): `UV_CFL_PRED` predicts as `DC_PRED` for the
    // angle-delta question -- libaom's own `read_intra_frame_mode_info` reads
    // `angle_delta_uv` off `get_uv_mode(uv_mode)`, never off `uv_mode` raw,
    // so a CFL block (uv_mode==13, outside `V_PRED..=D67_PRED`) already takes
    // the `else` branch below and never reads one either way.
    let angle_delta_uv = if (V_PRED..=D67_PRED).contains(&uv_mode) {
        DIRECTIONAL_UV_HITS.with(|c| c.set(c.get() + 1));
        read_angle_delta(dec, &mut cdfs.angle_delta[uv_mode - V_PRED])
    } else {
        0
    };
    istep!("angle_uv", angle_delta_uv);
    if angle_delta_uv != 0 {
        UV_ANGLE_DELTA_HITS.with(|c| c.set(c.get() + 1));
    }
    // `read_palette_mode_info` (spec 5.11.13, libaom decodemv.c:567, called
    // at :840 right after `xd->cfl.store_y` and before `read_filter_intra_mode_info`):
    // gated by `av1_allow_palette` (blockd.h -- size bound only,
    // [`palette_bsize_ctx`]). `palette_mode_ctx` comes from the caller
    // (`palette` param, `None` refuses unconditionally at ctx 0 -- the old
    // behaviour, bit-identical since a hardcoded ctx-0 read and a real one
    // read the same CDF row whenever every neighbour really is non-palette).
    // `palette_uv_mode_ctx` is this block's own just-decided Y palette use,
    // per `read_palette_mode_info`'s own `pmi->palette_size[0] > 0` (not a
    // neighbour lookup at all), so it is exact even at the excluded call
    // sites once Y decodes real syntax there too.
    //
    // lane-palette r7: libaom's `decode_color_map_tokens` is NOT called
    // inline from `read_palette_mode_info` -- it runs later, from
    // `parse_decode_block` (`decodeframe.c:1135`, `av1_visit_palette`),
    // after the WHOLE mode-info read for the block, including UV palette
    // mode_info (`read_palette_mode_info`'s own second half, right below)
    // and `read_filter_intra_mode_info`. So this branch now only reads
    // mode/size/colours for both planes (deferring the two colour-index-map
    // decodes to after `filter_intra`, below) -- r6's range trace pinned
    // this exact ordering bug (`compare-range-not-tell`: ranges matched
    // bit-for-bit through the Y colours, diverged only at the old inline
    // `decode_color_index_map` call).
    let mut palette_y_pending: Option<(usize, [u16; 8])> = None;
    let mut palette_uv_pending: Option<(usize, [u16; 8], [u16; 8])> = None;
    if allow_screen_content_tools
        && let Some(bsize_ctx) = palette_bsize_ctx(side)
    {
        let (mode_ctx, cache) = palette.unwrap_or((0, &[]));
        if mode == DC_PRED {
            if trace {
                let (rng, _) = dec.debug_state();
                eprintln!(
                    "TRACE pre_palette_y_mode bsize_ctx={bsize_ctx} mode_ctx={mode_ctx} rng={rng}"
                );
            }
        }
        let use_palette_y =
            mode == DC_PRED && dec.symbol(&mut cdfs.palette_y_mode[bsize_ctx][mode_ctx]) != 0;
        if mode == DC_PRED && trace {
            let (rng, _) = dec.debug_state();
            eprintln!("TRACE palette_y_mode value={} rng={rng}", use_palette_y as i32);
        }
        if use_palette_y {
            if palette.is_none() {
                return Err(unsupported(
                    "a block that actually uses a palette (Y) -- reconstruction is out of scope",
                ));
            }
            let n = 2 + dec.symbol(&mut cdfs.palette_y_size[bsize_ctx]);
            if trace {
                let (rng, _) = dec.debug_state();
                eprintln!("TRACE palette_y_size n={n} rng={rng}");
            }
            let colors = read_palette_colors_y(dec, n, cache);
            if trace {
                let (rng, _) = dec.debug_state();
                eprintln!("TRACE palette_y_colors colors={:?} rng={rng}", &colors[..n]);
            }
            palette_y_pending = Some((n, colors));
        }
        // `read_palette_mode_info`'s UV half (decodemv.c:588): read
        // unconditionally at this bit position whenever `uv_mode ==
        // UV_DC_PRED`, regardless of whether Y just fired -- this decoder's
        // scope always reconstructs chroma alongside luma (`decode_block`
        // always calls `u.reconstruct`/`v.reconstruct`), so `is_chroma_ref`
        // is always true here and does not need threading in.
        let palette_uv_mode_ctx = usize::from(use_palette_y);
        if uv_mode == DC_PRED && dec.symbol(&mut cdfs.palette_uv_mode[palette_uv_mode_ctx]) != 0 {
            let n = 2 + dec.symbol(&mut cdfs.palette_uv_size[bsize_ctx]);
            let (u_colors, v_colors) = read_palette_colors_uv(dec, n, palette_uv_cache);
            palette_uv_pending = Some((n, u_colors, v_colors));
        }
    }
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
                "TRACE use_filter_intra side={side} rng={} value={}", dec.debug_state().0,
                use_filter_intra as i32
            );
        }
        istep!("use_filter_intra", use_filter_intra as i32);
        if use_filter_intra {
            FILTER_INTRA_HITS.with(|c| c.set(c.get() + 1));
            let fi_mode = dec.symbol(&mut cdfs.filter_intra_mode);
            if trace {
                eprintln!("TRACE filter_intra_mode value={fi_mode}");
            }
            istep!("filter_intra_mode", fi_mode as i32);
            filter_intra = Some(fi_mode);
        }
    }
    // `av1_visit_palette` (decoder.c:234): plane 0 (Y) then plane 1 (chroma,
    // shared U/V map) -- the same order libaom's own loop visits them in.
    let palette_y = palette_y_pending.map(|(n, colors)| {
        let map = decode_color_index_map(dec, cdfs, n, side, false);
        PALETTE_HITS.with(|c| c.set(c.get() + 1));
        if trace {
            eprintln!("TRACE palette_y size={n} colors={:?}", &colors[..n]);
        }
        PaletteY { size: n, colors, map }
    });
    let palette_uv = palette_uv_pending.map(|(n, u_colors, v_colors)| {
        let map = decode_color_index_map(dec, cdfs, n, side / 2, true);
        PALETTE_UV_HITS.with(|c| c.set(c.get() + 1));
        if trace {
            eprintln!(
                "TRACE palette_uv size={n} u_colors={:?} v_colors={:?}",
                &u_colors[..n],
                &v_colors[..n]
            );
        }
        PaletteUv { size: n, u_colors, v_colors, map }
    });
    Ok((
        skip,
        mode,
        angle_delta_y,
        uv_mode,
        angle_delta_uv,
        alpha,
        filter_intra,
        palette_y,
        palette_uv,
    ))
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
    CFL_BLOCK_HITS.with(|c| c.set(c.get() + 1));
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
/// lane-tiny r4: raw padded-plane dump for filter-stage bisection, written
/// only when `var` names a path. Same byte shape as `EC_AV1_PREFILT_DUMP`.
fn dump_stage(var: &str, y: &PlaneBuf, u: &PlaneBuf, v: &PlaneBuf) {
    use std::io::Write;
    if let Ok(path) = std::env::var(var)
        && let Ok(mut f) = std::fs::File::create(format!("{path}.f0"))
    {
        for p in [y, u, v] {
            let narrow: Vec<u8> = p.data.iter().map(|&s| s as u8).collect();
            let _ = f.write_all(&narrow);
        }
    }
}

#[derive(Clone)]
struct PlaneBuf {
    data: Vec<u16>,
    width: usize,
    height: usize,
    /// The frame's true, decodable extent in this plane's own units — past
    /// this, samples are the padded coding surface's invented tail, never a
    /// real decoder's edge or reach reads.
    true_width: usize,
    true_height: usize,
    /// This plane's own tile's top-left pixel origin (spec: intra prediction
    /// never reaches across a tile boundary even though an earlier-decoded
    /// tile's real pixels sit right there in the shared buffer) — set via
    /// [`Self::set_tile_origin`] before each tile's own superblock walk,
    /// `0` for the single-tile case. lane-tiles r2: only the left/top bound
    /// is threaded (the two-tile column-split stage 1 fixture's tile always
    /// ends at the frame's own right/bottom edge); a non-last tile's own
    /// right/bottom reach bound is still open, tracked in the report.
    tile_x0: usize,
    tile_y0: usize,
    /// lane-tiles r5: this plane's own tile's right/bottom pixel bound
    /// (exclusive) -- a non-last tile column/row's own reach must stop at
    /// its own SB-aligned tile boundary. `width`/`height` (this plane's
    /// full padded extent) for the single-tile/last-tile case, matching the
    /// old unclipped behaviour exactly.
    tile_x1: usize,
    tile_y1: usize,
}

/// One entry per `MV_REFERENCE_FRAME` (index 0/1 unused -- `NONE`/
/// `INTRA_FRAME`/`LAST_FRAME` all resolve elsewhere), `Some` when this
/// frame's own `ref_frame_idx` names a DPB slot that has a picture in it
/// (lane-av1refs: the same "empty slot" case `GOLDEN_FRAME` already
/// refused by name, now generic across `LAST2`/`LAST3`/`GOLDEN`/`BWDREF`/
/// `ALTREF2`/`ALTREF`).
type RefSlots<'a> = [Option<(&'a PlaneBuf, &'a PlaneBuf, &'a PlaneBuf)>; 8];

impl PlaneBuf {
    /// Sets this plane's own tile pixel origin ([`Self::tile_x0`]/
    /// [`Self::tile_y0`]) before that tile's own superblock walk. `x0`/`y0`
    /// are already in this plane's own units (luma pixels for `y`, chroma
    /// pixels — halved — for `u`/`v`).
    fn set_tile_origin(&mut self, x0: usize, y0: usize, x1: usize, y1: usize) {
        self.tile_x0 = x0;
        self.tile_y0 = y0;
        self.tile_x1 = x1;
        self.tile_y1 = y1;
    }

    fn edges(
        &self,
        x: usize,
        y: usize,
        side: usize,
        reach: Reach,
    ) -> (Option<Vec<u16>>, Option<Vec<u16>>, Option<u16>) {
        let own_across = x + side.min(self.true_width.saturating_sub(x));
        let across = if reach.above_right {
            own_across + side.min(self.true_width.saturating_sub(own_across))
        } else {
            own_across
        }
        .min(self.width)
        .min(self.tile_x1);
        let own_down = y + side.min(self.true_height.saturating_sub(y));
        let down = if reach.below_left {
            own_down + side.min(self.true_height.saturating_sub(own_down))
        } else {
            own_down
        }
        .min(self.height)
        .min(self.tile_y1);
        let above = (y > self.tile_y0 && across > x)
            .then(|| self.data[(y - 1) * self.width + x..][..across - x].to_vec());
        let left = (x > self.tile_x0 && down > y).then(|| {
            (y..down)
                .map(|row| self.data[row * self.width + x - 1])
                .collect::<Vec<_>>()
        });
        let corner =
            (x > self.tile_x0 && y > self.tile_y0).then(|| self.data[(y - 1) * self.width + x - 1]);
        (above, left, corner)
    }

    /// [`Self::edges`] for a true `bw`x`bh` block (lane-intradisp r1):
    /// the above/left reach each extend by their OWN axis (`bw` across,
    /// `bh` down), not one shared `side`.
    fn edges_rect(
        &self,
        x: usize,
        y: usize,
        bw: usize,
        bh: usize,
        reach: Reach,
    ) -> (Option<Vec<u16>>, Option<Vec<u16>>, Option<u16>) {
        let own_across = x + bw.min(self.true_width.saturating_sub(x));
        let across = if reach.above_right {
            own_across + bw.min(self.true_width.saturating_sub(own_across))
        } else {
            own_across
        }
        .min(self.width)
        .min(self.tile_x1);
        let own_down = y + bh.min(self.true_height.saturating_sub(y));
        let down = if reach.below_left {
            own_down + bh.min(self.true_height.saturating_sub(own_down))
        } else {
            own_down
        }
        .min(self.height)
        .min(self.tile_y1);
        let above = (y > self.tile_y0 && across > x)
            .then(|| self.data[(y - 1) * self.width + x..][..across - x].to_vec());
        let left = (x > self.tile_x0 && down > y).then(|| {
            (y..down)
                .map(|row| self.data[row * self.width + x - 1])
                .collect::<Vec<_>>()
        });
        let corner =
            (x > self.tile_x0 && y > self.tile_y0).then(|| self.data[(y - 1) * self.width + x - 1]);
        if matches!((bw, bh), (16, 8) | (8, 16)) {
            RECT_STRIP_PRED_HITS.with(|h| h.set(h.get() + 1));
            if across > own_across || down > own_down {
                RECT_STRIP_REACH_HITS.with(|h| h.set(h.get() + 1));
            }
        }
        (above, left, corner)
    }

    /// [`Self::reconstruct`] for a true `bw`x`bh` block (lane-intradisp r1),
    /// via [`Self::edges_rect`] and [`crate::intra::predict`]'s own
    /// already-rect-capable `(bw, bh)` pair.
    #[allow(clippy::too_many_arguments)]
    fn reconstruct_rect(
        &mut self,
        x: usize,
        y: usize,
        bw: usize,
        bh: usize,
        mode: usize,
        angle_delta: i32,
        reach: Reach,
        residual: &[i32],
        cfl: Option<(i32, &[i32])>,
        filter_intra: Option<usize>,
        smooth_neighbor: bool,
    ) {
        let (above, left, corner) = self.edges_rect(x, y, bw, bh, reach);
        if std::env::var_os("EC_DEBUG_EDGES").is_some() {
            eprintln!(
                "EDGES x={x} y={y} bw={bw} bh={bh} mode={mode} above={:?} left_len={:?} left0={:?} corner={corner:?} tx0={} ty0={} truew={} trueh={}",
                above.as_ref().map(|a| (a.len(), a[0])),
                left.as_ref().map(|l| l.len()),
                left.as_ref().map(|l| l[0]),
                self.tile_x0, self.tile_y0, self.true_width, self.true_height
            );
        }
        let mut prediction = vec![0u16; bw * bh];
        if let Some(fi_mode) = filter_intra {
            crate::intra::predict_filter_intra(
                fi_mode,
                above.as_deref(),
                left.as_deref(),
                corner,
                bw,
                bh,
                &mut prediction,
            );
        } else {
            let enable_edge_filter = ENABLE_EDGE_FILTER.with(std::cell::Cell::get);
            predict(
                mode as u8,
                angle_delta,
                above.as_deref(),
                left.as_deref(),
                corner,
                bw,
                bh,
                enable_edge_filter,
                smooth_neighbor,
                &mut prediction,
            );
        }
        for row in 0..bh {
            for col in 0..bw {
                let idx = row * bw + col;
                let mut base = i32::from(prediction[idx]);
                if let Some((alpha_q3, ac_q3)) = cfl {
                    base = (base + cfl_scaled(alpha_q3, ac_q3[idx])).clamp(0, sample_max());
                }
                let sample = (base + residual[idx]).clamp(0, sample_max()) as u16;
                self.data[(y + row) * self.width + x + col] = sample;
            }
        }
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
        angle_delta: i32,
        reach: Reach,
        residual: &[i32],
        cfl: Option<(i32, &[i32])>,
        filter_intra: Option<usize>,
        // `smooth_neighbor` (spec `get_intra_edge_filter_type` /
        // libaom `get_filt_type`): true when the block's above OR left
        // neighbour's mode is `SMOOTH_PRED..=SMOOTH_H_PRED` (never `PAETH`).
        // Callers pass their own plane's neighbour-mode check
        // ([`is_smooth_mode`]) — chroma's is still hardcoded `false` by its
        // callers until a smooth/paeth `uv_mode` can reach this call at all
        // (still refused in `read_intra_mode`/`read_intra_mode_rect`), which
        // keeps `false` exact for chroma without this fn needing to know why.
        smooth_neighbor: bool,
    ) {
        // A palette block's own [`PALETTE_PRED`] override (lane-palette r2)
        // takes priority over `predict()`/`predict_filter_intra` entirely --
        // palette prediction needs no edge pixels at all.
        let prediction = if let Some(buf) = PALETTE_PRED.with(|c| c.borrow_mut().take()) {
            buf
        } else {
            let (above, left, corner) = self.edges(x, y, side, reach);
            let mut prediction = vec![0u16; side * side];
            if let Some(fi_mode) = filter_intra {
                crate::intra::predict_filter_intra(
                    fi_mode,
                    above.as_deref(),
                    left.as_deref(),
                    corner,
                    side,
                    side,
                    &mut prediction,
                );
            } else {
                // `enable_intra_edge_filter` is the sequence header's own flag
                // (`ENABLE_EDGE_FILTER`, set once per `decode_key_frame_tile_with_cdfs`
                // call, never per-block) -- threading it through every
                // `read_plane`/`decode_block`/`decode_leaf8` call would touch two
                // dozen sites for a value that never changes mid-decode, so it
                // rides a thread-local instead (this decoder is single-threaded
                // per stream).
                let enable_edge_filter = ENABLE_EDGE_FILTER.with(std::cell::Cell::get);
                predict(
                    mode as u8,
                    angle_delta,
                    above.as_deref(),
                    left.as_deref(),
                    corner,
                    side,
                    side,
                    enable_edge_filter,
                    smooth_neighbor,
                    &mut prediction,
                );
            }
            prediction
        };
        for row in 0..side {
            for col in 0..side {
                let idx = row * side + col;
                let mut base = i32::from(prediction[idx]);
                if let Some((alpha_q3, ac_q3)) = cfl {
                    base = (base + cfl_scaled(alpha_q3, ac_q3[idx])).clamp(0, sample_max());
                }
                let sample = (base + residual[idx]).clamp(0, sample_max()) as u16;
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
    angle_delta: i32,
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
    // See [`PlaneBuf::reconstruct`]'s own doc: caller's neighbour-smooth check.
    smooth_neighbor: bool,
) -> Result<Vec<i32>> {
    let skip_ctx = if plane_idx == 0 {
        luma_skip_ctx.unwrap_or(0)
    } else {
        usize::from(around.0) + usize::from(around.1)
    };
    // spec 5.11.47/libaom `av1_read_tx_type`: a filter-intra luma block's
    // `intra_ext_tx_cdf` row is indexed by `fimode_to_intradir[filter_intra_mode]`,
    // not by the block's ordinary `mode` (which is always DC_PRED whenever
    // filter-intra is on) -- caller passes the raw filter-intra mode 0..4 in
    // `filter_intra`, only meaningful for `plane_idx == 0` (chroma has no
    // filter-intra in AV1).
    const FIMODE_TO_INTRADIR: [usize; 5] = [DC_PRED, V_PRED, H_PRED, D157_PRED, DC_PRED];
    let tx_mode = if plane_idx == 0 {
        filter_intra.map_or(tx_mode, |fi| FIMODE_TO_INTRADIR[fi])
    } else {
        tx_mode
    };
    let mut coding = cdfs.txb(set, tx_mode);
    // Luma's `default_tx_type` is only a fallback for the sizes whose
    // `TxbSet` carries no symbol at all (32-point and up), which the spec
    // fixes at `DCT_DCT` regardless of mode; a chroma plane's `tx_type` is
    // *never* its own coded symbol (`coding.tx_type` is always `None` for
    // every `TxbSet::Chroma*`), so it would take `Intra_Mode_To_Tx_Type` of
    // the plane's own predicted mode (`default_intra_tx_type`) -- except
    // `av1_get_ext_tx_set_type` (`blockd.h`) resolves chroma at `tx_size_sqr
    // >= TX_32X32` to `EXT_TX_SET_DCTONLY`, which forces the result back to
    // `DCT_DCT` no matter what the mode-indexed table said (`TxbSet::Chroma8`
    // and `Chroma16` never hit this — every value `default_intra_tx_type` can
    // produce is already a member of both `TX_SET_INTRA_1`/`_2`, so nothing
    // narrows there — only `Chroma32` and up do).
    let default_tx_type = if plane_idx == 0 || side >= 32 {
        TxType::DctDct
    } else {
        default_intra_tx_type(predict_mode as u8)
    };
    let (grid, tx_type) = read_coeffs(
        dec,
        &mut coding,
        scan,
        skip_ctx,
        dc_sign_ctx(around.2),
        default_tx_type,
        None,
    )?;
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
    let (dc_delta, ac_delta) = plane_q_delta(plane_idx);
    let residual = dequant_and_inverse_typed(&levels, side, bit_depth(), block_q_idx(), dc_delta, ac_delta, tx_type);
    if std::env::var_os("EC_AV1_TRACE").is_some() {
        eprintln!(
            "TRACE dequant plane={plane_idx} base_q_idx={base_q_idx} tx_type={tx_type:?} side={side} levels={levels:?} residual={residual:?}",
        );
    }
    plane.reconstruct(
        x,
        y,
        side,
        predict_mode,
        angle_delta,
        reach,
        &residual,
        cfl,
        filter_intra,
        smooth_neighbor,
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
    allow_screen_content_tools: bool,
    allow_intrabc: bool,
    scan32: &[u16],
    scan16: &[u16],
    scan8: &[u16],
    scan4: &[u16],
    tx_select: bool,
    reduced_tx_set: bool,
) -> Result<()> {
    let (r, c) = at;
    let (px, py) = (c * SUB, r * SUB);
    let (palette_ctx, palette_cache) = neighbours.palette_ctx_and_cache(at);
    let palette_uv_cache = neighbours.palette_uv_cache(at);
    let (nb_above_mode, nb_left_mode) = neighbours.modes_above_left(r, c);
    let (skip, mode, angle_delta_y, uv_mode, angle_delta_uv, alpha, filter_intra, palette_y, palette_uv) =
        read_intra_mode(
            dec,
            cdfs,
            nb_above_mode,
            nb_left_mode,
            cfl,
            side,
            enable_filter_intra,
            neighbours.skip_txfm_ctx(r * (SUB / MI), c * (SUB / MI)),
            allow_screen_content_tools,
            allow_intrabc,
            Some((palette_ctx, &palette_cache)),
            &palette_uv_cache,
            r * (SUB / MI),
            c * (SUB / MI),
        )?;
    let intrabc_dv = INTRABC_DV.with(|c| c.take());
    // libaom `parse_decode_block`: an intrabc block is `inter_block_tx`, so
    // under `TX_MODE_SELECT` its transform size comes from the inter var-tx
    // partition tree (`read_var_tx_size`), a syntax element this decoder has
    // nowhere -- reading the intra `tx_depth` symbol instead would desync.
    if intrabc_dv.is_some() && tx_select && !skip {
        return Err(unsupported(
            "an intrabc block under TxMode::Select (its transform size is coded by the inter var-tx partition tree, which this decoder never reads)",
        ));
    }
    // `predict` panics outside `DC_PRED..=PAETH_PRED` (0..=12); `UV_CFL_PRED`
    // (13) predicts as `DC_PRED` with the [`cfl_scaled`] nudge carrying the
    // actual chroma-from-luma correlation, same as this decoder always did.
    let uv_predict_mode = if uv_mode == UV_CFL_PRED {
        DC_PRED
    } else {
        uv_mode
    };
    let smooth_neighbor = is_smooth_mode(nb_above_mode) || is_smooth_mode(nb_left_mode);
    if smooth_neighbor {
        SMOOTH_LUMA_HITS.with(|c| c.set(c.get() + 1));
    }
    let smooth_neighbor_uv =
        neighbours.smooth_uv_neighbour(r * (SUB / MI), c * (SUB / MI), r, c);
    if std::env::var_os("EC_AV1_TRACE").is_some() {
        eprintln!(
            "TRACE block px={px} py={py} side={side} mode={mode} uv_mode={uv_mode} \
             angle_delta_uv={angle_delta_uv}"
        );
    }
    let (mi_r, mi_c) = (r * (SUB / MI), c * (SUB / MI));
    // `TxMode::Select`'s `tx_depth` (spec 5.11.16): read for every intra
    // block, skipped or not. `logical_tx` is what the loop filter's edge
    // lookup and the next block's own `tx_size_context` want (the *nominal*
    // transform size -- 64 even for the 64x64 corner-scanned case, spec
    // 5.11.40); `coeff_tx_side` is the actual coded coefficient grid's side,
    // which only differs from it at that one corner case.
    let (logical_tx, coeff_tx_side) = if tx_select {
        let resolved = read_tx_size(dec, cdfs, neighbours, (mi_r, mi_c), side, None);
        (resolved, if resolved == 64 { 32 } else { resolved })
    } else {
        (side, luma_tx)
    };
    if tx_select && logical_tx < side && uv_mode != DC_PRED {
        SQ_CHROMA_TX_HITS.with(|c| c.set(c.get() + 1));
    }
    let luma_set = if tx_select {
        txbset_for(logical_tx, reduced_tx_set)
    } else {
        luma_set
    };
    let luma_set = if intrabc_dv.is_some() {
        txbset_for_inter(logical_tx, reduced_tx_set)
    } else {
        luma_set
    };
    let luma_scan = scan_for(coeff_tx_side, scan32, scan16, scan8, scan4);
    let reach = Reach::of(side, px, py, y.width, y.height);
    let (cpx, cpy) = (px / 2, py / 2);
    let chroma_side = side / 2;
    // spec 7.11.3.4 with `use_intrabc`: the reference is the *current*
    // frame's own pre-loop-filter reconstruction (all loop filters are off
    // whenever `allow_intrabc`, frame.rs:194/210/252/272) at the full-pel
    // block vector, with the BILINEAR kernel libaom forces for intrabc --
    // luma is a straight copy, chroma lands half-pel whenever the luma DV is
    // odd, which is exactly what the bilinear taps then interpolate.
    let intrabc_bufs = intrabc_dv.map(|(dv_row, dv_col)| {
        let mut yb = vec![0u16; side * side];
        mc::predict_with_filter(
            &y.data, y.width, y.true_width, y.true_height,
            mv_to_q4(px, dv_col, true), mv_to_q4(py, dv_row, true),
            side, side, mc::InterpFilterKind::Bilinear, &mut yb,
        );
        let mut ub = vec![0u16; chroma_side * chroma_side];
        mc::predict_with_filter(
            &u.data, u.width, u.true_width, u.true_height,
            mv_to_q4(cpx, dv_col, false), mv_to_q4(cpy, dv_row, false),
            chroma_side, chroma_side, mc::InterpFilterKind::Bilinear, &mut ub,
        );
        let mut vb = vec![0u16; chroma_side * chroma_side];
        mc::predict_with_filter(
            &v.data, v.width, v.true_width, v.true_height,
            mv_to_q4(cpx, dv_col, false), mv_to_q4(cpy, dv_row, false),
            chroma_side, chroma_side, mc::InterpFilterKind::Bilinear, &mut vb,
        );
        (yb, ub, vb)
    });
    if let Some((yb, _, _)) = &intrabc_bufs {
        PALETTE_PRED.with(|c| *c.borrow_mut() = Some(yb.clone()));
    }
    // A palette-Y block's own prediction is the reconstructed colour-index
    // map, not `predict()`'s edge-based one -- [`PALETTE_PRED`] carries it
    // to [`PlaneBuf::reconstruct`]'s next call rather than threading a
    // parameter through this function's dozen `y.reconstruct`/`read_plane`
    // sites. A split luma transform (`tx_select && logical_tx != side`)
    // would need the map sliced per-TU -- untested against a real
    // `--enable-palette=1` stream this round, so refuse by name rather than
    // guess at the slicing (charter's own named-acceptable fallback).
    if let Some(ref py) = palette_y {
        if tx_select && logical_tx != side {
            return Err(unsupported(
                "a palette block with a split luma transform (round 1)",
            ));
        }
        let buf: Vec<u16> = py
            .map
            .iter()
            .map(|&idx| py.colors[idx as usize])
            .collect();
        PALETTE_PRED.with(|c| *c.borrow_mut() = Some(buf));
    }
    // A UV-palette block's own chroma prediction override (lane-palette2 r1),
    // same [`PALETTE_PRED`] slot as Y above -- set fresh right before each
    // plane's own `reconstruct`/`read_plane` call below (never both planes at
    // once), since Y's own set-then-take already emptied the slot by the time
    // either of these run.
    let palette_uv_bufs: Option<(Vec<u16>, Vec<u16>)> = palette_uv.as_ref().map(|puv| {
        (
            puv.map.iter().map(|&idx| puv.u_colors[idx as usize]).collect(),
            puv.map.iter().map(|&idx| puv.v_colors[idx as usize]).collect(),
        )
    });
    // The chroma half of the intrabc prediction rides the same per-plane
    // override slot the UV-palette buffers do (set fresh before each plane's
    // own `reconstruct`/`read_plane` call below).
    let palette_uv_bufs = match &intrabc_bufs {
        Some((_, ub, vb)) => Some((ub.clone(), vb.clone())),
        None => palette_uv_bufs,
    };
    if skip {
        // A skipped block codes no residual syntax at all (spec 5.11.34):
        // straight prediction, on every plane.
        y.reconstruct(
            px,
            py,
            side,
            mode,
            angle_delta_y,
            reach,
            &vec![0i32; side * side],
            None,
            filter_intra,
            smooth_neighbor,
        );
        let ac = alpha.map(|_| cfl_ac_q3(y, px, py, side));
        if let Some((ub, _)) = &palette_uv_bufs {
            PALETTE_PRED.with(|c| *c.borrow_mut() = Some(ub.clone()));
        }
        u.reconstruct(
            cpx,
            cpy,
            chroma_side,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            &vec![0i32; chroma_side * chroma_side],
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
            None,
            smooth_neighbor_uv,
        );
        if let Some((_, vb)) = &palette_uv_bufs {
            PALETTE_PRED.with(|c| *c.borrow_mut() = Some(vb.clone()));
        }
        v.reconstruct(
            cpx,
            cpy,
            chroma_side,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            &vec![0i32; chroma_side * chroma_side],
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
            None,
            smooth_neighbor_uv,
        );
        let luma_grid = vec![0i32; side * side];
        let u_grid = vec![0i32; chroma_side * chroma_side];
        let v_grid = vec![0i32; chroma_side * chroma_side];
        neighbours.record(at, side, mode, uv_predict_mode, &[luma_grid, u_grid, v_grid]);
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
            angle_delta_y,
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
            smooth_neighbor,
        )?;
        let ac = alpha.map(|_| cfl_ac_q3(y, px, py, side));
        if let Some((ub, _)) = &palette_uv_bufs {
            PALETTE_PRED.with(|c| *c.borrow_mut() = Some(ub.clone()));
        }
        let u_grid = read_plane(
            dec,
            cdfs,
            chroma_set,
            scans.1,
            1,
            around[1],
            mode,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            u,
            cpx,
            cpy,
            chroma_side,
            chroma_tx,
            base_q_idx,
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
            None,
            None,
            smooth_neighbor_uv,
        )?;
        if std::env::var_os("EC_AV1_TRACE").is_some() {
            eprintln!(
                "TRACE u-write cpx={cpx} cpy={cpy} chroma_side={chroma_side} row0={:?}",
                &u.data[cpy * u.width + cpx..][..chroma_side]
            );
        }
        if let Some((_, vb)) = &palette_uv_bufs {
            PALETTE_PRED.with(|c| *c.borrow_mut() = Some(vb.clone()));
        }
        let v_grid = read_plane(
            dec,
            cdfs,
            chroma_set,
            scans.1,
            2,
            around[2],
            mode,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            v,
            cpx,
            cpy,
            chroma_side,
            chroma_tx,
            base_q_idx,
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
            None,
            None,
            smooth_neighbor_uv,
        )?;
        neighbours.record(at, side, mode, uv_predict_mode, &[luma_grid, u_grid, v_grid]);
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
                let tu_reach = tu_reach(
                    side,
                    side,
                    logical_tx,
                    tu_row * (logical_tx / MI),
                    tu_col * (logical_tx / MI),
                    px,
                    py,
                    y.width,
                    y.height,
                );
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
                    angle_delta_y,
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
                    smooth_neighbor,
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
        if let Some((ub, _)) = &palette_uv_bufs {
            PALETTE_PRED.with(|c| *c.borrow_mut() = Some(ub.clone()));
        }
        let u_grid = read_plane(
            dec,
            cdfs,
            chroma_set,
            scans.1,
            1,
            chroma_around[1],
            mode,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            u,
            cpx,
            cpy,
            chroma_side,
            chroma_tx,
            base_q_idx,
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
            None,
            None,
            smooth_neighbor_uv,
        )?;
        if let Some((_, vb)) = &palette_uv_bufs {
            PALETTE_PRED.with(|c| *c.borrow_mut() = Some(vb.clone()));
        }
        let v_grid = read_plane(
            dec,
            cdfs,
            chroma_set,
            scans.1,
            2,
            chroma_around[2],
            mode,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            v,
            cpx,
            cpy,
            chroma_side,
            chroma_tx,
            base_q_idx,
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
            None,
            None,
            smooth_neighbor_uv,
        )?;
        neighbours.record_split_luma(at, side, mode, uv_predict_mode, [&u_grid, &v_grid]);
    }
    // Unconditional, `size == 0` for a non-palette block: clears any stale
    // neighbour palette state the same way [`Neighbours::record`] always
    // overwrites `above_mode`/`left_mode` regardless of this block's own mode
    // (the split-luma-TU branch above always refuses before reaching here
    // when `palette_y` is `Some`, so it is never live for that branch).
    let (py_size, py_colors) = palette_y
        .as_ref()
        .map_or((0, [0u16; 8]), |p| (p.size, p.colors));
    neighbours.record_palette_y(at, side, py_size, py_colors);
    let (puv_size, puv_colors) = palette_uv
        .as_ref()
        .map_or((0, [0u16; 8]), |p| (p.size, p.u_colors));
    neighbours.record_palette_uv(at, side, puv_size, puv_colors);
    neighbours.fill_skip_grid((r * (SUB / MI), c * (SUB / MI)), side / MI, skip);
    neighbours.fill_lf_grid(
        (r * (SUB / MI), c * (SUB / MI)),
        side / MI,
        logical_tx as u8,
        0,
    );
    record_intrabc_mi(mi_r, mi_c, side / MI, intrabc_dv);
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
/// The coarse 16x16 `above_mode`/`left_mode` cells a four-way SPLIT of one
/// 16x16 parent leaves behind (lane-rectx r3). The next block DOWN reads
/// `above_mode[col]`, and its true above neighbour is the mi directly above
/// its own top-left corner -- the parent's BOTTOM-LEFT leaf, not the last one
/// decoded; the next block RIGHT reads `left_mode[row]`, whose true left
/// neighbour is the parent's TOP-RIGHT leaf. Writing the z-order last leaf
/// (bottom-right) into both, as this path did, is right only when all four
/// leaves happen to share a mode. `done` may be short at the true frame edge
/// (the straddle caller filters positions off the frame), so each side falls
/// back to the last leaf actually decoded.

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
    allow_screen_content_tools: bool,
    allow_intrabc: bool,
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
    // The mi-exact neighbour wins over both the coarse 16x16 slot and
    // `prev_leaf`: one 16x16 slot cannot hold the two different modes its
    // two 8x8 (or four 4x4) columns leave behind, so a leaf whose above/left
    // neighbour sits in a split cell read the wrong `kf_y_mode` row.
    if let Some(m) = neighbours.mode_above_mi(leaf_mi.0, leaf_mi.1) {
        above_mode = m;
    }
    if let Some(m) = neighbours.mode_left_mi(leaf_mi.0, leaf_mi.1) {
        left_mode = m;
    }
    let smooth_neighbor = is_smooth_mode(above_mode) || is_smooth_mode(left_mode);
    // Chroma's edge-filter type reads the CHROMA neighbour's `uv_mode`
    // (`get_intra_edge_filter_type`, reconintra.c:974), never the luma mode
    // this leaf's `smooth_neighbor` is built from -- passing `false` here
    // filtered a directional chroma block's edge at the wrong strength
    // (lane-sub8 r5: 4 pixels of one 8x8 leaf's chroma, deltas 1..3).
    // The `uv_mode` neighbours are mi-exact (`uv_mode_col`/`uv_mode_row`),
    // with the coarse [`SUB`] slot as the fallback when no block recorded
    // that exact mi cell.
    let smooth_neighbor_uv = neighbours.smooth_uv_neighbour(leaf_mi.0, leaf_mi.1, r, c);
    if smooth_neighbor {
        SMOOTH_LUMA_HITS.with(|c| c.set(c.get() + 1));
    }
    // An 8x8 leaf is well within `is_cfl_allowed`'s <=32x32 bound (spec
    // 5.11.5), so it reads the CFL-allowed `uv_mode_cfl` CDF, like every other
    // `decode_block` caller at 16x16 and up.
    let (skip, mode, angle_delta_y, uv_mode, angle_delta_uv, alpha, filter_intra, _palette_y, _palette_uv) =
        read_intra_mode(
            dec,
            cdfs,
            above_mode,
            left_mode,
            true,
            8,
            enable_filter_intra,
            neighbours.skip_txfm_ctx(leaf_mi.0, leaf_mi.1),
            allow_screen_content_tools,
            allow_intrabc,
            None,
            &[],
            leaf_mi.0,
            leaf_mi.1,
        )?;
    let uv_predict_mode = if uv_mode == UV_CFL_PRED {
        DC_PRED
    } else {
        uv_mode
    };
    neighbours.above_uv_mode[c] = uv_predict_mode;
    neighbours.left_uv_mode[r] = uv_predict_mode;
    neighbours.record_uv_mode_mi(leaf_mi.0, leaf_mi.1, 2, 2, uv_predict_mode);
    // `TxMode::Select`'s `tx_depth` at an 8x8 leaf (spec 5.11.16): the only
    // depths an 8x8 block offers are `TX8` (depth 0) and `TX4` (depth 1,
    // which splits the leaf's own 8x8 prediction into a 2x2 grid of 4x4
    // transform units, same raster-order per-TU pattern `decode_block`'s
    // multi-TU branch runs).
    let resolved = if tx_select {
        read_tx_size(dec, cdfs, neighbours, leaf_mi, 8, None)
    } else {
        8
    };
    let (px, py) = (leaf_mi.1 * MI, leaf_mi.0 * MI);
    let reach = Reach::of(8, px, py, y.width, y.height);
    let (cpx, cpy) = (px / 2, py / 2);
    let (luma_grid, u_grid, v_grid);
    if skip {
        y.reconstruct(
            px,
            py,
            8,
            mode,
            angle_delta_y,
            reach,
            &vec![0i32; 64],
            None,
            filter_intra,
            smooth_neighbor,
        );
        let ac = alpha.map(|_| cfl_ac_q3(y, px, py, 8));
        u.reconstruct(
            cpx,
            cpy,
            4,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            &vec![0i32; 16],
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
            None,
            smooth_neighbor_uv,
        );
        v.reconstruct(
            cpx,
            cpy,
            4,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            &vec![0i32; 16],
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
            None,
            smooth_neighbor_uv,
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
            angle_delta_y,
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
            smooth_neighbor,
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
            uv_predict_mode,
            angle_delta_uv,
            reach,
            u,
            cpx,
            cpy,
            4,
            TX4,
            base_q_idx,
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
            None,
            None,
            smooth_neighbor_uv,
        )?;
        v_grid = read_plane(
            dec,
            cdfs,
            TxbSet::Chroma4,
            scans.1,
            2,
            around[2],
            mode,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            v,
            cpx,
            cpy,
            4,
            TX4,
            base_q_idx,
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
            None,
            None,
            smooth_neighbor_uv,
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
                let tu_reach = tu_reach(8, 8, 4, tu_row, tu_col, px, py, y.width, y.height);
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
                    angle_delta_y,
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
                    smooth_neighbor,
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
            uv_predict_mode,
            angle_delta_uv,
            reach,
            u,
            cpx,
            cpy,
            4,
            TX4,
            base_q_idx,
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
            None,
            None,
            smooth_neighbor_uv,
        )?;
        v_grid = read_plane(
            dec,
            cdfs,
            TxbSet::Chroma4,
            scans.1,
            2,
            chroma_around[2],
            mode,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            v,
            cpx,
            cpy,
            4,
            TX4,
            base_q_idx,
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
            None,
            None,
            smooth_neighbor_uv,
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
        neighbours.fill_lf_grid(leaf_mi, 2, 4, 0);
        neighbours.record_mode_mi(leaf_mi.0, leaf_mi.1, 2, 2, mode);
        return Ok(mode);
    }
    neighbours.record_mi(leaf_mi, 8, &[luma_grid, u_grid, v_grid]);
    neighbours.fill_skip_grid(leaf_mi, 2, skip);
    neighbours.fill_lf_grid(leaf_mi, 2, 8, 0);
    neighbours.record_mode_mi(leaf_mi.0, leaf_mi.1, 2, 2, mode);
    Ok(mode)
}

/// `read_intra_frame_mode_info`'s mode-info read for one `BLOCK_4X4` leaf
/// below an 8x8 partition (spec `decode_partition`'s `bSize < BLOCK_8X8`
/// bottom, where no partition symbol is read at all -- `decode_block` runs
/// directly). No palette (`av1_allow_palette` needs both dims >= 8, spec
/// blockd.h) and no `read_tx_size` (a sub-8x8 block's transform is always
/// its own bsize, spec 5.11.16). `has_chroma` follows `is_chroma_reference`
/// (spec/libaom `av1_common_int.h`): for 4:2:0, only the bottom-right leaf
/// of the 2x2 4x4 group carries chroma syntax at all.
#[allow(clippy::too_many_arguments)]
fn read_intra_mode_sub8(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    above_mode: usize,
    left_mode: usize,
    has_chroma: bool,
    enable_filter_intra: bool,
    skip_ctx: usize,
    allow_intrabc: bool,
    mi_r: usize,
    mi_c: usize,
) -> Result<(bool, usize, i32, Option<(usize, i32, Option<(i32, i32)>)>, Option<usize>)> {
    let trace = std::env::var_os("EC_AV1_TRACE").is_some();
    let skip = dec.symbol(&mut cdfs.skip[skip_ctx]) != 0;
    if trace {
        eprintln!("TRACE sub8 skip mi=({mi_r},{mi_c}) ctx={skip_ctx} value={} rng={}", skip as i32, dec.debug_state().0);
    }
    maybe_read_cdef_idx(dec, mi_r, mi_c, skip);
    maybe_read_delta_q(dec, cdfs, mi_r, mi_c, false, skip);
    maybe_read_delta_lf(dec, cdfs, mi_r, mi_c, false, skip);
    if allow_intrabc {
        let use_intrabc = dec.symbol(&mut cdfs.intrabc) != 0;
        if use_intrabc {
            return Err(unsupported(
                "a sub-8x8 leaf that uses intrabc (this reader has no block-vector path; the 8x8-and-up reader reconstructs one)",
            ));
        }
    }
    let above_ctx = INTRA_MODE_CTX[above_mode];
    let left_ctx = INTRA_MODE_CTX[left_mode];
    let mode = dec.symbol(&mut cdfs.kf_y_mode[above_ctx][left_ctx]);
    if trace {
        eprintln!("TRACE sub8 y_mode mi=({mi_r},{mi_c}) ctx=({above_ctx},{left_ctx}) value={mode} rng={}", dec.debug_state().0);
    }
    // No `angle_delta` at all below 8x8: spec 5.11.6's `intra_angle_info_y`/
    // `_uv` are gated on `MiSize >= BLOCK_8X8` (libaom `av1_use_angle_delta`,
    // blockd.h), so a directional mode on a `BLOCK_4X4` leaf carries no delta
    // symbol. Reading one consumed a symbol the encoder never wrote and
    // desynced the tile at the very next element.
    let angle_delta_y = 0;
    let chroma = if has_chroma {
        let uv_mode = dec.symbol(&mut cdfs.uv_mode_cfl[mode]);
        if trace {
            eprintln!("TRACE sub8 uv_mode value={uv_mode} rng={}", dec.debug_state().0);
        }
        if (9..=12).contains(&uv_mode) {
            SMOOTH_UV_HITS.with(|c| c.set(c.get() + 1));
        }
        let alpha = if uv_mode == UV_CFL_PRED {
            Some(read_cfl_alphas(dec, cdfs))
        } else {
            None
        };
        if (V_PRED..=D67_PRED).contains(&uv_mode) {
            DIRECTIONAL_UV_HITS.with(|c| c.set(c.get() + 1));
        }
        // See the luma comment above: no angle delta below 8x8.
        let angle_delta_uv = 0;
        Some((uv_mode, angle_delta_uv, alpha))
    } else {
        None
    };
    let mut filter_intra = None;
    if mode == DC_PRED && enable_filter_intra {
        // `BLOCK_4X4`'s own row (class 0, `filter_intra_size_class(4)`) --
        // every sub-8x8 leaf here is a `BLOCK_4X4`.
        let use_filter_intra = dec.symbol(&mut cdfs.filter_intra[0]) != 0;
        if trace {
            eprintln!("TRACE sub8 use_filter_intra value={}", use_filter_intra as i32);
        }
        if use_filter_intra {
            FILTER_INTRA_HITS.with(|c| c.set(c.get() + 1));
            let fi_mode = dec.symbol(&mut cdfs.filter_intra_mode);
            filter_intra = Some(fi_mode);
        }
    }
    Ok((skip, mode, angle_delta_y, chroma, filter_intra))
}

/// Decodes a `PARTITION_SPLIT` of one 8x8 block into four `BLOCK_4X4` leaves
/// (spec `decode_partition`'s recursion bottom), sharing [`decode_leaf8`]'s
/// own signature so its caller's `prev_leaf`/`above_mode`/`left_mode`
/// bookkeeping needs no change. Each leaf reads its own luma mode info,
/// neighboured off the *previously decoded leaf in this same group* on
/// whichever axis is adjacent (mirroring [`decode_leaf8`]'s own `prev_leaf`
/// pattern, one level deeper); chroma is read and reconstructed exactly
/// once, on the last (bottom-right) leaf, at the whole 8x8 group's own 4x4
/// chroma unit -- [`read_intra_mode_sub8`]'s doc has the `is_chroma_reference`
/// derivation.
#[allow(clippy::too_many_arguments)]
fn decode_leaf_split4(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    outer_at: (usize, usize),
    leaf_mi: (usize, usize),
    scan4: &[u16],
    prev_leaf: Option<((usize, usize), usize)>,
    y: &mut PlaneBuf,
    u: &mut PlaneBuf,
    v: &mut PlaneBuf,
    base_q_idx: u8,
    enable_filter_intra: bool,
    allow_intrabc: bool,
    reduced_tx_set: bool,
    // Returns `(below_mode, right_mode)`: the mode a neighbour BELOW this 8x8
    // group sees (its bottom-left 4x4, libaom reads mi(row-1, col)) and the
    // one a neighbour to its RIGHT sees (its top-right 4x4, mi(row, col-1)).
) -> Result<(usize, usize)> {
    SUB8_SPLIT_HITS.with(|c| c.set(c.get() + 1));
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
    // See `decode_leaf8`'s own `smooth_neighbor_uv`: the chroma edge-filter
    // type is the chroma neighbours' `uv_mode`, mi-exact where recorded.
    let smooth_neighbor_uv = neighbours.smooth_uv_neighbour(leaf_mi.0, leaf_mi.1, r, c);
    let mut leaf_modes = [0usize; 4];
    let mut leaf_skips = [false; 4];
    let mut last_uv: Option<(usize, i32, Option<(i32, i32)>)> = None;
    for i in 0..4usize {
        let (dr, dc) = (i / 2, i % 2);
        let lmi = (leaf_mi.0 + dr, leaf_mi.1 + dc);
        // Neighbour modes at real mi granularity: inside the group the
        // sibling leaf, outside it the split-leaf map when the adjacent mi
        // was itself a 4x4 leaf, else the coarse 16x16 slot (see
        // `sub8_mode_col`'s doc).
        let leaf_above = if dr == 0 {
            neighbours.mode_above_mi(lmi.0, lmi.1).unwrap_or(above_mode)
        } else {
            leaf_modes[i - 2]
        };
        let leaf_left = if dc == 0 {
            neighbours.mode_left_mi(lmi.0, lmi.1).unwrap_or(left_mode)
        } else {
            leaf_modes[i - 1]
        };
        let has_chroma = i == 3;
        let skip_ctx = neighbours.skip_txfm_ctx(lmi.0, lmi.1);
        let (skip, mode, angle_delta_y, chroma, filter_intra) = read_intra_mode_sub8(
            dec, cdfs, leaf_above, leaf_left, has_chroma, enable_filter_intra, skip_ctx,
            allow_intrabc, lmi.0, lmi.1,
        )?;
        leaf_modes[i] = mode;
        leaf_skips[i] = skip;
        neighbours.record_mode_mi(lmi.0, lmi.1, 1, 1, mode);
        let smooth_neighbor = is_smooth_mode(leaf_above) || is_smooth_mode(leaf_left);
        if smooth_neighbor {
            SMOOTH_LUMA_HITS.with(|c| c.set(c.get() + 1));
        }
        let (px, py) = (lmi.1 * MI, lmi.0 * MI);
        let reach = Reach::of(4, px, py, y.width, y.height);
        if skip {
            y.reconstruct(px, py, 4, mode, angle_delta_y, reach, &vec![0i32; 16], None, filter_intra, smooth_neighbor);
            neighbours.record_mi_luma(lmi, 4, &vec![0i32; 16]);
        } else {
            let tu_around = neighbours.around_mi(lmi, 4)[0];
            // A `BLOCK_4X4` leaf's luma TU *is* the whole block, so libaom's
            // `get_txb_ctx` takes its `plane_bsize == txsize_to_bsize[tx_size]`
            // branch: `txb_skip_ctx = 0`, never the neighbour-magnitude table
            // (`txb_common.h:400`). Reading the table here decoded `all_zero`
            // off the wrong CDF on the very first leaf and desynced the tile.
            let tu_grid = read_plane(
                dec,
                cdfs,
                if reduced_tx_set { TxbSet::Luma4 } else { TxbSet::Luma4Set1 },
                scan4,
                0,
                tu_around,
                mode,
                mode,
                angle_delta_y,
                reach,
                y,
                px,
                py,
                4,
                4,
                base_q_idx,
                None,
                filter_intra,
                None,
                smooth_neighbor,
            )?;
            neighbours.record_mi_luma(lmi, 4, &tu_grid);
        }
        neighbours.fill_skip_grid(lmi, 1, skip);
        neighbours.fill_lf_grid(lmi, 1, 4, 0);
        if has_chroma {
            last_uv = chroma;
        }
    }
    let (gpx, gpy) = (leaf_mi.1 * MI, leaf_mi.0 * MI);
    let (cpx, cpy) = (gpx / 2, gpy / 2);
    let group_reach = Reach::of(8, gpx, gpy, y.width, y.height);
    let (uv_mode, angle_delta_uv, alpha) =
        last_uv.expect("i==3 always sets has_chroma, so chroma is always Some");
    let uv_predict_mode = if uv_mode == UV_CFL_PRED { DC_PRED } else { uv_mode };
    neighbours.above_uv_mode[c] = uv_predict_mode;
    neighbours.left_uv_mode[r] = uv_predict_mode;
    neighbours.record_uv_mode_mi(leaf_mi.0, leaf_mi.1, 2, 2, uv_predict_mode);
    let (u_grid, v_grid): (Vec<i32>, Vec<i32>) = if leaf_skips[3] {
        u.reconstruct(cpx, cpy, 4, uv_predict_mode, angle_delta_uv, group_reach, &vec![0i32; 16], None, None, smooth_neighbor_uv);
        v.reconstruct(cpx, cpy, 4, uv_predict_mode, angle_delta_uv, group_reach, &vec![0i32; 16], None, None, smooth_neighbor_uv);
        (vec![0i32; 16], vec![0i32; 16])
    } else {
        let ac = alpha.map(|_| cfl_ac_q3(y, gpx, gpy, 8));
        let chroma_around = neighbours.around_mi(leaf_mi, 8);
        let ug = read_plane(
            dec, cdfs, TxbSet::Chroma4, scan4, 1, chroma_around[1], uv_mode, uv_predict_mode,
            angle_delta_uv, group_reach, u, cpx, cpy, 4, TX4, base_q_idx,
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)), None, None, smooth_neighbor_uv,
        )?;
        let vg = read_plane(
            dec, cdfs, TxbSet::Chroma4, scan4, 2, chroma_around[2], uv_mode, uv_predict_mode,
            angle_delta_uv, group_reach, v, cpx, cpy, 4, TX4, base_q_idx,
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)), None, None, smooth_neighbor_uv,
        )?;
        (ug, vg)
    };
    // Chroma neighbour bookkeeping for the whole 8x8 group -- mirrors
    // [`decode_leaf8`]'s own tx4-chroma section (`record_split_luma`'s
    // rescaling is wrong for an already-mi-unit position, so this inlines
    // its tail directly at `leaf_mi`, side_mi=2), EXCEPT for the partition
    // context value itself. Unlike `decode_leaf8`'s TX4 branch (still one
    // `BLOCK_8X8`, `PARTITION_NONE`, transform depth only), this group is
    // genuinely four `BLOCK_4X4` blocks (`PARTITION_SPLIT` at the 8x8
    // level, libaom `decode_partition`'s `bsize2`/`subsize` recursion
    // bottom). libaom's `update_ext_partition_context` writes
    // `partition_context_lookup[subsize]` at the 8x8 slot -- `{30,30}` for
    // `BLOCK_8X8` (bit0=0, what `decode_leaf8` already reproduces via
    // side_mi=8) but `{31,31}` for `BLOCK_4X4` (bit0=1) here, and
    // `partition_ctx_mi`'s `left_side_mi[mi_r]*2 <= side` needs
    // `left_side_mi <= 4` to read that bit as 1. Leaving this at 8 (this
    // round's bug) makes the very next sibling 8x8's `partition_w8` read
    // with the wrong context and desync immediately.
    let side_mi = 2;
    for cell in 0..side_mi {
        neighbours.left_side_mi[leaf_mi.0 + cell] = 4;
        neighbours.above_side_mi[leaf_mi.1 + cell] = 4;
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
    Ok((leaf_modes[2], leaf_modes[1]))
}

/// Decodes a `PARTITION_VERT`/`PARTITION_HORZ` of one 8x8 block into two
/// `BLOCK_4X8`/`BLOCK_8X4` leaves (lane-tx4x8), the last partition shape
/// below 8x8 this decoder refused. Mirrors [`decode_leaf_split4`] element for
/// element -- same mode reads ([`read_intra_mode_sub8`]: no angle delta, no
/// tx-depth symbol below 8x8), same "chroma once, on the last sub-block, at
/// the whole 8x8 group's 4x4 unit" rule (`is_chroma_reference` is false for
/// every sub-block one mi wide or high but the last) -- differing only where
/// the shape does:
///
/// * the transform is a true `TX_4X8`/`TX_8X4`
///   ([`dequant_and_inverse_typed_wh`], whose `abs(rect_type) == 1` scale and
///   `av1_inv_txfm_shift_ls` row already cover this size pair);
/// * the CDF set is [`TxbSet::LumaRect4x8`] (`get_txsize_entropy_ctx` squares
///   up to `TX_8X8`) with a 32-position `eob_pt` and the `TX_4X4` `tx_type`
///   row -- see that variant's doc comment;
/// * the scan is [`SCAN_4X8`]/[`SCAN_8X4`], or their `mrow`/`mcol` siblings
///   ([`class_scan_table_wh`]) when the coded `tx_type` is `V_DCT`/`H_DCT`;
/// * `Reach` comes from libaom's own `has_tr_4x8`/`has_bl_8x4` rows
///   ([`Reach::of_rect`]);
/// * `update_partition_context` writes `partition_context_lookup[BLOCK_4X8]`
///   = `{31, 30}` (width 4 above, height 8 left), not the `{31, 31}` a
///   `BLOCK_4X4` split writes.
///
/// Filter intra on one of these leaves is refused by name: `predict_filter_intra`
/// is square-only in this decoder (the same refusal the 16x16-level rect
/// strips already carry).
#[allow(clippy::too_many_arguments)]
fn decode_leaf_rect8(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    outer_at: (usize, usize),
    leaf_mi: (usize, usize),
    vert: bool,
    scan4: &[u16],
    y: &mut PlaneBuf,
    u: &mut PlaneBuf,
    v: &mut PlaneBuf,
    base_q_idx: u8,
    enable_filter_intra: bool,
    allow_intrabc: bool,
    tx_select: bool,
    reduced_tx_set: bool,
) -> Result<(usize, usize)> {
    let (r, c) = outer_at;
    let above_mode = neighbours.above_mode[c];
    let left_mode = neighbours.left_mode[r];
    let smooth_neighbor_uv = neighbours.smooth_uv_neighbour(leaf_mi.0, leaf_mi.1, r, c);
    let (bw, bh) = if vert { (4, 8) } else { (8, 4) };
    let (w_mi, h_mi) = (bw / MI, bh / MI);
    let scan: &[u16] = if vert { &SCAN_4X8 } else { &SCAN_8X4 };
    let mut leaf_modes = [0usize; 2];
    let mut leaf_skips = [false; 2];
    let mut last_uv: Option<(usize, i32, Option<(i32, i32)>)> = None;
    for i in 0..2usize {
        let lmi = if vert {
            (leaf_mi.0, leaf_mi.1 + i)
        } else {
            (leaf_mi.0 + i, leaf_mi.1)
        };
        // Inside the pair the sibling is the neighbour on the split axis;
        // outside it the mi-granular map, else the coarse 16x16 slot.
        let leaf_above = if !vert && i == 1 {
            leaf_modes[0]
        } else {
            neighbours.mode_above_mi(lmi.0, lmi.1).unwrap_or(above_mode)
        };
        let leaf_left = if vert && i == 1 {
            leaf_modes[0]
        } else {
            neighbours.mode_left_mi(lmi.0, lmi.1).unwrap_or(left_mode)
        };
        let has_chroma = i == 1;
        let skip_ctx = neighbours.skip_txfm_ctx(lmi.0, lmi.1);
        let (skip, mode, angle_delta_y, chroma, filter_intra) = read_intra_mode_sub8(
            dec, cdfs, leaf_above, leaf_left, has_chroma, enable_filter_intra, skip_ctx,
            allow_intrabc, lmi.0, lmi.1,
        )?;
        if filter_intra.is_some() {
            return Err(unsupported(
                "filter intra on a HORZ/VERT strip (this decoder predicts square-only)",
            ));
        }
        // `TxMode::Select`'s `tx_depth` (spec 5.11.16) exists at a 4x8/8x4
        // leaf too, and is read for every intra block, skipped or not. Its
        // category is libaom's `bsize_to_tx_size_cat(BLOCK_4X8) == 0` --
        // `sub_tx_size_map[TX_4X8] == TX_4X4` is a single step, so it is the
        // same 2-symbol `tx_size_cat0` an 8x8 uses, not the 3-symbol
        // `tx_size_cat2` the 32x16 strips read -- with the rect context
        // (`get_tx_size_context`'s `max_tx_wide`/`max_tx_high` = bw/bh).
        // Missing this read consumed one symbol fewer than the encoder wrote
        // and desynced the tile at the very first rect leaf (class
        // `symbol-consumption-gap`).
        let depth = if tx_select {
            let ctx = tx_size_context_rect(neighbours, lmi, bw, bh);
            dec.symbol(&mut cdfs.tx_size_cat0[ctx])
        } else {
            0
        };
        if depth != 0 {
            TX_DEPTH_HITS.with(|c| c.set(c.get() + 1));
            RECT8_SPLIT_TX_HITS.with(|c| c.set(c.get() + 1));
        }
        leaf_modes[i] = mode;
        leaf_skips[i] = skip;
        neighbours.record_mode_mi(lmi.0, lmi.1, w_mi, h_mi, mode);
        let smooth_neighbor = is_smooth_mode(leaf_above) || is_smooth_mode(leaf_left);
        if smooth_neighbor {
            SMOOTH_LUMA_HITS.with(|c| c.set(c.get() + 1));
        }
        let (px, py) = (lmi.1 * MI, lmi.0 * MI);
        let reach = Reach::of_rect(bw, bh, px, py, y.width, y.height);
        // `sub_tx_size_map[TX_4X8] == TX_4X4`: depth 1 splits the leaf into
        // two 4x4 transform units along its long axis, each predicted from
        // the previous unit's reconstruction and reading its own `txb_skip`
        // context -- `decode_leaf8`'s own depth-1 loop, two units instead of
        // four. The per-TU path writes its own neighbour state, so the
        // block-level write below is skipped for it.
        let split = depth != 0;
        if split {
            let mut done = 0usize;
            for tu in 0..2usize {
                let tu_mi = if vert {
                    (lmi.0 + tu, lmi.1)
                } else {
                    (lmi.0, lmi.1 + tu)
                };
                let (tu_px, tu_py) = (tu_mi.1 * MI, tu_mi.0 * MI);
                let tu_around = neighbours.around_mi(tu_mi, 4)[0];
                let tu_reach = tu_reach(
                    bw,
                    bh,
                    4,
                    if vert { tu } else { 0 },
                    if vert { 0 } else { tu },
                    px,
                    py,
                    y.width,
                    y.height,
                );
                let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, 1);
                if skip {
                    // A skipped leaf still predicts per transform unit: the
                    // second 4x4 takes its above/left edge from the first
                    // one's reconstruction, which is NOT what a single 4x8
                    // prediction of the same mode produces.
                    let zeros = vec![0i32; 16];
                    y.reconstruct(
                        tu_px, tu_py, 4, mode, angle_delta_y, tu_reach, &zeros, None, None,
                        smooth_neighbor,
                    );
                    neighbours.record_mi_luma(tu_mi, 4, &zeros);
                    continue;
                }
                let tu_grid = read_plane(
                    dec,
                    cdfs,
                    if reduced_tx_set {
                        TxbSet::Luma4
                    } else {
                        TxbSet::Luma4Set1
                    },
                    scan4,
                    0,
                    tu_around,
                    mode,
                    mode,
                    angle_delta_y,
                    tu_reach,
                    y,
                    tu_px,
                    tu_py,
                    4,
                    TX4,
                    base_q_idx,
                    None,
                    None,
                    Some(tu_skip_ctx),
                    smooth_neighbor,
                )?;
                if tu_grid.iter().any(|&l| l != 0) {
                    done += 1;
                }
                neighbours.record_mi_luma(tu_mi, 4, &tu_grid);
            }
            if done > 0 {
                if vert {
                    TX4X8_CODED_HITS.with(|h| h.set(h.get() + 1));
                } else {
                    TX8X4_CODED_HITS.with(|h| h.set(h.get() + 1));
                }
            }
            neighbours.fill_skip_grid_rect(lmi, w_mi, h_mi, skip);
            neighbours.fill_lf_grid_rect(lmi, w_mi, h_mi, 4, 4, 0);
            if has_chroma {
                last_uv = chroma;
            }
            continue;
        }
        let grid = if skip {
            let zeros = vec![0i32; bw * bh];
            y.reconstruct_rect(
                px, py, bw, bh, mode, angle_delta_y, reach, &zeros, None, None, smooth_neighbor,
            );
            zeros
        } else {
            // `plane_bsize == txsize_to_bsize[tx_size]` here too (the leaf's
            // luma TU *is* the leaf), so `get_txb_ctx` fixes `txb_skip_ctx`
            // at 0 -- see `decode_leaf_split4`'s own note.
            let around = neighbours.around_mi_rect(lmi, bw, bh)[0];
            let set = if reduced_tx_set {
                TxbSet::LumaRect4x8
            } else {
                TxbSet::LumaRect4x8Set1
            };
            let mut coding = cdfs.txb(set, mode);
            let (levels, tx_type) = read_coeffs_rect(
                dec,
                &mut coding,
                scan,
                bw,
                bh,
                0,
                dc_sign_ctx(around.2),
                TxType::DctDct,
            )?;
            if levels.iter().any(|&l| l != 0) {
                if vert {
                    TX4X8_CODED_HITS.with(|h| h.set(h.get() + 1));
                } else {
                    TX8X4_CODED_HITS.with(|h| h.set(h.get() + 1));
                }
            }
            let residual = dequant_and_inverse_typed_wh(
                &levels,
                bw,
                bh,
                crate::decode::bit_depth(),
                CURRENT_Q_IDX.with(|q| q.get()),
                plane_q_delta(0).0,
                plane_q_delta(0).1,
                tx_type,
            );
            y.reconstruct_rect(
                px, py, bw, bh, mode, angle_delta_y, reach, &residual, None, None, smooth_neighbor,
            );
            levels
        };
        // Plane 0's per-TU coefficient context over this leaf's own mi span
        // (`record_mi_luma`'s rect twin -- the chroma planes are written once
        // for the whole 8x8 group below).
        let state = neighbour_state(&grid);
        for cell in 0..h_mi {
            if lmi.0 + cell < neighbours.mi_rows {
                neighbours.left[lmi.0 + cell][0] = state;
            }
        }
        for cell in 0..w_mi {
            if lmi.1 + cell < neighbours.mi_cols {
                neighbours.above[lmi.1 + cell][0] = state;
            }
        }
        neighbours.fill_skip_grid_rect(lmi, w_mi, h_mi, skip);
        let (tx_w, tx_h) = if split { (4, 4) } else { (bw as u8, bh as u8) };
        neighbours.fill_lf_grid_rect(lmi, w_mi, h_mi, tx_w, tx_h, 0);
        if has_chroma {
            last_uv = chroma;
        }
    }
    let (gpx, gpy) = (leaf_mi.1 * MI, leaf_mi.0 * MI);
    let (cpx, cpy) = (gpx / 2, gpy / 2);
    let group_reach = Reach::of(8, gpx, gpy, y.width, y.height);
    let (uv_mode, angle_delta_uv, alpha) =
        last_uv.expect("i==1 always sets has_chroma, so chroma is always Some");
    let uv_predict_mode = if uv_mode == UV_CFL_PRED { DC_PRED } else { uv_mode };
    neighbours.above_uv_mode[c] = uv_predict_mode;
    neighbours.left_uv_mode[r] = uv_predict_mode;
    neighbours.record_uv_mode_mi(leaf_mi.0, leaf_mi.1, 2, 2, uv_predict_mode);
    let (u_grid, v_grid): (Vec<i32>, Vec<i32>) = if leaf_skips[1] {
        u.reconstruct(cpx, cpy, 4, uv_predict_mode, angle_delta_uv, group_reach, &vec![0i32; 16], None, None, smooth_neighbor_uv);
        v.reconstruct(cpx, cpy, 4, uv_predict_mode, angle_delta_uv, group_reach, &vec![0i32; 16], None, None, smooth_neighbor_uv);
        (vec![0i32; 16], vec![0i32; 16])
    } else {
        let ac = alpha.map(|_| cfl_ac_q3(y, gpx, gpy, 8));
        let chroma_around = neighbours.around_mi(leaf_mi, 8);
        let ug = read_plane(
            dec, cdfs, TxbSet::Chroma4, scan4, 1, chroma_around[1], uv_mode, uv_predict_mode,
            angle_delta_uv, group_reach, u, cpx, cpy, 4, TX4, base_q_idx,
            alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)), None, None, smooth_neighbor_uv,
        )?;
        let vg = read_plane(
            dec, cdfs, TxbSet::Chroma4, scan4, 2, chroma_around[2], uv_mode, uv_predict_mode,
            angle_delta_uv, group_reach, v, cpx, cpy, 4, TX4, base_q_idx,
            alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)), None, None, smooth_neighbor_uv,
        )?;
        (ug, vg)
    };
    // `update_partition_context` at the 8x8 level writes
    // `partition_context_lookup[subsize]` over the whole 8x8 span: BLOCK_4X8
    // is `{31, 30}` (bit0 set on the above half only), BLOCK_8X4 `{30, 31}`
    // -- the mirror of `decode_leaf_split4`'s `{31, 31}`, which
    // `partition_ctx_mi` reads back off these side values.
    for cell in 0..2 {
        neighbours.above_side_mi[leaf_mi.1 + cell] = bw;
        neighbours.left_side_mi[leaf_mi.0 + cell] = bh;
    }
    let round_up_even = |n: usize| n.div_ceil(2) * 2;
    let bound_h = round_up_even(neighbours.mi_rows);
    let bound_w = round_up_even(neighbours.mi_cols);
    let u_state = neighbour_state(&u_grid);
    let v_state = neighbour_state(&v_grid);
    for cell in 0..2usize {
        if cell < 2usize.min(bound_h.saturating_sub(leaf_mi.0)) {
            neighbours.left[leaf_mi.0 + cell][1] = u_state;
            neighbours.left[leaf_mi.0 + cell][2] = v_state;
        }
        if cell < 2usize.min(bound_w.saturating_sub(leaf_mi.1)) {
            neighbours.above[leaf_mi.1 + cell][1] = u_state;
            neighbours.above[leaf_mi.1 + cell][2] = v_state;
        }
    }
    // `(below_mode, right_mode)`: the sub-block a neighbour below sees is the
    // bottom one (the second under HORZ, the left one under VERT), the one a
    // neighbour to the right sees is the top-right (the second under VERT).
    Ok(if vert {
        (leaf_modes[0], leaf_modes[1])
    } else {
        (leaf_modes[1], leaf_modes[0])
    })
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
fn cdef_find_dir(coeff_shift: i32, sample: impl Fn(usize, usize) -> i32) -> (usize, i32) {
    const DIV_TABLE: [i64; 9] = [0, 840, 420, 280, 210, 168, 140, 120, 105];
    let mut partial = [[0i64; 15]; 8];
    for i in 0..8i64 {
        for j in 0..8i64 {
            // libaom `cdef_find_dir_c`: normalise to an 8-bit sample before
            // accumulating, same as `constrain`'s threshold shift below.
            let x = i64::from(sample(i as usize, j as usize) >> coeff_shift) - 128;
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
    coeff_shift: i32,
) {
    let clipping_required = enable_primary && enable_secondary;
    // libaom `cdef_filter_block_internal`: `cdef_pri_taps[(pri_strength >>
    // coeff_shift) & 1]` -- `pri_strength` (and its `adjust_strength`
    // derivative) carries the bit-depth shift, so the tap-set parity bit
    // must be read back above that shift, not off the raw low bit.
    let pri_taps = CDEF_PRI_TAPS[((pri_strength >> coeff_shift) & 1) as usize];
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
            dst.data[(oy + i) * dst.width + (ox + j)] = y.clamp(0, sample_max()) as u16;
        }
    }
}

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
    frame_width: usize,
    frame_height: usize,
) {
    // lane-part32 r2 debug rung, env-gated: bisecting a widespread pixel
    // mismatch between the raw reconstruction and post-filter output.
    if std::env::var_os("EC_AV1_DEBUG_SKIP_DEBLOCK").is_some() {
        return;
    }
    if lf.level[0] != 0 || lf.level[1] != 0 {
        deblock_plane(y, 0, lf, n, frame_width, frame_height);
    }
    if lf.level[2] != 0 {
        deblock_plane(u, 1, lf, n, frame_width, frame_height);
    }
    if lf.level[3] != 0 {
        deblock_plane(v, 2, lf, n, frame_width, frame_height);
    }
}

/// Luma-mi-grid position (row or col) a plane-local pixel coordinate falls
/// in -- `Neighbours::tx_grid`/`ref_grid` are always indexed at the luma
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
    let grid = if chroma { &n.uv_tx_grid } else { &n.tx_grid };
    grid.get(mi_r * n.skip_grid_cols_mi + mi_c).copied().unwrap_or(0) as u32
}

/// [`tx_px_at`]'s HEIGHT twin (lane-rect r2): the deblocker's horizontal
/// edges step and gate by transform height, which differs from width only
/// on rect partition strips.
fn tx_h_px_at(n: &Neighbours, chroma: bool, mi_r: usize, mi_c: usize) -> u32 {
    let grid = if chroma { &n.uv_tx_h_grid } else { &n.tx_h_grid };
    grid.get(mi_r * n.skip_grid_cols_mi + mi_c).copied().unwrap_or(0) as u32
}

/// `get_tx_size_context` (spec 9.3's `TxDepthCtx`, libaom
/// `TXFM_CONTEXT`/`entropymode.c`): the sum of two boolean terms, each
/// present only when its neighbour exists inside the true frame -- whether
/// the block immediately above (at this block's own leftmost mi column) or
/// to the left (at its own topmost mi row) coded a transform at least as wide
/// as `own_side`.
fn tx_size_context(n: &Neighbours, (mi_r, mi_c): (usize, usize), own_side: usize) -> usize {
    let above = mi_r > n.tile_row0_mi && tx_px_at(n, false, mi_r - 1, mi_c) as usize >= own_side;
    let left = mi_c > n.tile_col0_mi && tx_h_px_at(n, false, mi_r, mi_c - 1) as usize >= own_side;
    usize::from(above) + usize::from(left)
}

/// `get_tx_size_context` as libaom actually writes it (`pred_common.h:342`),
/// for a block inside an INTER frame: the two `TXFM_CONTEXT` arrays
/// ([`Neighbours::above_txfm`]/[`Neighbours::left_txfm`], which only the
/// inter path maintains), except that an *inter* neighbour contributes its
/// own BLOCK size rather than its transform size -- the two differ the moment
/// a neighbour's var-tx tree splits. [`tx_size_context`]'s deblock-grid
/// approximation coincides with this only while every neighbour codes one
/// whole-block transform, which is exactly what `TxMode::Select` ends.
fn tx_size_context_txfm(n: &Neighbours, (mi_r, mi_c): (usize, usize), side: usize) -> usize {
    let has_above = mi_r > n.tile_row0_mi;
    let has_left = mi_c > n.tile_col0_mi;
    let mut above = usize::from(n.above_txfm[mi_c]) >= side;
    let mut left = usize::from(n.left_txfm[mi_r]) >= side;
    if has_above && n.above_inter[mi_c / (SUB / MI)] {
        above = n.above_side_mi[mi_c] >= side;
    }
    if has_left && n.left_inter[mi_r / (SUB / MI)] {
        left = n.left_side_mi[mi_r] >= side;
    }
    match (has_above, has_left) {
        (true, true) => usize::from(above) + usize::from(left),
        (true, false) => usize::from(above),
        (false, true) => usize::from(left),
        (false, false) => 0,
    }
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
    // lane-txselect: `get_tx_size_context`'s real context, for the callers
    // that have one -- see [`tx_size_context_txfm`]. `None` keeps the
    // key-frame path's own deblock-grid approximation.
    ctx_override: Option<usize>,
) -> usize {
    let ctx = ctx_override.unwrap_or_else(|| tx_size_context(n, at_mi, side));
    let depth = match side {
        8 => {
            dec.symbol(&mut cdfs.tx_size_cat0[ctx])
        }
        16 => {
            dec.symbol(&mut cdfs.tx_size_cat1[ctx])
        }
        32 => {
            dec.symbol(&mut cdfs.tx_size_cat2[ctx])
        }
        64 => {
            dec.symbol(&mut cdfs.tx_size_cat3[ctx])
        }
        _ => unreachable!("decode_block/decode_leaf8 only call this at 8/16/32/64"),
    };
    if std::env::var_os("EC_TRACE_MODE_STEP").is_some() {
        let (rng, _) = dec.debug_state();
        let (mi_r, mi_c) = at_mi;
        eprintln!(
            "EC_ISTEP mi_row={mi_r} mi_col={mi_c} name=tx_depth val={depth} ctx={ctx} rng={rng}"
        );
    }
    if depth != 0 {
        TX_DEPTH_HITS.with(|c| c.set(c.get() + 1));
    }
    side >> depth
}

/// `txfm_partition_context` (libaom `av1_common_int.h`): an inter block's
/// `txfm_split` context -- a category from the block's own largest square
/// transform and whether this unit already sits below it, times three, plus
/// the two neighbour terms (whether the transform last written above/left of
/// this unit is narrower/shorter than the unit itself).
fn txfm_partition_ctx(above_px: u8, left_px: u8, blk_max_px: usize, tx_px: usize) -> usize {
    let above = usize::from(usize::from(above_px) < tx_px);
    let left = usize::from(usize::from(left_px) < tx_px);
    // `get_sqr_tx_size(max(block_size_wide, block_size_high))`, capped at the
    // largest transform (`TX_64X64`); `TX_4X4`..`TX_64X64` index as
    // `log2(px) - 2`.
    let max_tx = blk_max_px.min(64);
    let max_idx = max_tx.trailing_zeros() as usize - 2;
    let category = usize::from(tx_px != max_tx && max_tx > 8) + (4 - max_idx) * 2;
    category * 3 + above + left
}

/// `txfm_partition_update` (libaom `av1_common_int.h`): once a var-tx unit
/// resolves, every mi cell its *parent* unit (`txb_px`) spans records the
/// resolved transform's own size.
fn txfm_partition_update(
    n: &mut Neighbours,
    (mi_r, mi_c): (usize, usize),
    tx_px: usize,
    txb_px: usize,
) {
    for i in 0..txb_px / MI {
        if let Some(cell) = n.left_txfm.get_mut(mi_r + i) {
            *cell = tx_px as u8;
        }
        if let Some(cell) = n.above_txfm.get_mut(mi_c + i) {
            *cell = tx_px as u8;
        }
    }
}

/// `set_txfm_ctxs` (libaom `av1_common_int.h`): the non-var-tx write of the
/// same contexts -- a skipped *inter* block records its own block size rather
/// than its transform size.
fn set_txfm_ctxs(
    n: &mut Neighbours,
    (mi_r, mi_c): (usize, usize),
    tx_px: usize,
    w_mi: usize,
    h_mi: usize,
    skip_inter: bool,
) {
    let bw = if skip_inter { w_mi * MI } else { tx_px };
    let bh = if skip_inter { h_mi * MI } else { tx_px };
    for i in 0..w_mi {
        if let Some(cell) = n.above_txfm.get_mut(mi_c + i) {
            *cell = bw as u8;
        }
    }
    for i in 0..h_mi {
        if let Some(cell) = n.left_txfm.get_mut(mi_r + i) {
            *cell = bh as u8;
        }
    }
}

/// The value libaom initialises both transform contexts to -- NOT zero:
/// `av1_zero_above_context` memsets the txfm row to
/// `tx_size_wide[TX_SIZES_LARGEST]` and `av1_zero_left_context` does the same
/// with `tx_size_high[TX_SIZES_LARGEST]` (`av1_common_int.h`), i.e. 64
/// pixels, so an unwritten neighbour reads as the *widest* possible
/// transform, not the narrowest.
const TXFM_CTX_INIT: u8 = 64;

/// libaom's `MAX_VARTX_DEPTH`: an inter block's transform tree halves at most
/// twice below its own largest transform.
const MAX_VARTX_DEPTH: usize = 2;

/// `read_var_tx_size` (spec 5.11.17, libaom `decodeframe.c`
/// `read_tx_size_vartx`): the recursive `txfm_split` tree of one
/// max-transform-sized unit of an inter block, collecting the resolved leaf
/// transform units into `out` as `(row offset, col offset, side)` in mi units
/// -- exactly the order `decode_reconstruct_tx` later reads their
/// coefficients in. `blk_row`/`blk_col` are mi offsets inside the block;
/// `max_w_mi`/`max_h_mi` clip it to the true frame edge (`max_block_wide`/
/// `max_block_high`).
#[allow(clippy::too_many_arguments)]
fn read_var_tx_size(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    n: &mut Neighbours,
    at_mi: (usize, usize),
    blk_max_px: usize,
    (max_w_mi, max_h_mi): (usize, usize),
    tx_px: usize,
    depth: usize,
    blk_row: usize,
    blk_col: usize,
    out: &mut Vec<(usize, usize, usize)>,
) {
    if blk_row >= max_h_mi || blk_col >= max_w_mi {
        return;
    }
    let unit = (at_mi.0 + blk_row, at_mi.1 + blk_col);
    if depth == MAX_VARTX_DEPTH {
        out.push((blk_row, blk_col, tx_px));
        txfm_partition_update(n, unit, tx_px, tx_px);
        return;
    }
    let ctx = txfm_partition_ctx(n.above_txfm[unit.1], n.left_txfm[unit.0], blk_max_px, tx_px);
    let split = dec.symbol(&mut cdfs.txfm_partition[ctx]) == 1;
    TXFM_SPLIT_READS.with(|c| c.set(c.get() + 1));
    if std::env::var_os("EC_TRACE_MODE_STEP").is_some() {
        let (rng, _) = dec.debug_state();
        eprintln!(
            "EC_ISTEP mi_row={} mi_col={} name=txfm_split val={} ctx={ctx} rng={rng}",
            unit.0,
            unit.1,
            u8::from(split)
        );
    }
    if !split {
        out.push((blk_row, blk_col, tx_px));
        txfm_partition_update(n, unit, tx_px, tx_px);
        return;
    }
    TXFM_SPLIT_HITS.with(|c| c.set(c.get() + 1));
    let sub = tx_px / 2;
    if sub == 4 {
        // libaom's `sub_txs == TX_4X4` early return: the whole 8x8 unit is
        // marked 4x4 (four transform units, raster order) without recursing.
        for row in 0..2 {
            for col in 0..2 {
                if blk_row + row < max_h_mi && blk_col + col < max_w_mi {
                    out.push((blk_row + row, blk_col + col, 4));
                }
            }
        }
        txfm_partition_update(n, unit, 4, tx_px);
        return;
    }
    for row in (0..tx_px / MI).step_by(sub / MI) {
        for col in (0..tx_px / MI).step_by(sub / MI) {
            read_var_tx_size(
                dec,
                cdfs,
                n,
                at_mi,
                blk_max_px,
                (max_w_mi, max_h_mi),
                sub,
                depth + 1,
                blk_row + row,
                blk_col + col,
                out,
            );
        }
    }
}

/// `read_block_tx_size` (libaom `decodeframe.c` `parse_decode_block`'s tx
/// branch): every block of a `TxMode::Select` inter frame codes its transform
/// size right after its own mode info -- an inter non-skip block through the
/// recursive var-tx tree ([`read_var_tx_size`], whose leaf list this returns),
/// every other block through the intra `tx_depth` symbol (a skipped inter
/// block codes no symbol at all) plus a `set_txfm_ctxs` write. Returns the
/// block's own resolved transform side and the leaf list, the latter only
/// when the tree resolved to more than one transform unit.
#[allow(clippy::too_many_arguments)]
fn read_block_tx_size(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    n: &mut Neighbours,
    at_mi: (usize, usize),
    side: usize,
    (mi_cols, mi_rows): (usize, usize),
    is_inter: bool,
    skip: bool,
) -> Result<(usize, Option<Vec<(usize, usize, usize)>>)> {
    if !TX_SELECT_INTER.with(std::cell::Cell::get) {
        return Ok((side, None));
    }
    let side_mi = side / MI;
    if is_inter && !skip {
        let max_tx = side.min(64);
        let max_w_mi = side_mi.min(mi_cols.saturating_sub(at_mi.1));
        let max_h_mi = side_mi.min(mi_rows.saturating_sub(at_mi.0));
        let mut leaves = Vec::new();
        let step = max_tx / MI;
        for row in (0..side_mi).step_by(step) {
            for col in (0..side_mi).step_by(step) {
                read_var_tx_size(
                    dec,
                    cdfs,
                    n,
                    at_mi,
                    side,
                    (max_w_mi, max_h_mi),
                    max_tx,
                    0,
                    row,
                    col,
                    &mut leaves,
                );
            }
        }
        let single = leaves.len() == 1 && leaves[0].2 == side;
        if !single && leaves.iter().any(|leaf| leaf.2 > 32) {
            // Only a block wider than a superblock's own 64x64 max transform
            // reaches here; this crate has no 64-point *inter* coefficient
            // table set ([`inter_txbset_for`] stops at 32).
            return Err(unsupported(
                "an inter var-tx tree with a leaf transform larger than 32x32",
            ));
        }
        let resolved = leaves.last().map_or(side, |l| l.2);
        return Ok((resolved, (!single).then_some(leaves)));
    }
    let tx = if is_inter {
        side.min(64)
    } else {
        let resolved = read_tx_size(
            dec,
            cdfs,
            n,
            at_mi,
            side,
            Some(tx_size_context_txfm(n, at_mi, side)),
        );
        if resolved != side {
            // An intra block inside an inter frame whose `tx_depth` splits its
            // luma transform needs the per-transform-unit intra prediction
            // loop `decode_block` runs for a key frame; this path predicts the
            // whole block at once, so refuse rather than mis-reconstruct.
            return Err(unsupported(
                "an intra block in an inter frame whose tx_depth splits its luma transform \
                 (round 1)",
            ));
        }
        resolved
    };
    set_txfm_ctxs(n, at_mi, tx, side_mi, side_mi, skip && is_inter);
    Ok((tx, None))
}

/// One 8x8 inter leaf's luma residual (lane-txselect): either the whole-8x8
/// transform, or -- when [`read_block_tx_size`]'s var-tx tree took the single
/// split a `BLOCK_8X8` can take (`split`) -- the four 4x4 transform units in
/// raster order, each reading its own coefficient context off the units
/// already decoded before it. Returns the block's luma grid (all-zero in the
/// split case, whose reconstruction has already happened unit by unit) and
/// the `tx_type` chroma inherits: `av1_get_tx_type`'s colocated luma lookup,
/// which for a split block lands on the top-left unit.
#[allow(clippy::too_many_arguments)]
fn read_inter_luma8(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    leaf_mi: (usize, usize),
    y: &mut PlaneBuf,
    px: usize,
    py: usize,
    pred_y: &[u16],
    mode_for_tx: usize,
    base_q_idx: u8,
    scan8: &[u16],
    scan4: &[u16],
    reduced_tx_set: bool,
    split: bool,
) -> Result<(Vec<i32>, TxType)> {
    if !split {
        return read_inter_plane(
            dec,
            cdfs,
            if reduced_tx_set {
                TxbSet::Luma8Inter
            } else {
                TxbSet::Luma8InterSet1
            },
            scan8,
            0,
            neighbours.around_mi(leaf_mi, 8)[0],
            mode_for_tx,
            y,
            px,
            py,
            8,
            base_q_idx,
            pred_y,
            None,
            None,
        );
    }
    let mut first = TxType::DctDct;
    for tu_row in 0..2 {
        for tu_col in 0..2 {
            let tu_mi = (leaf_mi.0 + tu_row, leaf_mi.1 + tu_col);
            let mut tu_pred = Vec::with_capacity(16);
            for rr in 0..4 {
                let start = (tu_row * 4 + rr) * 8 + tu_col * 4;
                tu_pred.extend_from_slice(&pred_y[start..start + 4]);
            }
            let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, 1);
            let (tu_grid, tu_tx_type) = read_inter_plane(
                dec,
                cdfs,
                if reduced_tx_set {
                    TxbSet::Luma4Inter
                } else {
                    TxbSet::Luma4InterSet1
                },
                scan4,
                0,
                neighbours.around_mi(tu_mi, 4)[0],
                mode_for_tx,
                y,
                px + tu_col * 4,
                py + tu_row * 4,
                4,
                base_q_idx,
                &tu_pred,
                None,
                Some(tu_skip_ctx),
            )?;
            if tu_row == 0 && tu_col == 0 {
                first = tu_tx_type;
            }
            neighbours.record_mi_luma(tu_mi, 4, &tu_grid);
        }
    }
    Ok((vec![0i32; 64], first))
}

/// The [`TxbSet`] one leaf of an inter block's var-tx tree reads its
/// coefficient tables from -- the inter counterpart of [`txbset_for`].
fn inter_txbset_for(tx_px: usize, reduced_tx_set: bool) -> TxbSet {
    match tx_px {
        32 => TxbSet::Luma32Inter,
        16 if reduced_tx_set => TxbSet::Luma16Inter,
        16 => TxbSet::Luma16InterSet1,
        8 if reduced_tx_set => TxbSet::Luma8Inter,
        8 => TxbSet::Luma8InterSet1,
        4 if reduced_tx_set => TxbSet::Luma4Inter,
        4 => TxbSet::Luma4InterSet1,
        _ => unreachable!("a var-tx leaf is never wider than a 64x64 block's 32x32 sub-transform"),
    }
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
/// [`txbset_for`], for a block libaom's `is_inter_block` calls inter -- which
/// an intrabc block is (`blockd.h:373`), so its `tx_type` comes off the
/// inter `ext_tx` sets, never the mode-indexed intra ones.
fn txbset_for_inter(tx_px: usize, reduced_tx_set: bool) -> TxbSet {
    match tx_px {
        64 => TxbSet::Luma64,
        32 => TxbSet::Luma32Inter,
        16 if reduced_tx_set => TxbSet::Luma16Inter,
        16 => TxbSet::Luma16InterSet1,
        8 if reduced_tx_set => TxbSet::Luma8Inter,
        8 => TxbSet::Luma8InterSet1,
        _ => TxbSet::Luma64,
    }
}

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

/// spec 7.14.4's `get_filter_level`: `ref_deltas` is indexed by the block's
/// reference frame, `mode_deltas` by `mode_lf_lut[mode]` -- `0` for
/// `GLOBALMV`, `1` for `NEARESTMV`/`NEARMV`/`NEWMV` (intra gets no mode
/// delta). `ref_frame` here is [`Neighbours::ref_grid`]'s packed cell:
/// magnitude = reference (`0` = intra), sign = `GLOBALMV` (lane-av1golden2:
/// the flat `mode_deltas[1]` this used before was a `LAST_FRAME`-reachable
/// wrong delta on every `GLOBALMV` block, masked while any `GOLDEN_FRAME`
/// block anywhere aborted the whole stream's decode before comparison).
fn lf_level(
    lf: &LoopFilterParams,
    plane_idx: usize,
    dir: usize,
    ref_frame: i8,
    delta_lf: i32,
    segment_id: u8,
) -> i32 {
    let base = i32::from(if plane_idx == 0 {
        lf.level[dir]
    } else if plane_idx == 1 {
        lf.level[2]
    } else {
        lf.level[3]
    });
    // spec 7.14.4: the per-superblock `DeltaLF` term applies before the
    // ref/mode delta scaling below, and regardless of `delta_enabled`
    // (that flag only gates `ref_deltas`/`mode_deltas`).
    let base = (base + delta_lf).clamp(0, 63);
    // lane-seg, spec 7.14.4 / libaom `av1_loop_filter_frame_init`: this
    // block's segment's own `SEG_LVL_ALT_LF_*` delta applies after the
    // per-superblock `DeltaLF` term and before the ref/mode deltas below.
    // Feature index: `SEG_LVL_ALT_LF_Y_V + dir` for luma, `_U`/`_V` (3/4)
    // for chroma.
    let feature = if plane_idx == 0 { SEG_LVL_ALT_LF_Y_V + dir } else { plane_idx + 2 };
    let base = match seg_feature(usize::from(segment_id), feature) {
        Some(data) => (base + data).clamp(0, 63),
        None => base,
    };
    if !lf.delta_enabled {
        return base;
    }
    let scale = 1i32 << (base >> 5);
    let is_inter = ref_frame != 0;
    let is_globalmv = ref_frame < 0;
    let ref_idx = ref_frame.unsigned_abs() as usize;
    let mut lvl = base + i32::from(lf.ref_deltas[ref_idx]) * scale;
    if is_inter {
        lvl += i32::from(lf.mode_deltas[usize::from(!is_globalmv)]) * scale;
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
    let cur_tx = if dir == 0 {
        tx_px_at(n, chroma, mi_r, mi_c)
    } else {
        tx_h_px_at(n, chroma, mi_r, mi_c)
    };
    if cur_tx == 0 || (if dir == 0 { x0 } else { y0 }) % cur_tx as usize != 0 {
        return None;
    }
    let cur_ref = n.ref_grid[mi_r * n.skip_grid_cols_mi + mi_c];
    let (pv_mi_r, pv_mi_c) = if dir == 0 {
        (mi_r, mi_c - 1)
    } else {
        (mi_r - 1, mi_c)
    };
    let pv_tx = if dir == 0 {
        tx_px_at(n, chroma, pv_mi_r, pv_mi_c)
    } else {
        tx_h_px_at(n, chroma, pv_mi_r, pv_mi_c)
    };
    let pv_ref = n.ref_grid[pv_mi_r * n.skip_grid_cols_mi + pv_mi_c];
    let delta_idx = if plane_idx == 0 { dir } else { plane_idx + 1 };
    let cur_delta_lf = i32::from(n.delta_lf_grid[mi_r * n.skip_grid_cols_mi + mi_c][delta_idx]);
    let pv_delta_lf = i32::from(n.delta_lf_grid[pv_mi_r * n.skip_grid_cols_mi + pv_mi_c][delta_idx]);

    let cur_level = lf_level(lf, plane_idx, dir, cur_ref, cur_delta_lf, segment_id_at(mi_r, mi_c));
    let pv_level = lf_level(
        lf,
        plane_idx,
        dir,
        pv_ref,
        pv_delta_lf,
        segment_id_at(pv_mi_r, pv_mi_c),
    );
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
    DEBLOCK_HITS.with(|c| c.set(c.get() + 1));
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
    // libaom's `signed_char_clamp_high`: the clamp range scales with
    // `128 << (bit_depth - 8)` alongside the centering constant below.
    let scale = 1i32 << (bit_depth() - 8);
    v.clamp(-128 * scale, 128 * scale - 1)
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
    let centre = 128i32 << (bit_depth() - 8);
    let (ps1, ps0, qs0, qs1) = (p1 - centre, p0 - centre, q0 - centre, q1 - centre);
    let outer = if hev { sclamp(ps1 - qs1) } else { 0 };
    let filter = sclamp(outer + 3 * (qs0 - ps0));
    let filter1 = sclamp(filter + 4) >> 3;
    let filter2 = sclamp(filter + 3) >> 3;
    let oq0 = sclamp(qs0 - filter1) + centre;
    let op0 = sclamp(ps0 + filter2) + centre;
    let tap = if hev { 0 } else { round_pow2(filter1, 1) };
    let oq1 = sclamp(qs1 - tap) + centre;
    let op1 = sclamp(ps1 + tap) + centre;
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
    data: &mut [u16],
    base: usize,
    outer_stride: isize,
    tap_stride: isize,
    len: u8,
    level: i32,
    sharpness: u8,
) {
    let (blimit, limit, hev_thr) = deblock_thresholds(level, sharpness);
    // libaom `highbd_filter_mask*`/`highbd_flat_mask4`/`highbd_hev_mask`:
    // every 8-bit threshold (including the flat masks' fixed `1`) is
    // compared against a raw pixel difference on the stream's own bit-depth
    // scale, so each is shifted left by `bit_depth - 8` before use.
    let shift = bit_depth() - 8;
    let (blimit, limit, hev_thr) = (blimit << shift, limit << shift, hev_thr << shift);
    let flat_thresh = 1i32 << shift;
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
                data[idx(-2)] = op1.clamp(0, sample_max()) as u16;
                data[idx(-1)] = op0.clamp(0, sample_max()) as u16;
                data[idx(0)] = oq0.clamp(0, sample_max()) as u16;
                data[idx(1)] = oq1.clamp(0, sample_max()) as u16;
            }
            6 => {
                let (p2, p1, p0, q0, q1, q2) = (px(-3), px(-2), px(-1), px(0), px(1), px(2));
                let mask = (p2 - p1).abs() <= limit
                    && (p1 - p0).abs() <= limit
                    && (q1 - q0).abs() <= limit
                    && (q2 - q1).abs() <= limit
                    && (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 <= blimit;
                let flat = (p1 - p0).abs() <= flat_thresh
                    && (q1 - q0).abs() <= flat_thresh
                    && (p2 - p0).abs() <= flat_thresh
                    && (q2 - q0).abs() <= flat_thresh;
                let [op1, op0, oq0, oq1] = filter6(mask, hev_thr, flat, p2, p1, p0, q0, q1, q2);
                data[idx(-2)] = op1.clamp(0, sample_max()) as u16;
                data[idx(-1)] = op0.clamp(0, sample_max()) as u16;
                data[idx(0)] = oq0.clamp(0, sample_max()) as u16;
                data[idx(1)] = oq1.clamp(0, sample_max()) as u16;
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
                let flat = (p1 - p0).abs() <= flat_thresh
                    && (q1 - q0).abs() <= flat_thresh
                    && (p2 - p0).abs() <= flat_thresh
                    && (q2 - q0).abs() <= flat_thresh
                    && (p3 - p0).abs() <= flat_thresh
                    && (q3 - q0).abs() <= flat_thresh;
                let [op2, op1, op0, oq0, oq1, oq2] =
                    filter8(mask, hev_thr, flat, p3, p2, p1, p0, q0, q1, q2, q3);
                data[idx(-3)] = op2.clamp(0, sample_max()) as u16;
                data[idx(-2)] = op1.clamp(0, sample_max()) as u16;
                data[idx(-1)] = op0.clamp(0, sample_max()) as u16;
                data[idx(0)] = oq0.clamp(0, sample_max()) as u16;
                data[idx(1)] = oq1.clamp(0, sample_max()) as u16;
                data[idx(2)] = oq2.clamp(0, sample_max()) as u16;
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
                let flat = (p1 - p0).abs() <= flat_thresh
                    && (q1 - q0).abs() <= flat_thresh
                    && (p2 - p0).abs() <= flat_thresh
                    && (q2 - q0).abs() <= flat_thresh
                    && (p3 - p0).abs() <= flat_thresh
                    && (q3 - q0).abs() <= flat_thresh;
                let flat2 = (p4 - p0).abs() <= flat_thresh
                    && (q4 - q0).abs() <= flat_thresh
                    && (p5 - p0).abs() <= flat_thresh
                    && (q5 - q0).abs() <= flat_thresh
                    && (p6 - p0).abs() <= flat_thresh
                    && (q6 - q0).abs() <= flat_thresh;
                let out = filter14(
                    mask, hev_thr, flat, flat2, p6, p5, p4, p3, p2, p1, p0, q0, q1, q2, q3, q4, q5,
                    q6,
                );
                for (k, v) in (-6..=5isize).zip(out) {
                    data[idx(k)] = v.clamp(0, sample_max()) as u16;
                }
            }
        }
    }
}

/// One plane's worth of spec 7.14: every vertical edge across the plane,
/// then every horizontal edge (the order the spec and libaom both use).
fn deblock_plane(
    plane: &mut PlaneBuf,
    plane_idx: usize,
    lf: &LoopFilterParams,
    n: &Neighbours,
    frame_width: usize,
    frame_height: usize,
) {
    let chroma = plane_idx != 0;
    // r7: libaom clips the deblock loop to the CODED frame's own
    // width/height, not the mi-aligned `true_width`/`true_height` margin
    // (the padding up to the next 8-pixel/superblock boundary that
    // reconstruction still fills in but the loop filter never visits).
    // Confirmed against the superres gate: with the margin included, the
    // last coded mi column's horizontal edges were filtered when libaom's
    // real output leaves them untouched (r6's post-deblock diff, 37 px, all
    // in columns 44-47); clipping here closes it (3/3 frames pixel-exact).
    let (cw, ch) = if chroma {
        (frame_width.div_ceil(2), frame_height.div_ceil(2))
    } else {
        (frame_width, frame_height)
    };
    let (tw, th, stride) = (
        plane.true_width.min(cw),
        plane.true_height.min(ch),
        plane.width,
    );
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
                if std::env::var_os("EC_AV1_DEBLOCK_TRACE").is_some()
                    && plane_idx == 0
                    && x0 == 44
                {
                    eprintln!(
                        "HEDGE x0={x0} y0={y0} len={len} level={level} sharpness={}",
                        lf.sharpness
                    );
                }
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
    // lane-part32 r2 debug rung, env-gated: see `apply_deblock`'s sibling.
    if std::env::var_os("EC_AV1_DEBUG_SKIP_CDEF").is_some() {
        return;
    }
    // `cdef.bits == 0` still means "no filtering at all" whenever the single
    // (index 0) strength pair is all-zero -- the fast path every existing
    // gate stream takes. A `bits > 0` frame can still be all-zero-strength
    // at every index, but that is rare enough not to special-case; the
    // per-superblock lookup below degrades to the same index-0 read anyway
    // when [`CDEF_IDX_GRID`] was never populated (`bits == 0`).
    if cdef.bits == 0
        && cdef.y_pri_strength[0] == 0
        && cdef.y_sec_strength[0] == 0
        && cdef.uv_pri_strength[0] == 0
        && cdef.uv_sec_strength[0] == 0
    {
        return;
    }
    let sb_cols = CDEF_SB_COLS.with(|c| c.get());
    let idx_grid = CDEF_IDX_GRID.with(|g| g.borrow().clone());
    let strength_idx = |mi_r: usize, mi_c: usize| -> usize {
        if sb_cols == 0 {
            return 0;
        }
        let (sb_r, sb_c) = (mi_r / SB_MI as usize, mi_c / SB_MI as usize);
        idx_grid
            .get(sb_r * sb_cols + sb_c)
            .copied()
            .unwrap_or(0) as usize
    };
    let src_y = y.data.clone();
    let src_u = u.data.clone();
    let src_v = v.data.clone();
    // libaom `av1_cdef_filter_fb`: `pri_strength = level << coeff_shift`,
    // `sec_strength <<= coeff_shift`, `damping += coeff_shift - (pli != 0)`.
    let coeff_shift = i32::from(bit_depth()) - 8;
    let damping = i32::from(cdef.damping) + coeff_shift;
    let damping_uv = (damping - 1).max(0);
    let mut mi_r = 0usize;
    while mi_r < skip_grid.mi_rows {
        let mut mi_c = 0usize;
        while mi_c < skip_grid.mi_cols {
            if !skip_grid.is_skip_txfm(mi_r, mi_c) {
                let sidx = strength_idx(mi_r, mi_c);
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
                let (dir, var) = cdef_find_dir(coeff_shift, |r, c| {
                    let (ny, nx) = ((oy + r).min(y_true_h - 1), (ox + c).min(y_true_w - 1));
                    i32::from(src_y[ny * y_stride + nx])
                });

                // libaom `av1_cdef_filter_fb` passes `pri_strength ? dir : 0`
                // -- each plane zeroes the shared direction when *its own*
                // frame-level primary strength is 0, independent of the
                // per-block adjusted `t` and of the other plane's strength.
                let y_pri_strength = i32::from(cdef.y_pri_strength[sidx]) << coeff_shift;
                let y_sec_strength = i32::from(cdef.y_sec_strength[sidx]) << coeff_shift;
                let y_dir = if y_pri_strength != 0 { dir } else { 0 };
                let t = cdef_adjust_strength(y_pri_strength, var);
                let enable_primary = t != 0;
                let enable_secondary = y_sec_strength != 0;
                if enable_primary || enable_secondary {
                    cdef_filter_block(
                        sample_y,
                        y,
                        ox,
                        oy,
                        8,
                        8,
                        t,
                        y_sec_strength,
                        y_dir,
                        damping,
                        damping,
                        enable_primary,
                        enable_secondary,
                        coeff_shift,
                    );
                }

                let t_uv = i32::from(cdef.uv_pri_strength[sidx]) << coeff_shift;
                let uv_sec_strength = i32::from(cdef.uv_sec_strength[sidx]) << coeff_shift;
                let uv_dir = if t_uv != 0 { dir } else { 0 };
                let enable_primary_uv = t_uv != 0;
                let enable_secondary_uv = uv_sec_strength != 0;
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
                            uv_sec_strength,
                            uv_dir,
                            damping_uv,
                            damping_uv,
                            enable_primary_uv,
                            enable_secondary_uv,
                            coeff_shift,
                        );
                    }
                }
            }
            mi_c += 2;
        }
        mi_r += 2;
    }
}

/// Spec 7.17: loop restoration, run after [`apply_cdef`] (a sibling lane's
/// ordering note: libaom also runs `superres_post_decode` between the two,
/// but this decoder never implements super-resolution, so there is nothing
/// to interleave). `deblocked_*` is each plane's post-deblock, pre-CDEF
/// buffer (the caller clones it before calling [`apply_cdef`]) -- loop
/// restoration's stripe-boundary rows read from it instead of the
/// CDEF-filtered plane, per `restoration.rs`'s own `lr_sample` doc comment.
#[allow(clippy::too_many_arguments)]
fn apply_loop_restoration(
    y: &mut PlaneBuf,
    u: &mut PlaneBuf,
    v: &mut PlaneBuf,
    deblocked_y: &PlaneBuf,
    deblocked_u: &PlaneBuf,
    deblocked_v: &PlaneBuf,
    lr: &LoopRestorationParams,
    grid: &crate::restoration::RestorationGrid,
) {
    y.data = crate::restoration::apply_loop_restoration_plane(
        &y.data,
        &deblocked_y.data,
        y.width,
        y.true_width,
        y.true_height,
        0,
        lr.frame_restoration_type[0],
        lr.loop_restoration_size[0],
        grid,
        0,
    );
    u.data = crate::restoration::apply_loop_restoration_plane(
        &u.data,
        &deblocked_u.data,
        u.width,
        u.true_width,
        u.true_height,
        1,
        lr.frame_restoration_type[1],
        lr.loop_restoration_size[1],
        grid,
        1,
    );
    v.data = crate::restoration::apply_loop_restoration_plane(
        &v.data,
        &deblocked_v.data,
        v.width,
        v.true_width,
        v.true_height,
        1,
        lr.frame_restoration_type[2],
        lr.loop_restoration_size[2],
        grid,
        2,
    );
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
    allow_screen_content_tools: bool,
    allow_intrabc: bool,
) -> Result<Picture> {
    let delta = DeltaParams::default();
    let single_tile = TileInfo {
        mi_col_starts: vec![0, mi_cols],
        mi_row_starts: vec![0, mi_rows],
        ..TileInfo::default()
    };
    decode_key_frame_tile_with_cdfs(
        &[data],
        &single_tile,
        mi_cols,
        mi_rows,
        base_q_idx,
        crate::quant::QuantDeltas::default(),
        frame_width,
        frame_height,
        enable_filter_intra,
        false,
        cdef,
        loop_filter,
        &LoopRestorationParams::default(),
        None,
        tx_select,
        reduced_tx_set,
        allow_screen_content_tools,
        allow_intrabc,
        delta,
    )
    .map(|(picture, _)| picture)
}

thread_local! {
    /// lane-superres r3: the real decoded margin beyond `frame_width` up to
    /// the mi-aligned `true_width`, set by the most recent
    /// [`decode_key_frame_tile_with_cdfs`] OR (lane-superres r10)
    /// [`decode_inter_frame_tile_with_cdfs`] call -- both frame kinds crop
    /// their reconstructed buffer down from the same mi-aligned extent, so
    /// they share this one slot. See each function's own comment at its
    /// write site. `None` when there was no margin to save.
    static LAST_FRAME_WIDE_MARGIN: std::cell::RefCell<Option<Picture>> =
        const { std::cell::RefCell::new(None) };
}

/// The margin the most recent key- or inter-frame tile decode stashed
/// (see [`LAST_FRAME_WIDE_MARGIN`]) -- `stream.rs`'s superres path
/// reads this immediately after the call, before any other frame
/// decode can overwrite it.
pub(crate) fn take_last_frame_wide_margin() -> Option<Picture> {
    LAST_FRAME_WIDE_MARGIN.with(|m| m.borrow_mut().take())
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
    tiles: &[&[u8]],
    tile_info: &TileInfo,
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    // lane-sbpart r11: this frame header's own per-plane DC/AC quantizer
    // deltas (spec 5.9.12), threaded to [`QUANT_DELTAS`] at the top of every
    // tile alongside [`CURRENT_Q_IDX`].
    deltas: crate::quant::QuantDeltas,
    frame_width: u32,
    frame_height: u32,
    enable_filter_intra: bool,
    enable_edge_filter: bool,
    cdef: &CdefParams,
    loop_filter: &LoopFilterParams,
    lr: &LoopRestorationParams,
    initial_cdfs: Option<Cdfs>,
    tx_select: bool,
    reduced_tx_set: bool,
    allow_screen_content_tools: bool,
    allow_intrabc: bool,
    // lane-realworld r4: this frame header's own `delta` (spec 5.9.17/5.9.18)
    // -- only `q_present`/`q_res` are actually read ([`maybe_read_delta_q`]);
    // `stream.rs` still refuses a frame with `lf_present` set before this
    // function is ever called.
    delta: DeltaParams,
) -> Result<(Picture, Cdfs)> {
    if mi_cols == 0 || mi_rows == 0 {
        return Err(unsupported("a frame with no mode-info grid"));
    }
    ENABLE_EDGE_FILTER.with(|f| f.set(enable_edge_filter));
    let (sb_cols, sb_rows) = (mi_cols.div_ceil(SB_MI), mi_rows.div_ceil(SB_MI));
    INTRABC_MI_GRID.with(|g| {
        *g.borrow_mut() = allow_intrabc.then(|| {
            (
                crate::mvstack::MiGrid::new(mi_cols as usize, mi_rows as usize),
                mi_cols as usize,
                mi_rows as usize,
            )
        });
    });
    INTRABC_DV.with(|c| c.set(None));
    CDEF_BITS.with(|c| c.set(cdef.bits));
    CDEF_SB_COLS.with(|c| c.set(sb_cols as usize));
    CDEF_IDX_GRID.with(|g| *g.borrow_mut() = vec![0u8; sb_cols as usize * sb_rows as usize]);
    DELTA_Q_PRESENT.with(|c| c.set(delta.q_present));
    DELTA_Q_RES.with(|c| c.set(1i32 << delta.q_res));
    DELTA_LF_PRESENT.with(|c| c.set(delta.lf_present));
    DELTA_LF_RES.with(|c| c.set(1i32 << delta.lf_res));
    DELTA_LF_MULTI.with(|c| c.set(delta.lf_multi));
    let (cols32, rows32) = block_grid(mi_cols, mi_rows);
    let (true_width, true_height) = ((mi_cols * 4) as usize, (mi_rows * 4) as usize);
    let (width, height) = (cols32 as usize * BLOCK, rows32 as usize * BLOCK);

    let mut y = PlaneBuf {
        data: vec![0u16; width * height],
        width,
        height,
        true_width,
        true_height,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: width,
        tile_y1: height,
    };
    let mut u = PlaneBuf {
        data: vec![0u16; width * height / 4],
        width: width / 2,
        height: height / 2,
        true_width: true_width / 2,
        true_height: true_height / 2,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: width / 2,
        tile_y1: height / 2,
    };
    let mut v = PlaneBuf {
        data: vec![0u16; width * height / 4],
        width: width / 2,
        height: height / 2,
        true_width: true_width / 2,
        true_height: true_height / 2,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: width / 2,
        tile_y1: height / 2,
    };

    let scan32 = default_scan(TX32);
    let scan16 = default_scan(TX16);
    let scan8 = default_scan(TX8);
    let scan4 = default_scan(TX4);

    let base_cdfs = initial_cdfs.unwrap_or_else(|| Cdfs::new(q_ctx_of(base_q_idx)));
    let mut result_cdfs = base_cdfs.clone();
    let mut neighbours = Neighbours::new(
        cols32 as usize * 2,
        rows32 as usize * 2,
        mi_cols as usize,
        mi_rows as usize,
    );
    let mut lr_grid = crate::restoration::RestorationGrid::new(lr, frame_width, frame_height);
    let mut lr_reference = [(
        crate::restoration::WienerInfo::default(),
        crate::restoration::SgrprojInfo::default(),
    ); 3];

    // lane-tiles r2: each tile gets its own fresh `SymbolDecoder` over its
    // own byte range and its own fresh copy of the frame's initial CDFs
    // (spec 5.11.2 `decode_tile`'s `init_symbol`); only the tile named by
    // `context_update_tile_id` has its end-of-tile adapted table kept as the
    // frame's own output (spec `exit_symbol`) -- every other tile's
    // adaptation is discarded, matching the refusal this replaces.
    for (tile_idx, &tile_bytes) in tiles.iter().enumerate() {
        let tile_num = tile_idx as u32;
        let (trow, tcol) = (tile_num / tile_info.cols, tile_num % tile_info.cols);
        let mi_row0 = tile_info.mi_row_starts[trow as usize];
        let mi_row1 = tile_info.mi_row_starts[trow as usize + 1];
        let mi_col0 = tile_info.mi_col_starts[tcol as usize];
        let mi_col1 = tile_info.mi_col_starts[tcol as usize + 1];
        // Tile boundaries are always superblock-aligned in mi units (spec
        // 5.9.15's uniform/non-uniform derivation both only ever emit
        // multiples of the superblock's own mi size), so plain division is
        // exact here -- no rounding needed.
        // `mi_row0`/`mi_col0` (this tile's own start, `0` or an interior
        // tile boundary) are always superblock-aligned by spec 5.9.15; the
        // frame's own true final edge (`mi_row_starts`/`mi_col_starts`'
        // last entry) is not, so the upper bound needs `div_ceil` the same
        // way the whole-frame `sb_rows`/`sb_cols` above does, to keep the
        // trailing partial superblock row/column in scope.
        let (sb_r0, sb_r1) = ((mi_row0 / SB_MI).min(sb_rows), mi_row1.div_ceil(SB_MI).min(sb_rows));
        let (sb_c0, sb_c1) = ((mi_col0 / SB_MI).min(sb_cols), mi_col1.div_ceil(SB_MI).min(sb_cols));

        let mut cdfs = base_cdfs.clone();
        // spec `decode_tile`: `CurrentQIndex` resets to the frame's own
        // `base_q_idx` at the top of every tile, not once per frame.
        CURRENT_Q_IDX.with(|c| c.set(i32::from(base_q_idx)));
        QUANT_DELTAS.with(|c| c.set(deltas));
        CURRENT_DELTA_LF.with(|c| c.set([0; 4]));
        if std::env::var_os("EC_AV1_TRACE").is_some() {
            eprintln!(
                "TRACE key_tile_bytes tile={tile_num} len={} first8={:02x?} base_q_idx={base_q_idx} mi_cols={mi_cols} mi_rows={mi_rows}",
                tile_bytes.len(),
                &tile_bytes[..tile_bytes.len().min(8)]
            );
        }
        let mut dec = SymbolDecoder::new(tile_bytes);
        neighbours.start_tile(mi_row0 as usize, mi_col0 as usize, mi_col1 as usize);
        y.set_tile_origin(
            mi_col0 as usize * 4,
            mi_row0 as usize * 4,
            (mi_col1 as usize * 4).min(y.width),
            (mi_row1 as usize * 4).min(y.height),
        );
        u.set_tile_origin(
            mi_col0 as usize * 2,
            mi_row0 as usize * 2,
            (mi_col1 as usize * 2).min(u.width),
            (mi_row1 as usize * 2).min(u.height),
        );
        v.set_tile_origin(
            mi_col0 as usize * 2,
            mi_row0 as usize * 2,
            (mi_col1 as usize * 2).min(v.width),
            (mi_row1 as usize * 2).min(v.height),
        );
        TILE_HITS.with(|c| c.set(c.get() + 1));

    for sb_r in sb_r0..sb_r1 {
        neighbours.start_row();
        for sb_c in sb_c0..sb_c1 {
            crate::restoration::read_lr(
                &mut dec,
                &mut cdfs,
                lr,
                &mut lr_grid,
                &mut lr_reference,
                sb_r * SB_MI,
                sb_c * SB_MI,
                SB_MI,
            );
            CDEF_TRANSMITTED.with(|c| c.set(false));
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
                        allow_screen_content_tools,
                        allow_intrabc,
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
                                let (rng, _) = dec.debug_state();
                                eprintln!(
                                    "TRACE partition_w32 mi=({},{}) ctx={ctx32} value={p} rng={rng}",
                                    r32 * BLOCK_MI,
                                    c32 * BLOCK_MI
                                );
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
                                    allow_screen_content_tools,
                                    allow_intrabc,
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
                                                allow_screen_content_tools,
                                                allow_intrabc,
                                                &scan32,
                                                &scan16,
                                                &scan8,
                                                &scan4,
                                                tx_select,
                                                reduced_tx_set,
                                            )?;
                                            continue;
                                        }
                                        if part16 == PARTITION_VERT_B {
                                            VERT_B_INTRA_HITS.with(|c| c.set(c.get() + 1));
                                            // lane-part32 r6: same availability
                                            // defect r5 fixed one level up --
                                            // the TR/BR 8x8 squares below are
                                            // visited out of raster order, so
                                            // they need libaom's
                                            // `has_tr_vert_*`/`has_bl_vert_*`
                                            // tables (the left 8x16 rect goes
                                            // through `Reach::of_rect`, which
                                            // the guard deliberately does not
                                            // affect).
                                            let _vert_ab =
                                                crate::encode::Reach::vert_ab_partition();
                                            // Left rect: 8x16, real
                                            // `decode_block_rect`, sat at the
                                            // 16x16 parent's own origin (no
                                            // fractional-SUB-grid pixel
                                            // offset needed -- the plain
                                            // HORZ/VERT arms this lane's
                                            // charter named DO need one,
                                            // since their second half is
                                            // offset by half the parent;
                                            // unported, see decode_block_rect's
                                            // own doc comment).
                                            decode_block_rect(
                                                &mut dec,
                                                &mut cdfs,
                                                &mut neighbours,
                                                at16,
                                                8,
                                                16,
                                                &mut y,
                                                &mut u,
                                                &mut v,
                                                enable_filter_intra,
                                                allow_screen_content_tools,
                                                base_q_idx,
                                                tx_select,
                                                reduced_tx_set,
                                            )?;
                                            // Two 8x8 squares stacked on the
                                            // right, chained through
                                            // `prev_leaf` exactly like the
                                            // real (non-straddle) SPLIT path
                                            // below: the bottom leaf's ABOVE
                                            // context is the top leaf, not
                                            // the coarse SUB-grid array (a
                                            // real block never sat there).
                                            let (mi_row0, mi_col0) =
                                                (sr as u32 * SUB_MI, sc as u32 * SUB_MI);
                                            let top_right =
                                                (mi_row0 as usize, mi_col0 as usize + 2);
                                            let bot_right =
                                                (mi_row0 as usize + 2, mi_col0 as usize + 2);
                                            let mode_tr = decode_leaf8(
                                                &mut dec,
                                                &mut cdfs,
                                                &mut neighbours,
                                                at16,
                                                top_right,
                                                (&scan8, &scan4),
                                                None,
                                                &mut y,
                                                &mut u,
                                                &mut v,
                                                base_q_idx,
                                                enable_filter_intra,
                                                allow_screen_content_tools,
                                                allow_intrabc,
                                                tx_select,
                                                reduced_tx_set,
                                            )?;
                                            let mode_br = decode_leaf8(
                                                &mut dec,
                                                &mut cdfs,
                                                &mut neighbours,
                                                at16,
                                                bot_right,
                                                (&scan8, &scan4),
                                                Some((top_right, mode_tr)),
                                                &mut y,
                                                &mut u,
                                                &mut v,
                                                base_q_idx,
                                                enable_filter_intra,
                                                allow_screen_content_tools,
                                                allow_intrabc,
                                                tx_select,
                                                reduced_tx_set,
                                            )?;
                                            // `record()`'s above_mode/left_mode
                                            // write is a no-op at an 8x8
                                            // leaf's own side (same gap the
                                            // real-SPLIT path above patches):
                                            // force it from the last-decoded
                                            // (bottom-right) leaf.
                                            neighbours.above_mode[sc] = mode_br;
                                            neighbours.left_mode[sr] = mode_br;
                                            continue;
                                        }
                                        if part16 == PARTITION_HORZ {
                                            HORZ_VERT_INTRA_HITS.with(|c| c.set(c.get() + 1));
                                            let (mi_row0, mi_col0) = (
                                                sr as u32 * SUB_MI,
                                                sc as u32 * SUB_MI,
                                            );
                                            let top = (mi_row0 as usize, mi_col0 as usize);
                                            let bot = (mi_row0 as usize + 2, mi_col0 as usize);
                                            let mode_top = decode_leaf_rect(
                                                &mut dec,
                                                &mut cdfs,
                                                &mut neighbours,
                                                at16,
                                                top,
                                                16,
                                                8,
                                                None,
                                                &mut y,
                                                &mut u,
                                                &mut v,
                                                enable_filter_intra,
                                                allow_screen_content_tools,
                                                base_q_idx,
                                                tx_select,
                                                reduced_tx_set,
                                            )?;
                                            let mode_bot = decode_leaf_rect(
                                                &mut dec,
                                                &mut cdfs,
                                                &mut neighbours,
                                                at16,
                                                bot,
                                                16,
                                                8,
                                                Some((top, mode_top)),
                                                &mut y,
                                                &mut u,
                                                &mut v,
                                                enable_filter_intra,
                                                allow_screen_content_tools,
                                                base_q_idx,
                                                tx_select,
                                                reduced_tx_set,
                                            )?;
                                            neighbours.above_mode[sc] = mode_bot;
                                            neighbours.left_mode[sr] = mode_bot;
                                            continue;
                                        }
                                        if part16 == PARTITION_VERT {
                                            HORZ_VERT_INTRA_HITS.with(|c| c.set(c.get() + 1));
                                            let (mi_row0, mi_col0) = (
                                                sr as u32 * SUB_MI,
                                                sc as u32 * SUB_MI,
                                            );
                                            let left = (mi_row0 as usize, mi_col0 as usize);
                                            let right = (mi_row0 as usize, mi_col0 as usize + 2);
                                            let mode_left = decode_leaf_rect(
                                                &mut dec,
                                                &mut cdfs,
                                                &mut neighbours,
                                                at16,
                                                left,
                                                8,
                                                16,
                                                None,
                                                &mut y,
                                                &mut u,
                                                &mut v,
                                                enable_filter_intra,
                                                allow_screen_content_tools,
                                                base_q_idx,
                                                tx_select,
                                                reduced_tx_set,
                                            )?;
                                            let mode_right = decode_leaf_rect(
                                                &mut dec,
                                                &mut cdfs,
                                                &mut neighbours,
                                                at16,
                                                right,
                                                8,
                                                16,
                                                Some((left, mode_left)),
                                                &mut y,
                                                &mut u,
                                                &mut v,
                                                enable_filter_intra,
                                                allow_screen_content_tools,
                                                base_q_idx,
                                                tx_select,
                                                reduced_tx_set,
                                            )?;
                                            neighbours.above_mode[sc] = mode_right;
                                            neighbours.left_mode[sr] = mode_right;
                                            continue;
                                        }
                                        if part16 != PARTITION_SPLIT {
                                            return Err(unsupported(
                                                "a HORZ_A/HORZ_B/VERT_A partition below 16x16 (this decoder codes only the square arms, HORZ, VERT, VERT_B, and a clean split below 16x16)",
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
                                        // Each 8x8 leaf's intra-mode context
                                        // must see its own siblings: the
                                        // bottom-left leaf's ABOVE neighbour
                                        // is leaf 0, not the previously
                                        // decoded leaf 1 (and the bottom-right
                                        // leaf's above is leaf 1, while
                                        // `prev_leaf` only ever carried the
                                        // immediately preceding one). The
                                        // coarse `above_mode`/`left_mode`
                                        // arrays hold one slot per 16x16, so
                                        // the sibling modes are swapped in
                                        // around each leaf call and restored
                                        // after; the loop tail writes the last
                                        // leaf's mode as before.
                                        let mut sib_modes: [Option<(usize, usize)>; 4] = [None; 4];
                                        for i in 0..4 {
                                            let (mr, mc) =
                                                (mi_row0 + (i / 2) * 2, mi_col0 + (i % 2) * 2);
                                            let leaf_mi = (mr as usize, mc as usize);
                                            let li = i as usize;
                                            let saved_modes =
                                                (neighbours.above_mode[sc], neighbours.left_mode[sr]);
                                            if li >= 2 {
                                                if let Some((below, _)) = sib_modes[li - 2] {
                                                    neighbours.above_mode[sc] = below;
                                                }
                                            }
                                            if li % 2 == 1 {
                                                if let Some((_, right)) = sib_modes[li - 1] {
                                                    neighbours.left_mode[sr] = right;
                                                }
                                            }
                                            let leaf_ctx = neighbours.partition_ctx_mi(leaf_mi, 8);
                                            let part8 =
                                                dec.symbol(&mut cdfs.partition_w8[leaf_ctx]);
                                            if std::env::var_os("EC_AV1_TRACE").is_some() {
                                                eprintln!(
                                                    "TRACE partition_w8 mi=({mr},{mc}) ctx={leaf_ctx} value={part8}"
                                                );
                                            }
                                            let leaf_mode = if part8 == PARTITION_NONE {
                                                decode_leaf8(
                                                    &mut dec,
                                                    &mut cdfs,
                                                    &mut neighbours,
                                                    at16,
                                                    leaf_mi,
                                                    (&scan8, &scan4),
                                                    None,
                                                    &mut y,
                                                    &mut u,
                                                    &mut v,
                                                    base_q_idx,
                                                    enable_filter_intra,
                                                    allow_screen_content_tools,
                                                    allow_intrabc,
                                                    tx_select,
                                                    reduced_tx_set,
                                                ).map(|m| (m, m))?
                                            } else if part8 == PARTITION_SPLIT {
                                                decode_leaf_split4(
                                                    &mut dec,
                                                    &mut cdfs,
                                                    &mut neighbours,
                                                    at16,
                                                    leaf_mi,
                                                    &scan4,
                                                    None,
                                                    &mut y,
                                                    &mut u,
                                                    &mut v,
                                                    base_q_idx,
                                                    enable_filter_intra,
                                                    allow_intrabc,
                                                    reduced_tx_set,
                                                )?
                                            } else if part8 == PARTITION_HORZ
                                                || part8 == PARTITION_VERT
                                            {
                                                decode_leaf_rect8(
                                                    &mut dec,
                                                    &mut cdfs,
                                                    &mut neighbours,
                                                    at16,
                                                    leaf_mi,
                                                    part8 == PARTITION_VERT,
                                                    &scan4,
                                                    &mut y,
                                                    &mut u,
                                                    &mut v,
                                                    base_q_idx,
                                                    enable_filter_intra,
                                                    allow_intrabc,
                                                    tx_select,
                                                    reduced_tx_set,
                                                )?
                                            } else {
                                                unreachable!("partition_w8 is a 4-symbol CDF: NONE/HORZ/VERT/SPLIT only");
                                            };
                                            neighbours.above_mode[sc] = saved_modes.0;
                                            neighbours.left_mode[sr] = saved_modes.1;
                                            sib_modes[li] = Some(leaf_mode);
                                        }
                                        if let Some((below, _)) = sib_modes[2].or(sib_modes[3]).or(sib_modes[0]) {
                                            neighbours.above_mode[sc] = below;
                                        }
                                        if let Some((_, right)) = sib_modes[1].or(sib_modes[3]).or(sib_modes[0]) {
                                            neighbours.left_mode[sr] = right;
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
                                    // See the sibling-mode note in the clean
                                    // SPLIT path above.
                                                                        let mut sib_modes: [Option<(usize, usize)>; 4] = [None; 4];
                                    for (mr, mc) in leaf_positions {
                                        let leaf_mi = (mr as usize, mc as usize);
                                        let li = (((mr - mi_row0) / 2) * 2 + (mc - mi_col0) / 2)
                                            as usize;
                                        let saved_modes =
                                            (neighbours.above_mode[sc], neighbours.left_mode[sr]);
                                        if li >= 2 {
                                            if let Some((below, _)) = sib_modes[li - 2] {
                                                neighbours.above_mode[sc] = below;
                                            }
                                        }
                                        if li % 2 == 1 {
                                            if let Some((_, right)) = sib_modes[li - 1] {
                                                neighbours.left_mode[sr] = right;
                                            }
                                        }
                                        let leaf_ctx = neighbours.partition_ctx_mi(leaf_mi, 8);
                                        let part8 = dec.symbol(&mut cdfs.partition_w8[leaf_ctx]);
                                        if std::env::var_os("EC_AV1_TRACE").is_some() {
                                            eprintln!(
                                                "TRACE partition_w8 mi=({mr},{mc}) ctx={leaf_ctx} value={part8}"
                                            );
                                        }
                                        let leaf_mode = if part8 == PARTITION_NONE {
                                            decode_leaf8(
                                                &mut dec,
                                                &mut cdfs,
                                                &mut neighbours,
                                                at16,
                                                leaf_mi,
                                                (&scan8, &scan4),
                                                None,
                                                &mut y,
                                                &mut u,
                                                &mut v,
                                                base_q_idx,
                                                enable_filter_intra,
                                                allow_screen_content_tools,
                                                allow_intrabc,
                                                tx_select,
                                                reduced_tx_set,
                                            ).map(|m| (m, m))?
                                        } else if part8 == PARTITION_SPLIT {
                                            decode_leaf_split4(
                                                &mut dec,
                                                &mut cdfs,
                                                &mut neighbours,
                                                at16,
                                                leaf_mi,
                                                &scan4,
                                                None,
                                                &mut y,
                                                &mut u,
                                                &mut v,
                                                base_q_idx,
                                                enable_filter_intra,
                                                allow_intrabc,
                                                reduced_tx_set,
                                            )?
                                        } else if part8 == PARTITION_HORZ
                                            || part8 == PARTITION_VERT
                                        {
                                            decode_leaf_rect8(
                                                &mut dec,
                                                &mut cdfs,
                                                &mut neighbours,
                                                at16,
                                                leaf_mi,
                                                part8 == PARTITION_VERT,
                                                &scan4,
                                                &mut y,
                                                &mut u,
                                                &mut v,
                                                base_q_idx,
                                                enable_filter_intra,
                                                allow_intrabc,
                                                tx_select,
                                                reduced_tx_set,
                                            )?
                                        } else {
                                            unreachable!("partition_w8 is a 4-symbol CDF: NONE/HORZ/VERT/SPLIT only");
                                        };
                                        neighbours.above_mode[sc] = saved_modes.0;
                                        neighbours.left_mode[sr] = saved_modes.1;
                                        sib_modes[li] = Some(leaf_mode);
                                    }
                                    // `record()`'s `above_mode`/`left_mode`
                                    // write is a no-op at an 8x8 leaf's own
                                    // side, so force the write once the whole
                                    // 16x16 slot's leaves are done, from the
                                    // last leaf (mirrors the writer's r15
                                    // fix).
                                    if let Some((below, _)) = sib_modes[2].or(sib_modes[3]).or(sib_modes[0]) {
                                        neighbours.above_mode[sc] = below;
                                    }
                                    if let Some((_, right)) = sib_modes[1].or(sib_modes[3]).or(sib_modes[0]) {
                                        neighbours.left_mode[sr] = right;
                                    }
                                }
                            }
                            PARTITION_HORZ => {
                                // lane-intradisp r1: two true 32x16 strips.
                                decode_block_rect(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    at32,
                                    32,
                                    16,
                                    &mut y,
                                    &mut u,
                                    &mut v,
                                    enable_filter_intra,
                                    allow_screen_content_tools,
                                    base_q_idx,
                                    tx_select,
                                    reduced_tx_set,
                                )?;
                                decode_block_rect(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    (at32.0 + 1, at32.1),
                                    32,
                                    16,
                                    &mut y,
                                    &mut u,
                                    &mut v,
                                    enable_filter_intra,
                                    allow_screen_content_tools,
                                    base_q_idx,
                                    tx_select,
                                    reduced_tx_set,
                                )?;
                            }
                            PARTITION_VERT => {
                                // lane-intradisp r1: mirror of PARTITION_HORZ
                                // above with width/height swapped.
                                decode_block_rect(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    at32,
                                    16,
                                    32,
                                    &mut y,
                                    &mut u,
                                    &mut v,
                                    enable_filter_intra,
                                    allow_screen_content_tools,
                                    base_q_idx,
                                    tx_select,
                                    reduced_tx_set,
                                )?;
                                decode_block_rect(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    (at32.0, at32.1 + 1),
                                    16,
                                    32,
                                    &mut y,
                                    &mut u,
                                    &mut v,
                                    enable_filter_intra,
                                    allow_screen_content_tools,
                                    base_q_idx,
                                    tx_select,
                                    reduced_tx_set,
                                )?;
                            }
                            PARTITION_HORZ_A => {
                                // lane-part32 r1: two 16x16 squares on top +
                                // a true 32x16 strip below (mirrors
                                // decode_inter_block's cd6cb6d HORZ_A: TL,
                                // TR, bottom strip).
                                INTRA_HORZ_A_HITS.with(|c| c.set(c.get() + 1));
                                decode_block(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    at32,
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
                                    allow_screen_content_tools,
                                    allow_intrabc,
                                    &scan32,
                                    &scan16,
                                    &scan8,
                                    &scan4,
                                    tx_select,
                                    reduced_tx_set,
                                )?;
                                decode_block(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    (at32.0, at32.1 + 1),
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
                                    allow_screen_content_tools,
                                    allow_intrabc,
                                    &scan32,
                                    &scan16,
                                    &scan8,
                                    &scan4,
                                    tx_select,
                                    reduced_tx_set,
                                )?;
                                decode_block_rect(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    (at32.0 + 1, at32.1),
                                    32,
                                    16,
                                    &mut y,
                                    &mut u,
                                    &mut v,
                                    enable_filter_intra,
                                    allow_screen_content_tools,
                                    base_q_idx,
                                    tx_select,
                                    reduced_tx_set,
                                )?;
                            }
                            PARTITION_HORZ_B => {
                                // lane-part32 r1: a true 32x16 strip on top +
                                // two 16x16 squares below (mirrors HORZ_A
                                // with the strip/squares order flipped, same
                                // shape as libaom decode_partition's HORZ_B).
                                INTRA_HORZ_B_HITS.with(|c| c.set(c.get() + 1));
                                decode_block_rect(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    at32,
                                    32,
                                    16,
                                    &mut y,
                                    &mut u,
                                    &mut v,
                                    enable_filter_intra,
                                    allow_screen_content_tools,
                                    base_q_idx,
                                    tx_select,
                                    reduced_tx_set,
                                )?;
                                decode_block(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    (at32.0 + 1, at32.1),
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
                                    allow_screen_content_tools,
                                    allow_intrabc,
                                    &scan32,
                                    &scan16,
                                    &scan8,
                                    &scan4,
                                    tx_select,
                                    reduced_tx_set,
                                )?;
                                decode_block(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    (at32.0 + 1, at32.1 + 1),
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
                                    allow_screen_content_tools,
                                    allow_intrabc,
                                    &scan32,
                                    &scan16,
                                    &scan8,
                                    &scan4,
                                    tx_select,
                                    reduced_tx_set,
                                )?;
                            }
                            PARTITION_VERT_A => {
                                // lane-part32 r1: mirror of HORZ_A with
                                // width/height swapped (TL, BL, right 16x32
                                // strip).
                                INTRA_VERT_A_HITS.with(|c| c.set(c.get() + 1));
                                let _vert_ab = crate::encode::Reach::vert_ab_partition();
                                decode_block(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    at32,
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
                                    allow_screen_content_tools,
                                    allow_intrabc,
                                    &scan32,
                                    &scan16,
                                    &scan8,
                                    &scan4,
                                    tx_select,
                                    reduced_tx_set,
                                )?;
                                decode_block(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    (at32.0 + 1, at32.1),
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
                                    allow_screen_content_tools,
                                    allow_intrabc,
                                    &scan32,
                                    &scan16,
                                    &scan8,
                                    &scan4,
                                    tx_select,
                                    reduced_tx_set,
                                )?;
                                decode_block_rect(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    (at32.0, at32.1 + 1),
                                    16,
                                    32,
                                    &mut y,
                                    &mut u,
                                    &mut v,
                                    enable_filter_intra,
                                    allow_screen_content_tools,
                                    base_q_idx,
                                    tx_select,
                                    reduced_tx_set,
                                )?;
                            }
                            PARTITION_VERT_B => {
                                // lane-part32 r1: a true 16x32 strip on the
                                // left + two 16x16 squares on the right
                                // (libaom decode_partition VERT_B: left
                                // strip, TR, BR).
                                INTRA_VERT_B_HITS.with(|c| c.set(c.get() + 1));
                                let _vert_ab = crate::encode::Reach::vert_ab_partition();
                                decode_block_rect(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    at32,
                                    16,
                                    32,
                                    &mut y,
                                    &mut u,
                                    &mut v,
                                    enable_filter_intra,
                                    allow_screen_content_tools,
                                    base_q_idx,
                                    tx_select,
                                    reduced_tx_set,
                                )?;
                                decode_block(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    (at32.0, at32.1 + 1),
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
                                    allow_screen_content_tools,
                                    allow_intrabc,
                                    &scan32,
                                    &scan16,
                                    &scan8,
                                    &scan4,
                                    tx_select,
                                    reduced_tx_set,
                                )?;
                                decode_block(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    (at32.0 + 1, at32.1 + 1),
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
                                    allow_screen_content_tools,
                                    allow_intrabc,
                                    &scan32,
                                    &scan16,
                                    &scan8,
                                    &scan4,
                                    tx_select,
                                    reduced_tx_set,
                                )?;
                            }
                            PARTITION_HORZ_4 | PARTITION_VERT_4 => {
                                // lane-tx64x16 r3: the 1:4 pair at the 32x32
                                // level -- four 32x8 (or 8x32) strips in
                                // raster order, `quarter_step` = 2 mi, with
                                // libaom `decode_partition`'s own `i > 0`
                                // frame-edge break.
                                let horz = part32 == PARTITION_HORZ_4;
                                let base_mi = (at32.0 * (SUB / MI), at32.1 * (SUB / MI));
                                let mut prev: Option<((usize, usize), usize)> = None;
                                for i in 0..4 {
                                    let strip_mi = if horz {
                                        (base_mi.0 + i * 2, base_mi.1)
                                    } else {
                                        (base_mi.0, base_mi.1 + i * 2)
                                    };
                                    if i > 0
                                        && (strip_mi.0 >= mi_rows as usize
                                            || strip_mi.1 >= mi_cols as usize)
                                    {
                                        break;
                                    }
                                    let (bw, bh) = if horz { (32, 8) } else { (8, 32) };
                                    let (mode, _uv) = decode_block_rect4(
                                        &mut dec,
                                        &mut cdfs,
                                        &mut neighbours,
                                        strip_mi,
                                        bw,
                                        bh,
                                        prev,
                                        &mut y,
                                        &mut u,
                                        &mut v,
                                        enable_filter_intra,
                                        allow_screen_content_tools,
                                        base_q_idx,
                                        tx_select,
                                    )?;
                                    prev = Some((strip_mi, mode));
                                }
                            }
                            _ => {
                                return Err(unsupported(format!(
                                    "a 32x32 partition type this decoder does not code (value={part32})"
                                )));
                            }
                        }
                    }
                }
                PARTITION_HORZ => {
                    // lane-sbpart r2: two true 64x32 strips.
                    decode_block_rect64(
                        &mut dec,
                        &mut cdfs,
                        &mut neighbours,
                        at,
                        64,
                        32,
                        &mut y,
                        &mut u,
                        &mut v,
                        enable_filter_intra,
                        allow_screen_content_tools,
                        base_q_idx,
                        tx_select,
                        reduced_tx_set,
                    )?;
                    decode_block_rect64(
                        &mut dec,
                        &mut cdfs,
                        &mut neighbours,
                        (at.0 + 2, at.1),
                        64,
                        32,
                        &mut y,
                        &mut u,
                        &mut v,
                        enable_filter_intra,
                        allow_screen_content_tools,
                        base_q_idx,
                        tx_select,
                        reduced_tx_set,
                    )?;
                }
                PARTITION_VERT => {
                    // lane-sbpart r2: mirror of PARTITION_HORZ above with
                    // width/height swapped.
                    decode_block_rect64(
                        &mut dec,
                        &mut cdfs,
                        &mut neighbours,
                        at,
                        32,
                        64,
                        &mut y,
                        &mut u,
                        &mut v,
                        enable_filter_intra,
                        allow_screen_content_tools,
                        base_q_idx,
                        tx_select,
                        reduced_tx_set,
                    )?;
                    decode_block_rect64(
                        &mut dec,
                        &mut cdfs,
                        &mut neighbours,
                        (at.0, at.1 + 2),
                        32,
                        64,
                        &mut y,
                        &mut u,
                        &mut v,
                        enable_filter_intra,
                        allow_screen_content_tools,
                        base_q_idx,
                        tx_select,
                        reduced_tx_set,
                    )?;
                }
                PARTITION_HORZ_A | PARTITION_HORZ_B | PARTITION_VERT_A | PARTITION_VERT_B => {
                    // lane-part32 r4: the four superblock-level AB arms, each
                    // two 32x32 squares plus one 64x32/32x64 strip, in
                    // libaom's own `decode_partition` order (decodeframe.c:
                    // HORZ_A = TL, TR, bottom strip; HORZ_B = top strip, BL,
                    // BR; VERT_A = TL, BL, right strip; VERT_B = left strip,
                    // TR, BR). The pieces are exactly the ones already proven
                    // by the `PARTITION_NONE`-under-SPLIT (32x32 square) and
                    // `PARTITION_HORZ`/`VERT` (rect64 strip) arms above.
                    SB_AB_HITS.with(|c| {
                        let mut h = c.get();
                        h[part - PARTITION_HORZ_A] += 1;
                        c.set(h);
                    });
                    // lane-part32 r5: only the two VERTICAL arms reorder the
                    // square sub-blocks (TL, BL, TR, BR), so only they switch
                    // libaom's `has_tr`/`has_bl` tables.
                    let _vert_ab = (part == PARTITION_VERT_A || part == PARTITION_VERT_B)
                        .then(crate::encode::Reach::vert_ab_partition);
                    macro_rules! square32 {
                        ($at:expr) => {
                            decode_block(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                $at,
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
                                allow_screen_content_tools,
                                allow_intrabc,
                                &scan32,
                                &scan16,
                                &scan8,
                                &scan4,
                                tx_select,
                                reduced_tx_set,
                            )?
                        };
                    }
                    macro_rules! strip64 {
                        ($at:expr, $bw:expr, $bh:expr) => {
                            decode_block_rect64(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                $at,
                                $bw,
                                $bh,
                                &mut y,
                                &mut u,
                                &mut v,
                                enable_filter_intra,
                                allow_screen_content_tools,
                                base_q_idx,
                                tx_select,
                                reduced_tx_set,
                            )?
                        };
                    }
                    let (br, bc) = (at.0 + 2, at.1 + 2);
                    match part {
                        PARTITION_HORZ_A => {
                            square32!(at);
                            square32!((at.0, bc));
                            strip64!((br, at.1), 64, 32);
                        }
                        PARTITION_HORZ_B => {
                            strip64!(at, 64, 32);
                            square32!((br, at.1));
                            square32!((br, bc));
                        }
                        PARTITION_VERT_A => {
                            square32!(at);
                            square32!((br, at.1));
                            strip64!((at.0, bc), 32, 64);
                        }
                        _ => {
                            strip64!(at, 32, 64);
                            square32!((at.0, bc));
                            square32!((br, bc));
                        }
                    }
                }
                PARTITION_HORZ_4 | PARTITION_VERT_4 => {
                    // lane-tx64x16: the 1:4 pair at 64x64 -- four 64x16 (or
                    // 16x64) strips in raster order. `decode_partition`
                    // (`decodeframe.c`) steps by `quarter_step = mi_size / 4`
                    // (4 mi = 16 px = one SUB unit here) and BREAKS at the
                    // frame edge for `i > 0`, so a partial superblock codes
                    // only the strips whose origin is inside the frame.
                    let horz = part == PARTITION_HORZ_4;
                    for i in 0..4 {
                        let step_mi = i * (SB_MI as usize / 4);
                        let (this_r_mi, this_c_mi) = if horz {
                            (sb_r as usize * SB_MI as usize + step_mi, sb_c as usize * SB_MI as usize)
                        } else {
                            (sb_r as usize * SB_MI as usize, sb_c as usize * SB_MI as usize + step_mi)
                        };
                        if i > 0
                            && (this_r_mi >= mi_rows as usize || this_c_mi >= mi_cols as usize)
                        {
                            break;
                        }
                        let strip_at = if horz { (at.0 + i, at.1) } else { (at.0, at.1 + i) };
                        let (bw, bh) = if horz { (64, 16) } else { (16, 64) };
                        decode_block_rect64(
                            &mut dec,
                            &mut cdfs,
                            &mut neighbours,
                            strip_at,
                            bw,
                            bh,
                            &mut y,
                            &mut u,
                            &mut v,
                            enable_filter_intra,
                            allow_screen_content_tools,
                            base_q_idx,
                            tx_select,
                            reduced_tx_set,
                        )?;
                    }
                }
                _ => {
                    return Err(unsupported(
                        "a superblock-level partition value outside PARTITION_NONE..PARTITION_VERT_4",
                    ));
                }
            }
        }
    }

        if tile_num == tile_info.context_update_tile_id {
            result_cdfs = cdfs;
        }
    }

    // lane-comppin r4: pre-loop-filter decode-order dump, matching aomdec's
    // own EC_AV1_PREFILT_DUMP shape (decodeframe.c ~5451) -- diffs against
    // that isolate whether a decode-order frame's mismatch already exists
    // in reconstruction (this dump) vs is introduced by the loop filter
    // (EC_AV1_DECODE_ORDER_DUMP's post-filter dump).
    if let Ok(path) = std::env::var("EC_AV1_PREFILT_DUMP") {
        use std::io::Write;
        let idx = PREFILT_PICTURE_IDX.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut f) = std::fs::File::create(format!("{path}.f{idx}")) {
            let narrow = |p: &PlaneBuf| -> Vec<u8> { p.data.iter().map(|&s| s as u8).collect() };
            let _ = f.write_all(&narrow(&y));
            let _ = f.write_all(&narrow(&u));
            let _ = f.write_all(&narrow(&v));
        }
    } else {
        PREFILT_PICTURE_IDX.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    if let Ok(path) = std::env::var("EC_AV1_PREFILT_WIDE_DUMP") {
        use std::io::Write;
        static IDX2: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let idx = IDX2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let crop_wide = |plane: &PlaneBuf| -> Vec<u8> {
            let mut out = Vec::with_capacity(plane.true_width * plane.true_height);
            for row in 0..plane.true_height {
                out.extend(plane.data[row * plane.width..][..plane.true_width].iter().map(|&s| s as u8));
            }
            out
        };
        if let Ok(mut f) = std::fs::File::create(format!("{path}.f{idx}")) {
            let _ = f.write_all(&crop_wide(&y));
            let _ = f.write_all(&crop_wide(&u));
            let _ = f.write_all(&crop_wide(&v));
        }
    }
    apply_deblock(
        &mut y,
        &mut u,
        &mut v,
        loop_filter,
        &neighbours,
        frame_width as usize,
        frame_height as usize,
    );
    let (deblocked_y, deblocked_u, deblocked_v) = (y.clone(), u.clone(), v.clone());
    // lane-tiny r4: post-deblock / post-CDEF dumps mirroring aomdec's own
    // EC_AV1_POSTDEBLOCK_DUMP (decodeframe.c ~5404) -- with the pre-filter
    // dump above these bisect WHICH filter stage introduced a mismatch.
    dump_stage("EC_AV1_POSTDEBLOCK_DUMP", &y, &u, &v);
    apply_cdef(&mut y, &mut u, &mut v, cdef, &neighbours);
    dump_stage("EC_AV1_POSTCDEF_DUMP", &y, &u, &v);
    apply_loop_restoration(&mut y, &mut u, &mut v, &deblocked_y, &deblocked_u, &deblocked_v, lr, &lr_grid);

    let (fw, fh) = (frame_width as usize, frame_height as usize);
    if fw == width && fh == height {
        LAST_FRAME_WIDE_MARGIN.with(|m| *m.borrow_mut() = None);
        return Ok((
            Picture {
                width,
                height,
                y: y.data,
                u: u.data,
                v: v.data,
            },
            result_cdfs,
        ));
    }
    let crop = |plane: &PlaneBuf, w: usize, h: usize| -> Vec<u16> {
        let mut out = Vec::with_capacity(w * h);
        for row in 0..h {
            out.extend(plane.data[row * plane.width..][..w].iter().copied());
        }
        out
    };
    // lane-superres r3: `true_width`/`true_height` (`mi_cols`/`mi_rows` * 4)
    // is the real reconstructed extent -- columns `[fw, true_width)` hold
    // genuine decoded samples (the last coding block straddles the frame
    // edge whenever `frame_width` isn't 8-sample aligned), not padding.
    // libaom's superres upscale runs on that buffer's own border-extended
    // margin, so it reads those real columns, not a synthetic replicate of
    // column `fw - 1`. Stash them for `stream.rs`'s superres path (a real
    // aomenc 43->64 stream pinned column-by-column against libaom's own
    // `av1_convolve_horiz_rs_c` via `scripts/superres-pin-harness.c` --
    // `row6-realedgeval` proved the replicate-of-`fw-1` padding this
    // decoder used before was off by 1 at the columns the tap window
    // reaches into that real margin; `None` when there is no margin (this
    // frame's `true_width`/`true_height` already equal `fw`/`fh`, e.g. a
    // non-superres frame whose width just isn't 32-block aligned).
    LAST_FRAME_WIDE_MARGIN.with(|m| {
        *m.borrow_mut() = if true_width > fw || true_height > fh {
            let yc = crop(&y, true_width, true_height);
            if let Ok(path) = std::env::var("EC_AV1_MARGIN_DUMP") {
                use std::io::Write;
                static IDX: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                let idx = IDX.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if let Ok(mut f) = std::fs::File::create(format!("{path}.f{idx}")) {
                    // corner-cut: this diagnostic dump stays 8-bit-narrowed
                    // regardless of the stream's real bit depth (debug-only,
                    // never read by a gate); widen if a >8-bit margin dump
                    // is ever needed.
                    let _ = f.write_all(&yc.iter().map(|&s| s as u8).collect::<Vec<u8>>());
                }
            }
            Some(Picture {
                width: true_width,
                height: true_height,
                y: yc,
                u: crop(&u, true_width.div_ceil(2), true_height.div_ceil(2)),
                v: crop(&v, true_width.div_ceil(2), true_height.div_ceil(2)),
            })
        } else {
            None
        }
    });
    Ok((
        Picture {
            width: fw,
            height: fh,
            y: crop(&y, fw, fh),
            u: crop(&u, fw.div_ceil(2), fh.div_ceil(2)),
            v: crop(&v, fw.div_ceil(2), fh.div_ceil(2)),
        },
        result_cdfs,
    ))
}

/// `av1_get_pred_context_switchable_interp`'s context for `interp_filter[dir]`
/// -- `above`/`left` are the neighbour's own resolved index for this
/// direction (0..=2, or `3` = no info, spec `SWITCHABLE_FILTERS`).
/// `is_compound` folds in libaom `av1_get_pred_context_switchable_interp`'s
/// own `ctx_offset` term (`INTER_FILTER_COMP_OFFSET` = 4, added when *this*
/// block itself has a second reference) -- lane-av1idx: a stale "this
/// decoder never codes a compound reference" assumption baked this to
/// always-0, which desynced the CDF context bucket (not the decoded value
/// outright, hence small pixel deltas rather than a hard desync) the moment
/// lane-av1blend unmasked plain-average compound blocks on a Switchable-
/// filter frame.
fn switchable_interp_ctx(above: u8, left: u8, dir: usize, is_compound: bool) -> usize {
    let base = dir * 8 + if is_compound { 4 } else { 0 };
    let term = if left == above {
        left
    } else if left == 3 {
        above
    } else if above == 3 {
        left
    } else {
        3
    };
    base + term as usize
}

/// Resolves the interpolation filter kernel(s) an inter block's motion
/// compensation reads (spec 5.11.10/5.11.20): a fixed-filter frame
/// (`interp_fixed = Some`) never reads a symbol; a `Switchable` frame
/// (`interp_fixed = None`) reads `interp_filter[0]` and, when
/// `enable_dual_filter`, a second `interp_filter[1]` -- unless
/// `force_regular` (this block's own `needs_interp_filter() == 0` case: a
/// `GLOBALMV` block, whose global motion is always `IDENTITY` in this
/// decoder's scope, spec 5.11.10's `large && YMode == GLOBALMV` branch),
/// which forces `EIGHTTAP` without reading anything.
#[allow(clippy::too_many_arguments)]
fn resolve_interp_filter(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    interp_fixed: Option<mc::InterpFilterKind>,
    enable_dual_filter: bool,
    force_regular: bool,
    above: [u8; 2],
    left: [u8; 2],
    is_compound: bool,
) -> (mc::InterpFilterKind, mc::InterpFilterKind, [u8; 2]) {
    if let Some(kind) = interp_fixed {
        return (kind, kind, [3, 3]);
    }
    if force_regular {
        return (
            mc::InterpFilterKind::Regular,
            mc::InterpFilterKind::Regular,
            [0, 0],
        );
    }
    // libaom `av1_extract_interp_filter`/`read_mb_interp_filter`: the
    // symbol read at `dir=0` becomes the block's *vertical* (`y_filter`)
    // kernel, `dir=1` the *horizontal* (`x_filter`) one -- `dir` is not
    // "horizontal then vertical" despite `predict_with_filters`' own
    // `h_kind`/`v_kind` argument order, which is why this function returns
    // `(h, v, ..)` but reads `dir0` (`v`) before `dir1` (`h`).
    let ctx0 = switchable_interp_ctx(above[0], left[0], 0, is_compound);
    if std::env::var_os("EC_AV1_IFDBG").is_some() {
        eprintln!(
            "IFDBG dir=0 ctx={ctx0} above={above:?} left={left:?} cdf={:?}",
            &cdfs.switchable_interp[ctx0][..3]
        );
    }
    let sym0 = dec.symbol(&mut cdfs.switchable_interp[ctx0]) as u8;
    let sym1 = if enable_dual_filter {
        let ctx1 = switchable_interp_ctx(above[1], left[1], 1, is_compound);
        if std::env::var_os("EC_AV1_IFDBG").is_some() {
            eprintln!(
                "IFDBG dir=1 ctx={ctx1} above={above:?} left={left:?} cdf={:?} sym0={sym0}",
                &cdfs.switchable_interp[ctx1][..3]
            );
        }
        dec.symbol(&mut cdfs.switchable_interp[ctx1]) as u8
    } else {
        sym0
    };
    (
        mc::InterpFilterKind::from_switchable_symbol(sym1 as usize),
        mc::InterpFilterKind::from_switchable_symbol(sym0 as usize),
        [sym0, sym1],
    )
}

/// The `is_inter` context (spec 5.11.16 via `av1_get_intra_inter_context`),
/// duplicating [`crate::tile`]'s private copy of the same rule.
pub(crate) fn intra_inter_ctx(
    has_above: bool,
    has_left: bool,
    above_inter: bool,
    left_inter: bool,
) -> usize {
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
    force_integer_mv: bool,
) -> i32 {
    let sign = dec.symbol(&mut c.sign);
    let class = dec.symbol(&mut c.class);
    let local = if class == 0 {
        let bit = dec.symbol(&mut c.class0_bit);
        // spec 5.11.32 `mv_class0_fr`: not coded at all when the frame
        // forces integer motion vectors -- implicitly `3` (the top
        // fractional value) rather than read, else every symbol after it
        // in the tile desyncs by one read.
        let fr = if force_integer_mv {
            3
        } else {
            dec.symbol(&mut c.class0_fr[bit])
        };
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
        // spec 5.11.32 `mv_fr`: same force_integer_mv carve-out as
        // `mv_class0_fr` above.
        let fr = if force_integer_mv { 3 } else { dec.symbol(&mut c.fr) };
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
    force_integer_mv: bool,
) -> (i32, i32) {
    let joint = dec.symbol(mv_joint);
    let mut diff = (0, 0);
    if joint == 2 || joint == 3 {
        diff.0 = read_mv_component(dec, &mut mv_comp[0], allow_high_precision_mv, force_integer_mv);
    }
    if joint == 1 || joint == 3 {
        diff.1 = read_mv_component(dec, &mut mv_comp[1], allow_high_precision_mv, force_integer_mv);
    }
    (pred.0 + diff.0, pred.1 + diff.1)
}

/// One intrabc block's block vector (spec 5.11.13 `read_intrabc_info` ->
/// `assign_mv` with `use_intrabc`, libaom `assign_dv`/`av1_find_ref_dv`).
///
/// The predictor is the ordinary MV stack built against `INTRA_FRAME`
/// (`av1_find_mv_refs(..., INTRA_FRAME, ...)`): `nearestmv`, or `nearmv`
/// when that is zero, or -- when both are -- the fixed fallback DV one
/// superblock up, or `INTRABC_DELAY_PIXELS` (256) plus one superblock to the
/// left when this is the tile's first superblock row. The predictor is then
/// rounded to full pel, and the difference reads off the *dv* nmv context
/// (libaom `ndvc`) at full-pel precision (`MV_SUBPEL_NONE`: no `fr`/`hp`
/// symbols, exactly this decoder's `force_integer_mv` carve-out). No DRL
/// symbol is coded for a DV, so none of this can move the bitstream
/// position -- only the reconstructed vector.
fn read_intrabc_dv(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    mi_r: usize,
    mi_c: usize,
    side: usize,
) -> (i32, i32) {
    const INTRABC_DELAY_PIXELS: i32 = 256;
    let n4 = side / MI;
    let stack_pred = INTRABC_MI_GRID.with(|g| {
        let g = g.borrow();
        g.as_ref().map(|(grid, mi_cols, mi_rows)| {
            let stack = crate::mvstack::find_mv_stack(
                grid, mi_r, mi_c, n4, n4, 0, /* INTRA_FRAME */
                *mi_cols, *mi_rows,
            );
            if stack.nearest_mv == (0, 0) {
                stack.near_mv
            } else {
                stack.nearest_mv
            }
        })
    });
    let mut pred = stack_pred.unwrap_or((0, 0));
    if pred == (0, 0) {
        // `av1_find_ref_dv` (mvref_common.c). Tile row start is 0 here (the
        // single-tile-row case every intrabc gate stream is).
        let sb_px = SB_MI as i32 * MI as i32;
        pred = if mi_r < SB_MI as usize {
            (0, -(sb_px + INTRABC_DELAY_PIXELS) * 8)
        } else {
            (-sb_px * 8, 0)
        };
    }
    // "Ref DV should not have sub-pel" (`assign_dv`): floor to full pel.
    let pred = ((pred.0 >> 3) * 8, (pred.1 >> 3) * 8);
    read_mv(dec, &mut cdfs.dv_comp, &mut cdfs.dv_joint, pred, false, true)
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
        prediction: &[u16],
        residual: &[i32],
    ) {
        self.reconstruct_mc_rect(x, y, side, side, side, prediction, residual);
    }

    /// [`Self::reconstruct_mc`], writing only a `w`x`h` sub-rectangle of the
    /// `side`x`side` `prediction`/`residual` buffers (which stay `side`-square
    /// for stride/indexing) -- lane-partitions r1: a true rectangular HORZ/
    /// VERT strip is predicted at its enclosing square `side` (matching
    /// `HORZ_B`'s already-accepted square-context corner-cut, lane-warp r5)
    /// but only its own true `w`x`h` footprint is committed to the plane, so
    /// two strips sharing a square's coordinate origin never clobber each
    /// other's real pixels.
    fn reconstruct_mc_rect(
        &mut self,
        x: usize,
        y: usize,
        side: usize,
        w: usize,
        h: usize,
        prediction: &[u16],
        residual: &[i32],
    ) {
        for row in 0..h {
            for col in 0..w {
                let sample = (i32::from(prediction[row * side + col]) + residual[row * side + col])
                    .clamp(0, crate::decode::sample_max()) as u16;
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
    prediction: &[u16],
    // lane-chromau: this block's own luma transform's *actual coded*
    // `tx_type` -- `av1_get_tx_type` (libaom `blockd.h`): an inter block's
    // chroma plane never codes its own `tx_type` symbol, but (unlike an
    // intra block, which falls back to `Intra_Mode_To_Tx_Type`) it inherits
    // the colocated luma transform's coded type verbatim, clamped back to
    // `DCT_DCT` at the chroma tx sizes the spec forces DCT-only
    // (`av1_get_ext_tx_set_type`: `tx_size_sqr_up >= TX_32X32`, i.e. this
    // function's own `side >= 32`). `None` for the luma call itself
    // (`plane_idx == 0`), where the `TxbSet`'s own symbol (or the true
    // `DCT_DCT` default at 32-point+) already resolves it without help.
    inherited_luma_tx_type: Option<TxType>,
    // lane-txselect: the luma `txb_skip_ctx` of a transform unit smaller than
    // its own block (spec `get_txb_ctx_general`'s `plane_bsize != tx_size`
    // branch, [`Neighbours::luma_skip_ctx`]) -- `None` for the whole-block
    // transform every non-var-tx caller reads, whose plane-0 context is the
    // lone-transform-unit 0.
    luma_skip_ctx: Option<usize>,
) -> Result<(Vec<i32>, TxType)> {
    let skip_ctx = if plane_idx == 0 {
        luma_skip_ctx.unwrap_or(0)
    } else {
        usize::from(around.0) + usize::from(around.1)
    };
    let mut coding = cdfs.txb(set, tx_mode);
    let default_tx_type = match inherited_luma_tx_type {
        Some(t) if side < 32 => t,
        _ => TxType::DctDct,
    };
    let (grid, tx_type) = read_coeffs(
        dec,
        &mut coding,
        scan,
        skip_ctx,
        dc_sign_ctx(around.2),
        default_tx_type,
        None,
    )?;
    // lane-inter8 r1: a 64-point transform covers the whole 64x64 area but
    // codes only its top-left 32x32 of frequencies (spec 5.11.40) -- the
    // caller hands the 32-point `scan` there, and the coded corner goes into
    // the top-left of the true `side`-sized grid before dequantization
    // (dqDenom is keyed by the true size, not the corner's), mirroring
    // [`read_plane`]'s own `tx_side != side` branch. Every other transform
    // this decoder codes has `tx_side == side`, so this is inert below 64.
    let tx_side = side.min(32);
    let grid = if tx_side == side {
        grid
    } else {
        let mut full = vec![0i32; side * side];
        for row in 0..tx_side {
            full[row * side..][..tx_side].copy_from_slice(&grid[row * tx_side..][..tx_side]);
        }
        full
    };
    let (dc_delta, ac_delta) = plane_q_delta(plane_idx);
    let residual = dequant_and_inverse_typed(&grid, side, bit_depth(), block_q_idx(), dc_delta, ac_delta, tx_type);
    plane.reconstruct_mc(x, y, side, prediction, &residual);
    Ok((grid, tx_type))
}

/// Spec 5.11.25's `single_ref_p1`..`p6` tree (reachable only once `comp_mode`
/// is known `SINGLE_REFERENCE` -- `decode_stream` refuses any frame with
/// `reference_select` set before this is ever called, so that read is never
/// needed here). `above_ref`/`left_ref` are `-1` for an intra/unavailable
/// neighbour, else the `1..=7` reference that neighbour coded (lane-av1refs).
fn read_single_ref(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    above_ref: i8,
    above_ref1: Option<i8>,
    left_ref: i8,
    left_ref1: Option<i8>,
) -> i8 {
    let above = (above_ref > 0).then_some(above_ref);
    let left = (left_ref > 0).then_some(left_ref);
    let p1 = dec.symbol(&mut cdfs.single_ref[single_ref_p1_ctx(above, above_ref1, left, left_ref1)][0]);
    let ref_frame = if p1 == 1 {
        let p2 = dec.symbol(&mut cdfs.single_ref[single_ref_p2_ctx(above, above_ref1, left, left_ref1)][1]);
        if p2 == 1 {
            ALTREF_FRAME
        } else {
            let p6 = dec.symbol(&mut cdfs.single_ref[single_ref_p6_ctx(above, above_ref1, left, left_ref1)][5]);
            if p6 == 1 { ALTREF2_FRAME } else { BWDREF_FRAME }
        }
    } else {
        let p3 = dec.symbol(&mut cdfs.single_ref[single_ref_p3_ctx(above, above_ref1, left, left_ref1)][2]);
        if p3 == 1 {
            let p5 = dec.symbol(&mut cdfs.single_ref[single_ref_p5_ctx(above, above_ref1, left, left_ref1)][4]);
            if p5 == 1 { GOLDEN_FRAME } else { LAST3_FRAME }
        } else {
            let p4 = dec.symbol(&mut cdfs.single_ref[single_ref_p4_ctx(above, above_ref1, left, left_ref1)][3]);
            if p4 == 1 { LAST2_FRAME } else { LAST_FRAME }
        }
    };
    if ref_frame != LAST_FRAME {
        NON_LAST_REF_HITS.with(|c| c.set(c.get() + 1));
        let c = match ref_frame {
            LAST2_FRAME => &LAST2_HITS,
            LAST3_FRAME => &LAST3_HITS,
            GOLDEN_FRAME => &GOLDEN_HITS,
            BWDREF_FRAME => &BWDREF_HITS,
            ALTREF2_FRAME => &ALTREF2_HITS,
            ALTREF_FRAME => &ALTREF_HITS,
            _ => unreachable!("read_single_ref only ever returns LAST_FRAME..=ALTREF_FRAME"),
        };
        c.with(|v| v.set(v.get() + 1));
    }
    ref_frame
}

/// Spec 5.11.25's `comp_mode` (lane-av1comp): whether an inter block reads
/// `SINGLE_REFERENCE` or `COMPOUND_REFERENCE`, only reached once a frame's
/// `reference_select` header bit lets a block choose per-block instead of
/// fixing the mode. `above`/`left` are `None` for an intra/unavailable
/// neighbour, [`NeighbourRef`] for a coded one -- [`reference_mode_ctx`]'s
/// own doc. Called from [`decode_inter_block`] once its own `reference_select`
/// parameter is set; [`decode_inter_block8`]'s 8x8 leaf path gates the same
/// way with a coarser (`LAST_FRAME`-only) neighbour shape.
/// Resolves a compound reference's own planes -- the same `ref_frame ==
/// LAST_FRAME -> ref_y/u/v, else other_refs[ref_frame]` lookup
/// [`decode_inter_block`]'s single-ref path already does, factored out so
/// the compound path can call it twice (lane-av1comp).
fn ref_planes<'a>(
    ref_frame: i8,
    ref_y: &'a PlaneBuf,
    ref_u: &'a PlaneBuf,
    ref_v: &'a PlaneBuf,
    other_refs: &'a RefSlots,
) -> Result<(&'a PlaneBuf, &'a PlaneBuf, &'a PlaneBuf)> {
    if ref_frame == LAST_FRAME {
        Ok((ref_y, ref_u, ref_v))
    } else {
        match other_refs[ref_frame as usize] {
            Some((ry, ru, rv)) => Ok((ry, ru, rv)),
            None => Err(unsupported(
                "a reference frame selected with no picture at this frame's own \
                 ref_frame_idx slot for it",
            )),
        }
    }
}

/// libaom `foreach_overlappable_nb_above` (`obmc.h`): walks the mi row
/// directly above this block's own column span, one bordering block at a
/// time (stepping by that neighbour's own `MiInfo::size`, which is always
/// `<= 16` since no block this decoder ever codes exceeds 64x64), collecting
/// every *inter* neighbour found (an intra or unset cell is skipped, one mi
/// unit at a time, and does not itself count as a step). `max_neighbors`
/// caller-bounds the scan: `1` for the eligibility check (any hit at all),
/// `av1_build_obmc_inter_prediction`'s own `max_neighbor_obmc` table for the
/// real blend pass. Round 1 corner-cut: libaom's own "4-wide block, treat as
/// half of a chroma pair" merge (`obmc.h`'s `mi_step == 1` special case) is
/// not ported -- no block this decoder codes is narrower than 8px, so it
/// never fires here regardless of what a neighbour's own decoder was.
fn overlappable_above(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    mi_cols: usize,
    max_neighbors: usize,
) -> Vec<(usize, usize, MiInfo)> {
    let mut out = Vec::new();
    if mi_row == 0 {
        return out;
    }
    let end_col = (mi_col + bw4).min(mi_cols);
    let mut col = mi_col;
    while col < end_col && out.len() < max_neighbors {
        let cell = grid.get(mi_row - 1, col);
        let step = cell.map_or(1, |c| c.size).max(1);
        if let Some(info) = cell {
            if info.is_inter {
                out.push((col - mi_col, step.min(end_col - col), *info));
            }
        }
        col += step;
    }
    out
}

/// `foreach_overlappable_nb_left`'s left-column mirror of
/// [`overlappable_above`] -- see its doc for the shared corner-cut.
fn overlappable_left(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bh4: usize,
    mi_rows: usize,
    max_neighbors: usize,
) -> Vec<(usize, usize, MiInfo)> {
    let mut out = Vec::new();
    if mi_col == 0 {
        return out;
    }
    let end_row = (mi_row + bh4).min(mi_rows);
    let mut row = mi_row;
    while row < end_row && out.len() < max_neighbors {
        let cell = grid.get(row, mi_col - 1);
        // Vertical walk steps by the neighbour's HEIGHT -- a 32x16 strip's
        // width (8) would swallow the strip below it.
        let step = cell.map_or(1, |c| c.size_h).max(1);
        if let Some(info) = cell {
            if info.is_inter {
                out.push((row - mi_row, step.min(end_row - row), *info));
            }
        }
        row += step;
    }
    out
}

/// `has_top_right` (`av1/common/mvref_common.c`), reduced to the
/// square-block, fixed-64px-superblock case this decoder ever reaches: no
/// rectangular partition (`VERT`/`HORZ`/`_4`/`VERT_A`) ever produces an
/// inter leaf here (`xd->width == xd->height` always), so libaom's own
/// rect-partition branches (`xd->width < xd->height`, `xd->width >
/// xd->height`, `PARTITION_VERT_A`) never fire and are dropped. `bs` is the
/// block's own side in 4x4 (`mi`) units.
fn has_top_right(mi_row: usize, mi_col: usize, bs: usize) -> bool {
    let sb_mi_size = SB_MI as usize;
    let mask_row = mi_row & (sb_mi_size - 1);
    let mask_col = mi_col & (sb_mi_size - 1);
    if bs > sb_mi_size {
        return false;
    }
    let mut has_tr = !((mask_row & bs != 0) && (mask_col & bs != 0));
    let mut b = bs;
    while b < sb_mi_size {
        if mask_col & b != 0 {
            if (mask_col & (2 * b) != 0) && (mask_row & (2 * b) != 0) {
                has_tr = false;
                break;
            }
        } else {
            break;
        }
        b <<= 1;
    }
    has_tr
}

/// `av1_findSamples` (`mvref_common.c`), full port: the up to
/// `LEAST_SQUARES_SAMPLES_MAX` (8) above/left/top-left/top-right neighbour
/// samples (`record_samples`) that share this block's own single reference
/// frame and are themselves single-ref -- `mbmi->num_proj_ref` is
/// `.len()` of the returned vec. `bw4` is the block's own side in 4x4 units
/// (square, so also `bh4`; a real block's `xd->width`/`xd->height` in
/// libaom are also mi units of that same square side here). Each sample's
/// own `bw`/`bh` (fed to [`crate::warp::record_sample`]'s offset formula)
/// come from the *neighbour* cell's own coded size, not this block's.
#[allow(clippy::too_many_arguments)]
fn find_samples(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    bh4: usize,
    mi_cols: usize,
    mi_rows: usize,
    ref_frame: i8,
) -> Vec<crate::warp::Sample> {
    const MAX: usize = crate::warp::LEAST_SQUARES_SAMPLES_MAX;
    let up_available = mi_row > 0;
    let left_available = mi_col > 0;
    let mut samples = Vec::with_capacity(MAX);
    let single_ref_match = |info: &MiInfo| info.ref_frame == ref_frame && info.ref_frame1.is_none();
    let mut do_tl = true;
    let mut do_tr = true;
    let rec = |info: &MiInfo, row_offset: i32, sign_r: i32, col_offset: i32, sign_c: i32| {
        let nb_bw = (info.size * 4) as i32;
        // lane-rect r2: the neighbour's own true height -- a rect strip
        // neighbour donates its sample at ITS center (aom pts y=-72 for a
        // 32x16 above strip; the square assumption put it at -136).
        let nb_bh = (info.size_h * 4) as i32;
        crate::warp::record_sample(nb_bw, nb_bh, info.mv, row_offset, sign_r, col_offset, sign_c)
    };

    'outer: {
        if up_available {
            let above = grid.get(mi_row - 1, mi_col);
            let superblock_width = above.map_or(1, |c| c.size).max(1);
            if bw4 <= superblock_width {
                let col_offset = -((mi_col % superblock_width) as isize);
                if col_offset < 0 {
                    do_tl = false;
                }
                if col_offset + superblock_width as isize > bw4 as isize {
                    do_tr = false;
                }
                if let Some(info) = above {
                    if single_ref_match(info) {
                        samples.push(rec(info, 0, -1, col_offset as i32, 1));
                        if samples.len() >= MAX {
                            break 'outer;
                        }
                    }
                }
            } else {
                let mut i = 0usize;
                let limit = bw4.min(mi_cols.saturating_sub(mi_col));
                while i < limit {
                    let cell = grid.get(mi_row - 1, mi_col + i);
                    let sw = cell.map_or(1, |c| c.size).max(1);
                    if let Some(info) = cell {
                        if single_ref_match(info) {
                            samples.push(rec(info, 0, -1, i as i32, 1));
                            if samples.len() >= MAX {
                                break 'outer;
                            }
                        }
                    }
                    i += sw;
                }
            }
        }

        if left_available {
            let left = grid.get(mi_row, mi_col - 1);
            let superblock_height = left.map_or(1, |c| c.size_h).max(1);
            if bh4 <= superblock_height {
                let row_offset = -((mi_row % superblock_height) as isize);
                if row_offset < 0 {
                    do_tl = false;
                }
                if let Some(info) = left {
                    if single_ref_match(info) {
                        samples.push(rec(info, row_offset as i32, 1, 0, -1));
                        if samples.len() >= MAX {
                            break 'outer;
                        }
                    }
                }
            } else {
                let mut i = 0usize;
                let limit = bh4.min(mi_rows.saturating_sub(mi_row));
                while i < limit {
                    let cell = grid.get(mi_row + i, mi_col - 1);
                    let sh = cell.map_or(1, |c| c.size_h).max(1);
                    if let Some(info) = cell {
                        if single_ref_match(info) {
                            samples.push(rec(info, i as i32, 1, 0, -1));
                            if samples.len() >= MAX {
                                break 'outer;
                            }
                        }
                    }
                    i += sh;
                }
            }
        }

        if do_tl && left_available && up_available {
            if let Some(info) = grid.get(mi_row - 1, mi_col - 1) {
                if single_ref_match(info) {
                    samples.push(rec(info, 0, -1, 0, -1));
                    if samples.len() >= MAX {
                        break 'outer;
                    }
                }
            }
        }

        if do_tr && has_top_right(mi_row, mi_col, bw4) {
            let tr_col = mi_col + bw4;
            if mi_row > 0 && tr_col < mi_cols {
                if let Some(info) = grid.get(mi_row - 1, tr_col) {
                    if single_ref_match(info) {
                        samples.push(rec(info, 0, -1, bw4 as i32, 1));
                    }
                }
            }
        }
    }
    samples
}

/// `mbmi->num_proj_ref`: [`find_samples`]'s own count, all that
/// `motion_mode_allowed` needs to pick the 3-symbol `motion_mode_cdf` over
/// the 2-symbol `obmc_cdf` (spec 5.11.24).
#[allow(clippy::too_many_arguments)]
fn num_proj_ref(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    bh4: usize,
    mi_cols: usize,
    mi_rows: usize,
    ref_frame: i8,
) -> u8 {
    find_samples(grid, mi_row, mi_col, bw4, bh4, mi_cols, mi_rows, ref_frame).len() as u8
}

/// `av1_get_obmc_mask` (`reconinter.c`): the per-position blend weight
/// (`AOM_BLEND_A64` numerator, out of 64) for the neighbour's own
/// contribution's *complement* -- i.e. `dst = (mask*orig + (64-mask)*nbr +
/// 32) >> 6`, low near the shared border (heavier neighbour weight) rising
/// to 64 (no neighbour contribution) away from it. Only the lengths this
/// decoder's own overlap sizes ever produce (luma 8/16/32, chroma 4/8/16)
/// are transcribed.
fn obmc_mask(len: usize) -> &'static [u8] {
    // lane-gmaffine r1: the two SHORT masks (libaom `reconinter.c`
    // `obmc_mask_1`/`obmc_mask_2`) -- an 8x8 leaf's chroma plane is 4x4, so
    // its overlap is 2 (and 1 at a half-block edge). Unreachable until this
    // round wired interior 16x16 splits down to real 8x8 leaves, where the
    // old `unreachable!()` below fired as a hard panic.
    const M1: [u8; 1] = [64];
    // lane-inter8 r1: `obmc_mask_2` (`reconinter.c` 753) -- reached by the
    // LEFT pass of an 8x8 block's chroma plane (overlap 4 luma columns, 2
    // chroma), which only became live once 8x8 inter leaves stopped being
    // refused.
    const M2: [u8; 2] = [45, 64];
    const M4: [u8; 4] = [39, 50, 59, 64];
    const M8: [u8; 8] = [36, 42, 48, 53, 57, 61, 64, 64];
    const M16: [u8; 16] = [
        34, 37, 40, 43, 46, 49, 52, 54, 56, 58, 60, 61, 64, 64, 64, 64,
    ];
    const M32: [u8; 32] = [
        33, 35, 36, 38, 40, 41, 43, 44, 45, 47, 48, 50, 51, 52, 53, 55, 56, 57, 58, 59, 60, 60,
        61, 62, 64, 64, 64, 64, 64, 64, 64, 64,
    ];
    match len {
        1 => &M1,
        2 => &M2,
        4 => &M4,
        8 => &M8,
        16 => &M16,
        32 => &M32,
        _ => unreachable!("obmc overlap length outside this decoder's own block-size range"),
    }
}

/// A bordering block's own resolved interp filter kernel, `(h, v)` --
/// mirrors [`resolve_interp_filter`]'s own return order from its stored
/// `[u8; 2]` (`Neighbours::above_filter`/`left_filter`, `[sym0, sym1]` =
/// `[v_sym, h_sym]`). Never calls `from_switchable_symbol` on the `[3, 3]`
/// sentinel: that value is only ever stored for an intra neighbour (already
/// excluded by [`overlappable_above`]/[`overlappable_left`]'s `is_inter`
/// gate) or when `interp_fixed` is `Some` for the whole frame, handled here
/// first.
fn neighbour_filter(
    interp_fixed: Option<mc::InterpFilterKind>,
    sym: [u8; 2],
) -> (mc::InterpFilterKind, mc::InterpFilterKind) {
    if let Some(kind) = interp_fixed {
        return (kind, kind);
    }
    (
        mc::InterpFilterKind::from_switchable_symbol(sym[1] as usize),
        mc::InterpFilterKind::from_switchable_symbol(sym[0] as usize),
    )
}

/// One neighbour's own re-prediction over a `w`x`h` region, MC'd fresh
/// against its own `mv`/reference/filter (libaom `av1_build_inter_predictor`
/// under `av1_setup_build_prediction_by_{above,left}_pred`).
#[allow(clippy::too_many_arguments)]
fn obmc_neighbour_pred(
    refplane: &PlaneBuf,
    x: usize,
    y: usize,
    mv: (i32, i32),
    w: usize,
    h: usize,
    luma: bool,
    h_kind: mc::InterpFilterKind,
    v_kind: mc::InterpFilterKind,
    // lane-scaledref r1: spec 7.11.3.3's x_scale_fp for THIS neighbour's own
    // reference (derived from luma widths, applied unchanged to chroma whose
    // x_q4 is already in its own plane's pixel units). `REF_NO_SCALE` takes
    // the ordinary stride-1 path bit-exact.
    x_scale_fp: i64,
) -> Vec<u16> {
    let mut out = vec![0u16; w * h];
    if x_scale_fp == mc::REF_NO_SCALE {
        mc::predict_with_filters(
            &refplane.data,
            refplane.width,
            refplane.true_width,
            refplane.true_height,
            mv_to_q4(x, mv.1, luma),
            mv_to_q4(y, mv.0, luma),
            w,
            h,
            h_kind,
            v_kind,
            &mut out,
        );
    } else {
        mc::predict_scaled(
            &refplane.data,
            refplane.width,
            refplane.true_width,
            refplane.true_height,
            mv_to_q4(x, mv.1, luma),
            mv_to_q4(y, mv.0, luma),
            x_scale_fp,
            w,
            h,
            h_kind,
            v_kind,
            &mut out,
        );
    }
    out
}

/// Blends `tmp` into `dst` (stride `stride`) at `(ox, oy)`, mask varying by
/// row (the above-neighbour pass, `aom_blend_a64_vmask`).
/// `ii_weights1d` (reconinter.c:524): the interintra smooth-mask falloff.
const II_WEIGHTS_1D: [u8; 128] = [
    60, 58, 56, 54, 52, 50, 48, 47, 45, 44, 42, 41, 39, 38, 37, 35, 34, 33, 32,
    31, 30, 29, 28, 27, 26, 25, 24, 23, 22, 22, 21, 20, 19, 19, 18, 18, 17, 16,
    16, 15, 15, 14, 14, 13, 13, 12, 12, 12, 11, 11, 10, 10, 10, 9, 9, 9, 8,
    8, 8, 8, 7, 7, 7, 7, 6, 6, 6, 6, 6, 5, 5, 5, 5, 5, 4, 4,
    4, 4, 4, 4, 4, 4, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2, 2, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
];

/// lane-interintra r1: non-wedge interintra prediction for one plane
/// (reconinter.c `av1_build_interintra_predictor` + `combine_interintra` +
/// `build_smooth_interintra_mask`). Builds the intra predictor for the
/// block's II mode (DC/V/H/SMOOTH, angle 0, no filter-intra) from `plane`'s
/// own decoded edges at `(x, y)`, then blends it over the inter prediction
/// in `pred`: `comp = (mask*intra + (64-mask)*inter + 32) >> 6`
/// (`aom_blend_a64_mask`, intra as src0). `ii_size_scales[plane_bsize]`
/// reduces to `128 / side` for the square plane sizes this decoder reaches.
/// lane-wii r2: `wedge == Some(..)` replaces the mode mask with the wedge
/// codebook mask (fixed sign 0; luma stride, 2x2 box-averaged for chroma).
fn interintra_blend(
    plane: &PlaneBuf,
    x: usize,
    y: usize,
    side: usize,
    ii_mode: u8,
    wedge: Option<(&'static [u8], usize)>,
    pred: &mut [u16],
) {
    // `interintra_to_intra_mode`: II_DC/II_V/II_H/II_SMOOTH.
    let intra_mode = match ii_mode {
        0 => crate::intra::DC_PRED,
        1 => crate::intra::V_PRED,
        2 => crate::intra::H_PRED,
        _ => crate::intra::SMOOTH_PRED,
    };
    let (above, left, corner) = plane.edges(
        x,
        y,
        side,
        crate::encode::Reach {
            above_right: false,
            below_left: false,
        },
    );
    let mut intra = vec![0u16; side * side];
    // Edge filtering never applies here: V/H at delta 0 are plain edge
    // copies (angle 90/180 skip the directional walk), DC/SMOOTH carry no
    // angle at all.
    predict(
        intra_mode,
        0,
        above.as_deref(),
        left.as_deref(),
        corner,
        side,
        side,
        false,
        false,
        &mut intra,
    );
    let scale = 128 / side;
    for i in 0..side {
        for j in 0..side {
            let m = u32::from(match wedge {
                // lane-wii r2: wedge-interintra mask (aom_blend_a64_mask
                // over the wedge codebook, intra as src0, fixed sign 0).
                // Luma plane: mask rows are luma-resolution, ms == side.
                // 4:2:0 chroma: 2x2 box average (reconinter.c sub8x8 --
                // subw == subh == 1).
                Some((mask, ms)) if ms == side => mask[i * side + j],
                Some((mask, ms)) => {
                    let t = 2 * i * ms + 2 * j;
                    ((u32::from(mask[t])
                        + u32::from(mask[t + 1])
                        + u32::from(mask[t + ms])
                        + u32::from(mask[t + ms + 1])
                        + 2)
                        >> 2) as u8
                }
                None => match ii_mode {
                    0 => 32,
                    1 => II_WEIGHTS_1D[i * scale],
                    2 => II_WEIGHTS_1D[j * scale],
                    _ => II_WEIGHTS_1D[i.min(j) * scale],
                },
            });
            let idx = i * side + j;
            pred[idx] =
                ((m * u32::from(intra[idx]) + (64 - m) * u32::from(pred[idx]) + 32) >> 6) as u16;
        }
    }
}

fn obmc_blend_v(dst: &mut [u16], stride: usize, ox: usize, oy: usize, w: usize, h: usize, tmp: &[u16]) {
    let mask = obmc_mask(h);
    for row in 0..h {
        let m = u32::from(mask[row]);
        for col in 0..w {
            let d = &mut dst[(oy + row) * stride + ox + col];
            let t = u32::from(tmp[row * w + col]);
            *d = ((m * u32::from(*d) + (64 - m) * t + 32) >> 6) as u16;
        }
    }
}

/// Blends `tmp` into `dst` at `(ox, oy)`, mask varying by column (the
/// left-neighbour pass, `aom_blend_a64_hmask`).
fn obmc_blend_h(dst: &mut [u16], stride: usize, ox: usize, oy: usize, w: usize, h: usize, tmp: &[u16]) {
    let mask = obmc_mask(w);
    for row in 0..h {
        for col in 0..w {
            let m = u32::from(mask[col]);
            let d = &mut dst[(oy + row) * stride + ox + col];
            let t = u32::from(tmp[row * w + col]);
            *d = ((m * u32::from(*d) + (64 - m) * t + 32) >> 6) as u16;
        }
    }
}

/// `av1_build_obmc_inter_prediction` (libaom `reconinter.c`): re-predicts
/// the overlap strip against each bordering above/left inter neighbour's own
/// mv/ref/filter and blends it into this block's just-built single-reference
/// prediction, luma then both 4:2:0 chroma planes -- above pass first, then
/// left, each reading only the *original* prediction this function was
/// handed (never the other pass's own output), matching libaom's own
/// independence between the two. `max_neighbor_obmc`'s own cap table
/// (`{0,1,2,3,4,4}` indexed by `mi_size_wide_log2`/`_high_log2`, this
/// decoder's own square blocks always index the same value both ways) bounds
/// how many bordering blocks actually get blended, separate from the
/// eligibility scan's own unbounded ("any at all") walk.
#[allow(clippy::too_many_arguments)]
fn obmc_blend(
    grid: &MiGrid,
    neighbours: &Neighbours,
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    bh4: usize,
    mi_rows: usize,
    mi_cols: usize,
    side: usize,
    write_w: usize,
    write_h: usize,
    chroma_side: usize,
    px: usize,
    py: usize,
    cpx: usize,
    cpy: usize,
    ref_y: &PlaneBuf,
    ref_u: &PlaneBuf,
    ref_v: &PlaneBuf,
    other_refs: &RefSlots,
    interp_fixed: Option<mc::InterpFilterKind>,
    // lane-scaledref r1: this frame's own coded luma width -- each OBMC
    // neighbour is re-predicted against ITS OWN reference, which may be
    // scaled differently from this block's (spec 7.11.3.3 / libaom
    // `av1_setup_build_prediction_by_above_pred` passing that ref's own
    // `scale_factors`).
    frame_width: usize,
    pred_y: &mut [u16],
    pred_u: &mut [u16],
    pred_v: &mut [u16],
) -> Result<()> {
    // lane-rect r2 (libaom av1_build_obmc_inter_prediction): each pass has
    // its OWN neighbour cap (width-log2 for above, height-log2 for left) and
    // overlap (bh/2 rows above, bw/2 cols left) -- one square `side` was
    // right only for square blocks.
    let max_nb = |n4: usize| match n4 {
        1..=4 => 2,
        5..=8 => 3,
        _ => 4,
    };
    let overlap_above = write_h / 2;
    let overlap_left = write_w / 2;
    // lane-inter8 r1 / lane-scaledref r1: `av1_skip_u4x4_pred_in_obmc`
    // (`reconinter.c` 820, `DISABLE_CHROMA_U8X8_OBMC == 0` -- "one-sided
    // obmc") returns `dir == 0` when the chroma plane's own block is
    // BLOCK_4X4/8X4/4X8 (an 8x8, 16x8 or 8x16 luma block at 4:2:0): the
    // ABOVE pass skips chroma entirely, the LEFT pass still blends it.
    let skip_chroma_above = matches!((write_w, write_h), (8, 8) | (16, 8) | (8, 16));

    for (off4, span4, nb) in overlappable_above(grid, mi_row, mi_col, bw4, mi_cols, max_nb(bw4))
    {
        // `Neighbours::above_filter` is indexed in `SUB`(16px)-wide columns
        // (this decoder's own outer block-loop granularity), one step
        // coarser than the mi(4px) units `overlappable_above` walks in --
        // divide back down to that column.
        // lane-gmaffine r2: the neighbour's OWN mi-granular filter
        // (libaom `av1_setup_build_prediction_by_above_pred` reads
        // `above_mbmi->interp_filters`); the 16px-granular `Neighbours` slot
        // stays as the fallback for paths that do not record one yet
        // (compound), which is what every earlier round used.
        let (h_kind, v_kind) = neighbour_filter(
            interp_fixed,
            neighbours.above_filter[mi_col + off4],
        );
        let (ny, nu, nv) = ref_planes(nb.ref_frame, ref_y, ref_u, ref_v, other_refs)?;
        let nb_scale = mc::scale_factor(ny.width, frame_width);
        let (bw, bh, ox) = (span4 * 4, overlap_above, off4 * 4);
        let tmp_y = obmc_neighbour_pred(ny, px + ox, py, nb.mv, bw, bh, true, h_kind, v_kind, nb_scale);
        obmc_blend_v(pred_y, side, ox, 0, bw, bh, &tmp_y);
        if !skip_chroma_above {
            let (cbw, cbh, cox) = (bw / 2, overlap_above / 2, ox / 2);
            let tmp_u = obmc_neighbour_pred(nu, cpx + cox, cpy, nb.mv, cbw, cbh, false, h_kind, v_kind, nb_scale);
            obmc_blend_v(pred_u, chroma_side, cox, 0, cbw, cbh, &tmp_u);
            let tmp_v = obmc_neighbour_pred(nv, cpx + cox, cpy, nb.mv, cbw, cbh, false, h_kind, v_kind, nb_scale);
            obmc_blend_v(pred_v, chroma_side, cox, 0, cbw, cbh, &tmp_v);
        }
    }

    for (off4, span4, nb) in overlappable_left(grid, mi_row, mi_col, bh4, mi_rows, max_nb(bh4)) {
        let (h_kind, v_kind) = neighbour_filter(
            interp_fixed,
            neighbours.left_filter[mi_row + off4],
        );
        let (ny, nu, nv) = ref_planes(nb.ref_frame, ref_y, ref_u, ref_v, other_refs)?;
        let nb_scale = mc::scale_factor(ny.width, frame_width);
        let (bw, bh, oy) = (overlap_left, span4 * 4, off4 * 4);
        let tmp_y = obmc_neighbour_pred(ny, px, py + oy, nb.mv, bw, bh, true, h_kind, v_kind, nb_scale);
        obmc_blend_h(pred_y, side, 0, oy, bw, bh, &tmp_y);
        let (cbw, cbh, coy) = (overlap_left / 2, bh / 2, oy / 2);
        let tmp_u = obmc_neighbour_pred(nu, cpx, cpy + coy, nb.mv, cbw, cbh, false, h_kind, v_kind, nb_scale);
        obmc_blend_h(pred_u, chroma_side, 0, coy, cbw, cbh, &tmp_u);
        let tmp_v = obmc_neighbour_pred(nv, cpx, cpy + coy, nb.mv, cbw, cbh, false, h_kind, v_kind, nb_scale);
        obmc_blend_h(pred_v, chroma_side, 0, coy, cbw, cbh, &tmp_v);
    }
    Ok(())
}

/// `is_any_masked_compound_used` (libaom `reconinter.h`), specialised to
/// the square block sizes this decoder ever decodes (32/16/8, all
/// `min(w,h) >= 8`, `is_comp_ref_allowed` true): `COMPOUND_DIFFWTD` is then
/// always `is_interinter_compound_used`, so the function is always `true`
/// for `side` -- kept as a named call (rather than inlined `true`) so a
/// future sub-8 block size does not silently misdecode a real stream.
fn is_any_masked_compound_used_here(side: usize) -> bool {
    side.min(side) >= 8
}

/// Whether a compound reference pair is unidirectional (both references on
/// the same temporal side of the current frame) -- `has_uni_comp_refs`
/// (libaom `av1_reference_frame_utils.h`/`pred_common.c`'s own definition:
/// a pair is `UNIDIR_COMP_REFERENCE` exactly when neither reference is
/// backward-while-the-other-is-forward, i.e. `is_backward(ref0) ==
/// is_backward(ref1)`). Used to build a real
/// [`crate::mvstack::NeighbourRef::uni`] once `ref1` is known, matching
/// [`crate::mvstack::comp_reference_type_ctx`]'s own `is_backward` calls.
fn is_uni_comp_ref(ref0: i8, ref1: i8) -> bool {
    let backward = |r: i8| (BWDREF_FRAME..=ALTREF_FRAME).contains(&r);
    backward(ref0) == backward(ref1)
}

/// `get_comp_group_idx_context` (libaom `pred_common.h`): sums the
/// above/left neighbour contributions [`Neighbours::record_inter`]/
/// [`Neighbours::record_compound_ctx`] already precomputed, clamped to 5.
/// lane-inter8 r2: `at_mi` is the block's own top-left in 4x4 mi units --
/// an 8x8 leaf used to pass its enclosing 16x16's corner here, so all four
/// leaves read the same neighbour cell (class context-read-from-one-cell).
fn get_comp_group_idx_context(
    neighbours: &Neighbours,
    at_mi: (usize, usize),
    side: usize,
) -> usize {
    let _ = side;
    let (rmi, cmi) = at_mi;
    (neighbours.above_comp_group_idx[cmi] as usize + neighbours.left_comp_group_idx[rmi] as usize)
        .min(5)
}

/// `get_comp_index_context` (libaom `pred_common.h`): the above/left
/// neighbour bits plus `3 * (fwd == bck)`, where `fwd`/`bck` are this
/// block's own two references' distances from the current frame (spec
/// 7.11.3.15's `get_relative_dist`, same convention
/// [`crate::compound::dist_wtd_comp_weight_assign`] uses).
#[allow(clippy::too_many_arguments)]
fn get_comp_index_context(
    neighbours: &Neighbours,
    at_mi: (usize, usize),
    side: usize,
    order_hint_bits: u32,
    cur_order_hint: u32,
    ref0_order_hint: u32,
    ref1_order_hint: u32,
) -> usize {
    let _ = side;
    let (rmi, cmi) = at_mi;
    let fwd =
        crate::motion_field::get_relative_dist(order_hint_bits, ref1_order_hint, cur_order_hint)
            .abs();
    let bck =
        crate::motion_field::get_relative_dist(order_hint_bits, cur_order_hint, ref0_order_hint)
            .abs();
    let offset = usize::from(fwd == bck);
    neighbours.above_compound_idx[cmi] as usize
        + neighbours.left_compound_idx[rmi] as usize
        + 3 * offset
}

fn read_comp_mode(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    above: Option<NeighbourRef>,
    left: Option<NeighbourRef>,
) -> bool {
    let ctx = reference_mode_ctx(above, left);
    let compound = dec.symbol(&mut cdfs.comp_mode[ctx]) == 1;
    if compound {
        COMP_MODE_HITS.with(|c| c.set(c.get() + 1));
    }
    compound
}

/// Spec 5.11.25's `comp_reference_type` + the `uni_comp_ref`/`comp_ref`/
/// `comp_bwdref` trees (libaom `decodemv.c`'s `read_ref_frames` compound
/// arm), reached once [`read_comp_mode`] has already returned
/// `COMPOUND_REFERENCE`. Returns `(RefFrame[0], RefFrame[1])`.
/// `above`/`left` are [`comp_reference_type_ctx`]'s own `NeighbourRef`
/// convention; `above_ref`/`left_ref` are the scalar `1..=7`/`-1`
/// convention [`read_single_ref`] already uses, for every downstream binary
/// decision. libaom `pred_common.c`'s own compound context functions
/// (`av1_get_pred_context_uni_comp_ref_p[1|2]`, `comp_ref_p[1|2]`,
/// `comp_bwdref_p[1]`) turned out to be exactly [`single_ref_p1_ctx`]..
/// [`single_ref_p6_ctx`]/[`uni_comp_ref_p1_ctx`] under a different name
/// (same vote-the-neighbours-above-left shape, same forward/backward and
/// LAST/LAST2/LAST3/GOLDEN groupings) -- lane-av1comp audited each one
/// against the C source rather than porting six near-duplicate functions.
/// Its caller today only needs the symbols consumed, correctly, to keep the
/// arithmetic decoder's own state in sync before refusing the block by name
/// -- the returned pair is not yet threaded into any motion-compensation
/// path.
fn read_compound_ref_frames(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    above: Option<NeighbourRef>,
    left: Option<NeighbourRef>,
    above_ref: i8,
    left_ref: i8,
) -> (i8, i8) {
    let a = (above_ref > 0).then_some(above_ref);
    let l = (left_ref > 0).then_some(left_ref);
    // libaom `av1_collect_neighbors_ref_counts` counts BOTH of a compound
    // neighbour's references, not just its first -- `a1`/`l1` carry that
    // second reference through to every vote below (lane-av1blend r6: a
    // missing `a1`/`l1` here was decoding `compound_idx` off the wrong CDF
    // row for a block sitting under a skip_mode-forced compound neighbour).
    let a1 = above.and_then(|n| n.ref1);
    let l1 = left.and_then(|n| n.ref1);
    let type_ctx = comp_reference_type_ctx(above, left);
    let unidir = dec.symbol(&mut cdfs.comp_ref_type[type_ctx]) == 0;
    if unidir {
        // uni_comp_ref (p0): forward vs. backward -- av1_get_pred_context_-
        // uni_comp_ref_p is single_ref_p1's own forward/backward vote.
        let bit = dec.symbol(&mut cdfs.uni_comp_ref[single_ref_p1_ctx(a, a1, l, l1)][0]);
        if bit == 1 {
            (BWDREF_FRAME, ALTREF_FRAME)
        } else {
            let p1 = dec.symbol(&mut cdfs.uni_comp_ref[uni_comp_ref_p1_ctx(a, a1, l, l1)][1]);
            if p1 == 1 {
                // uni_comp_ref_p2: LAST3 vs. GOLDEN, same vote as single_ref_p5.
                let p2 = dec.symbol(&mut cdfs.uni_comp_ref[single_ref_p5_ctx(a, a1, l, l1)][2]);
                if p2 == 1 {
                    (LAST_FRAME, GOLDEN_FRAME)
                } else {
                    (LAST_FRAME, LAST3_FRAME)
                }
            } else {
                (LAST_FRAME, LAST2_FRAME)
            }
        }
    } else {
        // comp_ref (p0): LAST/LAST2 vs. LAST3/GOLDEN, same vote as single_ref_p3.
        let bit0 = dec.symbol(&mut cdfs.comp_ref[single_ref_p3_ctx(a, a1, l, l1)][0]);
        let ref0 = if bit0 == 0 {
            // comp_ref_p1: LAST vs. LAST2, same vote as single_ref_p4.
            let p1 = dec.symbol(&mut cdfs.comp_ref[single_ref_p4_ctx(a, a1, l, l1)][1]);
            if p1 == 1 { LAST2_FRAME } else { LAST_FRAME }
        } else {
            // comp_ref_p2: LAST3 vs. GOLDEN, same vote as single_ref_p5.
            let p2 = dec.symbol(&mut cdfs.comp_ref[single_ref_p5_ctx(a, a1, l, l1)][2]);
            if p2 == 1 { GOLDEN_FRAME } else { LAST3_FRAME }
        };
        // comp_bwdref (p0): BWDREF/ALTREF2 vs. ALTREF, same vote as single_ref_p2.
        let bit1 = dec.symbol(&mut cdfs.comp_bwdref[single_ref_p2_ctx(a, a1, l, l1)][0]);
        let ref1 = if bit1 == 0 {
            // comp_bwdref_p1: BWDREF vs. ALTREF2, same vote as single_ref_p6.
            let p1 = dec.symbol(&mut cdfs.comp_bwdref[single_ref_p6_ctx(a, a1, l, l1)][1]);
            if p1 == 1 { ALTREF2_FRAME } else { BWDREF_FRAME }
        } else {
            ALTREF_FRAME
        };
        (ref0, ref1)
    }
}

/// Spec 5.11.24's `compound_mode` (lane-av1comp): which of the eight
/// `INTER_COMPOUND_MODES` (`NEAREST_NEARESTMV`..`NEW_NEWMV`, returned as
/// `0..=7`) a `COMPOUND_REFERENCE` block takes, reached once
/// [`read_comp_mode`] has already returned `COMPOUND_REFERENCE`. `ctx` is
/// libaom's `av1_mode_context_analyzer`'s compound branch --
/// `compound_mode_ctx_map[ref_mv_ctx >> 1][min(new_mv_ctx, COMP_NEWMV_CTXS
/// - 1)]` (`mvref_common.h`) -- folded from [`crate::mvstack::
/// find_mv_stack_compound`]'s own `new_mv_ctx`/`ref_mv_ctx`. The read symbol
/// is not yet turned into an assigned MV pair or motion-compensated --
/// lane-av1comp.
fn read_inter_compound_mode(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    new_mv_ctx: usize,
    ref_mv_ctx: usize,
) -> u8 {
    let ctx = cdf::COMPOUND_MODE_CTX_MAP[ref_mv_ctx >> 1][new_mv_ctx.min(4)];
    COMPOUND_MODE_HITS.with(|c| c.set(c.get() + 1));
    dec.symbol(&mut cdfs.inter_compound_mode[ctx]) as u8
}

/// spec 7.10.2.10 `assign_mv`'s compound branch, plus the compound half of
/// `read_drl_idx` (spec 5.11.24) it depends on -- `mode` is
/// [`read_inter_compound_mode`]'s own `0..=7` (`NEAREST_NEARESTMV`..
/// `NEW_NEWMV`, libaom `enums.h` order). Every `GLOBAL_GLOBALMV` side reads
/// as `(0, 0)` since [`crate::stream::decode_stream`] already refuses any
/// non-`IDENTITY` global motion model before a tile is ever decoded.
///
/// Ported straight from libaom `decodemv.c`'s `read_drl_idx` (the
/// `NEW_NEWMV`/`have_nearmv_in_inter_mode` branches) and `assign_mv`/its
/// caller's `ref_mv[]`/`nearmv[]` setup: the `NEAR_NEWMV`/`NEW_NEARMV`
/// `1 + ref_mv_idx` special case is folded into `new_idx` below, matching
/// `read_drl_idx`'s own comment about "offsetting the NEARESTMV mode".
fn assign_compound_mv(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    comp_stack: &crate::mvstack::CompoundMvStack,
    mode: u8,
    allow_high_precision_mv: bool,
    force_integer_mv: bool,
    // lane-gm r2: `gm_get_motion_vector(ref0)`/`(ref1)` at this block's own
    // position, spec 7.10.2.10's `GLOBAL_GLOBALMV` assignment.
    global_mv: ((i32, i32), (i32, i32)),
) -> ((i32, i32), (i32, i32)) {
    const NEAREST_NEARESTMV: u8 = 0;
    const NEAR_NEARMV: u8 = 1;
    const NEAREST_NEWMV: u8 = 2;
    const NEW_NEARESTMV: u8 = 3;
    const NEAR_NEWMV: u8 = 4;
    const NEW_NEARMV: u8 = 5;
    const GLOBAL_GLOBALMV: u8 = 6;
    const NEW_NEWMV: u8 = 7;

    let mut ref_mv_idx = 0usize;
    if mode == NEW_NEWMV {
        let mut idx = 0usize;
        while idx < 2 && comp_stack.entries.len() > idx + 1 {
            if dec.symbol(&mut cdfs.drl_mode[comp_stack.drl_ctx[idx]]) == 0 {
                break;
            }
            idx += 1;
        }
        ref_mv_idx = idx;
    } else if matches!(mode, NEAR_NEARMV | NEAR_NEWMV | NEW_NEARMV) {
        let mut idx = 1usize;
        while idx < 3 && comp_stack.entries.len() > idx + 1 {
            if dec.symbol(&mut cdfs.drl_mode[comp_stack.drl_ctx[idx]]) == 0 {
                break;
            }
            idx += 1;
        }
        ref_mv_idx = idx - 1;
    }

    let near = comp_stack
        .entries
        .get(ref_mv_idx + 1)
        .map_or(comp_stack.near_mv, |e| (e.mv0, e.mv1));
    let new_idx = if matches!(mode, NEAR_NEWMV | NEW_NEARMV) {
        ref_mv_idx + 1
    } else {
        ref_mv_idx
    };
    let new_pred = comp_stack
        .entries
        .get(new_idx)
        .map_or(comp_stack.nearest_mv, |e| (e.mv0, e.mv1));

    let read_new = |dec: &mut SymbolDecoder, cdfs: &mut Cdfs, base: (i32, i32)| {
        read_mv(
            dec,
            &mut cdfs.mv_comp,
            &mut cdfs.mv_joint,
            base,
            allow_high_precision_mv,
            force_integer_mv,
        )
    };

    match mode {
        NEAREST_NEARESTMV => comp_stack.nearest_mv,
        NEAR_NEARMV => near,
        NEAREST_NEWMV => (comp_stack.nearest_mv.0, read_new(dec, cdfs, new_pred.1)),
        NEW_NEARESTMV => (read_new(dec, cdfs, new_pred.0), comp_stack.nearest_mv.1),
        NEAR_NEWMV => (near.0, read_new(dec, cdfs, new_pred.1)),
        NEW_NEARMV => (read_new(dec, cdfs, new_pred.0), near.1),
        GLOBAL_GLOBALMV => global_mv,
        _ => (
            read_new(dec, cdfs, new_pred.0),
            read_new(dec, cdfs, new_pred.1),
        ),
    }
}

/// Derives this query block's own [`crate::mvstack::GmMvTable`] -- one
/// `gm_get_motion_vector` (spec 7.10.2.1) result per reference frame, at
/// THIS block's own `(mi_row, mi_col, bw4, bh4)` (mvstack.rs's own doc: the
/// table is always computed at the *querying* block's position, never a
/// donating neighbour's).
fn build_gm_mv_table(
    global_motion: &[ec_av1_syntax::WarpParams; 7],
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    bh4: usize,
    allow_high_precision_mv: bool,
    force_integer_mv: bool,
) -> crate::mvstack::GmMvTable {
    std::array::from_fn(|i| {
        let gm = &global_motion[i];
        crate::warp::gm_get_motion_vector(
            gm.model,
            &gm.params,
            mi_row,
            mi_col,
            bw4,
            bh4,
            allow_high_precision_mv,
            force_integer_mv,
        )
    })
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
    // lane-av1refs: every non-`LAST_FRAME` reference this frame header's own
    // `ref_frame_idx` names a live DPB slot for, indexed `[ref_frame]`
    // (`LAST2_FRAME`=2 .. `ALTREF_FRAME`=7; index 0/1 unused, `LAST_FRAME`
    // stays on `ref_y`/`u`/`v` above) -- `None` at a live index means the
    // slot's still empty (an error-resilient stream, or too early in the
    // GOP), matching `GOLDEN_FRAME`'s existing empty-slot refusal below.
    other_refs: &RefSlots,
    sign_bias_table: &SignBiasTable,
    // lane-gm r2: this frame header's own `global_motion` table (spec
    // 5.9.24), indexed `[ref_frame - LAST_FRAME]` same as `sign_bias_table`
    // -- feeds `warp::gm_get_motion_vector` for `GLOBALMV`/`GLOBAL_GLOBALMV`
    // assignment and the mv-stack's neighbour substitution.
    global_motion: &[ec_av1_syntax::WarpParams; 7],
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
    force_integer_mv: bool,
    interp_fixed: Option<mc::InterpFilterKind>,
    enable_dual_filter: bool,
    tpl_frame: Option<&TplFrameArgs>,
    // lane-av1comp: this frame header's own `reference_select` bit -- `false`
    // means every block is `SINGLE_REFERENCE` by construction and `comp_mode`
    // is never read (spec 5.11.25); `true` reads `comp_mode` per block and
    // refuses (rather than silently misdecoding) the ones that pick
    // `COMPOUND_REFERENCE`, since two-reference motion compensation is not
    // wired yet.
    reference_select: bool,
    // lane-av1comp: `seq_params`' own masked-compound/distance-weighted-
    // compound enable bits, gating `comp_group_idx`/`compound_idx`
    // (spec 5.11.25) -- always present (unlike `tpl_frame`), since the
    // compound blend weights need this frame's own order hint regardless
    // of `use_ref_frame_mvs`.
    enable_masked_compound: bool,
    // lane-sb128 r4: this sequence header's own `enable_interintra_compound`
    // bit (spec 5.11.24's `interintra` read, libaom `decodemv.c`
    // read_inter_block_mode_info ~1490-1510) -- read right after `assign_mv`
    // and before `read_motion_mode` on the single-ref path below, gated by
    // `is_interintra_allowed` (single-ref, `BLOCK_8X8..=BLOCK_32X32`, and
    // every mode this single-ref branch can produce is already in
    // `SINGLE_INTER_MODE_START..END`); `interintra == 1` is a named refusal
    // (inter+intra blended prediction unimplemented) rather than a
    // mis-decode.
    enable_interintra_compound: bool,
    enable_jnt_comp: bool,
    order_hint_bits: u32,
    order_hint: u32,
    ref_order_hints: [u32; 7],
    // lane-av1comp round 14: this frame header's own `skip_mode_present` bit
    // and `skip_mode_frame` reference pair (spec 5.9.22, `frame.rs`'s
    // `read_skip_mode_params`, already 1-based `MV_REFERENCE_FRAME`) --
    // `skip_mode` (spec 5.11.23 `read_skip_mode`) is read *before* `skip`
    // when this is `true` (every block this decoder reaches is >= 8x8, so
    // `is_comp_ref_allowed` never gates it further); a `true` result forces
    // `skip_txfm`, `is_inter`, `NEAREST_NEARESTMV` of this pair with no mode/
    // DRL symbol read, and a plain average blend with no `comp_group_idx`/
    // `compound_idx` read (libaom `decodemv.c` 1604-1619, 1382-1384, 1509).
    skip_mode_present: bool,
    skip_mode_frame: [u8; 2],
    // lane-motionmode round 1: this frame header's own `is_motion_mode_switchable`/
    // `allow_warped_motion` bits (spec 5.11.24's `read_motion_mode`).
    switchable_motion_mode: bool,
    allow_warped_motion: bool,
    // lane-warp r5: set by the HORZ_B arm -- the 32x16 top strip is decoded
    // as a square 32x32 block, which over-reads a residual unless coded
    // `skip`; a non-skip strip must refuse cleanly instead of desyncing.
    reject_residual: bool,
    // lane-partitions r1: the strip's own true width/height in pixels --
    // `side` keeps governing every syntax/CDF/prediction-buffer decision
    // (matching `HORZ_B`'s already-accepted square-context corner-cut), but
    // the final pixel write and this block's own neighbour-context stamp
    // (`record*`/`fill_*_grid`) use `write_w`/`write_h` instead, so a
    // PARTITION_HORZ/VERT strip only claims its own true footprint. `side`
    // for every existing (square) caller.
    write_w: usize,
    write_h: usize,
    // lane-screen r2: this frame header's own `allow_screen_content_tools`
    // bit, threaded to the intra-sub-block branch below -- libaom's
    // `read_intra_block_mode_info` (decodemv.c:1065-1107, the inter-frame
    // counterpart of `read_intra_frame_mode_info`) reads `y_mode_cdf`
    // (never `kf_y_mode_cdf`) and has NO `use_intrabc` call at all (that
    // symbol is intra-frame-only, spec `av1_read_mode_info` dispatches on
    // `frame_is_intra_only`), but it still calls `read_palette_mode_info`
    // under the same `av1_allow_palette` gate as the key-frame path.
    allow_screen_content_tools: bool,
    // lane-superres r9: this frame's own coded luma width (spec 7.11.3.3) --
    // compared against each reference's stored picture width to decide
    // whether a scaled-MC path (`mc::predict_scaled`) or the ordinary
    // stride-1 one (`mc::predict_with_filters`) applies per reference, and to
    // refuse (rather than mis-decode) the warp/OBMC/interintra/compound
    // combinations that don't implement scaled MC yet.
    frame_width: usize,
) -> Result<()> {
    let (r, c) = at;
    // lane-inter8 r2: the inter side bands are mi-granular; this block's own
    // above/left neighbour is the mi cell at its top-left corner (libaom
    // `above_mbmi` = mi(mi_row - 1, mi_col)), so a SUB-aligned block reads
    // exactly what it read before.
    let (rmi, cmi) = (r * (SUB / MI), c * (SUB / MI));
    let (px, py) = (c * SUB, r * SUB);
    let (cpx, cpy) = (px / 2, py / 2);
    let chroma_side = side / 2;
    let (write_chroma_w, write_chroma_h) = (write_w / 2, write_h / 2);

    if std::env::var_os("EC_AV1_TELL").is_some() {
        eprintln!(
            "TELL mi_row={} mi_col={} label=block_entry side={side} tell={} range={}",
            r * SUB_MI as usize, c * SUB_MI as usize, dec.debug_bitpos(), dec.debug_state().0
        );
    }
    let skip_mode_ctx =
        usize::from(neighbours.above_skip_mode[cmi]) + usize::from(neighbours.left_skip_mode[rmi]);
    // lane-comppin r3: skip_mode desync isolation -- dump ctx + the pre-read
    // MSAC tell so it can be diffed against an equivalent hook in libaom's
    // own `read_skip_mode`/`av1_get_skip_mode_context`.
    if std::env::var_os("EC_AV1_SKIPMODE_DUMP").is_some() {
        eprintln!(
            "EC_SKIPMODE r={r} c={c} ctx={skip_mode_ctx} skip_mode_present={skip_mode_present} tell_before={}",
            dec.debug_bitpos()
        );
    }
    // spec 5.11.5 `inter_frame_mode_info`: `inter_segment_id(1)` before
    // `skip_mode`/`skip`, `inter_segment_id(0)` after them.
    let (seg_mi_r, seg_mi_c) = (r * SUB_MI as usize, c * SUB_MI as usize);
    let (seg_w_mi, seg_h_mi) = (write_w / 4, write_h / 4);
    inter_segment_id(dec, cdfs, seg_mi_r, seg_mi_c, seg_w_mi, seg_h_mi, false, true);
    let skip_mode = skip_mode_present && dec.symbol(&mut cdfs.skip_mode[skip_mode_ctx]) == 1;
    if std::env::var_os("EC_AV1_SKIPMODE_DUMP").is_some() {
        eprintln!(
            "EC_SKIPMODE r={r} c={c} result={skip_mode} tell_after={}",
            dec.debug_bitpos()
        );
    }
    if skip_mode {
        SKIP_MODE_HITS.with(|c| c.set(c.get() + 1));
    }

    let skip_ctx = usize::from(neighbours.above_skip[cmi]) + usize::from(neighbours.left_skip[rmi]);
    let skip = skip_mode || dec.symbol(&mut cdfs.skip[skip_ctx]) == 1;
    inter_segment_id(dec, cdfs, seg_mi_r, seg_mi_c, seg_w_mi, seg_h_mi, skip, false);
    maybe_read_cdef_idx(dec, r * SUB_MI as usize, c * SUB_MI as usize, skip);
    maybe_read_delta_q(dec, cdfs, r * SUB_MI as usize, c * SUB_MI as usize, side == 64, skip);
    maybe_read_delta_lf(dec, cdfs, r * SUB_MI as usize, c * SUB_MI as usize, side == 64, skip);
    // lane-warp r5: see `reject_residual` -- a square-block decode of a
    // HORZ_B strip is symbol-exact only for `skip` (no residual, no
    // motion_mode/warp symbol), so gate here before any further read.
    if reject_residual && !skip {
        return Err(unsupported(
            "a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding",
        ));
    }

    let (has_above, has_left) = (
        r > neighbours.tile_row0_mi / (SUB / MI),
        c > neighbours.tile_col0_mi / (SUB / MI),
    );
    let (above_inter, left_inter) = (neighbours.above_inter[cmi], neighbours.left_inter[rmi]);
    let ii_ctx = intra_inter_ctx(has_above, has_left, above_inter, left_inter);
    let is_inter = skip_mode || dec.symbol(&mut cdfs.intra_inter[ii_ctx]) == 1;
    if std::env::var_os("EC_TRACE_MODE").is_some() {
        eprintln!(
            "EC_MODE mi_row={} mi_col={} rng={}",
            r * SUB_MI as usize,
            c * SUB_MI as usize,
            dec.debug_state().0
        );
    }
    if std::env::var_os("EC_AV1_TELL").is_some() {
        eprintln!(
            "TELL mi_row={} mi_col={} label=post_is_inter skip_mode={skip_mode} skip={skip} is_inter={is_inter} tell={} range={}",
            r * SUB_MI as usize, c * SUB_MI as usize, dec.debug_bitpos(), dec.debug_state().0
        );
    }

    let mode_for_tx;
    let uv_predict_mode;
    let (luma_grid, u_grid, v_grid);
    let block_filter;
    let ref_frame_for_lf;
    let globalmv_for_lf;
    // lane-av1comp: `Some((comp_group_idx, compound_idx))` only for a real
    // `COMPOUND_REFERENCE` block -- applied to `neighbours` *after* the
    // common `record_inter` call below (which would otherwise stamp the
    // single-ref ALTREF-special-case default over it).
    let mut compound_ctx: Option<(i8, u8, u8)> = None;
    // lane-txselect: `read_block_tx_size`'s leaf transform units (spec
    // 5.11.17), `Some` only when this block's var-tx tree resolved to more
    // than one luma transform -- read back by the residual loop, the
    // coefficient-context write-back and the loop-filter grid fill below.
    let mut vartx_leaves: Option<Vec<(usize, usize, usize)>> = None;
    let at_mi = (r * (SUB / MI), c * (SUB / MI));
    if is_inter {
        let above_nbr = has_above.then(|| NeighbourRef {
            is_inter: above_inter,
            ref0: if above_inter {
                neighbours.above_ref[cmi]
            } else {
                0
            },
            ref1: neighbours.above_ref1[cmi],
            uni: neighbours.above_ref1[cmi]
                .is_some_and(|r1| is_uni_comp_ref(neighbours.above_ref[cmi], r1)),
        });
        let left_nbr = has_left.then(|| NeighbourRef {
            is_inter: left_inter,
            ref0: if left_inter {
                neighbours.left_ref[rmi]
            } else {
                0
            },
            ref1: neighbours.left_ref1[rmi],
            uni: neighbours.left_ref1[rmi]
                .is_some_and(|r1| is_uni_comp_ref(neighbours.left_ref[rmi], r1)),
        });
        if std::env::var_os("EC_AV1_COMPIDX_DUMP").is_some() {
            eprintln!(
                "EC_PRECOMP r={r} c={c} skip_mode={skip_mode} skip={skip} reference_select={reference_select} tell={}",
                dec.debug_bitpos()
            );
        }
        let is_compound =
            skip_mode || (reference_select && read_comp_mode(dec, cdfs, above_nbr, left_nbr));
        if is_compound {
            let (ref0, ref1) = if skip_mode {
                (skip_mode_frame[0] as i8, skip_mode_frame[1] as i8)
            } else {
                read_compound_ref_frames(
                    dec,
                    cdfs,
                    above_nbr,
                    left_nbr,
                    neighbours.above_ref[cmi],
                    neighbours.left_ref[rmi],
                )
            };
            let (mi_row, mi_col) = (r * SUB_MI as usize, c * SUB_MI as usize);
            // lane-rect r2: true footprint, mirroring the single-ref path below.
            let bw4 = write_w / 4;
            let bh4 = write_h / 4;
            // spec 7.10.2.8, doubled per side (`CompoundTplArgs`): same
            // `use_ref_frame_mvs` gating as the single-ref `tpl` build
            // below, just one `get_relative_dist` per reference.
            let comp_tpl = tpl_frame.map(|t| crate::mvstack::CompoundTplArgs {
                field: t.field,
                cur_offset_0: crate::motion_field::get_relative_dist(
                    t.order_hint_bits,
                    t.order_hint,
                    t.ref_order_hints[(ref0 - LAST_FRAME) as usize],
                ),
                cur_offset_1: crate::motion_field::get_relative_dist(
                    t.order_hint_bits,
                    t.order_hint,
                    t.ref_order_hints[(ref1 - LAST_FRAME) as usize],
                ),
                allow_high_precision_mv,
            });
            let gm_table = build_gm_mv_table(
                global_motion,
                mi_row,
                mi_col,
                bw4,
                bh4,
                allow_high_precision_mv,
                force_integer_mv,
            );
            let comp_stack = crate::mvstack::find_mv_stack_compound(
                grid,
                mi_row,
                mi_col,
                bw4,
                bh4,
                (ref0, ref1),
                mi_cols as usize,
                mi_rows as usize,
                sign_bias_table,
                &gm_table,
                comp_tpl,
            );
            let compound_mode = if skip_mode {
                0 // NEAREST_NEARESTMV, forced -- no mode symbol read
            } else {
                read_inter_compound_mode(dec, cdfs, comp_stack.new_mv_ctx, comp_stack.ref_mv_ctx)
            };
            let (mv0, mv1) = assign_compound_mv(
                dec,
                cdfs,
                &comp_stack,
                compound_mode,
                allow_high_precision_mv,
                force_integer_mv,
                (gm_table[(ref0 - LAST_FRAME) as usize], gm_table[(ref1 - LAST_FRAME) as usize]),
            );
            let is_globalmv = compound_mode == 6; // GLOBAL_GLOBALMV
            // spec `is_nontrans_global_motion`: ALL active refs' models must
            // be non-TRANSLATION (IDENTITY counts) for the compound block's
            // interp-filter read to be suppressed.
            let gm_nontrans = is_globalmv
                && global_motion[(ref0 - LAST_FRAME) as usize].model != ec_av1_syntax::WarpModel::Translation
                && global_motion[(ref1 - LAST_FRAME) as usize].model != ec_av1_syntax::WarpModel::Translation;
            let (py0, pu0, pv0) = ref_planes(ref0, ref_y, ref_u, ref_v, other_refs)?;
            let (py1, pu1, pv1) = ref_planes(ref1, ref_y, ref_u, ref_v, other_refs)?;
            // lane-cwarp r1: `is_global_mv_block` (blockd.h:421-429) is
            // PER REFERENCE SLOT -- mode GLOBAL_GLOBALMV, block >= 8x8, and
            // THAT slot's own gm model > TRANSLATION. `bw4`/`bh4` come from
            // the write extent below, so recompute the size bound from
            // `side` here (the block's own size, what libaom's `bsize`
            // is).
            let is_global_mv0 = is_globalmv
                && side >= 8
                && global_motion[(ref0 - LAST_FRAME) as usize].model as u8 > 1;
            let is_global_mv1 = is_globalmv
                && side >= 8
                && global_motion[(ref1 - LAST_FRAME) as usize].model as u8 > 1;
            // libaom `av1_init_warp_params` + `allow_warp`'s
            // `global_warp_allowed` branch, run once per reference for a
            // compound block exactly as for a single-ref one (reconinter.c
            // `build_inter_predictors_8x8_and_bigger` loops over `ref` and
            // calls `av1_init_warp_params` inside the loop) -- WARPED_CAUSAL
            // never reaches here (spec 5.11.27 / libaom `read_motion_mode`:
            // `has_second_ref` returns SIMPLE_TRANSLATION), so the local-warp
            // arm of `allow_warp` is dead on this path and only the global
            // one applies. `force_integer_mv` short-circuits
            // `av1_init_warp_params` before `allow_warp`.
            let compound_warp = |ref_frame: i8, eligible: bool| {
                if eligible
                    && !force_integer_mv
                    && !global_motion[(ref_frame - LAST_FRAME) as usize].invalid
                {
                    crate::warp::global_warp_params(
                        global_motion[(ref_frame - LAST_FRAME) as usize].params,
                    )
                } else {
                    None
                }
            };
            let warp0 = compound_warp(ref0, is_global_mv0);
            let warp1 = compound_warp(ref1, is_global_mv1);
            if warp0.is_some() || warp1.is_some() {
                COMPOUND_WARP_HITS.with(|c| c.set(c.get() + 1));
            }
            // spec `get_ref_filter_type`: matches when EITHER of the
            // neighbour's two references equals this block's own ref0 --
            // `above_ref1`/`left_ref1` is the neighbour's real second
            // reference (`record_compound_ctx`) when it was itself a
            // compound block; checking `above_ref`/`left_ref` alone drops a
            // real filter match down to the "no neighbour" sentinel
            // whenever the shared reference sits in the neighbour's SECOND
            // slot (lane-av1idx r2).
            let above_filter_ctx = if neighbours.above_ref[cmi] == ref0
                || neighbours.above_ref1[cmi] == Some(ref0)
            {
                neighbours.above_filter[cmi]
            } else {
                [3, 3]
            };
            let left_filter_ctx = if neighbours.left_ref[rmi] == ref0
                || neighbours.left_ref1[rmi] == Some(ref0)
            {
                neighbours.left_filter[rmi]
            } else {
                [3, 3]
            };
            globalmv_for_lf = is_globalmv;
            ref_frame_for_lf = ref0;
            mode_for_tx = 0;
            uv_predict_mode = DC_PRED;
            // lane-rect r2: `grid` (mvstack.rs's own MiGrid, distinct from
            // `neighbours`) must be stamped with the block's true footprint
            // too -- a square `bw4`x`bw4` stamp here claims a rect strip's
            // NEXT strip's own rows/cols before it decodes, corrupting
            // every later mvstack scan that reads this cell back.
            for dr in 0..bh4 {
                for dc in 0..bw4 {
                    grid.set(
                        mi_row + dr,
                        mi_col + dc,
                        MiInfo {
                            is_inter: true,
                            ref_frame: ref0,
                            ref_frame1: Some(ref1),
                            mv1: Some(mv1),
                            mv: mv0,
                            is_new_mv: matches!(compound_mode, 2 | 3 | 4 | 5 | 7),
                            size: bw4,
                            size_h: bh4,
                            // libaom `is_global_mv_block`: per-ref-slot, mode
                            // GLOBAL_GLOBALMV, block >= 8x8, and THAT slot's
                            // own gm model > TRANSLATION (IDENTITY excluded
                            // too -- the two-predicate trap, distinct from
                            // `gm_nontrans` above which allows IDENTITY).
                            is_global_mv0,
                            is_global_mv1,
                        },
                    );
                }
            }
            // spec 5.11.25's `comp_group_idx`/`compound_idx`: only read when
            // masked compound / distance-weighted compound are actually
            // enabled for this stream -- `is_any_masked_compound_used_here`
            // is `true` for every block size this decoder ever reaches
            // (`min(side,side) >= 8`, `COMPOUND_DIFFWTD` always eligible
            // there), so the gate reduces to the sequence header bit; kept
            // as an explicit call so a future smaller block size is not
            // silently wrong (libaom `reconinter.h::is_any_masked_compound_used`).
            let group_ctx = get_comp_group_idx_context(neighbours, (rmi, cmi), side);
            let comp_group_idx = if !skip_mode
                && enable_masked_compound
                && is_any_masked_compound_used_here(side)
            {
                dec.symbol(&mut cdfs.comp_group_idx[group_ctx])
            } else {
                0
            };
            // lane-maskcomp r2 / lane-wedge r3: `Some(mask_type)` for a
            // `COMPOUND_DIFFWTD` block, `Some(mask)` (wedge codebook lookup)
            // for a `COMPOUND_WEDGE` block -- exactly one of the two is set
            // when `comp_group_idx == 1`.
            let mut diffwtd_mask_type: Option<u8> = None;
            let mut wedge_mask: Option<&'static [u8]> = None;
            if comp_group_idx == 1 {
                // lane-maskcomp r1: `read_compound_type` (spec 5.11.25,
                // libaom `decodemv.c` 1634-1656). `is_interinter_compound_used
                // (COMPOUND_WEDGE, bsize)` (`av1_is_wedge_used`,
                // `av1_wedge_params_lookup`) is true for every square bsize
                // this leaf reaches (8x8/16x16/32x32 all have
                // `wedge_types > 0`), so `compound_type` always reads a real
                // symbol here -- no alphabet collapse to worry about at this
                // block-size family.
                let wedge_bsize = match side {
                    8 => 3,
                    16 => 6,
                    32 => 9,
                    // lane-inter8 r1: `av1_is_wedge_used` is false at 64x64
                    // (`av1_wedge_params_lookup[BLOCK_64X64].wedge_types ==
                    // 0`), so a real encoder writes NO `compound_type` symbol
                    // there -- COMPOUND_DIFFWTD is inferred. Reading one
                    // would desync, so refuse by name rather than guess the
                    // inferred-DIFFWTD blend this round.
                    _ => {
                        return Err(unsupported(
                            "a masked compound 64x64 inter block (compound_type is inferred, not coded, at this size)",
                        ))
                    }
                };
                let compound_type = dec.symbol(&mut cdfs.compound_type[wedge_bsize]);
                if compound_type == 0 {
                    // COMPOUND_WEDGE: lane-wedge r3, codebook checksum-
                    // verified vs independent C dump (wedge.rs).
                    let wedge_index = dec.symbol(&mut cdfs.wedge_idx[wedge_bsize]);
                    let wedge_sign = dec.literal(1);
                    WEDGE_HITS.with(|c| c.set(c.get() + 1));
                    wedge_mask = Some(
                        crate::wedge::wedge_masks()
                            .codebook(side)
                            .mask(wedge_sign as usize, wedge_index as usize),
                    );
                } else {
                    // COMPOUND_DIFFWTD: `mask_type`, MAX_DIFFWTD_MASK_BITS==1.
                    let mask_type = dec.literal(1);
                    diffwtd_mask_type = Some(mask_type as u8);
                }
                MASKED_COMPOUND_HITS.with(|c| c.set(c.get() + 1));
            }
            // corner-cut (lane-av1comp r16/r17, lane-av1blend r1): r16/r17
            // called this a `predict_compound_intermediate`/`combine_compound`
            // reconstruction defect. r1 FALSIFIED that hypothesis outright:
            // unmasking and running the real gates
            // (a_real_aomenc_stream_with_reference_select_reads_comp_mode_correctly,
            // a_real_aomenc_stream_with_compound_references_decodes_pixel_exact)
            // shows the plain-average blend is bit-exact *most of the time*
            // -- `combine_compound`'s two-shift-in-one round2 was proven
            // algebraically identical to libaom's separate
            // `>>DIST_PRECISION_BITS`-then-`ROUND_POWER_OF_TWO` sequence
            // (av1_dist_wtd_convolve_2d_c, convolve.c:402-415) for every
            // input, and a whole-pel zero-MV compound block (mv0=mv1=(0,0),
            // the common case a lag-in-frames GOP's hidden altref frames
            // hit) reproduces aomdec's own pre-deblock dump byte-for-byte
            // for several consecutive frames.
            //
            // The real defect a live gate run isolated (seed 49,
            // `fixtures/av1blend-r1-mismatch.obu`): decode-order frame 0
            // (keyframe) and frame 1 (first hidden altref, `ref1 ==
            // ALTREF_FRAME`) match aomdec's own pre-deblock dump byte for
            // byte; decode-order frame 2 -- same shape, same zero MV, only
            // difference `ref1 == BWDREF_FRAME` instead of `ALTREF_FRAME` --
            // is the *first* frame to diverge (881/6144 luma+chroma bytes).
            // Every later frame up through decode-order 9 also mismatches,
            // *including* frame 8, whose four blocks are back to
            // `ref1 == ALTREF_FRAME` only -- ruling out "BWDREF_FRAME's MC
            // math is wrong" (that block's own math is proven exact) in
            // favour of "the picture sitting in some DPB slot is already
            // wrong by the time frame 2 reads it, and the corruption
            // propagates through refresh_frame_flags into every later
            // reference regardless of which named ref a later block picks".
            // Frames 10 through 18 (once the GOP's lineage stops tracing
            // back through the bad slot) are bit-exact again. This points at
            // reference-slot bookkeeping (`ref_slots`/`refresh_frame_flags`
            // in `stream.rs`'s `decode_stream`, or `ref_order_hints`/DPB
            // indexing feeding `dist_wtd_comp_weight_assign`) around
            // BWDREF_FRAME specifically, not at the MC/blend arithmetic in
            // mc.rs. Not isolated further within lane-av1blend's budget --
            // refuse by name rather than ship a blend that is right only
            // sometimes. Ceiling: r13/r14's MC plumbing above this line
            // stays -- only the plain-average reconstruct is masked off
            // again. Upgrade: instrument `ref_slots`/`refresh_frame_flags`
            // per decode-order frame (which slot each `Frame` OBU refreshes,
            // and which slot each `ref_frame_idx[BWDREF_FRAME - LAST_FRAME]`
            // reads) against aomdec's own `RefCntBuffer` bookkeeping on
            // `fixtures/av1blend-r1-mismatch.obu`, starting at decode-order
            // frame 2.
            //
            // r2 (fresh fixture, seed 45, same recipe -- the original r1
            // fixture is gitignored and could not be regenerated byte-exact):
            // static review of `stream.rs`'s `ref_slots`/`refresh_frame_flags`/
            // `cdf_slots` bookkeeping and the `show_existing_frame` refresh
            // arm (spec 7.21: only refreshes on a shown KEY frame, which the
            // parser already enforces at `frame.rs:791/815` -- `refresh_frame_flags`
            // is forced 0 on every non-key `show_existing_frame`) turned up
            // nothing wrong; `other_refs`/`ref_planes` slot indexing for
            // `BWDREF_FRAME` is spec-correct too. A live unmasked run
            // isolated a *different, smaller* shape than r1's: decode-order
            // frame 3 (a `ref1 == BWDREF_FRAME`, zero-MV, simple-average
            // compound block) is the first to diverge from ffmpeg (worst
            // delta 1, luma only, clustered at 32-pixel block edges) even
            // though BOTH its compound inputs -- the key frame and
            // decode-order frame 2's own picture (`ref1 == ALTREF_FRAME`,
            // itself proven bit-exact: it is later re-shown via
            // `show_existing_frame` and matches ffmpeg exactly) -- are
            // independently pixel-exact, and the blend arithmetic is the
            // same proven-exact formula. That rules out "wrong slot"/"stale
            // picture" for this fixture: the inputs are right, so the defect
            // is in decode-order frame 3's *own* per-block decode (entropy
            // desync from compound-CDF adaptation carried across a slot, or
            // the compound mode/MV read itself) -- not in `ref_slots`/
            // `refresh_frame_flags`. Not isolated further within budget.
            // Upgrade: bisect decode-order frame 3's tile decode against a
            // synced `EC_TOK`/`EC_PART` trace from the instrumented aomdec
            // build (`/tmp/libaom-src/build/decoder-debug/aomdec`) on
            // `fixtures/av1blend-r1-mismatch.obu` (seed 45 now, not seed 49)
            // -- the pre-deblock dump frame-index alignment between this
            // crate and that aomdec build is NOT 1:1 on this fixture (23
            // dumps here vs 24 there), so calibrate that first.
            // r3 (this round): pre-deblock buffers were bisected byte-for-byte
            // against an instrumented aomdec build (frame-index alignment
            // recalibrated via content-hash matching rather than the raw
            // dump count -- the keyframe never calls a pre-deblock dump at
            // all, since only `decode_inter_frame_tile` does, which is the
            // whole "23 vs 24" gap, not a real bug).
            // Decode-order frame 3 (`fixtures/av1blend-r1-mismatch.obu`,
            // seed 45) is still the first divergence: PRE-deblock differs
            // from aomdec's own pre-deblock dump by up to 1, on ~20% of
            // luma pixels, clustered where its two compound inputs' pixel
            // values disagree by more than a couple of levels -- both
            // inputs (the keyframe and decode-order frame 2's own picture)
            // are independently proven bit-exact (pre- and post-deblock)
            // against aomdec, and `decodemv`'s own EC_TRACE (mode/mv/skip)
            // matches this decoder's block loop exactly for every block in
            // this frame. Solving `aomval = (key*w0 + bwd*w1 + 8) >> 4` for
            // every mismatching pixel against `key`/`bwd` (both known-exact)
            // picks out `w0=9, w1=7` for the majority (525/846) with zero
            // matches for the plain average this decoder actually emits or
            // the reversed `w0=7, w1=9` -- i.e. `compound_idx` is being
            // decoded as `1` (simple average, spec `COMPOUND_AVERAGE`) when
            // the real bitstream encoded `0` (`COMPOUND_DISTWTD`,
            // `dist_wtd_comp_weight_assign`'s own weights for this frame's
            // order-hint distances 2/3 land on exactly `(9, 7)` -- this
            // decoder's own `dist_wtd_comp_weight_assign` was independently
            // re-verified against `QUANT_DIST_WEIGHT`/`QUANT_DIST_LOOKUP_TABLE`
            // by hand for these distances and gives the same `(9, 7)`).
            // `get_comp_index_context` was checked line-for-line against
            // libaom's `pred_common.h` (the `fwd`/`bck` distance halves, the
            // `above_mi`/`left_mi` `has_second_ref`/`ALTREF_FRAME` fallback,
            // the `3 * offset` term) and matches; `Default_Compound_Idx_Cdf`
            // (`cdf.rs`'s `COMPOUND_IDX`) matches `entropymode.c`'s
            // `default_compound_idx_cdfs` value-for-value; the read is
            // gated on `!skip_mode && enable_jnt_comp`, matching libaom's
            // `has_second_ref(mbmi) && !mbmi->skip_mode` outer guard plus
            // `order_hint_info.enable_dist_wtd_comp` inner guard. The
            // remaining 321/846 mismatched pixels do NOT fit `(9, 7)`
            // either -- some blocks in this frame plausibly decode a
            // genuinely different (correct, context-dependent) compound_idx
            // than block (0,0)'s own -- so this is not simply "always wrong
            // the same way": table/context/gate are all individually
            // verified correct, yet the decoded VALUE at this specific
            // symbol is wrong at least once in this frame. Not isolated
            // further within budget. Upgrade: instrument `dec.symbol`'s own
            // range/bit state (`rng`/`tell`, matching aomdec's own EC_PART
            // trace fields) immediately before and after the `compound_idx`
            // read specifically, not just the surrounding mode/mv/skip
            // trace which does match -- the desync (if any) is local to
            // this one read, not a wider stream desync.
            //
            // r5 (this round): the `resolve_interp_filter` call used to sit
            // right here, ahead of `comp_group_idx`/`compound_idx` -- spec
            // 5.11.19/libaom `decodemv.c:1575` reads `interp_filter` AFTER
            // `read_compound_type`, not before, so that was a genuine
            // ordering bug for a Switchable-filter frame (moved below, past
            // the `compound_ctx = Some(...)` line). It does NOT explain this
            // fixture's own mismatch, though: `av1blend-r1-mismatch.obu`'s
            // header carries a *fixed* interpolation filter
            // (`interp_fixed = Some(Regular)`, confirmed live via a
            // one-off dump), so `resolve_interp_filter` never read a symbol
            // in either position -- zero bits moved, decoded values
            // unchanged before and after the reorder. Kept anyway: it is a
            // real spec-order fix for the next Switchable-filter compound
            // stream, independently correct regardless of this fixture.
            //
            // Also ruled out this round: comparing this crate's own
            // `SymbolDecoder::debug_bitpos()` against aomdec's
            // `aom_reader_tell()` at the block boundary is NOT a valid
            // desync probe -- they are different quantities (raw consumed
            // input bits vs. an entropy-cost estimate that advances without
            // a literal byte fetch), so equal/unequal bitpos proves nothing
            // either way. Do not repeat that comparison; instead trace
            // `range`/`value` (this crate) against libaom's own internal
            // `dif`/`rng` (needs new instrumentation in both) immediately
            // after each read *upstream* of this one in the same block:
            // `read_comp_mode`, `read_compound_ref_frames` (comp_ref_type/
            // uni_comp_ref/comp_ref/comp_bwdref), `read_inter_compound_mode`,
            // and the DRL loop. All of those independently decoded the SAME
            // values this crate and aomdec both show (ref0=1, ref1=5, mode
            // NEAREST_NEARESTMV, mv=(0,0)) -- but an upstream *context*
            // miscomputation can still decode the identical symbol value off
            // a slightly different CDF, narrowing `range` by a different
            // amount than aomdec without changing the decoded value itself,
            // and only surface as a wrong decode a few reads later exactly
            // like this. `get_comp_index_context` and the `COMPOUND_IDX`
            // table were independently re-verified again this round and are
            // still correct; the four candidates above were not re-audited
            // this round. Refusing here (rather than shipping a value-exact-
            // most-of-the-time blend) until one of them is found wrong.
            // r6/r7: found and fixed one real bug (mvstack.rs ref_ctx family
            // undercounted compound neighbours in the single_ref_p*/comp_ref*/
            // comp_bwdref* context reads -- kept, real fix). Unmasking on
            // that fix alone and running the wide gate sweep (many seeds,
            // `a_real_aomenc_stream_with_compound_references_decodes_pixel_exact`/
            // `a_real_aomenc_stream_with_reference_select_reads_comp_mode_correctly`,
            // cq-level=45) still mismatches roughly half the time, at
            // different frames each run for the SAME seed -- aomenc's own
            // cpu-used=0 RD search is run-to-run nondeterministic (noted
            // above), so this is a second, still-unfound compound_idx defect,
            // not the one r6/r7 fixed. r6's own residual-mismatch note (321/
            // 846 pixels not fitting the (9,7)-vs-plain-average hypothesis
            // either) already flagged more than one bug here. Re-masking:
            // do not ship a blend that is right only sometimes.
            // lane-av1idx r1: fixed one real bug this round
            // (`switchable_interp_ctx` never folded in libaom's
            // `INTER_FILTER_COMP_OFFSET` for a compound block's own
            // interp_filter context -- kept). Pinned
            // `fixtures/av1idx-refsel-pin.obu` (seed 61, reference_select
            // gate) still mismatches after that fix (decode-order frame 8
            // first, small deltas, worst 1-2, propagating through later
            // frames via DPB reference) -- a second, still-unfound bug.
            // Re-masking: see that fixture + `scratch_isolate_pinned_mismatch`
            // (`EC_AV1_PIN=fixtures/av1idx-refsel-pin.obu EC_AV1_PIN_N=20`)
            // for the next round's starting point.
            // lane-maskcomp r2: `compound_idx` is only read when
            // `comp_group_idx == 0` (libaom `decodemv.c:1606`) -- a masked
            // (wedge/diffwtd) block never also carries a distance-weighted
            // split. This gate was missing before r2 (dead code: the masked
            // path always refused earlier, so it never executed with
            // `comp_group_idx == 1` in practice).
            let (fwd_offset, bck_offset, compound_idx) = if comp_group_idx == 0
                && !skip_mode
                && enable_jnt_comp
            {
                let idx_ctx = get_comp_index_context(
                    neighbours,
                    (rmi, cmi),
                    side,
                    order_hint_bits,
                    order_hint,
                    ref_order_hints[(ref0 - LAST_FRAME) as usize],
                    ref_order_hints[(ref1 - LAST_FRAME) as usize],
                );
                let idx = dec.symbol(&mut cdfs.compound_idx[idx_ctx]);
                if idx == 1 {
                    (8, 8, 1u8)
                } else {
                    let (fwd, bck) = crate::compound::dist_wtd_comp_weight_assign(
                        order_hint_bits,
                        order_hint,
                        ref_order_hints[(ref0 - LAST_FRAME) as usize],
                        ref_order_hints[(ref1 - LAST_FRAME) as usize],
                    );
                    (fwd, bck, 0u8)
                }
            } else {
                (8, 8, 1u8)
            };
            compound_ctx = Some((ref1, comp_group_idx as u8, compound_idx));
            if std::env::var_os("EC_AV1_COMPIDX_DUMP").is_some() {
                eprintln!(
                    "EC_COMPIDX mi_row={mi_row} mi_col={mi_col} bsize={side} mode={compound_mode} mv0=({},{}) mv1=({},{}) ref0={ref0} ref1={ref1} comp_group_idx={comp_group_idx} compound_idx={compound_idx} tell={}",
                    mv0.0, mv0.1, mv1.0, mv1.1, dec.debug_bitpos()
                );
            }

            // spec 5.11.19/libaom `decodemv.c` 1575: `read_mb_interp_filter`
            // comes AFTER `read_compound_type` (the `comp_group_idx`/
            // `compound_idx` reads just above), not before -- moved here
            // (lane-av1blend r5) from its old position ahead of those reads,
            // which stole their bits for a Switchable-filter compound block
            // and desynced every symbol read after it in the tile.
            // `av1_is_interp_needed` is also 0 for `skip_mode` blocks: the
            // filter is derived (Regular), never read -- same
            // conditional-read contract as the WARPED_CAUSAL suppression on
            // the single-ref path below.
            let (h_filter, v_filter, resolved_filter) = resolve_interp_filter(
                dec,
                cdfs,
                interp_fixed,
                enable_dual_filter,
                gm_nontrans || skip_mode,
                above_filter_ctx,
                left_filter_ctx,
                true,
            );
            block_filter = resolved_filter;

            // lane-scaledref r1: each compound tap scales INDEPENDENTLY off
            // its own stored reference's luma width (spec 7.11.3.3 derives
            // x_scale_fp per reference; libaom `av1_setup_pre_planes` builds
            // one `scale_factors` per `ref_frame`), so the two taps can carry
            // different ratios in the same block. `REF_NO_SCALE` reduces
            // `predict_compound_intermediate`'s walk to the ordinary
            // stride-1 one, so the unscaled case is unchanged.
            let scale0 = mc::scale_factor(py0.width, frame_width);
            let scale1 = mc::scale_factor(py1.width, frame_width);
            if scale0 != mc::REF_NO_SCALE || scale1 != mc::REF_NO_SCALE {
                SCALED_COMPOUND_HITS.with(|c| c.set(c.get() + 1));
            }
            // lane-scaledref r2: the MIXED case -- one tap scaled, the other
            // not -- is the one an all-frames-scaled recipe never produces,
            // and the one a single shared scale factor would get wrong.
            if (scale0 == mc::REF_NO_SCALE) != (scale1 == mc::REF_NO_SCALE) {
                MIXED_SCALE_COMPOUND_HITS.with(|c| c.set(c.get() + 1));
            }

            let mut inter0_y = vec![0i32; side * side];
            mc::predict_compound_intermediate(
                &py0.data,
                py0.width,
                py0.true_width,
                py0.true_height,
                mv_to_q4(px, mv0.1, true),
                mv_to_q4(py, mv0.0, true),
                scale0,
                side,
                side,
                h_filter,
                v_filter,
                &mut inter0_y,
            );
            // lane-cwarp r1: this reference's own GLOBAL warp replaces the
            // translational tap (libaom `av1_warp_plane` with
            // `conv_params->is_compound`); the blend below is unchanged.
            if let Some(wp) = &warp0 {
                crate::warp::warp_affine_compound(
                    wp, &py0.data, py0.true_width as i32, py0.true_height as i32,
                    py0.width as i32, &mut inter0_y, px as i32, py as i32, side as i32,
                    side as i32, side as i32, 0, 0,
                );
            }
            let mut inter1_y = vec![0i32; side * side];
            mc::predict_compound_intermediate(
                &py1.data,
                py1.width,
                py1.true_width,
                py1.true_height,
                mv_to_q4(px, mv1.1, true),
                mv_to_q4(py, mv1.0, true),
                scale1,
                side,
                side,
                h_filter,
                v_filter,
                &mut inter1_y,
            );
            // lane-cwarp r1: this reference's own GLOBAL warp replaces the
            // translational tap (libaom `av1_warp_plane` with
            // `conv_params->is_compound`); the blend below is unchanged.
            if let Some(wp) = &warp1 {
                crate::warp::warp_affine_compound(
                    wp, &py1.data, py1.true_width as i32, py1.true_height as i32,
                    py1.width as i32, &mut inter1_y, px as i32, py as i32, side as i32,
                    side as i32, side as i32, 0, 0,
                );
            }
            let mut pred_y = vec![0u16; side * side];
            let mut diffwtd_mask_y = Vec::new();
            // lane-wedge r3: `mask_y` is the DIFFWTD buffer just computed OR
            // the wedge codebook lookup (comp_group_idx==1's two mutually
            // exclusive branches) -- same [`mc::blend_masked_compound`] call
            // either way, only the mask source differs.
            let mask_y: Option<&[u8]> = if let Some(mask_type) = diffwtd_mask_type {
                diffwtd_mask_y = vec![0u8; side * side];
                mc::diffwtd_mask(&inter0_y, &inter1_y, mask_type == 1, &mut diffwtd_mask_y);
                Some(diffwtd_mask_y.as_slice())
            } else {
                wedge_mask
            };
            if let Some(mask_y) = mask_y {
                mc::blend_masked_compound(
                    &inter0_y, &inter1_y, mask_y, side, side, side, false, &mut pred_y,
                );
            } else {
                mc::combine_compound(&inter0_y, &inter1_y, fwd_offset, bck_offset, &mut pred_y);
            }

            let mut inter0_u = vec![0i32; chroma_side * chroma_side];
            mc::predict_compound_intermediate(
                &pu0.data,
                pu0.width,
                pu0.true_width,
                pu0.true_height,
                mv_to_q4(cpx, mv0.1, false),
                mv_to_q4(cpy, mv0.0, false),
                scale0,
                chroma_side,
                chroma_side,
                h_filter,
                v_filter,
                &mut inter0_u,
            );
            // `av1_init_warp_params` bails when the PLANE's block is
            // narrower/shorter than 8 (`block_width < 8`), so a 4x4 chroma
            // block of an 8x8 luma block stays translational.
            if let Some(wp) = &warp0 {
                if chroma_side >= 8 {
                    crate::warp::warp_affine_compound(
                        wp, &pu0.data, pu0.true_width as i32, pu0.true_height as i32,
                        pu0.width as i32, &mut inter0_u, cpx as i32, cpy as i32,
                        chroma_side as i32, chroma_side as i32, chroma_side as i32, 1, 1,
                    );
                }
            }
            let mut inter1_u = vec![0i32; chroma_side * chroma_side];
            mc::predict_compound_intermediate(
                &pu1.data,
                pu1.width,
                pu1.true_width,
                pu1.true_height,
                mv_to_q4(cpx, mv1.1, false),
                mv_to_q4(cpy, mv1.0, false),
                scale1,
                chroma_side,
                chroma_side,
                h_filter,
                v_filter,
                &mut inter1_u,
            );
            // `av1_init_warp_params` bails when the PLANE's block is
            // narrower/shorter than 8 (`block_width < 8`), so a 4x4 chroma
            // block of an 8x8 luma block stays translational.
            if let Some(wp) = &warp1 {
                if chroma_side >= 8 {
                    crate::warp::warp_affine_compound(
                        wp, &pu1.data, pu1.true_width as i32, pu1.true_height as i32,
                        pu1.width as i32, &mut inter1_u, cpx as i32, cpy as i32,
                        chroma_side as i32, chroma_side as i32, chroma_side as i32, 1, 1,
                    );
                }
            }
            let mut pred_u = vec![0u16; chroma_side * chroma_side];
            if let Some(mask_y) = mask_y {
                mc::blend_masked_compound(
                    &inter0_u,
                    &inter1_u,
                    mask_y,
                    side,
                    chroma_side,
                    chroma_side,
                    true,
                    &mut pred_u,
                );
            } else {
                mc::combine_compound(&inter0_u, &inter1_u, fwd_offset, bck_offset, &mut pred_u);
            }

            let mut inter0_v = vec![0i32; chroma_side * chroma_side];
            mc::predict_compound_intermediate(
                &pv0.data,
                pv0.width,
                pv0.true_width,
                pv0.true_height,
                mv_to_q4(cpx, mv0.1, false),
                mv_to_q4(cpy, mv0.0, false),
                scale0,
                chroma_side,
                chroma_side,
                h_filter,
                v_filter,
                &mut inter0_v,
            );
            // `av1_init_warp_params` bails when the PLANE's block is
            // narrower/shorter than 8 (`block_width < 8`), so a 4x4 chroma
            // block of an 8x8 luma block stays translational.
            if let Some(wp) = &warp0 {
                if chroma_side >= 8 {
                    crate::warp::warp_affine_compound(
                        wp, &pv0.data, pv0.true_width as i32, pv0.true_height as i32,
                        pv0.width as i32, &mut inter0_v, cpx as i32, cpy as i32,
                        chroma_side as i32, chroma_side as i32, chroma_side as i32, 1, 1,
                    );
                }
            }
            let mut inter1_v = vec![0i32; chroma_side * chroma_side];
            mc::predict_compound_intermediate(
                &pv1.data,
                pv1.width,
                pv1.true_width,
                pv1.true_height,
                mv_to_q4(cpx, mv1.1, false),
                mv_to_q4(cpy, mv1.0, false),
                scale1,
                chroma_side,
                chroma_side,
                h_filter,
                v_filter,
                &mut inter1_v,
            );
            // `av1_init_warp_params` bails when the PLANE's block is
            // narrower/shorter than 8 (`block_width < 8`), so a 4x4 chroma
            // block of an 8x8 luma block stays translational.
            if let Some(wp) = &warp1 {
                if chroma_side >= 8 {
                    crate::warp::warp_affine_compound(
                        wp, &pv1.data, pv1.true_width as i32, pv1.true_height as i32,
                        pv1.width as i32, &mut inter1_v, cpx as i32, cpy as i32,
                        chroma_side as i32, chroma_side as i32, chroma_side as i32, 1, 1,
                    );
                }
            }
            let mut pred_v = vec![0u16; chroma_side * chroma_side];
            if let Some(mask_y) = mask_y {
                mc::blend_masked_compound(
                    &inter0_v,
                    &inter1_v,
                    mask_y,
                    side,
                    chroma_side,
                    chroma_side,
                    true,
                    &mut pred_v,
                );
            } else {
                mc::combine_compound(&inter0_v, &inter1_v, fwd_offset, bck_offset, &mut pred_v);
            }

            vartx_leaves = read_block_tx_size(
                dec,
                cdfs,
                neighbours,
                at_mi,
                side,
                (mi_cols as usize, mi_rows as usize),
                true,
                skip,
            )?
            .1;
            if skip {
                y.reconstruct_mc_rect(
                    px,
                    py,
                    side,
                    write_w,
                    write_h,
                    &pred_y,
                    &vec![0i32; side * side],
                );
                u.reconstruct_mc_rect(
                    cpx,
                    cpy,
                    chroma_side,
                    write_chroma_w,
                    write_chroma_h,
                    &pred_u,
                    &vec![0i32; chroma_side * chroma_side],
                );
                v.reconstruct_mc_rect(
                    cpx,
                    cpy,
                    chroma_side,
                    write_chroma_w,
                    write_chroma_h,
                    &pred_v,
                    &vec![0i32; chroma_side * chroma_side],
                );
                luma_grid = vec![0i32; side * side];
                u_grid = vec![0i32; chroma_side * chroma_side];
                v_grid = vec![0i32; chroma_side * chroma_side];
            } else {
                let around = neighbours.around(at, side);
                let luma_tx_type;
                if let Some(leaves) = vartx_leaves.clone() {
                    // spec 5.11.17's transform tree, leaf by leaf in the order
                    // `read_var_tx_size` coded it: each unit reads its own
                    // coefficients against the neighbour magnitudes the
                    // earlier units already wrote, and reconstructs over its
                    // own slice of the block's motion-compensated predictor.
                    let reduced = REDUCED_TX_SET_INTER.with(std::cell::Cell::get);
                    let mut first_tx_type = TxType::DctDct;
                    for (idx, &(row, col, tx_px)) in leaves.iter().enumerate() {
                        let tu_mi = (at_mi.0 + row, at_mi.1 + col);
                        let (tu_px, tu_py) = (px + col * MI, py + row * MI);
                        let mut tu_pred = Vec::with_capacity(tx_px * tx_px);
                        for rr in 0..tx_px {
                            let start = (row * MI + rr) * side + col * MI;
                            tu_pred.extend_from_slice(&pred_y[start..start + tx_px]);
                        }
                        let scan = default_scan(tx_px);
                        let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, tx_px / MI);
                        let (tu_grid, tu_tx_type) = read_inter_plane(
                            dec,
                            cdfs,
                            inter_txbset_for(tx_px, reduced),
                            &scan,
                            0,
                            neighbours.around_mi(tu_mi, tx_px)[0],
                            mode_for_tx,
                            y,
                            tu_px,
                            tu_py,
                            tx_px,
                            base_q_idx,
                            &tu_pred,
                            None,
                            Some(tu_skip_ctx),
                        )?;
                        if idx == 0 {
                            first_tx_type = tu_tx_type;
                        }
                        neighbours.record_mi_luma(tu_mi, tx_px, &tu_grid);
                    }
                    // `av1_get_tx_type`'s chroma lookup scales the chroma
                    // position back to luma, so an inter block's chroma
                    // inherits the *first* (top-left) luma unit's coded type.
                    luma_tx_type = first_tx_type;
                    luma_grid = vec![0i32; side * side];
                } else {
                    (luma_grid, luma_tx_type) = read_inter_plane(
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
                        None,
                        None,
                    )?;
                }
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
                    Some(luma_tx_type),
                    None,
                )?
                .0;
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
                    Some(luma_tx_type),
                    None,
                )?
                .0;
            }
        } else {
            let ref_frame =
                read_single_ref(
                    dec,
                    cdfs,
                    neighbours.above_ref[cmi],
                    neighbours.above_ref1[cmi],
                    neighbours.left_ref[rmi],
                    neighbours.left_ref1[rmi],
                );
            ref_frame_for_lf = ref_frame;
            if std::env::var_os("EC_AV1_TELL").is_some() {
                let (mi_row, mi_col) = (r * SUB_MI as usize, c * SUB_MI as usize);
                eprintln!(
                    "TELL mi_row={mi_row} mi_col={mi_col} label=post_ref_frame ref={ref_frame} tell={} range={}",
                    dec.debug_bitpos(), dec.debug_state().0
                );
            }
            // round 4-9 (lane-av1golden..lane-av1golden7): GOLDEN_FRAME's own
            // stacked defects (lf_level ref/mode-delta forwarding, the
            // switchable_interp ref-frame ctx gate, and a film-grain synthesis
            // gap that looked like a GOLDEN MC bug by survivor bias -- see the
            // ledger) are all closed; GOLDEN_FRAME decodes live via `other_refs`
            // below same as every other non-`LAST_FRAME` reference. lane-av1refs
            // widens this from GOLDEN alone to every reference `read_single_ref`
            // can name.
            let (py_ref, pu_ref, pv_ref) = if ref_frame == LAST_FRAME {
                (ref_y, ref_u, ref_v)
            } else {
                match other_refs[ref_frame as usize] {
                    Some((ry, ru, rv)) => (ry, ru, rv),
                    None => {
                        return Err(unsupported(
                            "a reference frame selected with no picture at this frame's own \
                         ref_frame_idx slot for it",
                        ));
                    }
                }
            };

            let (mi_row, mi_col) = (r * SUB_MI as usize, c * SUB_MI as usize);
            // lane-rect r2: the mv stack must be queried with the block's
            // REAL extent (libaom `av1_get_mv_refs` gets bw4/bh4 from
            // xd->width/xd->height, not a square guess) -- `write_w`/
            // `write_h` already name the true HORZ/VERT/HORZ_B strip
            // footprint (r1's plumbing), so derive bw4/bh4 from those
            // instead of the old `side`-square (HORZ_B-only) heuristic.
            let bw4 = write_w / 4;
            let bh4 = write_h / 4;
            // spec 7.10.2.8: only reached when the frame header set
            // `use_ref_frame_mvs` (`tpl_frame` is `Some` then, `None`
            // otherwise) -- `cur_offset_0` is this query block's own resolved
            // `ref_frame`'s distance from the current frame (spec 7.9.3),
            // recomputed per block since different blocks pick different refs.
            let tpl = tpl_frame.map(|t| crate::mvstack::TplArgs {
                field: t.field,
                cur_offset_0: crate::motion_field::get_relative_dist(
                    t.order_hint_bits,
                    t.order_hint,
                    t.ref_order_hints[(ref_frame - LAST_FRAME) as usize],
                ),
                allow_high_precision_mv,
            });
            let gm_table = build_gm_mv_table(
                global_motion,
                mi_row,
                mi_col,
                bw4,
                bh4,
                allow_high_precision_mv,
                force_integer_mv,
            );
            let stack = find_mv_stack_with_sign_bias(
                grid,
                mi_row,
                mi_col,
                bw4,
                bh4,
                ref_frame,
                mi_cols as usize,
                mi_rows as usize,
                sign_bias_table,
                &gm_table,
                tpl,
            );
            let not_new = dec.symbol(&mut cdfs.new_mv[stack.new_mv_ctx]) == 1;
            let mut is_globalmv = false;
            let (mv, is_new_mv) = if !not_new {
                // NEWMV (spec 5.11.24's `read_drl_idx`, `RefMvIdx` starting at 0):
                // read at most two `drl_mode` bits, one per stack entry past the
                // first, stopping at the first `0`. The chosen index selects
                // which stack entry `read_mv`'s base predictor comes from
                // (spec 7.10.2.10 `assign_mv`'s `PredMv = RefStackMv[RefMvIdx]`)
                // -- entry 0 (`stack.pred_mv`) only when the loop never advances.
                let mut idx = 0usize;
                while idx < 2 && stack.entries.len() > idx + 1 {
                    if dec.symbol(&mut cdfs.drl_mode[stack.drl_ctx[idx]]) == 0 {
                        break;
                    }
                    idx += 1;
                }
                let base_mv = stack.entries.get(idx).map_or(stack.pred_mv, |e| e.mv);
                (
                    read_mv(
                        dec,
                        &mut cdfs.mv_comp,
                        &mut cdfs.mv_joint,
                        base_mv,
                        allow_high_precision_mv,
                        force_integer_mv,
                    ),
                    true,
                )
            } else {
                let not_zero = dec.symbol(&mut cdfs.zero_mv[stack.zero_mv_ctx]) == 1;
                is_globalmv = !not_zero;
                let mv = if !not_zero {
                    // GLOBALMV: `gm_get_motion_vector` (spec 7.10.2.1), already
                    // computed at this block's own position in `gm_table`.
                    gm_table[(ref_frame - LAST_FRAME) as usize]
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
            // lane-gm r2: the two libaom predicates single-ref GLOBALMV
            // blocks need -- `is_global_mv_block` (blockd.h:421-429, mode
            // GLOBAL* AND model > TRANSLATION AND min(bw,bh) >= 8px, gating
            // `motion_mode_eligible`/`MiInfo::is_global_mv0` below) and
            // `is_nontrans_global_motion` (reconinter.h:420-425, model !=
            // TRANSLATION -- IDENTITY counts here -- gating the interp
            // filter suppress below). Do NOT unify these.
            let gm_model = global_motion[(ref_frame - LAST_FRAME) as usize].model;
            let min_bw_bh4 = bw4.min(bh4);
            let is_global_mv_block = is_globalmv && gm_model as u8 > 1 && min_bw_bh4 >= 2;
            let gm_nontrans =
                is_globalmv && gm_model != ec_av1_syntax::WarpModel::Translation && min_bw_bh4 >= 2;
            if std::env::var_os("EC_AV1_TELL").is_some() {
                eprintln!(
                    "TELL mi_row={mi_row} mi_col={mi_col} label=post_assign_mv tell={} range={}",
                    dec.debug_bitpos(), dec.debug_state().0
                );
            }
            // lane-sb128 r4: `interintra` (spec 5.11.24; libaom
            // `decodemv.c` read_inter_block_mode_info ~1490-1510), placed
            // right after `assign_mv` and before `read_motion_mode` below --
            // libaom's own sequential order. `is_interintra_allowed_bsize`
            // (`BLOCK_8X8..=BLOCK_32X32`) is the only real gate on this
            // single-ref path: `is_interintra_allowed_ref` always holds here
            // (single ref, `ref_frame[1]` not yet `INTRA_FRAME`) and
            // `is_interintra_allowed_mode` always holds (every mode this
            // branch produces -- NEARESTMV/NEARMV/GLOBALMV/NEWMV -- is
            // `SINGLE_INTER_MODE_START..END` by construction).
            // `size_group_lookup`: BLOCK_16X16 -> 2, BLOCK_32X32 -> 3.
            let mut interintra_mode: Option<u8> = None;
            let mut wedge_mask: Option<(&'static [u8], usize)> = None;
            if enable_interintra_compound && !skip_mode && (side == 16 || side == 32) {
                let bsize_group = if side == 16 { 2 } else { 3 };
                let interintra = dec.symbol(&mut cdfs.interintra[bsize_group]) == 1;
                if interintra {
                    // lane-interintra r1 (decodemv.c 1540-1555): interintra_mode,
                    // then -- gated on block size ALONE, never the
                    // `enable_interintra_wedge` seq bit -- the wedge flag
                    // (`av1_is_wedge_used(bsize)` holds for 16x16/32x32).
                    let ii = dec.symbol(&mut cdfs.interintra_mode[bsize_group]) as u8;
                    let wedge_bsize = if side == 16 { 6 } else { 9 };
                    let wedge = dec.symbol(&mut cdfs.wedge_interintra[wedge_bsize]) == 1;
                    if wedge {
                        // lane-wii r2 (spec 5.11.25): `wedge_index` is an
                        // ADAPTING CDF symbol over the same `wedge_bsize` row;
                        // NO sign symbol follows -- libaom fixes
                        // INTERINTRA_WEDGE_SIGN 0 (blockd.h).
                        let wedge_index = dec.symbol(&mut cdfs.wedge_idx[wedge_bsize]);
                        WII_HITS.with(|c| c.set(c.get() + 1));
                        wedge_mask = Some((
                            crate::wedge::wedge_masks()
                                .codebook(side)
                                .mask(0, wedge_index as usize),
                            side,
                        ));
                    } else {
                        INTERINTRA_HITS.with(|c| c.set(c.get() + 1));
                    }
                    interintra_mode = Some(ii);
                }
            }
            if std::env::var_os("EC_AV1_TELL").is_some() {
                eprintln!(
                    "TELL mi_row={mi_row} mi_col={mi_col} label=post_interintra tell={} range={}",
                    dec.debug_bitpos(), dec.debug_state().0
                );
            }
            // lane-motionmode round 1: `read_motion_mode` (spec 5.11.24;
            // libaom `decodemv.c` read_inter_block_mode_info, ~1520-1528),
            // placed right after MV assignment and before the switchable
            // interp filter read below (`read_mb_interp_filter`, decodemv.c
            // ~1600) -- libaom's own sequential order. `motion_mode_allowed`
            // (`reconinter.h`) reduces, for every block this single-ref path
            // reaches, to "does an inter neighbour border the above row or
            // left column": `is_motion_variation_allowed_bsize` always holds
            // (`side` is never below 16px on this path), `ref_frame[1] !=
            // INTRA_FRAME` always holds (no interintra reader here),
            // `is_motion_variation_allowed_compound` always holds (this is
            // the single-ref branch), and `is_global_mv_block` never
            // triggers (this decoder's global motion is always `IDENTITY`,
            // so its warp `type` is never `> TRANSLATION`). A stream with
            // `allow_warped_motion=1` is refused right at an eligible block
            // rather than guessed at: `av1_findSamples`/`num_proj_ref` is
            // not ported, so this decoder cannot tell a real
            // `WARPED_CAUSAL`-eligible block (3-symbol `motion_mode_cdf`)
            // from an `OBMC_CAUSAL`-only one (2-symbol `obmc_cdf`) whenever
            // the header allows warp -- reading the wrong alphabet desyncs
            // the tile.
            // An interintra block has `ref_frame[1] == INTRA_FRAME`, which
            // fails libaom's `is_motion_variation_allowed_compound`: the
            // motion_mode symbol is NOT read (SIMPLE_TRANSLATION implied).
            let motion_mode_eligible = switchable_motion_mode
                && !skip_mode
                && interintra_mode.is_none()
                && (!overlappable_above(grid, mi_row, mi_col, bw4, mi_cols as usize, 1).is_empty()
                    || !overlappable_left(grid, mi_row, mi_col, bh4, mi_rows as usize, 1)
                        .is_empty())
                // libaom `motion_mode_allowed`: a GLOBALMV block whose model
                // is non-IDENTITY-non-TRANSLATION under free (non-integer)
                // mv reads no motion_mode/obmc symbol at all -- implicit
                // SIMPLE_TRANSLATION.
                && !(is_global_mv_block && !force_integer_mv);
            let mut obmc_selected = false;
            // lane-warp round 2: `Some` when motion_mode == WARPED_CAUSAL
            // *and* the local warp estimate is valid (`!wm_params.invalid`,
            // `av1_find_projection`) -- an invalid estimate falls back to
            // this block's own translational mv, same as any other block
            // (spec `allow_warp`/`reconinter.c` -- global warp is never the
            // fallback here since this decoder's global motion is always
            // IDENTITY).
            let mut warp_params: Option<crate::warp::WarpParams> = None;
            // Tracks the SYMBOL value, not projection validity: libaom's
            // `av1_is_interp_needed` suppresses the interp-filter read for
            // `motion_mode == WARPED_CAUSAL` even when the projection later
            // falls back to translation. Its third suppressor,
            // `is_nontrans_global_motion`, matches our unconditional
            // `is_globalmv` only because this decoder codes global motion as
            // IDENTITY (non-TRANSLATION) and never reads switchable filters
            // below 8x8 -- porting TRANSLATION global motion must add the
            // wmtype check here.
            let mut warped_selected = false;
            if motion_mode_eligible {
                // `default_obmc_cdf`'s own index: square bsize 8/16/32/64 ->
                // 0/1/2/3; lane-rect r2: a rect strip reads its OWN bsize row
                // (libaom `motion_mode_cdf[mbmi->bsize]`: BLOCK_16X32 -> 4,
                // BLOCK_32X16 -> 5 in our packed table) -- rect-flake-1's
                // strip motion_mode read diverged on the square row.
                let bsize_idx = if write_w == write_h {
                    (write_w.trailing_zeros() - 3) as usize
                } else if write_w == 16 {
                    4
                } else {
                    5
                };
                // `motion_mode_allowed` reads the 3-symbol `motion_mode_cdf`
                // instead of the 2-symbol `obmc_cdf` exactly when
                // `num_proj_ref >= 1` under `allow_warped_motion`.
                // lane-scaledref r2: libaom `motion_mode_allowed`
                // (`blockd.h:1484`) requires
                // `!av1_is_scaled(block_ref_scale_factors[0])` for
                // WARPED_CAUSAL -- under a scaled reference (superres) the
                // block reads the 2-symbol `obmc_cdf`, never the 3-symbol
                // `motion_mode_cdf`. Reading the wrong alphabet here narrows
                // the arithmetic coder by the wrong amount and predicts the
                // rest of the tile off a diverged state: it was silently
                // wrong pixels (r1's `--enable-warped-motion=1` superres
                // mismatch), not a desync error.
                let ref_is_scaled =
                    mc::scale_factor(py_ref.width, frame_width) != mc::REF_NO_SCALE;
                let warp_eligible = allow_warped_motion
                    && !ref_is_scaled
                    && num_proj_ref(grid, mi_row, mi_col, bw4, bh4, mi_cols as usize, mi_rows as usize, ref_frame) >= 1;
                if allow_warped_motion
                    && ref_is_scaled
                    && num_proj_ref(grid, mi_row, mi_col, bw4, bh4, mi_cols as usize, mi_rows as usize, ref_frame) >= 1
                {
                    SCALED_WARP_SUPPRESSED_HITS.with(|c| c.set(c.get() + 1));
                }
                if warp_eligible {
                    let mode = dec.symbol(&mut cdfs.motion_mode[bsize_idx]);
                    match mode {
                        0 => {}
                        1 => {
                            obmc_selected = true;
                            OBMC_HITS.with(|c| c.set(c.get() + 1));
                        }
                        _ => {
                            warped_selected = true;
                            WARP_SELECTED_HITS.with(|c| c.set(c.get() + 1));
                            let mut samples = find_samples(
                                grid,
                                mi_row,
                                mi_col,
                                bw4,
                                bh4,
                                mi_cols as usize,
                                mi_rows as usize,
                                ref_frame,
                            );
                            if std::env::var_os("EC_WARP_DEBUG").is_some() {
                                eprintln!(
                                    "EC_WARP_DEBUG findSamples mi_row={mi_row} mi_col={mi_col} bsize={side} num_proj_ref={}",
                                    samples.len()
                                );
                                for (i, s) in samples.iter().enumerate() {
                                    eprintln!("EC_WARP_DEBUG sample[{i}] pts={:?} pts_inref={:?}", s.pts1, s.pts2);
                                }
                            }
                            if samples.len() > 1 {
                                crate::warp::select_samples(mv, &mut samples, side as i32, side as i32);
                            }
                            warp_params = crate::warp::find_projection(
                                &samples,
                                // lane-rect r2: the block's TRUE dims -- a
                                // 32x16 strip's model center is not a
                                // 32x32's (aom av1_find_projection bw/bh).
                                write_w as i32,
                                write_h as i32,
                                mv.1,
                                mv.0,
                                mi_row as i32,
                                mi_col as i32,
                            );
                            if std::env::var_os("EC_WARP_DEBUG").is_some() {
                                eprintln!(
                                    "EC_WARP_DEBUG projection mi_row={mi_row} mi_col={mi_col} num_proj_ref(final)={} mv=({},{}) params={:?}",
                                    samples.len(), mv.0, mv.1, warp_params
                                );
                                for (i, s) in samples.iter().enumerate() {
                                    eprintln!("EC_WARP_DEBUG sample_used[{i}] pts={:?} pts_inref={:?}", s.pts1, s.pts2);
                                }
                            }
                        }
                    }
                } else {
                    obmc_selected = dec.symbol(&mut cdfs.obmc[bsize_idx]) == 1;
                    if obmc_selected {
                        OBMC_HITS.with(|c| c.set(c.get() + 1));
                    }
                }
            }
            // lane-gm r4: `allow_warp`'s `global_warp_allowed` branch
            // (`reconinter.c:33-55`), gated INDEPENDENTLY of `motion_mode` --
            // libaom's `av1_init_warp_params` runs for every inter
            // predictor build, not just a `WARPED_CAUSAL`-selected one. Only
            // reachable when `warp_params` is still `None` here: a block
            // that read (and got) local `WARPED_CAUSAL` above already has
            // `local_warp_allowed && !wm_params.invalid`, which `allow_warp`
            // checks FIRST and short-circuits on -- the global branch is an
            // `else if`, never layered on top. `is_global_mv_block` already
            // encodes `allow_warp`'s size bound (`min(bw4,bh4) >= 2`, i.e.
            // >=8px both dims, matching `av1_init_warp_params`'s
            // `block_height < 8 || block_width < 8` early return) and, via
            // `motion_mode_eligible`'s own suppression above, the
            // `force_integer_mv` case never reads a motion_mode symbol at
            // all -- checked again here directly since this branch fires
            // independent of `motion_mode_eligible`.
            let gm_ref = &global_motion[(ref_frame - LAST_FRAME) as usize];
            if warp_params.is_none() && is_global_mv_block && !force_integer_mv && !gm_ref.invalid {
                warp_params = crate::warp::global_warp_params(gm_ref.params);
                if warp_params.is_some() && gm_ref.model == ec_av1_syntax::WarpModel::Affine {
                    AFFINE_GM_HITS.with(|c| c.set(c.get() + 1));
                }
            }
            if std::env::var_os("EC_AV1_TELL").is_some() {
                eprintln!(
                    "TELL mi_row={mi_row} mi_col={mi_col} label=post_motion_mode eligible={} tell={} range={}",
                    motion_mode_eligible as u8, dec.debug_bitpos(), dec.debug_state().0
                );
            }
            // spec `get_ref_filter_type`: a neighbour's own filter only feeds
            // this block's switchable_interp context when that neighbour coded
            // the SAME reference frame this block is about to read -- a
            // different-ref neighbour reads as "no neighbour" ([3, 3], the
            // sentinel `Neighbours::new` already seeds unset slots with),
            // exactly like an intra neighbour. Without this gate, a GOLDEN_FRAME
            // block next to a LAST_FRAME one inherits that neighbour's filter
            // choice into its own context, corrupting the symbol it reads.
            // lane-av1idx r2: also match on the neighbour's SECOND reference
            // (`above_ref1`/`left_ref1`, real for a compound neighbour) --
            // see the 16x16 leaf's own comment above for the spec citation.
            let above_filter_ctx = if neighbours.above_ref[cmi] == ref_frame
                || neighbours.above_ref1[cmi] == Some(ref_frame)
            {
                neighbours.above_filter[cmi]
            } else {
                [3, 3]
            };
            let left_filter_ctx = if neighbours.left_ref[rmi] == ref_frame
                || neighbours.left_ref1[rmi] == Some(ref_frame)
            {
                neighbours.left_filter[rmi]
            } else {
                [3, 3]
            };
            let (h_filter, v_filter, resolved_filter) = resolve_interp_filter(
                dec,
                cdfs,
                interp_fixed,
                enable_dual_filter,
                gm_nontrans || warped_selected,
                above_filter_ctx,
                left_filter_ctx,
                false,
            );
            block_filter = resolved_filter;
            globalmv_for_lf = is_globalmv;
            if std::env::var_os("EC_AV1_TELL").is_some() {
                eprintln!(
                    "TELL mi_row={mi_row} mi_col={mi_col} label=post_interp_filter tell={} range={}",
                    dec.debug_bitpos(), dec.debug_state().0
                );
            }
            if std::env::var_os("EC_AV1_TRACE").is_some() {
                eprintln!(
                    "EC_TRACE mi_row={mi_row} mi_col={mi_col} skip={} is_inter=1 mv=({},{}) is_new_mv={is_new_mv} bsize={side} ref={ref_frame} filter={:?} motion_mode_eligible={} obmc_selected={} warped={} tell={}",
                    skip as u8, mv.0, mv.1, block_filter, motion_mode_eligible as u8, obmc_selected as u8, warp_params.is_some() as u8, dec.debug_bitpos()
                );
            }
            // lane-rect r2: see the compound path's matching comment above --
            // `grid` must be stamped with the block's true bh4/bw4 span, not
            // a square `bw4`x`bw4` guess.
            for dr in 0..bh4 {
                for dc in 0..bw4 {
                    grid.set(
                        mi_row + dr,
                        mi_col + dc,
                        MiInfo {
                            is_inter: true,
                            ref_frame,
                            // An interintra block records ref_frame[1] ==
                            // INTRA_FRAME (0): warp-sample gathering
                            // (libaom av1_findSamples, mvref_common.c:1155)
                            // requires ref_frame[1] == NONE_FRAME, so such a
                            // neighbour must not donate samples or count in
                            // num_proj_ref.
                            ref_frame1: interintra_mode.map(|_| 0),
                            mv1: None,
                            mv,
                            is_new_mv,
                            size: bw4,
                            size_h: bh4,
                            is_global_mv0: is_global_mv_block,
                            is_global_mv1: false,
                        },
                    );
                }
            }
            mode_for_tx = 0;
            uv_predict_mode = DC_PRED;

            // lane-superres r9: spec 7.11.3.3's x_scale_fp comes from LUMA
            // widths only (r8's derivation) and applies unchanged to the
            // chroma calls below (their x_q4 is already in the chroma
            // plane's own pixel units). `mc::predict_scaled` has no warp/OBMC
            // /interintra counterpart -- those three combinations are refused
            // by name instead of silently sampling a scaled reference wrong
            // (warp_params/obmc_selected/interintra_mode are all resolved by
            // this point, every symbol for this block already read).
            let luma_scale = mc::scale_factor(py_ref.width, frame_width);
            if luma_scale != mc::REF_NO_SCALE {
                // lane-scaledref r1: libaom `allow_warp`
                // (av1/common/reconinter.c:41) suppresses BOTH local and
                // global warp under a scaled reference and predicts
                // translationally instead -- that fallback is implemented
                // below, but the one real aomenc stream this round found
                // that reaches it (`--enable-warped-motion=1`,
                // `--superres-denominator=16`, seed 47) still mismatches
                // ffmpeg at frame 2 luma, so the case stays refused by name
                // until it has a green gate rather than shipping wrong
                // pixels behind a lifted refusal.
                if warp_params.is_some() {
                    SCALED_WARP_FALLBACK_HITS.with(|c| c.set(c.get() + 1));
                    return Err(unsupported(
                        "warp prediction with a scaled reference (superres, unimplemented)",
                    ));
                }
                if obmc_selected {
                    SCALED_OBMC_HITS.with(|c| c.set(c.get() + 1));
                }
                if interintra_mode.is_some() {
                    SCALED_INTERINTRA_HITS.with(|c| c.set(c.get() + 1));
                }
            }

            let mut pred_y = vec![0u16; side * side];
            let mut pred_u = vec![0u16; chroma_side * chroma_side];
            let mut pred_v = vec![0u16; chroma_side * chroma_side];
            if luma_scale == mc::REF_NO_SCALE {
                mc::predict_with_filters(
                    &py_ref.data,
                    py_ref.width,
                    py_ref.true_width,
                    py_ref.true_height,
                    mv_to_q4(px, mv.1, true),
                    mv_to_q4(py, mv.0, true),
                    side,
                    side,
                    h_filter,
                    v_filter,
                    &mut pred_y,
                );
                mc::predict_with_filters(
                    &pu_ref.data,
                    pu_ref.width,
                    pu_ref.true_width,
                    pu_ref.true_height,
                    mv_to_q4(cpx, mv.1, false),
                    mv_to_q4(cpy, mv.0, false),
                    chroma_side,
                    chroma_side,
                    h_filter,
                    v_filter,
                    &mut pred_u,
                );
                mc::predict_with_filters(
                    &pv_ref.data,
                    pv_ref.width,
                    pv_ref.true_width,
                    pv_ref.true_height,
                    mv_to_q4(cpx, mv.1, false),
                    mv_to_q4(cpy, mv.0, false),
                    chroma_side,
                    chroma_side,
                    h_filter,
                    v_filter,
                    &mut pred_v,
                );
            } else {
                mc::predict_scaled(
                    &py_ref.data,
                    py_ref.width,
                    py_ref.true_width,
                    py_ref.true_height,
                    mv_to_q4(px, mv.1, true),
                    mv_to_q4(py, mv.0, true),
                    luma_scale,
                    side,
                    side,
                    h_filter,
                    v_filter,
                    &mut pred_y,
                );
                mc::predict_scaled(
                    &pu_ref.data,
                    pu_ref.width,
                    pu_ref.true_width,
                    pu_ref.true_height,
                    mv_to_q4(cpx, mv.1, false),
                    mv_to_q4(cpy, mv.0, false),
                    luma_scale,
                    chroma_side,
                    chroma_side,
                    h_filter,
                    v_filter,
                    &mut pred_u,
                );
                mc::predict_scaled(
                    &pv_ref.data,
                    pv_ref.width,
                    pv_ref.true_width,
                    pv_ref.true_height,
                    mv_to_q4(cpx, mv.1, false),
                    mv_to_q4(cpy, mv.0, false),
                    luma_scale,
                    chroma_side,
                    chroma_side,
                    h_filter,
                    v_filter,
                    &mut pred_v,
                );
            }

            // lane-scaledref r1: libaom `allow_warp` (av1/common/reconinter.c:41)
            // opens with `if (av1_is_scaled(sf)) return 0;` -- BOTH local
            // (`wm_params`) and global warp fall back to the translational
            // prediction above when the reference is scaled, even though the
            // motion_mode symbol was still read and warp_params still
            // resolved. Suppress the warp, keep every symbol read.
            if let Some(params) = warp_params.as_ref().filter(|_| luma_scale == mc::REF_NO_SCALE) {
                crate::warp::warp_affine(
                    params, &py_ref.data, py_ref.true_width as i32, py_ref.true_height as i32,
                    py_ref.width as i32, &mut pred_y, px as i32, py as i32, side as i32,
                    side as i32, side as i32, 0, 0,
                );
                // libaom `av1_init_warp_params` (reconinter.c): warp is
                // per PLANE and bails out at `block_width < 8 ||
                // block_height < 8`, so a plane whose own block is under
                // 8x8 keeps the translational prediction built above --
                // in 420 that is every chroma plane of a luma block below
                // 16x16 (lane-gmaffine r4: chroma-only mismatch of a few
                // levels on both 8x8-leaf motion gates).
                if chroma_side >= 8 {
                    crate::warp::warp_affine(
                        params, &pu_ref.data, pu_ref.true_width as i32, pu_ref.true_height as i32,
                        pu_ref.width as i32, &mut pred_u, cpx as i32, cpy as i32, chroma_side as i32,
                        chroma_side as i32, chroma_side as i32, 1, 1,
                    );
                    crate::warp::warp_affine(
                        params, &pv_ref.data, pv_ref.true_width as i32, pv_ref.true_height as i32,
                        pv_ref.width as i32, &mut pred_v, cpx as i32, cpy as i32, chroma_side as i32,
                        chroma_side as i32, chroma_side as i32, 1, 1,
                    );
                }
            }

            if obmc_selected {
                obmc_blend(
                    grid,
                    neighbours,
                    mi_row,
                    mi_col,
                    bw4,
                    bh4,
                    mi_rows as usize,
                    mi_cols as usize,
                    side,
                    write_w,
                    write_h,
                    chroma_side,
                    px,
                    py,
                    cpx,
                    cpy,
                    ref_y,
                    ref_u,
                    ref_v,
                    other_refs,
                    interp_fixed,
                    frame_width,
                    &mut pred_y,
                    &mut pred_u,
                    &mut pred_v,
                )?;
            }

            // Mutually exclusive with warp/OBMC (motion_mode is not read for
            // an interintra block): blend the intra predictor over the MC
            // result, all planes (reconinter.c av1_build_interintra_predictor
            // plane loop).
            if let Some(ii) = interintra_mode {
                interintra_blend(y, px, py, side, ii, wedge_mask, &mut pred_y);
                interintra_blend(u, cpx, cpy, chroma_side, ii, wedge_mask, &mut pred_u);
                interintra_blend(v, cpx, cpy, chroma_side, ii, wedge_mask, &mut pred_v);
            }

            vartx_leaves = read_block_tx_size(
                dec,
                cdfs,
                neighbours,
                at_mi,
                side,
                (mi_cols as usize, mi_rows as usize),
                true,
                skip,
            )?
            .1;
            if skip {
                y.reconstruct_mc_rect(
                    px,
                    py,
                    side,
                    write_w,
                    write_h,
                    &pred_y,
                    &vec![0i32; side * side],
                );
                u.reconstruct_mc_rect(
                    cpx,
                    cpy,
                    chroma_side,
                    write_chroma_w,
                    write_chroma_h,
                    &pred_u,
                    &vec![0i32; chroma_side * chroma_side],
                );
                v.reconstruct_mc_rect(
                    cpx,
                    cpy,
                    chroma_side,
                    write_chroma_w,
                    write_chroma_h,
                    &pred_v,
                    &vec![0i32; chroma_side * chroma_side],
                );
                luma_grid = vec![0i32; side * side];
                u_grid = vec![0i32; chroma_side * chroma_side];
                v_grid = vec![0i32; chroma_side * chroma_side];
            } else {
                let around = neighbours.around(at, side);
                let luma_tx_type;
                if let Some(leaves) = vartx_leaves.clone() {
                    // spec 5.11.17's transform tree, leaf by leaf in the order
                    // `read_var_tx_size` coded it: each unit reads its own
                    // coefficients against the neighbour magnitudes the
                    // earlier units already wrote, and reconstructs over its
                    // own slice of the block's motion-compensated predictor.
                    let reduced = REDUCED_TX_SET_INTER.with(std::cell::Cell::get);
                    let mut first_tx_type = TxType::DctDct;
                    for (idx, &(row, col, tx_px)) in leaves.iter().enumerate() {
                        let tu_mi = (at_mi.0 + row, at_mi.1 + col);
                        let (tu_px, tu_py) = (px + col * MI, py + row * MI);
                        let mut tu_pred = Vec::with_capacity(tx_px * tx_px);
                        for rr in 0..tx_px {
                            let start = (row * MI + rr) * side + col * MI;
                            tu_pred.extend_from_slice(&pred_y[start..start + tx_px]);
                        }
                        let scan = default_scan(tx_px);
                        let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, tx_px / MI);
                        let (tu_grid, tu_tx_type) = read_inter_plane(
                            dec,
                            cdfs,
                            inter_txbset_for(tx_px, reduced),
                            &scan,
                            0,
                            neighbours.around_mi(tu_mi, tx_px)[0],
                            mode_for_tx,
                            y,
                            tu_px,
                            tu_py,
                            tx_px,
                            base_q_idx,
                            &tu_pred,
                            None,
                            Some(tu_skip_ctx),
                        )?;
                        if idx == 0 {
                            first_tx_type = tu_tx_type;
                        }
                        neighbours.record_mi_luma(tu_mi, tx_px, &tu_grid);
                    }
                    // `av1_get_tx_type`'s chroma lookup scales the chroma
                    // position back to luma, so an inter block's chroma
                    // inherits the *first* (top-left) luma unit's coded type.
                    luma_tx_type = first_tx_type;
                    luma_grid = vec![0i32; side * side];
                } else {
                    (luma_grid, luma_tx_type) = read_inter_plane(
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
                        None,
                        None,
                    )?;
                }
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
                    Some(luma_tx_type),
                    None,
                )?
                .0;
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
                    Some(luma_tx_type),
                    None,
                )?
                .0;
            }
        }
    } else {
        // lane-partitions r1: intra prediction (`PlaneBuf::reconstruct`) has
        // no rectangular counterpart -- an intra-coded HORZ/VERT strip needs
        // its own true-width/height edge/predict math, unlike the inter
        // skip path above (which only needed a clipped write of an
        // already-square-predicted buffer). Named refusal instead of a
        // silently wrong square-shaped intra prediction.
        if write_w != side || write_h != side {
            return Err(unsupported(
                "an intra-coded HORZ/VERT strip needs rectangular intra prediction \
                 this decoder does not code yet",
            ));
        }
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
        if (9..=12).contains(&uv_mode) {
            SMOOTH_UV_HITS.with(|c| c.set(c.get() + 1));
        }
        let alpha = if uv_mode == DC_PRED {
            None
        } else if uv_mode == UV_CFL_PRED {
            Some(read_cfl_alphas(dec, cdfs))
        } else {
            None
        };
        // `get_uv_mode` (spec 9.3): `UV_CFL_PRED` predicts as `DC_PRED` for
        // the angle-delta question, same as [`read_intra_mode`]'s own copy
        // -- `uv_mode` (13) already falls outside `V_PRED..=D67_PRED` so the
        // `else` branch below is exact for it either way.
        let angle_delta_uv = if (V_PRED..=D67_PRED).contains(&uv_mode) {
            DIRECTIONAL_UV_HITS.with(|c| c.set(c.get() + 1));
            read_angle_delta(dec, &mut cdfs.angle_delta[uv_mode - V_PRED])
        } else {
            0
        };
        if angle_delta_uv != 0 {
            UV_ANGLE_DELTA_HITS.with(|c| c.set(c.get() + 1));
        }
        uv_predict_mode = if uv_mode == UV_CFL_PRED {
            DC_PRED
        } else {
            uv_mode
        };
        // lane-chroma r4: same chroma edge-filter-strength neighbour check
        // as [`read_intra_mode`]/[`read_intra_mode_rect`] -- the CHROMA
        // neighbour's own `uv_mode`, not the luma one.
        let smooth_neighbor_uv =
            neighbours.smooth_uv_neighbour(r * (SUB / MI), c * (SUB / MI), r, c);
        // `read_palette_mode_info` (decodemv.c:567, called from
        // `read_intra_block_mode_info` right after `xd->cfl.store_y`, same
        // gating and same corner-cut as `read_intra_mode`'s own copy above
        // -- `palette_mode_ctx`/`palette_uv_mode_ctx` hardcoded 0, provably
        // safe since a nonzero neighbour `palette_size` would already have
        // refused the decode that produced it.
        if allow_screen_content_tools
            && let Some(bsize_ctx) = palette_bsize_ctx(side)
        {
            if mode == DC_PRED && dec.symbol(&mut cdfs.palette_y_mode[bsize_ctx][0]) != 0 {
                return Err(unsupported(
                    "a block that actually uses a palette (Y) -- reconstruction is out of scope",
                ));
            }
            if uv_mode == DC_PRED && dec.symbol(&mut cdfs.palette_uv_mode[0]) != 0 {
                return Err(unsupported(
                    "a block that actually uses a palette (UV) -- reconstruction is out of scope",
                ));
            }
        }
        mode_for_tx = mode;
        block_filter = [3, 3];
        ref_frame_for_lf = 0;
        globalmv_for_lf = false;
        let (mi_row, mi_col) = (r * SUB_MI as usize, c * SUB_MI as usize);
        for dr in 0..side / 4 {
            for dc in 0..side / 4 {
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
                        size: side / 4,
                        size_h: side / 4,
                        is_global_mv0: false,
                        is_global_mv1: false,
                    },
                );
            }
        }

        read_block_tx_size(
            dec,
            cdfs,
            neighbours,
            at_mi,
            side,
            (mi_cols as usize, mi_rows as usize),
            false,
            skip,
        )?;
        let reach = Reach::of(side, px, py, y.width, y.height);
        if skip {
            y.reconstruct(
                px,
                py,
                side,
                mode,
                0,
                reach,
                &vec![0i32; side * side],
                None,
                None,
                false,
            );
            let ac = alpha.map(|_| cfl_ac_q3(y, px, py, side));
            u.reconstruct(
                cpx,
                cpy,
                chroma_side,
                uv_predict_mode,
                angle_delta_uv,
                reach,
                &vec![0i32; chroma_side * chroma_side],
                alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
                None,
                smooth_neighbor_uv,
            );
            v.reconstruct(
                cpx,
                cpy,
                chroma_side,
                uv_predict_mode,
                angle_delta_uv,
                reach,
                &vec![0i32; chroma_side * chroma_side],
                alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
                None,
                smooth_neighbor_uv,
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
                0,
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
                false,
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
                uv_predict_mode,
                angle_delta_uv,
                reach,
                u,
                cpx,
                cpy,
                chroma_side,
                chroma_tx,
                base_q_idx,
                alpha.zip(ac.as_deref()).map(|((au, _), ac)| (au, ac)),
                None,
                None,
                smooth_neighbor_uv,
            )?;
            v_grid = read_plane(
                dec,
                cdfs,
                chroma_set,
                scan_chroma,
                2,
                around[2],
                mode,
                uv_predict_mode,
                angle_delta_uv,
                reach,
                v,
                cpx,
                cpy,
                chroma_side,
                chroma_tx,
                base_q_idx,
                alpha.zip(ac.as_deref()).map(|((_, av), ac)| (av, ac)),
                None,
                None,
                smooth_neighbor_uv,
            )?;
        }
    }
    // lane-comppin r9: end-of-block checkpoint -- everything between
    // post_assign_mv and here (interintra, motion_mode, interp_filter, then
    // every plane's coeffs) is unproven; a range mismatch here vs aomdec's
    // equivalent post-content point (added at the matching call site)
    // narrows the ladder to this block's own tail instead of the next
    // block's partition read.
    if std::env::var_os("EC_AV1_TELL").is_some() {
        eprintln!(
            "TELL mi_row={} mi_col={} label=block_end tell={} range={}",
            r * SUB_MI as usize, c * SUB_MI as usize, dec.debug_bitpos(), dec.debug_state().0
        );
    }
    if vartx_leaves.is_some() {
        // Plane 0 is already correct per transform unit
        // ([`Neighbours::record_mi_luma`] above); this writes everything else
        // [`Neighbours::record_rect`] would (a var-tx block is always square,
        // so `write_w == write_h == side`).
        neighbours.record_split_luma(at, side, mode_for_tx, uv_predict_mode, [&u_grid, &v_grid]);
    } else {
        neighbours.record_rect(
            at,
            write_w,
            write_h,
            mode_for_tx,
            uv_predict_mode,
            &[luma_grid, u_grid, v_grid],
        );
    }
    neighbours.record_inter_rect(
        at,
        write_w,
        write_h,
        skip,
        is_inter,
        ref_frame_for_lf,
        block_filter,
        skip_mode,
    );
    if let Some((ref1, group_idx, idx)) = compound_ctx {
        neighbours.record_compound_ctx_rect(at, write_w, write_h, ref1, group_idx, idx);
    }
    neighbours.fill_skip_grid_rect(
        (r * (SUB / MI), c * (SUB / MI)),
        write_w / MI,
        write_h / MI,
        skip,
    );
    neighbours.fill_lf_grid_rect(
        (r * (SUB / MI), c * (SUB / MI)),
        write_w / MI,
        write_h / MI,
        // lane-rect r2: the strip's true TX_32X16/TX_16X32 dims -- the
        // single-scalar corner-cut deblocked the strip seam with the wrong
        // edge length (1-px chroma drift on rect-flake-1 f17+).
        write_w as u8,
        write_h as u8,
        // ref_grid's packed convention (lf_level): sign carries GLOBALMV,
        // whose mode_lf_lut row is 0, not the 1 every other inter mode gets.
        if globalmv_for_lf {
            -ref_frame_for_lf
        } else {
            ref_frame_for_lf
        },
    );
    // lane-txselect: a var-tx block's deblock edges follow its own transform
    // units, not the block. Chroma still reads `tx_h_grid / 2` (the block's
    // uv transform is not split) -- a known corner-cut of this round.
    if let Some(leaves) = &vartx_leaves {
        for &(row, col, tx_px) in leaves {
            neighbours.fill_lf_grid_rect(
                (at_mi.0 + row, at_mi.1 + col),
                tx_px / MI,
                tx_px / MI,
                tx_px as u8,
                tx_px as u8,
                if globalmv_for_lf {
                    -ref_frame_for_lf
                } else {
                    ref_frame_for_lf
                },
            );
        }
    }
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
    other_refs: &RefSlots,
    base_q_idx: u8,
    scan8: &[u16],
    scan4: &[u16],
    allow_high_precision_mv: bool,
    force_integer_mv: bool,
    // lane-gmaffine r1: this frame header's own `global_motion` table (spec
    // 5.9.24), indexed `[ref_frame - LAST_FRAME]` -- same table
    // [`decode_inter_block`] takes, threaded here so the 8x8 leaf can code
    // `GLOBALMV` (`gm_get_motion_vector`), fill its mv stack's missing
    // candidates with the gm mv (gm r6's root cause) and build a global
    // warp prediction.
    global_motion: &[ec_av1_syntax::WarpParams; 7],
    // lane-gmaffine r1: this sequence header's own `enable_dual_filter` bit,
    // for the leaf's own `read_mb_interp_filter` below.
    enable_dual_filter: bool,
    // lane-av1comp: see [`decode_inter_block`]'s own doc -- this leaf path
    // only ever tracks a coarse `LAST_FRAME`-or-intra neighbour shape (no
    // per-leaf `above_ref`/`left_ref` array exists here), so `comp_mode`'s
    // context uses that same approximation.
    reference_select: bool,
    enable_masked_compound: bool,
    // lane-sb128 r4: see [`decode_inter_block`]'s own doc -- `BLOCK_8X8` is
    // always `is_interintra_allowed_bsize`-eligible, so this leaf's own gate
    // drops the `side` check entirely.
    enable_interintra_compound: bool,
    enable_jnt_comp: bool,
    order_hint_bits: u32,
    order_hint: u32,
    ref_order_hints: [u32; 7],
    // lane-av1comp round 14: see [`decode_inter_block`]'s own doc -- this
    // leaf has no per-leaf `above_skip_mode`/`left_skip_mode` array either,
    // so its `skip_mode` context reuses the same [`SUB`]-grid
    // `above_skip_mode[cmi]`/`left_skip_mode[rmi]` the enclosing 16x16 slot
    // tracks (mirroring `above_skip`/`left_skip`'s own leaf approximation
    // just above).
    skip_mode_present: bool,
    skip_mode_frame: [u8; 2],
    // lane-cdffwd2: this frame header's own `reduced_tx_set` bit -- see
    // [`decode_inter_frame_tile_with_cdfs`]'s own doc.
    reduced_tx_set: bool,
    // lane-motionmode round 3: this frame header's own `is_motion_mode_switchable`/
    // `allow_warped_motion` bits (spec 5.11.24's `read_motion_mode`), plus
    // the fixed/switchable interp filter kind `obmc_blend`'s own neighbour
    // reads need (see [`decode_inter_block`]'s own doc) -- this leaf's own
    // prediction is always `Regular` (documented corner-cut, `mc::predict`
    // has no filter param), but an OBMC neighbour can be a real 16x16+
    // block that DID resolve a switchable filter.
    interp_fixed: Option<mc::InterpFilterKind>,
    switchable_motion_mode: bool,
    allow_warped_motion: bool,
    // lane-av1comp: `Some((ref1, comp_group_idx, compound_idx))` only for a
    // real `COMPOUND_REFERENCE` leaf, for the caller's own end-of-16x16
    // `record_compound_ctx` call -- mirrors [`decode_inter_block`]'s own
    // `compound_ctx` local, just handed back instead of applied here (this
    // fn's `neighbours` recording happens once for the whole straddling
    // 16x16 block, after every leaf).
    // lane-screen r2: see [`decode_inter_block`]'s own copy of this param --
    // `BLOCK_8X8` (this leaf's fixed size) is always `av1_allow_palette`-
    // eligible (`bsize >= BLOCK_8X8`).
    allow_screen_content_tools: bool,
    // lane-inter8 r1: the leaf's mv stacks used to be built without the
    // frame's sign-bias table (`NO_SIGN_BIAS`), while [`decode_inter_block`]
    // one level up passed it -- it changes the candidate list and with it
    // `new_mv_ctx`/`ref_mv_ctx`, `drl_ctx` and the number of symbols read.
    // Measured: an 8x8 compound NEAR_NEWMV leaf desynced against aomdec's
    // EC_MODE ladder at exactly this block (mi 4,14 of the mandelbrot
    // 8x8-split gate).
    sign_bias_table: &SignBiasTable,
    // lane-inter8 r2: the frame's temporal mv field (spec 7.10.2.8), which
    // [`decode_inter_block`] one level up has always passed and this leaf
    // passed `None` for -- a `use_ref_frame_mvs` frame's stack then holds
    // fewer candidates here than in the reference decoder, which changes the
    // mode contexts AND the number of `drl_mode` bits read (class parsed
    // then discarded).
    tpl_frame: Option<&TplFrameArgs>,
    // lane-scaledref r1: this frame's own coded luma width (spec 7.11.3.3),
    // for the scaled-reference MC this leaf's own single-ref/compound/OBMC
    // predictions take when a stored reference was coded at another width
    // (`use_superres`) -- mirrors [`decode_inter_block`]'s own param.
    frame_width: usize,
) -> Result<(bool, bool, bool, Option<(i8, i8, u8, u8)>, [u8; 2], (i8, Option<i8>))> {
    const LAST_FRAME: i8 = 1;
    // lane-gmaffine r2: the leaf's OWN switchable-filter symbols, handed back
    // so the caller stamps them into `Neighbours` instead of the `[3, 3]`
    // placeholder. `[3, 3]` is the "intra neighbour, no filter" sentinel: an
    // OBMC blend that picks such a neighbour up feeds it to
    // `neighbour_filter`, which PANICS on it (`from_switchable_symbol`) --
    // that was the warp gate's crash, not an entropy desync. Stays `[3, 3]`
    // on the intra and compound paths.
    let mut leaf_filter_syms = [3u8; 2];
    // lane-gmaffine r1: this leaf's GLOBALMV vector AND the mv stack's
    // missing-candidate fallback both come from the frame's global motion
    // evaluated at THIS block's centre (gm r6's root cause).
    let gm_table = build_gm_mv_table(
        global_motion,
        leaf_mi.0,
        leaf_mi.1,
        2,
        2,
        allow_high_precision_mv,
        force_integer_mv,
    );
    /// `Y_MODE`'s size group (`common_data.h`'s `size_group_lookup[BLOCK_8X8]`).
    const SIZE_GROUP_8: usize = 1;

    let (px, py) = (leaf_mi.1 * MI, leaf_mi.0 * MI);
    let (cpx, cpy) = (px / 2, py / 2);
    // lane-txselect: whether this leaf's `TxMode::Select` var-tx tree took
    // the one split a `BLOCK_8X8` can take (`read_block_tx_size` below) --
    // read back by the luma residual read and the tail's neighbour/loop-filter
    // bookkeeping. Always `false` outside a `TxMode::Select` inter frame.
    let mut split8 = false;
    const SIDE: usize = 8;
    const CHROMA_SIDE: usize = 4;

    let (r, c) = outer_at;
    let _ = (r, c);
    // lane-inter8 r2: this leaf's OWN mi cell, not the enclosing 16x16's --
    // leaf (mi_r, mi_c + 2)'s left neighbour is leaf (mi_r, mi_c), and the
    // left column of a 16x16's second leaf ROW is the previous 16x16 block's
    // bottom leaf, neither of which a SUB-grid slot can name.
    let (rmi, cmi) = leaf_mi;
    // lane-inter8 r2: no sibling-leaf override any more -- each leaf stamps
    // its own 2x2-mi span into the bands right after it decodes (the caller's
    // `record_inter_rect_mi`), so a plain mi read already names the correct
    // neighbour, including one belonging to the previous 16x16 block.
    let above_skip = neighbours.above_skip[cmi];
    let left_skip = neighbours.left_skip[rmi];
    let above_inter = neighbours.above_inter[cmi];
    let left_inter = neighbours.left_inter[rmi];
    let above_ref0 = neighbours.above_ref[cmi];
    let above_ref1 = neighbours.above_ref1[cmi];
    let left_ref0 = neighbours.left_ref[rmi];
    let left_ref1 = neighbours.left_ref1[rmi];
    let skip_mode_ctx =
        usize::from(neighbours.above_skip_mode[cmi]) + usize::from(neighbours.left_skip_mode[rmi]);
    // Same two `inter_segment_id` positions as [`decode_inter_block`]; this
    // leaf is always `BLOCK_8X8`, i.e. 2x2 mi.
    inter_segment_id(dec, cdfs, leaf_mi.0, leaf_mi.1, 2, 2, false, true);
    let skip_mode = skip_mode_present && dec.symbol(&mut cdfs.skip_mode[skip_mode_ctx]) == 1;
    if skip_mode {
        SKIP_MODE_HITS.with(|c| c.set(c.get() + 1));
    }

    let skip_ctx = usize::from(above_skip) + usize::from(left_skip);
    let skip = skip_mode || dec.symbol(&mut cdfs.skip[skip_ctx]) == 1;
    inter_segment_id(dec, cdfs, leaf_mi.0, leaf_mi.1, 2, 2, skip, false);
    maybe_read_cdef_idx(dec, leaf_mi.0, leaf_mi.1, skip);
    // Always a `BLOCK_8X8` leaf here, never the whole superblock.
    maybe_read_delta_q(dec, cdfs, leaf_mi.0, leaf_mi.1, false, skip);
    maybe_read_delta_lf(dec, cdfs, leaf_mi.0, leaf_mi.1, false, skip);

    let (has_above, has_left) = (
        leaf_mi.0 > neighbours.tile_row0_mi,
        leaf_mi.1 > neighbours.tile_col0_mi,
    );
    let ii_ctx = intra_inter_ctx(has_above, has_left, above_inter, left_inter);
    let is_inter = skip_mode || dec.symbol(&mut cdfs.intra_inter[ii_ctx]) == 1;
    // lane-inter8 r1: mirrors the oracle's `EC_MODE` (instrument rung 4,
    // `read_inter_block_mode_info`) so an 8x8 leaf's mode-info range ladder
    // diffs line for line against a real aomdec.
    if std::env::var_os("EC_TRACE_MODE").is_some() {
        eprintln!(
            "EC_MODE mi_row={} mi_col={} rng={}",
            leaf_mi.0,
            leaf_mi.1,
            dec.debug_state().0
        );
    }

    let mode_for_tx;
    let (luma_grid, u_grid, v_grid);
    let mut compound_ctx8: Option<(i8, i8, u8, u8)> = None;
    // lane-inter8 r1: this leaf's own resolved references, handed back so the
    // NEXT leaf of the same 16x16 can use them as its real above/left
    // neighbour (`Neighbours`' arrays only move once per 16x16 block).
    // `(0, None)` is intra.
    let mut leaf_refs: (i8, Option<i8>) = (0, None);
    if is_inter {
        if reference_select || skip_mode {
            // lane-inter8 r1: the REAL neighbour references, not a
            // `LAST_FRAME`-or-intra stand-in. `av1_get_reference_mode_context`
            // and `read_compound_ref_frames`' own contexts both ask whether a
            // neighbour is COMPOUND, so pretending every inter neighbour is
            // single-ref LAST picked the wrong `comp_mode` CDF row -- measured
            // as an 8x8 leaf reading single-ref where aomdec read compound
            // NEAR_NEWMV (mi 4,14).
            let above_nbr = has_above.then(|| NeighbourRef {
                is_inter: above_inter,
                ref0: if above_inter { above_ref0 } else { 0 },
                ref1: above_ref1,
                uni: above_ref1.is_some_and(|r1| is_uni_comp_ref(above_ref0, r1)),
            });
            let left_nbr = has_left.then(|| NeighbourRef {
                is_inter: left_inter,
                ref0: if left_inter { left_ref0 } else { 0 },
                ref1: left_ref1,
                uni: left_ref1.is_some_and(|r1| is_uni_comp_ref(left_ref0, r1)),
            });
            if skip_mode || read_comp_mode(dec, cdfs, above_nbr, left_nbr) {
                let (ref0, ref1) = if skip_mode {
                    (skip_mode_frame[0] as i8, skip_mode_frame[1] as i8)
                } else {
                    read_compound_ref_frames(
                        dec,
                        cdfs,
                        above_nbr,
                        left_nbr,
                        if has_above && above_inter {
                            LAST_FRAME
                        } else {
                            -1
                        },
                        if has_left && left_inter {
                            LAST_FRAME
                        } else {
                            -1
                        },
                    )
                };
                leaf_refs = (ref0, Some(ref1));
                let (mi_row, mi_col) = leaf_mi;
                // spec 7.10.2.8, mirroring [`decode_inter_block`]'s own
                // `comp_tpl` build (`use_ref_frame_mvs` frames only).
                let comp_tpl = tpl_frame.map(|t| crate::mvstack::CompoundTplArgs {
                    field: t.field,
                    cur_offset_0: crate::motion_field::get_relative_dist(
                        t.order_hint_bits,
                        t.order_hint,
                        t.ref_order_hints[(ref0 - LAST_FRAME) as usize],
                    ),
                    cur_offset_1: crate::motion_field::get_relative_dist(
                        t.order_hint_bits,
                        t.order_hint,
                        t.ref_order_hints[(ref1 - LAST_FRAME) as usize],
                    ),
                    allow_high_precision_mv,
                });
                // lane-av1comp: this leaf path has neither a real sign-bias
                // table nor `tpl_frame` (see this function's own doc on the
                // coarse `LAST_FRAME`-or-intra neighbour approximation
                // already in force above), matching the plain `find_mv_stack`
                // (no bias, no tpl) the leaf's single-ref arm below already
                // uses.
                let comp_stack = crate::mvstack::find_mv_stack_compound(
                    grid,
                    mi_row,
                    mi_col,
                    2,
                    2,
                    (ref0, ref1),
                    mi_cols as usize,
                    mi_rows as usize,
                    sign_bias_table,
                    &gm_table,
                    comp_tpl,
                );
                let compound_mode = if skip_mode {
                    0 // NEAREST_NEARESTMV, forced -- no mode symbol read
                } else {
                    read_inter_compound_mode(
                        dec,
                        cdfs,
                        comp_stack.new_mv_ctx,
                        comp_stack.ref_mv_ctx,
                    )
                };
                if std::env::var_os("EC_TRACE_MODE").is_some() {
                    eprintln!(
                        "EC_LEAFMODE mi_row={} mi_col={} cmode={} stack={} newmv_ctx={} refmv_ctx={} rng={}",
                        leaf_mi.0,
                        leaf_mi.1,
                        compound_mode + 17,
                        comp_stack.entries.len(),
                        comp_stack.new_mv_ctx,
                        comp_stack.ref_mv_ctx,
                        dec.debug_state().0
                    );
                }
                let (mv0, mv1) = assign_compound_mv(
                    dec,
                    cdfs,
                    &comp_stack,
                    compound_mode,
                    allow_high_precision_mv,
                    force_integer_mv,
                    // lane-inter8 r1: the real per-ref gm mvs, same as
                    // [`decode_inter_block`]'s own compound arm.
                    (
                        gm_table[(ref0 - LAST_FRAME) as usize],
                        gm_table[(ref1 - LAST_FRAME) as usize],
                    ),
                );
                // lane-inter8 r3: byte-format-identical to the oracle's
                // rung-4 `EC_MODE_VAL`, so the leaf's mode+mv VALUE ladder
                // diffs line for line against a real aomdec.
                if std::env::var_os("EC_TRACE_MODE").is_some() {
                    eprintln!(
                        "EC_MODE_VAL mi_row={} mi_col={} mode={} ref0={} ref1={} mv0=({},{}) rng={}",
                        leaf_mi.0,
                        leaf_mi.1,
                        compound_mode + 17,
                        ref0,
                        ref1,
                        mv0.0,
                        mv0.1,
                        dec.debug_state().0
                    );
                }

                // spec 5.11.25: same gating [`decode_inter_block`]'s own
                // compound arm uses, keyed off `outer_at`'s [`SUB`]-grid
                // slot -- the only granularity `Neighbours`' comp_group_idx/
                // compound_idx arrays track.
                let group_ctx = get_comp_group_idx_context(neighbours, leaf_mi, SIDE);
                let comp_group_idx = if !skip_mode
                    && enable_masked_compound
                    && is_any_masked_compound_used_here(SIDE)
                {
                    dec.symbol(&mut cdfs.comp_group_idx[group_ctx])
                } else {
                    0
                };
                // lane-maskcomp r2 / lane-wedge r3: see the 16x16/32x32
                // leaf's own comment.
                let mut diffwtd_mask_type: Option<u8> = None;
                let mut wedge_mask: Option<&'static [u8]> = None;
                if comp_group_idx == 1 {
                    // lane-maskcomp r1: see the 16x16 leaf's own comment
                    // above -- SIDE==8 here, `wedge_bsize` index 3
                    // (BLOCK_8X8, `wedge_types > 0`).
                    let compound_type = dec.symbol(&mut cdfs.compound_type[3]);
                    if compound_type == 0 {
                        // COMPOUND_WEDGE: lane-wedge r3, see the 16x16/32x32
                        // leaf's own comment.
                        let wedge_index = dec.symbol(&mut cdfs.wedge_idx[3]);
                        let wedge_sign = dec.literal(1);
                        WEDGE_HITS.with(|c| c.set(c.get() + 1));
                        wedge_mask = Some(
                            crate::wedge::wedge_masks()
                                .codebook(SIDE)
                                .mask(wedge_sign as usize, wedge_index as usize),
                        );
                    } else {
                        let mask_type = dec.literal(1);
                        diffwtd_mask_type = Some(mask_type as u8);
                    }
                    MASKED_COMPOUND_HITS.with(|c| c.set(c.get() + 1));
                }
                // corner-cut (lane-av1comp r16/r17, lane-av1blend r1): same
                // reference-slot defect as the 16x16 leaf's own mask above
                // (r1: falsified as an MC/blend math bug, isolated to a DPB
                // slot/refresh_frame_flags issue around BWDREF_FRAME) -- see
                // that comment.
                // r3: same defect as the 16x16 leaf's own mask above -- see
                // that comment (compound_idx's decoded value, not the blend
                // math or reference-slot bookkeeping). r6/r7: same re-mask
                // as the 16x16 leaf above -- one real bug found and fixed
                // (mvstack.rs ref_ctx family), a second, still-unfound
                // compound_idx defect surfaces on the wide gate sweep.
                // lane-av1idx r1: same re-mask as the 16x16 leaf above --
                // see that comment.
                let (fwd_offset, bck_offset, compound_idx) = if comp_group_idx == 0
                    && !skip_mode
                    && enable_jnt_comp
                {
                    let idx_ctx = get_comp_index_context(
                        neighbours,
                        leaf_mi,
                        SIDE,
                        order_hint_bits,
                        order_hint,
                        ref_order_hints[(ref0 - LAST_FRAME) as usize],
                        ref_order_hints[(ref1 - LAST_FRAME) as usize],
                    );
                    let idx = dec.symbol(&mut cdfs.compound_idx[idx_ctx]);
                    if idx == 1 {
                        (8, 8, 1u8)
                    } else {
                        let (fwd, bck) = crate::compound::dist_wtd_comp_weight_assign(
                            order_hint_bits,
                            order_hint,
                            ref_order_hints[(ref0 - LAST_FRAME) as usize],
                            ref_order_hints[(ref1 - LAST_FRAME) as usize],
                        );
                        (fwd, bck, 0u8)
                    }
                } else {
                    (8, 8, 1u8)
                };
                compound_ctx8 = Some((ref0, ref1, comp_group_idx as u8, compound_idx));
                if std::env::var_os("EC_AV1_COMPIDX_DUMP").is_some() {
                    eprintln!(
                        "EC_COMPIDX mi_row={mi_row} mi_col={mi_col} bsize=8 mode={compound_mode} mv0=({},{}) mv1=({},{}) ref0={ref0} ref1={ref1} comp_group_idx={comp_group_idx} compound_idx={compound_idx} tell={}",
                        mv0.0, mv0.1, mv1.0, mv1.1, dec.debug_bitpos()
                    );
                }

                let (py0, pu0, pv0) = ref_planes(ref0, ref_y, ref_u, ref_v, other_refs)?;
                let (py1, pu1, pv1) = ref_planes(ref1, ref_y, ref_u, ref_v, other_refs)?;
                let scale0 = mc::scale_factor(py0.width, frame_width);
                let scale1 = mc::scale_factor(py1.width, frame_width);
                if scale0 != mc::REF_NO_SCALE || scale1 != mc::REF_NO_SCALE {
                    SCALED_COMPOUND_HITS.with(|c| c.set(c.get() + 1));
                    SCALED_BLOCK8_HITS.with(|c| c.set(c.get() + 1));
                }

                for dr in 0..2 {
                    for dc in 0..2 {
                        grid.set(
                            mi_row + dr,
                            mi_col + dc,
                            MiInfo {
                                is_inter: true,
                                ref_frame: ref0,
                                ref_frame1: Some(ref1),
                                mv1: Some(mv1),
                                mv: mv0,
                                is_new_mv: matches!(compound_mode, 2 | 3 | 4 | 5 | 7),
                                size: 2,
                                size_h: 2,
                                is_global_mv0: false,
                                is_global_mv1: false,
                            },
                        );
                    }
                }
                mode_for_tx = 0;

                // lane-av1comp corner-cut, matching this leaf's single-ref
                // arm below (`mc::predict`'s own fixed-Regular default):
                // `decode_inter_block8` never resolves a switchable filter,
                // so both compound taps use `Regular` too.
                use mc::InterpFilterKind::Regular;
                let mut inter0_y = vec![0i32; SIDE * SIDE];
                mc::predict_compound_intermediate(
                    &py0.data,
                    py0.width,
                    py0.true_width,
                    py0.true_height,
                    mv_to_q4(px, mv0.1, true),
                    mv_to_q4(py, mv0.0, true),
                    scale0,
                    SIDE,
                    SIDE,
                    Regular,
                    Regular,
                    &mut inter0_y,
                );
                let mut inter1_y = vec![0i32; SIDE * SIDE];
                mc::predict_compound_intermediate(
                    &py1.data,
                    py1.width,
                    py1.true_width,
                    py1.true_height,
                    mv_to_q4(px, mv1.1, true),
                    mv_to_q4(py, mv1.0, true),
                    scale1,
                    SIDE,
                    SIDE,
                    Regular,
                    Regular,
                    &mut inter1_y,
                );
                let mut pred_y = vec![0u16; SIDE * SIDE];
                let mut diffwtd_mask_y = Vec::new();
                let mask_y: Option<&[u8]> = if let Some(mask_type) = diffwtd_mask_type {
                    diffwtd_mask_y = vec![0u8; SIDE * SIDE];
                    mc::diffwtd_mask(&inter0_y, &inter1_y, mask_type == 1, &mut diffwtd_mask_y);
                    Some(diffwtd_mask_y.as_slice())
                } else {
                    wedge_mask
                };
                if let Some(mask_y) = mask_y {
                    mc::blend_masked_compound(
                        &inter0_y, &inter1_y, mask_y, SIDE, SIDE, SIDE, false, &mut pred_y,
                    );
                } else {
                    mc::combine_compound(&inter0_y, &inter1_y, fwd_offset, bck_offset, &mut pred_y);
                }

                let mut inter0_u = vec![0i32; CHROMA_SIDE * CHROMA_SIDE];
                mc::predict_compound_intermediate(
                    &pu0.data,
                    pu0.width,
                    pu0.true_width,
                    pu0.true_height,
                    mv_to_q4(cpx, mv0.1, false),
                    mv_to_q4(cpy, mv0.0, false),
                    scale0,
                    CHROMA_SIDE,
                    CHROMA_SIDE,
                    Regular,
                    Regular,
                    &mut inter0_u,
                );
                let mut inter1_u = vec![0i32; CHROMA_SIDE * CHROMA_SIDE];
                mc::predict_compound_intermediate(
                    &pu1.data,
                    pu1.width,
                    pu1.true_width,
                    pu1.true_height,
                    mv_to_q4(cpx, mv1.1, false),
                    mv_to_q4(cpy, mv1.0, false),
                    scale1,
                    CHROMA_SIDE,
                    CHROMA_SIDE,
                    Regular,
                    Regular,
                    &mut inter1_u,
                );
                let mut pred_u = vec![0u16; CHROMA_SIDE * CHROMA_SIDE];
                if let Some(mask_y) = mask_y {
                    mc::blend_masked_compound(
                        &inter0_u,
                        &inter1_u,
                        mask_y,
                        SIDE,
                        CHROMA_SIDE,
                        CHROMA_SIDE,
                        true,
                        &mut pred_u,
                    );
                } else {
                    mc::combine_compound(&inter0_u, &inter1_u, fwd_offset, bck_offset, &mut pred_u);
                }

                let mut inter0_v = vec![0i32; CHROMA_SIDE * CHROMA_SIDE];
                mc::predict_compound_intermediate(
                    &pv0.data,
                    pv0.width,
                    pv0.true_width,
                    pv0.true_height,
                    mv_to_q4(cpx, mv0.1, false),
                    mv_to_q4(cpy, mv0.0, false),
                    scale0,
                    CHROMA_SIDE,
                    CHROMA_SIDE,
                    Regular,
                    Regular,
                    &mut inter0_v,
                );
                let mut inter1_v = vec![0i32; CHROMA_SIDE * CHROMA_SIDE];
                mc::predict_compound_intermediate(
                    &pv1.data,
                    pv1.width,
                    pv1.true_width,
                    pv1.true_height,
                    mv_to_q4(cpx, mv1.1, false),
                    mv_to_q4(cpy, mv1.0, false),
                    scale1,
                    CHROMA_SIDE,
                    CHROMA_SIDE,
                    Regular,
                    Regular,
                    &mut inter1_v,
                );
                let mut pred_v = vec![0u16; CHROMA_SIDE * CHROMA_SIDE];
                if let Some(mask_y) = mask_y {
                    mc::blend_masked_compound(
                        &inter0_v,
                        &inter1_v,
                        mask_y,
                        SIDE,
                        CHROMA_SIDE,
                        CHROMA_SIDE,
                        true,
                        &mut pred_v,
                    );
                } else {
                    mc::combine_compound(&inter0_v, &inter1_v, fwd_offset, bck_offset, &mut pred_v);
                }

                split8 = read_block_tx_size(
                    dec,
                    cdfs,
                    neighbours,
                    leaf_mi,
                    SIDE,
                    (mi_cols as usize, mi_rows as usize),
                    true,
                    skip,
                )?
                .1
                .is_some();
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
                    let (grid8, luma_tx_type) = read_inter_luma8(
                        dec,
                        cdfs,
                        neighbours,
                        leaf_mi,
                        y,
                        px,
                        py,
                        &pred_y,
                        mode_for_tx,
                        base_q_idx,
                        scan8,
                        scan4,
                        reduced_tx_set,
                        split8,
                    )?;
                    luma_grid = grid8;
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
                        Some(luma_tx_type),
                        None,
                    )?
                    .0;
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
                        Some(luma_tx_type),
                        None,
                    )?
                    .0;
                }
                // lane-inter8 r2: the COMPOUND leaf used to return here
                // WITHOUT the end-of-function `record_mi`/`fill_lf_grid` the
                // single-ref path falls through to -- so a compound 8x8 leaf
                // left `above_side_mi`/`left_side_mi` (the next block's
                // partition context), the per-plane coefficient level state
                // and the loop-filter grid describing whatever block was
                // there before. Every leaf of the gate's stream is compound,
                // so the 16x16 below a split block read partition ctx 0
                // instead of 1 and mis-read PARTITION_SPLIT as NONE.
                neighbours.record_mi(leaf_mi, 8, &[luma_grid, u_grid, v_grid]);
                neighbours.fill_lf_grid(
                    leaf_mi,
                    2,
                    8,
                    if is_inter { leaf_refs.0.max(LAST_FRAME) } else { 0 },
                );
                return Ok((
                    skip,
                    is_inter,
                    skip_mode,
                    compound_ctx8,
                    leaf_filter_syms,
                    leaf_refs,
                ));
            }
        }
        // lane-gmaffine r1: the leaf reads the FULL `single_ref` tree
        // ([`read_single_ref`], the same one the 16x16+ leaf uses, with the
        // real per-neighbour p1..p6 contexts) instead of the old three-symbol
        // LAST-only probe plus a refusal -- aomenc picks GOLDEN/ALTREF for
        // 8x8 leaves constantly, so that refusal was what every 8x8 gate
        // actually stopped at.
        let ref_frame = read_single_ref(
            dec,
            cdfs,
            neighbours.above_ref[cmi],
            neighbours.above_ref1[cmi],
            neighbours.left_ref[rmi],
            neighbours.left_ref1[rmi],
        );
        leaf_refs = (ref_frame, None);
        let (sref_y, sref_u, sref_v) = ref_planes(ref_frame, ref_y, ref_u, ref_v, other_refs)?;

        let (mi_row, mi_col) = leaf_mi;
        // lane-inter8 r2: the frame's temporal mv candidates, keyed on THIS
        // leaf's own reference (lane-gmaffine r1 taught it non-LAST refs).
        let tpl = tpl_frame.map(|t| crate::mvstack::TplArgs {
            field: t.field,
            cur_offset_0: crate::motion_field::get_relative_dist(
                t.order_hint_bits,
                t.order_hint,
                t.ref_order_hints[(ref_frame - LAST_FRAME) as usize],
            ),
            allow_high_precision_mv,
        });
        let stack = crate::mvstack::find_mv_stack_with_sign_bias(
            grid,
            mi_row,
            mi_col,
            2,
            2,
            ref_frame,
            mi_cols as usize,
            mi_rows as usize,
            sign_bias_table,
            &gm_table,
            tpl,
        );

        let not_new = dec.symbol(&mut cdfs.new_mv[stack.new_mv_ctx]) == 1;
        let mut is_globalmv = false;
        let (mv, is_new_mv) = if !not_new {
            // NEWMV (spec 5.11.24's `read_drl_idx`, `RefMvIdx` starting at 0):
            // read at most two `drl_mode` bits, one per stack entry past the
            // first, stopping at the first `0`. The chosen index selects
            // which stack entry `read_mv`'s base predictor comes from
            // (spec 7.10.2.10 `assign_mv`'s `PredMv = RefStackMv[RefMvIdx]`)
            // -- entry 0 (`stack.pred_mv`) only when the loop never advances.
            let mut idx = 0usize;
            while idx < 2 && stack.entries.len() > idx + 1 {
                if dec.symbol(&mut cdfs.drl_mode[stack.drl_ctx[idx]]) == 0 {
                    break;
                }
                idx += 1;
            }
            let base_mv = stack.entries.get(idx).map_or(stack.pred_mv, |e| e.mv);
            (
                read_mv(
                    dec,
                    &mut cdfs.mv_comp,
                    &mut cdfs.mv_joint,
                    base_mv,
                    allow_high_precision_mv,
                    force_integer_mv,
                ),
                true,
            )
        } else {
            let not_zero = dec.symbol(&mut cdfs.zero_mv[stack.zero_mv_ctx]) == 1;
            is_globalmv = !not_zero;
            let mv = if !not_zero {
                // lane-gmaffine r1: GLOBALMV (spec 7.10.2.1) -- the same
                // `gm_get_motion_vector` value the 16x16+ leaf reads out of
                // its own `gm_table`, computed at this block's centre.
                GLOBALMV_HITS_8.with(|c| c.set(c.get() + 1));
                gm_table[(ref_frame - LAST_FRAME) as usize]
            } else {
                let nearest = dec.symbol(&mut cdfs.ref_mv[stack.ref_mv_ctx]) == 0;
                if nearest {
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
                }
            };
            (mv, false)
        };
        // lane-sb128 r4: `interintra` (spec 5.11.24) at the 8x8 leaf too --
        // see [`decode_inter_block`]'s own doc; `BLOCK_8X8` is always
        // `is_interintra_allowed_bsize`-eligible, single-ref-only branch
        // (compound already returned above), GLOBALMV already refused
        // (round 3), so the only real gate left is the header enable bit.
        let mut interintra_mode: Option<u8> = None;
        let mut wedge_mask: Option<(&'static [u8], usize)> = None;
        if enable_interintra_compound && !skip_mode {
            let interintra = dec.symbol(&mut cdfs.interintra[SIZE_GROUP_8]) == 1;
            if interintra {
                // lane-interintra r1: same read pair as the 16/32 site;
                // wedge cdf row = BLOCK_8X8 (index 3), read on block size
                // alone, never the `enable_interintra_wedge` seq bit.
                let ii = dec.symbol(&mut cdfs.interintra_mode[SIZE_GROUP_8]) as u8;
                let wedge = dec.symbol(&mut cdfs.wedge_interintra[3]) == 1;
                if wedge {
                    // lane-wii r2: same adapting `wedge_index` symbol as the
                    // 16/32 leaf, fixed sign 0 -- see that site's comment.
                    let wedge_index = dec.symbol(&mut cdfs.wedge_idx[3]);
                    WII_HITS.with(|c| c.set(c.get() + 1));
                    wedge_mask = Some((
                        crate::wedge::wedge_masks()
                            .codebook(SIDE)
                            .mask(0, wedge_index as usize),
                        SIDE,
                    ));
                } else {
                    INTERINTRA_HITS.with(|c| c.set(c.get() + 1));
                }
                interintra_mode = Some(ii);
            }
        }
        // lane-motionmode round 3: `read_motion_mode` (spec 5.11.24) at the
        // 8x8 leaf too -- `motion_mode_allowed` still holds here (BLOCK_8X8
        // is the spec's own minimum eligible size, single-ref-only branch,
        // no interintra reader, `IDENTITY`-only global motion). Mirrors the
        // 16x16 leaf's own `decode_inter_block` read (see its doc above) --
        // same 2-symbol `obmc_cdf` alphabet, same `allow_warped_motion=1`
        // refusal (warp needs `av1_findSamples`, not ported at either leaf).
        // lane-gmaffine r1: the two libaom predicates, exactly as the
        // 16x16+ leaf resolves them (see [`decode_inter_block`]'s own doc):
        // `is_global_mv_block` (mode GLOBAL*, model > TRANSLATION, block
        // >= 8x8 -- BLOCK_8X8 IS the 8px minimum, so the size predicate is
        // always true here) suppresses the motion_mode read entirely and
        // turns on the global warp below.
        let gm_ref = &global_motion[(ref_frame - LAST_FRAME) as usize];
        let is_global_mv_block = is_globalmv && gm_ref.model as u8 > 1;
        let motion_mode_eligible = switchable_motion_mode
            && !skip_mode
            && interintra_mode.is_none()
            && !(is_global_mv_block && !force_integer_mv)
            && (!overlappable_above(grid, mi_row, mi_col, 2, mi_cols as usize, 1).is_empty()
                || !overlappable_left(grid, mi_row, mi_col, 2, mi_rows as usize, 1).is_empty());
        let mut warp_params: Option<crate::warp::WarpParams> = None;
        let mut warped_selected = false;
        let mut obmc_selected = false;
        if motion_mode_eligible {
            // lane-warp round 1: same 3-vs-2-symbol split as the 16x16+ leaf
            // (see its own doc) -- this leaf is always `LAST_FRAME`-only
            // (grid.set below hardcodes it).
            // lane-scaledref r2 sibling of the 16x16+ leaf's fix: libaom
            // drops WARPED_CAUSAL eligibility (so this reads `obmc_cdf`)
            // when the reference is scaled. Not gated here because an 8x8
            // leaf under a scaled reference is refused before this function
            // runs (see the block8 refusal below, ~12500) -- whoever lifts
            // that refusal must add `&& !ref_is_scaled` here too.
            let warp_eligible = allow_warped_motion
                && num_proj_ref(grid, mi_row, mi_col, 2, 2, mi_cols as usize, mi_rows as usize, ref_frame) >= 1;
            if warp_eligible {
                let mode = dec.symbol(&mut cdfs.motion_mode[0]);
                match mode {
                    0 => {}
                    1 => {
                        obmc_selected = true;
                        OBMC_HITS.with(|c| c.set(c.get() + 1));
                        OBMC_HITS_8.with(|c| c.set(c.get() + 1));
                    }
                    _ => {
                        // lane-gmaffine r1: WARPED_CAUSAL at the 8x8 leaf --
                        // the same findSamples -> select_samples ->
                        // av1_find_projection chain the 16x16+ leaf runs,
                        // with BLOCK_8X8's own 8x8 model centre; an
                        // unusable projection falls back to the block's
                        // translational mv (libaom `av1_find_projection`
                        // returning 1 leaves `wm_params` invalid and
                        // `build_inter_predictors` skips the warp).
                        WARP_SELECTED_HITS.with(|c| c.set(c.get() + 1));
                        let mut samples = find_samples(
                            grid,
                            mi_row,
                            mi_col,
                            2,
                            2,
                            mi_cols as usize,
                            mi_rows as usize,
                            ref_frame,
                        );
                        if samples.len() > 1 {
                            crate::warp::select_samples(mv, &mut samples, SIDE as i32, SIDE as i32);
                        }
                        warp_params = crate::warp::find_projection(
                            &samples,
                            SIDE as i32,
                            SIDE as i32,
                            mv.1,
                            mv.0,
                            mi_row as i32,
                            mi_col as i32,
                        );
                        warped_selected = true;
                        if warp_params.is_some() {
                            WARP_HITS_8.with(|c| c.set(c.get() + 1));
                        }
                    }
                }
            } else {
                // `default_obmc_cdf`'s own index: square bsize 8/16/32/64 -> 0/1/2/3.
                obmc_selected = dec.symbol(&mut cdfs.obmc[0]) == 1;
                if obmc_selected {
                    OBMC_HITS.with(|c| c.set(c.get() + 1));
                    OBMC_HITS_8.with(|c| c.set(c.get() + 1));
                }
            }
        }
        // lane-gmaffine r1: `read_mb_interp_filter` (spec 5.11.26) at the 8x8
        // leaf, in libaom's own order (interintra, motion_mode, THEN the
        // filter). Until this round the leaf read no filter symbol at all and
        // predicted Regular unconditionally -- correct only for a frame with a
        // fixed `interp_filter`, and a silent entropy DESYNC for every
        // SWITCHABLE-filter stream that reaches an 8x8 leaf (which is what
        // made the interior-split wiring above unusable at first).
        // corner-cut: the neighbour filter context is the enclosing 16x16
        // block's own coarse `above_filter`/`left_filter` entry (this leaf
        // path keeps no per-leaf neighbour array), so all four siblings read
        // the same context row. Ceiling: a stream whose 8x8 siblings pick
        // DIFFERENT filters decodes the second..fourth leaf's symbol from the
        // wrong CDF row. Upgrade: give `Neighbours` a real 8x8-granular
        // filter array, same shape as the `[3, 3]` approximation the caller
        // records after the leaf loop.
        let above_filter_ctx = if neighbours.above_ref[cmi] == ref_frame
            || neighbours.above_ref1[cmi] == Some(ref_frame)
        {
            neighbours.above_filter[cmi]
        } else {
            [3, 3]
        };
        let left_filter_ctx = if neighbours.left_ref[rmi] == ref_frame
            || neighbours.left_ref1[rmi] == Some(ref_frame)
        {
            neighbours.left_filter[rmi]
        } else {
            [3, 3]
        };
        let gm_nontrans =
            is_globalmv && gm_ref.model != ec_av1_syntax::WarpModel::Translation;
        let (h_filter, v_filter, resolved_filter) = resolve_interp_filter(
            dec,
            cdfs,
            interp_fixed,
            enable_dual_filter,
            gm_nontrans || warped_selected || skip_mode,
            above_filter_ctx,
            left_filter_ctx,
            false,
        );
        leaf_filter_syms = resolved_filter;
        if std::env::var_os("EC_TRACE_MODE").is_some() {
            eprintln!(
                "EC_MODE_VAL8 mi_row={} mi_col={} newmv={is_new_mv} globalmv={is_globalmv} ref0={ref_frame} mv0=({},{}) stack={} rng={}",
                leaf_mi.0,
                leaf_mi.1,
                mv.0,
                mv.1,
                stack.entries.len(),
                dec.debug_state().0
            );
        }
        // lane-gmaffine r1: `allow_warp`'s `global_warp_allowed` branch
        // (`reconinter.c:33-55`), gated INDEPENDENTLY of `motion_mode` --
        // see [`decode_inter_block`]'s own copy. A local WARPED_CAUSAL model
        // wins (`is_none()` guard); BLOCK_8X8 already satisfies the >=8px
        // size predicate baked into `is_global_mv_block`.
        if warp_params.is_none() && is_global_mv_block && !force_integer_mv && !gm_ref.invalid {
            warp_params = crate::warp::global_warp_params(gm_ref.params);
            if warp_params.is_some() && gm_ref.model == ec_av1_syntax::WarpModel::Affine {
                AFFINE_GM_HITS.with(|c| c.set(c.get() + 1));
            }
        }
        if std::env::var_os("EC_TRACE_MODE").is_some() {
            eprintln!(
                "EC_WARP8 mi_row={} mi_col={} warp={} globalblk={is_global_mv_block}",
                leaf_mi.0,
                leaf_mi.1,
                warp_params.is_some(),
            );
        }
        for dr in 0..2 {
            for dc in 0..2 {
                grid.set(
                    mi_row + dr,
                    mi_col + dc,
                    MiInfo {
                        is_inter: true,
                        ref_frame,
                        // INTRA_FRAME marker for interintra blocks -- keeps
                        // them out of warp-sample gathering (see 16/32 site).
                        ref_frame1: interintra_mode.map(|_| 0),
                        mv1: None,
                        mv,
                        is_new_mv,
                        size: 2,
                        size_h: 2,
                        is_global_mv0: is_global_mv_block,
                        is_global_mv1: false,
                    },
                );
            }
        }
        mode_for_tx = 0;

        // lane-scaledref r1: this leaf's own prediction is always `Regular`
        // (documented corner-cut above); under a scaled reference the same
        // fixed kernel runs through spec 7.11.3.3's scaled walk instead.
        let luma_scale = mc::scale_factor(sref_y.width, frame_width);
        if luma_scale != mc::REF_NO_SCALE {
            SCALED_BLOCK8_HITS.with(|c| c.set(c.get() + 1));
        }
        let mut pred_y = vec![0u16; SIDE * SIDE];
        let mut pred_u = vec![0u16; CHROMA_SIDE * CHROMA_SIDE];
        let mut pred_v = vec![0u16; CHROMA_SIDE * CHROMA_SIDE];
        // Merge of lane-gmaffine (this leaf's own switchable filters) with
        // lane-scaledref (spec 7.11.3.3's scaled walk): same kernels either
        // way, only the sample walk differs.
        for (plane, x, y, dim, dst) in [
            (sref_y, px, py, SIDE, &mut pred_y),
            (sref_u, cpx, cpy, CHROMA_SIDE, &mut pred_u),
            (sref_v, cpx, cpy, CHROMA_SIDE, &mut pred_v),
        ] {
            let luma = std::ptr::eq(plane, sref_y);
            if luma_scale == mc::REF_NO_SCALE {
                mc::predict_with_filters(
                    &plane.data,
                    plane.width,
                    plane.true_width,
                    plane.true_height,
                    mv_to_q4(x, mv.1, luma),
                    mv_to_q4(y, mv.0, luma),
                    dim,
                    dim,
                    h_filter,
                    v_filter,
                    dst,
                );
            } else {
                mc::predict_scaled(
                    &plane.data,
                    plane.width,
                    plane.true_width,
                    plane.true_height,
                    mv_to_q4(x, mv.1, luma),
                    mv_to_q4(y, mv.0, luma),
                    luma_scale,
                    dim,
                    dim,
                    h_filter,
                    v_filter,
                    dst,
                );
            }
        }

        // lane-gmaffine r1: the warp filter REPLACES the translational
        // prediction just built (libaom builds one or the other), same as
        // the 16x16+ leaf's own call trio.
        if let Some(params) = &warp_params {
            crate::warp::warp_affine(
                params, &sref_y.data, sref_y.true_width as i32, sref_y.true_height as i32,
                sref_y.width as i32, &mut pred_y, px as i32, py as i32, SIDE as i32,
                SIDE as i32, SIDE as i32, 0, 0,
            );
            // `av1_init_warp_params`'s per-plane `block_width/height < 8`
            // bail-out: an 8x8 leaf's chroma plane is 4x4 in 420, so it is
            // predicted translationally even when the luma warps.
            #[allow(clippy::absurd_extreme_comparisons)]
            if CHROMA_SIDE >= 8 {
                crate::warp::warp_affine(
                    params, &sref_u.data, sref_u.true_width as i32, sref_u.true_height as i32,
                    sref_u.width as i32, &mut pred_u, cpx as i32, cpy as i32, CHROMA_SIDE as i32,
                    CHROMA_SIDE as i32, CHROMA_SIDE as i32, 1, 1,
                );
                crate::warp::warp_affine(
                    params, &sref_v.data, sref_v.true_width as i32, sref_v.true_height as i32,
                    sref_v.width as i32, &mut pred_v, cpx as i32, cpy as i32, CHROMA_SIDE as i32,
                    CHROMA_SIDE as i32, CHROMA_SIDE as i32, 1, 1,
                );
            }
        }

        if obmc_selected {
            obmc_blend(
                grid,
                neighbours,
                mi_row,
                mi_col,
                2,
                2,
                mi_rows as usize,
                mi_cols as usize,
                SIDE,
                SIDE,
                SIDE,
                CHROMA_SIDE,
                px,
                py,
                cpx,
                cpy,
                ref_y,
                ref_u,
                ref_v,
                other_refs,
                interp_fixed,
                frame_width,
                &mut pred_y,
                &mut pred_u,
                &mut pred_v,
            )?;
        }

        // Non-wedge interintra blend, mutually exclusive with OBMC (the
        // motion_mode symbol is not read for an interintra block).
        if let Some(ii) = interintra_mode {
            interintra_blend(y, px, py, SIDE, ii, wedge_mask, &mut pred_y);
            interintra_blend(u, cpx, cpy, CHROMA_SIDE, ii, wedge_mask, &mut pred_u);
            interintra_blend(v, cpx, cpy, CHROMA_SIDE, ii, wedge_mask, &mut pred_v);
        }

        split8 = read_block_tx_size(
            dec,
            cdfs,
            neighbours,
            leaf_mi,
            SIDE,
            (mi_cols as usize, mi_rows as usize),
            true,
            skip,
        )?
        .1
        .is_some();
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
            let (grid8, luma_tx_type) = read_inter_luma8(
                dec,
                cdfs,
                neighbours,
                leaf_mi,
                y,
                px,
                py,
                &pred_y,
                mode_for_tx,
                base_q_idx,
                scan8,
                scan4,
                reduced_tx_set,
                split8,
            )?;
            luma_grid = grid8;
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
                Some(luma_tx_type),
                None,
            )?
            .0;
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
                Some(luma_tx_type),
                None,
            )?
            .0;
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
        // lane-screen r2: see [`decode_inter_block`]'s own copy -- `mode`/
        // `uv_mode` are both effectively `DC_PRED`-gated already on this
        // leaf (a nonzero `mode` still passes, but `uv_mode` is forced
        // `DC_PRED` by the refusal just above), same order as libaom's
        // `read_palette_mode_info` call right after `xd->cfl.store_y`.
        if allow_screen_content_tools {
            if mode == DC_PRED && dec.symbol(&mut cdfs.palette_y_mode[0][0]) != 0 {
                return Err(unsupported(
                    "a block that actually uses a palette (Y) -- reconstruction is out of scope",
                ));
            }
            if dec.symbol(&mut cdfs.palette_uv_mode[0]) != 0 {
                return Err(unsupported(
                    "a block that actually uses a palette (UV) -- reconstruction is out of scope",
                ));
            }
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

        read_block_tx_size(
            dec,
            cdfs,
            neighbours,
            leaf_mi,
            SIDE,
            (mi_cols as usize, mi_rows as usize),
            false,
            skip,
        )?;
        let reach = Reach::of(SIDE, px, py, y.width, y.height);
        if skip {
            y.reconstruct(
                px,
                py,
                SIDE,
                mode,
                0,
                reach,
                &vec![0i32; SIDE * SIDE],
                None,
                None,
                false,
            );
            u.reconstruct(
                cpx,
                cpy,
                CHROMA_SIDE,
                DC_PRED,
                0,
                reach,
                &vec![0i32; CHROMA_SIDE * CHROMA_SIDE],
                None,
                None,
                false,
            );
            v.reconstruct(
                cpx,
                cpy,
                CHROMA_SIDE,
                DC_PRED,
                0,
                reach,
                &vec![0i32; CHROMA_SIDE * CHROMA_SIDE],
                None,
                None,
                false,
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
                0,
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
                false,
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
                0,
                reach,
                u,
                cpx,
                cpy,
                CHROMA_SIDE,
                TX4,
                base_q_idx,
                None,
                None,
                None,
                false,
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
                0,
                reach,
                v,
                cpx,
                cpy,
                CHROMA_SIDE,
                TX4,
                base_q_idx,
                None,
                None,
                None,
                false,
            )?;
        }
    }
    // lane-txselect: a split leaf's plane-0 neighbour context is per 4x4
    // transform unit (written by `record_mi_luma` inside `read_inter_luma8`),
    // but `record_mi` rewrites all three planes of these cells at once -- so
    // save plane 0 across it, exactly as `record_split_luma` leaves plane 0
    // alone at the bigger block sizes.
    let saved_luma_ctx = split8.then(|| {
        (
            [
                neighbours.left[leaf_mi.0][0],
                neighbours.left[leaf_mi.0 + 1][0],
            ],
            [
                neighbours.above[leaf_mi.1][0],
                neighbours.above[leaf_mi.1 + 1][0],
            ],
        )
    });
    neighbours.record_mi(leaf_mi, 8, &[luma_grid, u_grid, v_grid]);
    if let Some((left, above)) = saved_luma_ctx {
        for (cell, state) in left.into_iter().enumerate() {
            neighbours.left[leaf_mi.0 + cell][0] = state;
        }
        for (cell, state) in above.into_iter().enumerate() {
            neighbours.above[leaf_mi.1 + cell][0] = state;
        }
    }
    // lane-gmaffine r2: the deblock grid's ref id is the leaf's OWN reference
    // (r1 taught this leaf non-LAST refs; the hardcoded LAST_FRAME made every
    // GOLDEN/ALTREF leaf read as LAST at the loop-filter's ref/mv edge test).
    neighbours.fill_lf_grid(
        leaf_mi,
        2,
        if split8 { 4 } else { 8 },
        if is_inter { leaf_refs.0.max(LAST_FRAME) } else { 0 },
    );
    Ok((
        skip,
        is_inter,
        skip_mode,
        compound_ctx8,
        leaf_filter_syms,
        leaf_refs,
    ))
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
    other_refs: [Option<&Picture>; 8],
    cdef: &CdefParams,
    loop_filter: &LoopFilterParams,
    allow_high_precision_mv: bool,
    force_integer_mv: bool,
    interp_fixed: Option<mc::InterpFilterKind>,
    enable_dual_filter: bool,
    reference_select: bool,
) -> Result<Picture> {
    let single_tile = TileInfo {
        mi_col_starts: vec![0, mi_cols],
        mi_row_starts: vec![0, mi_rows],
        ..TileInfo::default()
    };
    decode_inter_frame_tile_with_cdfs(
        &[data],
        &single_tile,
        mi_cols,
        mi_rows,
        base_q_idx,
        crate::quant::QuantDeltas::default(),
        frame_width,
        frame_height,
        reference,
        other_refs,
        cdef,
        loop_filter,
        &LoopRestorationParams::default(),
        None,
        allow_high_precision_mv,
        force_integer_mv,
        NO_SIGN_BIAS,
        [ec_av1_syntax::WarpParams::default(); 7],
        interp_fixed,
        enable_dual_filter,
        0,
        0,
        [0; 7],
        None,
        reference_select,
        false,
        false,
        false,
        false,
        [0; 2],
        true,
        // `tx_select`: this raw entry point decodes this crate's own encoder's
        // streams, which always write `TxMode::Largest`.
        false,
        false,
        false,
        false,
        DeltaParams::default(),
    )
    .map(|(picture, _, _)| picture)
}

/// [`decode_inter_frame_tile`], threading a cross-frame CDF forward -- see
/// [`decode_key_frame_tile_with_cdfs`]'s doc for `initial_cdfs`/the returned
/// end-of-tile table. An inter frame's own header can name
/// `primary_ref_frame == PRIMARY_REF_NONE` too (an error-resilient stream),
/// so `None` is a real case here, not only a convenience default.
pub(crate) fn decode_inter_frame_tile_with_cdfs(
    tiles: &[&[u8]],
    tile_info: &TileInfo,
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    // lane-sbpart r11: see [`decode_key_frame_tile_with_cdfs`]'s own `deltas`.
    deltas: crate::quant::QuantDeltas,
    frame_width: u32,
    frame_height: u32,
    reference: &Picture,
    other_refs: [Option<&Picture>; 8],
    cdef: &CdefParams,
    loop_filter: &LoopFilterParams,
    lr: &LoopRestorationParams,
    initial_cdfs: Option<Cdfs>,
    allow_high_precision_mv: bool,
    force_integer_mv: bool,
    sign_bias_table: SignBiasTable,
    // lane-gm r2: this frame header's own `global_motion` table (spec
    // 5.9.24), threaded to every `decode_inter_block` call below.
    global_motion: [ec_av1_syntax::WarpParams; 7],
    interp_fixed: Option<mc::InterpFilterKind>,
    enable_dual_filter: bool,
    // lane-av1tmvp: `order_hint`/`order_hint_bits`/`ref_order_hints` are
    // always needed (this frame's own saved [`crate::motion_field::MotionField`]
    // carries them for a *later* frame's projection, spec 7.9); `tpl_field`
    // is `Some` only when this frame's own header set `use_ref_frame_mvs`
    // and the caller ([`crate::stream::decode_stream`]) projected one.
    order_hint_bits: u32,
    order_hint: u32,
    ref_order_hints: [u32; 7],
    tpl_field: Option<&crate::motion_field::TplField>,
    // lane-av1comp: this frame header's own `reference_select` bit, threaded
    // to every `decode_inter_block`/`decode_inter_block8` call below.
    reference_select: bool,
    // lane-av1comp: `seq_params`' own compound-blend enable bits.
    enable_masked_compound: bool,
    enable_jnt_comp: bool,
    // lane-sb128 r3: this sequence header's own `enable_interintra_compound`
    // bit, threaded to every `decode_inter_block`/`decode_inter_block8` call
    // below (spec 5.11.24's `interintra` read).
    enable_interintra_compound: bool,
    // lane-av1comp round 14: this frame header's own `skip_mode_present`/
    // `skip_mode_frame` (spec 5.9.22), threaded to every `decode_inter_block`/
    // `decode_inter_block8` call below.
    skip_mode_present: bool,
    skip_mode_frame: [u8; 2],
    // lane-cdffwd2: this frame header's own `reduced_tx_set` bit, threaded to
    // every inter `tx_type` read below (`false` needs the 12-/16-symbol
    // `Set1` coefficient tables at 16x16/8x8 rather than the reduced
    // 2-symbol ones -- see [`TxbSet::Luma16InterSet1`]/[`TxbSet::Luma8InterSet1`]).
    reduced_tx_set: bool,
    // lane-txselect: this frame header's own `tx_mode == TxMode::Select`
    // bit (spec 5.9.14). Together with `reduced_tx_set` it is stashed in a
    // thread-local by [`set_inter_tx_mode`] rather than threaded through
    // `decode_inter_block`'s forty parameters -- both are frame-constant, and
    // only [`read_block_tx_size`]/the var-tx residual loop read them.
    tx_select: bool,
    // lane-motionmode round 1/3: this frame header's own `is_motion_mode_switchable`/
    // `allow_warped_motion` bits, threaded to every `decode_inter_block`/
    // `decode_inter_block8` call below (spec 5.11.24's `read_motion_mode`).
    switchable_motion_mode: bool,
    allow_warped_motion: bool,
    // lane-screen r2: this frame header's own `allow_screen_content_tools`
    // bit, threaded to every `decode_inter_block`/`decode_inter_block8`
    // call below -- consumes (never reconstructs) the intra-sub-block
    // palette syntax libaom's `read_intra_block_mode_info` reads under the
    // same `av1_allow_palette` gate as the key-frame path.
    allow_screen_content_tools: bool,
    // lane-realworld r5: this frame header's own `delta` (spec 5.9.17/5.9.18),
    // same contract as [`decode_key_frame_tile_with_cdfs`]'s own `delta`
    // param -- only `q_present`/`q_res` are read.
    delta: DeltaParams,
) -> Result<(Picture, Cdfs, crate::motion_field::MotionField)> {
    set_inter_tx_mode(tx_select, reduced_tx_set);
    let tpl_frame = tpl_field.map(|field| TplFrameArgs {
        field,
        order_hint_bits,
        order_hint,
        ref_order_hints,
    });
    if mi_cols == 0 || mi_rows == 0 {
        return Err(unsupported("a frame with no mode-info grid"));
    }
    let (true_width, true_height) = ((mi_cols * 4) as usize, (mi_rows * 4) as usize);
    // lane-superres r10: a width mismatch alone is no longer refused here --
    // `mc::predict_scaled` (spec 7.11.3.3, threaded through
    // `decode_inter_block`'s single-ref branch) exists precisely for a
    // `use_superres` reference at a different width than this frame's own
    // true size. Height must still match: AV1 superres never scales height
    // (r8's derivation, `mc.rs`'s own doc), and nothing in this decode path
    // has a vertical scaling pass.
    if reference.height != true_height {
        return Err(unsupported(
            "a reference picture whose height does not match this frame's own true size",
        ));
    }
    let (cols, rows) = block_grid(mi_cols, mi_rows);
    let (sb_cols, sb_rows) = (cols.div_ceil(2), rows.div_ceil(2));
    CDEF_BITS.with(|c| c.set(cdef.bits));
    CDEF_SB_COLS.with(|c| c.set(sb_cols as usize));
    CDEF_IDX_GRID.with(|g| *g.borrow_mut() = vec![0u8; sb_cols as usize * sb_rows as usize]);
    let (width, height) = (cols as usize * BLOCK, rows as usize * BLOCK);

    let ref_y = PlaneBuf {
        data: reference.y.iter().map(|&s| s as u16).collect(),
        width: reference.width,
        height: reference.height,
        true_width: reference.width,
        true_height: reference.height,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: reference.width,
        tile_y1: reference.height,
    };
    let ref_u = PlaneBuf {
        data: reference.u.iter().map(|&s| s as u16).collect(),
        width: reference.width / 2,
        height: reference.height / 2,
        true_width: reference.width / 2,
        true_height: reference.height / 2,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: reference.width / 2,
        tile_y1: reference.height / 2,
    };
    let ref_v = PlaneBuf {
        data: reference.v.iter().map(|&s| s as u16).collect(),
        width: reference.width / 2,
        height: reference.height / 2,
        true_width: reference.width / 2,
        true_height: reference.height / 2,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: reference.width / 2,
        tile_y1: reference.height / 2,
    };
    // Every non-`LAST_FRAME` reference this frame header's own `ref_frame_idx`
    // names a live DPB slot for (lane-av1refs: generalised from the old
    // GOLDEN_FRAME-only block -- same empty-slot/size-mismatch refusal,
    // now per-reference). `decode_inter_block` refuses by name when the
    // block it is decoding selects an index that is still `None` here
    // (spec 7.9 requires every *active* reference be a compatible size;
    // an inactive one this frame never selects is simply left `None`).
    let owned_refs: [Option<(PlaneBuf, PlaneBuf, PlaneBuf)>; 8] = std::array::from_fn(|i| {
        other_refs[i]
            .filter(|g| g.width == reference.width && g.height == reference.height)
            .map(|g| {
                (
                    PlaneBuf {
                        data: g.y.iter().map(|&s| s as u16).collect(),
                        width: g.width,
                        height: g.height,
                        true_width: g.width,
                        true_height: g.height,
                        tile_x0: 0,
                        tile_y0: 0,
                        tile_x1: g.width,
                        tile_y1: g.height,
                    },
                    PlaneBuf {
                        data: g.u.iter().map(|&s| s as u16).collect(),
                        width: g.width / 2,
                        height: g.height / 2,
                        true_width: g.width / 2,
                        true_height: g.height / 2,
                        tile_x0: 0,
                        tile_y0: 0,
                        tile_x1: g.width / 2,
                        tile_y1: g.height / 2,
                    },
                    PlaneBuf {
                        data: g.v.iter().map(|&s| s as u16).collect(),
                        width: g.width / 2,
                        height: g.height / 2,
                        true_width: g.width / 2,
                        true_height: g.height / 2,
                        tile_x0: 0,
                        tile_y0: 0,
                        tile_x1: g.width / 2,
                        tile_y1: g.height / 2,
                    },
                )
            })
    });
    let ref_slots: RefSlots =
        std::array::from_fn(|i| owned_refs[i].as_ref().map(|(a, b, c)| (a, b, c)));

    let mut y = PlaneBuf {
        data: vec![0u16; width * height],
        width,
        height,
        true_width,
        true_height,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: width,
        tile_y1: height,
    };
    let mut u = PlaneBuf {
        data: vec![0u16; width * height / 4],
        width: width / 2,
        height: height / 2,
        true_width: true_width / 2,
        true_height: true_height / 2,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: width / 2,
        tile_y1: height / 2,
    };
    let mut v = PlaneBuf {
        data: vec![0u16; width * height / 4],
        width: width / 2,
        height: height / 2,
        true_width: true_width / 2,
        true_height: true_height / 2,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: width / 2,
        tile_y1: height / 2,
    };

    let scan32 = default_scan(TX32);
    let scan16 = default_scan(TX16);
    let scan8 = default_scan(TX8);
    let scan4 = default_scan(TX4);

    let base_cdfs = initial_cdfs.unwrap_or_else(|| Cdfs::new(q_ctx_of(base_q_idx)));
    let mut result_cdfs = base_cdfs.clone();
    // spec `decode_tile`: `CurrentQIndex`/`DeltaLF` reset to the frame's own
    // `base_q_idx`/zero at the top of every tile, not once per frame (mirrors
    // `decode_key_frame_tile_with_cdfs`'s per-tile reset below); the
    // delta_q/delta_lf presence/resolution flags themselves are frame-level
    // and set once here.
    DELTA_Q_PRESENT.with(|c| c.set(delta.q_present));
    DELTA_Q_RES.with(|c| c.set(1i32 << delta.q_res));
    DELTA_LF_PRESENT.with(|c| c.set(delta.lf_present));
    DELTA_LF_RES.with(|c| c.set(1i32 << delta.lf_res));
    DELTA_LF_MULTI.with(|c| c.set(delta.lf_multi));
    let mut neighbours = Neighbours::new(
        cols as usize * 2,
        rows as usize * 2,
        mi_cols as usize,
        mi_rows as usize,
    );
    let mut grid = MiGrid::new(mi_cols as usize, mi_rows as usize);
    let mut lr_grid = crate::restoration::RestorationGrid::new(lr, frame_width, frame_height);
    let mut lr_reference = [(
        crate::restoration::WienerInfo::default(),
        crate::restoration::SgrprojInfo::default(),
    ); 3];

    // lane-tiles r6: mirrors decode_key_frame_tile_with_cdfs's own per-tile
    // loop (spec 5.11.2 decode_tile / 7.20 exit_symbol) -- each tile gets
    // its own fresh SymbolDecoder over its own byte range and its own fresh
    // copy of the frame's initial CDFs; only context_update_tile_id's own
    // end-of-tile adapted table becomes the frame's own output.
    for (tile_idx, &data) in tiles.iter().enumerate() {
        let tile_num = tile_idx as u32;
        let (trow, tcol) = (tile_num / tile_info.cols, tile_num % tile_info.cols);
        let mi_row0 = tile_info.mi_row_starts[trow as usize];
        let mi_row1 = tile_info.mi_row_starts[trow as usize + 1];
        let mi_col0 = tile_info.mi_col_starts[tcol as usize];
        let mi_col1 = tile_info.mi_col_starts[tcol as usize + 1];
        let (sb_r0, sb_r1) = (
            (mi_row0 / SB_MI).min(sb_rows),
            mi_row1.div_ceil(SB_MI).min(sb_rows),
        );
        let (sb_c0, sb_c1) = (
            (mi_col0 / SB_MI).min(sb_cols),
            mi_col1.div_ceil(SB_MI).min(sb_cols),
        );
        let mut cdfs = base_cdfs.clone();
        CURRENT_Q_IDX.with(|c| c.set(i32::from(base_q_idx)));
        QUANT_DELTAS.with(|c| c.set(deltas));
        CURRENT_DELTA_LF.with(|c| c.set([0; 4]));
        let mut dec = SymbolDecoder::new(data);
        // lane-comppin r9: tile-entry range, the earliest point comparable
        // against aomdec's own `r->ec.rng` right after `aom_reader_init` -- the
        // first ladder rung before any symbol (partition or otherwise) is read.
        if std::env::var_os("EC_AV1_TELL").is_some() {
            eprintln!(
                "TELL label=tile_init tell={} range={}",
                dec.debug_bitpos(),
                dec.debug_state().0
            );
        }
        neighbours.start_tile(mi_row0 as usize, mi_col0 as usize, mi_col1 as usize);
        y.set_tile_origin(
            mi_col0 as usize * 4,
            mi_row0 as usize * 4,
            (mi_col1 as usize * 4).min(y.width),
            (mi_row1 as usize * 4).min(y.height),
        );
        u.set_tile_origin(
            mi_col0 as usize * 2,
            mi_row0 as usize * 2,
            (mi_col1 as usize * 2).min(u.width),
            (mi_row1 as usize * 2).min(u.height),
        );
        v.set_tile_origin(
            mi_col0 as usize * 2,
            mi_row0 as usize * 2,
            (mi_col1 as usize * 2).min(v.width),
            (mi_row1 as usize * 2).min(v.height),
        );
        // lane-tiles r6: an MV candidate scan (mvstack.rs's grid.get) must
        // not reach across a tile boundary any more than intra prediction's
        // PlaneBuf reach does -- bounding the shared grid's own read window
        // per tile is equivalent to threading tile bounds through every
        // find_mv_stack* call site (all four read through this one grid).
        grid.set_tile_bounds(
            mi_row0 as usize,
            mi_col0 as usize,
            mi_row1 as usize,
            mi_col1 as usize,
        );
        TILE_HITS.with(|c| c.set(c.get() + 1));

    for sb_r in sb_r0..sb_r1 {
        neighbours.start_row();
        for sb_c in sb_c0..sb_c1 {
            crate::restoration::read_lr(
                &mut dec,
                &mut cdfs,
                lr,
                &mut lr_grid,
                &mut lr_reference,
                sb_r * SB_MI,
                sb_c * SB_MI,
                SB_MI,
            );
            CDEF_TRANSMITTED.with(|c| c.set(false));
            let sb_at = (sb_r as usize * 4, sb_c as usize * 4);
            let sb_ctx = neighbours.partition_ctx(sb_at, SB);
            let (has_cols, has_rows) = (
                sb_c * SB_MI + SB_MI / 2 < mi_cols,
                sb_r * SB_MI + SB_MI / 2 < mi_rows,
            );
            // The straddle cases (one or both halves outside the true
            // frame) never carry a real alphabet symbol -- spec forces
            // SPLIT there (mirrors the intra key-frame tile's own
            // three-way write above). Only the (true, true) case can name
            // a non-SPLIT value, and this loop below only ever recurses as
            // SPLIT -- so a real non-SPLIT part64 here must refuse by name
            // instead of silently decoding as SPLIT and desyncing.
            let part64 = match (has_cols, has_rows) {
                (true, true) => {
                    let p = dec.symbol(&mut cdfs.partition_w64[sb_ctx]);
                    if std::env::var_os("EC_AV1_TRACE").is_some() {
                        eprintln!(
                            "TRACE partition_w64 mi=({},{}) ctx={sb_ctx} value={p}",
                            sb_r * SB_MI,
                            sb_c * SB_MI
                        );
                    }
                    p
                }
                (true, false) => {
                    dec.symbol_fixed(&gather(&cdfs.partition_w64[sb_ctx], VERT_ALIKE));
                    PARTITION_SPLIT
                }
                (false, true) => {
                    dec.symbol_fixed(&gather(&cdfs.partition_w64[sb_ctx], HORZ_ALIKE));
                    PARTITION_SPLIT
                }
                (false, false) => PARTITION_SPLIT,
            };
            if part64 == PARTITION_NONE {
                // lane-inter8 r1: the whole superblock as one 64x64 inter
                // block. `SB`-sized syntax throughout (size_group 3, same as
                // 32x32 -- `size_group_lookup[BLOCK_64X64] == 3`), luma
                // coefficients through `TxbSet::Luma64` with the 32-point
                // scan (TX_64X64 codes only its top-left corner, spec
                // 5.11.40 -- `read_inter_plane` widens the corner back to
                // 64x64 before the inverse transform), chroma as a plain
                // 32x32 `TxbSet::Chroma32`. TX_64X64 carries no `tx_type`
                // symbol at all, so the intra/inter luma sets coincide.
                INTER_SB_NONE_HITS.with(|c| c.set(c.get() + 1));
                decode_inter_block(
                    &mut dec,
                    &mut cdfs,
                    &mut neighbours,
                    &mut grid,
                    sb_at,
                    SB,
                    mi_cols,
                    mi_rows,
                    &mut y,
                    &mut u,
                    &mut v,
                    &ref_y,
                    &ref_u,
                    &ref_v,
                    &ref_slots,
                    &sign_bias_table,
                    &global_motion,
                    base_q_idx,
                    TxbSet::Luma64,
                    TxbSet::Luma64,
                    TxbSet::Chroma32,
                    TX32,
                    TX32,
                    &scan32,
                    &scan32,
                    3,
                    allow_high_precision_mv,
                    force_integer_mv,
                    interp_fixed,
                    enable_dual_filter,
                    tpl_frame.as_ref(),
                    reference_select,
                    enable_masked_compound,
                    enable_interintra_compound,
                    enable_jnt_comp,
                    order_hint_bits,
                    order_hint,
                    ref_order_hints,
                    skip_mode_present,
                    skip_mode_frame,
                    switchable_motion_mode,
                    allow_warped_motion,
                    false,
                    SB,
                    SB,
                    allow_screen_content_tools,
                    frame_width as usize,
                )?;
                continue;
            }
            if part64 != PARTITION_SPLIT {
                return Err(unsupported(
                    "an inter SB-level partition type other than NONE or SPLIT (this decoder's \
                     inter tile path recurses a superblock only as SPLIT)",
                ));
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
                    if std::env::var_os("EC_AV1_TRACE").is_some() {
                        eprintln!(
                            "TRACE part32_pre mi=({},{}) rng={}",
                            r32 * BLOCK_MI,
                            c32 * BLOCK_MI,
                            dec.debug_state().0
                        );
                    }
                    let p = dec.symbol(&mut cdfs.partition_w32[ctx32]);
                    if std::env::var_os("EC_AV1_TRACE").is_some() {
                        eprintln!(
                            "TRACE partition_w32 mi=({},{}) ctx={ctx32} value={p}",
                            r32 * BLOCK_MI,
                            c32 * BLOCK_MI
                        );
                    }
                    p
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
                            &ref_slots,
                            &sign_bias_table,
                            &global_motion,
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
                            force_integer_mv,
                            interp_fixed,
                            enable_dual_filter,
                            tpl_frame.as_ref(),
                            reference_select,
                            enable_masked_compound,
                            enable_interintra_compound,
                            enable_jnt_comp,
                            order_hint_bits,
                            order_hint,
                            ref_order_hints,
                            skip_mode_present,
                            skip_mode_frame,
                            switchable_motion_mode,
                            allow_warped_motion,
                            false,
                            BLOCK,
                            BLOCK,
                            allow_screen_content_tools,
                            frame_width as usize,
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
                            let ctx16 = neighbours.partition_ctx(at16, SUB);
                            // lane-inter8 r1: a straddling 16x16 cannot name a
                            // value (spec forces SPLIT, one gathered bit); an
                            // interior one reads the real alphabet, and
                            // `PARTITION_SPLIT` now recurses into four 8x8
                            // inter leaves through the very same loop the
                            // straddling case has always used.
                            let part16 = if has_cols16 && has_rows16 {
                                let p = dec.symbol(&mut cdfs.partition_w16[ctx16]);
                                if std::env::var_os("EC_AV1_TRACE").is_some() {
                                    let (rng, _) = dec.debug_state();
                                    eprintln!(
                                        "TRACE partition_w16 mi=({},{}) ctx={ctx16} value={p} rng={rng}",
                                        sr as u32 * SUB_MI,
                                        sc as u32 * SUB_MI
                                    );
                                }
                                p
                            } else {
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
                                PARTITION_SPLIT
                            };
                            if part16 == PARTITION_NONE {
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
                                    &ref_slots,
                                    &sign_bias_table,
                            &global_motion,
                                    base_q_idx,
                                    TxbSet::Luma16,
                                    if reduced_tx_set {
                                        TxbSet::Luma16Inter
                                    } else {
                                        TxbSet::Luma16InterSet1
                                    },
                                    TxbSet::Chroma8,
                                    TX16,
                                    TX8,
                                    &scan16,
                                    &scan8,
                                    2,
                                    allow_high_precision_mv,
                                    force_integer_mv,
                                    interp_fixed,
                                    enable_dual_filter,
                                    tpl_frame.as_ref(),
                                    reference_select,
                                    enable_masked_compound,
                                    enable_interintra_compound,
                                    enable_jnt_comp,
                                    order_hint_bits,
                                    order_hint,
                                    ref_order_hints,
                                    skip_mode_present,
                                    skip_mode_frame,
                                    switchable_motion_mode,
                                    allow_warped_motion,
                                    false,
                                    SUB,
                                    SUB,
                                    allow_screen_content_tools,
                                    frame_width as usize,
                                )?;
                            } else if part16 != PARTITION_SPLIT {
                                return Err(unsupported(
                                    "an inter partition below 16x16 other than SPLIT (16x8/8x16 rect inter leaves are not coded yet)",
                                ));
                            } else {
                                if has_cols16 && has_rows16 {
                                    INTER_SUB16_SPLIT_HITS.with(|c| c.set(c.get() + 1));
                                }
                                let (mi_row0, mi_col0) = (sr as u32 * SUB_MI, sc as u32 * SUB_MI);
                                let leaf_positions: Vec<(u32, u32)> = (0..4)
                                    .map(|i| (mi_row0 + (i / 2) * 2, mi_col0 + (i % 2) * 2))
                                    .filter(|&(mr, mc)| mr < mi_rows && mc < mi_cols)
                                    .collect();
                                // lane-scaledref r1: `decode_inter_block8` IS
                                // threaded for a scaled reference now (its
                                // single-ref, compound and OBMC MC all take
                                // spec 7.11.3.3's scaled walk), but the one
                                // recipe that reaches this path at all -- a
                                // 64x72 superres fixture whose bottom 16-row
                                // band straddles the true edge, so the
                                // gathered split bit lands on 8x8 leaves --
                                // DESYNCS inside the leaf itself
                                // (`from_switchable_symbol` handed a 4th
                                // symbol, mc.rs:200), the same
                                // below-8x8 desync lane-sub8 r2 left open and
                                // unrelated to scaling. So the scaled MC here
                                // is unproven: refuse by name rather than
                                // ship a capability claim no gate exercises.
                                // Deleting these six lines is the whole lift
                                // once the leaf8 desync is fixed.
                                if ref_y.width != frame_width as usize
                                    || ref_slots.iter().flatten().any(|(py, _, _)| {
                                        py.width != frame_width as usize
                                    })
                                {
                                    return Err(unsupported(
                                        "an 8x8 partition leaf under a scaled reference (superres, unimplemented)",
                                    ));
                                }
                                let mut prev_leaves: Vec<((usize, usize), bool, bool, i8, Option<i8>)> = Vec::new();
                                for (mr, mc) in leaf_positions {
                                    let leaf_mi = (mr as usize, mc as usize);
                                    let leaf_ctx = neighbours.partition_ctx_mi(leaf_mi, 8);
                                    let part8 = dec.symbol(&mut cdfs.partition_w8[leaf_ctx]);
                                    if part8 != PARTITION_NONE {
                                        return Err(unsupported(
                                            "an inter partition below 8x8 (this decoder codes no inter leaf smaller than 8x8; lane-sub8 scoped to intra)",
                                        ));
                                    }
                                    let (
                                        skip,
                                        is_inter,
                                        skip_mode_leaf,
                                        compound_ctx8,
                                        leaf_filter,
                                        leaf_refs,
                                    ) = decode_inter_block8(
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
                                            &ref_slots,
                                            base_q_idx,
                                            &scan8,
                                            &scan4,
                                            allow_high_precision_mv,
                                            force_integer_mv,
                                            &global_motion,
                                            enable_dual_filter,
                                            reference_select,
                                            enable_masked_compound,
                                            enable_interintra_compound,
                                            enable_jnt_comp,
                                            order_hint_bits,
                                            order_hint,
                                            ref_order_hints,
                                            skip_mode_present,
                                            skip_mode_frame,
                                            reduced_tx_set,
                                            interp_fixed,
                                            switchable_motion_mode,
                                            allow_warped_motion,
                                            allow_screen_content_tools,
                                            &sign_bias_table,
                                            tpl_frame.as_ref(),
                                            frame_width as usize,
                                        )?;
                                    prev_leaves.push((leaf_mi, skip, is_inter, leaf_refs.0, leaf_refs.1));
                                    // lane-inter8 r2: stamp THIS leaf's own
                                    // 2x2-mi span before the next leaf reads
                                    // it as a neighbour -- the whole-16x16
                                    // stamp below only ever described the
                                    // last leaf, so three quarters of the
                                    // block carried the wrong skip/ref/inter
                                    // state into the next block's contexts
                                    // (class context-read-from-one-cell).
                                    // lane-gmaffine r3: with the mi-granular
                                    // band, the leaf's OWN switchable-filter
                                    // symbols go in here -- the `[3, 3]`
                                    // ("intra, no filter") sentinel this used
                                    // to stamp made `obmc_blend`'s
                                    // `neighbour_filter` PANIC as soon as a
                                    // sibling leaf blended one of these.
                                    neighbours.record_inter_rect_mi(
                                        leaf_mi,
                                        2,
                                        2,
                                        skip,
                                        is_inter,
                                        leaf_refs.0,
                                        leaf_filter,
                                        skip_mode_leaf,
                                    );
                                    if let Some(ref1) = leaf_refs.1
                                        && let Some((_, _, group_idx, idx)) = compound_ctx8
                                    {
                                        neighbours.record_compound_ctx_rect_mi(
                                            leaf_mi, 2, 2, ref1, group_idx, idx,
                                        );
                                    }
                                }
                                // lane-inter8 r2: the four per-leaf
                                // `record_inter_rect_mi` stamps above replace
                                // the single whole-16x16 one that used to run
                                // here with the LAST leaf's state.
                            }
                        }
                    }
                    PARTITION_HORZ_B => {
                        // lane-warp r5: PARTITION_HORZ_B = 32x32 top strip +
                        // 16x16 bottom-left + 16x16 bottom-right. aomenc
                        // carves static regions into it; the strip is
                        // decoded as a square 32x32 block: for a `skip`
                        // block the symbol stream is identical to the true
                        // 32x16 coding (no residual, no motion_mode/warp
                        // symbol), and the square stamps it leaves over the
                        // bottom half (neighbour arrays, skip/lf grids) are
                        // re-stamped by the C/D leaves right after. The
                        // mi-granular `left_side_mi` rows of a last-block-in-
                        // tile strip leak 32-vs-16 only until the per-tile
                        // `Neighbours::new` reset.
                        EXTENDED_PARTITION_HITS.with(|c| c.set(c.get() + 1));
                        let at32 = at;
                        decode_inter_block(
                            &mut dec,
                            &mut cdfs,
                            &mut neighbours,
                            &mut grid,
                            at32,
                            BLOCK,
                            mi_cols,
                            mi_rows,
                            &mut y,
                            &mut u,
                            &mut v,
                            &ref_y,
                            &ref_u,
                            &ref_v,
                            &ref_slots,
                            &sign_bias_table,
                            &global_motion,
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
                            force_integer_mv,
                            interp_fixed,
                            enable_dual_filter,
                            tpl_frame.as_ref(),
                            reference_select,
                            enable_masked_compound,
                            enable_interintra_compound,
                            enable_jnt_comp,
                            order_hint_bits,
                            order_hint,
                            ref_order_hints,
                            skip_mode_present,
                            skip_mode_frame,
                            switchable_motion_mode,
                            allow_warped_motion,
                            true,
                            // lane-rect r2: the top strip's TRUE 32x16
                            // footprint (was BLOCK,BLOCK -- the same square
                            // corner-cut rect-flake-1 exposed on HORZ).
                            BLOCK,
                            SUB,
                            allow_screen_content_tools,
                            frame_width as usize,
                        )?;
                        if (r32 * 2 + 1) as u32 * SUB_MI < mi_rows {
                            decode_inter_block(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                &mut grid,
                            (r32 as usize * 2 + 1, c32 as usize * 2),
                                SUB,
                                mi_cols,
                                mi_rows,
                                &mut y,
                                &mut u,
                                &mut v,
                                &ref_y,
                                &ref_u,
                                &ref_v,
                                &ref_slots,
                                &sign_bias_table,
                            &global_motion,
                                base_q_idx,
                                TxbSet::Luma16,
                                if reduced_tx_set {
                                    TxbSet::Luma16Inter
                                } else {
                                    TxbSet::Luma16InterSet1
                                },
                                TxbSet::Chroma8,
                                TX16,
                                TX8,
                                &scan16,
                                &scan8,
                                2,
                                allow_high_precision_mv,
                                force_integer_mv,
                                interp_fixed,
                                enable_dual_filter,
                                tpl_frame.as_ref(),
                                reference_select,
                                enable_masked_compound,
                                enable_interintra_compound,
                                enable_jnt_comp,
                                order_hint_bits,
                                order_hint,
                                ref_order_hints,
                                skip_mode_present,
                                skip_mode_frame,
                                switchable_motion_mode,
                                allow_warped_motion,
                                false,
                                SUB,
                                SUB,
                                allow_screen_content_tools,
                                frame_width as usize,
                            )?;
                            if (c32 * 2 + 1) as u32 * SUB_MI < mi_cols {
                                decode_inter_block(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    &mut grid,
                                    (r32 as usize * 2 + 1, c32 as usize * 2 + 1),
                                    SUB,
                                    mi_cols,
                                    mi_rows,
                                    &mut y,
                                    &mut u,
                                    &mut v,
                                    &ref_y,
                                    &ref_u,
                                    &ref_v,
                                    &ref_slots,
                                    &sign_bias_table,
                            &global_motion,
                                    base_q_idx,
                                    TxbSet::Luma16,
                                    if reduced_tx_set {
                                        TxbSet::Luma16Inter
                                    } else {
                                        TxbSet::Luma16InterSet1
                                    },
                                    TxbSet::Chroma8,
                                    TX16,
                                    TX8,
                                    &scan16,
                                    &scan8,
                                    2,
                                    allow_high_precision_mv,
                                    force_integer_mv,
                                    interp_fixed,
                                    enable_dual_filter,
                                    tpl_frame.as_ref(),
                                    reference_select,
                                    enable_masked_compound,
                                    enable_interintra_compound,
                                    enable_jnt_comp,
                                    order_hint_bits,
                                    order_hint,
                                    ref_order_hints,
                                    skip_mode_present,
                                    skip_mode_frame,
                                    switchable_motion_mode,
                                    allow_warped_motion,
                                    false,
                                    SUB,
                                    SUB,
                                    allow_screen_content_tools,
                                    frame_width as usize,
                                )?;
                            }
                        }
                    }
                    PARTITION_HORZ => {
                        // lane-rect r2: two true 32x16 strips. Both read at
                        // `side=BLOCK` (CDF/size-class selection stays square,
                        // matching HORZ_B's accepted corner-cut) but bw4/bh4
                        // (mvstack.rs, `motion_mode_eligible`) now derive from
                        // `write_w`/`write_h`'s true 32x16 footprint -- the
                        // pin's fix (r1's finding).
                        RECT_PARTITION_HITS.with(|c| c.set(c.get() + 1));
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
                            &ref_slots,
                            &sign_bias_table,
                            &global_motion,
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
                            force_integer_mv,
                            interp_fixed,
                            enable_dual_filter,
                            tpl_frame.as_ref(),
                            reference_select,
                            enable_masked_compound,
                            enable_interintra_compound,
                            enable_jnt_comp,
                            order_hint_bits,
                            order_hint,
                            ref_order_hints,
                            skip_mode_present,
                            skip_mode_frame,
                            switchable_motion_mode,
                            allow_warped_motion,
                            true,
                            BLOCK,
                            SUB,
                            allow_screen_content_tools,
                            frame_width as usize,
                        )?;
                        decode_inter_block(
                            &mut dec,
                            &mut cdfs,
                            &mut neighbours,
                            &mut grid,
                            (r32 as usize * 2 + 1, c32 as usize * 2),
                            BLOCK,
                            mi_cols,
                            mi_rows,
                            &mut y,
                            &mut u,
                            &mut v,
                            &ref_y,
                            &ref_u,
                            &ref_v,
                            &ref_slots,
                            &sign_bias_table,
                            &global_motion,
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
                            force_integer_mv,
                            interp_fixed,
                            enable_dual_filter,
                            tpl_frame.as_ref(),
                            reference_select,
                            enable_masked_compound,
                            enable_interintra_compound,
                            enable_jnt_comp,
                            order_hint_bits,
                            order_hint,
                            ref_order_hints,
                            skip_mode_present,
                            skip_mode_frame,
                            switchable_motion_mode,
                            allow_warped_motion,
                            true,
                            BLOCK,
                            SUB,
                            allow_screen_content_tools,
                            frame_width as usize,
                        )?;
                    }
                    PARTITION_VERT => {
                        // lane-rect r2: mirror of PARTITION_HORZ above with
                        // width/height swapped.
                        RECT_PARTITION_HITS.with(|c| c.set(c.get() + 1));
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
                            &ref_slots,
                            &sign_bias_table,
                            &global_motion,
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
                            force_integer_mv,
                            interp_fixed,
                            enable_dual_filter,
                            tpl_frame.as_ref(),
                            reference_select,
                            enable_masked_compound,
                            enable_interintra_compound,
                            enable_jnt_comp,
                            order_hint_bits,
                            order_hint,
                            ref_order_hints,
                            skip_mode_present,
                            skip_mode_frame,
                            switchable_motion_mode,
                            allow_warped_motion,
                            true,
                            SUB,
                            BLOCK,
                            allow_screen_content_tools,
                            frame_width as usize,
                        )?;
                        decode_inter_block(
                            &mut dec,
                            &mut cdfs,
                            &mut neighbours,
                            &mut grid,
                            (r32 as usize * 2, c32 as usize * 2 + 1),
                            BLOCK,
                            mi_cols,
                            mi_rows,
                            &mut y,
                            &mut u,
                            &mut v,
                            &ref_y,
                            &ref_u,
                            &ref_v,
                            &ref_slots,
                            &sign_bias_table,
                            &global_motion,
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
                            force_integer_mv,
                            interp_fixed,
                            enable_dual_filter,
                            tpl_frame.as_ref(),
                            reference_select,
                            enable_masked_compound,
                            enable_interintra_compound,
                            enable_jnt_comp,
                            order_hint_bits,
                            order_hint,
                            ref_order_hints,
                            skip_mode_present,
                            skip_mode_frame,
                            switchable_motion_mode,
                            allow_warped_motion,
                            true,
                            SUB,
                            BLOCK,
                            allow_screen_content_tools,
                            frame_width as usize,
                        )?;
                    }
                    PARTITION_HORZ_A => {
                        // lane-partab r1: two 16x16 squares on top + a true
                        // 32x16 strip below (libaom decode_partition
                        // PARTITION_HORZ_A: TL, TR, bottom strip).
                        PARTAB_HITS.with(|c| c.set(c.get() + 1));
                        decode_inter_block(
                            &mut dec,
                            &mut cdfs,
                            &mut neighbours,
                            &mut grid,
                            at,
                            SUB,
                            mi_cols,
                            mi_rows,
                            &mut y,
                            &mut u,
                            &mut v,
                            &ref_y,
                            &ref_u,
                            &ref_v,
                            &ref_slots,
                            &sign_bias_table,
                            &global_motion,
                            base_q_idx,
                            TxbSet::Luma16,
                            if reduced_tx_set {
                                TxbSet::Luma16Inter
                            } else {
                                TxbSet::Luma16InterSet1
                            },
                            TxbSet::Chroma8,
                            TX16,
                            TX8,
                            &scan16,
                            &scan8,
                            2,
                            allow_high_precision_mv,
                            force_integer_mv,
                            interp_fixed,
                            enable_dual_filter,
                            tpl_frame.as_ref(),
                            reference_select,
                            enable_masked_compound,
                            enable_interintra_compound,
                            enable_jnt_comp,
                            order_hint_bits,
                            order_hint,
                            ref_order_hints,
                            skip_mode_present,
                            skip_mode_frame,
                            switchable_motion_mode,
                            allow_warped_motion,
                            false,
                            SUB,
                            SUB,
                            allow_screen_content_tools,
                            frame_width as usize,
                        )?;
                        decode_inter_block(
                            &mut dec,
                            &mut cdfs,
                            &mut neighbours,
                            &mut grid,
                            (r32 as usize * 2, c32 as usize * 2 + 1),
                            SUB,
                            mi_cols,
                            mi_rows,
                            &mut y,
                            &mut u,
                            &mut v,
                            &ref_y,
                            &ref_u,
                            &ref_v,
                            &ref_slots,
                            &sign_bias_table,
                            &global_motion,
                            base_q_idx,
                            TxbSet::Luma16,
                            if reduced_tx_set {
                                TxbSet::Luma16Inter
                            } else {
                                TxbSet::Luma16InterSet1
                            },
                            TxbSet::Chroma8,
                            TX16,
                            TX8,
                            &scan16,
                            &scan8,
                            2,
                            allow_high_precision_mv,
                            force_integer_mv,
                            interp_fixed,
                            enable_dual_filter,
                            tpl_frame.as_ref(),
                            reference_select,
                            enable_masked_compound,
                            enable_interintra_compound,
                            enable_jnt_comp,
                            order_hint_bits,
                            order_hint,
                            ref_order_hints,
                            skip_mode_present,
                            skip_mode_frame,
                            switchable_motion_mode,
                            allow_warped_motion,
                            false,
                            SUB,
                            SUB,
                            allow_screen_content_tools,
                            frame_width as usize,
                        )?;
                        decode_inter_block(
                            &mut dec,
                            &mut cdfs,
                            &mut neighbours,
                            &mut grid,
                            (r32 as usize * 2 + 1, c32 as usize * 2),
                            BLOCK,
                            mi_cols,
                            mi_rows,
                            &mut y,
                            &mut u,
                            &mut v,
                            &ref_y,
                            &ref_u,
                            &ref_v,
                            &ref_slots,
                            &sign_bias_table,
                            &global_motion,
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
                            force_integer_mv,
                            interp_fixed,
                            enable_dual_filter,
                            tpl_frame.as_ref(),
                            reference_select,
                            enable_masked_compound,
                            enable_interintra_compound,
                            enable_jnt_comp,
                            order_hint_bits,
                            order_hint,
                            ref_order_hints,
                            skip_mode_present,
                            skip_mode_frame,
                            switchable_motion_mode,
                            allow_warped_motion,
                            true,
                            BLOCK,
                            SUB,
                            allow_screen_content_tools,
                            frame_width as usize,
                        )?;
                    }
                    PARTITION_VERT_A => {
                        // lane-partab r1: mirror of PARTITION_HORZ_A with
                        // width/height swapped (TL, BL, right 16x32 strip).
                        PARTAB_HITS.with(|c| c.set(c.get() + 1));
                        let _vert_ab = crate::encode::Reach::vert_ab_partition();
                        decode_inter_block(
                            &mut dec,
                            &mut cdfs,
                            &mut neighbours,
                            &mut grid,
                            at,
                            SUB,
                            mi_cols,
                            mi_rows,
                            &mut y,
                            &mut u,
                            &mut v,
                            &ref_y,
                            &ref_u,
                            &ref_v,
                            &ref_slots,
                            &sign_bias_table,
                            &global_motion,
                            base_q_idx,
                            TxbSet::Luma16,
                            if reduced_tx_set {
                                TxbSet::Luma16Inter
                            } else {
                                TxbSet::Luma16InterSet1
                            },
                            TxbSet::Chroma8,
                            TX16,
                            TX8,
                            &scan16,
                            &scan8,
                            2,
                            allow_high_precision_mv,
                            force_integer_mv,
                            interp_fixed,
                            enable_dual_filter,
                            tpl_frame.as_ref(),
                            reference_select,
                            enable_masked_compound,
                            enable_interintra_compound,
                            enable_jnt_comp,
                            order_hint_bits,
                            order_hint,
                            ref_order_hints,
                            skip_mode_present,
                            skip_mode_frame,
                            switchable_motion_mode,
                            allow_warped_motion,
                            false,
                            SUB,
                            SUB,
                            allow_screen_content_tools,
                            frame_width as usize,
                        )?;
                        decode_inter_block(
                            &mut dec,
                            &mut cdfs,
                            &mut neighbours,
                            &mut grid,
                            (r32 as usize * 2 + 1, c32 as usize * 2),
                            SUB,
                            mi_cols,
                            mi_rows,
                            &mut y,
                            &mut u,
                            &mut v,
                            &ref_y,
                            &ref_u,
                            &ref_v,
                            &ref_slots,
                            &sign_bias_table,
                            &global_motion,
                            base_q_idx,
                            TxbSet::Luma16,
                            if reduced_tx_set {
                                TxbSet::Luma16Inter
                            } else {
                                TxbSet::Luma16InterSet1
                            },
                            TxbSet::Chroma8,
                            TX16,
                            TX8,
                            &scan16,
                            &scan8,
                            2,
                            allow_high_precision_mv,
                            force_integer_mv,
                            interp_fixed,
                            enable_dual_filter,
                            tpl_frame.as_ref(),
                            reference_select,
                            enable_masked_compound,
                            enable_interintra_compound,
                            enable_jnt_comp,
                            order_hint_bits,
                            order_hint,
                            ref_order_hints,
                            skip_mode_present,
                            skip_mode_frame,
                            switchable_motion_mode,
                            allow_warped_motion,
                            false,
                            SUB,
                            SUB,
                            allow_screen_content_tools,
                            frame_width as usize,
                        )?;
                        decode_inter_block(
                            &mut dec,
                            &mut cdfs,
                            &mut neighbours,
                            &mut grid,
                            (r32 as usize * 2, c32 as usize * 2 + 1),
                            BLOCK,
                            mi_cols,
                            mi_rows,
                            &mut y,
                            &mut u,
                            &mut v,
                            &ref_y,
                            &ref_u,
                            &ref_v,
                            &ref_slots,
                            &sign_bias_table,
                            &global_motion,
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
                            force_integer_mv,
                            interp_fixed,
                            enable_dual_filter,
                            tpl_frame.as_ref(),
                            reference_select,
                            enable_masked_compound,
                            enable_interintra_compound,
                            enable_jnt_comp,
                            order_hint_bits,
                            order_hint,
                            ref_order_hints,
                            skip_mode_present,
                            skip_mode_frame,
                            switchable_motion_mode,
                            allow_warped_motion,
                            true,
                            SUB,
                            BLOCK,
                            allow_screen_content_tools,
                            frame_width as usize,
                        )?;
                    }
                    PARTITION_VERT_B => {
                        // lane-partab r1: true 16x32 strip on the left + two
                        // 16x16 squares on the right (libaom decode_partition
                        // PARTITION_VERT_B: left strip, TR, BR).
                        PARTAB_HITS.with(|c| c.set(c.get() + 1));
                        let _vert_ab = crate::encode::Reach::vert_ab_partition();
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
                            &ref_slots,
                            &sign_bias_table,
                            &global_motion,
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
                            force_integer_mv,
                            interp_fixed,
                            enable_dual_filter,
                            tpl_frame.as_ref(),
                            reference_select,
                            enable_masked_compound,
                            enable_interintra_compound,
                            enable_jnt_comp,
                            order_hint_bits,
                            order_hint,
                            ref_order_hints,
                            skip_mode_present,
                            skip_mode_frame,
                            switchable_motion_mode,
                            allow_warped_motion,
                            true,
                            SUB,
                            BLOCK,
                            allow_screen_content_tools,
                            frame_width as usize,
                        )?;
                        decode_inter_block(
                            &mut dec,
                            &mut cdfs,
                            &mut neighbours,
                            &mut grid,
                            (r32 as usize * 2, c32 as usize * 2 + 1),
                            SUB,
                            mi_cols,
                            mi_rows,
                            &mut y,
                            &mut u,
                            &mut v,
                            &ref_y,
                            &ref_u,
                            &ref_v,
                            &ref_slots,
                            &sign_bias_table,
                            &global_motion,
                            base_q_idx,
                            TxbSet::Luma16,
                            if reduced_tx_set {
                                TxbSet::Luma16Inter
                            } else {
                                TxbSet::Luma16InterSet1
                            },
                            TxbSet::Chroma8,
                            TX16,
                            TX8,
                            &scan16,
                            &scan8,
                            2,
                            allow_high_precision_mv,
                            force_integer_mv,
                            interp_fixed,
                            enable_dual_filter,
                            tpl_frame.as_ref(),
                            reference_select,
                            enable_masked_compound,
                            enable_interintra_compound,
                            enable_jnt_comp,
                            order_hint_bits,
                            order_hint,
                            ref_order_hints,
                            skip_mode_present,
                            skip_mode_frame,
                            switchable_motion_mode,
                            allow_warped_motion,
                            false,
                            SUB,
                            SUB,
                            allow_screen_content_tools,
                            frame_width as usize,
                        )?;
                        decode_inter_block(
                            &mut dec,
                            &mut cdfs,
                            &mut neighbours,
                            &mut grid,
                            (r32 as usize * 2 + 1, c32 as usize * 2 + 1),
                            SUB,
                            mi_cols,
                            mi_rows,
                            &mut y,
                            &mut u,
                            &mut v,
                            &ref_y,
                            &ref_u,
                            &ref_v,
                            &ref_slots,
                            &sign_bias_table,
                            &global_motion,
                            base_q_idx,
                            TxbSet::Luma16,
                            if reduced_tx_set {
                                TxbSet::Luma16Inter
                            } else {
                                TxbSet::Luma16InterSet1
                            },
                            TxbSet::Chroma8,
                            TX16,
                            TX8,
                            &scan16,
                            &scan8,
                            2,
                            allow_high_precision_mv,
                            force_integer_mv,
                            interp_fixed,
                            enable_dual_filter,
                            tpl_frame.as_ref(),
                            reference_select,
                            enable_masked_compound,
                            enable_interintra_compound,
                            enable_jnt_comp,
                            order_hint_bits,
                            order_hint,
                            ref_order_hints,
                            skip_mode_present,
                            skip_mode_frame,
                            switchable_motion_mode,
                            allow_warped_motion,
                            false,
                            SUB,
                            SUB,
                            allow_screen_content_tools,
                            frame_width as usize,
                        )?;
                    }
                    _ => {
                        return Err(unsupported(format!(
                            "an INTER 32x32 partition type this decoder does not code (value={part32})"
                        )));
                    }
                }
            }
        }
    }

        if tile_num == tile_info.context_update_tile_id {
            result_cdfs = cdfs;
        }
    }

    // lane-comppin r4: pre-loop-filter decode-order dump, matching aomdec's
    // own EC_AV1_PREFILT_DUMP shape (decodeframe.c ~5451) -- diffs against
    // that isolate whether a decode-order frame's mismatch already exists
    // in reconstruction (this dump) vs is introduced by the loop filter
    // (EC_AV1_DECODE_ORDER_DUMP's post-filter dump).
    if let Ok(path) = std::env::var("EC_AV1_PREFILT_DUMP") {
        use std::io::Write;
        let idx = PREFILT_PICTURE_IDX.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut f) = std::fs::File::create(format!("{path}.f{idx}")) {
            let narrow = |p: &PlaneBuf| -> Vec<u8> { p.data.iter().map(|&s| s as u8).collect() };
            let _ = f.write_all(&narrow(&y));
            let _ = f.write_all(&narrow(&u));
            let _ = f.write_all(&narrow(&v));
        }
    } else {
        PREFILT_PICTURE_IDX.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    apply_deblock(
        &mut y,
        &mut u,
        &mut v,
        loop_filter,
        &neighbours,
        frame_width as usize,
        frame_height as usize,
    );
    let (deblocked_y, deblocked_u, deblocked_v) = (y.clone(), u.clone(), v.clone());
    // lane-tiny r4: post-deblock / post-CDEF dumps mirroring aomdec's own
    // EC_AV1_POSTDEBLOCK_DUMP (decodeframe.c ~5404) -- with the pre-filter
    // dump above these bisect WHICH filter stage introduced a mismatch.
    dump_stage("EC_AV1_POSTDEBLOCK_DUMP", &y, &u, &v);
    apply_cdef(&mut y, &mut u, &mut v, cdef, &neighbours);
    dump_stage("EC_AV1_POSTCDEF_DUMP", &y, &u, &v);
    apply_loop_restoration(&mut y, &mut u, &mut v, &deblocked_y, &deblocked_u, &deblocked_v, lr, &lr_grid);

    let motion_field = build_motion_field(
        &grid,
        mi_cols as usize,
        mi_rows as usize,
        order_hint,
        ref_order_hints,
    );

    let (fw, fh) = (frame_width as usize, frame_height as usize);
    if fw == width && fh == height {
        LAST_FRAME_WIDE_MARGIN.with(|m| *m.borrow_mut() = None);
        return Ok((
            Picture {
                width,
                height,
                y: y.data,
                u: u.data,
                v: v.data,
            },
            result_cdfs,
            motion_field,
        ));
    }
    let crop = |plane: &PlaneBuf, w: usize, h: usize| -> Vec<u16> {
        let mut out = Vec::with_capacity(w * h);
        for row in 0..h {
            out.extend(plane.data[row * plane.width..][..w].iter().copied());
        }
        out
    };
    // lane-superres r10 (fixed r9): same real-margin stash as
    // `decode_key_frame_tile_with_cdfs` (see that function's comment) -- the
    // real reconstructed extent is the mi-aligned `true_width`/`true_height`
    // (`mi_cols`/`mi_rows` * 4), NOT the superblock-padded `width`/`height`
    // this branch was reading before: columns `[true_width, width)` were
    // never actually coded (the last partial superblock stops at
    // `true_width`), so stashing out to `width` handed the chroma upscale's
    // right-edge replicate a slice of uninitialized/stale buffer one column
    // early -- the same shorten-vs-replicate shape as `av1-truesize`. The
    // key-frame branch above already used `true_width`/`true_height`; this
    // just matches it.
    LAST_FRAME_WIDE_MARGIN.with(|m| {
        *m.borrow_mut() = if true_width > fw || true_height > fh {
            Some(Picture {
                width: true_width,
                height: true_height,
                y: crop(&y, true_width, true_height),
                u: crop(&u, true_width.div_ceil(2), true_height.div_ceil(2)),
                v: crop(&v, true_width.div_ceil(2), true_height.div_ceil(2)),
            })
        } else {
            None
        }
    });
    Ok((
        Picture {
            width: fw,
            height: fh,
            y: crop(&y, fw, fh),
            u: crop(&u, fw.div_ceil(2), fh.div_ceil(2)),
            v: crop(&v, fw.div_ceil(2), fh.div_ceil(2)),
        },
        result_cdfs,
        motion_field,
    ))
}

#[cfg(test)]
mod tests {

    /// lane-fistrip r1 (class `enumerate-the-table-domain`): every block
    /// shape this decoder can hand [`filter_intra_size_class_rect`], against
    /// `av1_filter_intra_allowed_bsize` (`block_size_wide <= 32 &&
    /// block_size_high <= 32`) and `default_filter_intra_cdfs`
    /// (entropymode.c) row by row. A class that exists must carry ITS OWN
    /// bsize's default probability -- a wrong row is a silently different
    /// symbol, not a decode error. The shapes with no class are listed here
    /// with their reason so the residue is pinned rather than implied.
    #[test]
    fn filter_intra_classes_carry_their_own_libaom_default_row() {
        // `default_filter_intra_cdfs[bsize]`, BLOCK_SIZES_ALL order, as
        // (width, height, default, refused_by) for the shapes <= 32 on both
        // axes. The first three fields are GENERATED from the oracle by
        // `scripts/extract-filter-intra-cdfs.py` (it reads BLOCK_SIZES_ALL's
        // order from enums.h and the table from entropymode.c, so no hand
        // transcription can drift -- lane-fistrip r1 listed the four 1:4
        // shapes as 16384 out of exactly that kind of guess; their real rows
        // are 12770/10368/20229/18101). `refused_by` is `None` for a shape
        // this decoder's partition tree reaches today -- it must own a class,
        // and that class must carry exactly this row -- and the refusal
        // string that blocks the shape otherwise. Either way the row is
        // pinned HERE, so the round that lifts one of those refusals cannot
        // land a wrong CDF row for the shape it unlocks.
        const BELOW_8X8: &str =
            "a partition below 8x8 (this decoder codes no leaf smaller than 8x8)";
        const PART16: &str = "a HORZ_A/HORZ_B/VERT_A partition below 16x16 (this decoder codes \
             only the square arms, HORZ, VERT, VERT_B, and a clean split below 16x16)";
        const PART32: &str = "a 32x32 partition type this decoder does not code (value={part32})";
        let libaom: &[(usize, usize, u16, Option<&str>)] = &[
            (4, 4, 4621, None),
            (4, 8, 6743, Some(BELOW_8X8)),
            (8, 4, 5893, Some(BELOW_8X8)),
            (8, 8, 7866, None),
            (8, 16, 12551, None),
            (16, 8, 9394, None),
            (16, 16, 12408, None),
            (16, 32, 14301, None),
            (32, 16, 12756, None),
            (32, 32, 22343, None),
            // The 1:4 strips: a 16x16-level PARTITION_HORZ_4/VERT_4 gives
            // 4x16/16x4, a 32x32-level one 8x32/32x8, and both partition
            // values refuse by name before any leaf is coded.
            (4, 16, 12770, Some(PART16)),
            (16, 4, 10368, Some(PART16)),
            (8, 32, 20229, Some(PART32)),
            (32, 8, 18101, Some(PART32)),
        ];
        let mut classes_seen = std::collections::BTreeSet::new();
        for &(bw, bh, default, refused_by) in libaom {
            match filter_intra_size_class_rect(bw, bh) {
                Some(class) => {
                    assert!(
                        refused_by.is_none(),
                        "{bw}x{bh} has a CDF class but is listed as refused by {refused_by:?} -- \
                         one of the two is stale"
                    );
                    assert_eq!(
                        crate::cdf::FILTER_INTRA[class][0],
                        default,
                        "{bw}x{bh} (class {class}) must carry default_filter_intra_cdfs' own row"
                    );
                    assert_eq!(
                        &crate::cdf::FILTER_INTRA[class][1..],
                        &[32768u16, 0][..],
                        "{bw}x{bh} (class {class}) is a 2-symbol CDF: {{p, 32768, count}}"
                    );
                    assert!(
                        classes_seen.insert(class),
                        "class {class} is claimed by two shapes -- one of them reads the other's \
                         probability"
                    );
                }
                None => {
                    let why = refused_by.unwrap_or_else(|| {
                        panic!(
                            "{bw}x{bh} is inside av1_filter_intra_allowed_bsize and reachable, \
                             but has no CDF class -- its use_filter_intra symbol is never read"
                        )
                    });
                    assert!(!why.is_empty(), "{bw}x{bh}'s refusal string must name the blocker");
                }
            }
        }
        // The other direction: every row of `cdf::FILTER_INTRA` was claimed
        // by exactly one shape above, so no row can be left over (or shared)
        // once a class is added or removed.
        assert_eq!(
            classes_seen.len(),
            crate::cdf::FILTER_INTRA.len(),
            "cdf::FILTER_INTRA has {} rows but only {:?} are claimed by an allowed shape",
            crate::cdf::FILTER_INTRA.len(),
            classes_seen
        );
        // Past the bound on either axis: no symbol at all (spec
        // `filter_intra_mode_info` never reads one).
        for (bw, bh) in [(64, 64), (64, 32), (32, 64)] {
            assert!(
                filter_intra_size_class_rect(bw, bh).is_none(),
                "{bw}x{bh} is past av1_filter_intra_allowed_bsize's <=32 bound"
            );
        }
    }
    use super::*;

    /// lane-tx64x16 r3: the two new 4:1 chroma scans are a TRANSPOSED PAIR --
    /// `SCAN_4X16[i]` must name the transpose of the position
    /// `SCAN_16X4[i]` names (class scan-weights-cross-axis: a naive copy of
    /// libaom's column-major `default_scan_*` installs the transpose and only
    /// an asymmetric check catches it). Also pins both as permutations, and
    /// pins the same relation for the 32x8/8x32 luma pair this arm reuses.
    #[test]
    fn the_rect4_scans_are_transposed_pairs() {
        for (a, b, w, h) in [
            (&SCAN_16X4[..], &SCAN_4X16[..], 16usize, 4usize),
            (&SCAN_32X8[..], &SCAN_8X32[..], 32, 8),
        ] {
            let mut seen_a: Vec<u16> = a.to_vec();
            seen_a.sort_unstable();
            assert_eq!(
                seen_a,
                (0..(w * h) as u16).collect::<Vec<_>>(),
                "{w}x{h} scan is not a permutation of its positions"
            );
            for (i, (&pa, &pb)) in a.iter().zip(b.iter()).enumerate() {
                let (row, col) = (pa as usize / w, pa as usize % w);
                assert_eq!(
                    pb as usize,
                    col * w.min(h) + row,
                    "scan step {i}: {w}x{h} visits (row {row}, col {col}) but its transpose \
                     visits position {pb}"
                );
            }
        }
    }

    /// lane-rectsplit r4 (class `enumerate-table-domain`): [`base_ctx`]'s
    /// `TX_CLASS_2D` arm must, over the WHOLE 5x5 domain of both rect shapes
    /// and the square one, return exactly what libaom's
    /// `av1_nz_map_ctx_offset[tx_size][coeff_idx]` holds -- the flat tables
    /// transcribed in [`cdf`], read at libaom's own COLUMN-major
    /// `coeff_idx = col * 32 + row`, i.e. `table[col][row]`. r1..r3 read them
    /// `[row][col]`, which is the transpose (the OTHER shape's rule, with 11
    /// and 16 swapped): it desynced the first superblock strip whose luma
    /// corner held a coefficient off the first row/column.
    #[test]
    fn base_ctx_rect_offsets_match_the_transcribed_tables_over_the_whole_domain() {
        let grid = [0i32; 32 * 32];
        for (shape, table) in [
            (Some((64usize, 32usize)), &cdf::NZ_MAP_CTX_OFFSET_64X32),
            (Some((32, 64)), &cdf::NZ_MAP_CTX_OFFSET_32X64),
            (None, &cdf::NZ_MAP_CTX_OFFSET_32),
        ] {
            for row in 0..5usize {
                for col in 0..5usize {
                    // An all-zero neighbourhood makes the magnitude term 0,
                    // so `base_ctx` returns the offset itself.
                    let got = base_ctx(&grid, 32, row, col, TxClass::TwoD, shape);
                    let want = if row == 0 && col == 0 {
                        0
                    } else {
                        table[col][row] as usize
                    };
                    assert_eq!(got, want, "{shape:?} at (row {row}, col {col})");
                }
            }
        }
    }

    // `crate::tile`'s `flat_key_frame_tile`/`dc_key_frame_tile_levels`/
    // `split_dc_key_frame_tile` are not exercised here: they are synthetic
    // single-purpose writers with their own skip-context rule (keyed to
    // superblock position, not the neighbour-tracked one every real block
    // uses), never reached by `encode_key_frame_with_modes` — decoding them
    // would need a second, throwaway context convention next to the real
    // one below. `sb_coeff_key_frame_tile`, what the real encoder writes, is
    // this decoder's one target, and the round-trip test below is against
    // that path with real quantised residual, not a synthetic all-DC frame.

    /// [`SCAN_8X4`]/[`SCAN_4X8`] (lane-rectx r2): a bijection check alone
    /// cannot catch a transposed pair -- a swap of two orientation tables is
    /// still a bijection over the same 32 positions. This pins BOTH arrays
    /// at once, element-for-element, against literal transcriptions of
    /// libaom's real `default_scan_4x8`/`default_scan_8x4` in
    /// `av1/common/scan.c` (`~/.cache/aom-oracle/src`, read directly for
    /// this test, not re-derived from the zigzag formula). The two source
    /// sequences are themselves different in their own right (not equal to
    /// each other), so this is asymmetric in the two axes: if the repo's
    /// `SCAN_8X4`/`SCAN_4X8` were swapped with each other, one side of this
    /// assert pair would fail even though each individual array would still
    /// pass a same-orientation bijection test.
    #[test]
    fn scan_8x4_and_4x8_are_not_transposed_against_each_other() {
        // libaom `default_scan_4x8` verbatim (scan.c:29-32) -- this repo
        // names it `SCAN_8X4` under the established w/h axis swap (see
        // `SCAN_8X4`'s own doc comment).
        const LIBAOM_DEFAULT_SCAN_4X8: [u16; 32] = [
            0, 8, 1, 16, 9, 2, 24, 17, 10, 3, 25, 18, 11, 4, 26, 19, 12, 5, 27, 20, 13, 6, 28, 21,
            14, 7, 29, 22, 15, 30, 23, 31,
        ];
        // libaom `default_scan_8x4` verbatim (scan.c:44-47) -- this repo's
        // `SCAN_4X8`.
        const LIBAOM_DEFAULT_SCAN_8X4: [u16; 32] = [
            0, 1, 4, 2, 5, 8, 3, 6, 9, 12, 7, 10, 13, 16, 11, 14, 17, 20, 15, 18, 21, 24, 19, 22,
            25, 28, 23, 26, 29, 27, 30, 31,
        ];
        assert_ne!(
            LIBAOM_DEFAULT_SCAN_4X8, LIBAOM_DEFAULT_SCAN_8X4,
            "the two libaom source sequences must differ, or this pin would \
             pass regardless of a swap"
        );
        assert_eq!(SCAN_8X4, LIBAOM_DEFAULT_SCAN_4X8, "SCAN_8X4 vs libaom default_scan_4x8");
        assert_eq!(SCAN_4X8, LIBAOM_DEFAULT_SCAN_8X4, "SCAN_4X8 vs libaom default_scan_8x4");
    }

    /// lane-rectx r3: `av1_filter_intra_allowed_bsize` is "both sides <= 32",
    /// so a DC_PRED 16x8/8x16 strip reads a `use_filter_intra` flag exactly
    /// like a square 16x16 does. [`filter_intra_size_class_rect`] returned
    /// `None` at those two sizes, so the symbol was never read and the tile
    /// desynced from that block on (root-caused against an instrumented
    /// aomdec `EC_TRACE_COEFF` ladder on a real stream: identical `mode=0`/
    /// `uv_mode=9` values, our range 34808 vs the oracle's 40668 at the very
    /// next transform block).
    ///
    /// The two rows are ASYMMETRIC (9394 for 16x8, 12551 for 8x16), so this
    /// pin fails if the pair is ever transposed -- the shape the repo's
    /// scan tables already carry (`SCAN_8X4` == libaom `default_scan_4x8`).
    #[test]
    fn a_rect_strip_below_16x16_reads_its_own_filter_intra_cdf_row() {
        // `default_filter_intra_cdfs` (entropymode.c), indexed by BLOCK_SIZES_ALL:
        // [4] = BLOCK_8X16, [5] = BLOCK_16X8, [7] = BLOCK_16X32, [8] = BLOCK_32X16.
        const LIBAOM_8X16: u16 = 12551;
        const LIBAOM_16X8: u16 = 9394;
        let c16x8 = filter_intra_size_class_rect(16, 8).expect("16x8 reads use_filter_intra");
        let c8x16 = filter_intra_size_class_rect(8, 16).expect("8x16 reads use_filter_intra");
        assert_ne!(c16x8, c8x16, "the two orientations must not share a CDF row");
        assert_eq!(cdf::FILTER_INTRA[c16x8][0], LIBAOM_16X8, "16x8 filter_intra row");
        assert_eq!(cdf::FILTER_INTRA[c8x16][0], LIBAOM_8X16, "8x16 filter_intra row");
        // The sizes one class up keep their own distinct rows (lane-intradisp).
        assert_eq!(cdf::FILTER_INTRA[filter_intra_size_class_rect(32, 16).unwrap()][0], 12756);
        assert_eq!(cdf::FILTER_INTRA[filter_intra_size_class_rect(16, 32).unwrap()][0], 14301);
        // A size with a side over 32 is genuinely not allowed the flag.
        assert_eq!(filter_intra_size_class_rect(64, 32), None);
    }

    /// `is_uni_comp_ref` (lane-av1comp): a pair is unidirectional exactly
    /// when both references sit on the same temporal side -- the three
    /// `uni_comp_ref` bitstream pairs (LAST/LAST2, LAST/LAST3, LAST/GOLDEN)
    /// all read `true`, a forward/backward mix (LAST/BWDREF) reads `false`,
    /// and a same-side non-`uni_comp_ref` pair (BWDREF/ALTREF) still reads
    /// `true` (spec `has_uni_comp_refs` is symmetric in the reference set,
    /// not just the three enumerated syntax pairs).
    #[test]
    fn is_uni_comp_ref_matches_forward_backward_sides() {
        assert!(is_uni_comp_ref(LAST_FRAME, LAST2_FRAME));
        assert!(is_uni_comp_ref(LAST_FRAME, LAST3_FRAME));
        assert!(is_uni_comp_ref(LAST_FRAME, GOLDEN_FRAME));
        assert!(is_uni_comp_ref(BWDREF_FRAME, ALTREF_FRAME));
        assert!(!is_uni_comp_ref(LAST_FRAME, BWDREF_FRAME));
        assert!(!is_uni_comp_ref(GOLDEN_FRAME, ALTREF_FRAME));
    }

    /// [`Neighbours::record_compound_ctx`] stamps `above_ref1`/`left_ref1`
    /// with the real second reference (lane-av1comp round 12: previously
    /// always cleared to `None` by `record_inter`, so the next block's own
    /// `NeighbourRef::uni` -- and `comp_reference_type_ctx` downstream of it
    /// -- could never see a real compound neighbour).
    #[test]
    fn record_compound_ctx_stamps_ref1_for_the_next_block_to_read() {
        let mut n = Neighbours::new(4, 4, 16, 16);
        n.record_inter((0, 0), SUB, false, true, LAST_FRAME, [3, 3], false);
        assert_eq!(n.above_ref1[0], None, "record_inter alone never sets ref1");
        n.record_compound_ctx((0, 0), SUB, ALTREF_FRAME, 0, 1);
        assert_eq!(n.above_ref1[0], Some(ALTREF_FRAME));
        assert_eq!(n.left_ref1[0], Some(ALTREF_FRAME));
        assert!(!is_uni_comp_ref(LAST_FRAME, n.above_ref1[0].unwrap()));
    }

    /// [`cdf::COMPOUND_MODE_CTX_MAP`] folded by hand against libaom's
    /// `av1_mode_context_analyzer` compound branch (`mvref_common.h`):
    /// `comp_ctx = compound_mode_ctx_map[refmv_ctx >> 1][min(newmv_ctx, 4)]`
    /// -- a few corner and interior points, not the whole 6x6 grid.
    #[test]
    fn compound_mode_ctx_map_matches_libaom_corners() {
        assert_eq!(cdf::COMPOUND_MODE_CTX_MAP[0][0], 0);
        assert_eq!(cdf::COMPOUND_MODE_CTX_MAP[0][4], 1);
        assert_eq!(cdf::COMPOUND_MODE_CTX_MAP[1][0], 1);
        assert_eq!(cdf::COMPOUND_MODE_CTX_MAP[1][3], 4);
        assert_eq!(cdf::COMPOUND_MODE_CTX_MAP[2][2], 5);
        assert_eq!(cdf::COMPOUND_MODE_CTX_MAP[2][4], 7);
        // `ref_mv_ctx` up to 5 (`>> 1` = 2, the map's last row) and
        // `new_mv_ctx` up to 5 (clamped to 4, `COMP_NEWMV_CTXS - 1`) are
        // exactly the ranges `find_mv_stack_compound` produces.
        assert_eq!(cdf::COMPOUND_MODE_CTX_MAP[5 >> 1][5usize.min(4)], 7);
    }

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
                false,
                false,
            )
            .is_err()
        );
    }

    /// Encodes and decodes one picture with `modes`, asserting the decoder's
    /// planes are byte-exact against the encoder's own reconstruction.
    fn round_trips(w: usize, h: usize, modes: &[u8]) {
        use crate::encode::{Encoded, Picture as Pic};
        let mut source = vec![0u16; w * h];
        for row in 0..h {
            for col in 0..w {
                source[row * w + col] = ((row * 3 + col * 5) % 251) as u16;
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
            false,
            false,
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
            false,
            false,
        )
        .unwrap();
        assert!(
            decoded.y.iter().all(|&s| s == 128),
            "a skipped DC_PRED block with no neighbours predicts flat mid-grey"
        );
    }

    /// An inter tile's SB-level `part64` used to be read and thrown away
    /// (`let _ = part64;`), recursing as SPLIT unconditionally -- a real
    /// non-SPLIT value there silently desynced the whole tile instead of
    /// refusing. Hand-writes a single whole superblock's `part64 = HORZ`
    /// symbol (never produced by this crate's own encoder, which only ever
    /// writes NONE/SPLIT at this level, but a real aomenc-class encoder can)
    /// and asserts this decoder refuses by name rather than decoding garbage.
    #[test]
    fn a_non_split_inter_sb_partition_refuses_by_name_instead_of_desyncing() {
        use crate::msac::SymbolEncoder;
        let mut cdfs = Cdfs::new(q_ctx_of(40));
        let mut enc = SymbolEncoder::new();
        enc.symbol(PARTITION_HORZ, &mut cdfs.partition_w64[0]);
        let data = enc.finish();
        let reference = crate::encode::Picture::grey(64, 64);
        let err = decode_inter_frame_tile(
            &data,
            16,
            16,
            40,
            64,
            64,
            &reference,
            [None; 8],
            &CdefParams::default(),
            &LoopFilterParams::default(),
            false,
            false,
            Some(mc::InterpFilterKind::Regular),
            false,
            false,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("SPLIT"),
            "expected the named inter-SB-partition refusal, got: {msg}"
        );
    }

    // ffmpeg cross-oracle: an independent decoder (not this crate's own
    // encoder-side reconstruction) agrees with what this module decodes,
    // duplicating `crate::encode`'s own `#[cfg(test)]` helpers of the same
    // name (that module is another lane's territory this round, and the
    // helpers are `#[cfg(test)]`-private to it).
    use std::io::Write;
    use std::process::{Command, Stdio};

    /// Whether ffmpeg is on PATH. Absence normally SKIPs, but
    /// `EC_AV1_REQUIRE_FFMPEG=1` -- or `EC_AV1_REQUIRE_AOMENC=1`, since every
    /// aomenc gate decodes its stream through ffmpeg and is meaningless
    /// without it -- turns it into a hard failure. Without this the require
    /// flag was silently short-circuited: `!have_ffmpeg()` is evaluated first
    /// in `if !have_ffmpeg() || !have_aomenc()`, so a machine with no ffmpeg
    /// printed SKIP and reported green (class gate-skips-on-its-own-failure).
    fn have_ffmpeg() -> bool {
        let present = Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(
            present
                || (std::env::var_os("EC_AV1_REQUIRE_FFMPEG").is_none()
                    && std::env::var_os("EC_AV1_REQUIRE_AOMENC").is_none()),
            "EC_AV1_REQUIRE_FFMPEG/EC_AV1_REQUIRE_AOMENC is set but no working ffmpeg on PATH"
        );
        present
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
            y: out.stdout[..luma].iter().map(|&v| u16::from(v)).collect(),
            u: out.stdout[luma..luma + chroma].iter().map(|&v| u16::from(v)).collect(),
            v: out.stdout[luma + chroma..].iter().map(|&v| u16::from(v)).collect(),
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
                    y: out.stdout[base..base + luma].iter().map(|&v| u16::from(v)).collect(),
                    u: out.stdout[base + luma..base + luma + chroma].iter().map(|&v| u16::from(v)).collect(),
                    v: out.stdout[base + luma + chroma..base + frame_bytes].iter().map(|&v| u16::from(v)).collect(),
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
                false,
                false,
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
                picture.y[row * width + col] = ((row * 7 + col * 11) % 251) as u16;
            }
        }
        for row in 0..height / 2 {
            for col in 0..width / 2 {
                let i = row * width / 2 + col;
                picture.u[i] = (100 + (col * 60 / (width / 2).max(1))) as u16;
                picture.v[i] = (200 - (row * 80 / (height / 2).max(1))) as u16;
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
                picture.y[y * width + x] = (20.0 + gradient).clamp(0.0, 255.0) as u16;
            }
        }
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                let sx = (x as i64 - shift / 2).rem_euclid((width / 2) as i64) as usize;
                let i = y * width / 2 + x;
                picture.u[i] = (100 + (sx * 60 / (width / 2))) as u16;
                picture.v[i] = (200 - (y * 80 / (height / 2))) as u16;
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
            false,
            false,
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
                [None; 8],
                &CdefParams::default(),
                &LoopFilterParams::default(),
                false,
                false,
                Some(mc::InterpFilterKind::Regular),
                false,
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
            ref_frame1: None,
            mv1: None,
            mv,
            is_new_mv: false,
            size: 1,
            size_h: 1,
            is_global_mv0: false,
            is_global_mv1: false,
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
        let ref_pattern: Vec<u16> = (0..width * height).map(|i| (i % 251) as u16).collect();
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
            tile_x0: 0,
            tile_y0: 0,
            tile_x1: width / scale,
            tile_y1: height / scale,
        };
        let ref_y = PlaneBuf {
            data: ref_pattern.clone(),
            width,
            height,
            true_width: width,
            true_height: height,
            tile_x0: 0,
            tile_y0: 0,
            tile_x1: width,
            tile_y1: height,
        };
        let ref_u = ref_plane(2);
        let ref_v = ref_plane(2);

        let mut y = PlaneBuf {
            data: vec![0u16; width * height],
            width,
            height,
            true_width: width,
            true_height: height,
            tile_x0: 0,
            tile_y0: 0,
            tile_x1: width,
            tile_y1: height,
        };
        let mut u = PlaneBuf {
            data: vec![0u16; (width / 2) * (height / 2)],
            width: width / 2,
            height: height / 2,
            true_width: width / 2,
            true_height: height / 2,
            tile_x0: 0,
            tile_y0: 0,
            tile_x1: width / 2,
            tile_y1: height / 2,
        };
        let mut v = PlaneBuf {
            data: vec![0u16; (width / 2) * (height / 2)],
            width: width / 2,
            height: height / 2,
            true_width: width / 2,
            true_height: height / 2,
            tile_x0: 0,
            tile_y0: 0,
            tile_x1: width / 2,
            tile_y1: height / 2,
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
            &[None; 8],
            &NO_SIGN_BIAS,
            &[ec_av1_syntax::WarpParams::default(); 7],
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
            false,
            Some(mc::InterpFilterKind::Regular),
            false,
            None,
            false,
            false,
            false,
            false,
            0,
            0,
            [0; 7],
            false,
            [0; 2],
            false,
            false,
            false,
            side,
            side,
            false,
            width,
        )
        .unwrap();

        let (px, py) = (c * SUB, r * SUB);
        let mut want_near = vec![0u16; side * side];
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
        let mut want_nearest = vec![0u16; side * side];
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

        let mut got = vec![0u16; side * side];
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
            false,
            false,
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
                [None; 8],
                &CdefParams::default(),
                &LoopFilterParams::default(),
                false,
                false,
                Some(mc::InterpFilterKind::Regular),
                false,
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

#[cfg(test)]
mod tx4x8_table_tests {
    use super::*;

    /// Asymmetric gate on the two new scan tables (class
    /// `reference-layout-not-spec`): a transposed pair of tables passes any
    /// "is a permutation" check, so this pins the pair AGAINST each other --
    /// `SCAN_8X4[k]` must be `SCAN_4X8[k]` reflected across the diagonal, and
    /// both must be bijections of `0..32` in our own row-major indexing.
    #[test]
    fn the_rect_4x8_scans_are_each_other_transposed() {
        let mut seen_a = [false; 32];
        let mut seen_b = [false; 32];
        for k in 0..32usize {
            let a = SCAN_4X8[k] as usize;
            let b = SCAN_8X4[k] as usize;
            assert!(!seen_a[a] && !seen_b[b], "scan position {k} repeats");
            seen_a[a] = true;
            seen_b[b] = true;
            let (row, col) = (a / 4, a % 4);
            assert_eq!(b, col * 8 + row, "SCAN_8X4[{k}] is not SCAN_4X8[{k}] transposed");
        }
        assert!(seen_a.iter().all(|&s| s) && seen_b.iter().all(|&s| s));
    }

    /// `mrow_scan_4x8`/`mcol_scan_4x8`/`mrow_scan_8x4`/`mcol_scan_8x4`
    /// (libaom `scan.c`), converted out of their column-major encoding by
    /// `(p % h) * w + p / h` -- the head of each, pinned so the rect
    /// `V_DCT`/`H_DCT` walk cannot silently transpose.
    #[test]
    fn the_rect_class1_scans_match_libaom() {
        assert_eq!(
            class_scan_table_wh(4, 8, TxClass::Vert),
            (0..32u16).collect::<Vec<_>>()
        );
        assert_eq!(
            &class_scan_table_wh(4, 8, TxClass::Horiz)[..8],
            &[0u16, 4, 8, 12, 16, 20, 24, 28]
        );
        assert_eq!(
            class_scan_table_wh(8, 4, TxClass::Vert),
            (0..32u16).collect::<Vec<_>>()
        );
        assert_eq!(
            &class_scan_table_wh(8, 4, TxClass::Horiz)[..8],
            &[0u16, 8, 16, 24, 1, 9, 17, 25]
        );
    }

    /// The 32-position `eob_pt` alphabet: six symbols (one more than the
    /// 16-position table, one fewer than the 64-position one), strictly
    /// increasing, terminated at 32768 with the adaptation counter after it.
    #[test]
    fn the_32_position_eob_tables_are_well_formed() {
        for table in [
            &cdf::EOB_PT_32_LUMA_Q0,
            &cdf::EOB_PT_32_LUMA_Q1,
            &cdf::EOB_PT_32_LUMA,
            &cdf::EOB_PT_32_LUMA_Q3,
            &cdf::EOB_PT_32_LUMA_CLASS1_Q0,
            &cdf::EOB_PT_32_LUMA_CLASS1_Q1,
            &cdf::EOB_PT_32_LUMA_CLASS1,
            &cdf::EOB_PT_32_LUMA_CLASS1_Q3,
        ] {
            assert_eq!(table.len(), 7);
            assert_eq!(table[5], 32768);
            assert_eq!(table[6], 0);
            for w in table[..6].windows(2) {
                assert!(w[0] < w[1], "{table:?} is not increasing");
            }
        }
    }

    /// `Reach::of_rect` must read libaom's own `has_tr_4x8`/`has_tr_8x4`
    /// rows, not the 32x16 pair the two existing rect strips use: the two
    /// shapes disagree at a position inside the superblock, which is what
    /// makes this an asymmetric check rather than a tautology.
    #[test]
    fn the_sub8_rect_reach_tables_are_shape_specific() {
        let a = Reach::of_rect(4, 8, 4, 8, 64, 64);
        let b = Reach::of_rect(8, 4, 8, 4, 64, 64);
        assert!(
            a.above_right != b.above_right || a.below_left != b.below_left,
            "4x8 and 8x4 reach identically at mirrored positions -- one table is unused"
        );
    }
}
