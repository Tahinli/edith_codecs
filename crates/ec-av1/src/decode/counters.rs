//! Hit and census counters of the decode path: thread-local tallies bumped by the
//! `hit!` instrumentation and read back by the gate tests. Moved out of `decode/mod.rs`
//! by name (av1split D3); the items are unchanged except for `pub(super)` visibility.

use super::*;

// How many real 128-level partition symbols this decoder has read, and how
// many superblocks resolved to `PARTITION_SPLIT` (coded or forced) -- the
// gate's proof a `--sb-size=128` stream exercised the 128 partition root.
thread_local! {
    pub(super) static PART128_SPLIT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`PART128_SPLIT_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn part128_split_hits() -> usize {
    PART128_SPLIT_HITS.with(|c| c.get())
}

// How many `use_filter_intra` symbols this decoder has read as `1`, across
// every call on the current thread -- the cheap counter [`filter_intra_hits`] gate
// tests read (before/after, not the absolute value) to prove a stream
// actually exercised the filter-intra predictor rather than silently
// skipping it.
thread_local! {
    pub(super) static FILTER_INTRA_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`FILTER_INTRA_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn filter_intra_hits() -> usize {
    FILTER_INTRA_HITS.with(|c| c.get())
}

// lane-fiinter r1: how many of those `use_filter_intra == 1` symbols belonged
// to an INTRA block inside an INTER frame (`read_intra_block_mode_info`'s own
// `read_filter_intra_mode_info` call), which no gate exercised before this
// round -- every inter gate recipe passed `--enable-filter-intra=0`.
thread_local! {
    pub(super) static INTRA_IN_INTER_FILTER_INTRA_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`INTRA_IN_INTER_FILTER_INTRA_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn intra_in_inter_filter_intra_hits() -> usize {
    INTRA_IN_INTER_FILTER_INTRA_HITS.with(|c| c.get())
}

// lane-angleinter r1: how many INTRA blocks inside an INTER frame carried a
// nonzero `angle_delta_y` / `angle_delta_uv` (`read_intra_angle_info`). Both
// arms refused a nonzero luma delta before this round, and no inter gate
// enabled `--enable-angle-delta` (aomenc's own default is 1).
thread_local! {
    pub(super) static INTRA_IN_INTER_ANGLE_DELTA_Y_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    pub(super) static INTRA_IN_INTER_ANGLE_DELTA_UV_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`INTRA_IN_INTER_ANGLE_DELTA_Y_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn intra_in_inter_angle_delta_y_hits() -> usize {
    INTRA_IN_INTER_ANGLE_DELTA_Y_HITS.with(|c| c.get())
}

/// Current value of [`INTRA_IN_INTER_ANGLE_DELTA_UV_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn intra_in_inter_angle_delta_uv_hits() -> usize {
    INTRA_IN_INTER_ANGLE_DELTA_UV_HITS.with(|c| c.get())
}

// lane-uv8 r1: the three non-DC chroma mode classes an INTRA 8x8 leaf inside
// an INTER frame ([`decode_inter_block8`]'s intra arm) can carry -- the arm
// read `uv_mode` and then refused everything but `DC_PRED`. Counted here and
// nowhere else so the gate's assert cannot be satisfied by a >=16x16 block.
thread_local! {
    pub(super) static INTRA_IN_INTER8_UV_DIR_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static INTRA_IN_INTER8_UV_SMOOTH_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static INTRA_IN_INTER8_UV_CFL_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    pub(super) static INTRA_IN_INTER8_ANGLE_DELTA_Y_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Nonzero luma angle deltas coded by an intra 8x8 leaf inside an inter frame
/// (lane-leaf8tx r1) -- counted only in [`decode_inter_block8`]'s intra arm.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn intra_in_inter8_angle_delta_y_hits() -> usize {
    INTRA_IN_INTER8_ANGLE_DELTA_Y_HITS.with(|c| c.get())
}

/// `(directional, smooth/paeth, CfL)` chroma modes coded by an intra 8x8 leaf
/// inside an inter frame -- see [`INTRA_IN_INTER8_UV_DIR_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn intra_in_inter8_uv_hits() -> (usize, usize, usize) {
    (
        INTRA_IN_INTER8_UV_DIR_HITS.with(|c| c.get()),
        INTRA_IN_INTER8_UV_SMOOTH_HITS.with(|c| c.get()),
        INTRA_IN_INTER8_UV_CFL_HITS.with(|c| c.get()),
    )
}

// Whether this superblock's single CDEF unit (this decoder has no 128x128
// SB, so the SB IS the CDEF unit) has already had its `cdef_idx` literal
// read -- reset to `false` at the top of each superblock in the two tile
// loops, spec `read_cdef`'s `cdef_transmitted[index]`.
// Number of superblock columns in the current tile's grid, so
// [`maybe_read_cdef_idx`] can turn an `(mi_r, mi_c)` into a flat index into
// [`CDEF_IDX_GRID`].
// One resolved `cdef_idx` per superblock, read by [`apply_cdef`] to select
// which of the header's `1 << cdef.bits` strength pairs applies to that
// superblock's samples.
// How many real (`cdef.bits > 0`) `cdef_idx` literals [`maybe_read_cdef_idx`]
// has read, across every call on the current thread -- the gate's proof that a
// `cdef_bits > 0` stream actually reached the new reader rather than every
// superblock coincidentally being all-skip.
thread_local! {
    pub(super) static CDEF_IDX_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// How many times a warp was suppressed for one PLANE because that plane's own
// block is narrower or shorter than 8 (`av1_init_warp_params`'s own bail-out,
// reconinter.c) -- the gate's proof that a stream really carries the shape,
// since the square `side`/`chroma_side` can be 8 or more while the plane's
// true block is 4 wide (an 8x16 luma block's 4x8 chroma).
thread_local! {
    pub(super) static WARP_PLANE_SUPPRESS_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`WARP_PLANE_SUPPRESS_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn warp_plane_suppress_hits() -> usize {
    WARP_PLANE_SUPPRESS_HITS.with(|c| c.get())
}

/// Current value of [`CDEF_IDX_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn cdef_idx_hits() -> usize {
    CDEF_IDX_HITS.with(|c| c.get())
}

// lane-t900 r6: gate proof for the three r2-r4 root causes that shipped with no
// counter -- the in-inter tx-depth context (`tx_size_context_txfm_rect`), the
// non-chroma-reference block that must LEAVE its neighbours' chroma coefficient
// contexts alone, and the intra-edge filter neighbour pair taken mi-exact
// instead of from the coarse 16x16 cell.
thread_local! {
    pub(super) static INTRA_IN_INTER_TXCTX_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static NON_CHROMA_REF_CTX_SKIP_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static EDGE_FILTER_MI_FIX_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`INTRA_IN_INTER_TXCTX_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn intra_in_inter_txctx_hits() -> usize {
    INTRA_IN_INTER_TXCTX_HITS.with(|c| c.get())
}

/// Current value of [`NON_CHROMA_REF_CTX_SKIP_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn non_chroma_ref_ctx_skip_hits() -> usize {
    NON_CHROMA_REF_CTX_SKIP_HITS.with(|c| c.get())
}

/// Current value of [`EDGE_FILTER_MI_FIX_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn edge_filter_mi_fix_hits() -> usize {
    EDGE_FILTER_MI_FIX_HITS.with(|c| c.get())
}

// lane-realworld r4: this frame's `delta.q_present` (spec 5.9.17) and its
// actual step (`1 << delta.q_res`, already the real multiplier, not the raw
// 2-bit field). `false`/`1` (the defaults) make [`maybe_read_delta_q`] a
// true no-op, matching every existing gate stream's `delta_q_present == 0`
// behaviour exactly, the same corner-cut [`CDEF_BITS`] takes above.
// The running quantizer index (spec `CurrentQIndex`, libaom
// `xd->current_base_qindex`) -- reset to the frame's own `base_q_idx` at the
// top of every tile (spec `decode_tile`), carried across superblocks within
// that tile, and mutated only by [`maybe_read_delta_q`]. Read back at the
// dequantization call sites instead of the frame-constant `base_q_idx`.
// This frame's own per-plane DC/AC quantizer-index deltas (lane-sbpart r11,
// spec 5.9.12/7.12.2, [`crate::quant::QuantDeltas`]) -- frame-constant (not
// per-superblock like [`CURRENT_Q_IDX`]), set once per frame decode and read
// back at every dequantization call site keyed on which plane it is doing.
// How many real `delta_q_present` symbol groups [`maybe_read_delta_q`] has
// read, across every call on the current thread -- the gate's proof that a
// `delta_q_present` stream actually reached the new reader.
thread_local! {
    pub(super) static DELTA_Q_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`DELTA_Q_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn delta_q_hits() -> usize {
    DELTA_Q_HITS.with(|c| c.get())
}

// lane-realworld r5: this frame's `delta.lf_present`/`lf_res`/`lf_multi`
// (spec 5.9.18) -- same no-op-by-default corner-cut as `DELTA_Q_PRESENT`.
// The running per-plane/direction loop-filter delta (spec `DeltaLF[i]`,
// `i` in `0..FRAME_LF_COUNT`) -- reset to `[0; 4]` at the top of every tile,
// mutated only by [`maybe_read_delta_lf`]. `!delta_lf_multi` mode only ever
// updates index 0 and every reader treats that as the value for all 4
// indices (spec `read_delta_lf_params`'s own single-cdf branch).
// How many real `delta_lf_present` symbol groups [`maybe_read_delta_lf`] has
// read -- the gate's proof the reader fired, mirroring [`DELTA_Q_HITS`].
thread_local! {
    pub(super) static DELTA_LF_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`DELTA_LF_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn delta_lf_hits() -> usize {
    DELTA_LF_HITS.with(|c| c.get())
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
    /// `CurrentSegmentIds`: this frame's own map, `mi_rows * mi_cols` entries
    /// with stride `mi_cols` (NOT the padded `Neighbours` grid stride).
    /// `PrevSegmentIds`: the primary reference frame's saved map, empty when
    /// this frame has no primary reference (spec `load_previous_segment_ids`).
    /// `(mi_rows, mi_cols)` for both maps above.
    /// `AboveSegPredContext` (per mi column) / `LeftSegPredContext` (per mi
    /// row) -- spec 5.11.9's own `seg_id_predicted` neighbour state, which
    /// libaom keeps on the neighbouring `mbmi->seg_id_predicted` instead.
    /// The block currently being decoded -- read back by [`block_q_idx`].
    /// The tile's own top-left mi position, for `AvailU`/`AvailL`.
    /// How many real `segment_id` symbols were read, and the set of segment
    /// ids any block ended up with -- the gate's proof the feature fired.
    pub(super) static SEG_ID_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// How many `seg_id_predicted` symbols were read (temporal update).
    pub(super) static SEG_PRED_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many `segment_id` symbols have been read (never reset per frame).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn segment_id_hits() -> usize {
    SEG_ID_HITS.with(|c| c.get())
}

/// How many `seg_id_predicted` symbols have been read (temporal update).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn segment_pred_hits() -> usize {
    SEG_PRED_HITS.with(|c| c.get())
}

/// Resets the segmentation hit counters (gate setup).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_segment_hits() {
    SEG_ID_HITS.with(|c| c.set(0));
    SEG_IDS_SEEN.with(|c| c.set(0));
    SEG_PRED_HITS.with(|c| c.set(0));
    BLOCK_Q_SPAN.with(|c| c.set((i32::MAX, i32::MIN)));
}

// How many key-frame luma blocks resolved `smooth_neighbor` (spec
// `get_intra_edge_filter_type`) to `true` -- lane-chroma r2's own before/after
// counter, proving a stream actually put a directional block next to a
// smooth-predicted neighbour rather than every block reading the (previously
// hardcoded `false`) edge-filter strength bucket unchanged.
thread_local! {
    pub(super) static SMOOTH_LUMA_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`SMOOTH_LUMA_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn smooth_luma_hits() -> usize {
    SMOOTH_LUMA_HITS.with(|c| c.get())
}

thread_local! {
    /// lane-band46 r1: how many split-transform units answered their intra
    /// reach DIFFERENTLY under libaom's per-unit rules ([`Reach::of_tu`], the
    /// palette2 r12 fix) than the standalone-block lookup this decoder used to
    /// apply to them (class reach-is-per-transform-unit). Every hit is a
    /// transform unit whose above-right / below-left reference samples would
    /// be wrong without that fix.
    pub(super) static SPLIT_TU_REACH_FIX_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`SPLIT_TU_REACH_FIX_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn split_tu_reach_fix_hits() -> usize {
    SPLIT_TU_REACH_FIX_HITS.with(|c| c.get())
}

// How many `uv_mode` reads resolved to `SMOOTH_PRED..=PAETH_PRED` (9..=12),
// across every call on the current thread -- lane-chroma r3's own before/after
// counter, proving a stream actually exercised chroma's smooth/paeth
// predictor (the previously-refused round-2 gap) rather than every block
// landing on DC/CFL/directional.
thread_local! {
    pub(super) static SMOOTH_UV_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`SMOOTH_UV_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn smooth_uv_hits() -> usize {
    SMOOTH_UV_HITS.with(|c| c.get())
}

// How many tiles [`decode_key_frame_tile_with_cdfs`] has actually decoded a
// full superblock walk over, across every call on the current thread --
// lane-tiles r2's own gate-blind-to-feature guard: a gate over a
// multi-tile stream must prove more than one tile actually fired, not just
// that the stream carried tile_info.cols > 1.
thread_local! {
    pub(super) static TILE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`TILE_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn tile_hits() -> usize {
    TILE_HITS.with(|c| c.get())
}

// How many 4-pixel deblocking edge groups [`edge_params`] has actually
// selected for filtering, across every call on the current thread -- the same
// before/after counter pattern as [`FILTER_INTRA_HITS`], proving a stream
// exercised `apply_deblock` rather than every edge landing on level 0.
thread_local! {
    pub(super) static DEBLOCK_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`DEBLOCK_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn deblock_hits() -> usize {
    DEBLOCK_HITS.with(|c| c.get())
}

// lane-defon r1: how many COMPOUND_REFERENCE blocks read a
// `UNIDIR_COMP_REFERENCE` pair (both references on the same temporal side,
// spec 5.11.25 `comp_reference_type == 0`) -- the gate's proof that a
// stream built with `--enable-onesided-comp=1` really exercised the
// `uni_comp_ref` alphabet rather than only bidirectional pairs.
thread_local! {
    pub(super) static UNI_COMP_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`UNI_COMP_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn uni_comp_hits() -> usize {
    UNI_COMP_HITS.with(|c| c.get())
}

// lane-defon r1: how many blocks blended through a `COMPOUND_DIFFWTD` mask
// (`mc::diffwtd_mask`) -- the narrower half of `MASKED_COMPOUND_HITS`, the
// gate's proof that `--enable-diff-wtd-comp=1` reached the difference-weighted
// blend and not only the wedge arm.
thread_local! {
    pub(super) static DIFFWTD_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`DIFFWTD_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn diffwtd_hits() -> usize {
    DIFFWTD_HITS.with(|c| c.get())
}

// lane-mcomp64 r1: how many blocks took the INFERRED `COMPOUND_DIFFWTD` arm --
// a block size where `av1_is_wedge_used` is false (64x64 is the only such size
// this decoder's square inter path reaches), so no `compound_type` symbol is
// coded at all. Strictly narrower than `DIFFWTD_HITS`, which also counts the
// coded-symbol DIFFWTD blocks at 8x8/16x16/32x32.
thread_local! {
    pub(super) static DIFFWTD_INFERRED_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`DIFFWTD_INFERRED_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn diffwtd_inferred_hits() -> usize {
    DIFFWTD_INFERRED_HITS.with(|c| c.get())
}

// lane-defon r1: how many FORWARD KEYFRAMES were decoded -- a KEY frame coded
// with `show_frame == 0`, output later by a `show_existing_frame` header
// (aomenc `--enable-fwd-kf=1 --fwd-kf-dist=N`). Counted from
// `stream::decode_stream`, which is where the frame header is consumed; the
// gate's proof that the on-value really reached the bitstream rather than
// being dropped by the rate control.
thread_local! {
    pub(super) static FWD_KF_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`FWD_KF_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn fwd_kf_hits() -> usize {
    FWD_KF_HITS.with(|c| c.get())
}

// How many [`read_tx_size`] reads resolved a `tx_depth` strictly less than
// the block's own side, across every call on the current thread -- the same
// before/after counter pattern as [`FILTER_INTRA_HITS`], proving a stream
// actually exercised a *split* transform under `TxMode::Select` rather than
// every block coincidentally resolving `depth=0` (indistinguishable from
// `TxMode::Largest` pixel-wise).
thread_local! {
    pub(super) static TX_DEPTH_HITS: std::cell::Cell<usize> =
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
    pub(super) static SQ_CHROMA_TX_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`SQ_CHROMA_TX_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn sq_chroma_tx_hits() -> usize {
    SQ_CHROMA_TX_HITS.with(|c| c.get())
}

/// Current value of [`TX_DEPTH_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn tx_depth_hits() -> usize {
    TX_DEPTH_HITS.with(|c| c.get())
}

// lane-txselect: how many `txfm_split` symbols an inter frame's var-tx tree
// read, and how many of those actually took the split, on the current thread
// -- the same before/after counter pattern as [`TX_DEPTH_HITS`], proving a
// stream exercised spec 5.11.17's `read_var_tx_size` rather than every block
// coincidentally coding one whole-block transform.
thread_local! {
    pub(super) static TXFM_SPLIT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // lane-inter4 r2: rectangular INTER residual coding -- how many rect
    // strips coded one whole-block rect transform unit, and how many split
    // their rect max transform into square sub-transforms.
    pub(super) static RECT_INTER_TU_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static RECT_INTER_TXSPLIT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // lane-r14 r2: luma transform units of an inter strip with a 64-px axis
    // (64x32/32x64/64x16/16x64), whose coefficients are coded as the
    // truncated low-32 corner.
    pub(super) static RECT64_INTER_TU_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // lane-rectres r1: whole-block luma transform units of a 32x32-level 1:4
    // inter strip, `[32x8, 8x32]`.
    pub(super) static RECT32X8_INTER_TU_HITS: std::cell::Cell<[usize; 2]> = const { std::cell::Cell::new([0; 2]) };
    // lane-r14 r3: rectangular var-tx LEAVES, [32x16, 16x32].
    pub(super) static VARTX_RECT_LEAF_HITS: std::cell::Cell<[usize; 2]> = const { std::cell::Cell::new([0; 2]) };
    // lane-inter16ab r4: rectangular var-tx leaves with a 4-px axis, [8x4, 4x8]
    // -- the `sub_tx_size_map[TX_16X4]` split leaves of a 16x16-level 1:4 strip.
    pub(super) static VARTX_RECT_LEAF4_HITS: std::cell::Cell<[usize; 2]> = const { std::cell::Cell::new([0; 2]) };
    // lane-inter4 r3: 16x16-level PARTITION_HORZ / PARTITION_VERT on an inter
    // frame -- 16x8 and 8x16 inter leaves.
    pub(super) static INTER_LEAF16_HORZ_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static INTER_LEAF16_VERT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // This frame's own `tx_mode == TxMode::Select` and `reduced_tx_set` bits,
    // set once per inter tile by [`decode_inter_frame_tile_with_cdfs`] --
    // `decode_inter_block` already carries forty parameters and both are
    // frame-constant.
    // lane-fiinter r1: this sequence header's own `enable_filter_intra` bit,
    // frame-constant like the two above and stashed the same way -- an intra
    // block inside an inter frame reads `use_filter_intra` under it.
}

/// Current value of [`TXFM_SPLIT_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn txfm_split_hits() -> usize {
    TXFM_SPLIT_HITS.with(|c| c.get())
}

/// Current value of [`INTER_LEAF16_HORZ_HITS`] (lane-inter4 r3).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn inter_leaf16_horz_hits() -> usize {
    INTER_LEAF16_HORZ_HITS.with(|c| c.get())
}

/// Current value of [`INTER_LEAF16_VERT_HITS`] (lane-inter4 r3).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn inter_leaf16_vert_hits() -> usize {
    INTER_LEAF16_VERT_HITS.with(|c| c.get())
}

/// Current value of [`RECT_INTER_TU_HITS`] (lane-inter4 r2).
pub(crate) fn rect_inter_tu_hits() -> usize {
    RECT_INTER_TU_HITS.with(|c| c.get())
}

/// Current value of [`RECT_INTER_TXSPLIT_HITS`] (lane-inter4 r2).
pub(crate) fn rect_inter_txsplit_hits() -> usize {
    RECT_INTER_TXSPLIT_HITS.with(|c| c.get())
}

/// Current value of [`RECT64_INTER_TU_HITS`] (lane-r14 r2).
pub(crate) fn rect64_inter_tu_hits() -> usize {
    RECT64_INTER_TU_HITS.with(|c| c.get())
}

/// Current value of [`RECT32X8_INTER_TU_HITS`] (lane-rectres r1): `[32x8, 8x32]`
/// whole-block luma units of a 32x32-level 1:4 inter strip.
pub(crate) fn rect32x8_inter_tu_hits() -> [usize; 2] {
    RECT32X8_INTER_TU_HITS.with(|c| c.get())
}

/// Current value of [`VARTX_RECT_LEAF_HITS`] (lane-r14 r3): how many
/// rectangular var-tx leaf transform units were coded, `[32x16, 16x32]`.
pub(crate) fn vartx_rect_leaf_hits() -> [usize; 2] {
    VARTX_RECT_LEAF_HITS.with(|c| c.get())
}

/// Current value of [`VARTX_RECT_LEAF4_HITS`] (lane-inter16ab r4): 8x4/4x8
/// split leaves of a 16x4/4x16 strip, `[8x4, 4x8]`.
pub(crate) fn vartx_rect_leaf4_hits() -> [usize; 2] {
    VARTX_RECT_LEAF4_HITS.with(|c| c.get())
}

/// Records one rectangular var-tx leaf.
pub(super) fn vartx_rect_leaf_hit(tw: usize, th: usize) {
    if crate::envflags::env_flag!("EC_VARTXLEAF") {
        eprintln!("EC_VARTXLEAF tw={tw} th={th}");
    }
    let cell = if tw.min(th) == 4 {
        &VARTX_RECT_LEAF4_HITS
    } else {
        &VARTX_RECT_LEAF_HITS
    };
    cell.with(|c| {
        let mut h = c.get();
        h[usize::from(th > tw)] += 1;
        c.set(h);
    });
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
    pub(super) static VERT_B_INTRA_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`VERT_B_INTRA_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn vert_b_intra_hits() -> usize {
    VERT_B_INTRA_HITS.with(|c| c.get())
}

// lane-ab16 r1: how many 16x16-level AB partitions fired, per arm, in
// `PARTITION_HORZ_A`, `_HORZ_B`, `_VERT_A`, `_VERT_B` order. Each arm is two
// 8x8 squares (`decode_leaf8`) plus one 16x8/8x16 strip (`decode_leaf_rect`,
// the non-skip coefficient path lane-rectx proved); the gate hard-asserts
// every arm separately so a run where aomenc's RD only picked one cannot
// pass as proof of four.
thread_local! {
    pub(super) static AB16_HITS: std::cell::Cell<[usize; 4]> = const { std::cell::Cell::new([0; 4]) };
}

// [`AB16_HITS`] per arm (HORZ_A, HORZ_B, VERT_A, VERT_B at 16x16).
thread_local! {
    /// 4x4 transform units decoded inside an 8x8 square of a
    /// `PARTITION_VERT_A`/`_B` -- the exact call that panicked before
    /// [`crate::encode::Reach::of_tu`] (lane-ab16 r2), so the gate can prove
    /// a real `--enable-tx-size-search=1` stream reaches it.
    pub(super) static VERT_AB_TX4_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn vert_ab_tx4_hits() -> usize {
    VERT_AB_TX4_HITS.with(std::cell::Cell::get)
}

#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn ab16_hits_by_arm() -> [usize; 4] {
    AB16_HITS.with(std::cell::Cell::get)
}

// How many `partition_w16` reads resolved to a plain `PARTITION_HORZ`/
// `PARTITION_VERT` (lane-rect16 r2): re-measuring after the VERT_B fix above
// showed mandelbrot's real first-hit blocker is this plain arm at mi=(0,7),
// not VERT_B (`refusal-names-a-correlate`; the VERT_B write-up's "frame 0's
// very first non-NONE/SPLIT symbol" claim was about decode ORDER within one
// superblock's z-order recursion, not raster mi position).
thread_local! {
    pub(super) static HORZ_VERT_INTRA_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`HORZ_VERT_INTRA_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn horz_vert_intra_hits() -> usize {
    HORZ_VERT_INTRA_HITS.with(|c| c.get())
}

// How many blocks were decoded as a frame-edge rect strip -- the single
// coded half of a `PARTITION_HORZ`/`PARTITION_VERT` at the true frame edge
// (lane-oddh r1), i.e. a block living inside the 8px band of a frame whose
// height or width is 8 mod 16 (the user's 3840x1608 film). Intra and inter
// paths share the counter.
thread_local! {
    pub(super) static EDGE_RECT_STRIP_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`EDGE_RECT_STRIP_HITS`].
#[allow(dead_code)] // gate counter accessor with no reader yet; the counter itself is live
pub(crate) fn edge_rect_strip_hits() -> usize {
    EDGE_RECT_STRIP_HITS.with(|c| c.get())
}

// lane-interedge r1: frame-edge half strips on the INTER path, per partition
// level (64/32/16) x axis (HORZ at a bottom edge, VERT at a right edge). The
// index is `level_idx * 2 + axis` with `level_idx` 0=64, 1=32, 2=16 and
// `axis` 0=HORZ (`!has_rows`), 1=VERT (`!has_cols`). Both of the user's films
// are height mod 16 == 8, so every inter frame crosses these nodes.
thread_local! {
    pub(super) static INTER_EDGE_STRIP_HITS: std::cell::Cell<[usize; 6]> =
        const { std::cell::Cell::new([0; 6]) };
}

// lane-edgeboth r1: how many partition nodes were coded as the BOTH-AXES-CUT
// forced split -- libaom `decode_partition` (decodeframe.c):
// `else if (!has_rows && !has_cols) partition = PARTITION_SPLIT;`, the one
// straddle case that reads NO symbol at all. Indexed by level:
// 0 = 16x16, 1 = 32x32, 2 = 64x64, 3 = 128x128; intra and inter share it.
thread_local! {
    pub(super) static EDGE_BOTH_CUT_HITS: std::cell::Cell<[usize; 4]> =
        const { std::cell::Cell::new([0; 4]) };
}

/// Current value of [`INTER_EDGE_STRIP_HITS`] (`[h64, v64, h32, v32, h16, v16]`).
pub fn inter_edge_strip_hits() -> [usize; 6] {
    INTER_EDGE_STRIP_HITS.with(|c| c.get())
}

/// Resets [`INTER_EDGE_STRIP_HITS`] -- a refused attempt must not carry hits
/// into the next one (class `counter-from-refused-stream`).
pub fn reset_inter_edge_strip_hits() {
    INTER_EDGE_STRIP_HITS.with(|c| c.set([0; 6]));
}

/// Current value of [`EDGE_BOTH_CUT_HITS`] (`[16, 32, 64, 128]` levels).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn edge_both_cut_hits() -> [usize; 4] {
    EDGE_BOTH_CUT_HITS.with(|c| c.get())
}

/// Zeroes [`EDGE_BOTH_CUT_HITS`] so a gate can measure a per-attempt delta.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_edge_both_cut_hits() {
    EDGE_BOTH_CUT_HITS.with(|c| c.set([0; 4]));
}

// How many HORZ/VERT intra strips resolved a SPLIT luma transform
// (`tx_depth != 0`) and decoded it per transform unit (lane-rectsplit r1) --
// the case every rect path here refused by name before, and the first
// refusal the user's ReelB extract (600s in) stops at.
thread_local! {
    pub(super) static RECT_SPLIT_TX_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT_SPLIT_TX_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
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
    pub(super) static RECT_SPLIT_SB_INTERIOR_TU_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT_SPLIT_SB_INTERIOR_TU_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn rect_split_sb_interior_tu_hits() -> usize {
    RECT_SPLIT_SB_INTERIOR_TU_HITS.with(|c| c.get())
}

// Per-shape hit counts for the rect chroma transforms lane-rectchroma r1
// wired into [`decode_rect_split`] -- the shapes a split-transform strip's
// chroma half takes that had no coefficient tables before (index order:
// 32x8, 8x32, 8x4, 4x8). One counter per shape because a gate that only
// proves the pair it happens to hit cannot catch a transposed scan/context
// table (class scan-weights-cross-axis).
thread_local! {
    pub(super) static RECT_SPLIT_CHROMA_SHAPE_HITS: std::cell::Cell<[usize; 4]> =
        const { std::cell::Cell::new([0; 4]) };
}

/// Current value of [`RECT_SPLIT_CHROMA_SHAPE_HITS`] (32x8, 8x32, 8x4, 4x8).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn rect_split_chroma_shape_hits() -> [usize; 4] {
    RECT_SPLIT_CHROMA_SHAPE_HITS.with(|c| c.get())
}

// lane-av1-422: how many SQUARE chroma coefficient units the strip path read
// on a 4:2:2 frame (ss 1,0) -- the half-width, unhalved-height shape whose
// tables only exist because of the square rows in the chroma_tiled arm. A
// 4:2:0 or 4:4:4 frame can never increment it (their rect strips' chroma is
// itself rect), and a restored `/2 chroma height` diverts 4:2:2 squares onto
// the rect rows, so the witness gate reads 0 there and fails.
thread_local! {
    pub(super) static CHROMA422_SQUARE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`CHROMA422_SQUARE_HITS`].
// lane-av1-422b: `pub` so `examples/decode_probe.rs` can census it the way
// it does every other capability counter.
pub fn chroma422_square_hits() -> usize {
    CHROMA422_SQUARE_HITS.with(|c| c.get())
}

// lane-av1422tailskip: how many 4:2:2 (ss 1,0) intra-in-inter chroma TRANSFORM
// UNITS took their own window on the block-sized chroma palette prediction --
// `decode_inter_block`'s `side == 64` TX_32X32 walk, where the plane block is
// 32x64 but codes two stacked square units per plane. Zero on a 4:2:0 or 4:4:4
// frame (their chroma unit is the whole plane block, one window), and zero on
// a 4:2:2 block with no palette, so a witness gate reading 0 proves the arm
// never ran.
thread_local! {
    pub(super) static PALETTE_422_UNIT_WINDOW_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`PALETTE_422_UNIT_WINDOW_HITS`].
// lane-av1422tailskip: `pub` so `examples/decode_probe.rs` can census it the
// way it does every other capability counter.
pub fn palette_422_unit_window_hits() -> usize {
    PALETTE_422_UNIT_WINDOW_HITS.with(|c| c.get())
}

// lane-av1-422f: how many RECT chroma coefficient units the square-block
// path read on a 4:2:2 frame (ss 1,0) -- TX_4X8 / TX_8X16 / TX_16X32, the
// plane block's own half-width, unhalved-height shape. A 4:2:0 or 4:4:4
// frame never takes the rect branch (their units are square
// `chroma_tx == chroma_tx_h`), so a witness gate reading 0 here proves the
// model reverted to squares.
thread_local! {
    pub(super) static CHROMA422_RECT_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`CHROMA422_RECT_HITS`].
pub fn chroma422_rect_hits() -> usize {
    CHROMA422_RECT_HITS.with(|c| c.get())
}

// lane-av1-422g: how many PER-LEAF TX_4X4 chroma coefficient units the
// sub-8x8 split4 path read on a 4:2:2 frame (ss 1,0) -- one per even-column
// leaf per plane (`is_chroma_reference`'s `!(mi_col & ss_x)`). A 4:2:0 frame
// codes no sub-8 chroma at all and 4:4:4 goes through `sub8_leaf_chroma444`,
// so a witness gate reading 0 here proves the per-leaf 4:2:2 arm never ran.
thread_local! {
    pub(super) static CHROMA422_SUB8_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`CHROMA422_SUB8_HITS`].
pub fn chroma422_sub8_hits() -> usize {
    CHROMA422_SUB8_HITS.with(|c| c.get())
}

// lane-av1422llintra: how many 4:2:2 LOSSLESS chroma plane blocks
// (`decode_leaf8`) were walked as 4x4 units rather than as the one
// `av1_get_max_uv_txsize` rect that rule only reaches OUTSIDE a lossless
// frame. A 4:2:0 or 4:4:4 frame never takes the arm (its own lossless walk
// was already there), so a witness gate reading 0 here proves the model
// reverted to the rect unit.
thread_local! {
    pub(super) static CHROMA422_LOSSLESS_4X4_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`CHROMA422_LOSSLESS_4X4_HITS`].
pub fn chroma422_lossless_4x4_hits() -> usize {
    CHROMA422_LOSSLESS_4X4_HITS.with(|c| c.get())
}

// lane-av1-422sub8rect: how many PER-PIECE TX_4X4 chroma coefficient units
// the INTER sub-8x8 split4 path read on a 4:2:2 frame (ss 1,0) -- one per
// non-skipped odd-column piece per plane (`is_chroma_reference`'s
// `mi_col & 0x01` at ss_x 1). 4:2:0 goes through the shared group unit and
// 4:4:4 through [`sub8_leaf_chroma444`]'s own per-piece twin, so a witness
// gate reading 0 here proves the inter 4:2:2 per-piece arm never ran.
thread_local! {
    pub(super) static CHROMA422_INTER_SUB8_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`CHROMA422_INTER_SUB8_HITS`].
pub fn chroma422_inter_sub8_hits() -> usize {
    CHROMA422_INTER_SUB8_HITS.with(|c| c.get())
}

// lane-av1-422m: how many TX_4X4 chunk units the invalid-ss-size chroma arm
// read (the 8x32 luma block whose 4:2:2 plane has no `ss_size_lookup` block).
// A 4:2:0 or 4:4:4 frame cannot increment it, so a non-zero read proves the
// chunked 4:2:2 chroma really decoded.
thread_local! {
    pub(super) static CHROMA422_CHUNK_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`CHROMA422_CHUNK_HITS`].
pub fn chroma422_chunk_hits() -> usize {
    CHROMA422_CHUNK_HITS.with(|c| c.get())
}

// lane-av1-422stripanchor: how many 4-wide-strip PAIR chroma units
// ([`decode_rect_split`]'s ported floored-pair anchor) decoded. A 4:2:0 or
// 4:4:4 frame cannot increment it (the arm is keyed to a 2-px-wide chroma
// block, which only a 1-mi-wide strip at ss (1, 0) produces), so a non-zero
// read proves a stream routed a 4-wide strip through the split path.
thread_local! {
    pub(super) static CHROMA422_PAIR_WIDE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`CHROMA422_PAIR_WIDE_HITS`].
pub fn chroma422_pair_wide_hits() -> usize {
    CHROMA422_PAIR_WIDE_HITS.with(|c| c.get())
}

// How many HORZ/VERT intra strips actually predicted with filter intra
// (lane-rectsplit r1) -- the case `decode_block_rect` refused by name
// ("this decoder predicts square-only") until `predict_filter_intra` took
// `bw`x`bh`.
thread_local! {
    pub(super) static FILTER_INTRA_RECT_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`FILTER_INTRA_RECT_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn filter_intra_rect_hits() -> usize {
    FILTER_INTRA_RECT_HITS.with(|c| c.get())
}

// As [`FILTER_INTRA_RECT_HITS`], counting only the 32x32-level 1:4 strips
// (32x8/8x32, lane-fi32x8 r1) -- the shapes whose `use_filter_intra` symbol
// was never read at all before that round, so a gate needs to prove the flag
// fired at THAT shape and not at a 16x8 one somewhere else in the frame.
// Split by ORIENTATION: rows 8 (32x8) and 9 (8x32) of `cdf::FILTER_INTRA`
// carry different probabilities (18101 vs 20229), so a transposed pair is
// only caught by a gate that sees the flag fire on BOTH shapes (class
// scan-weights-cross-axis).
thread_local! {
    pub(super) static FILTER_INTRA_RECT4_HORZ_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    pub(super) static FILTER_INTRA_RECT4_VERT_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`FILTER_INTRA_RECT4_HORZ_HITS`] (32x8 strips).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn filter_intra_rect4_horz_hits() -> usize {
    FILTER_INTRA_RECT4_HORZ_HITS.with(|c| c.get())
}

/// Current value of [`FILTER_INTRA_RECT4_VERT_HITS`] (8x32 strips).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn filter_intra_rect4_vert_hits() -> usize {
    FILTER_INTRA_RECT4_VERT_HITS.with(|c| c.get())
}

// How many rectangular transform units actually coded coefficients through
// [`read_coeffs_rect`] (`all_zero == 0`) -- lane-rect1d's measure that a gate
// exercised the rect coefficient reader at all rather than passing on
// all-skip blocks.
thread_local! {
    pub(super) static RECT_COEFF_TU_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static RECT_CLASS1_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT_COEFF_TU_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn rect_coeff_tu_hits() -> usize {
    RECT_COEFF_TU_HITS.with(|c| c.get())
}

/// Rectangular transform units whose resolved `tx_type` is a 1D class
/// (`V_DCT`/`H_DCT`) -- [`TX_CLASS1_HITS`]'s rect-only half, so a gate can
/// pin that the 1D-class-on-a-RECT-transform path fired rather than settling
/// for the square reader's own 1D types.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn rect_class1_hits() -> usize {
    RECT_CLASS1_HITS.with(|c| c.get())
}

// lane-av1readcensus: the PER-UNIT coefficient census -- one unit counted
// per `read_coeffs`/`read_coeffs_rect` call, i.e. per object this decoder
// actually walks, which is the unit the round-4 `av1444rect` count measured
// (636 square chroma reads against the oracle's 1086).
//
// lane-av1422census422 widened the arming. The counters used to be
// 4:4:4-ONLY (`CENSUS_444`), so at 4:2:2 -- a chroma format an AV1
// `color_config` can carry and libaom's writer emits -- every
// `census_444_units()` / `census_444_coded()` read zero BY CONSTRUCTION: a
// byte-exact 4:2:2 gate asserting those counters would be asserting a
// literal, and would be green while proving nothing about that stream. The
// census is now armed for every chroma format that leaves at least one luma
// axis unsubsampled -- (0,0) 4:4:4 and (1,0) 4:2:2 -- and 4:2:0 (1,1) stays
// off, so the existing 4:4:4 exact counts and the 4:2:0 zero control are
// unmoved by the widening. The counters are per-PLANE, not per-format, so a
// 4:2:2 cell's `[luma, u, v]` split is directly comparable with a 4:4:4
// cell's on the same walk shape.
thread_local! {
    /// Per-plane unit counts, `[luma, u, v]`, at 4:4:4 and 4:2:2.
    pub(super) static CENSUS_NONSUB_UNITS: std::cell::Cell<[usize; 3]> = const { std::cell::Cell::new([0; 3]) };
    /// Of [`CENSUS_NONSUB_UNITS`], the units that coded at least one
    /// coefficient (`txb_skip == 0`) -- the per-plane split is only
    /// comparable against the oracle's when both sides count all-zero units
    /// the same way.
    pub(super) static CENSUS_NONSUB_CODED: std::cell::Cell<[usize; 3]> = const { std::cell::Cell::new([0; 3]) };
    /// Sequential unit index, so a trace line names its own position in the
    /// frame's walk rather than relying on line order alone.
    pub(super) static CENSUS_UNIT_N: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The plane the units read from here on belong to. A thread-local rather
    /// than a `read_coeffs` parameter because the three luma call sites
    /// (`decode_rect_split`, `decode_block_rect64`, `read_inter_plane_rect`)
    /// are not this lane's to edit and would each grow an argument; the
    /// plane-owning readers stamp it once on entry instead.
    pub(super) static CENSUS_PLANE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// At least one luma axis unsubsampled -- (0,0) 4:4:4 or (1,0) 4:2:2 --
    /// for the frame being decoded, set per frame from [`crate::stream`].
    /// 4:2:0 (1,1) arms nothing.
    pub(super) static CENSUS_NONSUB: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Current `[luma, u, v]` coefficient-unit counts, per plane. Zero at 4:2:0
/// by design ([`set_census_nonsub`] arms nothing there).
pub fn census_nonsub_units() -> [usize; 3] {
    CENSUS_NONSUB_UNITS.with(|c| c.get())
}

/// Current `[luma, u, v]` counts of units that coded coefficients.
pub fn census_nonsub_coded() -> [usize; 3] {
    CENSUS_NONSUB_CODED.with(|c| c.get())
}

/// lane-av1readcensus: arms the coefficient-unit census for the frame about
/// to decode, called once per frame from [`crate::stream`]'s decode loop
/// beside [`set_subsampling`]. Armed for every chroma format with an
/// unsubsampled luma axis -- 4:4:4 (0,0) and 4:2:2 (1,0) -- and NOT for
/// 4:2:0 (1,1), where the walk-shape question this census answers does not
/// arise, so a 4:2:0 control can still assert exactly zero.
///
/// lane-av1422census422 widened this from `(ss_x == 0 && ss_y == 0)`. The
/// old condition made 4:2:2 silently UNCUNTABLE rather than uncounted. The
/// two format pairs keep disjoint conditions, so neither the 4:4:4 exact
/// counts nor the 4:2:0 zero control can move because of this.
pub(crate) fn set_census_nonsub(ss_x: u8, ss_y: u8) {
    CENSUS_NONSUB.with(|c| c.set(ss_x == 0 || ss_y == 0));
}

/// lane-av1readcensus: the plane the units read from here on belong to,
/// stamped by the plane-owning readers ([`read_plane`], [`read_inter_plane`],
/// [`read_inter_plane_rect`]) on entry.
pub(crate) fn set_census_plane(plane: usize) {
    CENSUS_PLANE.with(|c| c.set(plane));
}

/// lane-av1readcensus: is the per-unit census trace on? Shares
/// [`coeff_trace_on`]'s per-frame narrowing so one frame's walk can be
/// dumped without the rest of the stream.
fn census_trace_on() -> bool {
    if !crate::envflags::env_flag!("EC_CENSUS_UNIT") {
        return false;
    }
    match COEFF_TRACE_FRAME.with(|c| c.get()) {
        None => true,
        Some(w) if w >= usize::MAX - 1 => true,
        Some(w) => COEFF_TRACE_CUR.with(|c| c.get()) == w,
    }
}

/// lane-av1readcensus: one line per coefficient unit this decoder walks, the
/// twin of the oracle's `EC_COEFF` / `EC_COEFF_VAL` bracket around each
/// `av1_read_coeffs_txb` call. Pair the two sides by `post_rng` (or by `n`),
/// never by plane label or by walk position -- the walk order is exactly what
/// the census is measuring.
pub(super) fn census_unit(
    w: usize,
    h: usize,
    tx_type: TxType,
    eob: usize,
    all_zero: bool,
    entry_rng: u32,
    dec: &SymbolDecoder,
) {
    let plane = CENSUS_PLANE.with(|c| c.get()).min(2);
    let coded = usize::from(!all_zero);
    CENSUS_UNIT_N.with(|c| c.set(c.get() + 1));
    hit_do! {
        CENSUS_NONSUB_UNITS.with(|c| {
            if CENSUS_NONSUB.with(|on| on.get()) {
                let mut v = c.get();
                v[plane] += 1;
                c.set(v);
            }
        });
        CENSUS_NONSUB_CODED.with(|c| {
            if CENSUS_NONSUB.with(|on| on.get()) {
                let mut v = c.get();
                v[plane] += coded;
                c.set(v);
            }
        });
    }
    if !census_trace_on() {
        return;
    }
    let n = CENSUS_UNIT_N.with(|c| c.get()) - 1;
    let post_rng = dec.debug_state().0;
    eprintln!(
        "EC_CENSUS_UNIT side=ours n={n} plane={plane} w={w} h={h} tx={tx_type:?} eob={eob} \
         all_zero={all_zero} entry_rng={entry_rng} post_rng={post_rng}"
    );
}

/// How many transform units this thread has read since the last
/// [`reset_census_unit_n`] (lane-av1offtile). The per-unit oracle twin is
/// `EC_ECDUMP_IN`, one line per `read_coeffs_txb` call, so this count IS
/// directly comparable with an oracle `EC_ECDUMP_IN` census -- which is what
/// makes "we walk N units where libaom walks M" a gated claim rather than an
/// inference from the desync's position.
pub fn census_unit_n() -> usize {
    CENSUS_UNIT_N.with(|c| c.get())
}

/// Reset [`census_unit_n`] to zero (lane-av1offtile).
pub fn reset_census_unit_n() {
    CENSUS_UNIT_N.with(|c| c.set(0));
}

// As [`FILTER_INTRA_RECT_HITS`], counting only the strips BELOW 16x16
// (8x16/16x8 leaves of a 16x16 HORZ/VERT partition, lane-fistrip r1) -- the
// shapes whose `use_filter_intra` symbol had no CDF class before this round.
// A gate asserting the wider counter would be satisfied by a 32x16 strip and
// prove nothing about the new shapes.
thread_local! {
    pub(super) static FILTER_INTRA_RECT_SUB16_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`FILTER_INTRA_RECT_SUB16_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn filter_intra_rect_sub16_hits() -> usize {
    FILTER_INTRA_RECT_SUB16_HITS.with(|c| c.get())
}

// As [`FILTER_INTRA_RECT_SUB16_HITS`], one partition level further down:
// 4x8/8x4 leaves of an 8x8 HORZ/VERT (lane-fi8 r1). A gate on the sub16
// counter would be satisfied by a 16x8 strip and prove nothing here.
thread_local! {
    pub(super) static FILTER_INTRA_RECT_SUB8_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`FILTER_INTRA_RECT_SUB8_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn filter_intra_rect_sub8_hits() -> usize {
    FILTER_INTRA_RECT_SUB8_HITS.with(|c| c.get())
}

// How many [`read_coeffs`] reads resolved a `tx_type` outside `TX_CLASS_2D`
// (`V_DCT`/`H_DCT`, spec 5.11.39's `TxClass::Horiz`/`Vert`), across every
// call on the current thread -- the same before/after counter pattern as
// [`TX_DEPTH_HITS`], proving a stream actually exercised the class-split
// `eob_pt`/1D-neighbour context path rather than every block coincidentally
// landing on `DCT_DCT`.
thread_local! {
    pub(super) static TX_CLASS1_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

// How many `uv_mode` reads resolved to a directional chroma mode (neither
// `DC_PRED` nor `UV_CFL_PRED`), across every call on the current thread -- the same
// before/after counter pattern as [`FILTER_INTRA_HITS`], proving a stream
// actually exercised chroma's own directional predictor.
thread_local! {
    pub(super) static DIRECTIONAL_UV_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`DIRECTIONAL_UV_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn directional_uv_hits() -> usize {
    DIRECTIONAL_UV_HITS.with(|c| c.get())
}

// How many `angle_delta`/`angle_delta_uv` symbols this decoder has read as
// something other than [`ANGLE_DELTA_ZERO`], across every call in the
// process -- the same before/after counter pattern as [`FILTER_INTRA_HITS`],
// proving a stream actually exercised a nonzero angle delta.
thread_local! {
    pub(super) static ANGLE_DELTA_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

// How many [`read_single_ref`] reads resolved to a reference other than
// `LAST_FRAME`, across every call on the current thread -- the same before/after
// counter pattern as [`FILTER_INTRA_HITS`], proving a stream actually
// exercised a non-`LAST_FRAME` reference (`GOLDEN_FRAME`, this decoder's
// only other supported one so far) rather than every block coincidentally
// resolving `LAST_FRAME`.
thread_local! {
    pub(super) static NON_LAST_REF_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`NON_LAST_REF_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
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
    pub(super) static LAST2_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

thread_local! {
    pub(super) static LAST3_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

thread_local! {
    pub(super) static GOLDEN_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

thread_local! {
    pub(super) static BWDREF_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

thread_local! {
    pub(super) static ALTREF2_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

thread_local! {
    pub(super) static ALTREF_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

// How many [`read_comp_mode`] reads resolved `COMPOUND_REFERENCE` (lane-av1comp)
// -- proves a `reference_select` stream actually reached a block that picked
// compound, not just that the header bit was set. Every such block is
// refused after this fires ([`decode_inter_block`]/[`decode_inter_block8`]),
// so this only ever counts attempted reads, never a completed decode.
thread_local! {
    pub(super) static COMP_MODE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`COMP_MODE_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn comp_mode_hits() -> usize {
    COMP_MODE_HITS.with(|c| c.get())
}

// How many blocks [`read_intra_mode`] decoded a real (nonzero-size) palette-Y
// use for, across every call on the current thread -- the same before/after
// counter pattern as [`FILTER_INTRA_HITS`], proving a stream actually
// exercised palette reconstruction rather than only reading and refusing the
// `palette_y_mode` symbol.
thread_local! {
    pub(super) static PALETTE_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`PALETTE_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn palette_hits() -> usize {
    PALETTE_HITS.with(|c| c.get())
}

// lane-tiles r12: of those, how many sat at their own TILE's left column --
// exactly the blocks whose palette cache/ctx must NOT see the previous tile's
// colours (`av1_get_palette_cache`'s `xd->left_mbmi == NULL`). A multi-tile
// palette gate that never hits one proves nothing about the tile guard.
thread_local! {
    pub(super) static PALETTE_TILE_LEFT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`PALETTE_TILE_LEFT_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn palette_tile_left_hits() -> usize {
    PALETTE_TILE_LEFT_HITS.with(|c| c.get())
}

// lane-palette2 r1: as [`PALETTE_HITS`], for a real (nonzero-size) chroma
// palette use -- proves a stream actually reconstructed UV palette pixels,
// not just read and refused the `palette_uv_mode` symbol.
thread_local! {
    pub(super) static PALETTE_UV_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`PALETTE_UV_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn palette_uv_hits() -> usize {
    PALETTE_UV_HITS.with(|c| c.get())
}

// lane-palette2 r1: how many HORZ/VERT rect intra strips ([`read_intra_mode_rect`])
// decoded a real palette-Y use -- proves the rect strip's own palette syntax
// (not just the square path's) actually fired.
thread_local! {
    pub(super) static PALETTE_RECT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`PALETTE_RECT_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn palette_rect_hits() -> usize {
    PALETTE_RECT_HITS.with(|c| c.get())
}

// lane-palette2 r6: how many HORZ/VERT rect intra strips ran the
// [`palette_bsize_ctx_wh`]-gated code path in [`read_intra_mode_rect`] --
// i.e. `allow_screen_content_tools` was set and the strip's `bw`x`bh` was in
// range, so `palette_y_mode`/`palette_uv_mode` were actually read (and, per
// RD, almost always refused-on-use because the block doesn't end up USING a
// palette -- see r5's report). This is the site the removed refusal
// ("a HORZ/VERT intra strip in a screen-content frame") used to guard: it
// fired here whether or not the block picked a palette, so this is the
// counter that proves the case r5's narrower [`PALETTE_RECT_HITS`] almost
// never observes.
thread_local! {
    pub(super) static RECT_SCREEN_CONTENT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT_SCREEN_CONTENT_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn rect_screen_content_hits() -> usize {
    RECT_SCREEN_CONTENT_HITS.with(|c| c.get())
}

// lane-palette2 r1: how many palette-Y luma transform UNITS decoded under a
// split luma transform (`logical_tx != side`) -- proves the per-TU index-map
// windowing actually ran, not just that the whole-block path stayed exact.
// Counts units, not blocks (a 64x64 palette block split to 16x16 TUs adds 16);
// first actually incremented in r7, when the refusal it guarded was lifted.
thread_local! {
    pub(super) static PALETTE_SPLIT_TX_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    /// lane-kf900 r4: rect intra strips that ACTUALLY use a palette -- the
    /// 1:4 shapes (32x8/8x32, 16x4/4x16) and the sub-16x16 2:1 leaves
    /// (16x8/8x16), the two shapes the lifted refusals named.
    pub(super) static RECT4_PALETTE_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT4_PALETTE_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn rect4_palette_hits() -> usize {
    RECT4_PALETTE_HITS.with(|c| c.get())
}

/// Current value of [`PALETTE_SPLIT_TX_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn palette_split_tx_hits() -> usize {
    PALETTE_SPLIT_TX_HITS.with(|c| c.get())
}

thread_local! {
    /// lane-av1444d10: chroma PREDICTION units on the 32-capped tiled chroma
    /// walk (`decode_block_rect`'s `uw/uh = min(chroma, 32)` units) that were
    /// handed a STRICT sub-slice of their block's palette buffer -- i.e. the
    /// units of a block the 32-cap actually TILED (`nw * nh > 1`).
    ///
    /// CORRECTED 2026-09-30 after the refutation pass `lanes/refute-av1444d10`
    /// read the numbers back: the previous comment claimed it counted "the
    /// units that are NOT the block's top-left corner", which is not what the
    /// code did -- the hit was unconditional, so a block's corner unit fired it
    /// too AND a single-unit block (whose window is a pure re-stride copy of
    /// the whole buffer) fired it as well. That made a hit prove only that some
    /// palette chroma block existed, not that the tiled arm ran. The `bigger`
    /// guard now makes the counter discriminate. On the pinned fixture the
    /// measured value is unchanged at 4 (one block with nw = 1, nh = 2 x two
    /// planes x two unit rows); a stream whose palette chroma blocks are all
    /// single-unit now leaves it at zero.
    ///
    /// FURTHER CORRECTED the same day by the palette lane's refutation
    /// (`lanes/av1444palw.report.md` + the arm-isolated mutation): the guard is
    /// about TILING, not about `nw > 1` specifically. Isolating the two window
    /// sites shows the RECT-unit arm alone reds the 10-bit cell, and the
    /// SQUARE-unit arm alone reds the 12-bit cell *and* the 10-bit one -- so
    /// windowing is load-bearing for every tiled block, and `nw > 1` is merely
    /// the case the corpus could not reach. Do not read this counter as
    /// evidence that only the horizontal term matters.
    pub(super) static CHROMA_PALETTE_WINDOW_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`CHROMA_PALETTE_WINDOW_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn chroma_palette_window_hits() -> usize {
    CHROMA_PALETTE_WINDOW_HITS.with(|c| c.get())
}

/// Zeroes [`CHROMA_PALETTE_WINDOW_HITS`] so a gate can read it as a DELTA
/// (it is process-wide, like every counter here).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_chroma_palette_window_hits() {
    CHROMA_PALETTE_WINDOW_HITS.with(|c| c.set(0));
}

thread_local! {
    /// lane-pal8 r1: square 8x8 LEAVES ([`decode_leaf8`]) that actually use a
    /// palette (Y or UV) -- the shape every 8-bit `--tune-content=screen`
    /// stream stopped on, since aomenc palettes square blocks at 8 bit and
    /// `--min-partition-size=8` puts them on this leaf path.
    pub(super) static PALETTE_LEAF8_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`PALETTE_LEAF8_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn palette_leaf8_hits() -> usize {
    PALETTE_LEAF8_HITS.with(|c| c.get())
}

/// Zeroes [`PALETTE_LEAF8_HITS`] (per-attempt gate counting).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_palette_leaf8_hits() {
    PALETTE_LEAF8_HITS.with(|c| c.set(0));
}

thread_local! {
    /// lane-pal8 r1: NON-SKIP palette blocks on a superblock-level (64-level)
    /// HORZ/VERT or 1:4 strip ([`decode_block_rect64`]) -- i.e. a palette
    /// block that also carries a real, corner-cropped luma transform.
    pub(super) static PALETTE_SB_STRIP_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`PALETTE_SB_STRIP_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn palette_sb_strip_hits() -> usize {
    PALETTE_SB_STRIP_HITS.with(|c| c.get())
}

/// Zeroes [`PALETTE_SB_STRIP_HITS`] (per-attempt gate counting).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_palette_sb_strip_hits() {
    PALETTE_SB_STRIP_HITS.with(|c| c.set(0));
}

thread_local! {
    /// lane-av1rectreach: units of `decode_rect_split`'s `chroma_tiled` walk
    /// whose PER-UNIT reach ([`tu_reach_rect`], libaom's per-unit
    /// `has_top_right` / `has_bottom_left`) DIFFERS from the block-level
    /// `reach` this walk used to hand every unit -- i.e. units for which the
    /// two answers are not interchangeable. This is what a gate needs before
    /// it compares the walk byte-exactly: if the two agreed everywhere, the
    /// substitution could not change a single predicted sample and the
    /// exactness assert would be measuring a witness the fix cannot touch.
    ///
    /// What it does NOT prove is that the argument handed to the unit reader
    /// is the per-unit one -- the count is taken from the same expression
    /// either way, so deleting the substitution reds the exactness assert
    /// rather than this counter. That is the right way round: the assert is
    /// the claim, this is only the floor under it.
    pub(super) static RECT_TILED_CHROMA_UNIT_REACH_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    /// Chroma units of the same walk whose grid is exactly 2x1 (a 4:4:4
    /// 64x32 chroma plane block as two 32x32 units).
    pub(super) static RECT_TILED_CHROMA_2X1_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Chroma units of the same walk whose grid is exactly 1x2 (4:4:4 32x64,
    /// or 16x64 as two 16x32s).
    pub(super) static RECT_TILED_CHROMA_1X2_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Chroma units of the same walk in any other multi-unit grid (2x2 and
    /// up). lane-av1422lift: this used to say "only reachable at 4:2:2, which
    /// is refused, so this stays 0 on every admitted stream", and that was
    /// true. 4:2:2 is decoded now, so the "cannot happen" is gone -- but the
    /// arm is still UNWITNESSED: measured on this branch's tip over all 21
    /// committed 4:2:2 cells and over the 4:2:0 / 4:4:4 cells probed, it
    /// reads 0 on every one of them. No committed gate reads it either (the
    /// only readers of `rect_tiled_chroma_grid_hits()` are two 4:4:4 strip
    /// gates, where `nxn` is interpolated into a failure message and never
    /// asserted). Kept so the match above is exhaustive.
    pub(super) static RECT_TILED_CHROMA_NXN_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current values of the four `RECT_TILED_CHROMA_*` counters, in the order
/// `(unit_reach_differs, 2x1, 1x2, other multi-unit)`.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn rect_tiled_chroma_grid_hits() -> (usize, usize, usize, usize) {
    (
        RECT_TILED_CHROMA_UNIT_REACH_HITS.with(|c| c.get()),
        RECT_TILED_CHROMA_2X1_HITS.with(|c| c.get()),
        RECT_TILED_CHROMA_1X2_HITS.with(|c| c.get()),
        RECT_TILED_CHROMA_NXN_HITS.with(|c| c.get()),
    )
}

/// Zeroes the four `RECT_TILED_CHROMA_*` counters (gate tests call this
/// before their own decode).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_rect_tiled_chroma_grid_hits() {
    RECT_TILED_CHROMA_UNIT_REACH_HITS.with(|c| c.set(0));
    RECT_TILED_CHROMA_2X1_HITS.with(|c| c.set(0));
    RECT_TILED_CHROMA_1X2_HITS.with(|c| c.set(0));
    RECT_TILED_CHROMA_NXN_HITS.with(|c| c.set(0));
}

// lane-t900 r22: palette blocks reconstructed inside an INTER frame (the
// intra-in-inter path, `decode_inter_block`/`decode_inter_block8`) -- counted
// apart from [`PALETTE_HITS`] so a witness gate can prove the INTER-frame
// capability rather than the key frame's, which every screen stream also
// carries.
thread_local! {
    pub(super) static INTRA_IN_INTER_PALETTE_Y_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static INTRA_IN_INTER_PALETTE_UV_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// lane-t900 r23: as [`INTRA_IN_INTER_PALETTE_Y_HITS`], narrowed to the 8x8
// LEAF site ([`decode_inter_block8`]'s intra arm) -- the >=16 site fires the
// shared counter too, so only this one proves a gate really reached the leaf
// where the refusal used to sit.
thread_local! {
    pub(super) static INTER8_PALETTE_HITS: std::cell::Cell<(usize, usize)> =
        const { std::cell::Cell::new((0, 0)) };
}

/// Current values of the 8x8-leaf intra-in-inter palette counters, `(y, uv)`.
pub(crate) fn inter8_palette_hits() -> (usize, usize) {
    INTER8_PALETTE_HITS.with(std::cell::Cell::get)
}

/// Current values of the two intra-in-inter palette counters, `(y, uv)`.
pub(crate) fn intra_in_inter_palette_hits() -> (usize, usize) {
    (
        INTRA_IN_INTER_PALETTE_Y_HITS.with(|c| c.get()),
        INTRA_IN_INTER_PALETTE_UV_HITS.with(|c| c.get()),
    )
}

// lane-hgkf r2: SKIPPED intra blocks whose `tx_depth` split the luma transform
// and which are therefore predicted one transform unit at a time (a skipped
// block codes no coefficients, so nothing else in the stream witnesses this
// path -- only the pixels do).
thread_local! {
    pub(super) static SKIP_SPLIT_TX_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`SKIP_SPLIT_TX_HITS`] (blocks, not units).
pub fn skip_split_tx_hits() -> usize {
    SKIP_SPLIT_TX_HITS.with(|c| c.get())
}

// lane-svt1: SKIPPED square blocks carrying a palette/intrabc prediction whose
// `tx_depth` split the luma transform -- the arm that predicted DC off the
// block edges before the fix. Like [`SKIP_SPLIT_TX_HITS`] only the pixels
// witness this path, so a gate that wants it needs the counter.
thread_local! {
    pub(super) static SKIP_SPLIT_TX_OVERRIDE_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1chromadc: chroma units of a skipped block whose intra-BC /
    /// UV-palette override had to be WINDOWED to the unit's own extent --
    /// i.e. `cn_cols * cn_rows > 1`, which at 4:2:0 never happens (the chroma
    /// plane block is one 32x32 unit) and at 4:4:4 is every block of 64 and
    /// above. Counts UNITS, not blocks. EXCLUSIVE to this walk: the other two
    /// override sites (the luma slot and `decode_intrabc_owned_rect`) have
    /// their own windows.
    pub(super) static SKIP_CHROMA_OVERRIDE_WINDOW_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`SKIP_SPLIT_TX_OVERRIDE_HITS`] (blocks).
pub fn skip_split_tx_override_hits() -> usize {
    SKIP_SPLIT_TX_OVERRIDE_HITS.with(|c| c.get())
}

/// lane-av1chromadc: [`SKIP_CHROMA_OVERRIDE_WINDOW_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub fn skip_chroma_override_window_hits() -> usize {
    SKIP_CHROMA_OVERRIDE_WINDOW_HITS.with(|c| c.get())
}

/// Zeroes [`SKIP_CHROMA_OVERRIDE_WINDOW_HITS`] before a gate's own decode.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub fn reset_skip_chroma_override_window_hits() {
    SKIP_CHROMA_OVERRIDE_WINDOW_HITS.with(|c| c.set(0));
}

/// Zeroes [`SKIP_SPLIT_TX_OVERRIDE_HITS`] before a gate's own decode attempt.
pub fn reset_skip_split_tx_override_hits() {
    SKIP_SPLIT_TX_OVERRIDE_HITS.with(|c| c.set(0));
}

// lane-svtinter: palette colour-index maps whose block is cut by the frame's
// bottom/right mi edge, so spec 5.11.50's `onscreenWidth`/`onscreenHeight`
// really are smaller than the block and FEWER symbols are read than the
// block has cells (class map-read-past-frame-edge).
thread_local! {
    pub(super) static PALETTE_ONSCREEN_CUT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`PALETTE_ONSCREEN_CUT_HITS`] (colour-index maps).
pub fn palette_onscreen_cut_hits() -> usize {
    PALETTE_ONSCREEN_CUT_HITS.with(|c| c.get())
}

/// Zeroes [`PALETTE_ONSCREEN_CUT_HITS`] before a gate's own decode attempt.
pub fn reset_palette_onscreen_cut_hits() {
    PALETTE_ONSCREEN_CUT_HITS.with(|c| c.set(0));
}

/// Zeroes [`SKIP_SPLIT_TX_HITS`] before a gate's own decode attempt.
pub fn reset_skip_split_tx_hits() {
    SKIP_SPLIT_TX_HITS.with(|c| c.set(0));
}

// lane-intrainter r1: intra blocks inside an INTER frame whose `tx_depth`
// symbol split their luma transform, bucketed by block side (8, 16, 32, 64) --
// the gate's proof that the lifted path actually ran, per size.
thread_local! {
    pub(super) static INTRA_IN_INTER_SPLIT_TX_HITS: std::cell::Cell<[usize; 4]> =
        const { std::cell::Cell::new([0; 4]) };
}

/// Current value of [`INTRA_IN_INTER_SPLIT_TX_HITS`], indexed 8/16/32/64.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn intra_in_inter_split_tx_hits() -> [usize; 4] {
    INTRA_IN_INTER_SPLIT_TX_HITS.with(std::cell::Cell::get)
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
    /// The block vector [`read_intra_mode`] just decoded, handed to
    /// [`decode_block`] off the call stack (same idiom as [`PALETTE_PRED`],
    /// which the alternative -- a tenth tuple member through every caller --
    /// already avoids). Always `.take()`n by the block that reads it.
    /// How many blocks read `use_intrabc == 1` and took the DV path -- the
    /// gate's hit counter. Bumped on the READ, before the rect-strip refusal
    /// returns too, so it counts reads, not reconstructions (a refused block
    /// is never reconstructed).
    pub(super) static INTRABC_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Zeroes [`RECT_INTRABC_READS`] (gate tests call this before a decode).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_rect_intrabc_reads() {
    RECT_INTRABC_READS.with(|c| c.set(0));
}

thread_local! {
    /// Blocks that USED intrabc at an 8x8 LEAF (`decode_leaf8`) specifically
    /// -- the shape lane-kf900 r7 fixed; [`INTRABC_HITS`] counts every shape.
    pub(super) static LEAF8_INTRABC_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    /// Chroma reads of an intrabc TX4-split leaf that consulted an ARMED
    /// [`INTRABC_CHROMA_TX`] slot -- proof that the arm's frame-copy +
    /// inherited-type route actually ran (lane-av1intrapred r2's non-vacuity
    /// counter: zero means the gate's fixture no longer reaches the route).
    pub(super) static INTRABC_TX4_CHROMA_COPY_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`INTRABC_TX4_CHROMA_COPY_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn intrabc_tx4_chroma_copy_hits() -> usize {
    INTRABC_TX4_CHROMA_COPY_HITS.with(|c| c.get())
}

/// Zeroes [`INTRABC_TX4_CHROMA_COPY_HITS`] (gate tests call this before a decode).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_intrabc_tx4_chroma_copy_hits() {
    INTRABC_TX4_CHROMA_COPY_HITS.with(|c| c.set(0));
}

/// Current value of [`LEAF8_INTRABC_HITS`].
pub(crate) fn leaf8_intrabc_hits() -> usize {
    LEAF8_INTRABC_HITS.with(|c| c.get())
}

/// Zeroes [`LEAF8_INTRABC_HITS`] (gate tests call this before a decode).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_leaf8_intrabc_hits() {
    LEAF8_INTRABC_HITS.with(|c| c.set(0));
}

/// Current value of [`INTRABC_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn intrabc_hits() -> usize {
    INTRABC_HITS.with(|c| c.get())
}

thread_local! {
    /// An intrabc block that IS a sub-8x8 group's CHROMA REFERENCE and is
    /// SKIPPED: the route that arms [`INTRABC_CHROMA_TX`] and then reads no
    /// chroma coefficient at all, so the slot used to stay armed into the NEXT
    /// block's chroma read. Both group shapes (4x4 split, 4x8/8x4 rect) count
    /// here. A non-zero value is what makes the disarm below load-bearing.
    pub(super) static SKIPPED_INTRABC_CHROMA_ARM_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`SKIPPED_INTRABC_CHROMA_ARM_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates and the probe
pub(crate) fn skipped_intrabc_chroma_arm_hits() -> usize {
    SKIPPED_INTRABC_CHROMA_ARM_HITS.with(|c| c.get())
}

/// Zeroes [`SKIPPED_INTRABC_CHROMA_ARM_HITS`] (gate tests call this before a
/// decode).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_skipped_intrabc_chroma_arm_hits() {
    SKIPPED_INTRABC_CHROMA_ARM_HITS.with(|c| c.set(0));
}

thread_local! {
    /// lane-av1-ibcskip2: a sub-8x8 4:4:4 leaf that is BOTH intrabc AND
    /// `skip` -- the arm whose chroma is the bare frame copy at the DV
    /// (libaom `av1_build_inter_predictors_sb` runs regardless of `skip`).
    /// Before the fix this route predicted intra-DC and every one of this
    /// fixture's 150 such cells differed from the oracle on both chroma
    /// planes.
    pub(super) static SKIPPED_INTRABC_DV_COPY_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`SKIPPED_INTRABC_DV_COPY_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn skipped_intrabc_dv_copy_hits() -> usize {
    SKIPPED_INTRABC_DV_COPY_HITS.with(|c| c.get())
}

/// Zeroes [`SKIPPED_INTRABC_DV_COPY_HITS`] (gate tests call this before a
/// decode).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_skipped_intrabc_dv_copy_hits() {
    SKIPPED_INTRABC_DV_COPY_HITS.with(|c| c.set(0));
}

/// Zeroes [`INTRABC_HITS`] (gate tests call this before a decode).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_intrabc_hits() {
    INTRABC_HITS.with(|c| c.set(0));
}

thread_local! {
    /// lane-t900 r32: intrabc blocks whose transform size came off the INTER
    /// var-tx partition tree (unskipped, `TX_MODE_SELECT`) -- the capability
    /// that replaced the refusal "an intrabc block under TxMode::Select".
    pub(super) static INTRABC_VARTX_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1-ibcvtx: intrabc var-tx trees that resolved to MIXED leaf
    /// sizes (not one square size across every leaf) -- the engagement
    /// counter proving a witness actually exercises the per-leaf reconstruct.
    pub(super) static INTRABC_VARTX_MIXED_LEAVES_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1txr: as above for a `BLOCK_4X8`/`BLOCK_8X4` intrabc leaf --
    /// libaom's `block_signals_txsize` is `bsize > BLOCK_4X4`, so these leaves
    /// signal a transform size where a plain `BLOCK_4X4` does not. Reading no
    /// symbol here desynced `warped.obu` at the very next block.
    pub(super) static RECT_INTRABC_VARTX_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1-intrabc: rect INTRABC blocks whose reconstruction
    /// [`decode_intrabc_rect`] ran -- the engagement counter the rect-strip
    /// gates assert on (a refused block bumps [`INTRABC_HITS`] but never
    /// this one).
    pub(super) static INTRABC_RECT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Of those, how many read the INTER var-tx tree (unskipped, TX_MODE_SELECT).
    pub(super) static INTRABC_RECT_VARTX_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1-intrabc r4: rect INTRABC blocks whose reconstruction
    /// [`decode_intrabc_rect`] ran with `skip == 0` (a real residual read),
    /// split by the strip's orientation -- `bw > bh` is a HORZ strip,
    /// `bh > bw` a VERT one. The reviewer's unblock condition needs a CODED
    /// witness in BOTH orientations, and the existing `INTRABC_RECT_HITS`
    /// counts neither orientation nor the skip flag, so an arm that flipped
    /// to an all-skip stream (no residual reader at all) would stay green.
    pub(super) static INTRABC_RECT_CODED_HITS: std::cell::Cell<[usize; 2]> =
        const { std::cell::Cell::new([0, 0]) };
    /// lane-av1-intrabc r5: the `skip == 1` twin of [`INTRABC_RECT_CODED_HITS`]
    /// -- rect INTRABC blocks reconstructed as a bare prediction copy, split
    /// by orientation. The r5 panic (residual stride `bw * bh` underrunning
    /// `reconstruct_mc_rect`'s `row * side + w` index on a VERT strip) was
    /// invisible to every gate because they only counted CODED strips.
    pub(super) static INTRABC_RECT_SKIPPED_HITS: std::cell::Cell<[usize; 2]> =
        const { std::cell::Cell::new([0, 0]) };
    /// lane-ibc444c: rect intra-BC blocks whose chroma plane block was sized
    /// `bw >> ss_x` by `bh >> ss_y` -- the 4:4:4 and 4:4:0 cells, where
    /// `av1_get_max_uv_txsize` (`ss_size_lookup[bsize]`, blockd.h) applies no
    /// halving of its own. At 4:2:0 this is the same arithmetic as the halving,
    /// so a `>= 1` witness can only come from a stream that is not 4:2:0.
    /// EXCLUSIVE to this arm: no other caller sizes an intrabc rect block's
    /// chroma plane block.
    pub(super) static IBC_RECT_CHROMA_FOOTPRINT_444_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    /// lane-whtshape: LOSSLESS rect intrabc blocks whose chroma plane block
    /// [`decode_intrabc_rect`] walked as a `(cw/4) * (ch/4)` PLANE-MAJOR
    /// raster of TX_4X4 units -- the shape libaom's lossless geometry codes
    /// (`read_tx_size` returns `TX_4X4` for EVERY plane of a lossless block,
    /// `get_vartx_max_txsize` likewise, and `read_tx_mode` forced the frame's
    /// `ONLY_4X4`) -- instead of the single rect unit, which is what put a
    /// non-4x4 unit on the lossless Walsh-Hadamard path. EXCLUSIVE to this arm:
    /// no other caller walks a rect intrabc block's chroma, so `>= 1` cannot
    /// be satisfied by any older route.
    pub(super) static INTRABC_RECT_LOSSLESS_CHROMA4_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    /// lane-av1chromarect: 128-root intra-BC HORZ/VERT strips
    /// ([`decode_intrabc_owned_rect`]) whose chroma plane block was sized
    /// `bw >> ss_x` by `bh >> ss_y` -- the 4:4:4 and 4:4:0 cells, where
    /// `av1_get_max_uv_txsize` (`ss_size_lookup[bsize]`, blockd.h:1372) applies
    /// no halving of its own. At 4:2:0 the two forms are the same arithmetic,
    /// so a `>= 1` witness can only come from a stream that is not 4:2:0.
    /// EXCLUSIVE to this arm: no other caller sizes a 128-root intra-BC
    /// strip's chroma plane block.
    pub(super) static IBC_OWNED_RECT_CHROMA_FOOTPRINT_444_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    /// lane-av1422ibctxsel: `decode_intrabc_owned_rect`'s NON-SPLIT chroma
    /// gather taken through the 4:2:2 pair rule (`around_mi_422_chroma`)
    /// under the ss (1, 0) guard -- both residual branches (the var-tx tree
    /// and the no-leaves twin). `>= 1` can only come from a 4:2:2 stream
    /// that reaches this function's non-split chroma arm; the 4:2:0/4:4:4
    /// arms keep the plain `around_mi_rect` gather (`ss_y != 0` / `ss_x == 0`
    /// take the else), so the counter stays 0 for them.
    pub(super) static IBC_OWNED_RECT_422_PAIR_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// lane-av1-intrabc r4: coded (`skip == 0`) rect intrabc blocks reconstructed,
/// as `[horz, vert]` -- see [`INTRABC_RECT_CODED_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub fn intrabc_rect_coded_hits() -> [usize; 2] {
    INTRABC_RECT_CODED_HITS.with(std::cell::Cell::get)
}

/// lane-av1-intrabc r5: skipped (`skip == 1`) rect intrabc blocks
/// reconstructed, as `[horz, vert]` -- see [`INTRABC_RECT_SKIPPED_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub fn intrabc_rect_skipped_hits() -> [usize; 2] {
    INTRABC_RECT_SKIPPED_HITS.with(std::cell::Cell::get)
}

/// Current value of [`INTRABC_RECT_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub fn intrabc_rect_hits() -> usize {
    INTRABC_RECT_HITS.with(|c| c.get())
}

/// Zeroes [`INTRABC_RECT_HITS`]/[`INTRABC_RECT_VARTX_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_intrabc_rect_hits() {
    INTRABC_RECT_HITS.with(|c| c.set(0));
    INTRABC_RECT_VARTX_HITS.with(|c| c.set(0));
    INTRABC_RECT_CODED_HITS.with(|c| c.set([0, 0]));
    INTRABC_RECT_SKIPPED_HITS.with(|c| c.set([0, 0]));
}

/// lane-ibc444c: [`IBC_RECT_CHROMA_FOOTPRINT_444_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub fn ibc_rect_chroma_footprint_444_hits() -> usize {
    IBC_RECT_CHROMA_FOOTPRINT_444_HITS.with(|c| c.get())
}

/// lane-ibc444c: zeroes [`IBC_RECT_CHROMA_FOOTPRINT_444_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_ibc_rect_chroma_footprint_444_hits() {
    IBC_RECT_CHROMA_FOOTPRINT_444_HITS.with(|c| c.set(0));
}

/// lane-av1chromarect: [`IBC_OWNED_RECT_CHROMA_FOOTPRINT_444_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub fn ibc_owned_rect_chroma_footprint_444_hits() -> usize {
    IBC_OWNED_RECT_CHROMA_FOOTPRINT_444_HITS.with(|c| c.get())
}

/// lane-av1chromarect: zeroes [`IBC_OWNED_RECT_CHROMA_FOOTPRINT_444_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_ibc_owned_rect_chroma_footprint_444_hits() {
    IBC_OWNED_RECT_CHROMA_FOOTPRINT_444_HITS.with(|c| c.set(0));
}

/// lane-av1422ibctxsel: [`IBC_OWNED_RECT_422_PAIR_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub fn ibc_owned_rect_422_pair_hits() -> usize {
    IBC_OWNED_RECT_422_PAIR_HITS.with(|c| c.get())
}

/// lane-av1422ibctxsel: zeroes [`IBC_OWNED_RECT_422_PAIR_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_ibc_owned_rect_422_pair_hits() {
    IBC_OWNED_RECT_422_PAIR_HITS.with(|c| c.set(0));
}

/// lane-whtshape: [`INTRABC_RECT_LOSSLESS_CHROMA4_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub fn intrabc_rect_lossless_chroma4_hits() -> usize {
    INTRABC_RECT_LOSSLESS_CHROMA4_HITS.with(|c| c.get())
}

/// lane-whtshape: zeroes [`INTRABC_RECT_LOSSLESS_CHROMA4_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_intrabc_rect_lossless_chroma4_hits() {
    INTRABC_RECT_LOSSLESS_CHROMA4_HITS.with(|c| c.set(0));
}

/// Current value of [`INTRABC_RECT_VARTX_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub fn intrabc_rect_vartx_hits() -> usize {
    INTRABC_RECT_VARTX_HITS.with(|c| c.get())
}

thread_local! {
    /// lane-av1-ibcrect: `use_intrabc` blocks on a 128-root HORZ/VERT strip
    /// (BLOCK_128X64 / BLOCK_64X128) whose reconstruction
    /// [`decode_intrabc_128rect`] ran -- the shape the named refusal used to
    /// guard (`decode_block_128rect`). A refused block bumps
    /// [`INTRABC_HITS`] but never this one.
    pub(super) static INTRABC_128RECT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1-ibc128chunk: chroma transform units the 128-root intrabc
    /// strip's mu-chunk walk read, one per `read_inter_plane` chroma call.
    /// At 4:2:0 a 64x64 mu chunk's chroma plane block is 32x32, so one
    /// TX_32X32 per plane per chunk; at 4:2:2 it is 32x64 and holds TWO
    /// STACKED units per plane. The square-cut `chunk_chroma = cside * 64 /
    /// side` model walked the 4:2:0 count on every axis, so this counter is
    /// the reach/arm witness for the per-axis walk.
    pub(super) static INTRABC_128RECT_CHROMA_CHUNK_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`INTRABC_128RECT_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub fn intrabc_128rect_hits() -> usize {
    INTRABC_128RECT_HITS.with(|c| c.get())
}

/// Zeroes [`INTRABC_128RECT_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_intrabc_128rect_hits() {
    INTRABC_128RECT_HITS.with(|c| c.set(0));
}

/// Current value of [`INTRABC_128RECT_CHROMA_CHUNK_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub fn intrabc_128rect_chroma_chunk_hits() -> usize {
    INTRABC_128RECT_CHROMA_CHUNK_HITS.with(|c| c.get())
}

/// Zeroes [`INTRABC_128RECT_CHROMA_CHUNK_HITS`] and
/// [`INTRABC_128RECT_CHUNK_SHAPE`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_intrabc_128rect_chroma_chunk_hits() {
    INTRABC_128RECT_CHROMA_CHUNK_HITS.with(|c| c.set(0));
    INTRABC_128RECT_CHUNK_SHAPE.with(|c| c.set(0));
}

/// Current value of [`INTRABC_VARTX_HITS`].
pub fn intrabc_vartx_hits() -> usize {
    INTRABC_VARTX_HITS.with(|c| c.get())
}

/// Current value of [`INTRABC_VARTX_MIXED_LEAVES_HITS`].
pub fn intrabc_vartx_mixed_leaves_hits() -> usize {
    INTRABC_VARTX_MIXED_LEAVES_HITS.with(|c| c.get())
}

/// Zeroes [`INTRABC_VARTX_HITS`] (gate tests call this before a decode).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_intrabc_vartx_hits() {
    INTRABC_VARTX_HITS.with(|c| c.set(0));
}

/// Zeroes [`INTRABC_VARTX_MIXED_LEAVES_HITS`] (gate tests call this before a
/// decode).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_intrabc_vartx_mixed_leaves_hits() {
    INTRABC_VARTX_MIXED_LEAVES_HITS.with(|c| c.set(0));
}

/// Current value of [`RECT_INTRABC_VARTX_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub fn rect_intrabc_vartx_hits() -> usize {
    RECT_INTRABC_VARTX_HITS.with(|c| c.get())
}

// How many blocks decoded `skip_mode == 1` (lane-av1comp round 14) --
// proves a real `skip_mode_present` stream actually reached a block that
// picked it, not just that the frame header bit was set.
thread_local! {
    pub(super) static SKIP_MODE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`SKIP_MODE_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn skip_mode_hits() -> usize {
    SKIP_MODE_HITS.with(|c| c.get())
}

// lane-motionmode: how many blocks actually decoded `obmc_selected == true`
// (as opposed to just reaching an eligible `read_motion_mode` symbol read)
// -- the gate's own proof that a real aomenc `--enable-obmc=1` stream
// reached [`obmc_blend`], not just the header bit.
thread_local! {
    pub(super) static OBMC_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

// lane-fimv r1 (class wrong-alphabet-same-value): how many blocks took the
// 2-symbol `obmc_cdf` alphabet *because* `force_integer_mv` (or a scaled
// reference) vetoed `WARPED_CAUSAL` in libaom `motion_mode_allowed`, while
// `allow_warped_motion == 1` and `num_proj_ref >= 1` would otherwise have
// picked the 3-symbol `motion_mode_cdf`. These are exactly the blocks this
// decoder used to read with the wrong alphabet, so a gate that asserts this
// counter advanced proves the fixed path fired (class
// `gate-blind-to-feature`).
thread_local! {
    pub(super) static FIMV_ALPHABET_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`FIMV_ALPHABET_HITS`].
pub fn fimv_alphabet_hits() -> usize {
    FIMV_ALPHABET_HITS.with(|c| c.get())
}

// lane-fimv r1: how many frame headers this decoder has decoded with
// `force_integer_mv == 1` (bumped by `stream.rs` per frame). Lets a gate
// prove the encoder really produced the header state under test rather than
// assume it.
thread_local! {
    pub(super) static FIMV_FRAME_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`FIMV_FRAME_HITS`].
pub fn fimv_frame_hits() -> usize {
    FIMV_FRAME_HITS.with(|c| c.get())
}

/// Current value of [`OBMC_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
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
    pub(super) static SCALED_COMPOUND_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static SCALED_WARP_FALLBACK_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static SCALED_OBMC_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static SCALED_INTERINTRA_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static SCALED_BLOCK8_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static SCALED_WARP_SUPPRESSED_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static MIXED_SCALE_COMPOUND_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`MIXED_SCALE_COMPOUND_HITS`]: compound blocks with one
/// scaled and one unscaled tap.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn mixed_scale_compound_hits() -> usize {
    MIXED_SCALE_COMPOUND_HITS.with(|c| c.get())
}

/// Current value of [`SCALED_WARP_SUPPRESSED_HITS`]: blocks that would have
/// read the 3-symbol `motion_mode_cdf` but read the 2-symbol `obmc_cdf`
/// instead because their reference is scaled (libaom `motion_mode_allowed`,
/// `blockd.h:1484`).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn scaled_warp_suppressed_hits() -> usize {
    SCALED_WARP_SUPPRESSED_HITS.with(|c| c.get())
}

/// Current value of [`SCALED_COMPOUND_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn scaled_compound_hits() -> usize {
    SCALED_COMPOUND_HITS.with(|c| c.get())
}

/// Current value of [`SCALED_WARP_FALLBACK_HITS`].
pub fn scaled_warp_fallback_hits() -> usize {
    SCALED_WARP_FALLBACK_HITS.with(|c| c.get())
}

/// Current value of [`SCALED_OBMC_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn scaled_obmc_hits() -> usize {
    SCALED_OBMC_HITS.with(|c| c.get())
}

/// Current value of [`SCALED_INTERINTRA_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn scaled_interintra_hits() -> usize {
    SCALED_INTERINTRA_HITS.with(|c| c.get())
}

/// Current value of [`SCALED_BLOCK8_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn scaled_block8_hits() -> usize {
    SCALED_BLOCK8_HITS.with(|c| c.get())
}

// lane-motionmode round 3: same proof as [`OBMC_HITS`], narrowed to
// `decode_inter_block8`'s own 8x8-leaf `read_motion_mode` -- a subset of
// [`OBMC_HITS`] (both fire together on an 8x8 hit), lets the gate tell an
// 8x8-leaf OBMC block apart from a 16x16+ one.
thread_local! {
    pub(super) static OBMC_HITS_8: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// lane-interp3 r1: how many inter blocks read a SWITCHABLE `interp_filter`
// pair whose two directions differ (`enable_dual_filter` only -- with dual
// filter off both directions take the single symbol, so this can never fire).
// A gate asserting it proves the stream really carries dual-filter blocks.
thread_local! {
    pub(super) static DUAL_FILTER_DIFF_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// lane-interp3 r1: how many COMPOUND 8x8 leaves read their own
// `read_mb_interp_filter` symbol(s) -- the read this round added (the leaf
// used to skip it entirely, losing a symbol and stamping the `[3, 3]`
// no-filter sentinel that made a neighbouring OBMC block panic).
thread_local! {
    pub(super) static COMPOUND8_FILTER_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`DUAL_FILTER_DIFF_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn dual_filter_diff_hits() -> usize {
    DUAL_FILTER_DIFF_HITS.with(|c| c.get())
}

/// Current value of [`COMPOUND8_FILTER_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn compound8_filter_hits() -> usize {
    COMPOUND8_FILTER_HITS.with(|c| c.get())
}

// lane-warp round 1: how many blocks resolved `motion_mode_allowed` to
// `WARPED_CAUSAL`-eligible (3-symbol read, `num_proj_ref >= 1` under
// `allow_warped_motion`) AND the symbol actually decoded to `WARPED_CAUSAL`
// -- the gate's proof that a fixture really exercises the alphabet this
// round changed, even though the block itself still refuses (warp
// estimation/filter not ported).
thread_local! {
    pub(super) static WARP_SELECTED_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`WARP_SELECTED_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn warp_selected_hits() -> usize {
    WARP_SELECTED_HITS.with(|c| c.get())
}

// lane-cwarp r1: how many COMPOUND_REFERENCE blocks had at least one
// reference predicted through the per-ref GLOBAL warp (`av1_warp_plane`
// into the compound intermediate) rather than translationally -- the
// gate's proof that a `GLOBAL_GLOBALMV` block under a ROTZOOM/AFFINE model
// really reached [`crate::warp::warp_affine_compound`].
// lane-av1422warp: the 4:2:2 coverage witness has to show compound and warped
// motion firing ABOVE the vertical midpoint, and every pre-existing warp
// counter is frame-global. These two are the same arms restricted to blocks
// whose top mode-info row is in the frame's upper half, which is the half
// this lane's evidence covers.
thread_local! {
    pub(super) static TOP_HALF_COMPOUND_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static TOP_HALF_WARP_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many `GLOBAL_GLOBALMV` (compound) inter blocks decoded in the frame's
/// upper half -- the block's top mode-info row is under `mi_rows / 2`.
///
/// NO COMMITTED TEST READS THIS, and none can while the 4:2:2 header refusal
/// stands: the pinned 4:2:2 coverage gate asserts the byte pin and the
/// refusal by name, so nothing in the suite ever decodes a 4:2:2 stream.
/// The figure quoted for that fixture comes from a patch-run-restore build
/// with the `EC_AV1_ALLOW_422_PROBE` bypass applied and then reverted, which
/// is the same discipline every 4:2:2 lane in this family uses.
#[allow(dead_code)]
pub(crate) fn top_half_compound_hits() -> usize {
    TOP_HALF_COMPOUND_HITS.with(std::cell::Cell::get)
}

/// How many inter blocks in the frame's upper half satisfy libaom's
/// `is_global_mv_block` on at least one reference slot -- mode
/// `GLOBAL_GLOBALMV` AND block >= 8x8 AND that slot's own global-motion
/// model > `TRANSLATION`. This is the block set whose prediction goes
/// through `crate::warp`, and it is exactly `is_global_mv0 || is_global_mv1`
/// as computed at the increment site; it is NOT merely "some slot has a
/// non-TRANSLATION model", which also counts blocks that are not global-mv
/// blocks.
///
/// Like [`top_half_compound_hits`], no committed test reads it: with the
/// 4:2:2 header refusal in force nothing in the suite decodes the stream
/// these numbers describe.
#[allow(dead_code)]
pub(crate) fn top_half_warp_hits() -> usize {
    TOP_HALF_WARP_HITS.with(std::cell::Cell::get)
}

thread_local! {
    pub(super) static COMPOUND_WARP_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`COMPOUND_WARP_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn compound_warp_hits() -> usize {
    COMPOUND_WARP_HITS.with(|c| c.get())
}

// lane-interp3 r3: the same proof narrowed to the 8x8 LEAF's own compound
// arm ([`decode_inter_block8`]) -- that arm predicted both taps
// translationally until r3, so a gate needs to tell a warped 8x8 compound
// leaf apart from a warped 16x16+ one (both bump [`COMPOUND_WARP_HITS`]).
thread_local! {
    pub(super) static COMPOUND_WARP_HITS_8: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`COMPOUND_WARP_HITS_8`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn compound_warp_hits_8() -> usize {
    COMPOUND_WARP_HITS_8.with(|c| c.get())
}

// lane-gmaffine r1: how many blocks built their prediction from a
// SIX-parameter (`AFFINE`) global-motion model through
// `crate::warp::global_warp_params` -- the gate's proof that a stream really
// carried an AFFINE `global_motion_params` AND that a block was predicted
// with it, not merely that the header parsed one.
thread_local! {
    pub(super) static AFFINE_GM_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`AFFINE_GM_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn affine_gm_hits() -> usize {
    AFFINE_GM_HITS.with(|c| c.get())
}

// lane-av1distwtd: how many blocks took the DISTANCE-WEIGHTED compound
// combine rather than the simple average -- `compound_idx == 0` under
// `enable_jnt_comp`, whose weights come from
// [`crate::compound::dist_wtd_comp_weight_assign`] instead of the constant
// `(8, 8)` split. Counting the `idx == 0` arm (not merely the
// `compound_idx` symbol read) is what makes this a witness of the TOOL: a
// stream that carries `enable_jnt_comp = true` in its sequence header but
// codes every compound block with `compound_idx == 1` leaves this at 0 and
// decoded identically without the whole distance-weight path.
thread_local! {
    pub(super) static DIST_WTD_COMP_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`DIST_WTD_COMP_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn dist_wtd_comp_hits() -> usize {
    DIST_WTD_COMP_HITS.with(|c| c.get())
}

/// Resets [`DIST_WTD_COMP_HITS`] to 0.
#[allow(dead_code)] // test-only
pub(crate) fn reset_dist_wtd_comp_hits() {
    DIST_WTD_COMP_HITS.with(|c| c.set(0));
}

// lane-av1gwarp12: how many 16x16+ blocks built their SINGLE-reference
// prediction through `crate::warp::global_warp_params` with a FOUR-parameter
// (`ROTZOOM`) model. Vanilla aomenc pins its global-motion search to ROTZOOM
// (`global_motion_facade.c:24`), so this is the only >TRANSLATION single-ref
// global-warp arm a real aomenc stream can reach (AFFINE needs the widened
// side build and its own counter, [`AFFINE_GM_HITS`]).
thread_local! {
    pub(super) static ROTZOOM_GM_WARP_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`ROTZOOM_GM_WARP_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn rotzoom_gm_warp_hits() -> usize {
    ROTZOOM_GM_WARP_HITS.with(|c| c.get())
}

// lane-t900 r9: rect blocks whose warp-sample top-right probe answers
// differently at the block's WIDTH than at libaom's
// `AOMMAX(xd->width, xd->height)` -- each one is a `num_proj_ref` that could
// pick the wrong motion_mode alphabet.
thread_local! {
    pub(super) static TR_REACH_LONGER_SIDE_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`TR_REACH_LONGER_SIDE_HITS`].
pub fn tr_reach_longer_side_hits() -> usize {
    TR_REACH_LONGER_SIDE_HITS.with(std::cell::Cell::get)
}

// lane-t900 r9: libaom `is_nontrans_global_motion` (mvref_common.h) returns 0
// for `AOMMIN(mi_size_wide[bsize], mi_size_high[bsize]) < 2`, i.e. any block
// with a 4-px side is NEVER "non-translational global" and therefore ALWAYS
// reads its interp-filter symbol. Counts the blocks where that guard flips
// our answer -- a GLOBALMV/GLOBAL_GLOBALMV block with a non-TRANSLATION model
// and a 4-px side, which we used to suppress the interp read on (a pre-mode
// entropy desync one symbol wide).
thread_local! {
    pub(super) static GM_NONTRANS_SMALL_SIDE_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`GM_NONTRANS_SMALL_SIDE_HITS`].
pub fn gm_nontrans_small_side_hits() -> usize {
    GM_NONTRANS_SMALL_SIDE_HITS.with(std::cell::Cell::get)
}

// lane-gmaffine r1: 8x8-leaf-only counters -- how many `BLOCK_8X8` leaves
// coded `GLOBALMV` (`GLOBALMV_HITS_8`) and how many built a real
// `WARPED_CAUSAL` local-warp prediction there (`WARP_HITS_8`, incremented
// only once `find_projection` returned a usable model, unlike
// `WARP_SELECTED_HITS` which counts the symbol). Both are the 8x8 gates'
// proof that the leaf path itself fired, not the 16x16+ one.
thread_local! {
    pub(super) static GLOBALMV_HITS_8: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static WARP_HITS_8: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`GLOBALMV_HITS_8`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn globalmv_hits_8() -> usize {
    GLOBALMV_HITS_8.with(|c| c.get())
}

/// Current value of [`WARP_HITS_8`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn warp_hits_8() -> usize {
    WARP_HITS_8.with(|c| c.get())
}

// How many blocks decoded `interintra == 1` with the non-wedge blended
// prediction applied (lane-interintra r1) -- the gate's proof that a
// stream actually exercised interintra.
thread_local! {
    pub(super) static INTERINTRA_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`INTERINTRA_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn interintra_hits() -> usize {
    INTERINTRA_HITS.with(|c| c.get())
}

// lane-midcut r1: of those, how many were RECTANGULAR (`write_w != write_h`)
// -- the shape `interintra_blend` used to build a square `side x side` intra
// predictor for, with `ii_size_scales`/`edges` taken at the square. Every
// pre-existing interintra gate passed with that bug, so the rect arm needs
// its own counter to stop being vacuous.
thread_local! {
    pub(super) static INTERINTRA_RECT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`INTERINTRA_RECT_HITS`].
pub fn interintra_rect_hits() -> usize {
    INTERINTRA_RECT_HITS.with(|c| c.get())
}

// How many blocks decoded `comp_group_idx == 1` (masked COMPOUND_REFERENCE,
// wedge or diffwtd) and had `compound_type`/`wedge_idx`/`wedge_sign`/
// `mask_type` consumed entropy-exact -- lane-maskcomp r1's proof that a
// gate fixture actually exercises this alphabet, even though the block
// still refuses (the mask blend itself is unported).
thread_local! {
    pub(super) static MASKED_COMPOUND_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`MASKED_COMPOUND_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn masked_compound_hits() -> usize {
    MASKED_COMPOUND_HITS.with(|c| c.get())
}

// lane-intersub8 r4/r5: the NON-SQUARE half of `MASKED_COMPOUND_HITS` -- a
// masked-compound block whose true footprint is rectangular, i.e. the case
// whose `compound_type`/`wedge_idx` CDF row r4 fixed (it used to read the
// square envelope's `BLOCK_SIZES_ALL` index). Square blocks cannot show that
// defect, so a gate asserting `masked_compound_hits` alone stays vacuous for it.
thread_local! {
    pub(super) static RECT_MASKED_COMPOUND_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT_MASKED_COMPOUND_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn rect_masked_compound_hits() -> usize {
    RECT_MASKED_COMPOUND_HITS.with(|c| c.get())
}

// How many blocks decoded a real `COMPOUND_WEDGE` block (`comp_group_idx ==
// 1`, `compound_type == 0`) -- lane-wedge r3's proof a gate fixture
// actually exercised the wedge blend, not just the DIFFWTD half of
// `MASKED_COMPOUND_HITS`.
thread_local! {
    pub(super) static WEDGE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`WEDGE_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn wedge_hits() -> usize {
    WEDGE_HITS.with(|c| c.get())
}

// How many blocks decoded a real wedge-INTERINTRA block
// (`interintra == 1 && use_wedge_interintra == 1`) -- lane-wii r2's proof
// a gate fixture exercised the wedge blend (fixed sign 0), not just the
// smooth arm counted by `INTERINTRA_HITS`.
thread_local! {
    pub(super) static WII_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`WII_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn wii_hits() -> usize {
    WII_HITS.with(|c| c.get())
}

// lane-sbrect10 r1: how many INTRA blocks read `uv_mode` off the 13-symbol
// NO-CFL alphabet *because their size excludes CFL* (libaom `is_cfl_allowed`:
// wide <= 32 && high <= 32). The witness that a gate stream really carries a
// 64-axis intra block -- the shape whose 14-symbol read desynced every inter
// frame carrying one.
thread_local! {
    pub(super) static NOCFL_UV_MODE_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`NOCFL_UV_MODE_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn nocfl_uv_mode_hits() -> usize {
    NOCFL_UV_MODE_HITS.with(|c| c.get())
}

// lane-inter16ab r5: how many COMPOUND_WEDGE / wedge-INTERINTRA blocks used a
// RECTANGULAR codebook row (hgtw/hltw), per shape -- the gate's proof that
// the rect wedge masks lifted this round actually fired, not just the square
// ones already counted by `WEDGE_HITS`/`WII_HITS`.
thread_local! {
    pub(super) static RECT_WEDGE_HITS: std::cell::Cell<[usize; 6]> = const { std::cell::Cell::new([0; 6]) };
    pub(super) static RECT_WII_HITS: std::cell::Cell<[usize; 6]> = const { std::cell::Cell::new([0; 6]) };
}

/// Current value of [`RECT_WEDGE_HITS`], indexed by [`RECT_WEDGE_SHAPES`].
pub(crate) fn rect_wedge_hits() -> [usize; 6] {
    RECT_WEDGE_HITS.with(|c| c.get())
}

/// Current value of [`RECT_WII_HITS`], indexed by [`RECT_WEDGE_SHAPES`].
pub(crate) fn rect_wii_hits() -> [usize; 6] {
    RECT_WII_HITS.with(|c| c.get())
}

// lane-inter4 r4: how many 16x8/8x16 INTER LEAVES actually ran the OBMC
// blend -- the gate's proof that `--enable-obmc=1` reached this shape (the
// r3 refusal's replacement: prefilt frames are bit-exact vs an instrumented
// aomdec, neighbour list included).
thread_local! {
    pub(super) static OBMC_RECT_LEAF_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`OBMC_RECT_LEAF_HITS`].
pub(crate) fn obmc_rect_leaf_hits() -> usize {
    OBMC_RECT_LEAF_HITS.with(|c| c.get())
}

// lane-obmcrec r1: how many OBMC neighbours were taken from the SECOND half of
// a 4-wide/4-tall (1:4 strip) chroma pair -- the case whose interp filter used
// to be read from the pair's FIRST mi, which is a different block and, when
// that half is intra, carries the `[3, 3]` "no filter" sentinel (the three
// 1080p 10-bit film cuts refused there). The gate's proof the shape fired.
thread_local! {
    pub(super) static OBMC_PAIR_FILTER_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`OBMC_PAIR_FILTER_HITS`].
pub(crate) fn obmc_pair_filter_hits() -> usize {
    OBMC_PAIR_FILTER_HITS.with(|c| c.get())
}

// lane-t900 r14: how many OBMC neighbour predictions run PAST the frame edge
// -- libaom's `op_mi_size` is `AOMMIN(xd->width|height, mi_step)`, the block's
// own side, so a block overhanging the bottom/right still predicts the full
// neighbour strip (the destination write is what clips). We used to shrink the
// prediction to the visible part, which at 4 chroma rows/cols also swaps the
// vertical/horizontal kernel to the 4-tap one. The gate's proof the shape fired.
thread_local! {
    pub(super) static OBMC_EDGE_SPAN_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`OBMC_EDGE_SPAN_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn obmc_edge_span_hits() -> usize {
    OBMC_EDGE_SPAN_HITS.with(|c| c.get())
}

/// Current value of [`OBMC_HITS_8`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
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
    pub(super) static EXTENDED_PARTITION_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`EXTENDED_PARTITION_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn extended_partition_hits() -> usize {
    EXTENDED_PARTITION_HITS.with(|c| c.get())
}

// lane-partitions r1: how many 32x32 quadrants decoded a real
// `PARTITION_HORZ`/`PARTITION_VERT` (two true rectangular strips, unlike
// `PARTITION_HORZ_B`'s square-context stand-in) -- the free-partition
// gate's proof this round's arms actually fired.
thread_local! {
    pub(super) static RECT_PARTITION_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT_PARTITION_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn rect_partition_hits() -> usize {
    RECT_PARTITION_HITS.with(|c| c.get())
}

// lane-inter8 r1: how many superblocks an inter tile coded as a single
// whole-SB `PARTITION_NONE` 64x64 inter block (the case aomenc picks for
// static content, previously a named refusal).
thread_local! {
    /// lane-sb128c r1: how many 128x128 `PARTITION_NONE` roots this INTER tile
    /// path decoded as one whole block (its gate's own witness).
    pub(crate) static INTER_SB128_NONE_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// 128x64 / 64x128 INTER blocks decoded at the 128 root (lane-sb128c r7).
    pub(crate) static INTER_SB128_HORZ_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static INTER_SB128_VERT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`INTER_SB128_HORZ_HITS`], for a gate's before/after delta.
pub(crate) fn inter_sb128_horz_hits() -> usize {
    INTER_SB128_HORZ_HITS.with(std::cell::Cell::get)
}

/// Current value of [`INTER_SB128_VERT_HITS`], for a gate's before/after delta.
pub(crate) fn inter_sb128_vert_hits() -> usize {
    INTER_SB128_VERT_HITS.with(std::cell::Cell::get)
}

/// Current value of [`INTER_SB128_NONE_HITS`], for a gate's before/after delta.
pub fn inter_sb128_none_hits() -> usize {
    INTER_SB128_NONE_HITS.with(std::cell::Cell::get)
}

thread_local! {
    /// lane-sb128c r3: how many INTRA-coded 128x128 blocks inside an INTER
    /// frame this decoder reconstructed (mu-chunk chroma, four TX_64X64 luma
    /// units) -- the gate's witness that the lifted refusal's case fired.
    pub(crate) static INTRA_128_IN_INTER_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`INTRA_128_IN_INTER_HITS`], for a gate's before/after delta.
pub fn intra_128_in_inter_hits() -> usize {
    INTRA_128_IN_INTER_HITS.with(std::cell::Cell::get)
}

thread_local! {
    /// lane-av1ibc128arm: how many 64x64 mu-chunk CHROMA units the
    /// intra-in-inter 128x128 root read (`decode_inter_block`'s
    /// `side > 64` mu-chunk arm, the INTRA half of the inter pair). Every
    /// unit is one `read_plane` on U and one on V, so the counter is the
    /// gate's proof the arm ran at all -- no committed 4:4:4 fixture
    /// reached it before this lane.
    pub(crate) static INTRA_128_IN_INTER_MU_CHROMA_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`INTRA_128_IN_INTER_MU_CHROMA_HITS`], for a gate's
/// before/after delta.
pub fn intra_128_in_inter_mu_chroma_hits() -> usize {
    INTRA_128_IN_INTER_MU_CHROMA_HITS.with(std::cell::Cell::get)
}

thread_local! {
    pub(super) static INTER_SB_NONE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`INTER_SB_NONE_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn inter_sb_none_hits() -> usize {
    INTER_SB_NONE_HITS.with(|c| c.get())
}

// lane-inter8 r1: how many INTERIOR (not frame-edge-straddling) 16x16 inter
// blocks coded a real `PARTITION_SPLIT` into four 8x8 leaves -- the straddle
// path has always run that loop, so only this counter proves the newly-lifted
// alphabet value fired.
thread_local! {
    pub(super) static INTER_SUB16_SPLIT_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`INTER_SUB16_SPLIT_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn inter_sub16_split_hits() -> usize {
    INTER_SUB16_SPLIT_HITS.with(|c| c.get())
}

// lane-t900 r28: how many 16x16 inter splits reached their four 8x8 leaves
// while at least one reference picture was SCALED (superres) -- the counter
// behind the lifted "8x8 partition leaf under a scaled reference" refusal.
thread_local! {
    pub(super) static SCALED_LEAF8_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`SCALED_LEAF8_HITS`].
pub fn scaled_leaf8_hits() -> usize {
    SCALED_LEAF8_HITS.with(|c| c.get())
}

// lane-t900 r28: how many sub-8x8 inter blocks (the 4x4 split and the 8x4/4x8
// rect pair) were predicted from a SCALED reference -- the counter behind the
// lifted "sub-8x8 inter block under a scaled reference" refusal.
thread_local! {
    pub(super) static SCALED_SUB8_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`SCALED_SUB8_HITS`].
pub fn scaled_sub8_hits() -> usize {
    SCALED_SUB8_HITS.with(|c| c.get())
}

// lane-rectwire r2: how many `PARTITION_HORZ`/`VERT` strips actually decoded
// real (non-skip) coefficients through [`read_coeffs_rect`] -- proves the
// rect coefficient reader itself fired, not just the strip-level partition
// symbol [`RECT_PARTITION_HITS`] already counts.
thread_local! {
    pub(super) static RECT_COEFF_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT_COEFF_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn rect_coeff_hits() -> usize {
    RECT_COEFF_HITS.with(|c| c.get())
}

// lane-rectx r1: how many below-16x16 HORZ/VERT leaves ([`decode_leaf_rect`])
// decoded real (non-skip) TX_16X8/TX_8X16 coefficients -- the counter the
// charter's real-aomenc gate hard-asserts.
thread_local! {
    pub(super) static RECT_LEAF_COEFF_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT_LEAF_COEFF_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn rect_leaf_coeff_hits() -> usize {
    RECT_LEAF_COEFF_HITS.with(|c| c.get())
}

// lane-intrarect r1: how many intra-coded HORZ/VERT strips decoded on the
// INTER block path ([`decode_intra_rect_in_inter`]), per shape class --
// [0] the superblock-level 64x32/32x64, [1] 32x16/16x32, [2] 16x8/8x16.
// Every one of them was the named refusal "an intra-coded HORZ/VERT strip
// needs rectangular intra prediction" before this round.
thread_local! {
    pub(super) static INTRA_RECT_IN_INTER_HITS: std::cell::Cell<[usize; 3]> =
        const { std::cell::Cell::new([0; 3]) };
}

/// Current value of [`INTRA_RECT_IN_INTER_HITS`] for one shape class
/// (0 = 64x32/32x64, 1 = 32x16/16x32, 2 = 16x8/8x16).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn intra_rect_strip_in_inter_hits(class: usize) -> usize {
    INTRA_RECT_IN_INTER_HITS.with(|c| c.get()[class])
}

// lane-rect64port r1 (ported by hand from lane-intra64split 5ffbaf3): coded
// (non-skip) 64-level HORZ/VERT intra strips whose luma transform is the
// un-split TX_64X32/TX_32X64 -- the corner-cropped 32x32 coefficient read of
// [`decode_rect_split`], the shape both 10-bit 3840x1608 film cuts stop on.
// Split by ORIENTATION (index 0 = 64x32 wide / HORZ, 1 = 32x64 tall / VERT):
// a single counter cannot tell an axis swap from a working arm
// (class [[scan-weights-cross-axis]]), so the gate asserts both cells.
thread_local! {
    pub(super) static RECT64_CORNER_TU_HITS: std::cell::Cell<[usize; 2]> =
        const { std::cell::Cell::new([0; 2]) };
}

/// Current value of [`RECT64_CORNER_TU_HITS`] for one orientation
/// (0 = 64x32, 1 = 32x64).
pub(crate) fn rect64_corner_tu_hits(orient: usize) -> usize {
    RECT64_CORNER_TU_HITS.with(|c| c.get()[orient])
}

// lane-intrasplit r1: intra HORZ/VERT strips inside an INTER frame whose luma
// transform is SPLIT, bucketed by the read `tx_depth` (index 1 and 2; index 0
// is never incremented) -- the gate's proof that the lifted path ran per depth.
thread_local! {
    pub(super) static INTRA_RECT_IN_INTER_SPLIT_TX_HITS: std::cell::Cell<[usize; 3]> =
        const { std::cell::Cell::new([0; 3]) };
}

// lane-t900 r21: rect intra strips inside an inter frame that read the
// screen-content palette syntax (`allow_screen_content_tools` set and the
// strip's own shape admitted by `palette_bsize_ctx_wh`) -- the site a blanket
// refusal used to guard, so this counter is what keeps its witness gate
// non-vacuous.
thread_local! {
    pub(super) static INTRA_RECT_IN_INTER_SCREEN_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`INTRA_RECT_IN_INTER_SCREEN_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn intra_rect_in_inter_screen_hits() -> usize {
    INTRA_RECT_IN_INTER_SCREEN_HITS.with(std::cell::Cell::get)
}

/// Resets [`INTRA_RECT_IN_INTER_SCREEN_HITS`] before a decode.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_intra_rect_in_inter_screen_hits() {
    INTRA_RECT_IN_INTER_SCREEN_HITS.with(|c| c.set(0));
}

/// Current value of [`INTRA_RECT_IN_INTER_SPLIT_TX_HITS`], indexed by tx depth.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn intra_rect_in_inter_split_tx_hits() -> [usize; 3] {
    INTRA_RECT_IN_INTER_SPLIT_TX_HITS.with(std::cell::Cell::get)
}

// lane-av1txbands: how many `tx_depth`/`txfm_split` reads the deblock-grid
// approximation would have resolved off a DIFFERENT CDF row than the real
// `TXFM_CONTEXT` bands do -- the exact blocks this band read fixes, on ANY
// frame (lane-midcut r2 had it inter-frame-only). A plain "rect/intra block"
// tally is green on the old code too (class gate-blind-to-feature).
thread_local! {
    pub(super) static TXCTX_DEBLOCK_DIVERGENCE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`TXCTX_DEBLOCK_DIVERGENCE_HITS`].
#[allow(dead_code)] // gate counter accessor with no reader yet; the counter itself is live
pub(crate) fn txctx_deblock_divergence_hits() -> usize {
    TXCTX_DEBLOCK_DIVERGENCE_HITS.with(std::cell::Cell::get)
}

// lane-av1txctxband: how many `get_tx_size_context` reads the per-CELL
// `is_inter_block` cover and the per-COLUMN `above_inter`/`left_inter` bands
// DISAGREE about -- i.e. exactly the reads whose CDF row the old code chose
// wrongly. Green on the old code too (a plain "some block was inter" tally
// would be), so this is the non-vacuity arm of the witness gate: it is 0 on
// every stream where the bands happened to be right and non-zero on the
// 4:4:4 lossy `--sb-size=128` cq-62 stream whose entropy forked at read 32813
// on the 8x4 intra leaf at mi(57,52).
thread_local! {
    pub(super) static TXCTX_INTER_CELL_BAND_DIVERGENCE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`TXCTX_INTER_CELL_BAND_DIVERGENCE_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gate
pub(crate) fn txctx_inter_cell_band_divergence_hits() -> usize {
    TXCTX_INTER_CELL_BAND_DIVERGENCE_HITS.with(std::cell::Cell::get)
}

/// Zeroes [`TXCTX_INTER_CELL_BAND_DIVERGENCE_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gate
pub(crate) fn reset_txctx_inter_cell_band_divergence_hits() {
    TXCTX_INTER_CELL_BAND_DIVERGENCE_HITS.with(|c| c.set(0));
}

// lane-intra14 r1: how many intra-coded 1:4 strips decoded on the INTER
// block path, per shape -- [0] 64x16, [1] 16x64, [2] 32x8, [3] 8x32. Every
// one of them was the named refusal "an intra-coded 1:4 (or other non-2:1)
// rect strip on the inter block path" before this round.
thread_local! {
    pub(super) static INTRA_RECT4_IN_INTER_HITS: std::cell::Cell<[usize; 4]> =
        const { std::cell::Cell::new([0; 4]) };
}

/// Current value of [`INTRA_RECT4_IN_INTER_HITS`] for one shape
/// (0 = 64x16, 1 = 16x64, 2 = 32x8, 3 = 8x32).
pub(crate) fn intra_rect4_strip_in_inter_hits(shape: usize) -> usize {
    INTRA_RECT4_IN_INTER_HITS.with(|c| c.get()[shape])
}

// lane-av1444rect: how many 1:4 inter strips at `ss_x == 0` read their chroma
// coefficient context over the STRIP'S OWN extent instead of the 4:2:0 pair's
// (see [`decode_inter_block`]'s `around_c`). `is_chroma_reference`
// (av1_common_int.h:1454) is unconditionally true at ss (0,0), so no pair
// exists there; the counter keeps a 4:4:4 1:4-strip gate from passing
// vacuously (a 4:2:0 stream of the same content leaves it at 0). It proves
// REACH only, never that the fix bites -- that is the gate's byte compare and
// its mutation, not this count (skill://ec-av1-pipeline-gate-counters).
thread_local! {
    pub(super) static RECT4_INTER_OWN_CHROMA444_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT4_INTER_OWN_CHROMA444_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn rect4_inter_own_chroma444_hits() -> usize {
    RECT4_INTER_OWN_CHROMA444_HITS.with(std::cell::Cell::get)
}

/// Resets [`RECT4_INTER_OWN_CHROMA444_HITS`] before a decode.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_rect4_inter_own_chroma444_hits() {
    RECT4_INTER_OWN_CHROMA444_HITS.with(|c| c.set(0));
}

// lane-av1444rect r2: how many intra-BC 1:4 strips at `ss (0,0)` took the
// own-chroma arm of [`decode_rect4_16_intrabc`]'s `own444`. A 4:2:0 / 4:2:2
// stream of the same recipe leaves it at 0, which is what makes it the
// 4:4:4-only route.
//
// WHAT THIS COUNTER IS NOT: it is NOT evidence that the fix bites, and it
// never was. A route counter fires identically before and after a change that
// changes nothing downstream, so a non-zero count proves REACH and nothing
// more. On this lane it is also armed deliberately OUTSIDE the `own444`
// expression, so that the mutation (`own444 = false`) still increments it and
// the red comes from the pixel/entropy arm instead. The evidence that the ss
// gate bites is the measured fork move -- first entropy fork read 11750 ->
// 12465, luma first wrong sample 164416 -> 205264, 455240 -> 390380 total --
// not this count. See skill://ec-av1-pipeline-gate-counters: a counter needs
// a DIFFERS-style assertion to carry a claim.
thread_local! {
    pub(super) static INTRABC_RECT4_OWN_CHROMA444_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// lane-av1422luma: the ss (1,0) twin of `INTRABC_RECT4_OWN_CHROMA444_HITS`
// -- how many intra-BC 1:4 strips at 4:2:2 took the own-chroma (no pair)
// route. Same caveat as the 4:4:4 counter: it proves REACH and nothing else.
// lane-av1422lift: 4:2:2 is DECODED, so this counter is no longer pinned
// behind a header refusal -- and it DOES fire: measured on this branch's tip,
// the six committed corpus cells each read 1 more than the cell before
// (`s422_320x246` 1, `s422_322x240` 2, `s422_322x246` 3, `s422_352x242_10b`
// 4, `s422_416x242_10b` 5, `s422_416x250_10b` 6), and every other committed
// 4:2:2 cell leaves it at 6. **No committed gate reads it**: the accessor
// below has no callers in the crate (the 4:4:4 twin is read at two
// `stream.rs` sites, this one nowhere). The honest sentence is therefore
// "no committed test reads this counter", which is what the 4:4:4 twin's
// doc says about itself -- it does NOT become "the corpus gate reads it".
thread_local! {
    pub(super) static INTRABC_RECT4_OWN_CHROMA422_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`INTRABC_RECT4_OWN_CHROMA422_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn intrabc_rect4_own_chroma422_hits() -> usize {
    INTRABC_RECT4_OWN_CHROMA422_HITS.with(std::cell::Cell::get)
}

/// Resets [`INTRABC_RECT4_OWN_CHROMA422_HITS`] before a decode.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_intrabc_rect4_own_chroma422_hits() {
    INTRABC_RECT4_OWN_CHROMA422_HITS.with(|c| c.set(0));
}

/// Current value of [`INTRABC_RECT4_OWN_CHROMA444_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn intrabc_rect4_own_chroma444_hits() -> usize {
    INTRABC_RECT4_OWN_CHROMA444_HITS.with(std::cell::Cell::get)
}

/// Resets [`INTRABC_RECT4_OWN_CHROMA444_HITS`] before a decode.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_intrabc_rect4_own_chroma444_hits() {
    INTRABC_RECT4_OWN_CHROMA444_HITS.with(|c| c.set(0));
}

// lane-rectx r5: how many times a NON-strip `kf_y_mode` reader
// ([`decode_block`], [`decode_block_rect`], [`decode_block_rect64`]) took an
// above/left mode from the mi-exact map that DIFFERS from its coarse 16x16
// slot -- i.e. how often the r5 defect would have picked the wrong CDF row.
thread_local! {
    pub(super) static MODE_MI_OVERRIDE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`MODE_MI_OVERRIDE_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn mode_mi_override_hits() -> usize {
    MODE_MI_OVERRIDE_HITS.with(|c| c.get())
}

// lane-cfl r1: how many times a chroma intra-edge filter-type read
// (`get_filt_type`, libaom reconintra.c) took an above/left UV mode from the
// mi-exact map that DIFFERS from its coarse 16x16 slot -- i.e. how often the
// r1 defect would have filtered a chroma edge with the wrong strength.
thread_local! {
    pub(super) static UV_MODE_MI_OVERRIDE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`UV_MODE_MI_OVERRIDE_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn uv_mode_mi_override_hits() -> usize {
    UV_MODE_MI_OVERRIDE_HITS.with(|c| c.get())
}

// lane-t900 r12: how many chroma edge-filter-type reads took their above/left
// `uv_mode` from the mi GRID where the one-slot-per-column map would have
// answered differently -- the sub-8x8 pair case where the pair's own first
// half had already overwritten the column slot the chroma-ref half reads.
thread_local! {
    pub(super) static UV_MODE_GRID_OVERRIDE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`UV_MODE_GRID_OVERRIDE_HITS`].
pub fn uv_mode_grid_override_hits() -> usize {
    UV_MODE_GRID_OVERRIDE_HITS.with(|c| c.get())
}

// lane-t900 r8: how many intra edge-filter-type reads (luma + chroma) the
// INTER neighbour stamp saved: the coarse SUB band still held a SMOOTH mode
// from an earlier block in that pixel row/column while the mi-exact map --
// which an inter block only started writing this round -- correctly says the
// neighbour is not smooth (libaom `is_smooth`: an inter neighbour's `mode` is
// an INTER mode, and its `uv_mode` "is not set for inter blocks").
thread_local! {
    pub(super) static INTER_NEIGHBOUR_NOT_SMOOTH_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`INTER_NEIGHBOUR_NOT_SMOOTH_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn inter_neighbour_not_smooth_hits() -> usize {
    INTER_NEIGHBOUR_NOT_SMOOTH_HITS.with(|c| c.get())
}

// lane-mtfix r1: how many times a chroma intra-edge filter-type read was
// suppressed at a TILE edge that is not a frame edge -- the above/left
// `uv_mode` neighbour of a block on a tile's first mi row/column lives in
// another tile, so libaom's `above_mbmi`/`left_mbmi` are NULL there. The
// coarse [`SUB`] maps are frame-sized and (`above_uv_mode`) not cleared per
// tile, so without the guard in [`Neighbours::smooth_uv_neighbour`] the tile
// above would decide this tile's chroma edge filter strength.
thread_local! {
    pub(super) static UV_TILE_EDGE_SUPPRESSED_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`UV_TILE_EDGE_SUPPRESSED_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn uv_tile_edge_suppressed_hits() -> usize {
    UV_TILE_EDGE_SUPPRESSED_HITS.with(|c| c.get())
}

// lane-av1leftref: how many chroma intra-edge filter-type reads land on a
// left neighbour whose `uv_mode` at libaom's real read cell
// (`base_mi[ss_y * mi_stride - 1]` = row `+ss_y`, col `-1`) DIFFERS from the
// cell this decoder used to read (its own mi row). Two buckets: the raw mode
// value moved, and `is_smooth_mode` -- the boolean the read actually feeds --
// flipped. The second is the one that can move pixels.
thread_local! {
    pub(super) static UV_LEFT_SS_Y_MODE_DIFF_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    pub(super) static UV_LEFT_SS_Y_SMOOTH_DIFF_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`UV_LEFT_SS_Y_MODE_DIFF_HITS`].
pub fn uv_left_ss_y_mode_diff_hits() -> usize {
    UV_LEFT_SS_Y_MODE_DIFF_HITS.with(|c| c.get())
}

/// Current value of [`UV_LEFT_SS_Y_SMOOTH_DIFF_HITS`].
pub fn uv_left_ss_y_smooth_diff_hits() -> usize {
    UV_LEFT_SS_Y_SMOOTH_DIFF_HITS.with(|c| c.get())
}

// lane-av1leftwit: decode-path chroma edge-filter-type reads at `ss_y > 0`
// whose left-neighbour COLUMN holds a run of length 1 -- the cell this
// decoder reads (its own mi row) and the cell libaom reads (one row down)
// were written by two DIFFERENT blocks, i.e. the left neighbour is at most
// 1 mi tall. That is the only geometry in which the `+ss_y` term can change
// the answer at all; `UV_LEFT_SS_Y_READS` alone proves nothing about it.
thread_local! {
    pub(super) static UV_LEFT_1MI_SPLIT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`UV_LEFT_1MI_SPLIT_HITS`].
pub fn uv_left_1mi_split_hits() -> usize {
    UV_LEFT_1MI_SPLIT_HITS.with(|c| c.get())
}

// lane-cfl r1 gate counters: `UV_CFL_PRED` blocks (every `cfl_alpha_signs`
// read) and chroma blocks with a NONZERO `angle_delta_uv` -- the two block
// shapes the r1 chroma defect showed up on.
thread_local! {
    pub(super) static CFL_BLOCK_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static UV_ANGLE_DELTA_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// lane-av1cfl: which `cfl_ac_ss` arm ran, one bump per CfL block. The 4:4:4
// arm is libaom `cfl_luma_subsampling_444_lbd_c` (1x1 luma per chroma sample)
// and the 4:2:2 arm is `cfl_luma_subsampling_422_lbd_c`; only the last two
// reach `cfl_ac_q3_at`'s 2x2 average, so these four are the reachability
// evidence for that function's format-blind extent.
thread_local! {
    pub(super) static CFL_AC_444_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static CFL_AC_422_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static CFL_AC_420_SQ_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static CFL_AC_420_RECT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static CFL_AC_Q3_AT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `[4:4:4, 4:2:2, 4:2:0 square, 4:2:0 rect]` [`cfl_ac_ss`] arm hits.
pub fn cfl_ac_arm_hits() -> [usize; 4] {
    [
        CFL_AC_444_HITS.with(|c| c.get()),
        CFL_AC_422_HITS.with(|c| c.get()),
        CFL_AC_420_SQ_HITS.with(|c| c.get()),
        CFL_AC_420_RECT_HITS.with(|c| c.get()),
    ]
}

/// Entries into [`cfl_ac_q3_at`] itself -- the one arm whose extent is
/// `bw / 2, bh / 2` and whose footprint is a 2x2 luma average. The gate
/// proves it is reached ONLY at 4:2:0.
pub fn cfl_ac_q3_at_hits() -> usize {
    CFL_AC_Q3_AT_HITS.with(|c| c.get())
}

/// Current value of [`CFL_BLOCK_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn cfl_block_hits() -> usize {
    CFL_BLOCK_HITS.with(|c| c.get())
}

/// Current value of [`UV_ANGLE_DELTA_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn uv_angle_delta_hits() -> usize {
    UV_ANGLE_DELTA_HITS.with(|c| c.get())
}

// lane-sbpart r2: how many superblock-level `PARTITION_HORZ`/`PARTITION_VERT`
// blocks (two true 64x32/32x64 strips, [`decode_block_rect64`]) fired -- the
// gate's proof this round's arms actually reached a real block, not just
// that `partition_w64` read one of those two symbol values.
thread_local! {
    pub(super) static SB_RECT_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

// Current value of [`SB_RECT_HITS`].
// lane-hgkf r1: the frame-edge partition bit, one level each. Slot 0 = a
// 64-level gathered edge bit read as HORZ/VERT, 1 = the same bit read as
// SPLIT, 2/3 = the 32-level pair. A gate asserting slot 0 or 2 proves the
// non-SPLIT edge arm actually decoded (before this round the bit was read and
// thrown away, so that arm was unreachable).
thread_local! {
    pub(super) static EDGE_PART_HITS: std::cell::Cell<[usize; 4]> = const { std::cell::Cell::new([0; 4]) };
}

/// Current value of [`EDGE_PART_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn edge_part_hits() -> [usize; 4] {
    EDGE_PART_HITS.with(|c| c.get())
}

/// Reset [`EDGE_PART_HITS`] (a gate counts one stream at a time).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn reset_edge_part_hits() {
    EDGE_PART_HITS.with(|c| c.set([0; 4]));
}

#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn sb_rect_hits() -> usize {
    SB_RECT_HITS.with(|c| c.get())
}

// lane-golomb r2: the 32x32-level FRAME-EDGE partition. Slot 0/1 = the
// gathered edge bit read as 0 (HORZ/VERT) / 1 (SPLIT); slot 2 = a 32-level
// PARTITION_HORZ decoded with `has_rows32 == false` (only the top 32x16 half
// is coded), slot 3 = a PARTITION_VERT with `has_cols32 == false`. Both tile
// paths (intra and inter) feed the same counter.
//
// lane-golomb r3: slots 4..7 are the same four facts one level up, at the
// SUPERBLOCK (64x64) edge -- 4/5 = the gathered 64-level bit read as
// HORZ/VERT / SPLIT, 6 = a 64x32 bottom-edge strip decoded with
// `has_rows == false`, 7 = a 32x64 right-edge strip. Real aomenc answers a
// bottom-edge superblock with the 64-level HORZ (measured: 192x80 flat-band,
// every SB of the last row), so the 32-level slots stay 0 on that content and
// only these prove the arm fired.
thread_local! {
    pub(super) static EDGE32_HITS: std::cell::Cell<[usize; 8]> =
        const { std::cell::Cell::new([0; 8]) };
}

/// Current value of [`EDGE32_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn edge32_hits() -> [usize; 8] {
    EDGE32_HITS.with(|c| c.get())
}

// lane-part32 r4: how many superblock-level AB blocks (`PARTITION_HORZ_A`/
// `_B`/`VERT_A`/`_B` at 64x64 -- two 32x32 squares plus one 64x32/32x64
// strip) fired. Real aomenc picks these at 64 even with
// `--enable-ab-partitions=0` (lane-sbpart r2's own measurement), so this is
// the counter the gate hard-asserts.
thread_local! {
    pub(super) static SB_AB_HITS: std::cell::Cell<[usize; 4]> =
        const { std::cell::Cell::new([0; 4]) };
}

/// Current value of [`SB_AB_HITS`], summed over the four arms.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn sb_ab_hits() -> usize {
    SB_AB_HITS.with(|c| c.get().iter().sum())
}

/// [`SB_AB_HITS`] split per arm, in `PARTITION_HORZ_A`, `_HORZ_B`,
/// `_VERT_A`, `_VERT_B` order -- what the gate hard-asserts arm by arm, so a
/// run where aomenc's RD only ever picked (say) `HORZ_A` cannot pass off as
/// proof of all four.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn sb_ab_hits_by_arm() -> [usize; 4] {
    SB_AB_HITS.with(std::cell::Cell::get)
}

// lane-tx64x16: the two 1:4 superblock arms, counted per orientation --
// `PARTITION_HORZ_4` (four 64x16 strips) and `PARTITION_VERT_4` (four 16x64
// strips). Two counters, not one, because a gate that only proved "some 1:4
// strip fired" would pass on a stream that never coded the other axis, and
// every scan/context table this round added comes in a transposed pair.
thread_local! {
    pub(super) static SB_RECT4_HORZ_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static SB_RECT4_VERT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of `SB_RECT4_HORZ_HITS` (64x16 strips decoded).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn sb_rect4_horz_hits() -> usize {
    SB_RECT4_HORZ_HITS.with(|c| c.get())
}

/// Current value of `SB_RECT4_VERT_HITS` (16x64 strips decoded).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn sb_rect4_vert_hits() -> usize {
    SB_RECT4_VERT_HITS.with(|c| c.get())
}

// lane-tx64x16 r3: the 32x32-level 1:4 arms, counted per orientation --
// `PARTITION_HORZ_4` (four 32x8 strips) and `PARTITION_VERT_4` (four 8x32
// strips), the arms the ReelB mid-film extract stops at.
thread_local! {
    pub(super) static RECT4_32_HORZ_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static RECT4_32_VERT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-inter4 r1: the INTER-frame twins of the two counters above --
    /// four 32x8 / 8x32 inter strips of a 32x32-level `PARTITION_HORZ_4` /
    /// `PARTITION_VERT_4`.
    pub(super) static RECT4_32_HORZ_INTER_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static RECT4_32_VERT_INTER_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-inter4 r1: superblock-level inter `PARTITION_HORZ`/`PARTITION_VERT`
    /// strips (64x32 / 32x64) and their 1:4 pair (64x16 / 16x64).
    pub(super) static INTER_SB_HORZ_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static INTER_SB_VERT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static INTER_SB_HORZ4_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static INTER_SB_VERT4_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// How many 32x32-level 1:4 strips actually read coefficients (non-skip):
// a gate that only counted strips would pass on an all-skip stream, which
// exercises none of the new scan/context tables.
thread_local! {
    pub(super) static RECT4_COEFF_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// lane-rectsplitx r1: 1:4 strips (32x8/8x32/64x16/16x64) decoded through the
// per-transform-unit split path, split by tx depth -- depth 1 is a 2:1 RECT
// unit (TX_16X8/TX_32X16), depth 2 a square one, and the two INTERLEAVE in
// every real stream, so a gate must prove both fired. `SUB16_SPLIT_HITS`
// counts the same for a 16x8/8x16 strip below 16x16.
thread_local! {
    pub(super) static RECT4_SPLIT_DEPTH1_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static RECT4_SPLIT_DEPTH2_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static SUB16_SPLIT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current values of `RECT4_SPLIT_DEPTH1_HITS` / `RECT4_SPLIT_DEPTH2_HITS`.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn rect4_split_depth_hits() -> (usize, usize) {
    (
        RECT4_SPLIT_DEPTH1_HITS.with(|c| c.get()),
        RECT4_SPLIT_DEPTH2_HITS.with(|c| c.get()),
    )
}

/// Current value of `SUB16_SPLIT_HITS`.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn sub16_split_hits() -> usize {
    SUB16_SPLIT_HITS.with(|c| c.get())
}

// lane-frame36 r2: superblock-level strips ([`decode_block_rect64`]) whose
// luma transform is SPLIT and which therefore leave through that fn's early
// return -- the arm that published no `TXFM_CONTEXT` band until this round.
thread_local! {
    pub(super) static RECT64_SPLIT_TXFM_PUBLISH_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of `RECT64_SPLIT_TXFM_PUBLISH_HITS`.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn rect64_split_txfm_publish_hits() -> usize {
    RECT64_SPLIT_TXFM_PUBLISH_HITS.with(|c| c.get())
}

/// Current value of `RECT4_COEFF_HITS`.
pub(crate) fn rect4_coeff_hits() -> usize {
    RECT4_COEFF_HITS.with(|c| c.get())
}

// lane-sbab r1: how many superblock-level (64x64) AB partitions fired IN AN
// INTER frame, per arm, in `PARTITION_HORZ_A`, `_HORZ_B`, `_VERT_A`,
// `_VERT_B` order (the intra-frame counterpart is `SB_AB_HITS`). Each arm is
// two 32x32 inter squares plus one true 64x32/32x64 inter strip; the gate
// asserts the arms it proves separately so a run whose RD only picked one
// cannot pass as proof of the others.
thread_local! {
    pub(super) static SB_AB_INTER_HITS: std::cell::Cell<[usize; 4]> = const { std::cell::Cell::new([0; 4]) };
}

/// [`SB_AB_INTER_HITS`] per arm (HORZ_A, HORZ_B, VERT_A, VERT_B at 64x64 in an
/// inter frame).
pub fn sb_ab_inter_hits_by_arm() -> [usize; 4] {
    SB_AB_INTER_HITS.with(std::cell::Cell::get)
}

// lane-inter16ab r1: how many 16x16-level AB partitions fired IN AN INTER
// frame, per arm, in `PARTITION_HORZ_A`, `_HORZ_B`, `_VERT_A`, `_VERT_B`
// order (the intra-frame counterpart is `AB16_HITS`). Each arm is two 8x8
// inter leaves (`decode_inter_block8`) plus one true 16x8/8x16 inter strip
// (`decode_inter_block` with `write_w`/`write_h`); the gate asserts every
// arm separately so a run whose RD only picked one cannot pass as proof of
// four.
thread_local! {
    pub(super) static AB16_INTER_HITS: std::cell::Cell<[usize; 4]> = const { std::cell::Cell::new([0; 4]) };
}

/// [`AB16_INTER_HITS`] per arm (HORZ_A, HORZ_B, VERT_A, VERT_B at 16x16 in an
/// inter frame).
pub fn ab16_inter_hits_by_arm() -> [usize; 4] {
    AB16_INTER_HITS.with(std::cell::Cell::get)
}

// lane-inter16ab r2: the 16x16-level 1:4 pair as INTER blocks -- four 16x4
// (`PARTITION_HORZ_4`) or 4x16 (`PARTITION_VERT_4`) strips. `CHROMA_PAIRS`
// counts the odd strips that close a 4:2:0 chroma pair
// (`is_chroma_reference`, av1_common_int.h:1454: bh == 1 mi => odd mi_row),
// `SUB8_PIECES` the pair chroma blocks whose TOP (LEFT) half was built from
// the PREVIOUS strip's own mv/ref/filters -- libaom
// `build_inter_predictors_sub8x8` (reconinter_template.inc:87-160), reached
// through `is_sub8x8_inter` (same file, :54).
thread_local! {
    pub(super) static INTER16_HORZ4_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static INTER16_VERT4_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static INTER16_CHROMA_PAIR_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static INTER16_SUB8_PIECE_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// lane-arfchroma: chroma transform-unit publications whose block started
// MID-CELL -- the condition `record_mi_chroma`'s origin alignment exists for.
// A 4x4 chroma unit is `1 << ss` luma mi per axis, so a block whose mi origin
// is not step-aligned (a 16x4 / 4x16 1:4 strip at 4:2:0 has an ODD mi row or
// column) publishes a context cell whose origin is the step-aligned mi BELOW
// or RIGHT of its own. Counted per publication, `[row_misaligned,
// col_misaligned, both]`, so a gate can prove the witness really walks the
// realigned path instead of passing on a stream whose chroma units all happen
// to start on a cell boundary.
thread_local! {
    pub(super) static CHROMA_MIDCELL_PUB_HITS: std::cell::Cell<[usize; 3]> =
        const { std::cell::Cell::new([0; 3]) };
}

/// ChROMA publications that started mid-cell: `[row, col, both]`. See
/// [`CHROMA_MIDCELL_PUB_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub fn chroma_midcell_pub_hits() -> [usize; 3] {
    CHROMA_MIDCELL_PUB_HITS.with(std::cell::Cell::get)
}

/// Zeroes [`CHROMA_MIDCELL_PUB_HITS`] so a gate can measure its own decode.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub fn reset_chroma_midcell_pub_hits() {
    CHROMA_MIDCELL_PUB_HITS.with(|c| c.set([0; 3]));
}

// lane-intra16x4: INTRA-coded strips of an inter 16x16-level 1:4 partition --
// [0] 16x4, [1] 4x16, [2] the odd (chroma-reference) ones, which carry the
// pair's single 8x4 (4x8) chroma unit for BOTH strips.
thread_local! {
    pub(super) static INTRA16X4_IN_INTER_HITS: std::cell::Cell<[usize; 3]> =
        const { std::cell::Cell::new([0; 3]) };
}

/// `(intra 16x4 strips, intra 4x16 strips, of which chroma-reference)` decoded
/// inside an INTER 16x16-level 1:4 partition (lane-intra16x4).
pub fn intra16x4_in_inter_hits() -> (usize, usize, usize) {
    let v = INTRA16X4_IN_INTER_HITS.with(std::cell::Cell::get);
    (v[0], v[1], v[2])
}

/// Current values of the inter 16x16-level 1:4 counters: `(16x4 strips, 4x16
/// strips, chroma pairs closed, chroma pairs built from two different
/// strips' motion vectors)`.
pub fn inter16_rect4_counters() -> (usize, usize, usize, usize) {
    (
        INTER16_HORZ4_HITS.with(std::cell::Cell::get),
        INTER16_VERT4_HITS.with(std::cell::Cell::get),
        INTER16_CHROMA_PAIR_HITS.with(std::cell::Cell::get),
        INTER16_SUB8_PIECE_HITS.with(std::cell::Cell::get),
    )
}

// lane-r14: the 16x16-level 1:4 pair -- four 16x4 (`PARTITION_HORZ_4`) or
// 4x16 (`PARTITION_VERT_4`) strips, `CHROMA` counting the odd strips that
// close a 4:2:0 chroma pair and `COEFF` the strips with a coded residual.
thread_local! {
    pub(super) static RECT4_16_HORZ_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static RECT4_16_VERT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static RECT4_16_CHROMA_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static RECT4_16_COEFF_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static RECT4_16_SPLIT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-kf1200: pairs whose chroma edge-filter type read from the PAIR's
    /// base mi differs from what the strip's own (odd) mi would have said --
    /// the shape that made 4 chroma samples of the ReelB `-ss 1200`
    /// key frame miss by 1. Zero here means a gate never exercised the fix.
    pub(super) static RECT4_16_UV_PAIR_FILT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1-rect14: lossless 16x4/4x16 chroma pairs walked as two TX_4X4s.
    pub(super) static RECT4_16_LOSSLESS_CHROMA_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1-rect14: intrabc reconstructions of a 16x4/4x16 pair strip.
    pub(super) static RECT4_16_INTRABC_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-kf1200 r2: sub-8x8 chroma blocks that are `skip_txfm` AND
    /// `UV_CFL_PRED` -- the arm that used to drop the CfL alpha entirely.
    pub(super) static SKIP_CFL_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1-loss64: chroma transform units the frame-edge clip skipped
    /// (libaom `decode_reconstruct_tx`'s `max_blocks_wide/high` test) in
    /// [`decode_block`]'s multi-transform-unit walk -- the units a lossless
    /// block overhanging the frame does NOT code.
    pub(super) static CHROMA_EDGE_TU_CLIP_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1-loss64: skipped lossless intrabc rect strips whose luma
    /// entropy bands were zeroed per unit (libaom `av1_reset_entropy_context`,
    /// decodeframe.c:1262) instead of leaking the previous occupant's.
    pub(super) static SKIP_LOSSLESS_BAND_RESET_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1-llband: 4x4 chroma transform units a lossless HORZ/VERT strip
    /// walked at subsampling 0/0 (one unit per mi cell -- the geometry the
    /// hardcoded 4:2:0 span mis-stepped).
    pub(super) static RECT_SPLIT_LOSSLESS_CHROMA444_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1-ibc444rect: unskipped intrabc 8x4/4x8 leaves whose luma
    /// coefficients a lossless frame walked as per-4x4 inter units (the walk
    /// that replaced the single rect unit).
    pub(super) static INTRABC_RECT8_LOSSLESS_LUMA_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1-ibc444rect: unskipped intrabc 8x4/4x8 leaves whose chroma a
    /// lossless 4:4:4 frame walked as per-4x4 [`TxbSet::Chroma4`] units.
    pub(super) static INTRABC_RECT8_LOSSLESS_CHROMA_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1loss444kf: 4:4:4 lossless sub-8x8 RECT (8x4 / 4x8) leaf chroma
    /// units whose libaom-faithful block-relative reach
    /// ([`Reach::of_tu`] over the leaf's own bsize) differs from the
    /// standalone-BLOCK_4X4 lookup that used to answer for them. Zero means
    /// no stream reached a rect leaf where the two disagree -- a gate
    /// asserting the reach fix must see this nonzero, or the fixture never
    /// exercised the fixed path.
    pub(super) static SUB8_CHROMA444_LOSSLESS_TU_REACH_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1offtile: 4x4 chroma units a lossless HORZ/VERT strip's plane-
    /// major walk skipped because their top-left sample falls outside the
    /// CHROMA plane. libaom bounds that walk per plane (`max_block_wide/high`,
    /// av1_common_int.h:1565); this walk used to count the block's own
    /// `chroma_w / 4` x `chroma_h / 4`. Zero means no stream decoded a
    /// lossless strip overhanging the frame edge -- a gate asserting the
    /// clip must see this nonzero.
    pub(super) static RECT_SPLIT_LOSSLESS_CHROMA_OFFTILE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// How many skipped sub-8x8 chroma blocks carried a CfL alpha (the shape
/// that reconstructed as flat DC before lane-kf1200 r2).
pub fn skip_cfl_hits() -> usize {
    SKIP_CFL_HITS.with(|c| c.get())
}

/// How many chroma transform units the frame-edge clip skipped in
/// [`decode_block`]'s multi-transform-unit walk (lane-av1-loss64). Zero on a
/// stream whose lossless blocks never overhang the frame -- a gate asserting
/// the clip fired must see this nonzero.
pub fn chroma_edge_tu_clip_hits() -> usize {
    CHROMA_EDGE_TU_CLIP_HITS.with(|c| c.get())
}

/// How many skipped lossless intrabc rect strips had their luma entropy
/// bands zeroed per unit (lane-av1-loss64).
pub fn skip_lossless_band_reset_hits() -> usize {
    SKIP_LOSSLESS_BAND_RESET_HITS.with(|c| c.get())
}

/// How many 4x4 chroma transform units a lossless HORZ/VERT strip walked at
/// subsampling 0/0 (lane-av1-llband). Zero means a stream never decoded the
/// 4:4:4 lossless unit walk this counter proves -- a gate asserting the fix
/// must see this nonzero.
pub fn rect_split_lossless_chroma444_hits() -> usize {
    RECT_SPLIT_LOSSLESS_CHROMA444_HITS.with(|c| c.get())
}

/// How many 4x4 chroma units a lossless HORZ/VERT strip's plane-major walk
/// skipped for falling outside the chroma plane (lane-av1offtile). Zero means
/// no stream decoded a lossless strip overhanging the frame edge -- a gate
/// asserting the clip must see this nonzero.
pub fn rect_split_lossless_chroma_offtile_hits() -> usize {
    RECT_SPLIT_LOSSLESS_CHROMA_OFFTILE_HITS.with(|c| c.get())
}

/// Reset [`rect_split_lossless_chroma_offtile_hits`] to zero (lane-av1offtile).
pub fn reset_rect_split_lossless_chroma_offtile_hits() {
    RECT_SPLIT_LOSSLESS_CHROMA_OFFTILE_HITS.with(|c| c.set(0));
}

/// How many unskipped intrabc 8x4/4x8 leaves a lossless frame walked luma as
/// per-4x4 inter units (lane-av1-ibc444rect).
pub fn intrabc_rect8_lossless_luma_hits() -> usize {
    INTRABC_RECT8_LOSSLESS_LUMA_HITS.with(|c| c.get())
}

/// How many unskipped intrabc 8x4/4x8 leaves a lossless 4:4:4 frame walked
/// chroma as per-4x4 [`TxbSet::Chroma4`] units (lane-av1-ibc444rect).
pub fn intrabc_rect8_lossless_chroma_hits() -> usize {
    INTRABC_RECT8_LOSSLESS_CHROMA_HITS.with(|c| c.get())
}

/// How many 4:4:4 lossless sub-8x8 RECT leaf chroma units took the
/// block-relative reach libaom's `has_bottom_left` gives, where the
/// standalone-BLOCK_4X4 lookup that used to answer disagreed
/// (lane-av1loss444kf). Zero on a stream with no such leaf.
pub fn sub8_chroma444_lossless_tu_reach_hits() -> usize {
    SUB8_CHROMA444_LOSSLESS_TU_REACH_HITS.with(|c| c.get())
}

/// Gate-side reset for [`SUB8_CHROMA444_LOSSLESS_TU_REACH_HITS`].
pub fn reset_sub8_chroma444_tu_reach_hits() {
    SUB8_CHROMA444_LOSSLESS_TU_REACH_HITS.with(|c| c.set(0));
}

/// Gate-side reset for the two lane-av1-ibc444rect counters above.
pub fn reset_ibc444rect_hits() {
    INTRABC_RECT8_LOSSLESS_LUMA_HITS.with(|c| c.set(0));
    INTRABC_RECT8_LOSSLESS_CHROMA_HITS.with(|c| c.set(0));
}

/// Gate-side reset for the two lane-av1-loss64 counters above.
pub fn reset_loss64_hits() {
    CHROMA_EDGE_TU_CLIP_HITS.with(|c| c.set(0));
    SKIP_LOSSLESS_BAND_RESET_HITS.with(|c| c.set(0));
}

/// How many 16x4/4x16 chroma pairs took a different `intra_edge_filter_type`
/// from the pair's base mi than the strip's own mi would have given.
pub fn rect4_16_uv_pair_filt_hits() -> usize {
    RECT4_16_UV_PAIR_FILT_HITS.with(|c| c.get())
}

/// How many lossless 16x4/4x16 chroma pairs walked two TX_4X4s per plane.
pub fn rect4_16_lossless_chroma_hits() -> usize {
    RECT4_16_LOSSLESS_CHROMA_HITS.with(|c| c.get())
}

/// How many 16x4/4x16 strips reconstructed as intra block copy.
pub fn rect4_16_intrabc_hits() -> usize {
    RECT4_16_INTRABC_HITS.with(|c| c.get())
}

/// Zero the lane-av1-rect14 pair counters so a gate measures one stream.
pub fn reset_rect4_16_pair_hits() {
    RECT4_16_LOSSLESS_CHROMA_HITS.with(|c| c.set(0));
    RECT4_16_INTRABC_HITS.with(|c| c.set(0));
}

// lane-reelckf r1: the two chroma defects ReelC's key frames exposed --
// CHROMA_SKIP_CFL counts skipped intra blocks whose chroma is `UV_CFL_PRED`
// (prediction still runs, libaom `predict_and_reconstruct_intra_block`), and
// RECT4_16_CHROMA_DIR counts 1:4 chroma pairs predicted directionally (whose
// intra-edge filter type must come from the chroma-reference position).
thread_local! {
    pub(super) static CHROMA_SKIP_CFL_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static RECT4_16_CHROMA_DIR_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `(skipped UV_CFL_PRED blocks, directional 1:4 chroma pairs)` -- lane-reelckf.
pub fn reelc_chroma_counters() -> (usize, usize) {
    (
        CHROMA_SKIP_CFL_HITS.with(|c| c.get()),
        RECT4_16_CHROMA_DIR_HITS.with(|c| c.get()),
    )
}

/// Current values of the 16x16-level 1:4 counters: `(16x4 strips, 4x16
/// strips, chroma-reference strips, strips with a coded residual, strips
/// whose luma transform is split)`.
pub fn rect4_16_counters() -> (usize, usize, usize, usize, usize) {
    (
        RECT4_16_HORZ_HITS.with(|c| c.get()),
        RECT4_16_VERT_HITS.with(|c| c.get()),
        RECT4_16_CHROMA_HITS.with(|c| c.get()),
        RECT4_16_COEFF_HITS.with(|c| c.get()),
        RECT4_16_SPLIT_HITS.with(|c| c.get()),
    )
}

/// Current value of `RECT4_32_HORZ_HITS` (32x8 strips decoded).
pub(crate) fn rect4_32_horz_hits() -> usize {
    RECT4_32_HORZ_HITS.with(|c| c.get())
}

/// Current value of `RECT4_32_VERT_HITS` (8x32 strips decoded).
pub(crate) fn rect4_32_vert_hits() -> usize {
    RECT4_32_VERT_HITS.with(|c| c.get())
}

/// Current value of `RECT4_32_HORZ_INTER_HITS` (32x8 inter strips decoded).
pub(crate) fn rect4_32_horz_inter_hits() -> usize {
    RECT4_32_HORZ_INTER_HITS.with(|c| c.get())
}

/// Current value of `RECT4_32_VERT_INTER_HITS` (8x32 inter strips decoded).
pub(crate) fn rect4_32_vert_inter_hits() -> usize {
    RECT4_32_VERT_INTER_HITS.with(|c| c.get())
}

/// Current value of `INTER_SB_HORZ_HITS` (64x32 inter strips decoded).
pub(crate) fn inter_sb_horz_hits() -> usize {
    INTER_SB_HORZ_HITS.with(|c| c.get())
}

/// Current value of `INTER_SB_VERT_HITS` (32x64 inter strips decoded).
pub(crate) fn inter_sb_vert_hits() -> usize {
    INTER_SB_VERT_HITS.with(|c| c.get())
}

/// Current value of `INTER_SB_HORZ4_HITS` (64x16 inter strips decoded).
pub(crate) fn inter_sb_horz4_hits() -> usize {
    INTER_SB_HORZ4_HITS.with(|c| c.get())
}

/// Current value of `INTER_SB_VERT4_HITS` (16x64 inter strips decoded).
pub(crate) fn inter_sb_vert4_hits() -> usize {
    INTER_SB_VERT4_HITS.with(|c| c.get())
}

// lane-rect64q r1: how many of [`decode_block_rect64`]'s three per-plane
// dequant calls actually observed `CURRENT_Q_IDX != base_q_idx` -- proof the
// running-vs-stale-snapshot bug this round fixed is exercised by a gate, not
// just inert-compiled. Zero forever on a passing gate means that gate's
// stream never carries `delta_q_present=1` through a rect64 block.
thread_local! {
    pub(super) static RECT64_QIDX_DRIFT_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT64_QIDX_DRIFT_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn rect64_qidx_drift_hits() -> usize {
    RECT64_QIDX_DRIFT_HITS.with(|c| c.get())
}

// lane-partab r1: how many 32x32 quadrants decoded an AB partition
// (PARTITION_HORZ_A / VERT_A / VERT_B -- two 16x16 squares plus one
// 16x32/32x16 strip). Like [`RECT_PARTITION_HITS`], this is the
// free-partition gate's proof this round's arms actually fired.
thread_local! {
    pub(super) static PARTAB_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`PARTAB_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn partab_hits() -> usize {
    PARTAB_HITS.with(|c| c.get())
}

// lane-part32 r1: how many 32x32 quadrants decoded each INTRA AB/4-way arm,
// one counter per arm rather than one shared total -- the gate hard-asserts
// each of these individually nonzero so a recipe that only ever fires
// HORZ_A can't silently stand in for the whole set.
thread_local! {
    pub(super) static INTRA_HORZ_A_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    pub(super) static INTRA_HORZ_B_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    pub(super) static INTRA_VERT_A_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    pub(super) static INTRA_VERT_B_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`INTRA_HORZ_A_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn intra_horz_a_hits() -> usize {
    INTRA_HORZ_A_HITS.with(|c| c.get())
}

/// Current value of [`INTRA_HORZ_B_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn intra_horz_b_hits() -> usize {
    INTRA_HORZ_B_HITS.with(|c| c.get())
}

/// Current value of [`INTRA_VERT_A_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn intra_vert_a_hits() -> usize {
    INTRA_VERT_A_HITS.with(|c| c.get())
}

/// Current value of [`INTRA_VERT_B_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
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
    pub(super) static COMPOUND_MODE_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`COMPOUND_MODE_HITS`].
#[allow(dead_code)] // reader is test-only (the 12-bit compound witness); dead in non-test builds
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
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn tmv_hits() -> usize {
    crate::mvstack::tmv_hits()
}

/// Current value of [`crate::mc::inter_pred_hits`] -- inter predictions
/// produced on this thread, the 10-bit inter gate's firing proof.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn inter_pred_hits() -> usize {
    crate::mc::inter_pred_hits()
}

/// Current value of the counter for `ref_frame` (`LAST2_FRAME`..=`ALTREF_FRAME`;
/// panics on `LAST_FRAME`/`INTRA_FRAME`/`NONE`, which have no counter here).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
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

/// Current value of [`ANGLE_DELTA_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn angle_delta_hits() -> usize {
    ANGLE_DELTA_HITS.with(|c| c.get())
}

/// Current value of [`TX_CLASS1_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
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
    pub(super) static SUB8_SPLIT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// How many 8x8 groups an INTER frame split into four BLOCK_4X4 inter
    /// sub-blocks (lane-intersub8).
    pub(super) static SUB8_INTER_SPLIT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// How many 8x8 groups an INTER frame split into two rectangular sub-8x8
    /// inter leaves (lane-intersub8 r3), indexed `[0] = HORZ` (two BLOCK_8X4),
    /// `[1] = VERT` (two BLOCK_4X8).
    pub(super) static SUB8_INTER_RECT_HITS: std::cell::Cell<[usize; 2]> = const { std::cell::Cell::new([0; 2]) };
    /// lane-sub8intra: INTRA leaves decoded inside an inter frame's sub-8x8
    /// HORZ/VERT partition -- `[0]` 8x4, `[1]` 4x8, `[2]` of those that were
    /// the chroma reference (so coded the group's 4x4 chroma intra), `[3]`
    /// MIXED groups whose chroma reference is INTER while its sibling is
    /// intra (the `is_sub8x8_inter` false path), `[4]` INTRA `BLOCK_4X4`
    /// leaves of an inter frame's sub-8x8 `PARTITION_SPLIT` (lane-sub8x4 r2 --
    /// the second lifted refusal's own witness; `[0]`/`[1]` count those too,
    /// so only this cell separates the split arm from the HORZ/VERT one).
    pub(super) static SUB8_INTRA_RECT_HITS: std::cell::Cell<[usize; 5]> = const { std::cell::Cell::new([0; 5]) };
}

/// Current value of [`SUB8_INTRA_RECT_HITS`]: `(8x4, 4x8, chroma-reference,
/// mixed groups, split 4x4 leaves)`.
pub fn sub8_intra_rect_hits() -> (usize, usize, usize, usize, usize) {
    SUB8_INTRA_RECT_HITS.with(|c| {
        let h = c.get();
        (h[0], h[1], h[2], h[3], h[4])
    })
}

/// Current value of [`SUB8_INTER_RECT_HITS`]: `(8x4 groups, 4x8 groups)`.
pub fn sub8_inter_rect_hits() -> (usize, usize) {
    SUB8_INTER_RECT_HITS.with(|c| {
        let h = c.get();
        (h[0], h[1])
    })
}

// lane-cdef r1: (a) how many 8x8 CDEF units `apply_cdef` EXCLUDED because all
// four of their mi cells are `skip_txfm` (libaom `cdef.c:29-38`
// `is_8x8_block_skip` / `av1_cdef_compute_sb_list`'s dlist test), and (b) how
// many 8x8 inter leaves (`decode_inter_block8`) wrote that skip band at all.
// (b) was 0 for every stream before this round -- the leaf path never touched
// the band, so an all-skip 8x8 group kept the PREVIOUS frame's flags and got
// filtered where libaom skips it. A gate asserts both are non-zero.
thread_local! {
    pub(super) static INTER8_SKIP_BAND_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // lane-cdefstrip r1: (d) skip RECTANGULAR blocks (every strip arm: 32x8,
    // 8x32, 16x8, 32x16, 64x16, AB/1:4 pieces...) that wrote the CDEF skip
    // band, and (e) 8x8 CDEF units whose four mi cells do NOT agree on skip --
    // the shape libaom's `is_8x8_block_skip` (`cdef.c:29-38`) exists for: an
    // 8-tall strip pair where only one half is skip must still be FILTERED.
    pub(super) static RECT_SKIP_BAND_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of `RECT_SKIP_BAND_HITS`.
pub fn rect_skip_band_hits() -> usize {
    RECT_SKIP_BAND_HITS.with(|c| c.get())
}

/// Current value of `INTER8_SKIP_BAND_HITS`.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn inter8_skip_band_hits() -> usize {
    INTER8_SKIP_BAND_HITS.with(|c| c.get())
}

/// Current value of [`SUB8_SPLIT_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn sub8_split_hits() -> usize {
    SUB8_SPLIT_HITS.with(|c| c.get())
}

/// Current value of [`SUB8_INTER_SPLIT_HITS`].
pub fn sub8_inter_split_hits() -> usize {
    SUB8_INTER_SPLIT_HITS.with(|c| c.get())
}

// lane-t900 r7: sub-8x8 inter groups whose one chroma unit inherits a
// `tx_type` DIFFERENT from the group's FIRST sub-block -- the only shape that
// tells the two rules apart (`av1_get_tx_type` indexes `xd->tx_type_map`,
// which `set_mi_offsets` re-points to the CHROMA-REFERENCE sub-block, so the
// first sub-block's type is never the one). Reading it off sub-block 0
// inverted one 4x4 chroma unit on the wrong basis with the entropy stream
// perfectly in sync (no chroma `tx_type` symbol is coded), so a gate can only
// see this rule through this counter.
thread_local! {
    pub(super) static SUB8_CHROMA_TX_FROM_REF_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of `SUB8_CHROMA_TX_FROM_REF_HITS`.
pub fn sub8_chroma_tx_from_ref_hits() -> usize {
    SUB8_CHROMA_TX_FROM_REF_HITS.with(|c| c.get())
}

// How many 4x8 / 8x4 leaves (lane-tx4x8's `PARTITION_VERT`/`PARTITION_HORZ`
// of an 8x8 block) this thread's decode reconstructed with REAL coefficients
// -- a skipped leaf exercises the prediction and the mi bookkeeping but never
// the rect transform, the 32-position `eob_pt` or the rect scan, so the gate
// asserts on these two rather than a plain leaf count.
thread_local! {
    pub(super) static TX4X8_CODED_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static TX8X4_CODED_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Sub-8x8 rect leaves whose `tx_depth` symbol resolved to 1 -- the leaf's
    /// two 4x4 transform units, predicted one after the other. A separate
    /// counter from [`TX_DEPTH_HITS`] (which every square path also bumps) so
    /// the gate can prove THIS path fired.
    pub(super) static RECT8_SPLIT_TX_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// 16x8/8x16 intra strips whose prediction actually read past the block's
    /// own width (above-right) or height (below-left) -- the samples
    /// `Reach::of_rect` decides on, and the only ones a wrong `has_tr_*`/
    /// `has_bl_*` table row can corrupt (lane-tx4x8 r3).
    pub(super) static RECT_STRIP_REACH_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Every 16x8/8x16 intra strip predicted, reaching or not -- the
    /// denominator [`RECT_STRIP_REACH_HITS`] is a fraction of.
    pub(super) static RECT_STRIP_PRED_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT8_SPLIT_TX_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn rect8_split_tx_hits() -> usize {
    RECT8_SPLIT_TX_HITS.with(|c| c.get())
}

/// Current value of [`RECT_STRIP_PRED_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn rect_strip_pred_hits() -> usize {
    RECT_STRIP_PRED_HITS.with(|c| c.get())
}

/// Current value of [`RECT_STRIP_REACH_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn rect_strip_reach_hits() -> usize {
    RECT_STRIP_REACH_HITS.with(|c| c.get())
}

/// Current value of [`TX4X8_CODED_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn tx4x8_coded_hits() -> usize {
    TX4X8_CODED_HITS.with(|c| c.get())
}

/// Current value of [`TX8X4_CODED_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn tx8x4_coded_hits() -> usize {
    TX8X4_CODED_HITS.with(|c| c.get())
}

// How many CHROMA coefficient blocks read their `eob_pt` from the class-1
// (`TX_CLASS_HORIZ`/`VERT`) row, across every call on the current thread.
// Chroma never codes a `tx_type` symbol -- it inherits the colocated luma
// type verbatim (`av1_get_tx_type`, `blockd.h:1291`) -- so this fires only on
// an inter leaf whose luma picked a 1D type, the path lane-eobc1 wired
// (`eob_pt_{16,64,128,256}_chroma_class1`).
thread_local! {
    pub(super) static CHROMA_EOB_CLASS1_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`CHROMA_EOB_CLASS1_HITS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn chroma_eob_class1_hits() -> usize {
    CHROMA_EOB_CLASS1_HITS.with(|c| c.get())
}

/// Enabled by `EC_AV1_PLANE_SENTINEL` (there is no separate flag for it): print
/// an EXACT census of the samples the tile walk never wrote -- with it, every
/// plane still holding [`plane_poison`] at this point is a sample no block
/// reconstructed, and no legal sample can equal the poison (reconstruction
/// clamps to `(1 << bit_depth) - 1`, at most 4095).
///
/// Coverage, stated as the code has it: this census runs at TWO of the four
/// places [`apply_deblock`] is entered from -- the key-frame path and the
/// non-pipelined inter path, both immediately before that frame's deblock. The
/// two it does NOT cover are the encoder's filter search ([`replay_filters`],
/// which runs the filters over a CAPTURED copy, not a live decode) and the
/// pipelined inter branch, where deblock has already run band by band inside
/// `par` by the time this tail is reached. A zero count therefore means "no hole
/// on the paths that reach this call", not "no hole on any path". It is EXACT
/// over the extent it scans -- the scan itself reads every sample of that
/// extent, unlike the older [`dump_stage`] pair, which truncates to `as u8`.
///
/// This is the shipped form of the four-line temporary scan the
/// `lane/av1422seed` census used: it replaces a wrong-sample diff (which says
/// where the OUTPUT is wrong, not whether the block was written at all) with a
/// direct count plus the coordinates of every hole, grouped into per-row
/// spans so a large hole stays readable.
///
/// It scans TWO regions per plane, because they answer different questions and
/// the earlier revision only scanned one of them:
/// * the TRUE EXTENT -- each plane over its own `true_width` x `true_height`,
///   PER AXIS (`true_width.min(p.width)`), so a 4:2:2 frame's chroma is scanned
///   at its own `fw/2 x fh` shape rather than the 4:2:0 `div_ceil(2)` crop the
///   older [`dump_stage16`] used. A hole here can reach the caller, and it is
///   the region lane/av1unwritten's 224 samples sat in.
/// * the PADDED TAIL -- everything outside that extent, out to the block-aligned
///   coding surface -- counted separately into [`PAD_UNWRITTEN_SAMPLES`]. The
///   earlier revision's doc comment asserted "past that is the padded coding
///   surface's invented tail, which no decoder ever reads". That assertion is
///   exactly what a tail that is never written and never read looks like, so it
///   was self-certifying; the tail is now scanned and counted like any other
///   region. `EC_AV1_PLANE_ALLOC=padzero`/`extzero` attribute a read to one
///   side or the other (see [`fresh_plane`]).
pub(super) fn census_unwritten(y: &PlaneBuf<'_>, u: &PlaneBuf<'_>, v: &PlaneBuf<'_>) {
    if !plane_sentinel_on() {
        return;
    }
    let poison = plane_poison();
    let idx = PREFILT_PICTURE_IDX.load(std::sync::atomic::Ordering::SeqCst);
    for (name, p) in [("Y", y), ("U", u), ("V", v)] {
        let (w, h) = (p.true_width.min(p.width), p.true_height.min(p.height));
        let mut total = 0usize;
        let mut spans: Vec<(usize, usize, usize)> = Vec::new();
        for row in 0..h {
            let mut run: Option<(usize, usize)> = None;
            for col in 0..w {
                if p.data[row * p.width + col] == poison {
                    total += 1;
                    run = Some(match run {
                        None => (col, col),
                        Some((a, _)) => (a, col),
                    });
                }
                if run.is_some() && (col + 1 == w || p.data[row * p.width + col + 1] != poison) {
                    let (a, b) = run.take().expect("a run is open here");
                    spans.push((row, a, b));
                }
            }
        }
        // The padded tail: columns `w..width` on rows `0..h`, plus every column
        // on rows `h..height`.
        let mut pad_total = 0usize;
        let mut pad_scanned = 0usize;
        for row in 0..p.height {
            let (from, to) = if row < h { (w, p.width) } else { (0, p.width) };
            if from >= to {
                continue;
            }
            pad_scanned += to - from;
            pad_total += p.data[row * p.width + from..row * p.width + to]
                .iter()
                .filter(|&&s| s == poison)
                .count();
        }
        UNWRITTEN_SAMPLES.fetch_add(total, std::sync::atomic::Ordering::SeqCst);
        CENSUS_SCANNED.fetch_add(w * h, std::sync::atomic::Ordering::SeqCst);
        PAD_UNWRITTEN_SAMPLES.fetch_add(pad_total, std::sync::atomic::Ordering::SeqCst);
        PAD_CENSUS_SCANNED.fetch_add(pad_scanned, std::sync::atomic::Ordering::SeqCst);
        eprintln!(
            "SENTINEL_CENSUS idx={idx} plane={name} surface={}x{} extent={w}x{h} \
             pad_scanned={pad_scanned} unwritten={total} pad_unwritten={pad_total}",
            p.width, p.height,
        );
        if total == 0 {
            continue;
        }
        let body: Vec<String> = spans
            .iter()
            .map(|&(r, a, b)| format!("y{r} x{a}..={b}"))
            .collect();
        eprintln!(
            "SENTINEL_CENSUS idx={idx} plane={name} surface={}x{} extent={w}x{h} \
             unwritten={total} spans={}: {}",
            p.width,
            p.height,
            spans.len(),
            body.join(" ")
        );
    }
}

/// Samples [`census_unwritten`] has looked at since the last
/// [`take_census_scanned`]. A gate that only asserts
/// [`take_unwritten_samples`] == 0 passes on a decode that never scanned
/// anything, so it reads THIS too: a census that scanned nothing has proved
/// nothing.
pub(super) static CENSUS_SCANNED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Reads and clears [`CENSUS_SCANNED`]; see [`take_unwritten_samples`] for the
/// pairing.
pub fn take_census_scanned() -> usize {
    CENSUS_SCANNED.swap(0, std::sync::atomic::Ordering::SeqCst)
}

/// Samples [`census_unwritten`] scanned in the padded tail, paired with
/// [`PAD_UNWRITTEN_SAMPLES`] for the same reason as [`CENSUS_SCANNED`].
pub(super) static PAD_CENSUS_SCANNED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Reads and clears [`PAD_CENSUS_SCANNED`].
pub fn take_pad_census_scanned() -> usize {
    PAD_CENSUS_SCANNED.swap(0, std::sync::atomic::Ordering::SeqCst)
}

/// Samples [`census_unwritten_final`] has looked at since the last
/// [`take_final_census_scanned`]. A gate that only asserts
/// [`take_final_unwritten_samples`] == 0 passes on a build whose census call was
/// deleted, so it reads THIS too -- the same pairing
/// [`CENSUS_SCANNED`] documents for the pre-deblock census.
pub(super) static FINAL_CENSUS_SCANNED: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Reads and clears [`FINAL_CENSUS_SCANNED`]; the pairing is the reason it
/// exists, exactly as for [`take_census_scanned`].
pub fn take_final_census_scanned() -> usize {
    FINAL_CENSUS_SCANNED.swap(0, std::sync::atomic::Ordering::SeqCst)
}

/// lane-unwritten-dep: the census at the pipeline's last point. Counts, over each
/// plane, how many samples still hold [`PLANE_SENTINEL`] once every filter has
/// run, and records how many samples it LOOKED at in [`FINAL_CENSUS_SCANNED`].
///
/// SCOPE, which lane-av1graincensus did not change but had to be true about:
/// this runs BEFORE `apply_grain`, over the PRE-grain planes. It does NOT and
/// never did measure the grained picture the caller receives -- that is
/// [`census_unwritten_grained`], a separate counter over a separate buffer.
///
/// Gated on [`plane_sentinel_on`] -- the SAME predicate as [`fresh_plane`] and as
/// the pre-deblock [`census_unwritten`]: the env flag OR the thread-local
/// [`set_plane_sentinel`] override. An earlier revision gated this on the raw env
/// var alone, which made the census silently inert for any in-process test using
/// the thread-local switch that the rest of the crate honours. The fill and both
/// censuses must agree on when they are live, or a "0" means nothing.
pub(crate) fn census_unwritten_final(y: &[u16], u: &[u16], v: &[u16]) {
    let Some((scanned, total)) = census_sentinels(y, u, v) else {
        return;
    };
    FINAL_CENSUS_SCANNED.fetch_add(scanned, std::sync::atomic::Ordering::SeqCst);
    FINAL_UNWRITTEN_SAMPLES.fetch_add(total, std::sync::atomic::Ordering::SeqCst);
}

/// The `(scanned, unwritten)` pair both output-point censuses accumulate, or
/// `None` when the sentinel fill is not live.
///
/// Shared so the two censuses cannot drift into disagreeing about what counts
/// as unwritten -- the failure this crate's other paired counters exist to
/// prevent. `None` (rather than `(0, 0)`) is what keeps a caller from
/// publishing a meaningless zero when the fill was off.
fn census_sentinels(y: &[u16], u: &[u16], v: &[u16]) -> Option<(usize, usize)> {
    if !plane_sentinel_on() {
        return None;
    }
    // Both callers pass planes cropped to the true extent (no padding past it
    // reaches either point), so a flat count over each plane IS the
    // true-extent count.
    let (scanned, total): (usize, usize) = [y, u, v]
        .iter()
        .map(|p| (p.len(), p.iter().filter(|&&s| s == plane_poison()).count()))
        .fold((0, 0), |(a, b), (c, d)| (a + c, b + d));
    Some((scanned, total))
}

/// The scanned half of the pairing for [`GRAINED_UNWRITTEN_SAMPLES`]. A gate
/// asserting only `== 0` passes on a build whose census call was deleted.
pub(super) static GRAINED_CENSUS_SCANNED: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Reads and clears [`GRAINED_CENSUS_SCANNED`]; the pairing is the reason it
/// exists, exactly as for [`take_final_census_scanned`].
pub fn take_grained_census_scanned() -> usize {
    GRAINED_CENSUS_SCANNED.swap(0, std::sync::atomic::Ordering::SeqCst)
}

/// lane-av1graincensus: the SECOND output-point census -- the one
/// [`census_unwritten_final`] structurally cannot be.
///
/// # WHY IT EXISTS
///
/// `census_unwritten_final` runs at `stream.rs`'s `EC_AV1_FINAL_DUMP` site, which
/// is BEFORE `apply_grain`. Every filter (deblock/CDEF/LR/superres) writes in
/// place into the same buffer that census scanned, so those stages cannot open a
/// hole outside its extent. `apply_grain` is the ONE stage after it that
/// allocates a fresh picture -- `uninit_plane`, `Vec::with_capacity` +
/// `set_len`, no fill -- and whose soundness rests on a band row partition
/// rather than on the type system. Its destination therefore reaches the caller
/// having never been looked at by any census, and `lanes/unwritten-dep.report.md`
/// §6/§8 explicitly declines to claim otherwise.
///
/// # WHAT IT IS WORTH ONLY IF THE FILL IS LIVE
///
/// Counting `== plane_poison()` over an uninitialised buffer is a census that
/// can only ever report ~0: garbage is arbitrary, so a hole survives with
/// probability ~1/65536 rather than always. `uninit_plane` therefore fills with
/// the poison when [`plane_sentinel_on`] -- under the SAME predicate
/// [`fresh_plane`] honours -- so "the band copy wrote every sample" becomes a
/// measurement instead of an assumption. With the sentinel off (every shipped
/// build) nothing here runs and `uninit_plane` keeps its corner-cut.
pub(crate) fn census_unwritten_grained(y: &[u16], u: &[u16], v: &[u16]) {
    let Some((scanned, total)) = census_sentinels(y, u, v) else {
        return;
    };
    GRAINED_CENSUS_SCANNED.fetch_add(scanned, std::sync::atomic::Ordering::SeqCst);
    GRAINED_UNWRITTEN_SAMPLES.fetch_add(total, std::sync::atomic::Ordering::SeqCst);
}

thread_local! {
    /// lane-sb128c r8: INTRA (key-frame) 128x64 blocks decoded at the 128
    /// root's HORZ arm, and 64x128 ones at its VERT arm.
    pub(crate) static INTRA_SB128_HORZ_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static INTRA_SB128_VERT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Those blocks by resolved intra `tx_depth` (0 = TX_64X64 units,
    /// 1 = TX_32X32, 2 = TX_16X16) -- proof the depth symbol is really read.
    pub(crate) static INTRA_SB128_DEPTH_HITS: std::cell::Cell<[usize; 3]> = const { std::cell::Cell::new([0; 3]) };
    /// lane-inter128intra r1: the same shapes decoded on the INTER block path
    /// (an intra-coded PARTITION_HORZ/VERT half of a 128 superblock inside an
    /// inter frame) -- (128x64, 64x128).
    pub(crate) static INTRA128_IN_INTER_HITS: std::cell::Cell<[usize; 2]> = const { std::cell::Cell::new([0; 2]) };
    /// lane-t900 r17: skipped INTER blocks whose `read_block_tx_size` still
    /// handed back var-tx leaves, so the per-transform-unit luma publication
    /// loop never ran and the block has to clear its own luma coefficient
    /// contexts (libaom `av1_reset_entropy_context`). Every 128 root under
    /// `TX_MODE_LARGEST` is one; the counter is what keeps that reset's gate
    /// non-vacuous.
    pub(crate) static SKIP_VARTX_LUMA_RESET_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-lossless128: an intra 128-axis (BLOCK_128X64/64X128) block decoded
    /// on a LOSSLESS frame, where every plane is coded as TX_4X4 units (the
    /// frame's `tx_mode` is forced `ONLY_4X4`). Keeps the gate that owns this
    /// shape non-vacuous -- a stream that never codes one reads 0.
    pub(crate) static INTRA128_LOSSLESS_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1-128rectspan: chroma transform units of a 128-root HORZ/VERT
    /// intra block re-stamped by the end-of-block replay
    /// (`record_split_luma_rect` clobbers the per-unit contexts the chroma
    /// walk published, so every unit replays its own state after it). A
    /// non-skip 128x64/64x128 block codes `nch_x * cn * 2 + nch_y * cn_h * 2`
    /// of them, so this reads 0 on any stream that never codes one -- the
    /// non-vacuity anchor for the replay's own gate.
    pub(crate) static SB128RECT_CHROMA_REPLAY_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// ... and the number of non-skip 128-root rect BLOCKS whose two LUMA-MI
    /// spans differ (`luma_span = chroma_tx << ss_x` vs
    /// `luma_span_h = chroma_tx << ss_y`). lane-av1422lift: the doc here used
    /// to read "Zero on every stream this decoder admits ... 4:2:2 ... is
    /// refused by name at the sequence header", and that was TRUE. Admitting
    /// 4:2:2 genuinely made this counter able to fire, and it does fire:
    /// measured, `422_intrabc_sb128_strip.obu` and
    /// `422_intrabc_sb128_strip_notxsearch.obu` each read
    /// `replay = 72, mismatch = 9`; the other 19 committed 4:2:2 cells read 0.
    ///
    /// lane-av1422mismatch9: the predicate is the span DIFFERENCE, which at
    /// ss (1, 0) is true BY CONSTRUCTION for every 128-root rect block -- so
    /// this is a shape test ("a 4:2:2-shaped block replayed"), NOT a
    /// divergence test, and it does not witness the lane-av1-128rectspan
    /// defect it was named for (that defect passed `luma_span` to both axes;
    /// the source-scan arm in
    /// `the_422_block_128rect_chroma_chunk_gather_stays_per_axis` is what
    /// pins the per-axis spelling). It is not a pixel-miss signature either:
    /// the replay re-stamps with the same per-axis pair the in-loop walk
    /// stamped with, and `record_mi_chroma` writes only the `left`/`above`
    /// context arrays -- zero pixel samples. Both pins are byte-exact against
    /// ffmpeg (`the_pinned_422_intrabc_sb128_strip_witnesses_decode_pixel_
    /// exact`, all five frames, every plane).
    ///
    /// **Nothing asserts zero here, and nothing should**: a zero assert would
    /// red on a provably correct cell. The measured NINE is asserted instead,
    /// in `the_422_block_128rect_chroma_chunk_gather_stays_per_axis` (with
    /// this reasoning in its message), so a change in the count is a red gate
    /// rather than a silent drift.
    ///
    /// The pair's other reader is `sb128rect_chroma_replay_hits` on a 4:4:4
    /// stream, where the unit's luma width and height are equal so the
    /// mismatch half is structurally zero -- a zero that is a statement about
    /// that stream, not about the format.
    pub(crate) static SB128RECT_REPLAY_SPAN_MISMATCH_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// [`SKIP_VARTX_LUMA_RESET_HITS`], for a gate's own before/after delta.
pub fn skip_vartx_luma_reset_hits() -> usize {
    SKIP_VARTX_LUMA_RESET_HITS.with(std::cell::Cell::get)
}

/// [`INTRA128_LOSSLESS_HITS`], for a gate's own before/after delta.
pub(crate) fn intra128_lossless_hits() -> usize {
    INTRA128_LOSSLESS_HITS.with(std::cell::Cell::get)
}

/// [`INTRA128_IN_INTER_HITS`], for a gate's own before/after delta.
pub(crate) fn intra128_in_inter_hits() -> [usize; 2] {
    INTRA128_IN_INTER_HITS.with(std::cell::Cell::get)
}

/// [`INTRA_SB128_HORZ_HITS`] / [`INTRA_SB128_VERT_HITS`] /
/// [`INTRA_SB128_DEPTH_HITS`], for a gate's own before/after delta.
pub(crate) fn intra_sb128_hits() -> (usize, usize, [usize; 3]) {
    (
        INTRA_SB128_HORZ_HITS.with(std::cell::Cell::get),
        INTRA_SB128_VERT_HITS.with(std::cell::Cell::get),
        INTRA_SB128_DEPTH_HITS.with(std::cell::Cell::get),
    )
}

/// [`SB128RECT_CHROMA_REPLAY_HITS`] (chroma units re-stamped by
/// `decode_block_128rect`'s end-of-block replay) and
/// [`SB128RECT_REPLAY_SPAN_MISMATCH_HITS`] (replays whose stamped height was
/// not the unit's own), for a gate's own before/after delta.
pub(crate) fn sb128rect_chroma_replay_hits() -> (usize, usize) {
    (
        SB128RECT_CHROMA_REPLAY_HITS.with(std::cell::Cell::get),
        SB128RECT_REPLAY_SPAN_MISMATCH_HITS.with(std::cell::Cell::get),
    )
}

/// Records one block's shape. The table is ordered by measured frequency over
/// the three witnesses the census gate decodes, so the common square shapes
/// leave the scan on their first comparison.
pub(super) fn census_inter_block_shape(side: usize, w: usize, h: usize) {
    let bit = match INTER_BLOCK_SHAPES.iter().position(|&s| s == (side, w, h)) {
        Some(i) => 1u32 << i,
        None => INTER_BLOCK_SHAPE_UNLISTED,
    };
    let seen = inter_block_shape_mask();
    if seen & bit == 0 {
        INTER_BLOCK_SHAPE_MASK.with(|c| c.set(seen | bit));
    }
}

/// lane-cdefstrip r1 instrument, run for every frame BEFORE either of
/// [`apply_cdef`]'s early returns.
pub(super) fn cdef_scan_counters(skip_grid: &Neighbours) {
    let _ = skip_grid;
    hit_do! {
        let mut r = 0usize;
        while r < skip_grid.mi_rows {
            let mut c = 0usize;
            while c < skip_grid.mi_cols {
                if (0..2).any(|rr| {
                    (0..2).any(|cc| {
                        !skip_grid
                            .blk_grid
                            .get((r + rr) * skip_grid.skip_grid_cols_mi + (c + cc))
                            .is_none_or(|x| x.written)
                    })
                }) {
                    hit!(CDEF_UNWRITTEN_SKIP_UNITS);
                    if crate::envflags::env_flag!("EC_AV1_DEBUG_CDEF_BAND") {
                        eprintln!("cdef band unwritten at mi ({r},{c}) px ({},{})", r * 4, c * 4);
                    }
                }
                let cell = |rr: usize, cc: usize| {
                    skip_grid
                        .blk_grid
                        .get((r + rr) * skip_grid.skip_grid_cols_mi + (c + cc))
                        .is_some_and(|x| x.skip)
                };
                let first = cell(0, 0);
                if (0..2).any(|rr| (0..2).any(|cc| cell(rr, cc) != first)) {
                    hit!(CDEF_MIXED_SKIP_UNITS);
                }
                c += 2;
            }
            r += 2;
        }
    }
}

// lane-sb128 r2: the 128x128 superblock root, shared by the key-frame and
// the inter tile loop. Its loop-restoration units are read once for the whole
// 128x128 (spec 5.11.57 is called with the superblock's own size), then one
// partition symbol off the `BLOCK_128X128` CDF (8 symbols, no HORZ_4/VERT_4)
// decides the root -- only SPLIT recurses into the four 64x64 quadrants this
// decoder codes, the other seven arms refuse by name.
thread_local! {
    /// How many 128x128 superblock roots resolved to `PARTITION_NONE` -- one
    /// whole 128x128 block (lane-sb128b r1's gate counter).
    pub(crate) static PART128_NONE_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// 128 roots that resolved to PARTITION_HORZ (two 128x64 pieces, or one
    /// when the bottom half is out of frame) -- lane-sb128c r7.
    pub(crate) static PART128_HORZ_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// 128 roots that resolved to PARTITION_VERT (two 64x128 pieces).
    pub(crate) static PART128_VERT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Of those, the ones decided by the GATHERED bit at a frame edge -- the
    /// only shape a 1080p (mi_rows = 270) stream reaches.
    pub(crate) static PART128_EDGE_HORZ_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static PART128_EDGE_VERT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-sb128c r9: 128 roots that resolved to one of the four AB shapes,
    /// in `PARTITION_HORZ_A`, `_HORZ_B`, `_VERT_A`, `_VERT_B` order.
    pub(crate) static PART128_AB_HITS: std::cell::Cell<[usize; 4]> = const { std::cell::Cell::new([0; 4]) };
}

/// [`PART128_AB_HITS`], for a gate's own before/after delta.
pub(crate) fn part128_ab_hits() -> [usize; 4] {
    PART128_AB_HITS.with(std::cell::Cell::get)
}

/// [`PART128_NONE_HITS`], for a gate's own before/after delta.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn part128_none_hits() -> usize {
    PART128_NONE_HITS.with(std::cell::Cell::get)
}

/// [`PART128_HORZ_HITS`], for a gate's own before/after delta.
#[allow(dead_code)] // gate counter accessor with no reader yet; the counter itself is live
pub(crate) fn part128_horz_hits() -> usize {
    PART128_HORZ_HITS.with(std::cell::Cell::get)
}

/// [`PART128_VERT_HITS`], for a gate's own before/after delta.
#[allow(dead_code)] // gate counter accessor with no reader yet; the counter itself is live
pub(crate) fn part128_vert_hits() -> usize {
    PART128_VERT_HITS.with(std::cell::Cell::get)
}

/// [`PART128_EDGE_HORZ_HITS`] -- gathered-bit HORZ at the bottom frame edge.
pub(crate) fn part128_edge_horz_hits() -> usize {
    PART128_EDGE_HORZ_HITS.with(std::cell::Cell::get)
}

/// [`PART128_EDGE_VERT_HITS`] -- gathered-bit VERT at the right frame edge.
pub(crate) fn part128_edge_vert_hits() -> usize {
    PART128_EDGE_VERT_HITS.with(std::cell::Cell::get)
}

thread_local! {
    /// How many CHROMA transform units were coded inside a block whose uv
    /// plane block is more than one unit (lane-sb128b r2 -- only a 128x128
    /// block, four per plane).
    pub(crate) static CHROMA_SPLIT_TX_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    /// lane-av1h5: how many blocks had their leaf walk actually PERMUTED into
    /// libaom's chunk-major order by [`mu_chunk_order`] -- a block above 64
    /// whose sort was not a no-op. Deliberately not "took the order": a
    /// no-op ordering change must read 0 so a gate cannot look armed.
    pub(crate) static MU_CHUNK_ORDER_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1txctxband2: SQUARE lossless blocks on an INTER frame whose
    /// `set_txfm_ctxs` publish CHANGED at least one above-band cell it covers
    /// (the cell did not already hold the published size), so a later
    /// `get_tx_size_context` / `txfm_partition_context` reader in the same
    /// tile column reads a value this publish produced. Zero on an intra-only
    /// frame (where `fill_lf_grid_rect_inner` publishes the same 4x4 value at
    /// the same footprint).
    ///
    /// CORRECTED 2026-09-30 (refutation pass `lanes/refute-av1txctxband2`):
    /// this doc previously claimed the hit meant "overwrote a size some
    /// earlier block left" and that a wholly-lossless all-non-skip stream
    /// leaves it at zero. MEASURED FALSE -- `420_lossless_tallinter_8x16.obu`
    /// fires **48**, all of them first touches of a column still at
    /// `TXFM_CTX_INIT`; `420_lossless_arf_1to4_320x240_5f.obu` fires 320, 53 of
    /// them first touches. Of the pin's 944, **183 are first-touch**. The
    /// predicate tests "the cell did not already hold this value", which is
    /// the decision that matters (no reader, no intra-only frame, no unchanged
    /// column still reads 0), but it is NOT the strictly narrower causal claim
    /// the old text made.
    pub(crate) static LOSSLESS_SQ_TXFM_BAND_OVERWRITE_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// [`MU_CHUNK_ORDER_HITS`], for a gate's own before/after delta.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn mu_chunk_order_hits() -> usize {
    MU_CHUNK_ORDER_HITS.with(std::cell::Cell::get)
}

/// [`LOSSLESS_SQ_TXFM_BAND_OVERWRITE_HITS`] -- square lossless inter blocks
/// whose `set_txfm_ctxs` publish overwrote a size an earlier block of the
/// same frame had left in the above band, for a gate's own before/after
/// delta.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn lossless_sq_txfm_band_overwrite_hits() -> usize {
    LOSSLESS_SQ_TXFM_BAND_OVERWRITE_HITS.with(std::cell::Cell::get)
}

/// [`CHROMA_SPLIT_TX_HITS`], for a gate's own before/after delta.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn chroma_split_tx_hits() -> usize {
    CHROMA_SPLIT_TX_HITS.with(std::cell::Cell::get)
}

thread_local! {
    /// How many INTRA leaves of an inter frame's 8x8 block ran the lossless
    /// 4:4:4 chroma walk ([`read_intra_chroma_lossless`], lane-av1-llintra8)
    /// -- one bump per leaf, four units per plane inside it. A gate proving
    /// the arm must see this nonzero; CHROMA_SPLIT_TX_HITS alone cannot
    /// attribute its bumps to THIS leaf.
    pub(crate) static LLINTRA8_CHROMA_WALK_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// [`LLINTRA8_CHROMA_WALK_HITS`]: lossless 4:4:4 chroma walks through an
/// inter frame's 8x8 INTRA leaf.
pub fn llintra8_chroma_walk_hits() -> usize {
    LLINTRA8_CHROMA_WALK_HITS.with(|c| c.get())
}

thread_local! {
    /// lane-av1sub8rect444: how many of `decode_intra_sub8_leaf`'s 8x4/4x8
    /// INTRA leaves ran the lossless 4:4:4 per-unit chroma walk (two TX_4X4
    /// units per plane, plane-major) instead of one TX_8X4/TX_4X8 rect unit
    /// -- the shape whose unit count AND skip ctx the rect read got wrong.
    /// A gate proving the arm fired; CHROMA_SPLIT_TX_HITS cannot attribute
    /// its bumps to THIS leaf.
    pub(crate) static LLSUB8RECT444_CHROMA_WALK_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// [`LLSUB8RECT444_CHROMA_WALK_HITS`]: lossless 4:4:4 chroma walks through a
/// sub-8 8x4/4x8 INTRA leaf.
pub fn llsub8rect444_chroma_walk_hits() -> usize {
    LLSUB8RECT444_CHROMA_WALK_HITS.with(|c| c.get())
}

thread_local! {
    /// lane-av1444chr: how many chroma units resolved their `tx_type` from the
    /// luma leaf covering their OWN quadrant instead of the block-level
    /// (`first_tx_type`) one -- the route this lane corrected.
    /// `CHROMA_SPLIT_TX_HITS` cannot attribute its bumps to it: every split
    /// chroma unit bumps that one.
    pub(crate) static CHROMA_QUAD_LEAF_TX_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    /// Of those, how many resolved a type that DIFFERS from the block-level one
    /// -- the exact condition lane-av1444chr measured wrong: over a luma leaf
    /// that coded nothing the map cell is `DCT_DCT` while the top-left leaf's
    /// is `IDTX`. A gate proving the corrected route fired is NOT enough on
    /// its own (a block whose four leaves agree changes zero pixels); this is
    /// the number that says the route changed an answer.
    pub(crate) static CHROMA_QUAD_LEAF_TX_DIFF_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    /// lane-av1f9singleland: the SINGLE-REFERENCE copy of
    /// [`CHROMA_QUAD_LEAF_TX_HITS`], and the half of its
    /// [`CHROMA_QUAD_LEAF_TX_DIFF_HITS`] that belongs to that arm.
    ///
    /// Why a separate pair rather than trusting the shared counters: the
    /// shared ones are bumped from BOTH arms of the four-unit 4:4:4 chroma
    /// arm, so a green witness on them says nothing about which arm produced
    /// the hits. `decode_inter_block` has two textually parallel copies of
    /// that arm -- the compound prediction arm (the `build` closure) and the
    /// single-reference arm -- and a refactor can delete or revert either one
    /// while the other keeps the shared total non-zero. Measured on the pinned
    /// witness `fixtures/444_quad_leaf_tx_type.obu`: 96 units resolve through
    /// the covering leaf, and 88 of them are the single-reference arm's, so
    /// the shared counter is 92% single-reference and the compound arm alone
    /// would be a 4-unit sample of the route it appears to witness.
    pub(crate) static CHROMA_QUAD_LEAF_TX_SINGLEREF_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    /// Of [`CHROMA_QUAD_LEAF_TX_SINGLEREF_HITS`], how many resolved a type
    /// that DIFFERS from the block-level one. This is the arm's own non-vacuity
    /// bar: a route count alone stays green through a no-op resolve, because
    /// on a block whose four luma leaves all agree the per-quadrant value and
    /// the block-level one are the same answer. Mutating only the
    /// single-reference arm's `Some(cu_tx_type)` back to `Some(luma_tx_type)`
    /// turns the witness red with 777 bytes differing in decode-order frame 1.
    pub(crate) static CHROMA_QUAD_LEAF_TX_SINGLEREF_DIFF_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// [`CHROMA_QUAD_LEAF_TX_SINGLEREF_HITS`]: single-reference-arm chroma units
/// that took their type from the covering luma leaf.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn chroma_quad_leaf_tx_singleref_hits() -> usize {
    CHROMA_QUAD_LEAF_TX_SINGLEREF_HITS.with(std::cell::Cell::get)
}

/// [`CHROMA_QUAD_LEAF_TX_SINGLEREF_DIFF_HITS`]: of those, the units whose
/// covering leaf's type differs from the block-level one.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn chroma_quad_leaf_tx_singleref_diff_hits() -> usize {
    CHROMA_QUAD_LEAF_TX_SINGLEREF_DIFF_HITS.with(std::cell::Cell::get)
}

/// [`CHROMA_QUAD_LEAF_TX_HITS`]: chroma units that took their type from the
/// covering luma leaf.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn chroma_quad_leaf_tx_hits() -> usize {
    CHROMA_QUAD_LEAF_TX_HITS.with(std::cell::Cell::get)
}

/// [`CHROMA_QUAD_LEAF_TX_DIFF_HITS`]: chroma units whose covering leaf's type
/// differs from the block-level one.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn chroma_quad_leaf_tx_diff_hits() -> usize {
    CHROMA_QUAD_LEAF_TX_DIFF_HITS.with(std::cell::Cell::get)
}

thread_local! {
    /// lane-av1chrtx: chroma units of a RECT inter block
    /// ([`read_inter_rect_chroma`], the sibling of the 4:4:4 four-unit arms)
    /// that resolved their `tx_type` from the luma leaf covering their OWN
    /// quadrant instead of the block-level one. A SEPARATE counter from
    /// [`CHROMA_QUAD_LEAF_TX_HITS`] on purpose: that one gates a 4:2:0 control
    /// that must stay at exactly 0, and this route IS reachable at 4:2:0, so
    /// sharing the counter would destroy that control's meaning.
    pub(crate) static CHROMA_RECT_LEAF_TX_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    /// Of those, how many resolved a type that DIFFERS from the block-level
    /// one -- the condition that changes pixels. A route counter alone fires
    /// on every multi-unit rect chroma read, including the blocks whose
    /// leaves all agree (the exact shape a no-op fix looks green on).
    pub(crate) static CHROMA_RECT_LEAF_TX_DIFF_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// [`CHROMA_RECT_LEAF_TX_HITS`]: rect chroma units that took their type from
/// the covering luma leaf.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn chroma_rect_leaf_tx_hits() -> usize {
    CHROMA_RECT_LEAF_TX_HITS.with(std::cell::Cell::get)
}

/// [`CHROMA_RECT_LEAF_TX_DIFF_HITS`]: of those, the units whose covering
/// leaf's type differs from the block-level one.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn chroma_rect_leaf_tx_diff_hits() -> usize {
    CHROMA_RECT_LEAF_TX_DIFF_HITS.with(std::cell::Cell::get)
}
