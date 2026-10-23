//! The stream-level entry point a real decoder needs: walk a raw AV1 OBU
//! stream (`Encoded::stream`, or any low-overhead-format bitstream this
//! crate's own writers produce) via [`ec_av1_syntax::Av1Parser`] and dispatch
//! each frame's tile payload to [`crate::decode::decode_key_frame_tile_with_cdfs`] /
//! [`crate::decode::decode_inter_frame_tile_with_cdfs`], threading a
//! single-slot picture DPB exactly as [`crate::encode::encode_sequence`] does
//! on the write side, plus the full 8-slot *CDF* reference bank spec 7.20
//! (`load_cdfs`) and 7.4 (the reference frame update) describe: a frame whose
//! header names `primary_ref_frame != PRIMARY_REF_NONE` resumes from the
//! adapted table a previous frame's `refresh_frame_flags` left in that slot,
//! rather than the spec 8.4 defaults.
//!
//! This is the only reachable path for a caller that does not have the
//! encoder's own `Encoded::tile`/`mi_cols`/`mi_rows`/`base_q_idx` fields (they
//! are `pub(crate)`, and rightly so: the wire is `stream`, everything else is
//! an implementation detail of how this crate happens to build it).

use ec_av1_syntax::{
    Av1Parser, FrameHeader, FrameType, MAX_SEGMENTS, NUM_REF_FRAMES, ObuKind, PRIMARY_REF_NONE,
    SEG_LVL_GLOBALMV, SEG_LVL_REF_FRAME, SEG_LVL_SKIP, Tile, TxMode,
};
// Census-only: read outside `mod tests` only in comments; the non-test lib
// target never touches them, so they ride in behind cfg(test) (via the test
// module's `use super::*`).
#[cfg(test)]
use ec_av1_syntax::{SEG_LVL_ALT_Q, SEG_LVL_MAX};
use ec_core::{Error, Result};

use crate::cdf_state::Cdfs;
use crate::decode;
#[cfg(test)]
use crate::decode::decode_key_frame_tile_lr;
use crate::decode::{decode_inter_frame_tile_with_cdfs, decode_key_frame_tile_with_cdfs, q_ctx_of};
use crate::encode::Picture;

/// lane-midcut r1: `EC_AV1_NO_GRAIN=1` skips film grain synthesis at output, so a
/// mid-film mismatch splits into "reconstruction" (compare against
/// `ffmpeg -c:v libdav1d -filmgrain 0`) and "grain synthesis" in one run.
fn no_grain() -> bool {
    std::env::var_os("EC_AV1_NO_GRAIN").is_some()
}

// lane-hidden r1: where `EC_AV1_FINAL_DUMP`'s per-frame dump goes for the
// current thread, overriding the environment variable. Every ffmpeg/aomdec
// pixel gate in this repo compares SHOWN frames only (class
// gate-blind-to-hidden-frames), so the hidden alt-ref frames a real aomenc
// stream carries were never compared at all; the gate that does compare
// them needs a dump path that cannot race a sibling test's decode on
// another thread, which a process-global env var cannot give.
thread_local! {
    static FINAL_DUMP_PREFIX: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Sets (or with `None` clears) this thread's `EC_AV1_FINAL_DUMP` prefix.
#[allow(dead_code)]
pub(crate) fn set_final_dump_prefix(prefix: Option<String>) {
    FINAL_DUMP_PREFIX.with(|c| *c.borrow_mut() = prefix);
}

fn final_dump_prefix() -> Option<String> {
    FINAL_DUMP_PREFIX
        .with(|c| c.borrow().clone())
        .or_else(|| std::env::var("EC_AV1_FINAL_DUMP").ok())
}

/// 32x32-level 1:4 strip counters, for `decode_probe`'s fixture search:
/// (HORZ_4 strips, VERT_4 strips, strips that read coefficients).
/// lane-inter4 r1: the INTER-frame partition counters this lane's gates
/// assert -- `(32x8, 8x32, 64x32, 32x64, 64x16, 16x64)` strips decoded.
pub fn inter_rect_counters() -> (usize, usize, usize, usize, usize, usize) {
    (
        crate::decode::rect4_32_horz_inter_hits(),
        crate::decode::rect4_32_vert_inter_hits(),
        crate::decode::inter_sb_horz_hits(),
        crate::decode::inter_sb_vert_hits(),
        crate::decode::inter_sb_horz4_hits(),
        crate::decode::inter_sb_vert4_hits(),
    )
}

/// lane-r14 r2: luma transform units of an inter strip with a 64-px axis,
/// coded as the truncated low-32 coefficient corner.
pub fn rect64_inter_tu_hits() -> usize {
    crate::decode::rect64_inter_tu_hits()
}

/// lane-rectres r1: whole-block luma transform units of a 32x32-level 1:4
/// inter strip (`TX_32X8`/`TX_8X32`), `[32x8, 8x32]`.
pub fn rect32x8_inter_tu_hits() -> [usize; 2] {
    crate::decode::rect32x8_inter_tu_hits()
}

/// lane-t900 r7: sub-8x8 inter groups whose one chroma unit inherited a
/// `tx_type` different from the group's FIRST sub-block, i.e. groups that can
/// only decode right under `av1_get_tx_type`'s chroma-reference rule.
pub fn sub8_chroma_tx_from_ref_hits() -> usize {
    crate::decode::sub8_chroma_tx_from_ref_hits()
}

/// lane-intra16x4: `(16x4, 4x16, chroma-reference)` INTRA strips decoded inside
/// an inter 16x16-level 1:4 partition.
pub fn intra16x4_in_inter_hits() -> (usize, usize, usize) {
    crate::decode::intra16x4_in_inter_hits()
}

/// lane-sub8intra: INTRA 8x4/4x8 leaves inside an inter frame's sub-8x8
/// HORZ/VERT partition, `(8x4, 4x8, chroma-reference, mixed groups, split
/// 4x4 leaves)`.
pub fn sub8_intra_rect_hits() -> (usize, usize, usize, usize, usize) {
    crate::decode::sub8_intra_rect_hits()
}

/// lane-r14 r3: RECTANGULAR var-tx leaf transform units, `[32x16, 16x32]` --
/// the leaves `sub_tx_size_map` produces when a 1:4 strip with a 64-px axis
/// splits its transform (`[TX_64X16] == TX_32X16`, `[TX_16X64] == TX_16X32`).
pub fn vartx_rect_leaf_hits() -> [usize; 2] {
    crate::decode::vartx_rect_leaf_hits()
}

/// How many rect intra strips read the `use_intrabc` symbol on the last decode.
pub fn rect_intrabc_reads() -> usize {
    crate::decode::rect_intrabc_reads()
}

/// lane-av1-ibc444rect: unskipped intrabc 8x4/4x8 leaves whose luma a lossless
/// frame walked as per-4x4 inter units.
pub fn intrabc_rect8_lossless_luma_hits() -> usize {
    crate::decode::intrabc_rect8_lossless_luma_hits()
}

/// lane-av1-ibc444rect: unskipped intrabc 8x4/4x8 leaves whose chroma a
/// lossless 4:4:4 frame walked as per-4x4 `Chroma4` units.
pub fn intrabc_rect8_lossless_chroma_hits() -> usize {
    crate::decode::intrabc_rect8_lossless_chroma_hits()
}

/// Gate-side reset for the lane-av1-ibc444rect counters above.
pub fn reset_ibc444rect_hits() {
    crate::decode::reset_ibc444rect_hits()
}

/// lane-av1chromarect: 128-root intra-BC HORZ/VERT strips whose chroma plane
/// block was sized `bw >> ss_x` by `bh >> ss_y` rather than the hardcoded
/// 4:2:0 halving -- i.e. the 4:4:4 (and 4:4:0) cells, where
/// `av1_get_max_uv_txsize` (`ss_size_lookup[bsize]`, blockd.h:1372) applies no
/// halving of its own. At 4:2:0 the two forms are the same arithmetic, so a
/// `>= 1` witness can only come from a stream that is not 4:2:0. EXCLUSIVE to
/// `decode_intrabc_owned_rect`: no other caller sizes a 128-root intra-BC
/// strip's chroma plane block.
pub fn ibc_owned_rect_chroma_footprint_444_hits() -> usize {
    crate::decode::ibc_owned_rect_chroma_footprint_444_hits()
}

/// Gate-side reset for the lane-av1chromarect counter above.
pub fn reset_ibc_owned_rect_chroma_footprint_444_hits() {
    crate::decode::reset_ibc_owned_rect_chroma_footprint_444_hits()
}

/// lane-av1chromadc: chroma units of a skipped block whose intra-BC / UV
/// palette override had to be windowed to the unit's own extent -- `cn_cols *
/// cn_rows > 1`, which at 4:2:0 never happens and at 4:4:4 is every block of
/// 64 and above. Counts UNITS. EXCLUSIVE to `decode_block`'s skipped square
/// block chroma walk.
pub fn skip_chroma_override_window_hits() -> usize {
    crate::decode::skip_chroma_override_window_hits()
}

/// Gate-side reset for the lane-av1chromadc counter above.
pub fn reset_skip_chroma_override_window_hits() {
    crate::decode::reset_skip_chroma_override_window_hits()
}

/// lane-av1rectreach: the `chroma_tiled` walk's per-unit reach and grid-shape
/// counters, as `(unit_reach_differs_from_block, 2x1 units, 1x2 units, other
/// multi-unit units)`. See [`crate::decode::rect_tiled_chroma_grid_hits`].
pub fn rect_tiled_chroma_grid_hits() -> (usize, usize, usize, usize) {
    crate::decode::rect_tiled_chroma_grid_hits()
}

/// Gate-side reset for the lane-av1rectreach counters above.
pub fn reset_rect_tiled_chroma_grid_hits() {
    crate::decode::reset_rect_tiled_chroma_grid_hits()
}

/// `(y, uv)` palette blocks RECONSTRUCTED inside an inter frame (lane-t900
/// r22) -- the counter a screen-content witness gate reads to prove it
/// exercises the inter-frame palette path, not the key frame's.
pub fn intra_in_inter_palette_hits() -> (usize, usize) {
    crate::decode::intra_in_inter_palette_hits()
}

/// `(y, uv)` palette blocks reconstructed on an 8x8 LEAF inside an inter
/// frame (lane-t900 r23) -- the site-specific half of
/// [`intra_in_inter_palette_hits`], which a >=16 block also moves.
pub fn inter8_palette_hits() -> (usize, usize) {
    crate::decode::inter8_palette_hits()
}

/// lane-rect64port r1: coded 64-level HORZ/VERT intra strips whose luma
/// transform unit is the un-split TX_64X32 (orient 0) / TX_32X64 (orient 1).
pub fn rect64_corner_tu_hits(orient: usize) -> usize {
    crate::decode::rect64_corner_tu_hits(orient)
}

/// lane-inter16ab r4: RECTANGULAR var-tx leaves with a 4-px axis, `[8x4, 4x8]`
/// -- what `sub_tx_size_map[TX_16X4] == TX_8X4` produces when a 16x16-level
/// 1:4 inter strip splits its transform.
pub fn vartx_rect_leaf4_hits() -> [usize; 2] {
    crate::decode::vartx_rect_leaf4_hits()
}

/// lane-inter16ab r5: COMPOUND_WEDGE / wedge-INTERINTRA blocks that used a
/// RECTANGULAR wedge codebook row, per shape (`RECT_WEDGE_SHAPES` order:
/// 8x16, 16x8, 16x32, 32x16, 8x32, 32x8).
pub fn rect_wedge_hits() -> [usize; 6] {
    crate::decode::rect_wedge_hits()
}

/// lane-inter16ab r5: the wedge-interintra half of [`rect_wedge_hits`].
pub fn rect_wii_hits() -> [usize; 6] {
    crate::decode::rect_wii_hits()
}

/// lane-sb128c r7: the 128 root's rect arms -- (gathered edge HORZ, gathered
/// edge VERT, inter 128x64 blocks, inter 64x128 blocks).
pub fn sb128_rect_counters() -> (usize, usize, usize, usize) {
    (
        crate::decode::part128_edge_horz_hits(),
        crate::decode::part128_edge_vert_hits(),
        crate::decode::inter_sb128_horz_hits(),
        crate::decode::inter_sb128_vert_hits(),
    )
}

/// lane-av1-ibcrect: the 128 root's interior partition arms plus the key-frame
/// intra 128-axis blocks -- (split, none, horz, vert, ab[4], intra_horz,
/// intra_vert). Non-zero horz/vert mean a stream carried real PARTITION_HORZ/
/// VERT at the 128 root, so an intrabc-rect hunt can tell "the partition was
/// never chosen" from "chosen and decoded intra (or refused)".
pub fn part128_census() -> (usize, usize, usize, usize, [usize; 4], usize, usize) {
    (
        crate::decode::part128_split_hits(),
        crate::decode::part128_none_hits(),
        crate::decode::part128_horz_hits(),
        crate::decode::part128_vert_hits(),
        crate::decode::part128_ab_hits(),
        crate::decode::intra_sb128_hits().0,
        crate::decode::intra_sb128_hits().1,
    )
}

/// lane-av1-ibcrect: `use_intrabc` blocks on a 128-root HORZ/VERT strip whose
/// reconstruction ([`crate::decode::decode_intrabc_128rect`]) ran -- the
/// engagement counter the sb128 intrabc-rect gate asserts on.
pub fn intrabc_128rect_hits() -> usize {
    crate::decode::intrabc_128rect_hits()
}

/// lane-av1-ibc128chunk: chroma transform units the 128-root intrabc strip's
/// mu-chunk walk read, one per `read_inter_plane` chroma call. At 4:2:2 a
/// 64x64 mu chunk's chroma plane block is 32x64 and holds TWO STACKED
/// TX_32X32 units per plane; the square-cut walk this gate witnesses read
/// one. Measured on the pinned witness: 8 before, 48 after.
pub fn intrabc_128rect_chroma_chunk_hits() -> usize {
    crate::decode::intrabc_128rect_chroma_chunk_hits()
}

/// lane-av1-ibc128chunk: the 128-root intrabc strip's per-axis chroma chunk
/// extents as `(64 >> ss_x) << 8 | (64 >> ss_y)` on the last chunk walked --
/// 32|32 at 4:2:0, 32|64 at 4:2:2, 64|64 at 4:4:4.
pub fn intrabc_128rect_chunk_shape() -> usize {
    crate::decode::intrabc_128rect_chunk_shape()
}

/// Blocks reconstructed with intra block copy at an 8x8 LEAF specifically
/// (lane-kf900 r7's shape); [`crate::decode::intrabc_hits`] counts every shape.
pub fn leaf8_intrabc_hits() -> usize {
    crate::decode::leaf8_intrabc_hits()
}

/// Blocks whose `use_intrabc` symbol came out 1 and took the DV path, at ANY
/// shape -- the engagement counter the intrabc gates assert on. It counts
/// "read a `use_intrabc` DV", not "reconstructed": a block the rect-strip
/// reader REFUSES for an unported shape bumps it too (the symbol and the DV
/// are parsed before the refusal). [`leaf8_intrabc_hits`] is its 8x8-leaf
/// subset.
pub fn intrabc_hits() -> usize {
    crate::decode::intrabc_hits()
}

/// lane-av1txbands: sub-8x8 group chroma-reference leaves that USED intrabc and
/// were SKIPPED -- the route that armed [`crate::decode::intrabc_hits`]'s chroma
/// transform slot and then read no chroma coefficient. A non-zero value is what
/// makes the disarm at that route load-bearing on a real stream.
pub fn skipped_intrabc_chroma_arm_hits() -> usize {
    crate::decode::skipped_intrabc_chroma_arm_hits()
}

/// lane-sb128c r8: the 128 root's INTRA (key-frame) rect arm -- (128x64
/// blocks, 64x128 blocks, blocks per resolved intra `tx_depth` 0/1/2).
pub fn intra_sb128_counters() -> (usize, usize, [usize; 3]) {
    crate::decode::intra_sb128_hits()
}

/// lane-inter128intra r1: the 128 root's HORZ/VERT half coded INTRA inside an
/// INTER frame -- (128x64 blocks, 64x128 blocks).
pub fn intra128_in_inter_counters() -> [usize; 2] {
    crate::decode::intra128_in_inter_hits()
}

/// lane-lossless128: intra 128-axis blocks decoded on a LOSSLESS frame, where
/// every plane is a TX_4X4 unit. The engagement counter for
/// `a_lossless_sb128_rect_intra_block_decodes_sample_exact`.
pub fn intra128_lossless_counters() -> usize {
    crate::decode::intra128_lossless_hits()
}

/// lane-av1-128rectspan: chroma transform units re-stamped by
/// `decode_block_128rect`'s end-of-block replay, and how many of those
/// replays stamped a height that was not the unit's own.
/// lane-av1422lift: the second half was described as "the 4:2:2 signature --
/// zero on every stream this decoder admits, since 4:2:2 is refused by name
/// at the sequence header", and that was TRUE. 4:2:2 is decoded now and the
/// mismatch is NOT zero: measured on this branch's tip, the two
/// `422_intrabc_sb128_strip*` pins each read `(72, 9)` while the other 19
/// committed 4:2:2 cells read `(0, 0)`. Those two pins are byte-exact
/// against ffmpeg, so the non-zero half counts the arm's own designed case
/// and is not a defect. lane-av1422mismatch9: the predicate is
/// `luma_span != luma_span_h` (`chroma_tx << ss_x` vs `<< ss_y`), true by
/// CONSTRUCTION at ss (1,0), and the replay re-stamps with the same per-axis
/// pair the in-loop walk used, so the counter is a 4:2:2 shape test, not a
/// divergence test (`record_mi_chroma` writes only the left/above context
/// arrays, never a pixel). One 4:2:2 gate now reads the pair and asserts the
/// measured `(72, 9)`:
/// `the_422_block_128rect_chroma_chunk_gather_stays_per_axis`; the other
/// reader is on a 4:4:4 stream, where the mismatch half is structurally zero.
pub fn sb128rect_chroma_replay_counters() -> (usize, usize) {
    crate::decode::sb128rect_chroma_replay_hits()
}

/// lane-sb128c r9: the four AB shapes at the 128 root, in `PARTITION_HORZ_A`,
/// `_HORZ_B`, `_VERT_A`, `_VERT_B` order (key and inter tile paths share the
/// counter -- it is bumped where the root symbol resolves).
pub fn sb128_ab_counters() -> [usize; 4] {
    crate::decode::part128_ab_hits()
}

/// lane-obmcrec r1: OBMC neighbours taken from the second half of a 1:4-strip
/// chroma pair -- the shape whose filter record the r1 fix re-sourced.
pub fn obmc_pair_filter_hits() -> usize {
    crate::decode::obmc_pair_filter_hits()
}

/// Gate counters for the 4x32/32x4 rectangular strips: `(horz, vert, coeff)` hits.
pub fn rect4_32_counters() -> (usize, usize, usize) {
    (
        crate::decode::rect4_32_horz_hits(),
        crate::decode::rect4_32_vert_hits(),
        crate::decode::rect4_coeff_hits(),
    )
}

/// lane-inter4 r4: rectangular inter transform units coded whole, rect
/// var-tx trees that split, and OBMC blends run on a 16x8/8x16 leaf -- the
/// three counters a recipe sweep needs to see from outside the crate.
pub fn rect_inter_tu_counters() -> (usize, usize, usize) {
    (
        crate::decode::rect_inter_tu_hits(),
        crate::decode::rect_inter_txsplit_hits(),
        crate::decode::obmc_rect_leaf_hits(),
    )
}

/// Decode every frame in a raw AV1 OBU stream, in coding order.
///
/// A key frame becomes the reference for the inter frames that follow it,
/// until the next key frame resets the DPB — the same single-slot chain
/// [`crate::encode::encode_sequence`] threads on the write side. Only a
/// `Frame` OBU (an uncompressed header and its tile group in one OBU, which
/// is all this crate's encoder ever writes) carries a tile payload; sequence
/// headers and temporal delimiters are consumed for the state they carry (or
/// skipped) and produce no picture.
///
/// # Errors
/// Returns an error when the stream is truncated or malformed (as
/// [`Av1Parser`] reports it), when a frame header names anything this
/// crate's tile decoders do not reconstruct (see their own docs), or when an
/// inter frame appears before any key frame has supplied a reference.
pub fn decode_stream(data: &[u8]) -> Result<Vec<Picture>> {
    let mut pictures = Vec::new();
    decode_stream_with(data, |picture, _decode_idx, shown| {
        if shown {
            pictures.push(picture.clone());
        }
        Ok(())
    })?;
    Ok(pictures)
}

/// Streaming form of [`decode_stream`]: hand every completed picture to
/// `sink` the moment it exists and drop it right after, so peak memory is one
/// output picture plus the 8-slot DPB instead of the whole decoded stream --
/// a 4K long-GOP segment costs the same as a short one. `decode_stream`
/// itself is this function collecting into a `Vec`.
///
/// `sink` is called with the picture, the decode-order index of the coded
/// frame it came from, and whether it is output: a hidden reference frame
/// (`show_frame == 0`) is reported with `shown == false` and is NOT part of
/// `decode_stream`'s output, a `show_existing_frame` output with `true`.
/// The grain-synthesised picture is what a shown frame's sink sees; the
/// reference bank keeps the clean one (spec 7.18.3.1). An `Err` the sink
/// returns aborts the decode and is returned unchanged.
///
/// # Errors
/// As [`decode_stream`], plus whatever `sink` returns.
pub fn decode_stream_with(
    data: &[u8],
    sink: impl FnMut(&Picture, usize, bool) -> Result<()>,
) -> Result<()> {
    decode_stream_with_layers(data, LayerOutput::default(), sink)
}

/// Which shown frames a sink receives when a temporal unit carries more than
/// one (spatial layers). Mirrors libaom's `output_all_layers` control.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayerOutput {
    /// libaom's default (`output_all_layers == 0`): within one temporal unit
    /// (delimited by `OBU_TEMPORAL_DELIMITER`) only the last shown frame is
    /// output, which is the highest spatial layer. Only streams whose OBUs
    /// carry a spatial layer id above 0 are affected; every other stream
    /// delivers exactly what [`LayerOutput::AllShown`] does.
    #[default]
    LastShownPerTemporalUnit,
    /// Every shown frame, in decode order (libaom `output_all_layers == 1`).
    AllShown,
}

/// [`decode_stream_with`] with the output policy named (see [`LayerOutput`]).
/// Hidden frames (`shown == false`) are always forwarded, as before.
///
/// # Errors
/// As [`decode_stream_with`].
pub fn decode_stream_with_layers(
    data: &[u8],
    layers: LayerOutput,
    mut sink: impl FnMut(&Picture, usize, bool) -> Result<()>,
) -> Result<()> {
    let keep = match layers {
        LayerOutput::AllShown => None,
        LayerOutput::LastShownPerTemporalUnit => last_shown_per_temporal_unit(data),
    };
    let Some(keep) = keep else {
        return decode_stream_with_threads(data, decode_threads(), sink);
    };
    let mut shown_index = 0usize;
    decode_stream_with_threads(data, decode_threads(), |picture, decode_idx, shown| {
        if !shown {
            return sink(picture, decode_idx, shown);
        }
        let k = shown_index;
        shown_index += 1;
        // A plan shorter than the decode (never expected) forwards the rest.
        if keep.get(k).copied().unwrap_or(true) {
            sink(picture, decode_idx, shown)
        } else {
            Ok(())
        }
    })
}

/// The output plan of [`LayerOutput::LastShownPerTemporalUnit`]: one flag per
/// shown frame in stream order, `true` for the last shown frame of each
/// temporal unit. `None` when the stream has no spatial layer above 0 (so the
/// policy does not apply) or no temporal delimiter (its units are unknown).
///
/// Only OBU headers and the first payload byte of frame OBUs are read (the
/// `show_existing_frame` and `show_frame` bits of spec 5.9.2), so the plan
/// costs no decode state.
fn last_shown_per_temporal_unit(data: &[u8]) -> Option<Vec<bool>> {
    const OBU_TEMPORAL_DELIMITER: u8 = 2;
    const OBU_FRAME_HEADER: u8 = 3;
    const OBU_FRAME: u8 = 6;
    // (temporal unit index, spatial_id) of each shown frame, in stream order.
    let mut shown: Vec<(usize, u8)> = Vec::new();
    let mut tu = 0usize;
    let mut saw_td = false;
    let mut spatial_layers = false;
    let mut pos = 0usize;
    while pos < data.len() {
        let b0 = data[pos];
        let obu_type = (b0 >> 3) & 0xf;
        let extension = b0 & 0x04 != 0;
        let has_size = b0 & 0x02 != 0;
        let mut header_len = 1usize;
        let mut spatial_id = 0u8;
        if extension {
            let b1 = *data.get(pos + 1)?;
            spatial_id = (b1 >> 3) & 0x3;
            header_len = 2;
        }
        let (payload_start, payload_len) = if has_size {
            let mut value = 0usize;
            let mut len = 0usize;
            loop {
                let byte = *data.get(pos + header_len + len)?;
                value |= usize::from(byte & 0x7f) << (7 * len);
                len += 1;
                if byte & 0x80 == 0 || len == 8 {
                    break;
                }
            }
            (pos + header_len + len, value)
        } else {
            (pos + header_len, data.len().checked_sub(pos + header_len)?)
        };
        let payload_end = payload_start.checked_add(payload_len)?.min(data.len());
        if extension && spatial_id > 0 {
            spatial_layers = true;
        }
        match obu_type {
            OBU_TEMPORAL_DELIMITER => {
                if saw_td {
                    tu += 1;
                }
                saw_td = true;
            }
            OBU_FRAME_HEADER | OBU_FRAME => {
                // uncompressed_header: show_existing_frame (1 bit), then
                // frame_type (2 bits) and show_frame (1 bit).
                if let Some(&first) = data
                    .get(payload_start)
                    .filter(|_| payload_start < payload_end)
                {
                    let shown_frame = if first & 0x80 != 0 {
                        true
                    } else {
                        first & 0x10 != 0
                    };
                    if shown_frame {
                        shown.push((tu, spatial_id));
                    }
                }
            }
            _ => {}
        }
        pos = payload_end;
    }
    if !spatial_layers || !saw_td {
        return None;
    }
    Some(
        shown
            .iter()
            .enumerate()
            .map(|(k, &(unit, _))| shown.get(k + 1).is_none_or(|&(next, _)| next != unit))
            .collect(),
    )
}

/// lane-thread2: how many threads [`decode_stream_with`] decodes with --
/// `EC_AV1_THREADS`, default `min(cores, 6)` (lane-av1threads, see
/// [`crate::autothreads`]). `EC_AV1_THREADS=1` is the single-threaded decoder
/// unchanged, which is what keeps every gate that reads this crate's
/// per-thread counters meaningful.
fn decode_threads() -> usize {
    static N: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
        crate::autothreads::decode_threads_from(
            crate::envflags::var("EC_AV1_THREADS").ok().as_deref(),
        )
    });
    *N
}

/// lane-thread2: output-order queue. A leaf frame decoded on a worker
/// finishes whenever it finishes; `sink` must still see every picture in the
/// order the stream codes them, so each sink call gets an `emit_seq` when the
/// main thread reaches its frame header and is held here until every earlier
/// one has been emitted. (The decode index the sink is TOLD is unchanged, and
/// is deliberately not unique: a `show_existing_frame` re-output reports the
/// count of frames decoded before it, per this function's contract.)
#[derive(Default)]
struct Reorder {
    next: usize,
    // lane-pool1: the picture is shared, not owned -- a decoded frame that
    // refreshes a reference slot is already behind an `Arc` for the slot, and
    // handing the output an owned `Picture` meant copying every plane of it
    // (24 MB per 4K 10-bit frame) purely to put it in this queue.
    pending: std::collections::BTreeMap<usize, (std::sync::Arc<Picture>, usize, bool)>,
}

impl Reorder {
    fn push(
        &mut self,
        seq: usize,
        picture: std::sync::Arc<Picture>,
        decode_idx: usize,
        shown: bool,
    ) {
        self.pending.insert(seq, (picture, decode_idx, shown));
    }

    /// Emits every queued picture whose turn has come. An `Err` from `sink`
    /// aborts the decode, unchanged, exactly as the serial path did.
    fn drain(&mut self, sink: &mut impl FnMut(&Picture, usize, bool) -> Result<()>) -> Result<()> {
        while let Some((picture, decode_idx, shown)) = self.pending.remove(&self.next) {
            self.next += 1;
            sink(&picture, decode_idx, shown)?;
            crate::timeline::output(decode_idx);
        }
        Ok(())
    }
}

/// lane-thread2/3: how many coded frames this PROCESS has dispatched to a
/// worker thread (see [`decode_stream_with_threads`]). Process-global and
/// atomic on purpose: it is the only counter here a caller can read after a
/// threaded decode, and it is what keeps a "decodes identically with 4
/// threads" gate from silently proving nothing because every frame ran
/// inline. (lane-thread2 counted only LEAF frames, the only ones it could
/// dispatch; lane-thread3 dispatches every non-key, non-`show_existing_frame`
/// frame, hence the rename.)
static FRAMES_DISPATCHED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Reads [`FRAMES_DISPATCHED`].
pub fn frames_dispatched() -> usize {
    FRAMES_DISPATCHED.load(std::sync::atomic::Ordering::Relaxed)
}

/// lane-thread3: what one decoded frame stores into every reference slot its
/// `refresh_frame_flags` names (spec 7.20), as one shared, never-mutated
/// object -- so a slot costs an `Arc` clone instead of a picture copy.
struct SlotValue {
    /// lane-twostage: this frame's FILTERED pixels, still being produced
    /// when the rest of this value is published.
    pixels: std::sync::Arc<SlotPromise<std::sync::Arc<Picture>>>,
    /// The size those pixels will have, known from the tile decode.
    width: usize,
    height: usize,
    cdfs: Cdfs,
    motion_field: crate::motion_field::MotionField,
    seg_map: Vec<u8>,
    order_hint: u32,
}

/// lane-thread3: a reference slot's contents as a FUTURE. At `threads > 1`
/// the 8-slot bank holds these instead of values: a slot refreshed by a frame
/// that is still decoding on a worker holds the promise from the moment the
/// parsing thread DISPATCHES that frame, so the next frame can be dispatched
/// straight away and waits, on its own worker, for exactly the slots it
/// reads. This is what makes a pyramid stream (aomenc's default, where nearly
/// every coded frame refreshes a slot, so lane-thread2's leaf rule dispatched
/// almost nothing) decode in parallel.
///
/// DEADLOCK ARGUMENT. A promise is installed into a slot on the parsing
/// thread, in decode order, when its producer is dispatched; a consumer
/// captures the promises of the slots it reads at ITS dispatch, which is
/// strictly later in decode order. Every wait therefore points from a later
/// coded frame to an earlier one -- the wait graph is the reference DAG, and
/// is acyclic. The parsing thread waits only for a `show_existing_frame`
/// output (again an earlier frame) and at the in-flight bound, where it waits
/// for ANY worker to answer: the earliest in-flight frame has no earlier
/// in-flight producer, so it is always runnable and the bound always clears.
/// Each dispatched frame gets its own thread, so a blocked waiter never holds
/// a resource its producer needs. A producer that errors or panics marks its
/// promise `Failed`, so no consumer can wait for a value that never comes.
struct SlotPromise<T = std::sync::Arc<SlotValue>> {
    state: std::sync::Mutex<PromiseState<T>>,
    ready: std::sync::Condvar,
}

enum PromiseState<T> {
    Pending,
    Ready(T),
    Failed,
}

impl<T: Clone> SlotPromise<T> {
    /// The state lock, poison-tolerant (lane-av1panichang): a panic while it
    /// was held must not turn every later `fulfil`/`fail`/`wait` into a second
    /// panic, which would leave a waiter behind it unreleased.
    fn lock(&self) -> std::sync::MutexGuard<'_, PromiseState<T>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn pending() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            state: std::sync::Mutex::new(PromiseState::Pending),
            ready: std::sync::Condvar::new(),
        })
    }

    /// A slot filled by a frame the parsing thread decoded itself.
    fn ready(value: T) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            state: std::sync::Mutex::new(PromiseState::Ready(value)),
            ready: std::sync::Condvar::new(),
        })
    }

    fn fulfil(&self, value: T) {
        *self.lock() = PromiseState::Ready(value);
        self.ready.notify_all();
    }

    /// lane-twostage: fulfils only a promise the stage-1 sink did not
    /// already fill (a superres frame, or a key frame, whose meta is
    /// published here as it always was).
    fn fulfil_if_pending(&self, value: impl FnOnce() -> T) {
        let mut state = self.lock();
        if matches!(*state, PromiseState::Pending) {
            *state = PromiseState::Ready(value());
        }
        self.ready.notify_all();
    }

    fn fail(&self) {
        let mut state = self.lock();
        if matches!(*state, PromiseState::Pending) {
            *state = PromiseState::Failed;
        }
        self.ready.notify_all();
    }

    /// Blocks until the producing frame has published this slot.
    fn wait(&self) -> Result<T> {
        let mut state = self.lock();
        loop {
            match &*state {
                PromiseState::Ready(value) => return Ok(value.clone()),
                PromiseState::Failed => {
                    return Err(Error::corrupt(
                        "AV1 decode_stream: a reference frame this frame depends on failed to decode",
                    ));
                }
                PromiseState::Pending => {
                    state = self
                        .ready
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
            }
        }
    }
}

/// Answers one dispatched job exactly once and releases its promises
/// (lane-av1panichang). The frame thread blocks in `collect_one` for exactly
/// one answer per job, so a job that unwinds without sending one would hang
/// the whole decode. `send` answers on the normal path; `Drop` answers with an
/// error if the job is still unanswered, which is the only way a job ends
/// when it unwinds past its own `catch_unwind`.
///
/// The error is queued BEFORE the promises fail. A consumer learns of the
/// failure only by waking on `fail()`, so its own "a reference frame ...
/// failed" reply is then queued strictly after this frame's real reason.
/// Failing first let a consumer's secondary error win the race to the channel
/// under load, and the caller then saw the wrong reason.
struct JobAnswer {
    tx: Option<std::sync::mpsc::Sender<LeafResult>>,
    seq: usize,
    decode_job: bool,
    /// The frame's stage-1 promise (its slot meta), when it refreshes slots.
    stage1: Option<std::sync::Arc<SlotPromise>>,
    /// The frame's stage-2 promise (its filtered pixels).
    stage2: Option<std::sync::Arc<SlotPromise<std::sync::Arc<Picture>>>>,
}

impl JobAnswer {
    fn new(
        tx: std::sync::mpsc::Sender<LeafResult>,
        seq: usize,
        decode_job: bool,
        stage1: Option<std::sync::Arc<SlotPromise>>,
        stage2: Option<std::sync::Arc<SlotPromise<std::sync::Arc<Picture>>>>,
    ) -> Self {
        Self {
            tx: Some(tx),
            seq,
            decode_job,
            stage1,
            stage2,
        }
    }

    /// Answers with `reply`; a failed reply also fails both promises.
    fn send(&mut self, reply: Result<(std::sync::Arc<Picture>, usize, bool)>) {
        let failed = reply.is_err();
        if let Some(tx) = self.tx.take() {
            let _ = tx.send((self.seq, self.decode_job, reply));
        }
        if failed {
            self.fail_promises();
        }
    }

    fn fail_promises(&self) {
        // Neither stage may leave a consumer waiting for a value that will
        // never come. `fail` only moves a still-pending promise.
        if let Some(p) = &self.stage1 {
            p.fail();
        }
        if let Some(p) = &self.stage2 {
            p.fail();
        }
    }
}

impl Drop for JobAnswer {
    fn drop(&mut self) {
        // Still unanswered: the job unwound before it could answer.
        if let Some(tx) = self.tx.take() {
            let _ = tx.send((
                self.seq,
                self.decode_job,
                Err(Error::corrupt(
                    "AV1 decode_stream: a worker thread panicked decoding a frame",
                )),
            ));
            self.fail_promises();
        }
    }
}

/// One finished frame from a worker: its output position, whether the job was
/// a frame DECODE (as opposed to a `show_existing_frame` output, which only
/// waits for a slot and synthesises grain -- lane-thread3 counts the two
/// against separate in-flight bounds, since a waiting output job costs no
/// CPU), and either the (grain-synthesised) picture with the decode index to
/// report or the error that aborts the stream.
type LeafResult = (usize, bool, Result<(std::sync::Arc<Picture>, usize, bool)>);

/// Waits for one in-flight job and emits whatever that unblocks.
fn collect_one(
    rx: &std::sync::mpsc::Receiver<LeafResult>,
    decodes: &mut usize,
    shows: &mut usize,
    reorder: &mut Reorder,
    sink: &mut impl FnMut(&Picture, usize, bool) -> Result<()>,
) -> Result<()> {
    // Every dispatched worker answers exactly once, panic included (it is
    // caught and turned into an error below), so the sender count cannot
    // reach zero while `in_flight > 0`.
    let (seq, decode_job, result) = rx
        .recv()
        .map_err(|_| Error::corrupt("AV1 decode_stream: a worker thread vanished"))?;
    if decode_job {
        *decodes -= 1;
    } else {
        *shows -= 1;
    }
    let (picture, decode_idx, shown) = result?;
    reorder.push(seq, picture, decode_idx, shown);
    reorder.drain(sink)
}

/// [`decode_stream_with`] with an explicit worker count (lane-thread2).
///
/// `threads == 1` is the serial decoder. Above that, frames that no later
/// frame can reference -- a shown, non-key frame whose `refresh_frame_flags`
/// is 0, i.e. a leaf of the reference graph -- are decoded on worker threads
/// while the main thread keeps parsing headers and decoding the frames that
/// DO feed the reference banks; `sink` still sees every picture in coded
/// order. Each worker owns a fresh [`crate::decode::FrameCtx`] and a snapshot
/// of the banks taken before dispatch, so no decode state is shared.
///
/// The per-decode counters this crate's gates read are thread-local, so at
/// `threads > 1` a leaf frame's counts land on the worker and are invisible
/// to the caller; that is why the default is 1.
///
/// # Errors
/// As [`decode_stream_with`]; a worker's error aborts the stream.
pub fn decode_stream_with_threads(
    data: &[u8],
    threads: usize,
    mut sink: impl FnMut(&Picture, usize, bool) -> Result<()>,
) -> Result<()> {
    // lane-pool1: this replaced a `std::thread::scope` -- frame workers now
    // come from a persistent pool (one per submitting thread, workers reused
    // across frames, so their `thread_local!` decode scratch survives the
    // frame). `jobs` is declared here and dropped at the end of the function,
    // including on `?`, so no job outlives the data it borrows.
    let jobs = crate::par::Batch::new("ec-av1-frame");
    // lane-av1recon: at more than one decode thread the recon wavefront and
    // the filter bands default on (an explicit env value still wins).
    let _split = crate::par::DecodeSplit::install(threads);
    // lane-thread1: this stream's per-frame decode state (was 38 thread_local
    // statics in decode.rs) -- one object, threaded down the decode call graph.
    let fctx = &crate::decode::FrameCtx::new();
    let final_dump = final_dump_prefix();
    let mut parser = Av1Parser::new();
    let mut pictures_shown: usize = 0;
    let mut pictures_decoded: usize = 0;
    // Spec 7.20/7.4: each of the 8 reference slots remembers the picture a
    // previous frame's `refresh_frame_flags` stored into it, exactly like the
    // CDF slots below -- `LAST_FRAME` names a slot via `ref_frame_idx[0]`,
    // not simply "the previous frame decoded"; a frame whose own predecessor
    // didn't refresh that slot (an altref/hidden frame, or any GOP with more
    // than one live reference) must keep predicting from whatever picture is
    // still sitting there. Round 7's cdffwd hunt: a global `reference`
    // overwritten every frame silently always used "the most recent decode"
    // instead, producing a bit-exact motion vector into the WRONG picture --
    // luma-only (chroma coincidentally shares this fixture's flat regions)
    // and small-magnitude (the two pictures mostly agree), which is exactly
    // what made it look like a rounding bug rather than a wrong reference.
    // lane-pool1: shared, not owned -- a frame refreshing k slots used to copy
    // its whole picture k times, and its output a (k+1)-th time.
    let mut ref_slots: [Option<std::sync::Arc<Picture>>; NUM_REF_FRAMES] =
        std::array::from_fn(|_| None);
    // lane-av1tmvp: each of the 8 reference slots' own saved temporal motion
    // field (spec 7.9's per-frame `MotionFieldMvs` storage, libaom
    // `cur_frame->mvs`) plus the `OrderHint` the picture in that slot was
    // decoded with -- `Picture` itself carries neither, so both ride
    // alongside `ref_slots` on the exact same `refresh_frame_flags` update.
    let mut motion_field_slots: [Option<crate::motion_field::MotionField>; NUM_REF_FRAMES] =
        std::array::from_fn(|_| None);
    let mut order_hint_slots: [u32; NUM_REF_FRAMES] = [0; NUM_REF_FRAMES];
    // Spec 7.20/7.4: each of the 8 reference slots remembers the CDF state a
    // previous frame's `refresh_frame_flags` stored into it -- the frame's
    // own end-of-tile adapted table, or (when the frame set
    // `disable_frame_end_update_cdf`) the table it started from, unchanged.
    let mut cdf_slots: [Option<Cdfs>; NUM_REF_FRAMES] = std::array::from_fn(|_| None);
    // lane-seg, spec 7.20's `SavedSegmentIds`: each reference slot also
    // remembers the segment map the frame stored there decoded, which is what
    // a later frame's `primary_ref_frame` reads back as `PrevSegmentIds`
    // (spec `load_previous_segment_ids`) for temporal segment-id prediction
    // and for a `segmentation_update_map == 0` frame's inherited map.
    let mut seg_map_slots: [Option<Vec<u8>>; NUM_REF_FRAMES] = std::array::from_fn(|_| None);

    // lane-thread2: output plumbing (inert at `threads == 1`, where nothing
    // is ever dispatched and `reorder` emits each picture the moment it is
    // pushed).
    let mut emit_seq = 0usize;
    let mut reorder = Reorder::default();
    // lane-thread3: frame decodes and `show_existing_frame` outputs are
    // bounded separately -- an output job is a promise wait plus a memcpy, so
    // letting it eat a decode's budget just idles a core (measured: sharing
    // one bound cost 4K 18.7 s at 244% CPU against 17.7 s at 287%).
    let mut in_flight_decodes = 0usize;
    let mut in_flight_shows = 0usize;
    let in_flight_cap = threads + 2;
    let (tx, rx) = std::sync::mpsc::channel::<LeafResult>();
    // lane-thread3: at `threads > 1` THIS is the reference bank -- one
    // [`SlotPromise`] per DPB slot, ready for a slot the parsing thread filled
    // itself and pending for one an in-flight frame will fill. The owned
    // arrays above are then untouched (and the single-threaded path never
    // creates a promise: it stays exactly the decoder it was, which is what
    // keeps every counter-reading gate meaningful).
    let mut slot_promises: [Option<std::sync::Arc<SlotPromise>>; NUM_REF_FRAMES] =
        std::array::from_fn(|_| None);
    // lane-timeline: which decoded frame installed each slot's promise, so
    // the timeline records a dependency EDGE (frame -> producing frame) and
    // not just a slot number. `usize::MAX` = nobody yet.
    let mut slot_owner = [usize::MAX; NUM_REF_FRAMES];

    let mut pos = 0usize;
    // A frame whose tiles are split across several `OBU_TILE_GROUP`s (spec
    // 5.11.1, aomenc's `--num-tile-groups`) sends a standalone
    // `OBU_FRAME_HEADER` first, with `show_existing_frame == false` and no
    // tile payload of its own -- `OBU_FRAME` (the `ObuKind::Frame` arm below)
    // only ever folds ONE tile group into the header's own OBU. Stash the
    // header here and accumulate tiles from each `OBU_TILE_GROUP` that
    // follows until every tile of the frame has arrived.
    let mut pending_header: Option<Box<FrameHeader>> = None;
    let mut pending_tiles: Vec<Tile> = Vec::new();
    while pos < data.len() {
        let obu = parser.parse_obu(&data[pos..])?;
        let obu_offset = pos;
        pos += obu.total_size;

        if let ObuKind::FrameHeader(header) = &obu.kind {
            if !header.show_existing_frame {
                // Before this fix, this branch fired unconditionally for
                // EVERY `FrameHeader` OBU (including this one), so a
                // standalone header always fell into the `show_existing_frame`
                // slot lookup below and misreported "a show_existing_frame
                // header naming an empty reference slot" for a frame that was
                // never a show_existing_frame header at all.
                pending_header = Some(header.clone());
                pending_tiles = Vec::new();
                continue;
            }
            // `show_existing_frame` (spec 7.21/5.9.2): no tile payload, this
            // OBU only names a DPB slot to output again -- the coded-frame
            // count and the shown-picture count are different things (a
            // hidden altref's own `Frame` OBU below is never pushed to
            // `pictures` on its own; it is only pushed here, later, when a
            // `show_existing_frame` header names its slot). Before r15 this
            // arm didn't exist at all: every `ObuKind::Frame` was pushed
            // unconditionally regardless of `show_frame`, so a hidden altref
            // was emitted (wrongly, in decode order) and its real
            // `show_existing_frame` output was silently dropped.
            let slot = header.frame_to_show_map_idx as usize;
            // lane-hdrlossless: libaom errors on the same stream
            // (`decodeframe.c:4651`: `if (frame_to_show == NULL)
            // aom_internal_error(... AOM_CODEC_UNSUP_BITSTREAM, "Buffer does
            // not contain a decoded frame")`) -- a spec-conformant guard.
            let empty_slot = || {
                Error::unsupported(
                    "AV1 decode_stream",
                    "a show_existing_frame header naming an empty reference slot",
                )
            };
            if threads > 1 {
                // lane-thread3: the named slot may still be decoding on a
                // worker, and the copy into the slots this header refreshes
                // is then just the promise -- no value needed. The OUTPUT
                // does need the picture, so it is produced on a worker too:
                // waiting for it here would stall the parsing thread, and a
                // stalled parser dispatches nothing (measured before this
                // arm existed: 7.9 s of an 8.8 s 1080p decode, 18.9 s of
                // 23.3 s at 4K, spent in this one wait). The wait still
                // points at an earlier coded frame, so the argument in
                // [`SlotPromise`] is unchanged.
                let promise = slot_promises[slot].clone().ok_or_else(empty_slot)?;
                for i in 0..NUM_REF_FRAMES {
                    if header.refresh_frame_flags & (1 << i) != 0 {
                        slot_promises[i] = Some(promise.clone());
                        slot_owner[i] = slot_owner[slot];
                    }
                }
                let cap_t0 = crate::timeline::now();
                let capped = in_flight_shows >= in_flight_cap;
                while in_flight_shows >= in_flight_cap {
                    collect_one(
                        &rx,
                        &mut in_flight_decodes,
                        &mut in_flight_shows,
                        &mut reorder,
                        &mut sink,
                    )?;
                }
                if capped {
                    crate::timeline::cap_wait("show", cap_t0, crate::timeline::now());
                }
                let (job_seq, decode_idx) = (emit_seq, pictures_decoded);
                emit_seq += 1;
                pictures_shown += 1;
                let grain = header.film_grain;
                let seq = SeqFlags::read(&parser);
                let tx = tx.clone();
                jobs.submit(move || {
                    let mut answer = JobAnswer::new(tx, job_seq, false, None, None);
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let value = promise.wait()?;
                        let shown_picture = value.pixels.wait()?;
                        let output = if grain.apply_grain && !no_grain() {
                            std::sync::Arc::new(crate::film_grain::apply_grain(
                                &shown_picture,
                                &grain,
                                seq.mc_identity,
                                u32::from(seq.bit_depth),
                                &crate::decode::FrameCtx::new(),
                            ))
                        } else {
                            std::sync::Arc::clone(&shown_picture)
                        };
                        Ok((output, decode_idx, true))
                    }))
                    .unwrap_or_else(|_| {
                        Err(Error::corrupt(
                            "AV1 decode_stream: a worker thread panicked on a show_existing_frame output",
                        ))
                    });
                    answer.send(reply);
                    }));
                });
                in_flight_shows += 1;
                while let Ok((seq, decode_job, result)) = rx.try_recv() {
                    if decode_job {
                        in_flight_decodes -= 1;
                    } else {
                        in_flight_shows -= 1;
                    }
                    let (picture, decode_idx, shown) = result?;
                    reorder.push(seq, picture, decode_idx, shown);
                    reorder.drain(&mut sink)?;
                }
                continue;
            }
            let picture = {
                let picture = ref_slots[slot].clone().ok_or_else(empty_slot)?;
                for i in 0..NUM_REF_FRAMES {
                    if header.refresh_frame_flags & (1 << i) != 0 {
                        cdf_slots[i] = cdf_slots[slot].clone();
                        ref_slots[i] = Some(picture.clone());
                        motion_field_slots[i] = motion_field_slots[slot].clone();
                        seg_map_slots[i] = seg_map_slots[slot].clone();
                        order_hint_slots[i] = order_hint_slots[slot];
                    }
                }
                picture
            };
            let output = if header.film_grain.apply_grain && !no_grain() {
                // lane-hbd10 r1: grain synthesis is bit-depth generic
                // (`film_grain.rs`'s `grain_range`/`scale_lut`, spec 7.18.3).
                let bit_depth = parser
                    .sequence_header()
                    .map_or(8, |seq| seq.color_config.bit_depth);
                let mc_identity = parser
                    .sequence_header()
                    .is_some_and(|seq| seq.color_config.matrix_coefficients == 0);
                std::sync::Arc::new(crate::film_grain::apply_grain(
                    &picture,
                    &header.film_grain,
                    mc_identity,
                    bit_depth as u32,
                    fctx,
                ))
            } else {
                picture
            };
            reorder.push(emit_seq, output, pictures_decoded, true);
            emit_seq += 1;
            reorder.drain(&mut sink)?;
            pictures_shown += 1;
            continue;
        }
        // `tiles_base_offset` is added to every `Tile::offset` below to reach
        // an absolute position in `data`. `OBU_FRAME`'s tiles are relative to
        // this OBU's own `obu_offset`; an accumulated run of `OBU_TILE_GROUP`s
        // may span several different OBUs (each at a different offset in
        // `data`), so those tiles are normalised to an absolute offset the
        // moment they are appended to `pending_tiles`, and `tiles_base_offset`
        // is 0 for that case.
        let (header, tiles, tiles_base_offset): (Box<FrameHeader>, Vec<Tile>, usize) =
            match obu.kind {
                ObuKind::Frame(header, tiles) => (header, tiles, obu_offset),
                ObuKind::TileGroup(new_tiles) => {
                    let header = pending_header.clone().ok_or_else(|| {
                        Error::corrupt(
                            "AV1 decode_stream: a tile group OBU with no preceding frame header",
                        )
                    })?;
                    let expected = header.tile_info.cols * header.tile_info.rows;
                    pending_tiles.extend(new_tiles.into_iter().map(|mut t| {
                        t.offset += obu_offset;
                        t
                    }));
                    if (pending_tiles.len() as u32) < expected {
                        continue;
                    }
                    pending_header = None;
                    (header, std::mem::take(&mut pending_tiles), 0)
                }
                _ => continue,
            };
        // Every tile is decoded below through `tile_bufs`; this only rejects a
        // frame OBU that carries no tile group at all.
        if tiles.is_empty() {
            return Err(Error::unsupported(
                "AV1 decode_stream",
                "a frame OBU with no tile group",
            ));
        }
        // `read_cdef` (spec `decodeframe.c`, called at the first non-skip
        // block of each 64x64) only reads a literal `cdef_idx` when
        // `cdef_bits > 0`; at `cdef_bits == 0` there is nothing to read (a
        // true no-op). lane-realworld r1: the per-64x64 `cdef_idx` literal is
        // now read (`maybe_read_cdef_idx` in decode.rs) and threaded into
        // [`crate::decode::apply_cdef`]'s per-superblock strength lookup, so
        // a `cdef_bits > 0` stream no longer needs this refusal.
        // Both frame types now read their transform size under
        // `TxMode::Select`: an intra frame's `decode_key_frame_tile*` reads
        // `tx_depth` (lane-av1txsel, spec 5.11.16), and an inter frame's
        // `decode_inter_block`/`decode_inter_block8` read the recursive
        // `txfm_split` var-tx tree (lane-txselect, spec 5.11.17) -- so the
        // blanket inter-frame refusal that used to stand here is gone. What
        // remains refused is named at its own site in `read_block_tx_size`.
        // lane-av1real r11's blindness-audit sweep (`read_intra_frame_mode_info`,
        // libaom decodemv.c): three more per-block/per-SB symbols this crate
        // never reads at all, each gated by a header field this decoder
        // otherwise accepts uncomplaining -- the same silent-desync shape
        // `use_filter_intra` was. `allow_screen_content_tools` gates both
        // `read_intrabc_info` and `read_palette_mode_info`; `delta.q_present`/
        // `delta.lf_present` gate `read_delta_qindex`/`read_delta_lflevel`
        // (spec 5.11.10/5.11.11, one symbol group per superblock). Refuse
        // before desyncing rather than silently miscode, the same pattern as
        // the CDEF/TxMode::Select refusals above.
        // lane-screen r2: key frames consume (not reconstruct) palette/
        // intrabc syntax via read_intra_mode, and decode_inter_block/
        // decode_inter_block8's own intra-sub-block branches now consume
        // palette the same way (libaom's read_intra_block_mode_info has no
        // intrabc call at all -- that symbol is intra-frame-only). A
        // genuine palette/intrabc use still refuses by name deeper in the
        // block readers, so no whole-frame refusal is needed here anymore.
        // lane-realworld r5: both delta_q (maybe_read_delta_q, CURRENT_Q_IDX)
        // and delta_lf (maybe_read_delta_lf, CURRENT_DELTA_LF ->
        // Neighbours::delta_lf_grid -> lf_level) are now read and applied;
        // no whole-frame refusal needed here anymore.
        //
        // lane-hbd r5: `Picture`'s `y`/`u`/`v` are `u16` now (widened an
        // earlier round), `BIT_DEPTH` is wired from this frame's own sequence
        // header (`set_bit_depth` above), and a real `aomenc --bit-depth=10`
        // stream decodes pixel-exact against ffmpeg's own decode of the same
        // bytes (`a_real_aomenc_10bit_stream_decodes_pixel_exact`). The
        // blanket refusal that used to be here is gone; `film_grain.rs` and
        // `superres.rs` still refuse narrowly by name (their own hardcoded
        // 8-bit LUT/clamp, above), and any other rounding gap 10-bit exposes
        // will show up as a real pixel mismatch, not a silent truncation --
        // the whole reason the widen (and this wiring) happened.
        // lane-seg: `segment_id` is read per block now (spec 5.11.7-5.11.9,
        // `decode::intra_segment_id`/`inter_segment_id`) and consumed by the
        // per-segment quantizer (spec 7.12.2) and loop-filter level
        // (spec 7.14.4). The three features that instead REWRITE a block's
        // mode/reference decisions -- `SEG_LVL_REF_FRAME` (forces the
        // reference), `SEG_LVL_SKIP` and `SEG_LVL_GLOBALMV` (both suppress
        // symbols `decode_inter_block` unconditionally reads) -- have no
        // reader on the inter path, so they still refuse by name rather than
        // desyncing. aomenc's `--aq-mode` segmentation only ever sets
        // `SEG_LVL_ALT_Q` (census gate
        // `a_segmentation_census_over_real_aq_streams_finds_no_mode_overriding_feature`:
        // 1-pass, alt-ref-lag and two-pass arms, every frame header parsed).
        let tile_bufs: Vec<&[u8]> = tiles
            .iter()
            .map(|t| {
                data.get(tiles_base_offset + t.offset..tiles_base_offset + t.offset + t.size)
                    .ok_or(Error::NeedMore)
            })
            .collect::<Result<_>>()?;
        let seq = SeqFlags::read(&parser);
        // lane-thread3: at `threads > 1` every coded frame that is not a key
        // frame is decoded on a worker -- frames that refresh reference slots
        // included, which on a pyramid stream is nearly all of them. Its
        // inputs are taken as promises and its output is published as one,
        // installed into the slots it refreshes HERE, on the parsing thread,
        // in decode order, so the next frame's dependencies are known without
        // waiting for this one to finish (see [`SlotPromise`] for the
        // deadlock argument). A key frame stays inline: it resets the whole
        // bank and reads nothing, so there is nothing to gain.
        if threads > 1 && header.frame_type != FrameType::Key && !header.show_existing_frame {
            let cap_t0 = crate::timeline::now();
            let capped = in_flight_decodes >= in_flight_cap;
            while in_flight_decodes >= in_flight_cap {
                collect_one(
                    &rx,
                    &mut in_flight_decodes,
                    &mut in_flight_shows,
                    &mut reorder,
                    &mut sink,
                )?;
            }
            if capped {
                crate::timeline::cap_wait("decode", cap_t0, crate::timeline::now());
            }
            // Exactly the slots this frame reads: `ref_frame_idx` names its
            // references, and its `primary_ref_frame` CDFs/segment map, its
            // reference order hints and every field `setup_motion_field`
            // projects all come from those same slots. Nothing else is
            // waited on.
            let mut deps: [Option<std::sync::Arc<SlotPromise>>; NUM_REF_FRAMES] =
                std::array::from_fn(|_| None);
            for i in 0..7 {
                let slot = header.ref_frame_idx[i] as usize;
                deps[slot] = slot_promises[slot].clone();
            }
            let dep_owner = slot_owner;
            let produces = (header.refresh_frame_flags != 0).then(SlotPromise::pending);
            if let Some(promise) = &produces {
                for i in 0..NUM_REF_FRAMES {
                    if header.refresh_frame_flags & (1 << i) != 0 {
                        slot_promises[i] = Some(promise.clone());
                        slot_owner[i] = pictures_decoded;
                    }
                }
            }
            let (job_seq, decode_idx, shown_idx) = (emit_seq, pictures_decoded, pictures_shown);
            emit_seq += 1;
            pictures_decoded += 1;
            let shown = header.show_frame;
            if shown {
                pictures_shown += 1;
            }
            let tx = tx.clone();
            let dump = final_dump.clone();
            crate::timeline::submit(decode_idx, if shown { 'P' } else { 'A' }, shown);
            // The job outlives this call, so it owns its tile bytes (a copy of
            // this frame's compressed tiles) instead of borrowing `data`.
            let owned_tiles: Vec<Vec<u8>> = tile_bufs.iter().map(|t| t.to_vec()).collect();
            jobs.submit(move || {
                let tile_views: Vec<&[u8]> = owned_tiles.iter().map(Vec::as_slice).collect();
                let promise = produces;
                // lane-twostage: stage 2 -- this frame's filtered pixels. Its
                // consumers hold this promise from the moment stage 1 (CDFs,
                // segment map, motion field, size) is published at the tile
                // boundary, and wait on it at their first prediction.
                let pixels: std::sync::Arc<SlotPromise<std::sync::Arc<Picture>>> =
                    SlotPromise::pending();
                let mut answer = JobAnswer::new(
                    tx,
                    job_seq,
                    true,
                    promise.clone(),
                    Some(pixels.clone()),
                );
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        crate::fault::point("frame");
                        crate::timeline::start(decode_idx);
                        // The wait happens HERE, off the parsing thread, and only
                        // for this frame's own references.
                        let mut resolved: [Option<std::sync::Arc<SlotValue>>; NUM_REF_FRAMES] =
                            std::array::from_fn(|_| None);
                        for (slot, dep) in deps.iter().enumerate() {
                            if let Some(dep) = dep {
                                let t0 = crate::timeline::now();
                                resolved[slot] = Some(dep.wait()?);
                                crate::timeline::ref_wait(
                                    decode_idx,
                                    slot,
                                    dep_owner[slot],
                                    crate::timeline::now().saturating_sub(t0),
                                );
                            }
                        }
                        crate::timeline::refs_ready();
                        // Fresh per-thread state: every field this path reads is
                        // written from this frame's own header before it is read.
                        let fctx = crate::decode::FrameCtx::new();
                        // lane-twostage: the pictures behind this frame's own
                        // references, waited for at the first `ref_planes` call
                        // inside the tile decode rather than here.
                        let ref_metas: [Option<std::sync::Arc<SlotValue>>; 8] =
                            std::array::from_fn(|i| {
                                if i == 0 {
                                    None
                                } else {
                                    resolved
                                        .get(header.ref_frame_idx[i - 1] as usize)
                                        .cloned()
                                        .flatten()
                                }
                            });
                        let pix_cell: std::sync::OnceLock<
                            [Option<std::sync::Arc<Picture>>; 8],
                        > = std::sync::OnceLock::new();
                        let wait_pixels = || -> Result<[Option<&Picture>; 8]> {
                            if pix_cell.get().is_none() {
                                let mut pics: [Option<std::sync::Arc<Picture>>; 8] =
                                    Default::default();
                                for (i, meta) in ref_metas.iter().enumerate() {
                                    if let Some(meta) = meta {
                                        let slot = header.ref_frame_idx[i - 1] as usize;
                                        let t0 = crate::timeline::now();
                                        pics[i] = Some(meta.pixels.wait()?);
                                        crate::timeline::ref_wait(
                                            decode_idx,
                                            slot,
                                            dep_owner[slot],
                                            crate::timeline::now().saturating_sub(t0),
                                        );
                                    }
                                }
                                let _ = pix_cell.set(pics);
                            }
                            let pics = pix_cell.get().expect("the pictures were just set");
                            Ok(std::array::from_fn(|i| pics[i].as_deref()))
                        };
                        let ref_dims: [Option<(usize, usize)>; 8] =
                            std::array::from_fn(|i| {
                                ref_metas[i].as_ref().map(|m| (m.width, m.height))
                            });
                        // The same references for the recon workers, which wait
                        // for the pictures themselves (off the parse thread).
                        let shared_refs = {
                            let metas = ref_metas.clone();
                            let ref_idx = header.ref_frame_idx;
                            let owners = dep_owner;
                            decode::SharedRefs {
                                dims: ref_dims,
                                wait: std::sync::Arc::new(move || {
                                    let mut pics: [Option<std::sync::Arc<Picture>>; 8] =
                                        Default::default();
                                    for (i, meta) in metas.iter().enumerate() {
                                        if let Some(meta) = meta {
                                            let slot = ref_idx[i - 1] as usize;
                                            let t0 = crate::timeline::now();
                                            pics[i] = Some(meta.pixels.wait()?);
                                            crate::timeline::ref_wait(
                                                decode_idx,
                                                slot,
                                                owners[slot],
                                                crate::timeline::now().saturating_sub(t0),
                                            );
                                        }
                                    }
                                    Ok(pics)
                                }),
                            }
                        };
                        let refpix =
                            decode::RefPix::lazy(ref_dims, &wait_pixels, Some(shared_refs));
                        // Stage 1: published at this frame's tile boundary, before
                        // its filters run (`take_segment_ids` copies the map, so
                        // the filters that read it are unaffected).
                        let stage1 = |cdfs: Cdfs,
                                      motion_field: crate::motion_field::MotionField,
                                      width: usize,
                                      height: usize| {
                            if let Some(promise) = &promise {
                                promise.fulfil(std::sync::Arc::new(SlotValue {
                                    pixels: pixels.clone(),
                                    width,
                                    height,
                                    cdfs,
                                    motion_field,
                                    seg_map: decode::take_segment_ids(&fctx),
                                    order_hint: header.order_hint,
                                }));
                                crate::timeline::slot_fulfil();
                            }
                        };
                        // spec 7.9's `av1_setup_motion_field` runs inside
                        // `RefSnapshot::take`, i.e. here, after those waits --
                        // it reads the referenced slots' saved motion fields.
                        let motion_fields =
                            std::array::from_fn(|i| resolved[i].as_ref().map(|v| &v.motion_field));
                        let snap = RefSnapshot::take(
                            &header,
                            seq,
                            &refpix,
                            std::array::from_fn(|i| resolved[i].as_ref().map(|v| &v.cdfs)),
                            std::array::from_fn(|i| {
                                resolved[i].as_ref().map(|v| v.seg_map.as_slice())
                            }),
                            &motion_fields,
                            std::array::from_fn(|i| resolved[i].as_ref().map_or(0, |v| v.order_hint)),
                        );
                        let out = decode_frame(
                            &fctx,
                            &header,
                            &tile_views,
                            seq,
                            snap,
                            decode_idx,
                            shown_idx,
                            dump.as_deref(),
                            // lane-twostage publishes a frame's meta at its tile
                            // boundary instead of after its filters, so a consumer
                            // parses over its producer's filter tail. It is ON by
                            // default since lane-sub8group: with every reference
                            // pixel read deferred to the recon workers, the
                            // consumer parses its WHOLE tile before it needs a
                            // pixel, and the 4K critical chain shortens by 11-14%
                            // in three interleaved pairs (14450/10594/14407 ms off
                            // vs 12790/9207/12463 ms on) where it moved 3-7%
                            // before. `EC_AV1_TWO_STAGE=0` turns it back off.
                            (crate::envflags::var("EC_AV1_TWO_STAGE").as_deref() != Ok("0"))
                                .then_some(&stage1 as &dyn Fn(_, _, _, _)),
                        )?;
                        // Publish before grain synthesis: every frame waiting on
                        // this slot can start the moment the clean picture exists
                        // (spec 7.18.3.1 -- the bank never stores grain).
                        let picture = std::sync::Arc::new(out.picture);
                        pixels.fulfil(picture.clone());
                        if let Some(promise) = &promise {
                            // Only a frame whose stage-1 sink never fired (a
                            // superres frame) still has meta to publish here.
                            promise.fulfil_if_pending(|| {
                                std::sync::Arc::new(SlotValue {
                                    pixels: pixels.clone(),
                                    width: picture.width,
                                    height: picture.height,
                                    cdfs: out.stored_cdfs,
                                    motion_field: out.motion_field.expect(
                                        "a frame that published no stage 1 carries its motion field back",
                                    ),
                                    seg_map: out.seg_map,
                                    order_hint: header.order_hint,
                                })
                            });
                            crate::timeline::slot_fulfil();
                        }
                        // Grain is synthesised for OUTPUT only: a hidden frame is
                        // reported to the sink clean, exactly as the inline path
                        // below reports it (spec 7.18.3.1).
                        let output = if shown && header.film_grain.apply_grain && !no_grain() {
                            std::sync::Arc::new(crate::film_grain::apply_grain(
                                &picture,
                                &header.film_grain,
                                seq.mc_identity,
                                u32::from(seq.bit_depth),
                                &fctx,
                            ))
                        } else {
                            picture
                        };
                        crate::timeline::grain_done();
                        Ok((output, decode_idx, shown))
                    }))
                    .unwrap_or_else(|_| {
                        Err(Error::corrupt(
                            "AV1 decode_stream: a worker thread panicked decoding a frame",
                        ))
                    });
                    crate::fault::point("frame-after");
                    answer.send(reply);
                }));
            });
            in_flight_decodes += 1;
            FRAMES_DISPATCHED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // Anything that finished while this frame was being set up.
            while let Ok((seq, decode_job, result)) = rx.try_recv() {
                if decode_job {
                    in_flight_decodes -= 1;
                } else {
                    in_flight_shows -= 1;
                }
                let (picture, decode_idx, shown) = result?;
                reorder.push(seq, picture, decode_idx, shown);
                reorder.drain(&mut sink)?;
            }
            continue;
        }
        // lane-twostage: the inline path's references are decoded already,
        // so its handle never waits.
        let inline_refpix = decode::RefPix::ready(std::array::from_fn(|i| {
            if i == 0 {
                None
            } else {
                ref_slots
                    .get(header.ref_frame_idx[i - 1] as usize)
                    .and_then(|p| p.as_deref())
            }
        }));
        let snap = RefSnapshot::take(
            &header,
            seq,
            &inline_refpix,
            std::array::from_fn(|i| cdf_slots[i].as_ref()),
            std::array::from_fn(|i| seg_map_slots[i].as_deref()),
            &std::array::from_fn(|i| motion_field_slots[i].as_ref()),
            order_hint_slots,
        );
        let bit_depth = seq.bit_depth;
        let FrameOutput {
            picture,
            stored_cdfs,
            motion_field,
            seg_map: decoded_seg_map,
        } = decode_frame(
            fctx,
            &header,
            &tile_bufs,
            seq,
            snap,
            pictures_decoded,
            pictures_shown,
            final_dump.as_deref(),
            None,
        )?;
        let motion_field = motion_field.expect("the inline path passes no stage-1 sink");
        pictures_decoded += 1;
        // lane-pool1: one allocation, then every slot store and the output
        // itself are refcount bumps instead of plane copies.
        let picture = std::sync::Arc::new(picture);
        if threads > 1 {
            // lane-thread3: only a key frame reaches this inline path at
            // `threads > 1`, and the bank it fills is the promise array --
            // already resolved, since the picture exists right here.
            if header.refresh_frame_flags != 0 {
                let promise = SlotPromise::ready(std::sync::Arc::new(SlotValue {
                    pixels: SlotPromise::ready(std::sync::Arc::clone(&picture)),
                    width: picture.width,
                    height: picture.height,
                    cdfs: stored_cdfs,
                    motion_field,
                    seg_map: decoded_seg_map,
                    order_hint: header.order_hint,
                }));
                for i in 0..NUM_REF_FRAMES {
                    if header.refresh_frame_flags & (1 << i) != 0 {
                        slot_promises[i] = Some(promise.clone());
                        slot_owner[i] = pictures_decoded - 1;
                    }
                }
            }
        } else {
            for i in 0..NUM_REF_FRAMES {
                if header.refresh_frame_flags & (1 << i) != 0 {
                    cdf_slots[i] = Some(stored_cdfs.clone());
                    // Spec 7.18.3.1: synthesized grain is never stored for
                    // later prediction -- the reference bank always keeps the
                    // clean, pre-grain decode, even though the frame pushed
                    // onto the caller's output below carries the grained
                    // picture.
                    ref_slots[i] = Some(std::sync::Arc::clone(&picture));
                    motion_field_slots[i] = Some(motion_field.clone());
                    seg_map_slots[i] = Some(decoded_seg_map.clone());
                    order_hint_slots[i] = header.order_hint;
                }
            }
        }
        // lane-defon r1: a forward keyframe (aomenc `--enable-fwd-kf`) is a
        // KEY frame coded with `show_frame == 0`, output later by a
        // `show_existing_frame` header -- counted only once its tiles have
        // decoded, so the gate's assert covers decoded-and-compared frames.
        if header.frame_type == FrameType::Key && !header.show_frame {
            decode::note_fwd_kf();
        }
        if header.show_frame {
            let output = if header.film_grain.apply_grain && !no_grain() {
                let mc_identity = seq.mc_identity;
                std::sync::Arc::new(crate::film_grain::apply_grain(
                    &picture,
                    &header.film_grain,
                    mc_identity,
                    bit_depth as u32,
                    fctx,
                ))
            } else {
                picture
            };
            reorder.push(emit_seq, output, pictures_decoded - 1, true);
            emit_seq += 1;
            reorder.drain(&mut sink)?;
            pictures_shown += 1;
        } else {
            // Hidden (alt-ref/forward-key) frames never reach
            // `decode_stream`'s output, but a decode-order consumer -- the
            // `EC_AV1_FINAL_DUMP` comparison against aomdec, say -- needs
            // them, so they are reported too, with `shown == false`.
            reorder.push(emit_seq, picture, pictures_decoded - 1, false);
            emit_seq += 1;
            reorder.drain(&mut sink)?;
        }
    }
    while in_flight_decodes + in_flight_shows > 0 {
        collect_one(
            &rx,
            &mut in_flight_decodes,
            &mut in_flight_shows,
            &mut reorder,
            &mut sink,
        )?;
    }
    debug_assert!(reorder.pending.is_empty(), "a frame was never emitted");
    crate::timeline::dump();
    Ok(())
}

/// lane-thread2: the sequence-header bits a single frame's decode needs,
/// read once on the parsing thread so that [`decode_frame`] never touches the
/// [`Av1Parser`] (a worker thread has no access to it).
#[derive(Clone, Copy)]
pub(crate) struct SeqFlags {
    pub(crate) enable_filter_intra: bool,
    pub(crate) enable_dual_filter: bool,
    pub(crate) enable_edge_filter: bool,
    pub(crate) enable_masked_compound: bool,
    pub(crate) enable_jnt_comp: bool,
    pub(crate) enable_interintra_compound: bool,
    pub(crate) use_sb128: bool,
    pub(crate) bit_depth: u8,
    /// lane-av1mono: `color_config.mono_chrome` (spec 5.5.2):
    /// `NumPlanes == 1`, no chroma is coded or reconstructed.
    pub(crate) mono_chrome: bool,
    pub(crate) order_hint_bits: u32,
    pub(crate) mc_identity: bool,
    /// lane-av1txr: `color_config.subsampling_x/y` (spec 5.5.2). These used
    /// to exist only for the sequence-level refusal -- the decoder hardcoded
    /// 4:2:0 at every chroma extent. lane-av1422lift: that is no longer true
    /// and these are load-bearing on the DECODE path: `decode_stream_with`
    /// publishes both through `decode::set_subsampling` into `FrameCtx`, and
    /// every chroma extent, plane allocation, output crop, deblock/CDEF crop
    /// and restoration grid below reads them PER AXIS. A monochrome header
    /// parses as 1,1 and is unaffected. The only shape these cannot carry is
    /// (0,1) 4:4:0, which the header cannot code.
    pub(crate) subsampling_x: u8,
    pub(crate) subsampling_y: u8,
}

impl SeqFlags {
    fn read(parser: &Av1Parser) -> Self {
        let seq = parser.sequence_header();
        Self {
            enable_filter_intra: seq.is_some_and(|s| s.enable_filter_intra),
            enable_dual_filter: seq.is_some_and(|s| s.enable_dual_filter),
            enable_edge_filter: seq.is_some_and(|s| s.enable_intra_edge_filter),
            enable_masked_compound: seq.is_some_and(|s| s.enable_masked_compound),
            enable_jnt_comp: seq.is_some_and(|s| s.enable_jnt_comp),
            enable_interintra_compound: seq.is_some_and(|s| s.enable_interintra_compound),
            use_sb128: seq.is_some_and(|s| s.use_128x128_superblock),
            bit_depth: seq.map_or(8, |s| s.color_config.bit_depth),
            mono_chrome: seq.is_some_and(|s| s.color_config.mono_chrome),
            order_hint_bits: seq.map_or(0, |s| s.order_hint_bits),
            mc_identity: seq.is_some_and(|s| s.color_config.matrix_coefficients == 0),
            subsampling_x: seq.map_or(1, |s| s.color_config.subsampling_x),
            subsampling_y: seq.map_or(1, |s| s.color_config.subsampling_y),
        }
    }
}

/// lane-thread2: everything one frame's decode reads out of the cross-frame
/// banks `decode_stream_with` owns -- taken on the parsing thread, before any
/// later frame can overwrite a slot, so a frame can be decoded anywhere.
pub(crate) struct RefSnapshot<'a> {
    /// lane-twostage: this frame's references, indexed by `ref_frame`
    /// (`LAST_FRAME` = 1) -- sizes now, pixels at the first prediction.
    pub(crate) refpix: &'a decode::RefPix<'a>,
    /// `cdf_slots[ref_frame_idx[primary_ref_frame]]`, `None` when this frame
    /// names `PRIMARY_REF_NONE` *or* that slot holds no saved table (the
    /// second case is the refusal [`decode_frame`] raises where it always
    /// did, so the error order over a malformed stream is unchanged).
    pub(crate) primary_cdfs: Option<Cdfs>,
    /// Spec `load_previous_segment_ids`, from the same slot.
    pub(crate) prev_seg_map: Option<Vec<u8>>,
    /// Spec 7.9's projection, already run against `motion_field_slots`.
    pub(crate) tpl_field: Option<crate::motion_field::TplField>,
    /// `order_hint_slots[ref_frame_idx[i]]` for `LAST_FRAME + i`.
    pub(crate) ref_order_hints: [u32; 7],
}

impl<'a> RefSnapshot<'a> {
    /// Reads the banks for `header`. Every slot is passed in by reference so
    /// that the single-threaded path can borrow the DPB in place while a
    /// worker borrows the resolved [`SlotPromise`] values it waited on --
    /// spec 7.9's `setup_motion_field` therefore runs on the DECODING thread
    /// (lane-thread3), after those waits.
    fn take(
        header: &FrameHeader,
        seq: SeqFlags,
        refpix: &'a decode::RefPix<'a>,
        cdf_slots: [Option<&Cdfs>; NUM_REF_FRAMES],
        seg_map_slots: [Option<&[u8]>; NUM_REF_FRAMES],
        motion_field_slots: &[Option<&crate::motion_field::MotionField>; NUM_REF_FRAMES],
        order_hint_slots: [u32; NUM_REF_FRAMES],
    ) -> Self {
        let primary = (header.primary_ref_frame != PRIMARY_REF_NONE)
            .then(|| header.ref_frame_idx[header.primary_ref_frame as usize] as usize);
        // spec 7.9's own driver, `av1_setup_motion_field`: only run when
        // this frame's header asks for it, and never for a key frame (which
        // codes no inter block at all).
        let tpl_field =
            (header.frame_type != FrameType::Key && header.use_ref_frame_mvs).then(|| {
                crate::motion_field::setup_motion_field(
                    motion_field_slots,
                    header.ref_frame_idx,
                    header.order_hint,
                    seq.order_hint_bits,
                    header.mi_rows as usize,
                    header.mi_cols as usize,
                )
            });
        if std::env::var_os("EC_TPL").is_some() {
            eprintln!(
                "EC_TPL order_hint={} use_ref_frame_mvs={} cells={}",
                header.order_hint,
                header.use_ref_frame_mvs,
                tpl_field
                    .as_ref()
                    .map_or(0, crate::motion_field::TplField::filled_cells)
            );
        }
        Self {
            refpix,
            primary_cdfs: primary.and_then(|slot| cdf_slots[slot].cloned()),
            prev_seg_map: primary.and_then(|slot| seg_map_slots[slot].map(<[u8]>::to_vec)),
            tpl_field,
            ref_order_hints: std::array::from_fn(|i| {
                order_hint_slots[header.ref_frame_idx[i] as usize]
            }),
        }
    }
}

/// lane-thread2: what one decoded frame contributes back to the stream --
/// its picture (clean, pre-grain: spec 7.18.3.1) plus the three pieces a
/// `refresh_frame_flags` bit stores into a reference slot.
pub(crate) struct FrameOutput {
    pub(crate) picture: Picture,
    pub(crate) stored_cdfs: Cdfs,
    /// lane-twostage: `None` when this frame handed its motion field to the
    /// stage-1 sink at its tile boundary instead of carrying it back here.
    pub(crate) motion_field: Option<crate::motion_field::MotionField>,
    pub(crate) seg_map: Vec<u8>,
}

/// Spec 7.20's own `save_cdfs`: what a frame's `refresh_frame_flags` slots
/// keep -- the frame's INITIAL table when `disable_frame_end_update_cdf`,
/// otherwise the adapted end-of-tile table with every symbol counter zeroed
/// (libaom `av1_reset_cdf_symbol_counters`). lane-twostage: shared with the
/// stage-1 publish, which hands a consumer the same table it always got.
fn stored_cdfs_for(header: &FrameHeader, end_cdfs: Cdfs, started_from: Option<Cdfs>) -> Cdfs {
    if header.disable_frame_end_update_cdf {
        started_from.unwrap_or_else(|| Cdfs::new(q_ctx_of(header.quantization.base_q_idx)))
    } else {
        let mut end_cdfs = end_cdfs;
        end_cdfs.reset_counts();
        end_cdfs
    }
}

/// lane-thread2: one coded frame, decoded from its header, its tile payload
/// and a snapshot of the reference banks -- and from nothing else. Lifted
/// verbatim out of [`decode_stream_with`]'s loop so that the same code can
/// run on the parsing thread (a frame that refreshes a reference slot) or on
/// a worker (a leaf frame, see `decode_stream_with_threads`).
///
/// `fctx` is the DECODING thread's context: a worker uses a fresh one, which
/// is sound because every field this path reads is written by this function
/// (or by the tile decoders it calls) before it is read -- see the
/// `a_worker_context_starts_where_the_stream_context_would` gate.
///
/// # Errors
/// As [`decode_stream_with`], for this one frame.
#[allow(clippy::too_many_arguments)]
fn decode_frame(
    fctx: &crate::decode::FrameCtx,
    header: &FrameHeader,
    tile_bufs: &[&[u8]],
    seq: SeqFlags,
    snap: RefSnapshot<'_>,
    decode_idx: usize,
    shown_idx: usize,
    final_dump: Option<&str>,
    // lane-twostage: see `decode_inter_frame_tile_with_cdfs`'s own `stage1`.
    stage1: Option<&dyn Fn(Cdfs, crate::motion_field::MotionField, usize, usize)>,
) -> Result<FrameOutput> {
    crate::census::frame_start(header, tile_bufs.iter().map(|t| t.len()).sum(), decode_idx);
    if header.segmentation.enabled {
        // lane-hdrlossless: libaom IMPLEMENTS these three -- they rewrite a
        // block's reference / skip / mode BEFORE the symbols are read
        // (`decodemv.c:431` SEG_LVL_SKIP, `:437` SEG_LVL_REF_FRAME /
        // SEG_LVL_GLOBALMV, `:1071` in `read_ref_frames`, `:1350` in
        // `read_is_inter_block`), so a conformant stream CAN carry them and
        // this is a capability gap, not a spec-conformant guard. Reachability
        // from `aomenc` is nil, for three independent reasons (lane-av1seglvl
        // census): the AQ modules clear the table then set `SEG_LVL_ALT_Q`
        // only (`aq_variance.c:61` clear + `:89` enable, `aq_complexity.c:74`
        // + `:118`, `aq_cyclicrefresh.c:616` + `:630`); the one other setter,
        // `configure_static_seg_features` (`encoder_utils.c:315`, which would
        // set ALT_LF/REF_FRAME/SKIP around alt-ref groups), is gated on
        // `hl_sf.static_segmentation` -- a speed feature libaom's own
        // `speed_features.h` documents as "Always set to 0" -- AND on the
        // second pass of a two-pass encode (`encoder_utils.c:738`); and
        // ROI/active-map maps are control-API only (`av1_apply_roi_map`,
        // `av1_apply_active_map`: the CLI exposes no flag for either). Kept as
        // a named gap: the lift is an INTER-block-path change (ref/skip/mode
        // selection) outside this lane's region. See `lanes/av1hdr.report.md`
        // §4.1 and `lanes/av1seglvl.report.md`.
        for segment in 0..MAX_SEGMENTS {
            for feature in [SEG_LVL_REF_FRAME, SEG_LVL_SKIP, SEG_LVL_GLOBALMV] {
                if header.segmentation.feature_enabled[segment][feature] {
                    return Err(Error::unsupported(
                        "AV1 decode_stream",
                        "a frame whose segmentation enables SEG_LVL_REF_FRAME/SKIP/GLOBALMV (this decoder reads segment_id but never lets a segment override a block's reference, skip or mode)",
                    ));
                }
            }
        }
    }
    // spec `load_previous_segment_ids`: `PrevSegmentIds` comes from the
    // primary reference frame's slot, and is all-zero without one.
    decode::set_segmentation(
        header.segmentation,
        header.mi_rows as usize,
        header.mi_cols as usize,
        snap.prev_seg_map.as_deref(),
        fctx,
    );
    // lane-superres stage 2/3: `use_superres` adds/removes no per-block
    // symbol (spec 7.16's upscaling is a pixel-domain post-process
    // libaom's `decodeframe.c` runs between CDEF and loop restoration,
    // `av1_upscale_normative_*` in `resize.c`), so a key frame's tile
    // reads stay bit-exact in sync and `crate::superres::upscale_picture`
    // (below, after decode) makes its `Picture` the spec-correct
    // `upscaled_width` instead of silently staying at `frame_width`.
    // lane-superres r10: an INTER frame under `use_superres` no longer
    // refuses at the frame level. `mc::predict_scaled` (spec 7.11.3.3)
    // is wired into `decode_inter_block`'s single-ref, non-warp/OBMC/
    // interintra branch, and `decode_inter_frame_tile_with_cdfs` decodes
    // at the downscaled `frame_width` and upscales its own output the
    // same way the key-frame branch above does (see the inter branch
    // below). The combinations `predict_scaled` does not cover
    // (compound MC, warp/OBMC/interintra, `decode_inter_block8`'s 8x8
    // leaf) still refuse by name, deep in `decode.rs`, rather than
    // silently mispredicting -- see `refusal_inventory.rs`.
    // Loop restoration carries per-restoration-unit symbols in the tile
    // (spec 5.11.57 `read_lr`, one group per LR unit reached from the
    // superblock walk) -- lane-lr r2 ported the reader
    // (`crate::restoration`), r3 moved this refusal check to AFTER the
    // tile decode call below (was here, before it, in r2 -- meaning
    // `read_lr` was NEVER actually reached through `decode_stream`, the
    // gap the r3 gate caught live: 40/40 attempts hit this refusal with
    // zero `read_lr_unit` hits). Checking post-decode instead proves the
    // superblock walk actually survived `read_lr` (no desync) before
    // refusing on the true remaining gap: decoded Wiener/SGR filters are
    // stored per unit but never applied to pixels.
    // `decode_inter_block`/`decode_inter_block8`'s `GLOBALMV` arm (spec
    // 5.11.26's `read_inter_intra`... really `assign_mv`'s `GLOBALMV`
    // case, spec 7.10.2.1) uses `gm_get_motion_vector`, which is the
    // zero vector only under `GmType == IDENTITY` -- any other warp
    // model needs the full affine/translation MV derivation this
    // decoder does not carry. `global_motion` is indexed `ref_frame - 1`
    // (`read_global_motion_params`'s `i` loops from `LAST_FRAME`);
    // lane-av1refs widens this from `[0, 3]` (LAST_FRAME/GOLDEN_FRAME)
    // to every one of the 7 single-reference slots `decode_inter_block`
    // can now select.
    //
    // lane-gm r2 STEP-8 ATTEMPT (reverted): removing this refusal
    // exposed a real pixel mismatch --
    // a_real_aomenc_stream_with_interintra_wedge_decodes_pixel_exact
    // seed 43, frame 1 luma, with EC_WEDGE_GATE_ATTEMPTS=20
    // EC_AV1_REQUIRE_AOMENC=1 -- so the single-ref/compound MV wiring
    // is not yet provably correct end to end; keeping the refusal
    // until that seed is captured (EC_AV1_GATE_DUMP), self-pinned, and
    // root-caused rather than shipping a wrong decode.
    //
    // lane-gm r3 ROOT CAUSE (pinned, range-ladder-confirmed, still not
    // fixed): the mismatch is NOT an mv/mvstack/entropy bug -- msac
    // RANGE at the first divergent block (mi=(8,0), frame 1) matches
    // aomdec's own `EC_PART` trace bit-for-bit (rng=38664 both sides),
    // and this decoder's own `gm_get_motion_vector` for that block
    // (ROTZOOM, seed 43's own header) computes the spec-correct
    // mv=(-8,8) (hand-verified against mv.h's `block_center_x`/`_y`,
    // which found and fixed a real `- 1` omission in `warp.rs` --
    // harmless for THIS block's rounding but a genuine spec bug fixed
    // regardless). The actual gap: `reconinter.c`'s `allow_warp` has a
    // `global_warp_allowed` branch (`warp_types.global_warp_allowed =
    // is_global_mv_block(...)`, `reconinter_enc.c:281`) that predicts
    // an `is_global_mv_block` block (ROTZOOM/AFFINE GLOBALMV, >=8x8)
    // with the FULL per-pixel affine global-motion warp, independent
    // of `motion_mode` -- this decoder only ever builds a warp
    // prediction for local `WARPED_CAUSAL` motion mode and falls back
    // to plain translational MC with the block-centre mv for every
    // other GLOBALMV block, which is only a centre-point approximation
    // of the true per-pixel warp. That is the entire remaining gap.
    // lane-gm r4: `decode.rs` now feeds `global_motion[ref]` into
    // `crate::warp::global_warp_params` for every single-ref
    // `is_global_mv_block` (ROTZOOM/AFFINE), matching `allow_warp`'s
    // `global_warp_allowed` branch -- ROTZOOM is pin-verified
    // (`gm-seed43.obu`, frame 1 luma MATCH) and TRANSLATION already
    // worked (`gm_get_motion_vector`'s translation-only mv, no warp
    // needed). AFFINE is untested this round (no aomenc fixture in the
    // wedge gate reached a 6-parameter model) -- keep refusing it by
    // name rather than shipping an unverified decode.
    // lane-gmaffine r1: AFFINE is now GATED, not refused. Stock libaom
    // can never write a 6-parameter model -- its encoder-side search is
    // pinned to ROTZOOM (`av1/encoder/global_motion_facade.c:24`,
    // `FIRST_GLOBAL_TRANS_TYPE == LAST_GLOBAL_TRANS_TYPE == ROTZOOM`) --
    // which is why four rounds of `--enable-global-motion=1` recipes
    // never reached the case. `scripts/build-aom-affine-oracle.sh` widens
    // exactly that search range in a side build, and
    // `a_real_affine_global_motion_stream_decodes_pixel_exact` below
    // decodes its output pixel-exact against ffmpeg with
    // `affine_gm_hits() > 0`, i.e. real blocks predicted through the
    // 6-parameter warp. Nothing in the decoder needed changing: the
    // header reader already reads params 4/5 with their own subexp refs
    // (`ec-av1-syntax` `read_global_motion_params`) and `warp.rs`'s
    // shear-validity check + `warp_affine` are model-agnostic.
    // lane-gm r4/r5/r6: r4 found a NEW mismatch on frames with two
    // concurrently active ROTZOOM/AFFINE ref slots (GOLDEN + BWDREF) and
    // refused the shape by name rather than guess-fix it. r5 localized
    // the mismatch to an ordinary NEWMV/GOLDEN block, not the new
    // global-warp branch at all -- proving "two active slots" was a
    // correlated symptom, not the cause. r6 range-laddered that block
    // against aomdec's `EC_TRACE_MODE` (identical `rng` before/after
    // mode info -- symbol consumption bit-exact) and found the real
    // defect in `mvstack.rs`'s single-reference predictor fallback:
    // libaom's `setup_ref_mv_list` fills any `mv_ref_list` slot the real
    // neighbour scan left short with this block's OWN global motion
    // vector for its ref (`gm_get_motion_vector`), not zero -- invisible
    // whenever the live ref's global motion is itself
    // identity/translation, wrong the moment it's a real ROTZOOM/AFFINE
    // and the querying block's own stack comes up short (frame 14's
    // mi=(0,0), the very first block decoded, has no neighbours at
    // all). Fixed at the predictor fallback; the refusal above (single
    // untested shape: AFFINE on a single-ref frame) stays, but the
    // *multi-slot* refusal this comment used to guard is lifted -- the
    // r5/r6 pin (seed-43 wedge-gate mismatch) now decodes all 24 frames
    // byte-exact with it gone.
    // lane-av1comp: `decode_inter_block`/`decode_inter_block8` now read
    // `comp_mode` per block whenever this frame's own `reference_select`
    // header bit is set (spec 5.11.25), and refuse by name the blocks
    // that pick `COMPOUND_REFERENCE` -- two-reference motion
    // compensation is not wired yet. Round 14: `skip_mode` (spec 5.9.22)
    // is wired at the block level too (forced NEAREST_NEARESTMV compound
    // of `skip_mode_frame`, plain average blend), so the old blanket
    // `skip_mode_present` refusal is gone.
    // `context_update_tile_id` (spec `decode_tile`/`exit_symbol`) names
    // which tile's end-of-tile adapted CDFs become the frame's own.
    // lane-tiles r8: this comment used to claim a `!= 0` refusal existed
    // "further down" -- it did not (`decode.rs` reads
    // `tile_info.context_update_tile_id` generically at both the
    // key-frame and inter-frame tile loops' `result_cdfs = cdfs` sites,
    // never hardcoded to tile 0); stale documentation, not a real gap.
    // `a_real_aomenc_stream_with_four_tile_rows_decodes_pixel_exact`
    // hard-asserts a live aomenc stream naming tile 1/2/3 decodes
    // pixel-exact, so the capability is now proven as well as described
    // correctly.
    //
    // lane-tiles r4/r6/r7/r8: the multi-tile refusal itself is scoped,
    // not blanket. `decode_key_frame_tile_with_cdfs` and (r6)
    // `decode_inter_frame_tile_with_cdfs` both genuinely loop every
    // tile, `mvstack.rs`'s `MiGrid` is bounded per tile
    // (`MiGrid::set_tile_bounds`), and `PlaneBuf`'s `tile_x0/y0/x1/y1`
    // origin receives real values at every call site (r6's
    // `set_tile_origin` sweep). r7 proved the two remaining
    // tile-*column* gaps closed (inter frames, >2 columns with loop
    // filtering on). r8 proved tile *rows* the same way: the per-tile
    // loop's row math (`tile_num / tile_info.cols`,
    // `mi_row_starts`/`set_tile_origin`'s `y0`/`y1`) is exactly
    // symmetric with the column path already proven, and
    // `a_real_aomenc_stream_with_two_tile_rows_decodes_pixel_exact`
    // (bypass gate, loop filter ON) confirmed it: 20/20 pixel-exact.
    // `a_real_aomenc_stream_with_two_tile_rows_decodes_through_decode_
    // stream` below re-proves it through this entry point. Tile columns
    // and tile rows (any count, loop filter on) are no longer capped.
    // `Tile::offset` is relative to the buffer `parse_obu` was handed
    // (`&data[pos..]` at the time this OBU was parsed), so it is relative
    // to `obu_offset`, not to `data` as a whole.
    let enable_filter_intra = seq.enable_filter_intra;
    let enable_dual_filter = seq.enable_dual_filter;
    let enable_edge_filter = seq.enable_edge_filter;
    // Inter frames' intra blocks read this through a thread-local that only
    // the key-frame tile path used to set; without this call a stream whose
    // inter frames are decoded on a thread that previously decoded a
    // different sequence would inherit that sequence's bit.
    crate::decode::set_enable_edge_filter(enable_edge_filter, fctx);
    // lane-sb128 r1/r2: `use_128x128_superblock` (spec 5.5.1). Both the
    // key-frame and the inter tile path read the 128 partition root and
    // recurse SPLIT into the four 64x64 quadrants (r2 ported the root and
    // libaom's quadrant visit order into the inter loop too). A frame at
    // most 64x64 decodes identically either way (its 128 root reads no
    // symbol -- neither half is in frame, so SPLIT is inferred), so the
    // flag is only armed for frames bigger than one 64x64 superblock.
    let use_sb128 = seq.use_sb128;
    let sb128_active = use_sb128 && (header.mi_cols > 16 || header.mi_rows > 16);
    crate::decode::set_sb128(sb128_active, fctx);
    // lane-hbd r5: `BIT_DEPTH` (decode.rs) drives `crate::decode::sample_max`,
    // which every dequant/inverse-transform clamp reads -- set per frame,
    // not once per process, for the same cross-sequence-on-one-thread
    // reason as `set_enable_edge_filter` above. Defaults to 8 when no
    // sequence header has been seen yet, matching every existing fixture.
    let bit_depth = seq.bit_depth;
    // lane-av112bit: the blanket 12-bit refusal is LIFTED for the paths the
    // lane's witnesses prove byte-exact against ffmpeg on real `aomenc
    // --bit-depth=12` streams (key frame and inter sequence, CDEF and loop
    // restoration ON, grain off -- gates
    // `a_real_aomenc_12bit_stream_decodes_pixel_exact` and
    // `a_real_aomenc_12bit_inter_sequence_decodes_pixel_exact`). The
    // rounding sites that move at 12 bits are parameterised from libaom's
    // own rule (`mc.rs`'s `round_pair` -- convolve.h:81..86's `intbufrange`
    // delta: round_0 += bd-10, non-compound round_1 -= bd-10; the Wiener
    // pair via `get_conv_params_wiener`; the compound post-rounds absorb
    // the CONV_BUF gain), and CDEF/deblock/quant tables/SGR/palette reads/
    // superres upscale were already bit-depth generic (10-bit-proven).
    // Everything no 12-bit witness reached stays refused BY NAME at its own
    // site: warp (decode.rs, warp's reduce bits inherit the 12-bit round_0)
    // and any per-feature gap the witnesses' counters prove unreached -- see
    // the 12-bit refusals in decode.rs and `refusal_inventory.rs`.
    // lane-av112bitg: film grain joined the witnessed set (`grain_refusal`
    // deleted; the two-arm witness is
    // `a_real_aomenc_12bit_film_grain_stream_decodes_pixel_exact`).
    // lane-av1txr: `color_config.subsampling_x/y` (spec 5.5.2) are parsed by
    // `ec-av1-syntax`. This block used to REFUSE every `ss_x != ss_y` header by
    // name, on the theory that every chroma extent was hardcoded 4:2:0
    // (`bw / 2, bh / 2` at every rect site, `chroma_side = side / 2` in
    // `decode_block`). Both halves of that theory are now false: the per-axis
    // port replaced every one of those extents with `>> ss_x` / `>> ss_y`
    // reads, and 4:4:4 (0,0) already decoded through them.
    // lane-av1422lift: 4:2:2 (1,0) is DECODED. The refusal string, its
    // `refusal_inventory.rs` row and its `Proof::NegativeGate` pairing are
    // deleted in this commit -- a refused format that decodes is a false
    // claim, and the gate that asserted the refusal could not fail on a tree
    // where 4:2:2 decode was completely broken.
    // lane-av1odd440, kept as an ASSERTION: `subsampling_y` is coded ONLY when
    // `subsampling_x` is 1 (spec 5.5.2's `color_config`; libaom
    // `av1_read_color_config`, `av1/decoder/decodeframe.c:4171-4175`, and its
    // writer's own `assert(seq_params->subsampling_y == 0 && "4:4:0
    // subsampling not allowed in AV1")`, `av1/encoder/bitstream.c:2467-2468`),
    // so a parsed header can carry (1,1), (1,0) or (0,0) and never (0,1):
    // 4:4:0 is not a cell a decoder can be handed at all. That used to be a
    // comment plus a refusal string naming the shape; with the refusal gone
    // the residual is an unreachable arm, and this repo closes those with an
    // assertion, never prose. The gate `the_440_cell_is_not_a_codable_chroma_
    // shape` proves the whole reachable set through this crate's own writer
    // AND reader.
    assert!(
        !(seq.subsampling_x == 0 && seq.subsampling_y == 1),
        "AV1 decode_stream: a parsed color_config carried 4:4:0 (subsampling_x=0, \
         subsampling_y=1); subsampling_y is coded only when subsampling_x is 1 \
         (spec 5.5.2), so this shape cannot be produced by ec-av1-syntax's \
         sequence_header -- the parser is wrong, not the caller"
    );
    // lane-av1-qmatrix: `quantization_params.using_qmatrix` + `qm_y`/`qm_u`/
    // `qm_v` (spec 5.9.12) select per-plane INVERSE QUANTISATION MATRICES --
    // spec 7.12.3's per-position `dqv` scale, libaom `get_dqv` over
    // `iwt_matrix_ref`. The parsed 4-bit values ARE the table levels
    // (libaom `quant_params->qmatrix_level_y/u/v`); 15 is libaom's flat
    // NULL-table sentinel, and a `using_qmatrix == 0` frame keeps all three
    // parked there, selecting no matrix. Frame-constant: recorded here (the
    // spot where this decoder refused such frames until this lane), read by
    // every dequantisation call site through [`crate::decode::block_iqmatrix`].
    // A real `aomenc --enable-qm=1` stream (`using_qmatrix = 1`, levels 5)
    // decodes byte-exact vs ffmpeg; the witness is
    // `a_real_aomenc_quantisation_matrix_stream_decodes_pixel_exact` below.
    let q = &header.quantization;
    // When the bit is UNSET the parsed 4-bit fields read 0 but mean nothing
    // (spec: they are not even coded): libaom parks the level at
    // `NUM_QM_LEVELS - 1`, its flat NULL-table sentinel
    // (`setup_segmentation_dequant`: `use_qmatrix ? qmatrix_level_y : 15`),
    // so a matrix-free frame selects no matrix.
    let flat = crate::qm::NUM_QM_LEVELS as u8 - 1;
    let levels = if q.using_qmatrix {
        [q.qm_y, q.qm_u, q.qm_v]
    } else {
        [flat; 3]
    };
    crate::decode::set_qm_levels(levels, fctx);
    // lane-av112bit's screen-content refusal is LIFTED by lane-av1screen12.
    // The claim it carried -- "a 12-bit frame with screen content tools
    // (allow_screen_content_tools=1: neither palette nor intrabc has a
    // 12-bit witness)" -- named a gap WIDER than the one it was written for:
    // `allow_screen_content_tools` only decides whether an intra block reads
    // the `palette_y_mode`/`palette_uv_mode` symbols and whether a leaf reads
    // `use_intrabc`, and BOTH of those reconstructions read their colours and
    // their frame copy at the frame's own `bit_depth` (`read_palette_colors_y`
    // / `_uv` take `bit_depth(fctx)`, the colour-index map writes u16 samples).
    // Nothing about them is 8/10-bit-shaped.
    //
    // Measured, with this guard lifted, all three arms decode BYTE-EXACT
    // against the instrumented `aomdec --rawvideo` AND against ffmpeg, each
    // with a +1 control on the oracle's own bytes:
    //
    // | arm | pin | frames | screen | allow_intrabc | palette / palette-UV / intrabc |
    // | --- | --- | --- | --- | --- | --- |
    // | palette | `screen12_palette.obu` | 2 | 2 | 0 | 76 / 43 / 0 |
    // | palette+intrabc | `screen12_intrabc.obu` | 1 | 1 | 1 | 36 / 34 / 11 |
    // | lossless+palette | `screen12_lossless_palette.obu` | 2 | 2 | 0 | 80 / 59 / 0 |
    //
    // Gates: `a_real_aomenc_12bit_screen_content_palette_stream_decodes_pixel_exact`,
    // `a_real_aomenc_12bit_intrabc_screen_stream_decodes_pixel_exact`,
    // `a_real_aomenc_12bit_lossless_screen_content_stream_decodes_pixel_exact`.
    //
    // ON THE OLD "INTRABC IS UNPRODUCIBLE" NOTE: the earlier claim that
    // 12-bit `allow_intrabc` has no producer was a RECIPE artefact, not an
    // encoder limit. libaom decides that frame-header bit from its own
    // content detector (`encoder.c:2404`), not from `--tune-content=screen`:
    // an untiled `smptebars=size=128x96` source reads `allow_intrabc = 0`
    // with AND without the flag, while the 256x192 `-vf tile=2x2` source the
    // 8/10-bit intrabc census already uses reads `allow_intrabc = 1` with
    // and without it -- at 8, at 10 AND at 12 bits (measured).
    // lane-dpm1: a LOSSLESS frame (`qindex == 0` with zero deltas, spec 5.9.12)
    // is a different tile syntax, and this decoder has none of it: libaom
    // forces TX_4X4 with the WALSH-HADAMARD transform over every block (so no
    // `tx_depth` symbol is coded at all), and `is_cfl_allowed` narrows to
    // `plane_bsize == BLOCK_4X4` instead of the 32x32 rule, which changes the
    // `uv_mode` ALPHABET. Measured on his own screen-capture row at libaom
    // `-crf 5` (base_q_idx 0): the very first block, mi(0,0) BLOCK_32X32,
    // reads `uv_mode` off the CfL alphabet where libaom reads the plain one
    // (ours 12 = UV_CFL_PRED, aomdec 1 = V_PRED) and the tile desyncs at
    // symbol four of the frame. Refuse by name rather than return a picture
    // that is silently wrong from luma sample 0 (class `refusal-hides-a-defect`
    // read the other way: a silent wrong decode is worse than a named gap).
    // lane-lossless: `CodedLossless` (spec 5.9.12) -- every segment at
    // `qindex == 0` with zero plane deltas. The tile reader then forces TX_4X4
    // with the Walsh-Hadamard transform on every plane, codes no `tx_type`
    // symbol and narrows `is_cfl_allowed` to `plane_bsize == BLOCK_4X4`
    // (libaom `read_tx_size`/`read_tx_type`/`is_cfl_allowed`).
    //
    // lane-av1mixloss: PER-SEGMENT lossless (libaom `decodeframe.c:5205`) --
    // the header parser computes `lossless[segment_id]` for all 8 segments
    // (`qindex == 0` with zero plane deltas, `segment_qindex` applying that
    // segment's own `SEG_LVL_ALT_Q`) and `coded_lossless` = every segment
    // lossless. Everything FRAME-level already keys on the header's fields,
    // exactly as libaom gates them on `features->coded_lossless`/
    // `all_lossless`: `tx_mode` is `Only4x4` only when EVERY segment is
    // lossless, CDEF and the loop-filter levels are forced to 0 only then,
    // and loop restoration only on `all_lossless`. What this decoder lacked
    // was the BLOCK level: every block-decoder rule -- `read_tx_size`'s TX_4X4
    // early return, the suppressed `tx_type`, the Walsh-Hadamard,
    // `is_cfl_allowed`'s narrowing, the 4x4 chroma rasters -- indexes
    // `xd->lossless[segment_id]`, so a frame whose segments DISAGREE codes
    // lossless blocks inside a lossy frame. aomenc WRITES one:
    // `--end-usage=q --cq-level=0 --aq-mode=1` (variance AQ over a lossless
    // base) gives a high-variance segment a positive `SEG_LVL_ALT_Q` while a
    // flat one stays at `qindex == 0`. The table goes in whole and
    // [`decode::lossless`] resolves it per block through the block's own
    // `segment_id` (set by the spec-order `segment_id` read that precedes
    // every mode/transform symbol). Witness:
    // `a_real_aomenc_mixed_lossless_segment_frame_decodes_sample_exact`.
    crate::decode::set_lossless_per_seg(header.lossless, fctx);
    crate::decode::set_bit_depth(bit_depth, fctx);
    // lane-av1mono: the sequence header's `mono_chrome`, set per frame like
    // the bit depth so a mono frame decoded on a thread that last saw a
    // 4:2:0 one never inherits the wrong plane count.
    crate::decode::set_mono(seq.mono_chrome, fctx);
    crate::decode::set_subsampling(seq.subsampling_x, seq.subsampling_y, fctx);
    // lane-av1readcensus: the coefficient-unit census is armed here, off the
    // same sequence-header subsampling. lane-av1422census422 widened it from
    // 4:4:4-only to "any format with an unsubsampled luma axis", so a 4:2:2
    // cell's per-plane unit counts are readable and a 4:2:2 gate can assert
    // a non-vacuous walk; 4:2:0 still reads zero, so the 4:2:0 control in
    // `a_pixel_exact_444_stream_walks_the_same_coefficient_units_the_oracle_does`
    // keeps asserting exactly that.
    crate::decode::set_census_nonsub(seq.subsampling_x, seq.subsampling_y);
    // lane-av1txr: spec 5.9.2 `disable_cdf_update` -- a frame that sets it
    // codes every tile symbol against the CDFs it started with, so the tile
    // readers must not adapt (libaom `decodeframe.c:2909`:
    // `allow_update_cdf = allow_update_cdf && !cm->features.disable_cdf_update`).
    crate::decode::set_disable_cdf_update(header.disable_cdf_update, fctx);
    // spec 7.16: this frame's superres state, for the filter chain --
    // `decode.rs` upscales between CDEF and loop restoration and sizes
    // the LR unit grid in upscaled coordinates. Cleared (0, 8) on an
    // unscaled frame so a later frame never inherits it.
    crate::decode::set_superres(
        if header.use_superres {
            header.upscaled_width
        } else {
            0
        },
        u32::from(header.superres_denom),
        fctx,
    );
    // lane-av1comp: `comp_group_idx`/`compound_idx`'s own gating bits.
    let enable_masked_compound = seq.enable_masked_compound;
    let enable_jnt_comp = seq.enable_jnt_comp;
    // lane-sb128 r4: `interintra`'s own gating bit -- see decode.rs's
    // read site doc.
    let enable_interintra_compound = seq.enable_interintra_compound;
    let interp_fixed = match header.interpolation_filter {
        ec_av1_syntax::InterpolationFilter::Switchable => None,
        fixed => Some(crate::mc::InterpFilterKind::from_header(fixed)),
    };
    // Spec 7.20 `load_cdfs`: a frame naming a `primary_ref_frame` resumes
    // from that reference slot's saved CDF state instead of the spec 8.4
    // defaults `decode_key/inter_frame_tile_with_cdfs`'s `None` case
    // builds.
    // lane-hdrlossless: libaom errors on the same stream (`decodeframe.c:5075`:
    // `if (features->primary_ref_frame != PRIMARY_REF_NONE &&
    // get_primary_ref_frame_buf(cm) == NULL) aom_internal_error(...,
    // AOM_CODEC_CORRUPT_FRAME, "Reference frame containing this frame's
    // initial frame context is unavailable.")`) -- a spec-conformant guard.
    let initial_cdfs = if header.primary_ref_frame == PRIMARY_REF_NONE {
        None
    } else {
        Some(snap.primary_cdfs.clone().ok_or_else(|| {
            Error::unsupported(
                "AV1 decode_stream",
                "a frame naming primary_ref_frame at a reference slot with no saved CDF state",
            )
        })?)
    };
    let started_from = initial_cdfs.clone();
    // lane-frame80: narrow the EC_TRACE_COEFF rungs to one DECODE-order frame.
    decode::set_coeff_trace_frame(decode_idx);
    // lane-t900 r9: one line per DECODE-order frame naming the header
    // fields a desync cliff is usually keyed on (frame type, base q,
    // primary_ref, the 7 ref slots, refresh mask, order hints).
    if std::env::var_os("EC_PROBE_HDR").is_some() {
        eprintln!(
            "EC_HDR decode_idx={decode_idx} type={:?} show={} show_existing={}                  base_q={} primary_ref={} ref_idx={:?} refresh=0x{:02x} order_hint={}                  tiles={}x{} txmode={:?} interp={:?} reduced_tx={} allow_warp={}                  switchable_mo={} ref_mvs={} skip_mode={} cdef={:?} lr={:?}",
            header.frame_type,
            header.show_frame,
            header.show_existing_frame,
            header.quantization.base_q_idx,
            header.primary_ref_frame,
            header.ref_frame_idx,
            header.refresh_frame_flags,
            header.order_hint,
            header.tile_info.cols,
            header.tile_info.rows,
            header.tx_mode,
            header.interpolation_filter,
            header.reduced_tx_set,
            header.allow_warped_motion,
            header.is_motion_mode_switchable,
            header.use_ref_frame_mvs,
            header.skip_mode_present,
            header.cdef.bits,
            header.loop_restoration.frame_restoration_type,
        );
    }

    let order_hint_bits = seq.order_hint_bits;
    // spec 7.9/7.20: this frame's own `ref_order_hints` -- the OrderHint
    // of the picture sitting in each of its 7 single references' DPB
    // slots, for this frame's *own* saved [`crate::motion_field::MotionField`]
    // (a later frame projects from it) whether or not this frame itself
    // reads temporal candidates.
    let ref_order_hints = snap.ref_order_hints;

    crate::timeline::bind(
        decode_idx,
        if header.frame_type == FrameType::Key {
            'K'
        } else if header.show_frame {
            'P'
        } else {
            'A'
        },
        header.show_frame,
    );
    crate::timeline::hdr_done();
    let (picture, end_cdfs, motion_field) = if header.frame_type == FrameType::Key {
        let (picture, end_cdfs) = decode_key_frame_tile_with_cdfs(
            tile_bufs,
            &header.tile_info,
            header.mi_cols,
            header.mi_rows,
            header.quantization.base_q_idx,
            crate::quant::QuantDeltas {
                y_dc: i32::from(header.quantization.delta_q_y_dc),
                u_dc: i32::from(header.quantization.delta_q_u_dc),
                u_ac: i32::from(header.quantization.delta_q_u_ac),
                v_dc: i32::from(header.quantization.delta_q_v_dc),
                v_ac: i32::from(header.quantization.delta_q_v_ac),
            },
            header.frame_width,
            header.frame_height,
            enable_filter_intra,
            enable_edge_filter,
            &header.cdef,
            &header.loop_filter,
            &header.loop_restoration,
            initial_cdfs,
            header.tx_mode == TxMode::Select,
            header.reduced_tx_set,
            header.allow_screen_content_tools,
            header.allow_intrabc,
            header.delta,
            fctx,
        )?;
        // A key frame codes no inter blocks -- its own saved motion
        // field has no cells set, matching libaom's own "intra frame
        // contributes nothing to a later projection" behaviour, but
        // still carries `order_hint`/`ref_order_hints` for the distance
        // arithmetic a later frame's projection needs.
        let motion_field = crate::motion_field::MotionField::new(
            header.mi_cols as usize,
            header.mi_rows as usize,
            header.order_hint,
            ref_order_hints,
            // `frame_type` for the DPB slot: a key (or intra-only) frame is
            // never a projection source, and never spends a `ref_stamp`.
            true,
        );
        (picture, end_cdfs, Some(motion_field))
    } else {
        let last_slot = header.ref_frame_idx[0] as usize;
        let _ = last_slot;
        // lane-hdrlossless: libaom errors on the same stream -- a stream may
        // OPEN on an intra-only frame, but a reference resolving to a slot
        // that is still NULL is `AOM_CODEC_CORRUPT_FRAME "Inter frame
        // requests nonexistent reference"` (`decodeframe.c:5008-5031`) -- a
        // spec-conformant guard.
        if snap.refpix.dims(1).is_none() {
            return Err(Error::unsupported(
                "AV1 decode_stream",
                "an inter frame with no key frame before it",
            ));
        }
        // `ref_frame_idx[i]` names the DPB slot for `LAST_FRAME + i`
        // (spec 7.8/7.20). Any of them having no picture yet is not a
        // stream error on its own: `decode_inter_block` only needs a
        // given slot when a block actually selects that reference, and
        // refuses by name there if the slot is still empty -- widened
        // (lane-av1refs) from `GOLDEN_FRAME`'s own slot alone to every
        // one of the 7 single-reference slots.
        // lane-fimv r1: record the header state the fimv gate asserts on
        // (`motion_mode_allowed`'s `!cur_frame_force_integer_mv` clause).
        if header.force_integer_mv {
            decode::note_force_integer_mv_frame();
        }
        // spec 7.9's own driver, `av1_setup_motion_field`: only run when
        // this frame's header actually asks for temporal candidates --
        // `find_mv_stack_with_sign_bias` reproduces its old always-`None`
        // behaviour bit for bit when this is `None` (see its own doc).
        // lane-twostage: the stage-1 sink is handed the table a consumer
        // would have read out of the slot, not the raw end-of-tile one.
        let started_from_stage1 = started_from.clone();
        let stage1_adapter = stage1.map(|sink| {
            move |end_cdfs: Cdfs,
                  motion_field: crate::motion_field::MotionField,
                  width: usize,
                  height: usize| {
                sink(
                    stored_cdfs_for(header, end_cdfs, started_from_stage1.clone()),
                    motion_field,
                    width,
                    height,
                )
            }
        });
        let tpl_field = &snap.tpl_field;
        let (picture, end_cdfs, motion_field) = decode_inter_frame_tile_with_cdfs(
            tile_bufs,
            &header.tile_info,
            header.mi_cols,
            header.mi_rows,
            header.quantization.base_q_idx,
            crate::quant::QuantDeltas {
                y_dc: i32::from(header.quantization.delta_q_y_dc),
                u_dc: i32::from(header.quantization.delta_q_u_dc),
                u_ac: i32::from(header.quantization.delta_q_u_ac),
                v_dc: i32::from(header.quantization.delta_q_v_dc),
                v_ac: i32::from(header.quantization.delta_q_v_ac),
            },
            header.frame_width,
            header.frame_height,
            snap.refpix,
            &header.cdef,
            &header.loop_filter,
            &header.loop_restoration,
            initial_cdfs,
            header.allow_high_precision_mv,
            header.force_integer_mv,
            header.ref_frame_sign_bias,
            header.global_motion,
            interp_fixed,
            enable_dual_filter,
            order_hint_bits,
            header.order_hint,
            ref_order_hints,
            tpl_field.as_ref(),
            header.reference_select,
            enable_masked_compound,
            enable_jnt_comp,
            enable_interintra_compound,
            header.skip_mode_present,
            header.skip_mode_frame,
            header.reduced_tx_set,
            header.tx_mode == TxMode::Select,
            header.is_motion_mode_switchable,
            header.allow_warped_motion,
            header.allow_screen_content_tools,
            header.delta,
            enable_filter_intra,
            stage1_adapter
                .as_ref()
                .map(|f| f as &dyn Fn(Cdfs, crate::motion_field::MotionField, usize, usize)),
            fctx,
        )?;
        (picture, end_cdfs, motion_field)
    };
    // lane-lr r4: the Wiener/self-guided pixel filters are wired
    // (`decode.rs::apply_loop_restoration`) -- no more refusal here,
    // `uses_lr` frames now decode all the way to pixels.
    // Spec 7.20: `disable_frame_end_update_cdf` stores the frame's
    // *initial* table into the slots it refreshes, not the adapted
    // end-of-tile one.
    let stored_cdfs = stored_cdfs_for(header, end_cdfs, started_from);
    if let Ok(path) = std::env::var("EC_AV1_DUMP_TABLES") {
        // lane-av1dumploud: loud on a short write. This one is APPEND-mode and
        // spans every frame of a run, so there is no total length to declare
        // (`append_unknown` says so on the failure line rather than guessing
        // one); the per-line writes are still checked, so an ENOSPC halfway
        // through a table no longer passes as a complete one.
        let mut f = crate::dumpio::LoudDump::append_unknown("EC_AV1_DUMP_TABLES", &path);
        let mut line = |s: String| f.write(s.as_bytes());
        line(format!("=== frame idx={shown_idx} ===\n"));
        line(format!("partition_w64 {:?}\n", stored_cdfs.partition_w64));
        line(format!("partition_w32 {:?}\n", stored_cdfs.partition_w32));
        line(format!("partition_w16 {:?}\n", stored_cdfs.partition_w16));
        line(format!("skip {:?}\n", stored_cdfs.skip));
        line(format!("new_mv {:?}\n", stored_cdfs.new_mv));
        line(format!("zero_mv {:?}\n", stored_cdfs.zero_mv));
        line(format!("ref_mv {:?}\n", stored_cdfs.ref_mv));
        line(format!("mv_joint {:?}\n", stored_cdfs.mv_joint));
        f.finish();
    }
    // lane-comppin r3: decode-order (not display-order) dump of every
    // frame's own post-deblock buffer -- the pre-existing pixel diff
    // (`scratch_isolate_pinned_mismatch`) only ever compares *shown*
    // frames, so a hidden reference (altref/bwdref) that DPB-corrupts a
    // later shown frame is otherwise invisible; diff this byte-for-byte
    // against `EC_AV1_POSTDEBLOCK_DUMP.fN` from the instrumented aomdec build
    // (rung 6, `scripts/instrument-aom-oracle.sh`; 1 byte per sample, so it is
    // byte-comparable only at 8-bit -- for HBD use rung 12 `EC_AV1_FINAL_DUMP`)
    // to isolate whether the defect is in a hidden frame's own
    // reconstruction or downstream of it.
    if let Ok(path) = std::env::var("EC_AV1_DECODE_ORDER_DUMP") {
        // lane-hbd r4: debug dump narrows to u8 -- this diagnostic
        // predates 10-bit support and is an 8-bit-oracle comparison
        // only (aomdec's own dump is 8-bit here too).
        //
        // lane-av1dumploud: loud on a short write; expected length is the three
        // planes' own lengths (narrowing is 1 byte per sample, so the count is
        // unchanged).
        let narrow = |v: &[u16]| -> Vec<u8> { v.iter().map(|&s| s as u8).collect() };
        crate::dumpio::write_planes(
            "EC_AV1_DECODE_ORDER_DUMP",
            format!("{path}.f{decode_idx}"),
            &[
                &narrow(&picture.y),
                &narrow(&picture.u),
                &narrow(&picture.v),
            ],
        );
    }
    // lane-hidden r1: the frame exactly as it is about to be stored into
    // the reference slots -- after deblock/CDEF/LR/superres, in DECODE
    // order, hidden (`show_frame == 0`) alt-ref frames included, and
    // bit-depth correct (8-bit as u8, 10/12-bit as u16 LE) unlike the
    // u8-narrowing diagnostic dumps above. Matches the instrumented
    // aomdec's own `EC_AV1_FINAL_DUMP` rung byte for byte
    // (scripts/instrument-aom-oracle.sh rung 12), which is what makes a
    // hidden frame's pixels comparable at all.
    // lane-unwritten-dep: the census AT the output point, beside the dump that
    // defines "the output" -- the same bytes, so the claim is about the frame the
    // caller receives rather than about an intermediate stage.
    //
    // WHAT THIS IS FOR, corrected: an earlier revision of this comment said the
    // pre-deblock plane was byte-identical plain-vs-sentinel and that deblock
    // creates the dependence. That was wrong, and it was `dump_stage16`'s fault:
    // it cropped chroma as `(fw/2, fh/2)`, a 4:2:0 assumption, so on a 64x64
    // 4:2:2 frame 2048 chroma samples appeared in no stage dump at all -- exactly
    // the region the difference lives in. With per-axis extents the corrected
    // measurement on `440_request_is_422` is that the PRE-DEBLOCK dump already
    // differs by 2158 bytes and an exact u16 census finds 224 unwritten samples
    // (U 112, V 112, rows 36..63) -- the same count `lanes/av1unwritten.report.md`
    // reports. The hole is present before any filter runs, and the pre-deblock
    // census SEES it.
    //
    // So this second census is not a patch for a blind pre-deblock scan. It is
    // the measurement taken where the reference-bank bytes are taken, which
    // covers the whole span from the tile walk to those bytes: every filter
    // writes IN PLACE into the very buffer this scans, so nothing a later
    // filter does can open a hole outside its extent.
    //
    // lane-av1graincensus, the scope correction this comment needed: "to the
    // caller's frame" was WRONG, and this call is not what makes it right.
    // `apply_grain` runs after this and ALLOCATES a fresh picture
    // (`film_grain::uninit_plane`, `with_capacity` + `set_len`, no fill), so
    // that destination never passes through here. The claim over the PRE-GRAIN
    // planes -- the bytes the reference bank stores and every later frame
    // predicts from -- is what this line supports; the grained picture the
    // caller receives is censused by `decode::census_unwritten_grained`, at
    // `apply_grain`'s own return, over its own counters.
    decode::census_unwritten_final(&picture.y, &picture.u, &picture.v);
    if let Some(prefix) = final_dump {
        // lane-av1dumploud: this is the site that produced the 13 320 192-byte
        // truncated frame in `lanes/unwritten-dep.report.md` -- `let _ =
        // f.write_all(&buf)` swallowed the ENOSPC. Expected length is derived
        // from the PLANES and the sample width, independently of `buf`, so a
        // writer that builds the wrong buffer still gets caught.
        let sample_width = if bit_depth == 8 { 1 } else { 2 };
        let expected = (picture.y.len() + picture.u.len() + picture.v.len()) * sample_width;
        let mut f = crate::dumpio::LoudDump::create(
            "EC_AV1_FINAL_DUMP",
            format!("{prefix}.f{decode_idx}"),
            expected,
        );
        for plane in [&picture.y, &picture.u, &picture.v] {
            if bit_depth == 8 {
                f.write(&plane.iter().map(|&s| s as u8).collect::<Vec<u8>>());
            } else {
                let mut row = Vec::with_capacity(plane.len() * 2);
                for &s in plane {
                    row.extend_from_slice(&s.to_le_bytes());
                }
                f.write(&row);
            }
        }
        f.finish();
    }
    if let Some(why) = fctx.corrupt.get() {
        return Err(Error::corrupt(format!("AV1 frame {decode_idx}: {why}")));
    }
    Ok(FrameOutput {
        picture,
        stored_cdfs,
        motion_field,
        seg_map: decode::take_segment_ids(fctx),
    })
}

/// lane-intra14 r1: INTRA-coded 1:4 strips decoded on the INTER block path
/// (64x16, 16x64, 32x8, 8x32) -- the counters a recipe sweep needs from
/// outside the crate.
pub fn intra_rect4_in_inter_counters() -> (usize, usize, usize, usize) {
    (
        crate::decode::intra_rect4_strip_in_inter_hits(0),
        crate::decode::intra_rect4_strip_in_inter_hits(1),
        crate::decode::intra_rect4_strip_in_inter_hits(2),
        crate::decode::intra_rect4_strip_in_inter_hits(3),
    )
}

/// lane-av1recipehunt: the recipe sweep's view of the `delta_q` story from
/// OUTSIDE the crate -- `(delta_q symbol groups read, rect64 dequant calls that
/// saw `CURRENT_Q_IDX != base_q_idx`)`. A recipe that reads delta_q but never
/// lands one inside a rect64 root is a different failure from one that reads
/// neither, and the gate can only tell them apart with both numbers.
pub fn rect64_qidx_drift_counters() -> (usize, usize) {
    (
        crate::decode::delta_q_hits(),
        crate::decode::rect64_qidx_drift_hits(),
    )
}

#[cfg(test)]
pub(crate) mod tests;
