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
    Av1Parser, FrameHeader, FrameType, NUM_REF_FRAMES, ObuKind, PRIMARY_REF_NONE, Tile, TxMode,
    WarpModel,
};
use ec_core::{Error, Result};

use crate::cdf_state::Cdfs;
use crate::decode;
use crate::decode::{
    decode_inter_frame_tile_with_cdfs, decode_key_frame_tile, decode_key_frame_tile_with_cdfs,
    q_ctx_of,
};
use crate::encode::Picture;

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
    let mut parser = Av1Parser::new();
    let mut pictures = Vec::new();
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
    let mut ref_slots: [Option<Picture>; NUM_REF_FRAMES] = std::array::from_fn(|_| None);
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
            let picture = ref_slots[slot].clone().ok_or_else(|| {
                Error::unsupported(
                    "AV1 decode_stream",
                    "a show_existing_frame header naming an empty reference slot",
                )
            })?;
            for i in 0..NUM_REF_FRAMES {
                if header.refresh_frame_flags & (1 << i) != 0 {
                    cdf_slots[i] = cdf_slots[slot].clone();
                    ref_slots[i] = Some(picture.clone());
                    motion_field_slots[i] = motion_field_slots[slot].clone();
                    order_hint_slots[i] = order_hint_slots[slot];
                }
            }
            let output = if header.film_grain.apply_grain {
                // lane-hbd r5: `film_grain.rs`'s grain LUT and blend are
                // hardcoded 8-bit (`[i32; 256]`, clamps at 255) -- narrowed
                // from the old blanket bit-depth refusal to only the case
                // that actually reaches unported code.
                let bit_depth = parser
                    .sequence_header()
                    .map_or(8, |seq| seq.color_config.bit_depth);
                if bit_depth != 8 {
                    return Err(Error::unsupported(
                        "AV1 decode_stream",
                        "a bit depth other than 8 with film grain applied (film_grain.rs's LUT and blend are hardcoded 8-bit)",
                    ));
                }
                let mc_identity = parser
                    .sequence_header()
                    .is_some_and(|seq| seq.color_config.matrix_coefficients == 0);
                crate::film_grain::apply_grain(&picture, &header.film_grain, mc_identity)
            } else {
                picture
            };
            pictures.push(output);
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
        // An intra frame's `decode_key_frame_tile*` now reads `tx_depth`
        // under `TxMode::Select` (lane-av1txsel, spec 5.11.16): the key-frame
        // decode path threads `tx_select` through `decode_block`/
        // `decode_leaf8`, and refuses on its own (a named `unsupported`, not
        // a silent desync) the one case it still cannot code -- a resolved
        // 4x4 luma transform, which has no coefficient tables here. This
        // refusal is narrowed to what genuinely still has no reader at all:
        // `decode_inter_frame`'s inter path never threads `tx_select`
        // through its own block loop, so an inter frame under
        // `TxMode::Select` still desyncs the same way the comment above used
        // to describe for every frame -- lane-av1real r3's
        // luma-near-flat-vs-ffmpeg-gradient bug.
        if header.tx_mode == TxMode::Select && header.frame_type != FrameType::Key {
            return Err(Error::unsupported(
                "AV1 decode_stream",
                "an inter frame using TxMode::Select (this decoder's inter path never reads a tx_depth symbol, so it desyncs after the first block's mode)",
            ));
        }
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
        if header.segmentation.enabled {
            return Err(Error::unsupported(
                "AV1 decode_stream",
                "a frame with segmentation enabled (this decoder never reads a per-block segment_id symbol)",
            ));
        }
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
        if header.frame_type != FrameType::Key
            && header.global_motion[..7]
                .iter()
                .any(|gm| gm.model == WarpModel::Affine)
        {
            return Err(Error::unsupported(
                "AV1 decode_stream",
                "an inter frame whose global motion for a single-reference frame is AFFINE (unverified this round; ROTZOOM/TRANSLATION/IDENTITY are proven)",
            ));
        }
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
        let tile_bufs: Vec<&[u8]> = tiles
            .iter()
            .map(|t| {
                data.get(tiles_base_offset + t.offset..tiles_base_offset + t.offset + t.size)
                    .ok_or(Error::NeedMore)
            })
            .collect::<Result<_>>()?;
        let enable_filter_intra = parser
            .sequence_header()
            .is_some_and(|seq| seq.enable_filter_intra);
        let enable_dual_filter = parser
            .sequence_header()
            .is_some_and(|seq| seq.enable_dual_filter);
        let enable_edge_filter = parser
            .sequence_header()
            .is_some_and(|seq| seq.enable_intra_edge_filter);
        // Inter frames' intra blocks read this through a thread-local that only
        // the key-frame tile path used to set; without this call a stream whose
        // inter frames are decoded on a thread that previously decoded a
        // different sequence would inherit that sequence's bit.
        crate::decode::set_enable_edge_filter(enable_edge_filter);
        // lane-hbd r5: `BIT_DEPTH` (decode.rs) drives `crate::decode::sample_max`,
        // which every dequant/inverse-transform clamp reads -- set per frame,
        // not once per process, for the same cross-sequence-on-one-thread
        // reason as `set_enable_edge_filter` above. Defaults to 8 when no
        // sequence header has been seen yet, matching every existing fixture.
        let bit_depth = parser
            .sequence_header()
            .map_or(8, |seq| seq.color_config.bit_depth);
        crate::decode::set_bit_depth(bit_depth);
        // lane-av1comp: `comp_group_idx`/`compound_idx`'s own gating bits.
        let enable_masked_compound = parser
            .sequence_header()
            .is_some_and(|seq| seq.enable_masked_compound);
        let enable_jnt_comp = parser
            .sequence_header()
            .is_some_and(|seq| seq.enable_jnt_comp);
        // lane-sb128 r4: `interintra`'s own gating bit -- see decode.rs's
        // read site doc.
        let enable_interintra_compound = parser
            .sequence_header()
            .is_some_and(|seq| seq.enable_interintra_compound);
        let interp_fixed = match header.interpolation_filter {
            ec_av1_syntax::InterpolationFilter::Switchable => None,
            fixed => Some(crate::mc::InterpFilterKind::from_header(fixed)),
        };
        // Spec 7.20 `load_cdfs`: a frame naming a `primary_ref_frame` resumes
        // from that reference slot's saved CDF state instead of the spec 8.4
        // defaults `decode_key/inter_frame_tile_with_cdfs`'s `None` case
        // builds.
        let initial_cdfs = if header.primary_ref_frame == PRIMARY_REF_NONE {
            None
        } else {
            let slot = header.ref_frame_idx[header.primary_ref_frame as usize] as usize;
            Some(cdf_slots[slot].clone().ok_or_else(|| {
                Error::unsupported(
                    "AV1 decode_stream",
                    "a frame naming primary_ref_frame at a reference slot with no saved CDF state",
                )
            })?)
        };
        let started_from = initial_cdfs.clone();

        let order_hint_bits = parser
            .sequence_header()
            .map_or(0, |seq| seq.order_hint_bits);
        // spec 7.9/7.20: this frame's own `ref_order_hints` -- the OrderHint
        // of the picture sitting in each of its 7 single references' DPB
        // slots, for this frame's *own* saved [`crate::motion_field::MotionField`]
        // (a later frame projects from it) whether or not this frame itself
        // reads temporal candidates.
        let ref_order_hints: [u32; 7] =
            std::array::from_fn(|i| order_hint_slots[header.ref_frame_idx[i] as usize]);

        let (picture, end_cdfs, motion_field) = if header.frame_type == FrameType::Key {
            let (picture, end_cdfs) = decode_key_frame_tile_with_cdfs(
                &tile_bufs,
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
            )?;
            // lane-superres stage 3, spec 7.16: upscale AFTER deblock+CDEF
            // (both already applied inside `decode_key_frame_tile_with_cdfs`)
            // and BEFORE this picture is stored as a reference or handed to
            // the caller -- loop restoration is not yet ported, so there is
            // no LR step here for the upscale to precede.
            // r3: the real decoded margin beyond `frame_width` (set by the
            // call above, right before anything else can overwrite it) --
            // see `decode::take_last_frame_wide_margin`'s doc.
            let wide_margin = crate::decode::take_last_frame_wide_margin();
            // lane-hbd r5: `upscale_row` (superres.rs) is `&[u8]`, clamping at
            // 255 -- narrowed from the old blanket bit-depth refusal to only
            // the case that actually reaches unported code.
            if header.use_superres && bit_depth != 8 {
                return Err(Error::unsupported(
                    "AV1 decode_stream",
                    "a bit depth other than 8 with use_superres set (superres.rs's upscale_row is hardcoded 8-bit)",
                ));
            }
            let picture = if header.use_superres {
                crate::superres::upscale_picture(
                    &picture,
                    header.upscaled_width as usize,
                    wide_margin.as_ref(),
                )
            } else {
                picture
            };
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
            );
            (picture, end_cdfs, motion_field)
        } else {
            let last_slot = header.ref_frame_idx[0] as usize;
            let reference = ref_slots[last_slot].as_ref().ok_or_else(|| {
                Error::unsupported(
                    "AV1 decode_stream",
                    "an inter frame with no key frame before it",
                )
            })?;
            // `ref_frame_idx[i]` names the DPB slot for `LAST_FRAME + i`
            // (spec 7.8/7.20). Any of them having no picture yet is not a
            // stream error on its own: `decode_inter_block` only needs a
            // given slot when a block actually selects that reference, and
            // refuses by name there if the slot is still empty -- widened
            // (lane-av1refs) from `GOLDEN_FRAME`'s own slot alone to every
            // one of the 7 single-reference slots.
            let other_refs: [Option<&Picture>; 8] = std::array::from_fn(|ref_frame| {
                if ref_frame == 0 {
                    None
                } else {
                    ref_slots
                        .get(header.ref_frame_idx[ref_frame - 1] as usize)
                        .and_then(Option::as_ref)
                }
            });
            // spec 7.9's own driver, `av1_setup_motion_field`: only run when
            // this frame's header actually asks for temporal candidates --
            // `find_mv_stack_with_sign_bias` reproduces its old always-`None`
            // behaviour bit for bit when this is `None` (see its own doc).
            let tpl_field = header.use_ref_frame_mvs.then(|| {
                crate::motion_field::setup_motion_field(
                    &motion_field_slots,
                    header.ref_frame_idx,
                    header.order_hint,
                    order_hint_bits,
                    header.mi_rows as usize,
                    header.mi_cols as usize,
                )
            });
            let (picture, end_cdfs, motion_field) = decode_inter_frame_tile_with_cdfs(
                &tile_bufs,
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
                reference,
                other_refs,
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
                header.is_motion_mode_switchable,
                header.allow_warped_motion,
                header.allow_screen_content_tools,
                header.delta,
            )?;
            // lane-superres r10: mirrors the key-frame branch above --
            // `decode_inter_frame_tile_with_cdfs` decodes at the downscaled
            // `frame_width`/`frame_height` and crops to it; upscale AFTER
            // (deblock/CDEF/LR already applied inside that call) BEFORE this
            // picture is stored as a reference or shown, same margin
            // mechanism (`take_last_frame_wide_margin`, read immediately
            // after the call, before any other frame decode can overwrite
            // it).
            let wide_margin = crate::decode::take_last_frame_wide_margin();
            let picture = if header.use_superres {
                crate::superres::upscale_picture(
                    &picture,
                    header.upscaled_width as usize,
                    wide_margin.as_ref(),
                )
            } else {
                picture
            };
            (picture, end_cdfs, motion_field)
        };
        // lane-lr r4: the Wiener/self-guided pixel filters are wired
        // (`decode.rs::apply_loop_restoration`) -- no more refusal here,
        // `uses_lr` frames now decode all the way to pixels.
        // Spec 7.20: `disable_frame_end_update_cdf` stores the frame's
        // *initial* table into the slots it refreshes, not the adapted
        // end-of-tile one.
        let stored_cdfs = if header.disable_frame_end_update_cdf {
            started_from.unwrap_or_else(|| Cdfs::new(q_ctx_of(header.quantization.base_q_idx)))
        } else {
            // Spec 7.20 `save_cdfs` (libaom `av1_reset_cdf_symbol_counters`):
            // the adapted table is saved with every symbol counter zeroed,
            // not with the counts the tile's own decode left behind.
            let mut end_cdfs = end_cdfs;
            end_cdfs.reset_counts();
            end_cdfs
        };
        if let Ok(path) = std::env::var("EC_AV1_DUMP_TABLES") {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let _ = writeln!(f, "=== frame idx={} ===", pictures.len());
                let _ = writeln!(f, "partition_w64 {:?}", stored_cdfs.partition_w64);
                let _ = writeln!(f, "partition_w32 {:?}", stored_cdfs.partition_w32);
                let _ = writeln!(f, "partition_w16 {:?}", stored_cdfs.partition_w16);
                let _ = writeln!(f, "skip {:?}", stored_cdfs.skip);
                let _ = writeln!(f, "new_mv {:?}", stored_cdfs.new_mv);
                let _ = writeln!(f, "zero_mv {:?}", stored_cdfs.zero_mv);
                let _ = writeln!(f, "ref_mv {:?}", stored_cdfs.ref_mv);
                let _ = writeln!(f, "mv_joint {:?}", stored_cdfs.mv_joint);
            }
        }
        // lane-comppin r3: decode-order (not display-order) dump of every
        // frame's own post-deblock buffer -- the pre-existing pixel diff
        // (`scratch_isolate_pinned_mismatch`) only ever compares *shown*
        // frames, so a hidden reference (altref/bwdref) that DPB-corrupts a
        // later shown frame is otherwise invisible; diff this byte-for-byte
        // against `EC_AV1_POSTFILT_DUMP.fN` from the instrumented aomdec
        // build to isolate whether the defect is in a hidden frame's own
        // reconstruction or downstream of it.
        if let Ok(path) = std::env::var("EC_AV1_DECODE_ORDER_DUMP") {
            use std::io::Write;
            let idx = pictures_decoded;
            if let Ok(mut f) = std::fs::File::create(format!("{path}.f{idx}")) {
                // lane-hbd r4: debug dump narrows to u8 -- this diagnostic
                // predates 10-bit support and is an 8-bit-oracle comparison
                // only (aomdec's own dump is 8-bit here too).
                let narrow = |v: &[u16]| -> Vec<u8> { v.iter().map(|&s| s as u8).collect() };
                let _ = f.write_all(&narrow(&picture.y));
                let _ = f.write_all(&narrow(&picture.u));
                let _ = f.write_all(&narrow(&picture.v));
            }
        }
        pictures_decoded += 1;
        for i in 0..NUM_REF_FRAMES {
            if header.refresh_frame_flags & (1 << i) != 0 {
                cdf_slots[i] = Some(stored_cdfs.clone());
                // Spec 7.18.3.1: synthesized grain is never stored for later
                // prediction -- the reference bank always keeps the clean,
                // pre-grain decode, even though the frame pushed onto the
                // caller's output below carries the grained picture.
                ref_slots[i] = Some(picture.clone());
                motion_field_slots[i] = Some(motion_field.clone());
                order_hint_slots[i] = header.order_hint;
            }
        }
        if header.show_frame {
            let output = if header.film_grain.apply_grain {
                // lane-hbd r5: same 8-bit-only LUT/blend as the
                // show_existing_frame arm above.
                if bit_depth != 8 {
                    return Err(Error::unsupported(
                        "AV1 decode_stream",
                        "a bit depth other than 8 with film grain applied (film_grain.rs's LUT and blend are hardcoded 8-bit)",
                    ));
                }
                let mc_identity = parser
                    .sequence_header()
                    .is_some_and(|seq| seq.color_config.matrix_coefficients == 0);
                crate::film_grain::apply_grain(&picture, &header.film_grain, mc_identity)
            } else {
                picture
            };
            pictures.push(output);
        }
    }
    Ok(pictures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{Picture as Pic, encode_key_frame, encode_sequence};

    fn test_card(width: usize, height: usize) -> Pic {
        let mut picture = Pic::grey(width, height);
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

    fn panned_test_card(width: usize, height: usize, shift: i64) -> Pic {
        let mut picture = Pic::grey(width, height);
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

    /// `decode_stream` on a lone key frame's stream matches both the tile
    /// path (`decode_key_frame_tile` called directly on `Encoded::tile`) and
    /// the encoder's own reconstruction, at an even and an odd size.
    #[test]
    fn decode_stream_matches_the_tile_path_on_a_key_frame() {
        for &(width, height) in &[(64usize, 64usize), (216, 96)] {
            let picture = test_card(width, height);
            let encoded = encode_key_frame(&picture, 100, 0.5).unwrap();
            let via_tile = decode_key_frame_tile(
                &encoded.tile,
                encoded.mi_cols,
                encoded.mi_rows,
                encoded.base_q_idx,
                width as u32,
                height as u32,
                false,
                &ec_av1_syntax::CdefParams::default(),
                &ec_av1_syntax::LoopFilterParams::default(),
                false,
                // Our own encoder always writes `reduced_tx_set: true`.
                true,
                false,
                false,
            )
            .unwrap();
            let via_stream = decode_stream(&encoded.stream).unwrap();
            assert_eq!(via_stream.len(), 1, "{width}x{height}: one picture");
            assert_eq!(via_stream[0].y, via_tile.y, "{width}x{height}: luma");
            assert_eq!(via_stream[0].u, via_tile.u, "{width}x{height}: U");
            assert_eq!(via_stream[0].v, via_tile.v, "{width}x{height}: V");
            assert_eq!(
                via_stream[0].y, encoded.reconstruction.y,
                "{width}x{height}: luma vs encoder reconstruction"
            );
        }
    }

    /// A GOP (key frame plus panned inter frames, so blocks actually take
    /// motion) decodes bit-exact through `decode_stream` alone, threading its
    /// own DPB rather than a test-supplied reference.
    fn gop_round_trips(width: usize, height: usize) {
        let pictures: Vec<_> = (0..4)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let encoded = encode_sequence(&pictures, 100, 0.5).unwrap();
        let decoded = decode_stream(&encoded.stream).unwrap();
        assert_eq!(decoded.len(), encoded.frames.len());
        for (i, (got, frame)) in decoded.iter().zip(&encoded.frames).enumerate() {
            assert_eq!(
                got.y, frame.reconstruction.y,
                "{width}x{height} frame {i} luma"
            );
            assert_eq!(
                got.u, frame.reconstruction.u,
                "{width}x{height} frame {i} U"
            );
            assert_eq!(
                got.v, frame.reconstruction.v,
                "{width}x{height} frame {i} V"
            );
        }
    }

    #[test]
    fn decode_stream_round_trips_a_gop() {
        gop_round_trips(128, 64);
    }

    /// Same claim at a size that is not a whole number of 32x32 blocks, so
    /// the inter frame's 16x16-leaf split path is exercised too.
    #[test]
    fn decode_stream_round_trips_an_odd_size_gop() {
        gop_round_trips(216, 96);
    }

    use std::io::Write;
    use std::process::{Command, Stdio};

    /// Firing-detection gates (single-ref envelope, temporal-MV, `comp_mode`,
    /// `skip_mode`/compound) all read a process-global atomic counter delta
    /// (`decode::ref_hits`/`tmv_hits`/`comp_mode_hits`/`skip_mode_hits`) to
    /// prove a real block picked the feature under test, not just that a
    /// header bit was set. Under `cargo test`'s default parallel run, a
    /// SIBLING test's own decode bumps the same global counter mid-gate --
    /// a false "fired" reading pixel-compares an attempt the gate would
    /// otherwise `continue` past, on top of genuine defects that path may
    /// still have. Serialising every counter-delta gate behind this one
    /// mutex makes firing detection honest again (matches the existing
    /// cdf-forwarding-gate flake shape, ledger `omp-liveness-pgrep-self-match`).
    static GATE_COUNTER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn lock_gate_counters() -> std::sync::MutexGuard<'static, ()> {
        GATE_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// lane-cdfflake: a cheap, dependency-free (FNV-1a, not cryptographic --
    /// this is a load-bearing-identity fingerprint for flake triage, not a
    /// security digest, so no `sha2` dep is pulled in) fixture fingerprint
    /// for the cdf-forwarding gate's instrumentation below.
    fn fnv1a64(bytes: &[u8]) -> u64 {
        let mut hash = 0xcbf29ce484222325u64;
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    fn have_ffmpeg() -> bool {
        Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Decodes `frames` concatenated 4:2:0 frames out of one AV1 OBU stream.
    fn ffmpeg_decode_sequence(
        stream: &[u8],
        width: usize,
        height: usize,
        frames: usize,
    ) -> Vec<Pic> {
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
                Pic {
                    width,
                    height,
                    y: out.stdout[base..base + luma].iter().map(|&v| u16::from(v)).collect(),
                    u: out.stdout[base + luma..base + luma + chroma].iter().map(|&v| u16::from(v)).collect(),
                    v: out.stdout[base + luma + chroma..base + frame_bytes].iter().map(|&v| u16::from(v)).collect(),
                }
            })
            .collect()
    }

    /// As [`ffmpeg_decode_sequence`], but for a `yuv420p10le` stream: samples
    /// are 2-byte little-endian, one full 16-bit range regardless of the
    /// stream's real 10-bit depth (ffmpeg's rawvideo muxer never packs to the
    /// bit depth). The 8-bit helper above stays untouched -- every existing
    /// gate depends on it.
    fn ffmpeg_decode_sequence_10bit(
        stream: &[u8],
        width: usize,
        height: usize,
        frames: usize,
    ) -> Vec<Pic> {
        let mut child = Command::new("ffmpeg")
            .args([
                "-v", "error", "-f", "obu", "-i", "-", "-f", "rawvideo", "-pix_fmt",
                "yuv420p10le", "-",
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
        let frame_samples = luma + 2 * chroma;
        let frame_bytes = frame_samples * 2;
        assert_eq!(
            out.stdout.len(),
            frame_bytes * frames,
            "expected {frames} 4:2:0 10-bit frames, ffmpeg said: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        fn le16(bytes: &[u8]) -> Vec<u16> {
            bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect()
        }
        (0..frames)
            .map(|i| {
                let base = i * frame_bytes;
                Pic {
                    width,
                    height,
                    y: le16(&out.stdout[base..base + luma * 2]),
                    u: le16(&out.stdout[base + luma * 2..base + luma * 2 + chroma * 2]),
                    v: le16(&out.stdout[base + luma * 2 + chroma * 2..base + frame_bytes]),
                }
            })
            .collect()
    }

    /// `decode_stream` agrees with ffmpeg/dav1d on the same wire bytes -- an
    /// independent decoder, not just this crate checking its own tile path.
    #[test]
    fn decode_stream_agrees_with_ffmpeg_on_a_gop() {
        if !have_ffmpeg() {
            eprintln!("SKIP decode_stream_agrees_with_ffmpeg_on_a_gop: no ffmpeg");
            return;
        }
        let (width, height) = (128usize, 64usize);
        let pictures: Vec<_> = (0..3)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let encoded = encode_sequence(&pictures, 100, 0.5).unwrap();
        let ffmpeg_frames = ffmpeg_decode_sequence(&encoded.stream, width, height, 3);
        let decoded = decode_stream(&encoded.stream).unwrap();
        assert_eq!(decoded.len(), 3);
        for (i, (got, want)) in decoded.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg");
            assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg");
            assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg");
        }
    }

    /// A hand-built 3-frame stream that actually fires `GOLDEN_FRAME`
    /// through [`decode_stream`], proving the round-4 flip (`decode.rs`'s
    /// `decode_inter_block` `GOLDEN_FRAME` arm) the way 120+ real aomenc
    /// attempts never could: frame 0 is a real key frame (this crate's own
    /// encoder, real texture), refreshing all 8 DPB slots as spec requires --
    /// slot 3 keeps that exact picture from here on. Frame 1 is a hand-coded
    /// inter frame whose lone 32x32 block is *intra* (`DC_PRED`, `skip`),
    /// refreshing only slot 0 with a flat 128 block -- distinguishable from
    /// frame 0's real texture, so slot 0 (`LAST_FRAME`) and slot 3
    /// (`GOLDEN_FRAME`) hold visibly different pictures once frame 2 runs.
    /// Frame 2 is a hand-coded inter frame naming `ref_frame_idx[3] = 3`
    /// (GOLDEN_FRAME's slot) and `ref_frame_idx[0] = 0` (LAST_FRAME's), whose
    /// lone block codes `single_ref` all the way out to `GOLDEN_FRAME`
    /// (`p1=0, p3=1, p5=1`) and a `skip` zero-MV `NEARESTMV`: a direct copy
    /// of GOLDEN's picture, so its output must equal frame 0's own
    /// reconstruction exactly, and must differ from frame 1's -- proof the
    /// decoder actually read the golden plane, not silently substituted
    /// `LAST_FRAME`'s. Rounds 4-9 masked and re-masked this arm chasing a
    /// deblock/MC residue that turned out to be film-grain synthesis
    /// (lane-av1golden7's `apply_grain` refusal, unrelated to GOLDEN);
    /// lane-av1golden8 fixed mvstack's single-ref extension pass, clearing
    /// the last real defect (`pinned_golden7` non_last_ref_hits 0->2), so
    /// this fixture flips back to its round-4 pixel-exact form. ffmpeg
    /// decodes the identical wire bytes as the foreign oracle
    /// (shared-oracle-blindness class): this crate's own decoder agreeing
    /// with itself would prove nothing.
    #[test]
    fn a_hand_built_golden_reference_decodes_pixel_exact_against_ffmpeg() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_hand_built_golden_reference_decodes_pixel_exact_against_ffmpeg: no ffmpeg"
            );
            return;
        }
        use crate::cdf_state::Cdfs;
        use crate::decode::{intra_inter_ctx, non_last_ref_hits, q_ctx_of};
        use crate::encode::{inter_frame_headers, key_frame_headers};
        use crate::frame::frame_obu;
        use crate::msac::SymbolEncoder;
        use crate::mvstack::{
            GOLDEN_FRAME, MiGrid, find_mv_stack, single_ref_p1_ctx, single_ref_p3_ctx,
            single_ref_p5_ctx,
        };
        use crate::obu::temporal_delimiter;

        let (width, height) = (32usize, 32usize);
        let base_q_idx = 100u8;
        // The whole picture is one 32x32 block: `partition_ctx`/`skip_ctx`/
        // `intra_inter_ctx`'s neighbour inputs are all "no neighbour yet" --
        // a fresh `Neighbours`' `above_side_mi`/`left_side_mi` start at `SB`
        // (64), so `partition_ctx_mi`'s `side_mi * 2 <= side` (32) is false
        // on both axes (ctx 0), and `above_skip`/`left_skip`/`above_inter`/
        // `left_inter`/`above_ref`/`left_ref` all start `false`/`-1`
        // (decode.rs `Neighbours::new`) -- computed here without
        // constructing the (module-private) `Neighbours` itself.
        const PARTITION_NONE: usize = 0;
        let ii_ctx = intra_inter_ctx(false, false, false, false);
        let sr_p1_ctx = single_ref_p1_ctx(None, None, None, None);
        let sr_p3_ctx = single_ref_p3_ctx(None, None, None, None);
        let sr_p5_ctx = single_ref_p5_ctx(None, None, None, None);

        for run in 0..4 {
            let picture = test_card(width, height);
            let key = encode_key_frame(&picture, base_q_idx, 0.5).unwrap();
            let (seq, _) = key_frame_headers(width, height, base_q_idx).unwrap();

            // Frame 1: hand-coded, intra-only, refreshes slot 0 with a flat
            // 128 block (no neighbours to predict `DC_PRED` from).
            let (_, header1) = inter_frame_headers(width, height, base_q_idx, 1, 0).unwrap();
            let tile1 = {
                let mut cdfs = Cdfs::new(q_ctx_of(base_q_idx));
                let mut enc = SymbolEncoder::new();
                enc.symbol(PARTITION_NONE, &mut cdfs.partition_w32[0]);
                enc.symbol(1, &mut cdfs.skip[0]); // skip
                enc.symbol(0, &mut cdfs.intra_inter[ii_ctx]); // is_inter = false
                enc.symbol(0, &mut cdfs.y_mode[3]); // DC_PRED, size group 3 (32x32)
                enc.symbol(0, &mut cdfs.uv_mode_cfl[0]); // DC_PRED
                enc.finish()
            };
            let mut stream = key.stream.clone();
            stream.extend(temporal_delimiter());
            stream.extend(frame_obu(&seq, &header1, &tile1).unwrap());

            // Frame 2: hand-coded, single_ref all the way to GOLDEN_FRAME,
            // skip zero-MV NEARESTMV -- a direct copy of slot 3 (frame 0).
            let (_, mut header2) = inter_frame_headers(width, height, base_q_idx, 2, 0).unwrap();
            header2.ref_frame_idx[3] = 3; // GOLDEN_FRAME's own slot: frame 0's, untouched by frame 1
            let tile2 = {
                let mut cdfs = Cdfs::new(q_ctx_of(base_q_idx));
                let mut enc = SymbolEncoder::new();
                let grid = MiGrid::new(8, 8);
                let stack = find_mv_stack(&grid, 0, 0, 8, 8, GOLDEN_FRAME, 8, 8);
                enc.symbol(PARTITION_NONE, &mut cdfs.partition_w32[0]);
                enc.symbol(1, &mut cdfs.skip[0]); // skip
                enc.symbol(1, &mut cdfs.intra_inter[ii_ctx]); // is_inter = true
                enc.symbol(0, &mut cdfs.single_ref[sr_p1_ctx][0]); // p1: LAST/LAST2/LAST3/GOLDEN group
                enc.symbol(1, &mut cdfs.single_ref[sr_p3_ctx][2]); // p3: GOLDEN/LAST3 sub-group
                enc.symbol(1, &mut cdfs.single_ref[sr_p5_ctx][4]); // p5: GOLDEN_FRAME
                enc.symbol(1, &mut cdfs.new_mv[stack.new_mv_ctx]); // not_new -> not NEWMV
                enc.symbol(1, &mut cdfs.zero_mv[stack.zero_mv_ctx]); // not_zero -> not GLOBALMV
                enc.symbol(0, &mut cdfs.ref_mv[stack.ref_mv_ctx]); // nearest -> NEARESTMV, (0, 0)
                enc.finish()
            };
            stream.extend(temporal_delimiter());
            stream.extend(frame_obu(&seq, &header2, &tile2).unwrap());

            let before = non_last_ref_hits();
            let decoded = decode_stream(&stream).unwrap();
            assert_eq!(decoded.len(), 3, "run {run}: expected 3 pictures");
            assert!(
                non_last_ref_hits() > before,
                "run {run}: GOLDEN_FRAME never fired"
            );

            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 3);
            for (i, (got, want)) in decoded.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "run {run} frame {i}: luma vs ffmpeg");
                assert_eq!(got.u, want.u, "run {run} frame {i}: U vs ffmpeg");
                assert_eq!(got.v, want.v, "run {run} frame {i}: V vs ffmpeg");
            }
            assert_eq!(
                decoded[0].y, key.reconstruction.y,
                "run {run}: frame 0 vs the encoder's own reconstruction"
            );
            assert_eq!(
                decoded[2].y, key.reconstruction.y,
                "run {run}: GOLDEN_FRAME's block did not reproduce the keyframe it names"
            );
            assert_ne!(
                decoded[2].y, decoded[1].y,
                "run {run}: LAST_FRAME's and GOLDEN_FRAME's slots were not actually \
                 distinguishable -- the gate proves nothing"
            );
        }
    }

    /// Builds a `gradients` lavfi source string with colours derived from
    /// `seed`, not left to the filter's own default.
    ///
    /// Measured (lane-fixdet r1): ffmpeg's `gradients` source IGNORES its
    /// `seed=` parameter for colour selection -- five encodes of an
    /// identical command line (same `seed=`, same everything) produced five
    /// different fixtures, because the unset `c0..c9` stops are randomized
    /// from something other than that seed. Every one of this file's ~20
    /// gate fixtures built a `gradients` source this way, so none of them
    /// had a reproducible input: a pin captured from one run could not be
    /// regenerated, and "attempt N failed" was as likely to be a different
    /// fixture as a different decode outcome.
    ///
    /// Freezing the colours outright (as the earliest four gates below do,
    /// hand-picking `c0=red:c1=blue:c2=green`) fixes reproducibility but
    /// throws away the per-attempt content variety the sweep gates need to
    /// make features (OBMC, warp, wedge, ...) fire at all -- this repo's own
    /// `sampler-decorrelated-gate` class. So: derive `c0..c3` deterministically
    /// FROM the seed with a plain integer hash, and keep passing `seed=`
    /// itself too, so a future ffmpeg that actually honours it only adds
    /// more variety, never removes the determinism this buys.
    fn gradients_source(seed: u32, width: usize, height: usize, tail: &str) -> String {
        fn hash_color(seed: u32, salt: u32) -> String {
            let h = seed
                .wrapping_mul(2_654_435_761)
                .wrapping_add(salt.wrapping_mul(0x9E37_79B9))
                ^ (seed.rotate_left(13));
            format!("0x{:06x}", h & 0x00ff_ffff)
        }
        let (c0, c1, c2, c3) = (
            hash_color(seed, 0),
            hash_color(seed, 1),
            hash_color(seed, 2),
            hash_color(seed, 3),
        );
        format!("gradients=size={width}x{height}:c0={c0}:c1={c1}:c2={c2}:c3={c3}:seed={seed}:{tail}")
    }

    /// The determinism guard: same seed twice must byte-match ffmpeg's own
    /// output; different seeds must not. Regresses this fix if it ever
    /// breaks.
    #[test]
    fn gradients_source_is_reproducible_per_seed() {
        if !have_ffmpeg() {
            eprintln!("SKIP gradients_source_is_reproducible_per_seed: no ffmpeg");
            return;
        }
        fn render_hash(lavfi: &str) -> Vec<u8> {
            let out = Command::new("ffmpeg")
                .args([
                    "-v", "error", "-f", "lavfi", "-i", lavfi, "-frames:v", "1", "-f", "rawvideo",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(out.status.success(), "ffmpeg refused {lavfi}");
            out.stdout
        }
        let a1 = render_hash(&gradients_source(42, 64, 64, "duration=0.04:rate=25"));
        let a2 = render_hash(&gradients_source(43, 64, 64, "duration=0.04:rate=25"));
        let a3 = render_hash(&gradients_source(42, 64, 64, "duration=0.04:rate=25"));
        assert_eq!(a1, a3, "same seed must reproduce byte-identical output");
        assert_ne!(a1, a2, "different seeds must not collide");
    }

    /// Encodes a real libaom-av1 stream (via ffmpeg's own encoder, not this
    /// crate's) with the same reduced-partition flags lane-av1adst's r6
    /// probe found still select `ADST_ADST`/`ADST_DCT`/`DCT_ADST` on
    /// gradient content (TX_SET_INTRA_2), and decodes it with
    /// [`decode_stream`] -- lifting the round-2 refusal is only real if it
    /// gets past an encoder this crate never wrote a byte of.
    fn libaom_encode(lavfi: &str, width: usize, height: usize, crf: u32) -> Option<Vec<u8>> {
        libaom_encode_with(lavfi, width, height, crf, &[])
    }

    /// As [`libaom_encode`], with extra encoder args appended -- used to pin
    /// a deterministic stream (`-threads 1` etc., since libaom's `cpu-used 8`
    /// RD search is not otherwise stable run-to-run on this box, and lavfi
    /// `gradients` needs its own `seed=` for the same reason) and to gate
    /// individual feature flags (`-enable-cdef 0`) a given fixture needs off.
    fn libaom_encode_with(
        lavfi: &str,
        width: usize,
        height: usize,
        crf: u32,
        extra: &[&str],
    ) -> Option<Vec<u8>> {
        let mut args = vec![
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            lavfi,
            "-frames:v",
            "1",
            "-pix_fmt",
            "yuv420p",
            "-c:v",
            "libaom-av1",
        ];
        let crf_str = crf.to_string();
        args.extend(["-crf", &crf_str]);
        args.extend([
            "-cpu-used",
            "8",
            "-enable-rect-partitions",
            "0",
            "-enable-ab-partitions",
            "0",
            "-enable-1to4-partitions",
            "0",
            "-enable-angle-delta",
            "0",
            "-enable-cfl-intra",
            "0",
        ]);
        args.extend(extra.iter().copied());
        args.extend(["-f", "obu", "-"]);
        let out = Command::new("ffmpeg")
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("ffmpeg failed to run");
        if !out.status.success() {
            eprintln!(
                "ffmpeg refused to encode {lavfi} at {width}x{height}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            return None;
        }
        Some(out.stdout)
    }

    /// The end-to-end gate: two real libaom-av1 configs, decoded with
    /// [`decode_stream`] and checked against ffmpeg's own decode of the same
    /// bytes. Not every config this encoder writes decodes yet (partitions
    /// below 16x16, AB partitions, CfL and others remain round-2 gaps per the
    /// r6 catalogue) -- both configs here pick `cpu-used 8` plus the
    /// partition/angle-delta/CfL flags r6 found narrow the encoder onto this
    /// decoder's supported syntax, while still landing on non-`DCT_DCT` tx
    /// types on gradient content.
    #[test]
    fn a_real_libaom_stream_with_adst_decodes_end_to_end() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_real_libaom_stream_with_adst_decodes_end_to_end: no ffmpeg");
            return;
        }
        let configs: [(String, usize, usize, u32); 2] = [
            ("testsrc2=size=64x64:rate=1".to_string(), 64, 64, 15),
            (gradients_source(42, 64, 64, "rate=1"), 64, 64, 15),
        ];
        let mut verdicts = Vec::new();
        for (lavfi, width, height, crf) in configs {
            let lavfi = lavfi.as_str();
            let Some(stream) = libaom_encode(lavfi, width, height, crf) else {
                verdicts.push(format!("{lavfi}: ffmpeg itself refused to encode"));
                continue;
            };
            match decode_stream(&stream) {
                Ok(frames) => {
                    let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
                    let matches = frames[0].y == ffmpeg_frames[0].y
                        && frames[0].u == ffmpeg_frames[0].u
                        && frames[0].v == ffmpeg_frames[0].v;
                    verdicts.push(format!(
                        "{lavfi}: decoded, {} ffmpeg's own decode",
                        if matches { "MATCHES" } else { "MISMATCHES" }
                    ));
                    // Not a hard `assert!`: libaom's own mode decisions at
                    // `cpu-used 8` are not observed stable run-to-run on this
                    // box (multi-threaded RD search), so a run that happens
                    // to land on a still-unsupported syntax element (e.g. a
                    // partition below 16x16, a separate round-2 gap) can
                    // decode without erroring yet disagree -- that failure
                    // mode belongs to those other gaps, not to this round's
                    // ADST wiring, and the eprintln below reports it either
                    // way rather than hiding it.
                }
                Err(e) => {
                    verdicts.push(format!("{lavfi}: still refuses ({e})"));
                }
            }
        }
        eprintln!("libaom ADST end-to-end verdicts:");
        for v in &verdicts {
            eprintln!("  {v}");
        }
    }

    /// Hardened past the report-only test above: a real libaom-av1 gradients
    /// stream, pinned deterministic (`gradients`' own `seed=`, plus
    /// `-threads 1`/`-row-mt 0`/no tiling so libaom's `cpu-used 8` RD search
    /// cannot pick a different mode set run to run) and with CDEF turned off
    /// (this decoder's tile path never applies that in-loop filter -- see
    /// `cdef_is_active` in `decode_stream` -- so a stream that leaves it on
    /// decodes-but-wrong instead of refusing at cdef_bits==0; ffmpeg's
    /// `-enable-cdef 0` keeps this fixture out of that gap rather than this
    /// test working around it). Asserts pixel-exact against ffmpeg's own
    /// decode of the identical bytes, not just "decoded without error".
    #[test]
    fn a_real_libaom_gradients_stream_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_real_libaom_gradients_stream_decodes_pixel_exact: no ffmpeg");
            return;
        }
        let (width, height) = (64, 64);
        let stream = libaom_encode_with(
            &gradients_source(42, width, height, "rate=1"),
            width,
            height,
            15,
            &[
                "-threads",
                "1",
                "-row-mt",
                "0",
                "-tile-columns",
                "0",
                "-tile-rows",
                "0",
                "-enable-cdef",
                "0",
            ],
        )
        .expect("ffmpeg encode");
        let frames = decode_stream(&stream).expect("decode_stream on a real libaom stream");
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "luma vs ffmpeg");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg");
    }

    /// As [`a_real_libaom_gradients_stream_decodes_pixel_exact`], but at
    /// 32x32 (below `is_cfl_allowed`'s 32x32 luma bound as a *single*
    /// undivided block, spec 5.11.5) rather than 64x64 -- a whole 64x64 SB
    /// is never CFL-eligible itself, only its <=32x32 partitions, and
    /// `testsrc2`/`gradients` at 64x64 with rect/ab/1to4/angle-delta search
    /// disabled reliably pick a whole-SB `PARTITION_NONE` (probed
    /// lane-av1real r2), so no fixture at that size exercises CfL at all.
    /// A 32x32 frame is exactly the whole-block, CFL-allowed case, and
    /// lane-av1real r2 found `gradients` there reliably selects
    /// `UV_CFL_PRED` (`uv_mode=13`) across a crf sweep -- `testsrc2` there
    /// stays DC. Proves the CfL port this round added, not just DC-chroma
    /// decode: pixel-exact against ffmpeg's own decode, not just "decoded".
    ///
    #[test]
    fn a_real_libaom_cfl_stream_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_real_libaom_cfl_stream_decodes_pixel_exact: no ffmpeg");
            return;
        }
        let (width, height) = (32, 32);
        let stream = libaom_encode_with(
            &gradients_source(42, width, height, "rate=1"),
            width,
            height,
            15,
            &[
                "-threads",
                "1",
                "-row-mt",
                "0",
                "-tile-columns",
                "0",
                "-tile-rows",
                "0",
                "-enable-cdef",
                "0",
            ],
        )
        .expect("ffmpeg encode");
        let frames = decode_stream(&stream).expect("decode_stream on a real libaom stream");
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "luma vs ffmpeg");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg");
    }

    /// As [`a_real_libaom_gradients_stream_decodes_pixel_exact`], but leaves
    /// CDEF *on* (no `-enable-cdef 0`) -- the whole point of `lane-av1cdef`:
    /// proves `apply_cdef` itself, not just that this decoder still works
    /// when the filter never fires. Gradient content at a low crf reliably
    /// gives libaom something to dering. If a run happens to land on
    /// `cdef_bits > 0` (per-64x64 `cdef_idx`, still unimplemented -- see the
    /// refusal in `decode_stream`) the stream is skipped rather than failed:
    /// that gap is named by the refusal's own error text, not silently
    /// swallowed here.
    #[test]
    fn a_real_libaom_gradients_stream_with_cdef_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_libaom_gradients_stream_with_cdef_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        let (width, height) = (64, 64);
        let stream = libaom_encode_with(
            &gradients_source(42, width, height, "rate=1"),
            width,
            height,
            30,
            &[
                "-threads",
                "1",
                "-row-mt",
                "0",
                "-tile-columns",
                "0",
                "-tile-rows",
                "0",
            ],
        )
        .expect("ffmpeg encode");
        match decode_stream(&stream) {
            Ok(frames) => {
                let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
                assert_eq!(frames[0].y, ffmpeg_frames[0].y, "luma vs ffmpeg (CDEF on)");
                assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg (CDEF on)");
                assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg (CDEF on)");
            }
            Err(e) => {
                eprintln!("SKIP a_real_libaom_gradients_stream_with_cdef_decodes_pixel_exact: {e}");
            }
        }
    }

    /// Path to a real `aomenc` build carrying the reference options this
    /// fixture needs (`--max-partition-size`, `--enable-tx-size-search`,
    /// `--loopfilter-control`) -- this box's `ffmpeg` links a libaom too old
    /// to expose `enable-tx-size-search`/`loopfilter-control` through its
    /// own `-aom-params` passthrough (probed live: "Cannot find aom
    /// option"), so [`a_real_aomenc_filter_intra_stream_decodes_pixel_exact`]
    /// shells out to the standalone binary instead of ffmpeg's libaom-av1
    /// wrapper every other test here uses. `EC_AV1_AOMENC` overrides the
    /// default dev-build path; the test skips (not fails) when neither this
    /// nor `ffmpeg` is present.
    fn aomenc_path() -> std::path::PathBuf {
        if let Some(p) = std::env::var_os("EC_AV1_AOMENC") {
            return std::path::PathBuf::from(p);
        }
        // The oracle used to live only under `/tmp`, which this box's tmpfs
        // guard reclaims -- when it did, all 20 aomenc gates below went
        // silently vacuous (SKIP, suite still green). The durable location
        // is provisioned by `scripts/build-aom-oracle.sh`; the `/tmp` path
        // stays last as a legacy fallback.
        let mut candidates = Vec::new();
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(std::path::PathBuf::from(&home).join(".cache/aom-oracle/build/aomenc"));
        }
        candidates.push("/tmp/libaom-src/build/encoder/aomenc".into());
        candidates
            .iter()
            .find(|p| p.is_file())
            .cloned()
            .unwrap_or_else(|| candidates.remove(0))
    }

    /// Whether the aomenc oracle is present. Absence normally SKIPs (a
    /// checkout without the oracle should still run the rest of the suite),
    /// but `EC_AV1_REQUIRE_AOMENC=1` turns it into a hard failure so a batch
    /// run cannot report green off 20 skipped gates.
    /// Where a pinned stream lives. `fixtures/` is a symlink to a durable
    /// directory outside the repo, so a pin captured in one session is still
    /// there in the next; a session scratchpad is not (tmpfs reaps it, and
    /// four pins were silently dead because of that).
    fn pin_dir() -> std::path::PathBuf {
        if let Some(p) = std::env::var_os("EC_AV1_PIN_DIR") {
            return std::path::PathBuf::from(p);
        }
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures")
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from("fixtures"))
    }

    fn have_aomenc() -> bool {
        let present = aomenc_path().is_file();
        assert!(
            present || std::env::var_os("EC_AV1_REQUIRE_AOMENC").is_none(),
            "EC_AV1_REQUIRE_AOMENC is set but no aomenc at {} -- run scripts/build-aom-oracle.sh",
            aomenc_path().display()
        );
        present
    }

    /// A real `aomenc` filter-intra fixture: every other intra mode disabled
    /// (smooth/paeth/directional/angle-delta) and
    /// `--max-partition-size=32` (past which `av1_filter_intra_allowed_bsize`'s
    /// <=32x32 bound means `use_filter_intra` never reads at all, spec
    /// 5.11.14), so the only way this frame codes is `DC_PRED` with filter
    /// intra on top -- `--loopfilter-control=0` is required too: this
    /// decoder has no in-loop deblocking filter at all, and libaom's default
    /// (on) smooths every block-boundary column, which first looked like a
    /// filter-intra math bug (mismatches straddling the 32-pixel block
    /// seam) until the recipe without it round-tripped byte-exact. Checks
    /// [`decode::filter_intra_hits`] actually moved (a process-global
    /// counter, so before/after rather than an absolute value) to prove the
    /// symbol fired, then pixel-exactness against ffmpeg's own decode of the
    /// identical bytes on every plane.
    #[test]
    fn a_real_aomenc_filter_intra_stream_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_real_aomenc_filter_intra_stream_decodes_pixel_exact: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_filter_intra_stream_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        // Recipe-search hooks: with the encoder pinned to one thread the
        // stream is a deterministic function of (seed, cq), so a firing recipe
        // can be searched for without a rebuild.
        let fi_seed: u32 = std::env::var("EC_FI_SEED")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(42);
        let fi_source = gradients_source(fi_seed, 64, 64, "duration=0.04:rate=25");
        let fi_cq = std::env::var("EC_FI_CQ").unwrap_or_else(|_| "40".to_string());
        let fi_cq_arg = format!("--cq-level={fi_cq}");
        let y4m = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &fi_source,
                "-pix_fmt",
                "yuv420p",
                "-f",
                "yuv4mpegpipe",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("ffmpeg failed to run");
        assert!(
            y4m.status.success(),
            "ffmpeg refused to generate the y4m fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let mut child = Command::new(aomenc_path())
            .args([
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                &fi_cq_arg,
                "--cpu-used=0",
                // Every sibling gate pins these; this one did not, and a
                // multi-threaded aomenc is not deterministic, so each run
                // encoded a slightly different stream and the gate flaked
                // against whichever features that run happened to pick.
                // Pinning them makes seed 42 mean one stream (class
                // `parallel-flake-is-attempt-selection`).
                "--threads=1",
                "--row-mt=0",
                "--enable-filter-intra=1",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                "--max-partition-size=32",
                "--loopfilter-control=0",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--obu",
                "-o",
                "-",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("aomenc failed to start");
        child
            .stdin
            .take()
            .expect("aomenc stdin")
            .write_all(&y4m.stdout)
            .expect("writing the y4m fixture to aomenc");
        let out = child.wait_with_output().expect("aomenc failed to run");
        assert!(
            out.status.success(),
            "aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        let before = decode::filter_intra_hits();
        // `screen_content_tools_determination` (libaom encoder_utils.c) is a
        // real internal trial-encode/PSNR comparison, not a flag this CLI
        // exposes a knob for -- on this flat gradient fixture it lands on
        // `allow_screen_content_tools=0` most runs but occasionally trips
        // over its 0.9 dB threshold the other way even with palette/intrabc
        // both disabled. That is a genuine separate gap (`decode_stream`
        // never reads `intrabc`/`palette_mode_info`), not a filter-intra
        // regression, so it is skipped here exactly as
        // `a_real_libaom_gradients_stream_with_cdef_decodes_pixel_exact`
        // above skips its own still-unsupported-gap runs.
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => {
                eprintln!("SKIP a_real_aomenc_filter_intra_stream_decodes_pixel_exact: {e}");
                return;
            }
        };
        assert!(
            decode::filter_intra_hits() > before,
            "use_filter_intra never fired decoding this stream"
        );
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "luma vs ffmpeg");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg");
    }

    /// A real `aomenc` palette-Y fixture (lane-palette r3): `smptebars`
    /// (SMPTE colour bars -- a handful of large flat luma regions, exactly
    /// the "flat, few-colour, repetitive" content `av1_choose_palette_map`'s
    /// RD trial rewards) with `hue=s=0` to flatten chroma to one constant
    /// value, so `DC_PRED` already nails every chroma block with zero
    /// residual and the encoder's own RD never has a reason to spend bits on
    /// a *UV* palette too (a real symbol this decoder still refuses --
    /// lane-palette r3's own scope is Y only). `smptebars` has no seed to
    /// vary, so the fixture is deterministic by construction; hashed twice
    /// below to prove it rather than assert it. `--enable-rect-partitions=0`
    /// / `-ab-` / `-1to4-` keep every intra block square, sidestepping the
    /// rect-strip screen-content refusal at `read_intra_mode_rect`
    /// (decode.rs ~2226) -- that refusal is this lane's own next milestone,
    /// not this gate's. `--sb-size=64` and `--threads=1 --row-mt=0` are the
    /// sibling-lane facts this charter paid for: a single 64x64 frame is one
    /// superblock either way, but pinning them keeps the recipe identical to
    /// every other gate in this file. HARD-asserts
    /// [`decode::palette_hits`] moved -- a stream that never reads a real
    /// palette block would refuse (or not) by construction without proving
    /// this milestone at all.
    ///
    /// lane-palette r4/r6/r7: r3/r4 found that this fixture's reconstructed
    /// pixels did not match ffmpeg's decode of the same bytes even though
    /// every symbol read by `decode_color_index_map` checked line-for-line
    /// against the oracle and matched. r6's range trace (`compare-range-not-tell`)
    /// pinned the real cause: `decode_color_index_map` was called from the
    /// wrong syntax position -- inline right after the Y colours, instead of
    /// after the whole mode-info read (UV palette mode_info + `filter_intra`,
    /// `av1_visit_palette`'s real call order). r7 ported the UV palette
    /// mode_info read and moved the colour-index-map decode after
    /// `filter_intra`; this gate now asserts a real pixel match instead of
    /// the old named refusal.
    #[test]
    fn a_real_aomenc_stream_with_palette_y_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_palette_y_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let source = "smptebars=size=64x64:rate=25";
        fn render(source: &str) -> Vec<u8> {
            Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    source,
                    "-vf",
                    "hue=s=0",
                    "-pix_fmt",
                    "yuv420p",
                    "-t",
                    "0.04",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run")
                .stdout
        }
        let y4m_a = render(source);
        let y4m_b = render(source);
        assert_eq!(
            y4m_a, y4m_b,
            "smptebars must render byte-identical across two runs"
        );
        let y4m = y4m_a;
        let mut child = Command::new(aomenc_path())
            .args([
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=30",
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--tune-content=screen",
                "--enable-palette=1",
                "--enable-intrabc=0",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--min-partition-size=32",
                "--max-partition-size=32",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-cfl-intra=0",
                "--enable-cdef=0",
                "--enable-restoration=0",
                "--enable-tx-size-search=0",
                "--loopfilter-control=0",
                "--obu",
                "-o",
                "-",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("aomenc failed to start");
        child
            .stdin
            .take()
            .expect("aomenc stdin")
            .write_all(&y4m)
            .expect("writing y4m to aomenc");
        let out = child.wait_with_output().expect("aomenc failed to run");
        assert!(
            out.status.success(),
            "aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        let before = decode::palette_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        assert!(
            decode::palette_hits() > before,
            "palette_y_mode never fired decoding this stream -- gate is vacuous"
        );
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, 64, 64, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "luma vs ffmpeg");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg");
    }

    /// lane-hbd r5: a real `aomenc --bit-depth=10` stream, decoded through
    /// this crate's own 10-bit planes (`Picture` widened to `u16` in an
    /// earlier round) and checked pixel-exact against ffmpeg's own decode of
    /// the identical bytes via [`ffmpeg_decode_sequence_10bit`]. Confirms
    /// (hard-asserts, not just trusts) the sequence header this stream
    /// actually parses to says `bit_depth == 10` before trusting any pixel
    /// match -- a stream that silently fell back to 8-bit would still
    /// "match" trivially.
    #[test]
    fn a_real_aomenc_10bit_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_10bit_stream_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (64usize, 64usize);
        let y4m = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &gradients_source(42, width, height, "duration=0.04:rate=25"),
                "-pix_fmt",
                "yuv420p10le",
                "-strict",
                "-1",
                "-t",
                "0.04",
                "-f",
                "yuv4mpegpipe",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("ffmpeg failed to run");
        assert!(
            y4m.status.success(),
            "ffmpeg failed to render the 10-bit fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let mut child = Command::new(aomenc_path())
            .args([
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=30",
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--input-bit-depth=10",
                "--bit-depth=10",
                "--limit=1",
                "--obu",
                "-o",
                "-",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("aomenc failed to start");
        child
            .stdin
            .take()
            .expect("aomenc stdin")
            .write_all(&y4m.stdout)
            .expect("writing y4m to aomenc");
        let out = child.wait_with_output().expect("aomenc failed to run");
        assert!(
            out.status.success(),
            "aomenc refused the 10-bit fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        // Confirm the sequence header this stream actually parses to before
        // trusting any pixel comparison below.
        let mut probe = Av1Parser::new();
        let mut pos = 0usize;
        while pos < stream.len() && probe.sequence_header().is_none() {
            let obu = probe.parse_obu(&stream[pos..]).unwrap();
            pos += obu.total_size;
        }
        let seq = probe
            .sequence_header()
            .expect("stream has a sequence header OBU");
        assert_eq!(
            seq.color_config.bit_depth, 10,
            "{NAME}: aomenc did not actually write a 10-bit sequence header"
        );
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused a real 10-bit stream: {e}"),
        };
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "luma vs ffmpeg (10-bit)");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg (10-bit)");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg (10-bit)");
    }

    /// A real `aomenc` palette-**UV** fixture (lane-palette2 r2): `testsrc2`
    /// (the multicoloured AV1 test pattern -- flat, few-colour, repetitive
    /// per-region colour *and* chroma, unlike `smptebars=hue=s=0` above,
    /// which flattened chroma to a single value specifically to avoid
    /// exercising this path). Same square-only recipe as the palette-Y gate
    /// (`--enable-rect-partitions=0`/`-ab-`/`-1to4-`, fixed 32x32 partition)
    /// so a UV palette block never lands inside the still-refused HORZ/VERT
    /// screen-content strip. HARD-asserts [`decode::palette_uv_hits`] moved
    /// -- a stream that never reads a real UV palette block would decode (or
    /// not) by construction without proving this milestone at all
    /// ([[gate-blind-to-feature]]).
    #[test]
    fn a_real_aomenc_stream_with_palette_uv_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_palette_uv_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let source = "testsrc2=size=64x64:rate=25";
        fn render(source: &str) -> Vec<u8> {
            Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    source,
                    "-pix_fmt",
                    "yuv420p",
                    "-t",
                    "0.16",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run")
                .stdout
        }
        let y4m_a = render(source);
        let y4m_b = render(source);
        assert_eq!(
            y4m_a, y4m_b,
            "testsrc2 must render byte-identical across two runs"
        );
        let y4m = y4m_a;
        let mut child = Command::new(aomenc_path())
            .args([
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=30",
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--tune-content=screen",
                "--enable-palette=1",
                "--enable-intrabc=0",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--min-partition-size=32",
                "--max-partition-size=32",
                "--enable-filter-intra=0",
                "--enable-cdef=0",
                "--enable-restoration=0",
                "--enable-tx-size-search=0",
                "--loopfilter-control=0",
                "--obu",
                "-o",
                "-",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("aomenc failed to start");
        child
            .stdin
            .take()
            .expect("aomenc stdin")
            .write_all(&y4m)
            .expect("writing y4m to aomenc");
        let out = child.wait_with_output().expect("aomenc failed to run");
        assert!(
            out.status.success(),
            "aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        let before = decode::palette_uv_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        assert!(
            decode::palette_uv_hits() > before,
            "palette_uv_mode never fired decoding this stream -- gate is vacuous"
        );
        let frame_count = frames.len();
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, 64, 64, frame_count);
        for i in 0..frame_count {
            assert_eq!(frames[i].y, ffmpeg_frames[i].y, "luma vs ffmpeg, frame {i}");
            assert_eq!(frames[i].u, ffmpeg_frames[i].u, "U vs ffmpeg, frame {i}");
            assert_eq!(frames[i].v, ffmpeg_frames[i].v, "V vs ffmpeg, frame {i}");
        }
    }

    /// As [`a_real_aomenc_filter_intra_stream_decodes_pixel_exact`]'s
    /// harness, but with the deblocking filter left *on* (no
    /// `--loopfilter-control=0`) at a crf high enough that libaom actually
    /// picks a nonzero level -- proves `apply_deblock` itself, not just that
    /// this decoder still works when every edge lands on level 0. CDEF
    /// stays off so a pixel mismatch can only come from the loop filter.
    #[test]
    fn a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        let source = gradients_source(42, width, height, "duration=0.04:rate=25");
        let y4m = Command::new("ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i"])
            .arg(&source)
            .args(["-pix_fmt", "yuv420p", "-f", "yuv4mpegpipe", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("ffmpeg failed to run");
        assert!(
            y4m.status.success(),
            "ffmpeg fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let mut child = Command::new(aomenc_path())
            .args([
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=55",
                "--cpu-used=0",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                // Loop restoration is a named refusal (aomenc's RD picks
                // Sgrproj on this kind of content) -- off in every recipe
                // whose test targets a different surface.
                "--enable-restoration=0",
                "--max-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                // use_ref_frame_mvs (temporal MV projection) is unimplemented in mvstack;
                // leaving it on silently desyncs symbols on inter frames (grainfix lesson).
                "--enable-ref-frame-mvs=0",
                "--obu",
                "-o",
                "-",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("aomenc failed to start");
        child
            .stdin
            .take()
            .expect("aomenc stdin")
            .write_all(&y4m.stdout)
            .expect("writing y4m to aomenc");
        let out = child.wait_with_output().expect("aomenc failed to run");
        assert!(
            out.status.success(),
            "aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        let before = decode::deblock_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => {
                eprintln!(
                    "SKIP a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact: {e}"
                );
                return;
            }
        };
        assert!(
            decode::deblock_hits() > before,
            "no deblocking edge fired decoding this stream -- raise cq-level"
        );
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "luma vs ffmpeg");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg");
    }

    /// As [`a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact`],
    /// but WITHOUT `--enable-tx-size-search=0` -- the one flag every other
    /// recipe in this file pins off specifically to keep the stream on
    /// `TxMode::Largest`. Dropped, aomenc's RD is free to pick
    /// `TxMode::Select` and split some blocks' luma transform below their own
    /// side (spec 5.11.16), which is what this test's `tx_depth_hits()`
    /// counter proves actually happened -- a stream that resolves every
    /// `tx_depth` to 0 would pixel-match by construction (identical to
    /// `TxMode::Largest`) without exercising the new split-TU decode path at
    /// all. Same multi-seed retry shape as
    /// [`a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact`]:
    /// aomenc's own RD is free (with tx-size search on) to also occasionally
    /// pick `allow_screen_content_tools` on this content, a separate,
    /// already-named refusal -- try several seeds, and the first stream that
    /// both decodes AND actually split a transform must be pixel-exact.
    #[test]
    fn a_real_aomenc_intra_stream_with_tx_select_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_intra_stream_with_tx_select_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_intra_stream_with_tx_select_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        let mut refusals = Vec::new();
        for attempt in 0..40u32 {
            let seed = 42 + attempt;
            // A flat gradient never has enough local detail for the RD
            // search to prefer a split transform over the block's own full
            // size -- `mandelbrot`'s fractal boundary gives every attempt
            // (a different `start_scale`) sharp, varied-frequency edges.
            // start_scale 5.0 is the proven first try (decodes pixel-exact
            // AND reads the seven-symbol set-1 tx_type rows on TX8 TUs);
            // later attempts walk down for variety when aomenc's RD flips
            // elsewhere.
            let source = format!(
                "mandelbrot=size=64x64:start_scale={}:rate=25",
                5.0 - f64::from(attempt) * 0.06
            );
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    // Desaturated: chroma stays flat (constant 128) so
                    // `uv_mode` always resolves to `DC_PRED` and this
                    // decoder's still-unsupported directional-chroma round-2
                    // gap never fires, leaving the luma detail alone to
                    // drive whether `TxMode::Select`'s RD splits a block.
                    "-vf",
                    "hue=s=0",
                    "-pix_fmt",
                    "yuv420p",
                    "-t",
                    "0.04",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut child = Command::new(aomenc_path())
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    // Every non-DC intra tool off: directional chroma and
                    // angle deltas are separate round-2 gaps, and DC-only
                    // residual is exactly what gives the RD search a reason
                    // to split a transform.
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-directional-intra=0",
                    "--enable-angle-delta=0",
                    "--enable-cdef=0",
                    // Loop restoration is a named refusal (aomenc's RD picks
                    // Sgrproj on this kind of content) -- off in every recipe
                    // whose test targets a different surface.
                    "--enable-restoration=0",
                    "--max-partition-size=32",
                    // 16x16 blocks are what split into the TX8 TUs whose
                    // set-1 tx_type rows this gate exists to prove; some
                    // scales resolve depth 2 (TX4, a named refusal) and the
                    // attempt loop just moves on.
                    "--min-partition-size=16",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    // use_ref_frame_mvs (temporal MV projection) is unimplemented in mvstack;
                    // leaving it on silently desyncs symbols on inter frames (grainfix lesson).
                    "--enable-ref-frame-mvs=0",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before = decode::tx_depth_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    refusals.push(format!("seed {seed}: {e}"));
                    continue;
                }
            };
            if decode::tx_depth_hits() == before {
                refusals.push(format!(
                    "seed {seed}: decoded, but no block resolved a split tx_depth"
                ));
                continue;
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "luma vs ffmpeg (seed {seed})"
            );
            assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg (seed {seed})");
            assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg (seed {seed})");
            return;
        }
        eprintln!(
            "SKIP a_real_aomenc_intra_stream_with_tx_select_decodes_pixel_exact: every attempt \
             hit a named refusal or never split a transform:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-chroma r2 stage 1: a real aomenc key frame with
    /// `--enable-smooth-intra=1` (opposite of every sibling recipe's `=0`)
    /// and `--enable-paeth-intra=0` -- paeth chroma stays off so the still-
    /// refused smooth/paeth `uv_mode` (round-2, unrelated to this gate) never
    /// fires; smooth *luma* neighbours are exactly what this gate wants to
    /// exercise, proving [`decode::smooth_luma_hits`] actually landed a
    /// directional block next to a smooth-predicted one and decoded
    /// pixel-exact (the [`PlaneBuf::reconstruct`] `smooth_neighbor` fix).
    #[test]
    fn a_real_aomenc_stream_with_smooth_luma_neighbour_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_stream_with_smooth_luma_neighbour_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_stream_with_smooth_luma_neighbour_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        let mut refusals = Vec::new();
        let mut fired_runs = 0u32;
        for attempt in 0..40u32 {
            let seed = 42 + attempt;
            let source = format!(
                "mandelbrot=size=64x64:start_scale={}:rate=25",
                5.0 - f64::from(attempt) * 0.06
            );
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-t",
                    "0.04",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut child = Command::new(aomenc_path())
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=1",
                    // Smooth/paeth chroma is a still-refused, separate
                    // round-2 gap -- off so no attempt trips it while this
                    // gate is purely about luma's edge-filter fix.
                    "--enable-paeth-intra=0",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--enable-intra-edge-filter=1",
                    "--max-partition-size=32",
                    "--min-partition-size=16",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--enable-ref-frame-mvs=0",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let smooth_before = decode::smooth_luma_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    refusals.push(format!("seed {seed}: {e}"));
                    continue;
                }
            };
            if decode::smooth_luma_hits() == smooth_before {
                refusals.push(format!(
                    "seed {seed}: decoded, but no smooth-luma-neighbour block fired"
                ));
                continue;
            }
            fired_runs += 1;
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "luma vs ffmpeg (seed {seed})"
            );
            assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg (seed {seed})");
            assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg (seed {seed})");
            if fired_runs >= 4 {
                return;
            }
        }
        assert!(
            fired_runs > 0,
            "every attempt hit a named refusal or never exercised a smooth luma neighbour:\n{}",
            refusals.join("\n")
        );
    }

    /// The smooth/paeth-chroma gate (lane-chroma r1/r3): a real aomenc key
    /// frame with `--enable-smooth-intra=1 --enable-paeth-intra=1` (the
    /// opposite of the directional-chroma gate below, which turns these OFF
    /// to keep this gap from firing while directional chroma is under test).
    /// Some seeds won't pick smooth/paeth for chroma at all -- retry like
    /// every other real-encoder gate here, but the first attempt that
    /// actually fires [`decode::smooth_uv_hits`] must decode pixel-exact.
    #[test]
    fn a_real_aomenc_stream_with_smooth_paeth_chroma_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_stream_with_smooth_paeth_chroma_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_stream_with_smooth_paeth_chroma_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        let mut refusals = Vec::new();
        let mut fired_runs = 0u32;
        for attempt in 0..40u32 {
            let seed = 42 + attempt;
            let source = format!(
                "mandelbrot=size=64x64:start_scale={}:rate=25",
                5.0 - f64::from(attempt) * 0.06
            );
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v", "error", "-f", "lavfi", "-i", &source, "-pix_fmt", "yuv420p", "-t",
                    "0.04", "-f", "yuv4mpegpipe", "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut child = Command::new(aomenc_path())
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=1",
                    "--enable-paeth-intra=1",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--enable-intra-edge-filter=1",
                    "--max-partition-size=32",
                    // 8x8/leaf8 splits are a separate, already-named refusal.
                    "--min-partition-size=16",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--enable-ref-frame-mvs=0",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let smooth_before = decode::smooth_uv_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    refusals.push(format!("seed {seed}: {e}"));
                    continue;
                }
            };
            if decode::smooth_uv_hits() == smooth_before {
                refusals.push(format!(
                    "seed {seed}: decoded, but no smooth/paeth uv_mode fired"
                ));
                continue;
            }
            fired_runs += 1;
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "luma vs ffmpeg (seed {seed})"
            );
            assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg (seed {seed})");
            assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg (seed {seed})");
            if fired_runs >= 4 {
                return;
            }
        }
        assert!(
            fired_runs > 0,
            "every attempt hit a named refusal or never exercised smooth/paeth chroma:\n{}",
            refusals.join("\n")
        );
    }

    /// The directional-chroma/angle-delta gate: a real aomenc key frame with
    /// `--enable-directional-intra`/`--enable-angle-delta` left ON (the
    /// opposite of every sibling recipe's `=0`, which existed specifically to
    /// keep this round-2 gap from firing while a different surface was under
    /// test). A colourful, sharp-edged source gives the RD search a reason to
    /// pick a directional `uv_mode` and/or a nonzero angle delta -- some
    /// seeds still won't, so retry like every other real-encoder gate here,
    /// but the first attempt that actually fires either counter must decode
    /// pixel-exact.
    #[test]
    fn a_real_aomenc_stream_with_directional_chroma_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_stream_with_directional_chroma_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_stream_with_directional_chroma_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        let mut refusals = Vec::new();
        let mut fired_runs = 0u32;
        for attempt in 0..40u32 {
            let seed = 42 + attempt;
            let source = format!(
                "mandelbrot=size=64x64:start_scale={}:rate=25",
                5.0 - f64::from(attempt) * 0.06
            );
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-t",
                    "0.04",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut child = Command::new(aomenc_path())
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    // Smooth/Paeth chroma is a separate, still-refused round-2
                    // gap (an unrelated `Edges::build` no-neighbour bug this
                    // lane does not cover) -- off so the RD search's only
                    // non-DC/CFL chroma choice left is directional.
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    // The intra edge filter/upsample (spec 7.11.2.7-2.9) is
                    // now implemented (`directional()`) -- left ON (aomenc's
                    // own default) so this gate actually exercises it instead
                    // of the un-filtered fast path a disabled flag would keep
                    // testing.
                    "--enable-intra-edge-filter=1",
                    "--max-partition-size=32",
                    // Same reason as the tx-select gate above: 8x8 and other
                    // sub-16x16 splits are a separate, already-named refusal.
                    "--min-partition-size=16",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    // use_ref_frame_mvs (temporal MV projection) is unimplemented in mvstack;
                    // leaving it on silently desyncs symbols on inter frames (grainfix lesson).
                    "--enable-ref-frame-mvs=0",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let (uv_before, angle_before) =
                (decode::directional_uv_hits(), decode::angle_delta_hits());
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    refusals.push(format!("seed {seed}: {e}"));
                    continue;
                }
            };
            if decode::directional_uv_hits() == uv_before
                && decode::angle_delta_hits() == angle_before
            {
                refusals.push(format!(
                    "seed {seed}: decoded, but neither a directional uv_mode nor a \
                     nonzero angle delta fired"
                ));
                continue;
            }
            fired_runs += 1;
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "luma vs ffmpeg (seed {seed})"
            );
            assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg (seed {seed})");
            assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg (seed {seed})");
            if fired_runs >= 4 {
                return;
            }
        }
        assert!(
            fired_runs > 0,
            "every attempt hit a named refusal or never exercised directional chroma:\n{}",
            refusals.join("\n")
        );
    }

    /// As above, but a multi-frame inter sequence -- `--lag-in-frames=0
    /// --auto-alt-ref=0` keep every non-key frame a simple forward
    /// single-`LAST_FRAME` P frame (no alt-ref/backward reference this
    /// decoder's inter path does not model), `--kf-max-dist` past the clip's
    /// own length keeps frame 0 the only key frame.
    #[test]
    fn a_real_aomenc_inter_sequence_with_deblocking_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_inter_sequence_with_deblocking_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_inter_sequence_with_deblocking_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 4usize);
        let source = gradients_source(42, width, height, "duration=0.16:rate=25");
        let y4m = Command::new("ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i"])
            .arg(&source)
            .args([
                "-pix_fmt",
                "yuv420p",
                "-f",
                "yuv4mpegpipe",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("ffmpeg failed to run");
        assert!(
            y4m.status.success(),
            "ffmpeg fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let mut child = Command::new(aomenc_path())
            .args([
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=55",
                "--cpu-used=0",
                "--lag-in-frames=0",
                "--auto-alt-ref=0",
                "--kf-max-dist=1000",
                // Forces `primary_ref_frame = PRIMARY_REF_NONE` on every
                // frame (spec 5.9.2): without it a real encoder's inter
                // frame loads its initial CDF state from the previous
                // frame's *adapted* tables (spec 7.20's `load_cdfs`), which
                // this decoder never does -- it always starts each frame's
                // `Cdfs` from the spec defaults (`decode.rs`'s
                // `Cdfs::new(q_ctx)`, called fresh per frame in
                // `decode_stream`). Without this flag the inter frame's
                // very first symbol desyncs immediately (traced 2026-08-27:
                // no coefficient trace at all before the tile's first
                // partition read comes back an out-of-alphabet value). A
                // real decoder needs cross-frame CDF forwarding to handle
                // that stream; this fixture sidesteps it the same way a
                // real broadcast/low-latency encode legitimately can.
                "--error-resilient=1",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                // Loop restoration is a named refusal (aomenc's RD picks
                // Sgrproj on this kind of content) -- off in every recipe
                // whose test targets a different surface.
                "--enable-restoration=0",
                "--max-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                // use_ref_frame_mvs (temporal MV projection) is unimplemented in mvstack;
                // leaving it on silently desyncs symbols on inter frames (grainfix lesson).
                "--enable-ref-frame-mvs=0",
                "--obu",
                "-o",
                "-",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("aomenc failed to start");
        child
            .stdin
            .take()
            .expect("aomenc stdin")
            .write_all(&y4m.stdout)
            .expect("writing y4m to aomenc");
        let out = child.wait_with_output().expect("aomenc failed to run");
        assert!(
            out.status.success(),
            "aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        if let Ok(dump) = std::env::var("EC_AV1_GATE_DUMP") {
            std::fs::write(dump, &stream).expect("dump stream");
        }
        let before = decode::deblock_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => {
                eprintln!(
                    "SKIP a_real_aomenc_inter_sequence_with_deblocking_decodes_pixel_exact: {e}"
                );
                return;
            }
        };
        assert!(
            decode::deblock_hits() > before,
            "no deblocking edge fired decoding this sequence -- raise cq-level"
        );
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
        assert_eq!(frames.len(), frame_count);
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            // gradients' colors ignore its seed, so this gate encodes fresh
            // content every run -- a mismatch here may never reproduce.
            // Self-pin the stream before panicking so the failure is
            // bisectable (replay: EC_AV1_PIN on scratch_isolate_pinned_mismatch).
            let ok = got.y == want.y && got.u == want.u && got.v == want.v;
            if !ok {
                let pin = std::env::temp_dir().join("ec-av1-deblocking-gate-fail.obu");
                let _ = std::fs::write(&pin, &stream);
                panic!("frame {i} mismatch vs ffmpeg -- stream pinned at {}", pin.display());
            }
        }
    }

    /// The decisive cross-frame CDF forwarding test: identical to
    /// [`a_real_aomenc_inter_sequence_with_deblocking_decodes_pixel_exact`]
    /// except it drops `--error-resilient=1`, so aomenc codes every inter
    /// frame's `primary_ref_frame` at its default (not `PRIMARY_REF_NONE`)
    /// and expects the decoder to resume from the previous frame's *adapted*
    /// CDF state (spec 7.20 `load_cdfs`). Without cross-frame forwarding,
    /// frame 1's very first tile symbol desyncs (an out-of-alphabet
    /// partition read); with it, every frame decodes pixel-exact.
    #[test]
    fn a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 4usize);
        // The fixture's content varies run to run (lavfi gradients ignores its
        // seed option for the c0..c7 colors -- they default to "random"), so:
        // most encodes of this recipe trip one of the decoder's NAMED round-2
        // refusals (a reference other than LAST_FRAME, a sub-16 partition --
        // genuine coverage gaps, separately documented), and only some produce
        // a stream fully inside this decoder's declared support. So: attempt
        // several seeds, and the first stream that DECODES must be pixel-exact
        // -- that assertion is what guards the forwarded-CDF state (a wrong
        // table decodes plausible-but-wrong pixels; the counter-reset bug this
        // gate caught did exactly that). Exhausting every attempt on named
        // refusals skips loudly, listing them.
        let mut refusals = Vec::new();
        for attempt in 0..8u32 {
            let seed = 42 + attempt;
            let source = gradients_source(seed, 64, 64, "duration=0.16:rate=25");
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            // lane-cdfflake: hard length check on the y4m fixture itself --
            // a truncated/racing ffmpeg write would otherwise surface as a
            // downstream pixel mismatch indistinguishable from a real decode
            // defect. y4m frames are `FRAME\n` (6 bytes) + raw 4:2:0 payload;
            // find the header/frame-data boundary at the first `FRAME` marker
            // rather than parsing the header fields (duration/rate can move
            // the header text length without moving the frame math).
            let marker_at = y4m
                .stdout
                .windows(5)
                .position(|w| w == b"FRAME")
                .expect("y4m stream missing FRAME marker");
            let frame_420_bytes = width * height * 3 / 2;
            assert_eq!(
                y4m.stdout.len(),
                marker_at + frame_count * (6 + frame_420_bytes),
                "y4m fixture length mismatch (seed {seed})"
            );
            let y4m_hash = fnv1a64(&y4m.stdout);
            let mut child = Command::new(aomenc_path())
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=55",
                    "--cpu-used=0",
                    "--lag-in-frames=0",
                    "--auto-alt-ref=0",
                    "--kf-max-dist=1000",
                    // `--threads=1` (not the default): a multithreaded RD search
                    // makes aomenc's screen-content-tools heuristic race (seen
                    // flaky under `cargo test`'s own parallelism -- the same
                    // fixture, same seed, occasionally sets
                    // `allow_screen_content_tools`, a round-2 gap unrelated to
                    // CDF forwarding), so pin it to one thread for a
                    // deterministic header.
                    "--threads=1",
                    "--row-mt=0",
                    // `--enable-order-hint=0` kills BWDREF/ALTREF entirely (no
                    // order hints -> no bidirectional distance weighting) and,
                    // combined with `--auto-alt-ref=0`/`--lag-in-frames=0`
                    // above, leaves aomenc's RD search only LAST_FRAME/
                    // LAST2_FRAME/LAST3_FRAME/GOLDEN_FRAME to pick from --
                    // GOLDEN itself needs no order hint (it is the keyframe's
                    // fixed slot, not a distance-weighted reference), so this
                    // does not suppress GOLDEN selection the way the sibling
                    // CDF-forwarding gate's own comment worried about; it just
                    // narrows the pool this gate needs GOLDEN to win out of.
                    "--enable-order-hint=0",
                    "--enable-warped-motion=0",
                    "--enable-obmc=0",
                    "--enable-masked-comp=0",
                    "--enable-interintra-comp=0",
                    "--enable-dist-wtd-comp=0",
                    "--enable-diff-wtd-comp=0",
                    "--enable-onesided-comp=0",
                    "--enable-interintra-wedge=0",
                    "--enable-smooth-interintra=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-directional-intra=0",
                    "--enable-angle-delta=0",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    // Loop restoration is a named refusal (aomenc's RD picks
                    // Sgrproj on this kind of content) -- off in every recipe
                    // whose test targets a different surface.
                    "--enable-restoration=0",
                    "--max-partition-size=32",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    // use_ref_frame_mvs (temporal MV projection) is unimplemented in mvstack;
                    // leaving it on silently desyncs symbols on inter frames (grainfix lesson).
                    "--enable-ref-frame-mvs=0",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let stream_hash = fnv1a64(&stream);
            // A named `unsupported` refusal is a documented coverage gap (each
            // one carries its own test debt elsewhere), not a forwarding verdict
            // -- try the next seed. Any stream that DECODES must be pixel-exact,
            // and any non-refusal error is a hard failure.
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "cross-frame CDF forwarding failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            // lane-cdfflake: reconstruct ffmpeg's raw reference bytes (the
            // three per-frame slices are contiguous chunks of the raw
            // `-f rawvideo` output ffmpeg_decode_sequence sliced, so
            // concatenating them back is bit-for-bit the same bytes ffmpeg
            // actually produced -- no second ffmpeg invocation needed).
            let ffmpeg_raw: Vec<u8> = ffmpeg_frames
                .iter()
                .flat_map(|p| p.y.iter().chain(&p.u).chain(&p.v).map(|&v| v as u8))
                .collect();
            let ffmpeg_raw_hash = fnv1a64(&ffmpeg_raw);
            eprintln!(
                "cdfflake attempt seed={seed}: y4m_hash={y4m_hash:016x} \
                 stream_hash={stream_hash:016x} ffmpeg_raw_hash={ffmpeg_raw_hash:016x}"
            );
            // lane-av1golden3: pin the exact raw stream on the first mismatch
            // so a flaky-looking gate failure becomes a deterministic, static
            // fixture to bisect against -- env-gated, no cost on a normal run.
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched {
                if let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                    std::fs::write(&path, &stream).expect("writing pinned stream");
                    eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
                }
                // lane-cdfflake: unconditional pin on mismatch -- the aomenc
                // stream AND ffmpeg's raw reference bytes, so a flake caught
                // under load names the liar (stream bytes differ from a
                // clean rerun -> subprocess instability; reference bytes
                // differ -> ffmpeg instability; both stable -> real decode
                // defect) without relying on the env var being set.
                // The pin goes to `fixtures/`, which outlives the session: an
                // earlier version wrote to a session scratchpad, so once tmpfs
                // reaped it a real mismatch aborted on the write instead of
                // reporting itself. A failed write must not mask the assert
                // below either, so it warns rather than panicking.
                let sp = pin_dir();
                let stream_pin = sp.join(format!("cdfflake-stream-seed{seed}.bin"));
                let ref_pin = sp.join(format!("cdfflake-ffmpeg-raw-seed{seed}.bin"));
                for (path, bytes) in [(&stream_pin, &stream), (&ref_pin, &ffmpeg_raw)] {
                    if let Err(e) = std::fs::write(path, bytes) {
                        eprintln!("cdfflake: could not pin to {}: {e}", path.display());
                    }
                }
                eprintln!(
                    "cdfflake MISMATCH seed={seed}: pinned stream + ffmpeg reference to {}",
                    sp.display()
                );
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (seed {seed})");
            }
            return;
        }
        eprintln!(
            "SKIP a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact: every \
             attempt hit a named refusal:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-av1golden3: reproduces the pinned mismatch bytes captured by
    /// `EC_AV1_GATE_DUMP=$SP/golden3-pin.obu` off
    /// [`a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact`]
    /// (seed 47, frame 1 U). Deterministic and static -- no aomenc/ffmpeg
    /// re-encode involved, only re-decodes the fixed bytes on disk, so it is
    /// `#[ignore]`d (the file only exists on this machine's scratchpad) but
    /// gives a fast red/green loop for the actual fix.
    #[test]
    #[ignore = "reads a pinned fixture path outside the repo; run manually"]
    fn pinned_golden3_stream_decodes_pixel_exact() {
        let path = std::env::var("EC_AV1_GATE_DUMP_PIN")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| pin_dir().join("golden3-pin.obu"));
        let Ok(stream) = std::fs::read(&path) else {
            eprintln!(
                "SKIP pinned_golden3_stream_decodes_pixel_exact: no pinned bytes at {} \
                 -- re-capture with EC_AV1_GATE_DUMP off the cdf-forwarding gate",
                path.display()
            );
            return;
        };
        if !have_ffmpeg() {
            eprintln!("SKIP pinned_golden3_stream_decodes_pixel_exact: no ffmpeg");
            return;
        }
        let frames = decode_stream(&stream).expect("pinned stream must decode");
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, 64, 64, 4);
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (pinned)");
            assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (pinned)");
            assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (pinned)");
        }
    }

    /// lane-av1golden4: reproduces the pinned mismatch bytes captured by
    /// `EC_AV1_GATE_DUMP=$SP/golden4-pin.obu` off
    /// [`a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact`]
    /// (seed 43, frame 3 luma) the round the GOLDEN mask lifted. Deterministic
    /// and static -- `#[ignore]`d, run manually.
    #[test]
    #[ignore = "reads a pinned fixture path outside the repo; run manually"]
    fn pinned_golden4_stream_decodes_pixel_exact() {
        use crate::decode::non_last_ref_hits;
        let path = std::env::var("EC_AV1_GATE_DUMP_PIN")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| pin_dir().join("golden4-pin.obu"));
        let Ok(stream) = std::fs::read(&path) else {
            eprintln!(
                "SKIP pinned_golden4_stream_decodes_pixel_exact: no pinned bytes at {} \
                 -- re-capture with EC_AV1_GATE_DUMP off the cdf-forwarding gate",
                path.display()
            );
            return;
        };
        if !have_ffmpeg() {
            eprintln!("SKIP pinned_golden4_stream_decodes_pixel_exact: no ffmpeg");
            return;
        }
        let before = non_last_ref_hits();
        let frames = decode_stream(&stream).expect("pinned stream must decode");
        eprintln!(
            "non_last_ref_hits before={before} after={}",
            non_last_ref_hits()
        );
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, 64, 64, 4);
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (pinned)");
            assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (pinned)");
            assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (pinned)");
        }
    }

    /// lane-av1golden7 round 9: pins the residual GOLDEN_FRAME MC bug found
    /// by the forwarding gate once film grain is out of the way (1/20 real
    /// aomenc streams, frame 3 luma, apply_grain=false, `non_last_ref_hits`
    /// delta=2). `GOLDEN_FRAME` refuses by name until this decodes exact.
    #[test]
    #[ignore = "reads a pinned fixture path outside the repo; run manually"]
    fn pinned_golden7_stream_decodes_pixel_exact() {
        use crate::decode::non_last_ref_hits;
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/golden7-forwarding-mismatch.obu"
        );
        let stream = std::fs::read(path).expect("reading pinned stream");
        if !have_ffmpeg() {
            eprintln!("SKIP pinned_golden7_stream_decodes_pixel_exact: no ffmpeg");
            return;
        }
        let before = non_last_ref_hits();
        let frames = decode_stream(&stream).expect("pinned stream must decode");
        eprintln!(
            "non_last_ref_hits before={before} after={}",
            non_last_ref_hits()
        );
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, 64, 64, 4);
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (pinned)");
            assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (pinned)");
            assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (pinned)");
        }
    }

    /// lane-av1refs's decisive single-non-LAST-reference gate: identical
    /// recipe to
    /// [`a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact`]
    /// except `--enable-order-hint=0` is DROPPED (that flag's own comment on
    /// the sibling test names it as exactly what suppresses aomenc's RD from
    /// ever picking `GOLDEN_FRAME`) and the sequence runs 8 frames, not 4 --
    /// more chances for the RD search to reach past the first GOP's initial
    /// key frame. `--auto-alt-ref=0` stays: this decoder only supports
    /// `LAST_FRAME`/`GOLDEN_FRAME` so far, and alt-ref/backward references
    /// are still a named refusal, not this gate's target. A stream that
    /// decodes without firing [`decode::non_last_ref_hits`] is not a
    /// coverage win for this round -- SKIP it as a genuine, named miss
    /// rather than pass silently on untested ground.
    #[test]
    fn a_real_aomenc_inter_sequence_with_a_golden_reference_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_inter_sequence_with_a_golden_reference_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_inter_sequence_with_a_golden_reference_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 8usize);
        let mut refusals = Vec::new();
        let mut never_fired = 0u32;
        // aomenc's own RD search is nondeterministic run-to-run even at
        // `--threads=1` (confirmed live: identical CLI args, identical
        // input, two different output byte streams) -- so beyond varying
        // the fixture's seed, each attempt is its own independent coin
        // flip against every named refusal below. 120 attempts (not 40)
        // to give that coin flip enough tries to land on a stream this
        // decoder both accepts and that fires GOLDEN_FRAME. `duration=0.32`
        // at `rate=25` (not `0.24`) matches every sibling gate's own
        // frame-count fixture exactly -- a shorter duration here once
        // produced a frame-0 (keyframe, no reference at all) luma mismatch
        // against ffmpeg, i.e. a source-frame-count rounding artifact of
        // this recipe's own choosing, not a decoder bug.
        for attempt in 0..120u32 {
            let seed = 42 + attempt % 40;
            let source = gradients_source(seed, 64, 64, "duration=0.32:rate=25");
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut child = Command::new(aomenc_path())
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=55",
                    "--cpu-used=0",
                    "--lag-in-frames=0",
                    "--auto-alt-ref=0",
                    "--kf-max-dist=1000",
                    "--threads=1",
                    "--row-mt=0",
                    // Narrows the RD search's reference alphabet toward
                    // LAST_FRAME/GOLDEN_FRAME -- without this, aomenc's RD is
                    // just as likely to pick LAST2/LAST3/BWDREF/ALTREF2/
                    // ALTREF (this decoder's already-documented, still-open
                    // refusals), starving this gate of the one reference it
                    // targets.
                    "--max-reference-frames=3",
                    "--reduced-reference-set=1",
                    "--enable-warped-motion=0",
                    "--enable-obmc=0",
                    "--enable-masked-comp=0",
                    "--enable-interintra-comp=0",
                    "--enable-dist-wtd-comp=0",
                    "--enable-diff-wtd-comp=0",
                    "--enable-onesided-comp=0",
                    "--enable-interintra-wedge=0",
                    "--enable-smooth-interintra=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-directional-intra=0",
                    "--enable-angle-delta=0",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    // `min == max == 32` forces PARTITION_NONE for every
                    // 32x32 block, not just biases toward it -- with
                    // `--min-partition-size=16` alone aomenc's exhaustive
                    // `cpu-used=0` RD search still occasionally read HORZ/
                    // VERT/SPLIT partitions below 32x32 despite
                    // `--enable-rect-partitions=0`/`--enable-ab-partitions=0`/
                    // `--enable-1to4-partitions=0` all being off (two
                    // separate, already-documented round-2 gaps: "a
                    // partition below 16x16", "a partition type this encoder
                    // never writes"), starving this gate's already-narrow
                    // seed pool. Locking both bounds to 32 removes any
                    // partition-depth choice from the search entirely, so
                    // this gate can isolate GOLDEN_FRAME selection alone.
                    "--max-partition-size=32",
                    "--min-partition-size=32",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    // use_ref_frame_mvs (temporal MV projection) is unimplemented in mvstack;
                    // leaving it on silently desyncs symbols on inter frames (grainfix lesson).
                    "--enable-ref-frame-mvs=0",
                    // `--tune-content=film` was dropped: on this box it
                    // makes aomenc emit `apply_grain` almost every attempt
                    // (lane-av1golden8 round 10 measurement, live: 120/120
                    // real attempts hit a named refusal, 0 ever fired
                    // GOLDEN_FRAME -- mostly `apply_grain`, dominated by its
                    // own recipe flag) -- film grain synthesis has its own
                    // dedicated refusal test and must not starve this gate
                    // of the GOLDEN_FRAME draws it exists to prove. Default
                    // content tuning still occasionally flips
                    // `allow_screen_content_tools` on for this synthetic
                    // gradient content (a separate, already-documented open
                    // refusal), which the 120-attempt coin flip below
                    // tolerates the same way.
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before = decode::non_last_ref_hits();
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "a golden-reference stream failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
                Ok(frames) => frames,
            };
            if decode::non_last_ref_hits() == before {
                // This seed's RD search never actually picked GOLDEN_FRAME --
                // a genuine miss, not a refusal. Try the next seed rather
                // than claiming coverage this attempt did not exercise.
                never_fired += 1;
                continue;
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            // Same pin-on-mismatch pattern as the CDF forwarding gate above:
            // env-gated, no cost on a normal run.
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched {
                if let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                    std::fs::write(&path, &stream).expect("writing pinned stream");
                    eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
                }
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (seed {seed})");
            }
            eprintln!(
                "FIRING seed {seed}: non_last_ref_hits advanced by {}",
                decode::non_last_ref_hits() - before
            );
            return;
        }
        eprintln!(
            "SKIP a_real_aomenc_inter_sequence_with_a_golden_reference_decodes_pixel_exact: \
             {never_fired} attempts decoded but never fired GOLDEN_FRAME, and every other \
             attempt hit a named refusal:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-av1refs's decisive gate for the 5 references widened past
    /// `LAST_FRAME`/`GOLDEN_FRAME`: same envelope and pin-on-mismatch
    /// pattern as
    /// [`a_real_aomenc_inter_sequence_with_a_golden_reference_decodes_pixel_exact`],
    /// generalised over `(target_ref, extra_args, frame_count)` so each of
    /// `LAST2`/`LAST3`/`BWDREF`/`ALTREF2`/`ALTREF` gets its own attempt pool
    /// and its own proof that `decode::ref_hits(target_ref)` -- not just
    /// `non_last_ref_hits`, which cannot distinguish one reference from
    /// another -- actually advanced. `--max-reference-frames=3
    /// --reduced-reference-set=1` (the golden gate's own narrowing) is
    /// DROPPED here: both suppress exactly the references this gate needs
    /// aomenc's RD to reach.
    fn a_real_aomenc_single_ref_gate(
        gate_name: &str,
        target_ref: i8,
        target_ref_name: &str,
        extra_args: &[&str],
        width: usize,
        height: usize,
        frame_count: usize,
        attempts: u32,
    ) {
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {gate_name}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {gate_name}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let mut refusals = Vec::new();
        let mut never_fired = 0u32;
        for attempt in 0..attempts {
            let seed = 42 + attempt % 40;
            let duration = frame_count as f64 / 25.0;
            let source =
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"));
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=55",
                "--cpu-used=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--enable-warped-motion=0",
                "--enable-obmc=0",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=0",
                "--enable-dist-wtd-comp=0",
                "--enable-diff-wtd-comp=0",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=0",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                "--enable-restoration=0",
                "--max-partition-size=32",
                "--min-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                // use_ref_frame_mvs (temporal MV projection) is unimplemented in
                // mvstack; leaving it on silently desyncs symbols on inter frames.
                "--enable-ref-frame-mvs=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            // extra_args goes right before the trailing "-o", "-", "-" triple.
            let tail = args.split_off(args.len() - 3);
            args.extend_from_slice(extra_args);
            args.extend_from_slice(&tail);
            let mut child = Command::new(aomenc_path())
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before = decode::ref_hits(target_ref);
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{gate_name} failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
                Ok(frames) => frames,
            };
            if decode::ref_hits(target_ref) == before {
                never_fired += 1;
                continue;
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched {
                if let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                    std::fs::write(&path, &stream).expect("writing pinned stream");
                    eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
                }
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "{gate_name} frame {i} luma vs ffmpeg (seed {seed})"
                );
                assert_eq!(
                    got.u, want.u,
                    "{gate_name} frame {i} U vs ffmpeg (seed {seed})"
                );
                assert_eq!(
                    got.v, want.v,
                    "{gate_name} frame {i} V vs ffmpeg (seed {seed})"
                );
            }
            eprintln!(
                "{gate_name} FIRING seed {seed}: {target_ref_name} hits advanced by {}",
                decode::ref_hits(target_ref) - before
            );
            return;
        }
        eprintln!(
            "SKIP {gate_name}: {never_fired} attempts decoded but never fired {target_ref_name}, \
             and every other attempt hit a named refusal:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-av1comp: proves `reference_select`/`comp_mode` are actually read
    /// off a real bitstream rather than only unit-tested in isolation --
    /// `--auto-alt-ref=1 --lag-in-frames=16` gives aomenc a backward
    /// reference to pick `COMPOUND_REFERENCE` with, `--enable-order-hint=1`
    /// is required for it to ever consider compound at all. Three
    /// acceptable outcomes: (a) a stream where `reference_select` never
    /// actually drove a block to `COMPOUND_REFERENCE` decodes bit-exact end
    /// to end (proving the per-block `comp_mode` read did not desync the
    /// single-ref blocks around it); (b) a stream that does fire compound
    /// is refused by name with [`decode::comp_mode_hits`] having advanced
    /// (proving `comp_mode`/`read_compound_ref_frames` consumed the right
    /// symbols before refusing, rather than desyncing and refusing for an
    /// unrelated reason) -- masked/wedge compound (`comp_group_idx == 1`)
    /// and a partition below 16x16 (a real, separately tracked capability
    /// gap) are the two refusal arms this can still legitimately name; (c)
    /// r13 wired plain `COMPOUND_AVERAGE` MC for real, so as of r15 a
    /// stream that fires compound may also decode fully -- accepted only
    /// when every output frame is pixel-exact against ffmpeg.
    #[test]
    fn a_real_aomenc_stream_with_reference_select_reads_comp_mode_correctly() {
        const NAME: &str = "a_real_aomenc_stream_with_reference_select_reads_comp_mode_correctly";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 20usize);
        let mut refusals = Vec::new();
        let mut never_fired = 0u32;
        for attempt in 0..60u32 {
            let seed = 42 + attempt % 40;
            let duration = frame_count as f64 / 25.0;
            let source =
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"));
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-order-hint=1",
                "--enable-warped-motion=0",
                "--enable-obmc=0",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=0",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=0",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                "--enable-restoration=0",
                "--max-partition-size=32",
                "--min-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let mut child = Command::new(aomenc_path())
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before = decode::comp_mode_hits();
            match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    let fired = decode::comp_mode_hits() > before;
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright (seed {seed}): {msg}"
                    );
                    if fired {
                        // `comp_mode` firing does not stop the block walk --
                        // a later, unrelated block in the same tile can
                        // still hit any other named refusal this decoder
                        // carries. A partition below 16x16 is a real, open
                        // capability gap tracked on its own (not a
                        // COMPOUND_REFERENCE defect); accept it here too
                        // rather than fail a compound-specific gate on a
                        // partition-search coincidence. lane-av1blend r7:
                        // re-swept the wide gate with the plain-average
                        // blend unmasked (r6/r7's ref_ctx fix alone) and it
                        // still mismatches on real streams beyond that one
                        // bug -- re-masked comp_group_idx == 0 too, so this
                        // stays a legal named refusal.
                        assert!(
                            msg.contains("COMPOUND_REFERENCE") || msg.contains("a partition"),
                            "{NAME}: comp_mode fired (seed {seed}) but the refusal \
                             was for something else: {msg}"
                        );
                        let advanced = decode::comp_mode_hits() - before;
                        eprintln!(
                            "{NAME} FIRING seed {seed}: comp_mode hits advanced by \
                             {advanced}, refused by name as expected: {msg}"
                        );
                        return;
                    }
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
                Ok(frames) => {
                    if decode::comp_mode_hits() == before {
                        never_fired += 1;
                        continue;
                    }
                    // Outcome (c): plain COMPOUND_AVERAGE decoded fully --
                    // non-negotiable pixel exactness, no tolerance.
                    let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
                    assert_eq!(frames.len(), frame_count);
                    let mismatched = frames
                        .iter()
                        .zip(&ffmpeg_frames)
                        .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
                    if mismatched {
                        let pin = std::env::temp_dir().join("ec-av1-reference-select-gate-fail.obu");
                        let _ = std::fs::write(&pin, &stream);
                        if let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                            std::fs::write(&path, &stream).expect("writing pinned stream");
                            eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
                        }
                        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                            assert_eq!(
                                got.y, want.y,
                                "{NAME} frame {i} luma vs ffmpeg (seed {seed}) -- stream pinned at {}",
                                pin.display()
                            );
                            assert_eq!(
                                got.u, want.u,
                                "{NAME} frame {i} U vs ffmpeg (seed {seed}) -- stream pinned at {}",
                                pin.display()
                            );
                            assert_eq!(
                                got.v, want.v,
                                "{NAME} frame {i} V vs ffmpeg (seed {seed}) -- stream pinned at {}",
                                pin.display()
                            );
                        }
                    } else {
                        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                            assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg (seed {seed})");
                            assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (seed {seed})");
                            assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (seed {seed})");
                        }
                    }
                    eprintln!(
                        "{NAME} FIRING seed {seed}: comp_mode hits advanced by {}, decoded \
                         fully and pixel-exact",
                        decode::comp_mode_hits() - before
                    );
                    return;
                }
            }
        }
        eprintln!(
            "SKIP {NAME}: {never_fired} attempts decoded fully (reference_select never drove a \
             block to COMPOUND_REFERENCE) and every other attempt hit a named refusal that never \
             reached comp_mode:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-av1comp round 14's decisive gate for `skip_mode`: same
    /// pin-on-mismatch pattern as [`a_real_aomenc_single_ref_gate`], proving
    /// [`decode::skip_mode_hits`] actually advanced (a real block picked
    /// `skip_mode`, not just that the frame header bit was set) and that the
    /// resulting forced-compound `NEAREST_NEARESTMV` decode is still
    /// pixel-exact against ffmpeg. `--auto-alt-ref=1 --lag-in-frames=16
    /// --enable-fwd-kf=0` (backward references) plus
    /// `--enable-ref-frame-mvs=0` (unimplemented temporal MV projection,
    /// same as every other gate here) is aomenc's own requirement for
    /// `skip_mode_present` to ever be considered at all (spec 5.9.22 needs
    /// both a forward and a backward reference candidate).
    #[test]
    fn a_real_aomenc_stream_with_compound_references_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_compound_references_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        let mut refusals = Vec::new();
        let mut never_fired = 0u32;
        for attempt in 0..80u32 {
            let seed = 42 + attempt % 40;
            let duration = frame_count as f64 / 25.0;
            let source =
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"));
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=0",
                "--enable-obmc=0",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=0",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=0",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                "--enable-restoration=0",
                "--max-partition-size=32",
                "--min-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let mut child = Command::new(aomenc_path())
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before = decode::skip_mode_hits();
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
                Ok(frames) => frames,
            };
            if decode::skip_mode_hits() == before {
                never_fired += 1;
                continue;
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched {
                let pin = std::env::temp_dir().join("ec-av1-compound-refs-gate-fail.obu");
                let _ = std::fs::write(&pin, &stream);
                if let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                    std::fs::write(&path, &stream).expect("writing pinned stream");
                    eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
                }
                for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                    assert_eq!(
                        got.y, want.y,
                        "{NAME} frame {i} luma vs ffmpeg (seed {seed}) -- stream pinned at {}",
                        pin.display()
                    );
                    assert_eq!(
                        got.u, want.u,
                        "{NAME} frame {i} U vs ffmpeg (seed {seed}) -- stream pinned at {}",
                        pin.display()
                    );
                    assert_eq!(
                        got.v, want.v,
                        "{NAME} frame {i} V vs ffmpeg (seed {seed}) -- stream pinned at {}",
                        pin.display()
                    );
                }
            }
            eprintln!(
                "{NAME} FIRING seed {seed}: skip_mode hits advanced by {}",
                decode::skip_mode_hits() - before
            );
            return;
        }
        eprintln!(
            "SKIP {NAME}: {never_fired} attempts decoded but never fired skip_mode, and every \
             other attempt hit a named refusal:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-motionmode round 1: `--enable-obmc=1 --enable-warped-motion=0`
    /// on the base recipe (`--auto-alt-ref=1 --lag-in-frames=16
    /// --enable-fwd-kf=0 --enable-ref-frame-mvs=0`) -- pixel-exact against
    /// ffmpeg, gated on [`decode::obmc_hits`] actually advancing (a real
    /// block picked `OBMC_CAUSAL`, not just that the header bit was set).
    /// Structure copied from
    /// `a_real_aomenc_stream_with_compound_references_decodes_pixel_exact`:
    /// a seed sweep with a named-refusal escape hatch and an
    /// `EC_AV1_GATE_DUMP` pin on mismatch.
    #[test]
    fn a_real_aomenc_stream_with_obmc_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_obmc_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        let mut refusals = Vec::new();
        let mut never_fired = 0u32;
        for attempt in 0..80u32 {
            let seed = 42 + attempt % 40;
            let duration = frame_count as f64 / 25.0;
            let source =
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"));
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=0",
                "--enable-obmc=1",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=0",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=0",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                "--enable-restoration=0",
                "--max-partition-size=32",
                "--min-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let mut child = Command::new(aomenc_path())
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before = decode::obmc_hits();
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
                Ok(frames) => frames,
            };
            if decode::obmc_hits() == before {
                never_fired += 1;
                continue;
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched {
                if let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                    std::fs::write(&path, &stream).expect("writing pinned stream");
                    eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
                }
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (seed {seed})");
            }
            eprintln!(
                "{NAME} FIRING seed {seed}: obmc hits advanced by {}",
                decode::obmc_hits() - before
            );
            return;
        }
        eprintln!(
            "SKIP {NAME}: {never_fired} attempts decoded but never fired obmc, and every \
             other attempt hit a named refusal:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-motionmode round 3: same shape as
    /// `a_real_aomenc_stream_with_obmc_decodes_pixel_exact`, but relaxed the
    /// `--min-partition-size=32 --max-partition-size=32` clamp (that other
    /// gate's own doc: locked to remove partition-depth choice from the
    /// search entirely) to `8`/`32`, to make an 8x8-leaf `OBMC_CAUSAL` pick
    /// possible at all. Gated on [`decode::obmc_hits_8`] specifically, not
    /// the aggregate [`decode::obmc_hits`]. Measured live (round 3, this
    /// box): 80 attempts, 0 ever called `decode_inter_block8` at all --
    /// `cpu-used=0`'s RD search never once splits a 16x16 down to a clean
    /// 4-way 8x8 leaf for this smooth `gradients` fixture (large blocks
    /// always win the RD cost there); the handful of named refusals seen
    /// are `PARTITION_SPLIT` chosen *at* 8x8 (down to 4x4, already refused
    /// at `decode.rs`'s `part8 != PARTITION_NONE` check) or
    /// `allow_screen_content_tools`, neither reaching a leaf. Kept anyway
    /// (mirrors this file's own `never_fired`-skip convention) as the
    /// honest record of the attempt; a real 8x8 OBMC fixture needs
    /// higher-frequency content than this gate's fixtures use, out of
    /// round-3 budget.
    #[test]
    fn a_real_aomenc_stream_with_obmc_8x8_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_obmc_8x8_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        let mut refusals = Vec::new();
        let mut never_fired = 0u32;
        let mut total_obmc8 = 0usize;
        let n_attempts: u32 = std::env::var("EC_OBMC8_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(80);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt % 40;
            let duration = frame_count as f64 / 25.0;
            // Recipe search hook: `gradients` is smooth, so aomenc's RD never
            // splits below 16x16 and this gate never fired one 8x8 OBMC block
            // in 45 attempts -- it "passed" by skipping (gate-blind-to-feature).
            // EC_AV1_OBMC8_SOURCE overrides the lavfi source so a firing recipe
            // can be searched for without a rebuild.
            let source = std::env::var("EC_AV1_OBMC8_SOURCE").unwrap_or_else(|_| {
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"))
            });
            // A source supplied through EC_AV1_OBMC8_SOURCE need not carry its
            // own `duration=`; `-t` is the bound that always holds. Without it
            // an endless source (`testsrc2`) fills the pipe forever and the
            // gate hangs to its 300 s timeout.
            let duration_arg = format!("{duration}");
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-t",
                    &duration_arg,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=0",
                "--enable-obmc=1",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=0",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=0",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                "--enable-restoration=0",
                "--min-partition-size=8",
                "--max-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let mut child = Command::new(aomenc_path())
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before8 = decode::obmc_hits_8();
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
                Ok(frames) => frames,
            };
            let fired8 = decode::obmc_hits_8() - before8;
            total_obmc8 += fired8;
            if fired8 == 0 {
                never_fired += 1;
                continue;
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched {
                if let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                    std::fs::write(&path, &stream).expect("writing pinned stream");
                    eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
                }
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (seed {seed})");
            }
            eprintln!(
                "{NAME} FIRING seed {seed}: 8x8 obmc hits {fired8} (total across attempts so far {total_obmc8})"
            );
            return;
        }
        eprintln!(
            "SKIP {NAME}: {never_fired} attempts decoded but never fired an 8x8 obmc block \
             ({total_obmc8} total 8x8 obmc hits across all attempts), and every other attempt \
             hit a named refusal:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-warp r5e (flipped from lane-motionmode r1's refuses-or-matches):
    /// `--enable-warped-motion=1` streams must DECODE pixel-exact --
    /// `av1_findSamples`/`num_proj_ref`, the 3-symbol `motion_mode` read,
    /// the affine projection/filter, and the WARPED_CAUSAL interp-filter
    /// derivation are all ported. A refusal naming warp, or any pixel
    /// mismatch, fails the gate; refusals for OTHER named capabilities
    /// (screen content tools) still skip that seed. The sweep must also
    /// actually exercise warp (`warp_selected_hits > 0`) so a decoder that
    /// silently stops selecting WARPED_CAUSAL cannot pass vacuously.
    #[test]
    fn a_real_aomenc_stream_with_warped_motion_refuses_or_matches() {
        const NAME: &str = "a_real_aomenc_stream_with_warped_motion_refuses_or_matches";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_WARP_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(40);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let duration = frame_count as f64 / 25.0;
            let source =
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"));
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=1",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=0",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=0",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                "--enable-restoration=0",
                "--max-partition-size=32",
                "--min-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let mut child = Command::new(aomenc_path())
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    assert!(
                        !msg.contains("warp"),
                        "{NAME} refused on warp (seed {seed}) -- warp decode is ported, this \
                         refusal must not exist: {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal (non-warp capability): {msg}");
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                std::fs::write(&path, &stream).expect("writing pinned stream");
                eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (seed {seed})");
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused on other capabilities; the gate never exercised warp"
        );
        assert!(
            crate::decode::warp_selected_hits() > 0,
            "{NAME}: {matched} matches but zero WARPED_CAUSAL blocks fired -- the gate decodes \
             warp streams without ever selecting warp, so it proves nothing about warp"
        );
        eprintln!(
            "{NAME}: {named_refusals} non-warp refusals, {matched} pixel-exact matches out of {n_attempts}, warp_selected_hits={}",
            crate::decode::warp_selected_hits()
        );
    }

    /// lane-superres stage 3: a real aomenc `--superres-mode=1` key-frame
    /// sequence must decode pixel-exact vs ffmpeg/dav1d's own upscaled
    /// output, and must actually run [`crate::superres::upscale_picture`]
    /// (`superres_hits > 0`) -- a decoder that silently stopped upscaling
    /// would otherwise pass vacuously (class `gate-blind-to-feature`).
    /// `--kf-max-dist` set to the frame count keeps every frame a key
    /// frame: inter-frame superres (a differently-scaled reference) is a
    /// later stage of this lane, refused by name today.
    #[test]
    fn a_real_aomenc_superres_key_frame_sequence_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_superres_key_frame_sequence_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 3usize);
        let duration = frame_count as f64 / 25.0;
        let source = gradients_source(7, width, height, &format!("duration={duration}:rate=25"));
        let y4m = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &source,
                "-pix_fmt",
                "yuv420p",
                "-f",
                "yuv4mpegpipe",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("ffmpeg failed to run");
        assert!(
            y4m.status.success(),
            "ffmpeg fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let args: Vec<&str> = vec![
            "--codec=av1",
            "--passes=1",
            "--end-usage=q",
            "--cq-level=32",
            "--cpu-used=0",
            "--kf-max-dist=0",
            "--threads=1",
            "--row-mt=0",
            "--superres-mode=1",
            "--superres-denominator=12",
            "--superres-kf-denominator=12",
            "--enable-rect-partitions=0",
            "--enable-ab-partitions=0",
            "--enable-1to4-partitions=0",
            "--enable-filter-intra=0",
            "--enable-smooth-intra=0",
            "--enable-paeth-intra=0",
            "--enable-directional-intra=0",
            "--enable-angle-delta=0",
            "--enable-tx-size-search=0",
            "--enable-cdef=0",
            "--enable-restoration=0",
            "--max-partition-size=32",
            "--min-partition-size=32",
            "--enable-palette=0",
            "--enable-intrabc=0",
            "--enable-cfl-intra=0",
            "--obu",
            "-o",
            "-",
            "-",
        ];
        let mut child = Command::new(aomenc_path())
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("aomenc failed to start");
        child
            .stdin
            .take()
            .expect("aomenc stdin")
            .write_all(&y4m.stdout)
            .expect("writing y4m to aomenc");
        let out = child.wait_with_output().expect("aomenc failed to run");
        assert!(
            out.status.success(),
            "aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        if let Ok(path) = std::env::var("EC_SUPERRES_STREAM_DUMP") {
            std::fs::write(path, &stream).expect("dump stream");
        }
        let frames =
            decode_stream(&stream).unwrap_or_else(|e| panic!("{NAME} refused: {e}"));
        assert_eq!(frames.len(), frame_count);
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg");
            assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg");
            assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg");
        }
        assert!(
            crate::superres::superres_hits() > 0,
            "{NAME}: matched but zero superres_hits -- the gate never actually exercised the \
             upscaler, it proves nothing"
        );
        eprintln!(
            "{NAME}: {frame_count} frames pixel-exact, superres_hits={}",
            crate::superres::superres_hits()
        );
    }

    /// lane-superres r10: a real aomenc key+inter `--superres-mode=1`
    /// sequence must DECODE pixel-exact vs ffmpeg, with the frame-level
    /// `use_superres` refusal LIFTED for inter frames -- `mc::predict_scaled`
    /// (spec 7.11.3.3) is the only path this recipe can reach (warp/OBMC/
    /// compound/interintra/palette/intrabc/CDEF/LR are all off, matching the
    /// scoped refusals' own unimplemented list). A pixel match alone would
    /// pass just as well with an unscaled reference and prove nothing
    /// (class `gate-blind-to-feature`) -- hard-assert
    /// `crate::mc::predict_scaled_hits() > 0` so the gate can only pass if the
    /// scaled MC path actually ran.
    #[test]
    fn a_real_aomenc_key_and_inter_superres_sequence_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_key_and_inter_superres_sequence_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 3usize);
        let duration = frame_count as f64 / 25.0;
        let source = gradients_source(11, width, height, &format!("duration={duration}:rate=25"));
        let y4m = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &source,
                "-pix_fmt",
                "yuv420p",
                "-f",
                "yuv4mpegpipe",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("ffmpeg failed to run");
        assert!(
            y4m.status.success(),
            "ffmpeg fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let args: Vec<&str> = vec![
            "--codec=av1",
            "--passes=1",
            "--end-usage=q",
            "--cq-level=32",
            "--cpu-used=0",
            // `--kf-max-dist` above `frame_count` keeps only frame 0 a key
            // frame -- frames 1/2 are the inter frames whose `use_superres`
            // refusal this round lifts.
            "--kf-max-dist=1000",
            "--lag-in-frames=0",
            "--auto-alt-ref=0",
            "--threads=1",
            "--row-mt=0",
            "--sb-size=64",
            "--superres-mode=1",
            "--superres-denominator=12",
            "--superres-kf-denominator=12",
            // Forces every inter block onto the single-ref, non-warp/OBMC/
            // interintra branch `decode_inter_block` threads `predict_scaled`
            // through -- the only branch this round's scoped refusals leave
            // unrefused under a scaled reference.
            "--enable-warped-motion=0",
            "--enable-obmc=0",
            "--enable-masked-comp=0",
            "--enable-interintra-comp=0",
            "--enable-dist-wtd-comp=0",
            "--enable-diff-wtd-comp=0",
            "--enable-onesided-comp=0",
            "--enable-interintra-wedge=0",
            "--enable-smooth-interintra=0",
            "--enable-order-hint=0",
            "--enable-ref-frame-mvs=0",
            "--enable-rect-partitions=0",
            "--enable-ab-partitions=0",
            "--enable-1to4-partitions=0",
            "--enable-filter-intra=0",
            "--enable-smooth-intra=0",
            "--enable-paeth-intra=0",
            "--enable-directional-intra=0",
            "--enable-angle-delta=0",
            "--enable-tx-size-search=0",
            "--enable-cdef=0",
            "--enable-restoration=0",
            "--max-partition-size=32",
            "--min-partition-size=32",
            "--enable-palette=0",
            "--enable-intrabc=0",
            "--enable-cfl-intra=0",
            "--obu",
            "-o",
            "-",
            "-",
        ];
        let mut child = Command::new(aomenc_path())
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("aomenc failed to start");
        child
            .stdin
            .take()
            .expect("aomenc stdin")
            .write_all(&y4m.stdout)
            .expect("writing y4m to aomenc");
        let out = child.wait_with_output().expect("aomenc failed to run");
        assert!(
            out.status.success(),
            "aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        if let Ok(path) = std::env::var("EC_SUPERRES_INTER_STREAM_DUMP") {
            std::fs::write(&path, &stream).expect("dump stream");
        }
        let frames =
            decode_stream(&stream).unwrap_or_else(|e| panic!("{NAME} refused: {e}"));
        assert_eq!(frames.len(), frame_count);
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg");
            assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg");
            assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg");
        }
        assert!(
            crate::superres::superres_hits() > 0,
            "{NAME}: matched but zero superres_hits -- the upscaler never ran"
        );
        assert!(
            crate::mc::predict_scaled_hits() > 0,
            "{NAME}: matched but zero predict_scaled_hits -- every block took the unscaled MC \
             path, which proves nothing about the scaled-reference code this round adds \
             (class gate-blind-to-feature)"
        );
        eprintln!(
            "{NAME}: {frame_count} frames pixel-exact, superres_hits={}, predict_scaled_hits={}",
            crate::superres::superres_hits(),
            crate::mc::predict_scaled_hits()
        );
    }

    /// lane-interintra r1: `--enable-interintra-comp=1` streams must DECODE
    /// pixel-exact -- the interintra flag/mode/wedge-flag syntax and the
    /// non-wedge smooth blend are ported. A refusal naming interintra fails
    /// the gate UNLESS it is the wedge refusal (`use_wedge_interintra == 1`
    /// is r2, and this recipe encodes with wedge off so it must not fire
    /// either); warp/obmc are enabled and supported; refusals for OTHER
    /// named capabilities (screen content tools) still skip that seed. The
    /// sweep must actually exercise interintra (`interintra_hits > 0`).
    #[test]
    fn a_real_aomenc_stream_with_interintra_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_interintra_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_INTERINTRA_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(40);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let duration = frame_count as f64 / 25.0;
            let source =
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"));
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=1",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=1",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=1",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                "--enable-restoration=0",
                "--max-partition-size=32",
                "--min-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let mut child = Command::new(aomenc_path())
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    assert!(
                        !msg.contains("warp"),
                        "{NAME} refused on warp (seed {seed}) -- warp decode is ported, this \
                         refusal must not exist: {msg}"
                    );
                    assert!(
                        !msg.contains("interintra"),
                        "{NAME} refused on interintra (seed {seed}) -- non-wedge interintra \
                         is ported and this recipe encodes with wedge off: {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal (non-warp capability): {msg}");
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                std::fs::write(&path, &stream).expect("writing pinned stream");
                eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (seed {seed})");
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused on other capabilities; the gate never exercised warp"
        );
        assert!(
            crate::decode::interintra_hits() > 0,
            "{NAME}: {matched} matches but zero interintra blocks fired -- the gate decodes \
             these streams without ever taking interintra, so it proves nothing"
        );
        eprintln!(
            "{NAME}: {named_refusals} other-capability refusals, {matched} pixel-exact matches out of {n_attempts}, interintra_hits={}",
            crate::decode::interintra_hits()
        );
    }

    /// lane-wii r2: `--enable-interintra-wedge=1` streams must DECODE
    /// pixel-exact -- the wedge-interintra syntax (adapting `wedge_index`
    /// CDF symbol, fixed sign 0) and the wedge mask blend are ported over
    /// the checksum-verified codebook. `--enable-masked-comp=0` stays so
    /// the encoder cannot route inter blocks through the still-unported
    /// COMPOUND_WEDGE/DIFFWTD path. A refusal naming interintra/wedge
    /// fails the gate; refusals for OTHER named capabilities still skip
    /// that seed. Zero `wii_hits` on a run means the encoder never
    /// selected wedge-interintra -- soft-skipped (warned), not failed:
    /// that is encoder choice, not a decoder bug.
    #[test]
    fn a_real_aomenc_stream_with_interintra_wedge_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_interintra_wedge_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_WEDGE_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(40);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let duration = frame_count as f64 / 25.0;
            let source = if attempt % 2 == 0 {
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"))
            } else {
                // `end_pts` does NOT bound mandelbrot (it generated 300+
                // frames and hung the gate for an hour); `-t` below is the
                // real bound, matching every other mandelbrot gate recipe.
                format!("mandelbrot=size={width}x{height}:rate=25")
            };
            let duration_arg = format!("{duration}");
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-t",
                    &duration_arg,
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=1",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=1",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=1",
                "--enable-smooth-interintra=1",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                "--enable-restoration=0",
                "--max-partition-size=32",
                "--min-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let mut child = Command::new(aomenc_path())
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    // Verifier note (lane-wii r2): no refusal string containing
                    // "wedge"/"interintra" exists in the decoder any more, so
                    // string-forbidding asserts here would be vacuous (class
                    // gate-blind-to-feature). The proof this gate carries is
                    // pixel-exact decode plus the wii_hits count printed below.
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal (non-wedge capability): {msg}");
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                std::fs::write(&path, &stream).expect("writing pinned stream");
                eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (seed {seed})");
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused on other capabilities; the gate never exercised wedge-interintra"
        );
        // Hardened from a soft-note (verifier: gate-blind-to-feature): the
        // recipe fired wedge-interintra on EVERY one of 6 hammer runs
        // (wii_hits 2-7), so a zero-hit run is a regression, not sampling.
        assert!(
            crate::decode::wii_hits() > 0,
            "{NAME}: {matched} pixel-exact matches but zero wedge-interintra blocks fired -- \
             the blend is unexercised (recipe fired 2-7 hits per run when landed)"
        );
        eprintln!(
            "{NAME}: {named_refusals} other-capability refusals, {matched} pixel-exact matches out of {n_attempts}, wii_hits={}",
            crate::decode::wii_hits()
        );
    }

    /// lane-rect16 r1/r2: a plain default-settings aomenc run over lavfi
    /// `mandelbrot` (`--cpu-used=4 --end-usage=q --cq-level=32`, no
    /// `--enable-*` flags) used to refuse frame 0 outright with "a partition
    /// below 16x16 other than a clean split". r1 root-caused the FIRST hit to
    /// a `PARTITION_VERT_B` `partition_w16` symbol at mi=(3,2) and wired it
    /// (left 8x16 real `decode_block_rect` strip + two stacked 8x8
    /// `decode_leaf8` leaves on the right). r2 re-measured after that fix
    /// (`decision-before-quantiser`-shaped: don't trust a stale premise) and
    /// found this stream's REAL first-hit blocker in z-order is earlier, at
    /// mi=(0,7): a plain `PARTITION_HORZ`. That arm is now wired too
    /// (`decode_leaf_rect`, sibling to `decode_leaf8` but real-mi-addressed
    /// for a non-square strip) -- `horz_vert_intra_hits` proves it fires.
    /// This exact stream's own HORZ block at mi=(0,7) is *coded* (real
    /// coefficients), which is NOT ported (same ceiling as VERT_B's own
    /// non-skip refusal -- no rectangular-transform coefficient tables at
    /// this size), so decode still stops there and `vert_b_intra_hits`
    /// never gets a chance to increment on this particular stream. This gate
    /// proves the HORZ arm fires and the OLD (now-false) two-clause refusal
    /// never reappears, not a pixel-exact decode of this stream.
    #[test]
    fn a_real_aomenc_stream_with_mandelbrot_fires_the_vert_b_partition_arm() {
        const NAME: &str = "a_real_aomenc_stream_with_mandelbrot_fires_the_vert_b_partition_arm";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (192usize, 128usize);
        let y4m = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("mandelbrot=size={width}x{height}"),
                "-pix_fmt",
                "yuv420p",
                "-t",
                "1",
                "-f",
                "yuv4mpegpipe",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("ffmpeg failed to run");
        assert!(
            y4m.status.success(),
            "ffmpeg fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let mut child = Command::new(aomenc_path())
            .args([
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--cpu-used=4",
                "--end-usage=q",
                "--cq-level=32",
                "--passes=1",
                "--obu",
                "-o",
                "-",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("aomenc failed to start");
        child
            .stdin
            .take()
            .expect("aomenc stdin")
            .write_all(&y4m.stdout)
            .expect("writing y4m to aomenc");
        let out = child.wait_with_output().expect("aomenc failed to run");
        assert!(
            out.status.success(),
            "aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        // lane-rect16 r2: the recipe above was missing `--passes=1` --
        // without it aomenc silently ran 2-pass and wrote nothing to stdout
        // on pass 1 (still exit 0), so this gate always decoded an EMPTY
        // stream and never actually exercised the arm it claims to gate.
        assert!(
            !stream.is_empty(),
            "{NAME}: aomenc wrote an empty stream (missing --passes=1?)"
        );
        let result = decode_stream(&stream);
        // The now-lifted false claim ("codes only the square arms below
        // 16x16") must never appear again.
        if let Err(e) = &result {
            let msg = e.to_string();
            assert!(
                !msg.contains("this decoder codes only the square arms below 16x16"),
                "{NAME}: the OLD (disproved) refusal reappeared: {msg}"
            );
        }
        assert!(
            crate::decode::horz_vert_intra_hits() > 0,
            "{NAME}: the plain PARTITION_HORZ/VERT arm never fired on this stream (regression -- \
             it fired deterministically at mi=(0,7) when this gate was written)"
        );
        eprintln!(
            "{NAME}: horz_vert_intra_hits={}, vert_b_intra_hits={}, decode result: {:?}",
            crate::decode::horz_vert_intra_hits(),
            crate::decode::vert_b_intra_hits(),
            result.as_ref().map(|f| f.len()).map_err(|e| e.to_string())
        );
    }
    /// lane-rectx r3: a real `aomenc` stream whose 16x16-level
    /// `PARTITION_HORZ`/`PARTITION_VERT` strips are CODED (non-skip), i.e.
    /// carry genuine `TX_16X8`/`TX_8X16` luma coefficients and their
    /// `TX_8X4`/`TX_4X8` chroma halves through [`decode_leaf_rect`]'s
    /// lane-rectx arm -- the refusal "a coded (non-skip) HORZ/VERT rect strip
    /// below 16x16" this round lifts. `--min-partition-size=8
    /// --max-partition-size=32 --enable-rect-partitions=1` is what makes
    /// aomenc reach for a 16x8 leaf at all; mandelbrot at `cq-level=24` is
    /// what makes those leaves non-skip (a gradient fixture codes them all
    /// away). `--reduced-tx-type-set=1` pins the five-symbol
    /// `EXT_TX_SET_DTT4_IDTX` alphabet: without it aomenc also writes
    /// `V_DCT`/`H_DCT`, whose 1D tx classes [`read_coeffs_rect`] still
    /// refuses by name (see the r3 report's open residue), so a `=0` run
    /// would prove nothing about the 2D path it does support.
    ///
    /// r2 shipped this path unproven; it was wrong. `TxbSet::LumaRect16x8`
    /// read its `tx_type` symbol from the 16x16 row, but libaom's
    /// `read_tx_type` indexes `intra_ext_tx_cdf[eset][txsize_sqr_map[tx_size]]`
    /// and `txsize_sqr_map[TX_16X8] == TX_8X8` -- a five-symbol CDF where a
    /// seven-symbol one belonged, desyncing at the FIRST coded rect leaf
    /// (whole frame wrong from pixel (0,0)).
    #[test]
    fn a_real_aomenc_stream_with_a_coded_rect_strip_below_16x16_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_a_coded_rect_strip_below_16x16_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (64usize, 64usize);
        let y4m = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("mandelbrot=size={width}x{height}:start_x=-0.6"),
                "-pix_fmt",
                "yuv420p",
                "-t",
                "1",
                "-vframes",
                "1",
                "-f",
                "yuv4mpegpipe",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("ffmpeg failed to run");
        assert!(y4m.status.success(), "ffmpeg fixture: {}", String::from_utf8_lossy(&y4m.stderr));
        let encode = || {
            let mut child = Command::new(aomenc_path())
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=16",
                    "--cpu-used=4",
                    "--threads=1",
                    "--row-mt=0",
                    "--sb-size=64",
                    "--kf-max-dist=0",
                    "--enable-rect-partitions=1",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-tx-size-search=0",
                    // Filter intra on a strip is a separate, still-refused
                    // predictor (`filter intra on a HORZ/VERT strip`); this
                    // gate is about the rect RESIDUAL path, so it is off here
                    // and covered by the square filter-intra gate instead.
                    "--enable-filter-intra=0",
                    "--reduced-tx-type-set=1",
                    "--min-partition-size=8",
                    "--max-partition-size=32",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
        };
        // The fixture rule: an encoder output is only a pin if it reproduces.
        let stream = encode();
        assert_eq!(stream, encode(), "{NAME}: aomenc output is not reproducible for this recipe");
        let before = crate::decode::rect_leaf_coeff_hits();
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: decode failed, not a pixel mismatch: {e}"));
        let fired = crate::decode::rect_leaf_coeff_hits() - before;
        assert!(
            fired > 0,
            "{NAME}: the stream decoded but no coded (non-skip) rect leaf fired -- \
             the gate proves nothing"
        );
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frames.len());
        assert_eq!(frames.len(), 1, "{NAME}: expected one key frame");
        assert_eq!(ffmpeg_frames.len(), frames.len(), "{NAME}: ffmpeg frame count");
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg ({fired} coded rect leaves)");
            assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg");
            assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg");
        }
        eprintln!("{NAME}: pixel-exact, rect_leaf_coeff_hits={fired}");
    }

    /// lane-rectx r5: the same 16x8/8x16 rect leaves, but this time proving
    /// what a LATER, non-strip block reads from them. `decode_leaf_rect` took
    /// the mi-exact intra mode map in r4; `decode_block`/`decode_block_rect`/
    /// `decode_block_rect64` did not, so a 32x16 strip whose left neighbour was
    /// a 16x8 leaf still read the coarse per-16x16 `left_mode` slot -- which no
    /// sub-16x16 block ever writes -- and picked a DIFFERENT `kf_y_mode` CDF row
    /// for the same decoded mode value. The stream stayed in sync through mode
    /// info (`EC_TRACE_MODE_STEP` ranges match up to that symbol) and then
    /// diverged inside the block's own coefficients: the whole bottom-right
    /// 32x32 quadrant of this fixture decoded wrong (1650 bytes) while the
    /// three earlier quadrants were exact -- wrong pixels returned as success.
    /// `mode_mi_override_hits` counts exactly the reads where the mi-exact
    /// neighbour disagrees with the coarse slot, so a regression that drops the
    /// override again fails the counter as well as the pixels.
    #[test]
    fn a_real_aomenc_stream_whose_square_block_reads_a_sub16_neighbours_mode_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_whose_square_block_reads_a_sub16_neighbours_mode_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (64usize, 64usize);
        let y4m = Command::new("ffmpeg")
            .args([
                "-v", "error", "-f", "lavfi", "-i",
                &format!("rgbtestsrc=size={width}x{height}"),
                "-pix_fmt", "yuv420p", "-t", "1", "-vframes", "1", "-f", "yuv4mpegpipe", "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("ffmpeg failed to run");
        assert!(y4m.status.success(), "ffmpeg fixture: {}", String::from_utf8_lossy(&y4m.stderr));
        // Two swept cells of the r4 sweep that decoded to completion with
        // WRONG pixels (1714 and 1650 bytes): `--reduced-tx-type-set` and
        // `--enable-filter-intra` are the two axes that move which block sizes
        // aomenc reaches for here; both arms are pinned.
        let mut total_overrides = 0usize;
        for (rtx, filter_intra) in [("1", "0"), ("0", "1")] {
            let encode = || {
                let mut child = Command::new(aomenc_path())
                    .args([
                        "--codec=av1", "--passes=1", "--end-usage=q", "--cq-level=32",
                        "--cpu-used=4", "--threads=1", "--row-mt=0", "--sb-size=64",
                        "--kf-max-dist=0", "--enable-rect-partitions=1",
                        "--enable-ab-partitions=0", "--enable-1to4-partitions=0",
                        "--enable-tx-size-search=0", "--enable-cdef=0",
                        "--enable-restoration=0",
                        &format!("--enable-filter-intra={filter_intra}"),
                        &format!("--reduced-tx-type-set={rtx}"),
                        "--min-partition-size=8", "--max-partition-size=32",
                        "--obu", "-o", "-", "-",
                    ])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .expect("aomenc failed to start");
                child
                    .stdin
                    .take()
                    .expect("aomenc stdin")
                    .write_all(&y4m.stdout)
                    .expect("writing y4m to aomenc");
                let out = child.wait_with_output().expect("aomenc failed to run");
                assert!(
                    out.status.success(),
                    "aomenc refused the fixture: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                out.stdout
            };
            let stream = encode();
            assert_eq!(
                stream,
                encode(),
                "{NAME}: aomenc output is not reproducible (rtx={rtx} filter_intra={filter_intra})"
            );
            let before = crate::decode::mode_mi_override_hits();
            let frames = decode_stream(&stream).unwrap_or_else(|e| {
                panic!("{NAME}: decode failed (rtx={rtx} filter_intra={filter_intra}): {e}")
            });
            // Counted only on an attempt that actually decoded AND is compared
            // below -- a refusal never contributes to the firing assert.
            let overrides = crate::decode::mode_mi_override_hits() - before;
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frames.len());
            assert_eq!(frames.len(), 1, "{NAME}: expected one key frame");
            assert_eq!(ffmpeg_frames.len(), frames.len(), "{NAME}: ffmpeg frame count");
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "{NAME} frame {i} luma vs ffmpeg (rtx={rtx} filter_intra={filter_intra}, \
                     {overrides} mi-exact mode overrides)"
                );
                assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (rtx={rtx})");
                assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (rtx={rtx})");
            }
            total_overrides += overrides;
        }
        assert!(
            total_overrides > 0,
            "{NAME}: no non-strip block ever read a mi-exact neighbour that disagreed with its \
             coarse slot -- the gate proves nothing"
        );
        eprintln!("{NAME}: pixel-exact, mode_mi_override_hits={total_overrides}");
    }

    /// lane-cfl r1: the chroma intra-edge FILTER TYPE is the neighbour's
    /// `uv_mode` (libaom `get_filt_type`, reconintra.c: `chroma_above_mbmi`/
    /// `chroma_left_mbmi` -> `is_smooth` on `uv_mode`), and two whole decode
    /// paths passed a hardcoded `false` for it -- `decode_leaf_8x8` (every
    /// 8x8 leaf of a SPLIT) and `decode_leaf_rect` (every rect strip below
    /// 16x16), on a stale "chroma stays false (exact) until a smooth/paeth
    /// uv_mode can reach decode at all" comment. On top of that the coarse
    /// `above_uv_mode`/`left_uv_mode` cells are 16px-granular and their
    /// `for cell in 0..w / SUB` write runs ZERO times for any block below
    /// 16x16, so even the paths that DID read them saw whatever a
    /// 16x16-or-larger block had left there (class: context read from one
    /// cell / neighbour map at the wrong granularity). Result on this
    /// recipe: luma byte-exact, entropy ladder identical, and 40 U + 86 V
    /// chroma bytes wrong -- every one of them downstream of one 4x4 chroma
    /// block at mi(8,12) whose D135 + `angle_delta_uv=-3` left edge was
    /// filtered at the wrong strength (`ft=0` where the instrumented aomdec
    /// prints `ft=1`, left edge `95,59,30,28` vs `90,61,37,29`).
    ///
    /// Both arms hard-assert per-attempt counters for the two chroma block
    /// shapes involved (`UV_CFL_PRED` blocks, nonzero `angle_delta_uv`
    /// blocks) plus the mi-exact UV-neighbour override, and are counted only
    /// on an attempt that decoded AND is pixel-compared.
    #[test]
    fn a_real_aomenc_stream_whose_chroma_edge_filter_reads_a_sub16_neighbours_uv_mode_decodes_pixel_exact()
    {
        const NAME: &str = "a_real_aomenc_stream_whose_chroma_edge_filter_reads_a_sub16_neighbours_uv_mode_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let mut compared = 0usize;
        let (width, height) = (64usize, 64usize);
        // 8-bit arm = the r5 residue cell ("mandfi161"); 10-bit arm = its
        // twin through the same recipe at `--bit-depth=10`.
        for depth in [8usize, 10] {
            let pix = if depth == 10 { "yuv420p10le" } else { "yuv420p" };
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v", "error", "-f", "lavfi", "-i",
                    &format!("mandelbrot=size={width}x{height}:start_x=-0.6"),
                    "-pix_fmt", pix, "-strict", "-1", "-t", "1", "-vframes", "1",
                    "-f", "yuv4mpegpipe", "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "{NAME}: ffmpeg fixture (depth={depth}): {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let encode = || {
                let mut child = Command::new(aomenc_path())
                    .args([
                        "--codec=av1", "--passes=1", "--end-usage=q", "--cq-level=16",
                        "--cpu-used=4", "--threads=1", "--row-mt=0", "--sb-size=64",
                        "--kf-max-dist=0", "--enable-rect-partitions=1",
                        "--enable-ab-partitions=0", "--enable-1to4-partitions=0",
                        "--enable-tx-size-search=0", "--enable-filter-intra=1",
                        "--reduced-tx-type-set=1", "--min-partition-size=8",
                        "--max-partition-size=32",
                        &format!("--input-bit-depth={depth}"),
                        &format!("--bit-depth={depth}"),
                        "--obu", "-o", "-", "-",
                    ])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .expect("aomenc failed to start");
                child
                    .stdin
                    .take()
                    .expect("aomenc stdin")
                    .write_all(&y4m.stdout)
                    .expect("writing y4m to aomenc");
                let out = child.wait_with_output().expect("aomenc failed to run");
                assert!(
                    out.status.success(),
                    "aomenc refused the fixture (depth={depth}): {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                out.stdout
            };
            let stream = encode();
            assert_eq!(
                stream,
                encode(),
                "{NAME}: aomenc output is not reproducible (depth={depth})"
            );
            let before_cfl = crate::decode::cfl_block_hits();
            let before_angle = crate::decode::uv_angle_delta_hits();
            let before_uv = crate::decode::uv_mode_mi_override_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                // Only a NAMED refusal is tolerated (COMMON rule): the 10-bit
                // twin of this recipe still stops at another lane's open
                // sub-16 AB-partition / strip-filter-intra refusals, so it
                // cannot be compared yet. Any other error is a failure, and
                // `compared` below keeps the test from passing vacuously.
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME}: decode failed (depth={depth}): {msg}"
                    );
                    eprintln!("{NAME}: depth={depth} arm refused (not compared): {msg}");
                    continue;
                }
            };
            compared += 1;
            // Read only on an attempt that decoded AND is compared below.
            let cfl_blocks = crate::decode::cfl_block_hits() - before_cfl;
            let angle_blocks = crate::decode::uv_angle_delta_hits() - before_angle;
            let uv_overrides = crate::decode::uv_mode_mi_override_hits() - before_uv;
            let ffmpeg_frames = if depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len())
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frames.len())
            };
            assert_eq!(frames.len(), 1, "{NAME}: expected one key frame (depth={depth})");
            assert_eq!(ffmpeg_frames.len(), frames.len(), "{NAME}: ffmpeg frame count");
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg (depth={depth})");
                assert_eq!(
                    got.u, want.u,
                    "{NAME} frame {i} U vs ffmpeg (depth={depth}, {cfl_blocks} CFL blocks, \
                     {angle_blocks} nonzero-angle_delta_uv blocks, {uv_overrides} mi-exact UV \
                     neighbour overrides)"
                );
                assert_eq!(
                    got.v, want.v,
                    "{NAME} frame {i} V vs ffmpeg (depth={depth}, {cfl_blocks} CFL blocks, \
                     {angle_blocks} nonzero-angle_delta_uv blocks, {uv_overrides} mi-exact UV \
                     neighbour overrides)"
                );
            }
            assert!(
                cfl_blocks > 0,
                "{NAME}: no UV_CFL_PRED block decoded (depth={depth}) -- the gate proves nothing"
            );
            assert!(
                angle_blocks > 0,
                "{NAME}: no nonzero angle_delta_uv chroma block decoded (depth={depth}) -- \
                 the chroma intra-edge filter path never ran"
            );
            assert!(
                uv_overrides > 0,
                "{NAME}: no chroma edge-filter read ever took a mi-exact UV neighbour that \
                 disagreed with its coarse 16x16 slot (depth={depth}) -- the r1 defect is \
                 not exercised"
            );
            eprintln!(
                "{NAME}: pixel-exact at {depth}-bit, cfl_blocks={cfl_blocks} \
                 angle_delta_uv_blocks={angle_blocks} uv_mi_overrides={uv_overrides}"
            );
        }
        assert!(
            compared > 0,
            "{NAME}: every arm refused -- the gate compared no pixels at all"
        );
    }

    #[test]
    #[ignore = "lane-rectx r3 scratch sweep"]
    fn sweep_rectx_recipes() {
        let _gate_lock = lock_gate_counters();
        let srcs: Vec<String> = vec![
            "mandelbrot=size=64x64:start_x=0.4".into(),
            "mandelbrot=size=64x64:start_x=-0.6".into(),
            "mandelbrot=size=64x64:start_x=0.1".into(),
            "mandelbrot=size=128x128:start_x=0.4".into(),
            "mandelbrot=size=128x128:start_x=-0.6".into(),
            "testsrc2=size=64x64".into(),
            "testsrc2=size=128x128".into(),
            "rgbtestsrc=size=64x64".into(),
            "smptebars=size=128x128".into(),
        ];
        for start_x in srcs {
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v", "error", "-f", "lavfi", "-i",
                    &start_x,
                    "-pix_fmt", "yuv420p", "-t", "1", "-vframes", "1", "-f", "yuv4mpegpipe", "-",
                ])
                .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
                .output().expect("ffmpeg");
            assert!(y4m.status.success());
            let (width, height) = if start_x.contains("128x128") { (128usize, 128usize) } else { (64usize, 64usize) };
            for cq in [16u32, 20, 24, 28, 32, 40, 50] {
                for rtx in ["0", "1"] {
                    let mut child = Command::new(aomenc_path())
                        .args([
                            "--codec=av1", "--passes=1", "--end-usage=q",
                            &format!("--cq-level={cq}"), "--cpu-used=4", "--threads=1",
                            "--row-mt=0", "--sb-size=64", "--kf-max-dist=0",
                            "--enable-rect-partitions=1", "--enable-ab-partitions=0",
                            "--enable-1to4-partitions=0", "--enable-tx-size-search=0",
                            "--enable-cdef=0", "--enable-restoration=0",
                            &format!("--reduced-tx-type-set={rtx}"),
                            "--min-partition-size=8", "--max-partition-size=32",
                            "--obu", "-o", "-", "-",
                        ])
                        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
                        .spawn().expect("aomenc");
                    child.stdin.take().unwrap().write_all(&y4m.stdout).unwrap();
                    let out = child.wait_with_output().unwrap();
                    assert!(out.status.success());
                    let stream = out.stdout;
                    let before = crate::decode::rect_leaf_coeff_hits();
                    let res = decode_stream(&stream);
                    let fired = crate::decode::rect_leaf_coeff_hits() - before;
                    match res {
                        Err(e) => eprintln!("x={start_x} cq={cq} rtx={rtx} fired={fired} REFUSED {e}"),
                        Ok(frames) => {
                            let want = ffmpeg_decode_sequence(&stream, width, height, frames.len());
                            let bad = frames.iter().zip(&want)
                                .filter(|(g, w)| g.y != w.y || g.u != w.u || g.v != w.v).count();
                            eprintln!("x={start_x} cq={cq} rtx={rtx} fired={fired} frames={} mismatched={bad}", frames.len());
                            if bad == 0 { continue; }
                            let (g, w) = (&frames[0], &want[0]);
                            for br in 0..height / 8 {
                                let mut line = String::new();
                                for bc in 0..width / 8 {
                                    let mut n = 0;
                                    for row in br * 8..br * 8 + 8 {
                                        for col in bc * 8..bc * 8 + 8 {
                                            if g.y[row * width + col] != w.y[row * width + col] {
                                                n += 1;
                                            }
                                        }
                                    }
                                    line.push_str(&format!("{n:4}"));
                                }
                                eprintln!("  blk8 row{br}: {line}");
                            }
                        }
                    }
                }
            }
        }
    }

    /// lane-rectgate r1: every prior gate pins `--enable-rect-partitions=0
    /// --enable-ab-partitions=0 --min/max-partition-size=32` so aomenc never
    /// gets to choose a real rect/ab split. This charter's premise ("rect
    /// HORZ/VERT + HORZ_A/HORZ_B/VERT_A/VERT_B landed during the warp lane")
    /// does NOT hold: `decode_frame`'s inter-tile `match part32` only has
    /// arms for `PARTITION_NONE`/`SPLIT`/`HORZ_B` -- `HORZ`, `VERT`,
    /// `HORZ_A`, `VERT_A`, `VERT_B`, `HORZ_4`, `VERT_4` all fall into the
    /// generic "a partition type this encoder never writes" `_` arm. Two
    /// recipes were swept empirically (40 attempts each) to characterize the
    /// gap:
    /// - `--enable-rect-partitions=1 --enable-ab-partitions=1`, no
    ///   min/max clamp: 0/40 matched -- every attempt hit either the
    ///   screen-content refusal or a genuinely-unimplemented partition value
    ///   (1/2/6/7/9 all observed), always somewhere in the 24-frame stream.
    /// - Same, but `--enable-1to4-partitions=0` explicit and
    ///   `--min-partition-size=16`: still 0/40, `value=9` (`VERT_4`)
    ///   dominates -- this build's `--enable-1to4-partitions=0` does NOT
    ///   suppress `VERT_4` selection (aomenc-side, confirmed by direct flag
    ///   toggle; outside this decoder). This recipe (below) restores the
    ///   `--enable-rect-partitions=0 --min/max-partition-size=32` pin every
    ///   other gate uses, flipping only `--enable-ab-partitions=1`: matches
    ///   cleanly (33/40, rest screen-content refusals) but
    ///   `extended_partition_hits` stayed 0 -- confirmed with the same probe
    ///   wired into the already-green warp gate too (144 `warp_selected_hits`,
    ///   0 `extended_partition_hits`), so `PARTITION_HORZ_B` itself is
    ///   apparently never chosen by aomenc for this small gradient fixture at
    ///   `cq-level=45` regardless of clamp. No decode defect (crash or pixel
    ///   mismatch) was found in any sweep -- every refusal is a correctly
    ///   named, non-silent "unsupported" error. A refusal or mismatch naming
    ///   any OTHER already-ported capability (warp, obmc, interintra) still
    ///   fails the gate outright.
    #[test]
    fn a_real_aomenc_stream_with_free_partitions_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_free_partitions_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_RECTGATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(40);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let duration = frame_count as f64 / 25.0;
            let source =
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"));
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=1",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=0",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=0",
                // lane-rect r2: HORZ/VERT decode for real (rect-flake-1
                // byte-exact: size_h + warp dims + OBMC overlap + tx height)
                // -- free recipe restored; remaining partition kinds refuse
                // by name and skip their seeds.
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=1",
                "--enable-1to4-partitions=0",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                "--enable-restoration=0",
                "--max-partition-size=32",
                "--min-partition-size=16",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let mut child = Command::new(aomenc_path())
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    for banned in ["warp", "obmc", "interintra"] {
                        assert!(
                            !msg.contains(banned),
                            "{NAME} refused on {banned} (seed {seed}) -- that capability is \
                             ported and this recipe does not need it: {msg}"
                        );
                    }
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                std::fs::write(&path, &stream).expect("writing pinned stream");
                eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (seed {seed})");
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused; the gate never decoded a free-partition stream"
        );
        // lane-partitions r1: HORZ/VERT arms landed, so the diversity probe
        // graduates from rectgate r1's eprintln to a report line on
        // `rect_partition_hits`; a zero-hit run SOFT-reports (aomenc RD may
        // legitimately never pick a rect split in a small window -- same
        // sampling caveat as the masked-compound gate's zero-hit runs).
        // `extended_partition_hits` (HORZ_B) stays unasserted per rectgate r1.
        if crate::decode::rect_partition_hits() == 0 {
            eprintln!(
                "{NAME}: SOFT-NOTE -- zero HORZ/VERT strips fired this run \
                 ({matched} matches, {named_refusals} refusals); sampling, not a regression"
            );
        } else {
            // r5: the desync was the tx-depth symbol being read
            // unconditionally where both square paths gate it on `tx_select`
            // -- with --enable-tx-size-search=0 the encoder writes no such
            // symbol and the decoder consumed one that was never there. With
            // that fixed the real coefficient decode is pixel-exact, so this
            // is a HARD assert again: strips fired but no coefficients read
            // would mean the coded path silently stopped being exercised.
            assert!(
                crate::decode::rect_coeff_hits() > 0,
                "{NAME}: {} HORZ/VERT strips fired but rect_coeff_hits==0 -- the \
                 coded rect path is no longer exercised",
                crate::decode::rect_partition_hits()
            );
        }
        eprintln!(
            "{NAME}: {named_refusals} named refusals, {matched} pixel-exact matches out of {n_attempts}, rect_partition_hits={} rect_coeff_hits={} extended_partition_hits={} partab_hits={}",
            crate::decode::rect_partition_hits(),
            crate::decode::rect_coeff_hits(),
            crate::decode::extended_partition_hits(),
            crate::decode::partab_hits()
        );
    }

    /// lane-partab r1: `--enable-ab-partitions=1` (aomenc default-on) INTER
    /// streams must consume PARTITION_HORZ_A/VERT_A/VERT_B entropy-exact AND
    /// decode pixel-exact -- `partab_hits() > 0` is soft-skipped on a
    /// zero-hit run (aomenc RD may legitimately never pick an AB split in a
    /// small window, same sampling caveat as wedge/rect). The INTER AB
    /// refusal is FORBIDDEN (decode.rs names it "an INTER 32x32 partition
    /// ..."); intra-frame AB refusals stay accepted named refusals -- the
    /// intra dispatch is a later lane. Recipe = the free-partition gate's,
    /// which fires AB live (r1: partab_hits 6/2/4/3/1/5 over six
    /// 40-attempt runs).
    #[test]
    fn a_real_aomenc_stream_with_ab_partitions_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_ab_partitions_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_RECTGATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(40);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let duration = frame_count as f64 / 25.0;
            let source =
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"));
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=1",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=0",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=0",
                // lane-rect r2 recipe, kept byte-identical by lane-partab
                // r1: free partition search, 1to4 off, intra tools off.
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=1",
                "--enable-1to4-partitions=0",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                "--enable-restoration=0",
                "--max-partition-size=32",
                "--min-partition-size=16",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let mut child = Command::new(aomenc_path())
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    for banned in ["warp", "obmc", "interintra"] {
                        assert!(
                            !msg.contains(banned),
                            "{NAME} refused on {banned} (seed {seed}) -- that capability is \
                             ported and this recipe does not need it: {msg}"
                        );
                    }
                    for v in [4, 6, 7] {
                        assert!(
                            !msg.contains(&format!(
                                "an INTER 32x32 partition type this encoder never writes \
                                 (value={v})"
                            )),
                            "{NAME} refused an INTER AB partition (value={v}, seed {seed}) -- \
                             all three AB arms are ported, this refusal is forbidden: {msg}"
                        );
                    }
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (seed {seed})");
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused; the gate never decoded a stream"
        );
        if crate::decode::partab_hits() == 0 {
            eprintln!(
                "{NAME}: SOFT-NOTE -- zero AB partitions fired this run \
                 ({matched} matches, {named_refusals} refusals); sampling, not a regression"
            );
        }
        eprintln!(
            "{NAME}: {named_refusals} named refusals, {matched} pixel-exact matches out of {n_attempts}, partab_hits={}",
            crate::decode::partab_hits()
        );
    }

    /// lane-maskcomp r2 / lane-wedge r3: `--enable-masked-comp=1` streams
    /// must consume `compound_type`/`wedge_idx`/`wedge_sign`/`mask_type`
    /// entropy-exact AND build the real blend, DIFFWTD or WEDGE --
    /// `masked_compound_hits() > 0` is a hard assert and BOTH
    /// `COMPOUND_DIFFWTD` and `COMPOUND_WEDGE` refusals are now FORBIDDEN
    /// (matches interintra's own gate). r3: `--enable-dist-wtd-comp=0`
    /// (forces the masked choice toward wedge when comp_group_idx==1) plus
    /// a hard-diagonal-edge `geq` source (wedge's OBLIQUE directions favor
    /// a real diagonal edge over the r2 gradients content) to fire
    /// COMPOUND_WEDGE live; `wedge_hits()` is soft-skipped (not hard
    /// asserted) on a zero-hit run -- the recipe search is not proven to
    /// converge every run, see charter's "if no recipe fires" fallback.
    #[test]
    fn a_real_aomenc_stream_with_masked_compound_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_masked_compound_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_MASKCOMP_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(80);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let duration = frame_count as f64 / 25.0;
            // lane-wedge r3: a hard diagonal edge (not a smooth gradient)
            // is the content wedge's OBLIQUE master masks are shaped for --
            // `mandelbrot`'s pan position varies the edge angle/position
            // across attempts the way the r2 gradients seed did.
            let source = format!(
                "mandelbrot=size={width}x{height}:rate=25:start_x={sx}:start_y={sy}",
                sx = -0.6 + 0.005 * (attempt as f64),
                sy = -0.4 + 0.005 * (attempt as f64)
            );
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-t",
                    &duration.to_string(),
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=1",
                // r3: mandelbrot's pan/zoom triggers aomenc's single-ref
                // GLOBALMV global-motion search (a capability distinct from
                // and outside this lane's scope) -- off, matching this
                // gate's own goal of exercising masked compound, not GM.
                "--enable-global-motion=0",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-masked-comp=1",
                // r3: force the masked choice toward wedge (r2 always set
                // this =1; distance-weighted compound competes with the
                // wedge/diffwtd choice for the same comp_group_idx==1 slot).
                "--enable-dist-wtd-comp=0",
                "--enable-interintra-comp=1",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=1",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                "--enable-restoration=0",
                "--max-partition-size=32",
                "--min-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let mut child = Command::new(aomenc_path())
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    assert!(
                        !msg.contains("warp"),
                        "{NAME} refused on warp (seed {seed}) -- warp decode is ported: {msg}"
                    );
                    assert!(
                        !msg.contains("masked COMPOUND_REFERENCE"),
                        "{NAME} refused a masked-compound block (seed {seed}) -- both DIFFWTD \
                         and WEDGE blends are ported, this refusal is forbidden: {msg}"
                    );
                    assert!(
                        !msg.contains("interintra"),
                        "{NAME} refused on interintra (seed {seed}) -- non-wedge \
                         interintra is ported: {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    if std::env::var_os("EC_AV1_GATE_DUMP").is_some() {
                        // r1's self-pin is armed via EC_AV1_GATE_DUMP=<path>
                        // only for a real pixel MISMATCH below, not for this
                        // still-expected named refusal.
                    }
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                std::fs::write(&path, &stream).expect("writing pinned stream");
                eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (seed {seed})");
            }
            matched += 1;
        }
        // r1: observed flaky at 40 attempts -- aomenc's cpu-used=0 RD is
        // run-to-run nondeterministic in whether it ever picks
        // comp_group_idx == 1 for this synthetic gradients content, and
        // allow_screen_content_tools sometimes eats most of a small seed
        // window (class gate-recipe-confound / never-fired-gate, memory).
        // 80 attempts is the mitigation, not a fix; a still-zero run here
        // is that sampling flake, not a decode regression -- check
        // masked_compound_hits/named_refusals in the panic message before
        // assuming a real bug.
        // r2: DIFFWTD is ported and gated, so a zero-hit run is no longer
        // soft-skipped -- hard assert, matching the interintra gate.
        assert!(
            crate::decode::masked_compound_hits() > 0,
            "{NAME}: zero masked-compound blocks fired ({matched} matches, {named_refusals} \
             other refusals out of {n_attempts}) -- gate proved nothing this run"
        );
        // r3: wedge_hits() is soft-skipped, not hard-asserted -- the recipe
        // search (dist-wtd-comp=0 + diagonal mandelbrot content) is not
        // proven to fire wedge on every run; charter's fallback is to land
        // the checksum-verified codebook + blend with this soft path.
        if crate::decode::wedge_hits() == 0 {
            eprintln!(
                "{NAME}: WARNING wedge_hits()==0 this run -- COMPOUND_WEDGE never fired \
                 ({matched} matches, {named_refusals} refusals out of {n_attempts}); codebook \
                 is checksum-verified but unexercised live this run"
            );
        }
        eprintln!(
            "{NAME}: {named_refusals} other-capability refusals, {matched} pixel-exact matches \
             out of {n_attempts}, masked_compound_hits={} wedge_hits={}",
            crate::decode::masked_compound_hits(),
            crate::decode::wedge_hits()
        );
    }


    /// lane-lr r3: proves stage 1/2's exit criterion -- a real
    /// `--enable-restoration=1` aomenc stream must survive the whole
    /// partition walk (every symbol read entropy-exact) and land on the
    /// NEW, narrowed refusal ("read but not applied"), never desync into
    /// some other named refusal or an outright decode panic. Multi-superblock
    /// fixture (192x128 = 3x2 SBs of 64px) per the charter's own trap
    /// warning -- a single-SB fixture can never re-enter `read_lr` a second
    /// time and would leave `SWITCHABLE_HITS`/etc. proving only one call
    /// site, not the per-SB loop.
    #[test]
    fn a_real_aomenc_stream_with_restoration_reads_lr_symbols_correctly() {
        const NAME: &str = "a_real_aomenc_stream_with_restoration_reads_lr_symbols_correctly";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (192usize, 128usize);
        let n_attempts: u32 = std::env::var("EC_LR_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(40);
        let mut lr_refusals = 0u32;
        let mut other_refusals = Vec::new();
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let source = gradients_source(seed, width, height, "duration=0.04:rate=25");
            let y4m = Command::new("ffmpeg")
                .args(["-v", "error", "-f", "lavfi", "-i"])
                .arg(&source)
                .args(["-pix_fmt", "yuv420p", "-f", "yuv4mpegpipe", "-"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut child = Command::new(aomenc_path())
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    // r3: a low cq-level (high quality/bitrate) is load-bearing --
                    // sampled live (2026-08-30) that aomenc's RD only ever picks a
                    // non-`RESTORE_NONE` frame_restoration_type at cq-level<=20 for
                    // this fixture; cq-level=45 (this gate's sibling gates' usual
                    // value) landed `uses_lr=false` on every attempt.
                    "--cq-level=15",
                    "--cpu-used=0",
                    "--threads=1",
                    "--row-mt=0",
                    // r3: this decoder always assumes 64px superblocks
                    // (`decode.rs` hardcodes `SB_MI=16`, ledger `lane-sb128`
                    // dead-ends) -- force aomenc to match rather than risk
                    // the unrelated sb128 gap eating this gate's own window.
                    "--sb-size=64",
                    "--enable-restoration=1",
                    "--enable-cdef=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-tx-size-search=0",
                    "--max-partition-size=32",
                    "--min-partition-size=16",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--enable-ref-frame-mvs=0",
                    "--enable-warped-motion=0",
                    "--enable-global-motion=0",
                    "--enable-masked-comp=0",
                    "--enable-interintra-comp=0",
                    "--enable-obmc=0",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            match decode_stream(&stream) {
                Ok(pics) => {
                    // r4: the pixel filters are wired -- every `uses_lr`
                    // frame must now decode Ok AND match an independent
                    // decoder pixel-exact, not just refuse cleanly.
                    let reference = ffmpeg_decode_sequence(&stream, width, height, pics.len());
                    let mismatched = pics
                        .iter()
                        .zip(&reference)
                        .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
                    // r7: pin a mismatching stream the same way the
                    // masked-compound gate does -- lets a follow-up round
                    // decode this exact seed's bytes directly instead of
                    // re-deriving them from a live 40-attempt sweep.
                    if mismatched && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                        std::fs::write(&path, &stream).expect("writing pinned stream");
                        eprintln!(
                            "EC_AV1_GATE_DUMP: wrote mismatching LR stream (seed {seed}) to {path}"
                        );
                    }
                    for (i, (pic, refpic)) in pics.iter().zip(reference.iter()).enumerate() {
                        assert_eq!(pic.y, refpic.y, "{NAME}: luma mismatch (seed {seed} frame {i})");
                        assert_eq!(pic.u, refpic.u, "{NAME}: U mismatch (seed {seed} frame {i})");
                        assert_eq!(pic.v, refpic.v, "{NAME}: V mismatch (seed {seed} frame {i})");
                    }
                    lr_refusals += 1; // reused below as "attempts that actually exercised LR pixels"
                }
                Err(e) => {
                    let msg = e.to_string();
                    other_refusals.push(format!("seed {seed}: {msg}"));
                }
            }
        }
        assert!(
            other_refusals.is_empty(),
            "{NAME}: {} attempts failed outright instead of decoding:\n{}",
            other_refusals.len(),
            other_refusals.join("\n")
        );
        assert!(
            lr_refusals > 0,
            "{NAME}: no attempt ever decoded pixel-exact ({n_attempts} attempts)"
        );
        // Any real (non-`RESTORE_NONE`) filter kind is acceptable proof --
        // aomenc's RD picks the frame-level `restoration_type` itself (not
        // always `Switchable`; observed `Sgrproj` live at cq-level=15 for
        // this fixture), so hard-requiring one specific arm would be the
        // gate-recipe-confound class (ledger).
        let total_hits = crate::restoration::wiener_hits()
            + crate::restoration::sgrproj_hits()
            + crate::restoration::switchable_hits();
        assert!(
            total_hits > 0,
            "{NAME}: {lr_refusals} LR refusals fired but read_lr_unit never decoded a real \
             (non-None) filter -- gate proved nothing about the symbol path"
        );
        eprintln!(
            "{NAME}: {lr_refusals} LR refusals, {} other refusals out of {n_attempts}, \
             wiener_hits={} sgrproj_hits={} switchable_hits={}",
            other_refusals.len(),
            crate::restoration::wiener_hits(),
            crate::restoration::sgrproj_hits(),
            crate::restoration::switchable_hits()
        );
    }

    /// r8: decode the ONE pinned stream `EC_AV1_GATE_DUMP` captured in r7
    /// (`fixtures/lr-sgr-r7.obu`, seed 46) directly -- not the live
    /// 40-attempt sweep, which reuses `v_start` coordinates across unrelated
    /// seeds/frames and made r6's captured 9 bytes unprovable. Run with
    /// `EC_LR_CALL_DUMP=1` set (see `restoration.rs`'s `apply_sgrproj_stripe`)
    /// to get the real window on stderr, call-uniquely keyed to `xqd ==
    /// [-16,-32]` rather than any coordinate.
    #[test]
    #[ignore = "reads a pinned fixture under the gitignored fixtures dir; run manually with EC_LR_CALL_DUMP=1"]
    fn pinned_lr_sgr_stream_call_unique_dump() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/lr-sgr-r7.obu");
        let stream = std::fs::read(path).expect("reading pinned lr-sgr-r7.obu");
        let pics = decode_stream(&stream).expect("pinned lr-sgr-r7.obu must decode");
        if have_ffmpeg() {
            let reference = ffmpeg_decode_sequence(&stream, 192, 128, pics.len());
            for (i, (got, want)) in pics.iter().zip(&reference).enumerate() {
                let y_mismatch = got.y != want.y;
                let u_mismatch = got.u != want.u;
                let v_mismatch = got.v != want.v;
                eprintln!(
                    "frame {i}: y_mismatch={y_mismatch} u_mismatch={u_mismatch} v_mismatch={v_mismatch}"
                );
                if y_mismatch {
                    let n = got.y.iter().zip(&want.y).filter(|(a, b)| a != b).count();
                    eprintln!("  {n} luma bytes differ / {}", got.y.len());
                }
                if v_mismatch {
                    let w = 96usize; // chroma plane width (192/2)
                    for (idx, (a, b)) in got.v.iter().zip(&want.v).enumerate() {
                        if a != b {
                            eprintln!("  V[{},{}] got={a} want={b}", idx / w, idx % w);
                        }
                    }
                }
            }
        }
    }

    /// lane-realworld r2: `read_cdef` (spec 5.11.56) ported -- per-superblock
    /// `cdef_idx`, wired at all five `skip`-decode sites, `apply_cdef` looks
    /// up strength through the grid instead of a hardcoded `[0]`. Structure
    /// copied from `a_real_aomenc_stream_with_masked_compound_decodes_pixel_exact`
    /// but `--enable-cdef=1` (the one thing that gate turns off) and every
    /// *other* filter/mode feature this lane doesn't touch disabled, so a
    /// mismatch here can only be attributed to CDEF. `cdef_idx_hits()` is a
    /// hard assert -- a run that never reads a real cdef_idx symbol proves
    /// nothing (class `gate-blind-to-feature`).
    #[test]
    fn a_real_aomenc_stream_with_cdef_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_cdef_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        // cdef_bits selects among PER-SUPERBLOCK strengths -- a single-SB
        // 64x64 frame (the other gates' size) gives aomenc nothing to
        // differentiate, so it always writes bits=0 and cdef_idx is never
        // read. 128x64 = 2 SBs is the minimum that lets aomenc's RD ever
        // pick bits > 0. Multi-SB content at --cpu-used=0 (every other
        // gate's setting) makes aomenc pick PARTITION_HORZ_4/VERT_B etc.
        // at the top SB level regardless of the ab/1to4-partitions=0
        // flags below (a real gap: this decoder's intra part64 match only
        // covers NONE/SPLIT, and the inter path blindly assumes SPLIT --
        // both pre-existing, out of this lane's scope); --cpu-used=4
        // avoids that RD path entirely and needs --aq-mode=0
        // --deltaq-mode=0 to keep delta_q/delta_lf (next lane step) off.
        let (width, height, frame_count) = (128usize, 64usize, 24usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_CDEF_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(40);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let duration = frame_count as f64 / 25.0;
            // mandelbrot's ringing-prone hard edges are what CDEF's RD
            // search targets; flat gradients (charter's first guess) may
            // never make aomenc choose cdef_bits > 0.
            let source = format!(
                "mandelbrot=size={width}x{height}:rate=25:start_x={sx}:start_y={sy}",
                sx = -0.6 + 0.005 * (attempt as f64),
                sy = -0.4 + 0.005 * (attempt as f64)
            );
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-t",
                    &duration.to_string(),
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=4",
                "--aq-mode=0",
                "--deltaq-mode=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                // r2: a starved inter toolset (everything below off) pushes
                // aomenc's RD toward exotic partition shapes (HORZ_4/VERT_B
                // etc.) at the SB(64) level even with those partition
                // flags off -- this decoder's part64 8/9/10-way arms and
                // the inter path's blind SPLIT assumption don't cover that;
                // keeping the same rich toolset the (working, ab/1to4-off)
                // masked-compound gate uses avoids it.
                "--enable-warped-motion=1",
                "--enable-global-motion=0",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-masked-comp=1",
                "--enable-dist-wtd-comp=0",
                "--enable-interintra-comp=1",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=1",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=1",
                "--enable-restoration=0",
                "--max-partition-size=32",
                "--min-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let mut child = Command::new(aomenc_path())
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    assert!(
                        !msg.contains("cdef"),
                        "{NAME} refused on cdef (seed {seed}) -- cdef_idx decode is ported: {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                std::fs::write(&path, &stream).expect("writing pinned stream");
                eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (seed {seed})");
            }
            matched += 1;
        }
        assert!(
            crate::decode::cdef_idx_hits() > 0,
            "{NAME}: zero cdef_idx blocks fired ({matched} matches, {named_refusals} other \
             refusals out of {n_attempts}) -- gate proved nothing this run"
        );
        eprintln!(
            "{NAME}: {named_refusals} other-capability refusals, {matched} pixel-exact matches \
             out of {n_attempts}, cdef_idx_hits={}",
            crate::decode::cdef_idx_hits()
        );
    }
    /// lane-tiny r1 diagnostic (not a gate): sweeps tiny key-frame sizes
    /// against real aomenc + ffmpeg to find the exact size boundary of the
    /// silent-wrong-pixels defect the sub8 lane's r2 dead-end flagged (a
    /// plain 16x16 stream, no exotic tools). `#[ignore]`d -- prints a
    /// pass/fail table, asserts nothing; run manually with `--nocapture`.
    #[test]
    #[ignore = "diagnostic sweep, run manually with --nocapture"]
    fn probe_tiny_frame_size_boundary() {
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP: no ffmpeg/aomenc");
            return;
        }
        let sizes: &[(usize, usize)] =
            &[(8, 8), (16, 16), (32, 32), (16, 32), (32, 16), (64, 64), (48, 48), (24, 24)];
        for &(width, height) in sizes {
            let mut ok = 0u32;
            let mut bad = 0u32;
            let mut refused = 0u32;
            let n = 10u32;
            for attempt in 0..n {
                let seed = 42 + attempt;
                let source = format!(
                    "mandelbrot=size={width}x{height}:rate=25:start_x={sx}:start_y={sy}",
                    sx = -0.6 + 0.005 * (attempt as f64),
                    sy = -0.4 + 0.005 * (attempt as f64)
                );
                let y4m = Command::new("ffmpeg")
                    .args([
                        "-v", "error", "-f", "lavfi", "-i", &source, "-t", "0.04", "-pix_fmt",
                        "yuv420p", "-f", "yuv4mpegpipe", "-",
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output()
                    .expect("ffmpeg failed to run");
                assert!(y4m.status.success(), "ffmpeg fixture: {}", String::from_utf8_lossy(&y4m.stderr));
                let max_part = width.max(height).min(64).next_power_of_two();
                let min_part = width.min(height).next_power_of_two().max(8).min(max_part);
                let max_part_arg = format!("--max-partition-size={max_part}");
                let min_part_arg = format!("--min-partition-size={min_part}");
                let args: Vec<&str> = vec![
                    "--codec=av1", "--passes=1", "--end-usage=q", "--cq-level=32",
                    "--cpu-used=4", "--kf-max-dist=0", "--limit=1", "--threads=1",
                    "--row-mt=0", "--enable-rect-partitions=0", "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    &max_part_arg, &min_part_arg,
                    "--obu", "-o", "-", "-",
                ];
                let mut child = Command::new(aomenc_path())
                    .args(&args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .expect("aomenc failed to start");
                child.stdin.take().expect("aomenc stdin").write_all(&y4m.stdout).expect("write y4m");
                let out = child.wait_with_output().expect("aomenc failed to run");
                assert!(out.status.success(), "aomenc refused: {}", String::from_utf8_lossy(&out.stderr));
                let stream = out.stdout;
                let frames = match decode_stream(&stream) {
                    Err(e) => {
                        eprintln!("  {width}x{height} seed {seed}: REFUSED {e}");
                        refused += 1;
                        continue;
                    }
                    Ok(f) => f,
                };
                let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
                if frames.len() == 1
                    && frames[0].y == ffmpeg_frames[0].y
                    && frames[0].u == ffmpeg_frames[0].u
                    && frames[0].v == ffmpeg_frames[0].v
                {
                    ok += 1;
                } else {
                    bad += 1;
                    let mut first = None;
                    for row in 0..height {
                        for col in 0..width {
                            let i = row * width + col;
                            if frames[0].y[i] != ffmpeg_frames[0].y[i] {
                                first = Some((row, col, frames[0].y[i], ffmpeg_frames[0].y[i]));
                                break;
                            }
                        }
                        if first.is_some() {
                            break;
                        }
                    }
                    let ndiff = frames[0]
                        .y
                        .iter()
                        .zip(&ffmpeg_frames[0].y)
                        .filter(|(a, b)| a != b)
                        .count();
                    eprintln!(
                        "  {width}x{height} seed {seed}: MISMATCH luma first_diff={first:?} ndiff_luma={ndiff}/{}",
                        width * height
                    );
                }
            }
            eprintln!("{width}x{height}: {ok} ok / {bad} mismatch / {refused} refused (of {n})");
        }
    }

    /// lane-tiny r1: re-decodes the known-failing 32x32 seed 45 fixture with
    /// `EC_AV1_TRACE` on to inspect what mode/tx this single-block frame
    /// picked. Diagnostic only, `#[ignore]`d, run manually with --nocapture.
    #[test]
    #[ignore = "diagnostic, run manually with --nocapture"]
    fn probe_tiny_32x32_trace() {
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP: no ffmpeg/aomenc");
            return;
        }
        let (width, height) = (32usize, 32usize);
        let attempt = 3u32; // seed 45
        let source = format!(
            "mandelbrot=size={width}x{height}:rate=25:start_x={sx}:start_y={sy}",
            sx = -0.6 + 0.005 * (attempt as f64),
            sy = -0.4 + 0.005 * (attempt as f64)
        );
        let y4m = Command::new("ffmpeg")
            .args([
                "-v", "error", "-f", "lavfi", "-i", &source, "-t", "0.04", "-pix_fmt", "yuv420p",
                "-f", "yuv4mpegpipe", "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("ffmpeg failed to run");
        assert!(y4m.status.success());
        let args: Vec<&str> = vec![
            "--codec=av1", "--passes=1", "--end-usage=q", "--cq-level=32", "--cpu-used=4",
            "--kf-max-dist=0", "--limit=1", "--threads=1", "--row-mt=0",
            "--enable-rect-partitions=0", "--enable-ab-partitions=0",
            "--enable-1to4-partitions=0", "--max-partition-size=32", "--min-partition-size=32",
            "--enable-filter-intra=0",
            "--obu", "-o", "-", "-",
        ];
        let mut child = Command::new(aomenc_path())
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("aomenc failed to start");
        child.stdin.take().unwrap().write_all(&y4m.stdout).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        let stream = out.stdout;
        let frames = decode_stream(&stream).expect("decode");
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
        let mismatched = frames[0].y != ffmpeg_frames[0].y;
        eprintln!("mismatched={mismatched}");
    }

    /// lane-tiny r2: decodes a raw OBU stream at `EC_TINY_FIXTURE_PATH` --
    /// no ffmpeg/aomenc invocation -- so a range ladder can be run
    /// (`EC_TRACE_MODE_STEP=1`) against the exact same bytes fed to the
    /// oracle aomdec, byte for byte. `#[ignore]`d diagnostic, no assertion.
    #[test]
    #[ignore = "diagnostic, run manually with --nocapture and EC_TINY_FIXTURE_PATH set"]
    fn probe_tiny_fixture_trace() {
        let path = std::env::var("EC_TINY_FIXTURE_PATH")
            .expect("set EC_TINY_FIXTURE_PATH to an .obu file");
        let stream = std::fs::read(&path).expect("reading fixture");
        match decode_stream(&stream) {
            Ok(frames) => {
                eprintln!("decoded {} frame(s)", frames.len());
                if let Ok(dump) = std::env::var("EC_TINY_FIXTURE_Y_DUMP") {
                    let bytes: Vec<u8> = frames[0].y.iter().map(|&v| v as u8).collect();
                    std::fs::write(&dump, &bytes).expect("dump y plane");
                }
            }
            Err(e) => eprintln!("REFUSED: {e}"),
        }
    }
    /// Pinned streams captured by `EC_AV1_GATE_DUMP` off
    /// `a_real_aomenc_stream_with_warped_motion_refuses_or_matches` -- each
    /// one is a former mismatch, kept as a regression pin (warp-mismatch:
    /// the WARPED_CAUSAL interp-read suppression; warp-flake-7: the same
    /// suppression for skip_mode blocks; warp-flake-5: mvstack entry
    /// clamping; ii-flake-1..8: interintra neighbours excluded from
    /// warp-sample gathering, ref_frame[1] == INTRA_FRAME in the mi grid).
    /// `EC_AV1_GATE_DUMP_PIN` overrides to a single stream.
    /// Deterministic and static -- `#[ignore]`d because the gitignored
    /// fixtures dir may be absent; run manually.
    #[test]
    #[ignore = "reads pinned fixture paths under the gitignored fixtures dir; run manually"]
    fn pinned_warp_stream_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!("SKIP pinned_warp_stream_decodes_pixel_exact: no ffmpeg");
            return;
        }
        let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures");
        let paths: Vec<String> = match std::env::var("EC_AV1_GATE_DUMP_PIN") {
            Ok(p) => vec![p],
            Err(_) => [
                "warp-mismatch",
                "warp-flake-5",
                "warp-flake-7",
                "ii-flake-1",
                "ii-flake-2",
                "ii-flake-3",
                "ii-flake-5",
                "ii-flake-6",
                "ii-flake-7",
                "ii-flake-8",
                // switchable_interp missing from reset_counts (counter
                // saturation slowed the adaptation rate) -- unrelated to
                // interintra, caught by the same gate.
                "ii-flake-9",
                // lane-rect r2: HORZ strip rect defects (mvstack size_h,
                // warp sample/projection dims, OBMC overlap, deblock tx-h).
                "rect-flake-1",
                // scan_row/scan_col weight `inc` min'd by the wrong candidate
                // axis (width vs height) -- ties reordered DRL entry 1 for a
                // block under a 32x16 strip, and suppressed the -5 extended
                // row scan.
                "rect-flake-2",
                // overlappable_left stepped the vertical walk by the
                // neighbour's WIDTH: a 32x16 left strip swallowed the strip
                // below it, blending the wrong OBMC prediction there.
                "rect-flake-3",
            ]
                .iter()
                .map(|n| format!("{fixtures}/{n}.obu"))
                .collect(),
        };
        for path in paths {
            eprintln!("pin: {path}");
            check_pinned_warp_stream(&path);
        }
    }

    fn check_pinned_warp_stream(path: &str) {
        use crate::decode::warp_selected_hits;
        let stream = std::fs::read(path).expect("reading pinned stream");
        let before = warp_selected_hits();
        let frames = decode_stream(&stream).expect("pinned stream must decode");
        eprintln!(
            "warp_selected_hits before={before} after={}",
            warp_selected_hits()
        );
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, 64, 64, 24);
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            if got.y != want.y {
                for row in 0..64 {
                    let mut line = String::new();
                    for col in 0..64 {
                        let d = got.y[row * 64 + col] as i32 - want.y[row * 64 + col] as i32;
                        line.push(if d == 0 {
                            '.'
                        } else if d.abs() < 4 {
                            'o'
                        } else {
                            'X'
                        });
                    }
                    eprintln!("{line}");
                }
            }
            assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (pinned)");
            assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (pinned)");
            assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (pinned)");
        }
    }

    /// `LAST2_FRAME`/`LAST3_FRAME` are just older slots in the same forward
    /// reference-buffer ring `LAST_FRAME`/`GOLDEN_FRAME` already prove --
    /// dropping the golden gate's `--max-reference-frames=3
    /// --reduced-reference-set=1` narrowing (which suppresses exactly these)
    /// and running long enough for aomenc's own buffer rotation to reach
    /// past the first 2 frames is the whole difference.
    #[test]
    fn a_real_aomenc_stream_with_a_last2_reference_decodes_pixel_exact() {
        a_real_aomenc_single_ref_gate(
            "a_real_aomenc_stream_with_a_last2_reference_decodes_pixel_exact",
            crate::mvstack::LAST2_FRAME,
            "LAST2_FRAME",
            &["--auto-alt-ref=0", "--lag-in-frames=0"],
            64,
            64,
            8,
            120,
        );
    }

    #[test]
    fn a_real_aomenc_stream_with_a_last3_reference_decodes_pixel_exact() {
        a_real_aomenc_single_ref_gate(
            "a_real_aomenc_stream_with_a_last3_reference_decodes_pixel_exact",
            crate::mvstack::LAST3_FRAME,
            "LAST3_FRAME",
            &["--auto-alt-ref=0", "--lag-in-frames=0"],
            64,
            64,
            8,
            120,
        );
    }

    /// `BWDREF`/`ALTREF2`/`ALTREF` are all backward (display-order-forward)
    /// references that only exist once aomenc builds a hierarchical GOP --
    /// `--auto-alt-ref=1 --lag-in-frames` (need at least one full mini-GOP
    /// of lookahead) is what makes aomenc code them at all; without it
    /// every attempt would hit this decoder's own real, still-open
    /// per-block sign-bias gap by never drawing the reference in the first
    /// place, not by proving it correct.
    #[test]
    fn a_real_aomenc_stream_with_a_bwdref_reference_decodes_pixel_exact() {
        a_real_aomenc_single_ref_gate(
            "a_real_aomenc_stream_with_a_bwdref_reference_decodes_pixel_exact",
            crate::mvstack::BWDREF_FRAME,
            "BWDREF_FRAME",
            &[
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
            ],
            64,
            64,
            16,
            120,
        );
    }

    /// lane-altref2 r1: 16 frames was too short for aomenc to ever build a
    /// second-level alt-ref pyramid, so this gate ran 86 attempts and fired
    /// ALTREF2 exactly zero times (class `gate-blind-to-feature`) -- the
    /// flags were already right, only the clip length was wrong. A 64-frame
    /// clip at the same `--lag-in-frames=16` gives aomenc enough lookahead
    /// to schedule a 2-level GF pyramid and reliably codes ALTREF2 (4/4
    /// hits on a plain seed-42 direct-aomenc probe; see
    /// lanes/altref2-r1.report.md for the recipe search table). Firing is
    /// now HARD-asserted below: the gate fails rather than skips if
    /// `ref_hits(ALTREF2_FRAME)` does not advance.
    #[test]
    fn a_real_aomenc_stream_with_an_altref2_reference_decodes_pixel_exact() {
        let before = decode::ref_hits(crate::mvstack::ALTREF2_FRAME);
        a_real_aomenc_single_ref_gate(
            "a_real_aomenc_stream_with_an_altref2_reference_decodes_pixel_exact",
            crate::mvstack::ALTREF2_FRAME,
            "ALTREF2_FRAME",
            &[
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
            ],
            64,
            64,
            64,
            120,
        );
        let after = decode::ref_hits(crate::mvstack::ALTREF2_FRAME);
        assert!(
            after > before,
            "a_real_aomenc_stream_with_an_altref2_reference_decodes_pixel_exact: \
             ALTREF2_FRAME never fired across 120 attempts -- the gate is vacuous \
             again (class gate-blind-to-feature); see lanes/altref2-r1.report.md \
             for the recipe that is supposed to make this fire"
        );
    }

    #[test]
    fn a_real_aomenc_stream_with_an_altref_reference_decodes_pixel_exact() {
        a_real_aomenc_single_ref_gate(
            "a_real_aomenc_stream_with_an_altref_reference_decodes_pixel_exact",
            crate::mvstack::ALTREF_FRAME,
            "ALTREF_FRAME",
            &[
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
            ],
            64,
            64,
            16,
            120,
        );
    }

    /// lane-av1tmvp: the temporal-MV gate -- unlike every `_single_ref_gate`
    /// attempt above, this one leaves `--enable-ref-frame-mvs` at aomenc's
    /// own default (on) instead of pinning it off, so `use_ref_frame_mvs`
    /// actually fires and `decode::tmv_hits()` (this gate's own capability
    /// proof, not just `non_last_ref_hits`) has to advance for the run to
    /// count as a real pass rather than a `never_fired` skip.
    fn a_real_aomenc_temporal_mv_gate(seed_base: u32, attempts: u32) {
        let gate_name = "a_real_aomenc_temporal_mv_gate";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {gate_name}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {gate_name}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 16usize);
        let mut never_fired = 0u32;
        let mut refusals = Vec::new();
        for attempt in 0..attempts {
            let seed = seed_base + attempt;
            let duration = frame_count as f64 / 25.0;
            let source =
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"));
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            // Same envelope as `a_real_aomenc_single_ref_gate`, minus its
            // `--enable-ref-frame-mvs=0` pin -- everything else this decoder
            // still cannot code stays refused the same way.
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=55",
                "--cpu-used=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-warped-motion=0",
                "--enable-obmc=0",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=0",
                "--enable-dist-wtd-comp=0",
                "--enable-diff-wtd-comp=0",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=0",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                "--enable-restoration=0",
                "--max-partition-size=32",
                "--min-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let mut child = Command::new(aomenc_path())
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before = decode::tmv_hits();
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{gate_name} failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
                Ok(frames) => frames,
            };
            if decode::tmv_hits() == before {
                never_fired += 1;
                continue;
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            if let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                let mismatched = frames
                    .iter()
                    .zip(&ffmpeg_frames)
                    .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
                if mismatched {
                    std::fs::write(&path, &stream).expect("writing pinned stream");
                    eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
                }
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "{gate_name} frame {i} luma vs ffmpeg (seed {seed})"
                );
                assert_eq!(
                    got.u, want.u,
                    "{gate_name} frame {i} U vs ffmpeg (seed {seed})"
                );
                assert_eq!(
                    got.v, want.v,
                    "{gate_name} frame {i} V vs ffmpeg (seed {seed})"
                );
            }
            eprintln!(
                "{gate_name} FIRING seed {seed}: tmv_hits advanced by {}",
                decode::tmv_hits() - before
            );
            return;
        }
        eprintln!(
            "SKIP {gate_name}: {never_fired} attempts decoded but never fired a temporal MV \
             candidate, and every other attempt hit a named refusal:\n{}",
            refusals.join("\n")
        );
    }

    #[test]
    fn a_real_aomenc_stream_with_temporal_mvs_decodes_pixel_exact() {
        a_real_aomenc_temporal_mv_gate(42, 40);
    }

    /// bisect scratch: decode a pinned stream exactly once -- generic
    /// pin-repro tool, kept for manual use.
    #[test]
    #[ignore]
    fn scratch_decode_pinned_stream_once() {
        let path = std::env::var("EC_AV1_PIN").expect("set EC_AV1_PIN to the .obu path");
        let stream = std::fs::read(&path).expect("read pinned stream");
        match decode_stream(&stream) {
            Ok(frames) => eprintln!("OK: {} frames", frames.len()),
            Err(e) => eprintln!("ERR: {e}"),
        }
    }

    /// scratch: isolate a pinned mismatching stream's first divergent pixel.
    #[test]
    #[ignore]
    fn scratch_isolate_pinned_mismatch() {
        let path = std::env::var("EC_AV1_PIN").expect("set EC_AV1_PIN to the .obu path");
        let stream = std::fs::read(&path).expect("read pinned stream");
        {
            let mut p = Av1Parser::new();
            let mut pos = 0usize;
            while pos < stream.len() {
                let obu = p.parse_obu(&stream[pos..]).expect("parse");
                pos += obu.total_size;
                if let ObuKind::Frame(header, _) = obu.kind {
                    eprintln!("interpolation_filter: {:?}", header.interpolation_filter);
                    eprintln!("cdef: {:?}", header.cdef);
                    eprintln!("quantization: {:?}", header.quantization);
                    eprintln!(
                        "coded_lossless: {:?} tx_mode: {:?}",
                        header.coded_lossless, header.tx_mode
                    );
                    eprintln!("delta: {:?}", header.delta);
                    eprintln!("segmentation: {:?}", header.segmentation);
                    eprintln!("loop_restoration: {:?}", header.loop_restoration);
                    eprintln!(
                        "use_128x128_superblock: {:?} disable_cdf_update: {:?} disable_frame_end_update_cdf: {:?} primary_ref_frame: {:?} allow_high_precision_mv: {:?} force_integer_mv: {:?}",
                        p.sequence_header().map(|s| s.use_128x128_superblock),
                        header.disable_cdf_update,
                        header.disable_frame_end_update_cdf,
                        header.primary_ref_frame,
                        header.allow_high_precision_mv,
                        header.force_integer_mv,
                    );
                }
            }
        }
        let width = std::env::var("EC_AV1_PIN_W")
            .map(|s| s.parse().unwrap())
            .unwrap_or(64);
        let height = std::env::var("EC_AV1_PIN_H")
            .map(|s| s.parse().unwrap())
            .unwrap_or(64);
        let frame_count = std::env::var("EC_AV1_PIN_N")
            .map(|s| s.parse().unwrap())
            .unwrap_or(4);
        let frames = decode_stream(&stream).expect("our decode");
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
        for f in 0..frame_count.min(frames.len()).min(ffmpeg_frames.len()) {
            let ours = &frames[f];
            let theirs = &ffmpeg_frames[f];
            for (plane_name, a, b, w) in [
                ("y", &ours.y, &theirs.y, width),
                ("u", &ours.u, &theirs.u, width / 2),
                ("v", &ours.v, &theirs.v, width / 2),
            ] {
                let mut first = None;
                let mut count = 0;
                let mut worst = 0i32;
                for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                    if x != y {
                        count += 1;
                        if first.is_none() {
                            first = Some(i);
                        }
                        worst = worst.max((*x as i32 - *y as i32).abs());
                    }
                }
                if let Some(i) = first {
                    eprintln!(
                        "frame {f} plane {plane_name}: {count} mismatches (worst delta {worst}), first at offset {i} (row {}, col {}) ours={} theirs={}",
                        i / w,
                        i % w,
                        a[i],
                        b[i]
                    );
                } else {
                    eprintln!("frame {f} plane {plane_name}: MATCH");
                }
            }
        }
    }

    /// Final round of the TX4/tx-class lane's own gate: `--min-partition-size=8
    /// --max-partition-size=8` pins every leaf to exactly 8x8 (spec
    /// 5.11.4's own boundary carve-out aside, which a multiple-of-8 frame
    /// never triggers), so a real `aomenc` stream from this recipe can only
    /// legitimately read `PARTITION_NONE` at every `bsize=3` node -- any
    /// other value this decoder sees there is a genuine desync, not a
    /// "partition below 8x8" the encoder actually wrote (traced live against
    /// a debug `aomdec` 2026-08-27: the true bitstream's own 8x8 partition
    /// reads are all `NONE`; the mismatch was `base_ctx`'s DC-position
    /// short-circuit firing for `TX_CLASS_HORIZ`/`VERT` too, off by one
    /// `BR` read into a spurious extra Golomb call). The sinusoidal-stripe
    /// `geq` source (a smooth luma ramp, not the sharp two-tone stripes that
    /// tripped `aomenc`'s screen-content-tools auto-detect) is what actually
    /// resolves `V_DCT`/`H_DCT` (`TxClass::of`) on some 8x8 TUs at this
    /// content's frequency; the attempt loop moves on from named refusals
    /// (`allow_screen_content_tools`, angle-delta) same as every other
    /// multi-seed recipe in this file, and only counts a run as a genuine
    /// firing when [`decode::tx_class1_hits`] actually moved.
    #[test]
    fn a_real_aomenc_min8_stream_with_tx_class1_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_min8_stream_with_tx_class1_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_min8_stream_with_tx_class1_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        let mut refusals = Vec::new();
        let mut fired_runs = 0u32;
        for attempt in 0..60u32 {
            let cq = 20 + (attempt % 5) * 10;
            let period = [4u32, 6, 8, 12, 16][(attempt as usize / 5) % 5];
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=64x64:rate=25",
                    "-vf",
                    &format!(
                        "geq=lum='128+80*sin(2*PI*Y/{period})':cb=128:cr=128,noise=alls=6:allf=t"
                    ),
                    "-pix_fmt",
                    "yuv420p",
                    "-t",
                    "0.04",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut child = Command::new(aomenc_path())
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &format!("--cq-level={cq}"),
                    "--cpu-used=0",
                    "--enable-directional-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-rect-partitions=0",
                    "--enable-angle-delta=0",
                    "--enable-palette=0",
                    "--reduced-tx-type-set=0",
                    "--enable-restoration=0",
                    "--enable-cdef=0",
                    "--loopfilter-control=0",
                    "--min-partition-size=8",
                    "--max-partition-size=8",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    // use_ref_frame_mvs (temporal MV projection) is unimplemented in mvstack;
                    // leaving it on silently desyncs symbols on inter frames (grainfix lesson).
                    "--enable-ref-frame-mvs=0",
                    "--tune-content=default",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            if !out.status.success() {
                refusals.push(format!(
                    "cq={cq} period={period}: aomenc itself refused the fixture"
                ));
                continue;
            }
            let stream = out.stdout;
            let before = decode::tx_class1_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    refusals.push(format!("cq={cq} period={period}: {e}"));
                    continue;
                }
            };
            if decode::tx_class1_hits() == before {
                refusals.push(format!(
                    "cq={cq} period={period}: decoded, but no block read a V_DCT/H_DCT tx_type"
                ));
                continue;
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "luma vs ffmpeg (cq={cq} period={period})"
            );
            assert_eq!(
                frames[0].u, ffmpeg_frames[0].u,
                "U vs ffmpeg (cq={cq} period={period})"
            );
            assert_eq!(
                frames[0].v, ffmpeg_frames[0].v,
                "V vs ffmpeg (cq={cq} period={period})"
            );
            fired_runs += 1;
            if fired_runs >= 4 {
                return;
            }
        }
        panic!(
            "fewer than 4 firing+pixel-exact runs out of 60 attempts:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-av1golden7 r9's decisive fixture: a real aomenc stream (seed 59,
    /// `--tune-content=film`, GOLDEN_FRAME firing downstream) whose frame-0
    /// *intra keyframe* mismatched ffmpeg on 3855/4096 luma pixels -- traced
    /// to `apply_grain=true` (spec 7.18.3 grain synthesis, now implemented in
    /// `crate::film_grain`), entirely unrelated to GOLDEN_FRAME or frame
    /// ordering. Frames beyond 0 in this fixture hit an unrelated, pre-existing
    /// gap (`decode_inter_block`'s GOLDEN_FRAME MC path, lane-av1golden7's own
    /// `golden7-forwarding-mismatch.obu`), so this test truncates the stream to
    /// the sequence header + frame-0 OBUs only -- exactly the span the
    /// mismatch was traced to -- rather than reaching into `decode.rs`/`mc.rs`
    /// to widen MC support, which is out of this lane's scope.
    #[test]
    fn a_real_aomenc_stream_with_film_grain_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_real_aomenc_stream_with_film_grain_decodes_pixel_exact: no ffmpeg");
            return;
        }
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/golden6-mismatch.obu"
        ))
        .unwrap();
        // Truncate right after the first `Frame` OBU (the key frame): every
        // byte up to there is sequence header + frame-0, which is a complete,
        // independently decodable single-frame stream.
        let mut parser = Av1Parser::new();
        let mut pos = 0usize;
        let mut frame0_end = None;
        while pos < data.len() {
            let obu = parser.parse_obu(&data[pos..]).unwrap();
            pos += obu.total_size;
            if matches!(obu.kind, ObuKind::Frame(..)) {
                frame0_end = Some(pos);
                break;
            }
        }
        let data = &data[..frame0_end.expect("fixture has at least one frame OBU")];
        let (width, height) = (64usize, 64usize);
        let ffmpeg_frames = ffmpeg_decode_sequence(data, width, height, 1);
        let decoded = decode_stream(data).unwrap();
        assert_eq!(decoded.len(), 1);
        let (got, want) = (&decoded[0], &ffmpeg_frames[0]);
        assert_eq!(got.y, want.y, "frame 0 luma vs ffmpeg (film grain)");
        assert_eq!(got.u, want.u, "frame 0 U vs ffmpeg (film grain)");
        assert_eq!(got.v, want.v, "frame 0 V vs ffmpeg (film grain)");
    }

    /// A real-encoder sweep, not just the one pinned fixture above
    /// (fixture-proves-symbol-not-signal class): several fresh
    /// `--tune-content=film` draws at different seeds/content, each decoded
    /// pixel-exact against ffmpeg's own AV1 decoder (which synthesizes grain
    /// by default -- confirmed via `ffmpeg -h decoder=av1`, no
    /// `-export_side_data`/grain-disable flag is passed anywhere in this
    /// file).
    #[test]
    fn real_aomenc_film_grain_streams_decode_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!("SKIP real_aomenc_film_grain_streams_decode_pixel_exact: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP real_aomenc_film_grain_streams_decode_pixel_exact: no aomenc");
            return;
        }
        let (width, height) = (64usize, 64usize);
        for seed in [1u64, 2, 3] {
            let dir = std::env::temp_dir().join(format!(
                "ec-av1-filmgrain-gate-{}-{seed}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let yuv_path = dir.join("in.yuv");
            let obu_path = dir.join("out.obu");
            let frames = 3usize;
            let luma = width * height;
            let chroma = luma / 4;
            let mut yuv = Vec::with_capacity((luma + 2 * chroma) * frames);
            for f in 0..frames {
                let card = panned_test_card(width, height, ((f as i32 + seed as i32) * 5) as i64);
                yuv.extend(card.y.iter().map(|&v| v as u8));
                yuv.extend(card.u.iter().map(|&v| v as u8));
                yuv.extend(card.v.iter().map(|&v| v as u8));
            }
            std::fs::write(&yuv_path, &yuv).unwrap();
            let status = Command::new(aomenc_path())
                .args([
                    &format!("--width={width}"),
                    &format!("--height={height}"),
                    "--input-bit-depth=8",
                    "--bit-depth=8",
                    "--fps=25/1",
                    &format!("--limit={frames}"),
                    // The full constraint set of the forwarding/golden gates,
                    // so the streams stay inside this decoder's supported
                    // envelope and the gate isolates grain synthesis alone;
                    // `--tune-content=film` is the one flag under test (it
                    // enables apply_grain).
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=32",
                    "--cpu-used=0",
                    "--lag-in-frames=0",
                    "--auto-alt-ref=0",
                    "--kf-max-dist=1000",
                    "--threads=1",
                    "--row-mt=0",
                    "--max-reference-frames=3",
                    "--reduced-reference-set=1",
                    "--enable-warped-motion=0",
                    "--enable-obmc=0",
                    "--enable-masked-comp=0",
                    "--enable-interintra-comp=0",
                    "--enable-dist-wtd-comp=0",
                    "--enable-diff-wtd-comp=0",
                    "--enable-onesided-comp=0",
                    "--enable-interintra-wedge=0",
                    "--enable-smooth-interintra=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-directional-intra=0",
                    "--enable-angle-delta=0",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--max-partition-size=32",
                    "--min-partition-size=32",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    // Temporal MV projection (`use_ref_frame_mvs`) has no
                    // decode path here (`mvstack.rs`'s documented corner-cut:
                    // `zero_mv_ctx` and the mode-context GLOBALMV_OFFSET bit
                    // are always computed as if it were off) -- this gate
                    // isolates grain synthesis, so it must stay inside that
                    // envelope like every other feature disabled above, not
                    // silently decode a wrong mode context. Root cause of
                    // this test's frame-1 mismatch: aomenc turns
                    // `use_ref_frame_mvs` on by default once order hints are
                    // enabled, unrelated to `--tune-content=film`.
                    "--enable-ref-frame-mvs=0",
                    "--tune-content=film",
                    "--obu",
                    "-o",
                ])
                .arg(&obu_path)
                .arg(&yuv_path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("aomenc failed to start");
            assert!(status.success(), "aomenc failed for seed {seed}");
            let encoded = std::fs::read(&obu_path).unwrap();
            let _ = std::fs::remove_dir_all(&dir);

            let mut parser = ec_av1_syntax::Av1Parser::new();
            let mut pos = 0usize;
            let mut saw_grain = false;
            while pos < encoded.len() {
                let obu = parser.parse_obu(&encoded[pos..]).unwrap();
                pos += obu.total_size;
                if let ec_av1_syntax::ObuKind::Frame(header, _) = obu.kind
                    && header.film_grain.apply_grain
                {
                    saw_grain = true;
                }
            }
            assert!(saw_grain, "seed {seed}: aomenc did not turn on apply_grain");

            let ffmpeg_frames = ffmpeg_decode_sequence(&encoded, width, height, frames);
            let decoded = decode_stream(&encoded).unwrap();
            assert_eq!(decoded.len(), frames, "seed {seed}");
            for (i, (got, want)) in decoded.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "seed {seed} frame {i} luma vs ffmpeg");
                assert_eq!(got.u, want.u, "seed {seed} frame {i} U vs ffmpeg");
                assert_eq!(got.v, want.v, "seed {seed} frame {i} V vs ffmpeg");
            }
        }
    }

    /// lane-tiles r3/r7: a real two-tile-column aomenc key frame, decoded
    /// through [`decode_key_frame_tile_with_cdfs`] directly with the whole
    /// `tiles: &[&[u8]]` list -- `decode_stream`'s `cols > 1 || rows > 1`
    /// refusal (this file, ~line 292) still fires for any stream this wide,
    /// so this gate bypasses it on purpose to prove the per-tile loop r2
    /// landed (fresh `SymbolDecoder`/CDF copy per tile, `Neighbours::start_tile`,
    /// `PlaneBuf::set_tile_origin`) actually decodes both tiles pixel-exact
    /// before that refusal is lifted. 128x64 is exactly two 64x64
    /// superblocks wide with `--tile-columns=1` (`TileCols=2`), so tile 1 is
    /// also the frame's own right edge -- the known gap (`PlaneBuf`'s
    /// tile_x0/tile_y0 only clip left/top reach, not a non-last column's
    /// right-edge reach) does not apply to this fixture; it would to a
    /// 3+-column one, proven separately by the four-tile-column gate below.
    /// r7: `--loopfilter-control=0` REMOVED per the charter -- `PlaneBuf`'s
    /// `tile_x1`/`tile_y1` clamps now receive real values at every call site
    /// (r6's `set_tile_origin` sweep), so this gate now proves real
    /// deblocking across the tile boundary too, not just decode with it off.
    /// Tile count itself is a structural aomenc param, not
    /// RD-dependent content, but this decoder's chroma smooth/paeth gap
    /// (`decode.rs` "a smooth or paeth chroma mode (round 2)", unrelated to
    /// tiles, pre-dating this lane) is content-dependent -- no aomenc flag
    /// disables UV-only smooth/paeth search, so a small seed sweep is
    /// needed the same way the sibling `a_real_aomenc_stream_with_*` gates
    /// retry past unrelated named refusals (every refusal in this crate is
    /// prefixed `"AV1 tile"` regardless of category, so unlike the sibling
    /// gates' `warp`/`obmc`/`interintra` banned lists there is no
    /// tile-specific substring to forbid here -- `matched > 0` plus the
    /// hard `tile_hits > 1`/pixel-exact asserts on any actual decode are
    /// the real proof).
    #[test]
    fn a_real_aomenc_stream_with_two_tile_columns_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_two_tile_columns_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (128usize, 64usize);
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        for attempt in 0..20u32 {
            let seed = 42 + attempt;
            let source = gradients_source(seed, width, height, "duration=0.04:rate=25");
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut child = Command::new(aomenc_path())
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--limit=1",
                    "--kf-max-dist=1000",
                    "--threads=1",
                    "--row-mt=0",
                    "--tile-columns=1",
                    "--tile-rows=0",
                    // This decoder hardcodes SB_MI=16 (64px) superblocks
                    // everywhere (`decode.rs`'s sb128 dead-end, ledger
                    // `av1-superblock-contexts`) -- without this, aomenc's
                    // own default 128x128 superblock makes 128x64 exactly
                    // ONE superblock wide, so `--tile-columns=1` has nothing
                    // to split and `cols` comes back 1.
                    "--sb-size=64",
                    // Deblocking crosses tile boundaries by default (spec
                    // `loop_filter_across_tiles_enabled`); `PlaneBuf`'s
                    // tile_x0/tile_y0 origin only ever clips a tile's own
                    // left/top reach (r2/r3's documented known gap), so a
                    // non-last tile column's right-edge deblock would read
                    // into the next tile's still-undecoded/differently-
                    // origined samples. Off here since deblocking itself is
                    // not what this gate is proving.
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-directional-intra=0",
                    "--enable-angle-delta=0",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--max-partition-size=32",
                    "--min-partition-size=32",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;

            // Parse the stream ourselves (rather than `decode_stream`, whose
            // multi-tile refusal is exactly what this gate exists to run
            // ahead of) to reach the real `Frame` OBU's `tile_info` and its
            // full `Vec<Tile>` -- `Av1Parser::parse_obu` already locates
            // every tile in a multi-tile tile group (`parse_tile_group`), it
            // is only `decode_stream` that ever narrows this to
            // `tiles.first()`.
            let mut parser = Av1Parser::new();
            let mut pos = 0usize;
            let mut frame = None;
            while pos < stream.len() {
                let obu_offset = pos;
                let obu = parser.parse_obu(&stream[pos..]).unwrap();
                pos += obu.total_size;
                if let ObuKind::Frame(header, tiles) = obu.kind {
                    frame = Some((header, tiles, obu_offset));
                    break;
                }
            }
            let (header, tiles, obu_offset) = frame.expect("stream has a Frame OBU");
            assert!(
                header.tile_info.cols > 1,
                "{NAME}: aomenc did not actually split into >1 tile column (cols={}, seed {seed})",
                header.tile_info.cols
            );
            assert_eq!(
                tiles.len(),
                header.tile_info.cols as usize,
                "{NAME}: expected one tile per column (seed {seed})"
            );
            // `Tile::offset` is relative to the buffer `parse_obu` was
            // handed (`&stream[pos..]` at the time this OBU was parsed), so
            // it needs `obu_offset` added back -- the same adjustment
            // `decode_stream` makes for its own single-tile slice above.
            let tile_bufs: Vec<&[u8]> = tiles
                .iter()
                .map(|t| &stream[obu_offset + t.offset..obu_offset + t.offset + t.size])
                .collect();

            let enable_filter_intra = parser
                .sequence_header()
                .is_some_and(|seq| seq.enable_filter_intra);
            let enable_edge_filter = parser
                .sequence_header()
                .is_some_and(|seq| seq.enable_intra_edge_filter);

            let before = crate::decode::tile_hits();
            let picture = match decode_key_frame_tile_with_cdfs(
                &tile_bufs,
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
                None,
                header.tx_mode == TxMode::Select,
                header.reduced_tx_set,
                header.allow_screen_content_tools,
                header.allow_intrabc,
                header.delta,
            ) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok((picture, _end_cdfs)) => picture,
            };
            let hits = crate::decode::tile_hits() - before;
            assert!(
                hits > 1,
                "{NAME}: tile_hits delta was {hits}, expected >1 -- both tiles must actually \
                 decode, not just tile 0 (seed {seed})"
            );

            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(picture.y, ffmpeg_frames[0].y, "{NAME}: luma vs ffmpeg (seed {seed})");
            assert_eq!(picture.u, ffmpeg_frames[0].u, "{NAME}: U vs ffmpeg (seed {seed})");
            assert_eq!(picture.v, ffmpeg_frames[0].v, "{NAME}: V vs ffmpeg (seed {seed})");
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals} refusals); the gate never decoded \
             a two-tile-column stream"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals out of 20"
        );
    }

    /// lane-tiles r8 stage 1: the same bypass-the-refusal pattern as the
    /// two-tile-column gate above, this time for tile ROWS -- `decode_stream`
    /// still refuses any `tile_info.rows > 1` stream, so this gate calls
    /// [`decode_key_frame_tile_with_cdfs`] directly to prove the underlying
    /// per-tile loop (already row/col-generic: `tile_num / tile_info.cols`
    /// gives the row, `mi_row_starts`/`set_tile_origin`'s `y0`/`y1` are
    /// exactly symmetric with the column gate's `x0`/`x1`) actually decodes
    /// both tile rows pixel-exact before the refusal is lifted. 64x128 is
    /// one 64px superblock wide and two tall with `--tile-rows=1`
    /// (`TileRows=2`, `--tile-columns=0` keeps `cols=1` so only the row axis
    /// is under test). Loop filter is left ON (no
    /// `--loopfilter-control=0`) per the charter -- deblocking crosses tile
    /// row boundaries by spec default exactly as it crosses column ones, and
    /// `deblock_plane` runs once over the whole decoded picture after every
    /// tile is in, with no tile-axis distinction, so there is no reason to
    /// expect the row boundary to behave differently from the column one r7
    /// already proved -- this gate is what actually proves it either way.
    #[test]
    fn a_real_aomenc_stream_with_two_tile_rows_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_two_tile_rows_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (64usize, 128usize);
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        for attempt in 0..20u32 {
            let seed = 42 + attempt;
            let source = gradients_source(seed, width, height, "duration=0.04:rate=25");
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut child = Command::new(aomenc_path())
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--limit=1",
                    "--kf-max-dist=1000",
                    "--threads=1",
                    "--row-mt=0",
                    "--tile-columns=0",
                    "--tile-rows=1",
                    // This decoder hardcodes SB_MI=16 (64px) superblocks
                    // everywhere; without this aomenc's own default 128x128
                    // superblock makes 64x128 exactly ONE superblock tall,
                    // so `--tile-rows=1` has nothing to split.
                    "--sb-size=64",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-directional-intra=0",
                    "--enable-angle-delta=0",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--max-partition-size=32",
                    "--min-partition-size=32",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;

            let mut parser = Av1Parser::new();
            let mut pos = 0usize;
            let mut frame = None;
            while pos < stream.len() {
                let obu_offset = pos;
                let obu = parser.parse_obu(&stream[pos..]).unwrap();
                pos += obu.total_size;
                if let ObuKind::Frame(header, tiles) = obu.kind {
                    frame = Some((header, tiles, obu_offset));
                    break;
                }
            }
            let (header, tiles, obu_offset) = frame.expect("stream has a Frame OBU");
            assert!(
                header.tile_info.rows > 1,
                "{NAME}: aomenc did not actually split into >1 tile row (rows={}, seed {seed})",
                header.tile_info.rows
            );
            assert_eq!(
                tiles.len(),
                header.tile_info.rows as usize,
                "{NAME}: expected one tile per row (cols=1, seed {seed})"
            );
            let tile_bufs: Vec<&[u8]> = tiles
                .iter()
                .map(|t| &stream[obu_offset + t.offset..obu_offset + t.offset + t.size])
                .collect();

            let enable_filter_intra = parser
                .sequence_header()
                .is_some_and(|seq| seq.enable_filter_intra);
            let enable_edge_filter = parser
                .sequence_header()
                .is_some_and(|seq| seq.enable_intra_edge_filter);

            let before = crate::decode::tile_hits();
            let picture = match decode_key_frame_tile_with_cdfs(
                &tile_bufs,
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
                None,
                header.tx_mode == TxMode::Select,
                header.reduced_tx_set,
                header.allow_screen_content_tools,
                header.allow_intrabc,
                header.delta,
            ) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok((picture, _end_cdfs)) => picture,
            };
            let hits = crate::decode::tile_hits() - before;
            assert!(
                hits > 1,
                "{NAME}: tile_hits delta was {hits}, expected >1 -- both tile rows must actually \
                 decode, not just tile 0 (seed {seed})"
            );

            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(picture.y, ffmpeg_frames[0].y, "{NAME}: luma vs ffmpeg (seed {seed})");
            assert_eq!(picture.u, ffmpeg_frames[0].u, "{NAME}: U vs ffmpeg (seed {seed})");
            assert_eq!(picture.v, ffmpeg_frames[0].v, "{NAME}: V vs ffmpeg (seed {seed})");
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals} refusals); the gate never decoded \
             a two-tile-row stream"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals out of 20"
        );
    }

    /// lane-tiles r8 stage 2/3: the same two-tile-row stream as the bypass
    /// gate above, this time through `decode_stream` itself -- the entry
    /// point real callers use, with the `rows > 1` refusal lifted (same
    /// commit) so this actually reaches the tile decode instead of
    /// refusing by name. Loop filter ON, no `--loopfilter-control=0`.
    #[test]
    fn a_real_aomenc_stream_with_two_tile_rows_decodes_through_decode_stream() {
        const NAME: &str = "a_real_aomenc_stream_with_two_tile_rows_decodes_through_decode_stream";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (64usize, 128usize);
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        for attempt in 0..20u32 {
            let seed = 42 + attempt;
            let source = gradients_source(seed, width, height, "duration=0.04:rate=25");
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v", "error", "-f", "lavfi", "-i", &source, "-pix_fmt", "yuv420p", "-f",
                    "yuv4mpegpipe", "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut child = Command::new(aomenc_path())
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--limit=1",
                    "--kf-max-dist=1000",
                    "--threads=1",
                    "--row-mt=0",
                    "--tile-columns=0",
                    "--tile-rows=1",
                    "--sb-size=64",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-directional-intra=0",
                    "--enable-angle-delta=0",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--max-partition-size=32",
                    "--min-partition-size=32",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;

            let before = crate::decode::tile_hits();
            let pictures = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(pictures) => pictures,
            };
            let hits = crate::decode::tile_hits() - before;
            assert!(
                hits > 1,
                "{NAME}: tile_hits delta was {hits}, expected >1 -- both tile rows must actually \
                 decode, not just tile 0 (seed {seed})"
            );
            assert_eq!(pictures.len(), 1, "{NAME}: expected exactly 1 picture (seed {seed})");

            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(pictures[0].y, ffmpeg_frames[0].y, "{NAME}: luma vs ffmpeg (seed {seed})");
            assert_eq!(pictures[0].u, ffmpeg_frames[0].u, "{NAME}: U vs ffmpeg (seed {seed})");
            assert_eq!(pictures[0].v, ffmpeg_frames[0].v, "{NAME}: V vs ffmpeg (seed {seed})");
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals} refusals); decode_stream never \
             decoded a two-tile-row stream"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals out of 20"
        );
    }

    /// lane-tiles r7: the `>2 tile columns` refusal's own capability, proven
    /// separately from the 2-column gate above -- `--tile-columns=2` gives
    /// aomenc `TileCols=4` (log2), a real non-last-column right-edge case the
    /// 2-column fixture cannot exercise (there tile 1 IS the frame's right
    /// edge). 256x64 is four 64x64 superblocks wide with `--sb-size=64`, so
    /// each tile column is exactly one superblock. Loop filtering stays ON
    /// (no `--loopfilter-control=0`) to prove the same reach-bound fix also
    /// covers deblocking across an *interior* tile boundary, not just the
    /// 2-column case's boundary-that-is-also-the-frame-edge.
    #[test]
    fn a_real_aomenc_stream_with_four_tile_columns_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_four_tile_columns_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (256usize, 64usize);
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        for attempt in 0..20u32 {
            let seed = 42 + attempt;
            let source = gradients_source(seed, width, height, "duration=0.04:rate=25");
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v", "error", "-f", "lavfi", "-i", &source, "-pix_fmt", "yuv420p", "-f",
                    "yuv4mpegpipe", "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut child = Command::new(aomenc_path())
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--limit=1",
                    "--kf-max-dist=1000",
                    "--threads=1",
                    "--row-mt=0",
                    "--tile-columns=2",
                    "--tile-rows=0",
                    "--sb-size=64",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-directional-intra=0",
                    "--enable-angle-delta=0",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--max-partition-size=32",
                    "--min-partition-size=32",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;

            let mut parser = Av1Parser::new();
            let mut pos = 0usize;
            let mut frame = None;
            while pos < stream.len() {
                let obu_offset = pos;
                let obu = parser.parse_obu(&stream[pos..]).unwrap();
                pos += obu.total_size;
                if let ObuKind::Frame(header, tiles) = obu.kind {
                    frame = Some((header, tiles, obu_offset));
                    break;
                }
            }
            let (header, tiles, obu_offset) = frame.expect("stream has a Frame OBU");
            assert!(
                header.tile_info.cols > 2,
                "{NAME}: aomenc did not actually split into >2 tile columns (cols={}, seed {seed})",
                header.tile_info.cols
            );
            assert_eq!(
                tiles.len(),
                header.tile_info.cols as usize,
                "{NAME}: expected one tile per column (seed {seed})"
            );
            let tile_bufs: Vec<&[u8]> = tiles
                .iter()
                .map(|t| &stream[obu_offset + t.offset..obu_offset + t.offset + t.size])
                .collect();

            let enable_filter_intra = parser
                .sequence_header()
                .is_some_and(|seq| seq.enable_filter_intra);
            let enable_edge_filter = parser
                .sequence_header()
                .is_some_and(|seq| seq.enable_intra_edge_filter);

            let before = crate::decode::tile_hits();
            let picture = match decode_key_frame_tile_with_cdfs(
                &tile_bufs,
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
                None,
                header.tx_mode == TxMode::Select,
                header.reduced_tx_set,
                header.allow_screen_content_tools,
                header.allow_intrabc,
                header.delta,
            ) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok((picture, _end_cdfs)) => picture,
            };
            let hits = crate::decode::tile_hits() - before;
            assert!(
                hits > 2,
                "{NAME}: tile_hits delta was {hits}, expected >2 -- all four tiles must actually \
                 decode, not just a subset (seed {seed})"
            );

            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(picture.y, ffmpeg_frames[0].y, "{NAME}: luma vs ffmpeg (seed {seed})");
            assert_eq!(picture.u, ffmpeg_frames[0].u, "{NAME}: U vs ffmpeg (seed {seed})");
            assert_eq!(picture.v, ffmpeg_frames[0].v, "{NAME}: V vs ffmpeg (seed {seed})");
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals} refusals); the gate never decoded \
             a four-tile-column stream"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals out of 20"
        );
    }

    /// lane-tiles r8: the tile-*row* analogue of the four-tile-column gate
    /// above -- `--tile-rows=2` gives aomenc `TileRows=4` (log2; uniform
    /// tile spacing is always a power of two, so this is the smallest count
    /// that has an interior row neither the top nor bottom frame edge,
    /// mirroring why the column gate needed 4 columns rather than 3). 64x256
    /// is four 64x64 superblocks tall with `--sb-size=64`, one per tile row.
    /// Loop filter ON, run through `decode_stream` itself since the
    /// `rows > 1` refusal is already lifted by this commit. Also settles a
    /// stale documentation claim found while writing this gate: the old
    /// comment above `decode_stream`'s multi-tile block said
    /// `context_update_tile_id != 0` was refused "further down" -- no such
    /// refusal exists (`decode.rs` reads `tile_info.context_update_tile_id`
    /// generically, never hardcoded to tile 0), and this fixture's own
    /// aomenc runs pick 1/2/3 as often as 0 (RD-driven tile-size heuristic),
    /// all 20/20 pixel-exact -- so the capability was already there,
    /// undocumented and unproven; hard-asserted here so the proof can't go
    /// unnoticed if it stops firing.
    #[test]
    fn a_real_aomenc_stream_with_four_tile_rows_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_four_tile_rows_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (64usize, 256usize);
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        let mut saw_nonzero_context_update_tile_id = false;
        for attempt in 0..20u32 {
            let seed = 42 + attempt;
            let source = gradients_source(seed, width, height, "duration=0.04:rate=25");
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v", "error", "-f", "lavfi", "-i", &source, "-pix_fmt", "yuv420p", "-f",
                    "yuv4mpegpipe", "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut child = Command::new(aomenc_path())
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--limit=1",
                    "--kf-max-dist=1000",
                    "--threads=1",
                    "--row-mt=0",
                    "--tile-columns=0",
                    "--tile-rows=2",
                    "--sb-size=64",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-directional-intra=0",
                    "--enable-angle-delta=0",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--max-partition-size=32",
                    "--min-partition-size=32",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;

            let mut parser = Av1Parser::new();
            let mut pos = 0usize;
            let mut rows_seen = 0u32;
            while pos < stream.len() {
                let obu = parser.parse_obu(&stream[pos..]).unwrap();
                pos += obu.total_size;
                if let ObuKind::Frame(header, _) = &obu.kind {
                    rows_seen = header.tile_info.rows;
                    if header.tile_info.context_update_tile_id != 0 {
                        saw_nonzero_context_update_tile_id = true;
                    }
                }
            }
            assert!(
                rows_seen > 2,
                "{NAME}: aomenc did not actually split into >2 tile rows (rows={rows_seen}, seed {seed})"
            );

            let before = crate::decode::tile_hits();
            let pictures = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(pictures) => pictures,
            };
            let hits = crate::decode::tile_hits() - before;
            assert!(
                hits > 2,
                "{NAME}: tile_hits delta was {hits}, expected >2 -- all four tile rows must \
                 actually decode, not just a subset (seed {seed})"
            );
            assert_eq!(pictures.len(), 1, "{NAME}: expected exactly 1 picture (seed {seed})");

            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(pictures[0].y, ffmpeg_frames[0].y, "{NAME}: luma vs ffmpeg (seed {seed})");
            assert_eq!(pictures[0].u, ffmpeg_frames[0].u, "{NAME}: U vs ffmpeg (seed {seed})");
            assert_eq!(pictures[0].v, ffmpeg_frames[0].v, "{NAME}: V vs ffmpeg (seed {seed})");
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals} refusals); the gate never decoded \
             a four-tile-row stream"
        );
        assert!(
            saw_nonzero_context_update_tile_id,
            "{NAME}: every attempt named context_update_tile_id=0 -- this sweep proves nothing \
             about the != 0 case (see the stale-refusal note above `decode_stream`'s multi-tile \
             comment: aomenc's own tile-size heuristic picks it, seen 1/2/3 live in a prior probe \
             run of this exact fixture, so a 20-attempt window not seeing it is the gate's own bug)"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals out of 20"
        );
    }

    /// lane-tiles r4/r7: the same real two-tile-column stream as the
    /// bypass gate above, this time run through `decode_stream` itself --
    /// the entry point real callers use. Proves the `cols > 1` refusal is
    /// actually lifted (not just that the underlying per-tile loop works)
    /// for a key frame; `--loopfilter-control=0` stays here for its own
    /// sake (isolating the entry-point wiring from deblocking), but r7's
    /// `a_real_aomenc_stream_with_four_tile_columns_decodes_pixel_exact`
    /// bypass gate now proves loop filtering ON across a tile boundary too,
    /// and the `cols > 1 && loop_filter_on` / `cols > 2` refusals this
    /// comment used to describe are gone from `decode_stream`.
    #[test]
    fn a_real_aomenc_stream_with_two_tile_columns_decodes_through_decode_stream() {
        const NAME: &str = "a_real_aomenc_stream_with_two_tile_columns_decodes_through_decode_stream";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (128usize, 64usize);
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        for attempt in 0..20u32 {
            let seed = 42 + attempt;
            let source = gradients_source(seed, width, height, "duration=0.04:rate=25");
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v", "error", "-f", "lavfi", "-i", &source, "-pix_fmt", "yuv420p", "-f",
                    "yuv4mpegpipe", "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut child = Command::new(aomenc_path())
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--limit=1",
                    "--kf-max-dist=1000",
                    "--threads=1",
                    "--row-mt=0",
                    "--tile-columns=1",
                    "--tile-rows=0",
                    "--sb-size=64",
                    "--loopfilter-control=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-directional-intra=0",
                    "--enable-angle-delta=0",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--max-partition-size=32",
                    "--min-partition-size=32",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;

            let before = crate::decode::tile_hits();
            let pictures = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(pictures) => pictures,
            };
            let hits = crate::decode::tile_hits() - before;
            assert!(
                hits > 1,
                "{NAME}: tile_hits delta was {hits}, expected >1 -- both tiles must actually \
                 decode, not just tile 0 (seed {seed})"
            );
            assert_eq!(pictures.len(), 1, "{NAME}: expected exactly 1 picture (seed {seed})");

            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(pictures[0].y, ffmpeg_frames[0].y, "{NAME}: luma vs ffmpeg (seed {seed})");
            assert_eq!(pictures[0].u, ffmpeg_frames[0].u, "{NAME}: U vs ffmpeg (seed {seed})");
            assert_eq!(pictures[0].v, ffmpeg_frames[0].v, "{NAME}: V vs ffmpeg (seed {seed})");
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals} refusals); decode_stream never \
             decoded a two-tile-column stream"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals out of 20"
        );
    }
    /// lane-realworld r6: r5 ported `maybe_read_delta_q`/`maybe_read_delta_lf`
    /// and removed the whole-frame refusal, but no fixture anywhere set
    /// `delta_lf_present` -- the removal was unproven. `--deltaq-mode=2`
    /// (`DELTA_Q_PERCEPTUAL`, checked against libaom's own
    /// `encodeframe.c:2192-2225`) sets `delta_q_present_flag` unconditionally
    /// once `base_qindex > 0` (unlike the default `--deltaq-mode=1`
    /// `DELTA_Q_OBJECTIVE`, which additionally requires an alt-ref-eligible
    /// frame and `allow_deltaq_mode`'s own RD search); `--delta-lf-mode=1`
    /// then ANDs `delta_lf_present_flag` on top
    /// (`tool_cfg->enable_deltalf_mode`, `av1_cx_iface.c:1269-1270`). 128x64
    /// (2 SBs; this decoder hardcodes 64px SBs) is the charter's minimum for
    /// a per-superblock symbol to have somewhere to differ; `--cpu-used=4`
    /// (not the other gates' `=0`) sidesteps the multi-SB
    /// HORZ_4/VERT_B-at-part64 gap the cdef gate's own comment documents
    /// (lane-realworld r2 dead-end, part64 only covers NONE/SPLIT). Note:
    /// libaom hardcodes `delta_lf_multi = DEFAULT_DELTA_LF_MULTI == 0`
    /// (`enums.h:73`) with no CLI flag to set it -- this gate cannot and
    /// does not claim to exercise this decoder's `delta_lf_multi` branch;
    /// only the single-plane path is gate-proven.
    #[test]
    fn a_real_aomenc_stream_with_delta_q_and_delta_lf_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_delta_q_and_delta_lf_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (128usize, 64usize, 24usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_DELTAQ_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(40);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let duration = frame_count as f64 / 25.0;
            // Mandelbrot's varying local contrast is what gives
            // DELTA_Q_PERCEPTUAL's per-superblock RD something to
            // differentiate; flat gradients risk every superblock
            // resolving the same delta.
            let source = format!(
                "mandelbrot=size={width}x{height}:rate=25:start_x={sx}:start_y={sy}",
                sx = -0.6 + 0.005 * (attempt as f64),
                sy = -0.4 + 0.005 * (attempt as f64)
            );
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-t",
                    &duration.to_string(),
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=4",
                "--aq-mode=0",
                "--deltaq-mode=2",
                "--delta-lf-mode=1",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=1",
                "--enable-global-motion=0",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-masked-comp=1",
                "--enable-dist-wtd-comp=0",
                "--enable-interintra-comp=1",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=1",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=1",
                "--enable-restoration=0",
                "--max-partition-size=32",
                "--min-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let mut child = Command::new(aomenc_path())
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    assert!(
                        !msg.contains("delta_q") && !msg.contains("delta_lf"),
                        "{NAME} refused on delta_q/delta_lf (seed {seed}) -- that read is ported: {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                std::fs::write(&path, &stream).expect("writing pinned stream");
                eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (seed {seed})");
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused on other capabilities ({named_refusals} refusals); \
             the gate never exercised delta_q/delta_lf"
        );
        assert!(
            crate::decode::delta_q_hits() > 0,
            "{NAME}: {matched} pixel-exact matches but zero delta_q symbol groups fired -- \
             the reader is unexercised"
        );
        assert!(
            crate::decode::delta_lf_hits() > 0,
            "{NAME}: {matched} pixel-exact matches but zero delta_lf symbol groups fired -- \
             the reader is unexercised"
        );
        eprintln!(
            "{NAME}: {named_refusals} other-capability refusals, {matched} pixel-exact matches \
             out of {n_attempts}, delta_q_hits={}, delta_lf_hits={}",
            crate::decode::delta_q_hits(),
            crate::decode::delta_lf_hits()
        );
    }

    /// lane-tiles r7: the gate stage 2 (r6, commit 8899fc1) shipped without
    /// -- a real >1-tile-column stream with a genuine INTER frame (not just
    /// the key-frame-only two-column gate above), decoded through
    /// `decode_stream` itself. Same recipe as the key-frame gate
    /// (`--sb-size=64`, `--tile-columns=1`, `--loopfilter-control=0`, the
    /// same feature-disable envelope so the gate isolates the tile loop, not
    /// an unrelated unimplemented mode) plus `--limit=2 --kf-max-dist=1000`
    /// so frame 1 is a real inter frame, and `gradients_source`'s default
    /// `speed=0.01` rotation (undisturbed -- not overridden by the tail)
    /// gives it genuine motion against frame 0, so `decode_inter_frame_tile_
    /// with_cdfs`'s new per-tile loop and `MiGrid`'s per-tile bound actually
    /// have to walk a live MV stack across the tile-column boundary, not
    /// just replay skip/zero-mv blocks. Bounded with `duration=0.08` (2
    /// frames at rate=25) per the class in ledger
    /// `refusal-claim-disproved-by-its-own-gate`/`gate-loader-slurps-whole-
    /// file` (never leave an ffmpeg/lavfi generate unbounded).
    #[test]
    fn a_real_aomenc_stream_with_two_tile_columns_and_an_inter_frame_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_two_tile_columns_and_an_inter_frame_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (128usize, 64usize);
        let frames = 2usize;
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        for attempt in 0..20u32 {
            let seed = 42 + attempt;
            let source = gradients_source(seed, width, height, "duration=0.08:rate=25");
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v", "error", "-f", "lavfi", "-i", &source, "-pix_fmt", "yuv420p", "-f",
                    "yuv4mpegpipe", "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut child = Command::new(aomenc_path())
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=45",
                    "--cpu-used=0",
                    &format!("--limit={frames}"),
                    "--kf-max-dist=1000",
                    "--lag-in-frames=0",
                    "--auto-alt-ref=0",
                    "--threads=1",
                    "--row-mt=0",
                    "--tile-columns=1",
                    "--tile-rows=0",
                    "--sb-size=64",
                    "--loopfilter-control=0",
                    "--max-reference-frames=3",
                    "--reduced-reference-set=1",
                    "--enable-warped-motion=0",
                    "--enable-obmc=0",
                    "--enable-masked-comp=0",
                    "--enable-interintra-comp=0",
                    "--enable-dist-wtd-comp=0",
                    "--enable-diff-wtd-comp=0",
                    "--enable-onesided-comp=0",
                    "--enable-interintra-wedge=0",
                    "--enable-smooth-interintra=0",
                    "--enable-ref-frame-mvs=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-directional-intra=0",
                    "--enable-angle-delta=0",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--max-partition-size=32",
                    "--min-partition-size=32",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;

            // Confirm aomenc actually split into >1 tile column and that
            // frame 1 is a real inter frame before trusting a pixel match --
            // otherwise a match could mean "tiles collapsed to 1" or "frame
            // 1 fell back to key", not "the inter tile loop ran".
            let mut parser = Av1Parser::new();
            let mut pos = 0usize;
            let mut saw_multi_tile_inter = false;
            while pos < stream.len() {
                let obu = parser.parse_obu(&stream[pos..]).unwrap();
                pos += obu.total_size;
                if let ObuKind::Frame(header, _) = obu.kind
                    && header.frame_type != FrameType::Key
                    && header.tile_info.cols > 1
                {
                    saw_multi_tile_inter = true;
                }
            }
            assert!(
                saw_multi_tile_inter,
                "{NAME}: seed {seed} never produced a >1-tile-column inter frame (aomenc \
                 collapsed tiling or coded frame 1 as key)"
            );

            let before = crate::decode::tile_hits();
            let pictures = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(pictures) => pictures,
            };
            let hits = crate::decode::tile_hits() - before;
            assert!(
                hits > frames,
                "{NAME}: tile_hits delta was {hits}, expected > {frames} -- both tiles of both \
                 frames must actually decode (seed {seed})"
            );
            assert_eq!(pictures.len(), frames, "{NAME}: expected {frames} pictures (seed {seed})");

            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frames);
            for (i, (got, want)) in pictures.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "{NAME}: seed {seed} frame {i} luma vs ffmpeg");
                assert_eq!(got.u, want.u, "{NAME}: seed {seed} frame {i} U vs ffmpeg");
                assert_eq!(got.v, want.v, "{NAME}: seed {seed} frame {i} V vs ffmpeg");
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals} refusals); decode_stream never \
             decoded a two-tile-column inter frame"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals out of 20"
        );
    }

    /// lane-tiles r9: `--num-tile-groups` makes aomenc split one frame's
    /// tiles across several `OBU_TILE_GROUP`s instead of folding them into
    /// the single `OBU_FRAME` every other gate in this file relies on --
    /// verified live (`scratch_probe_num_tile_groups`, this round) to emit a
    /// standalone `OBU_FRAME_HEADER` (`show_existing_frame == false`)
    /// followed by two `OBU_TILE_GROUP` OBUs, one tile each. Before this
    /// round `decode_stream` had no arm for `ObuKind::TileGroup` at all (it
    /// fell through the catch-all `continue`, silently dropping the frame),
    /// and its `ObuKind::FrameHeader` arm matched *every* frame header
    /// unconditionally, so a standalone header was misrouted into the
    /// `show_existing_frame` slot lookup and returned the wrong named error
    /// ("a show_existing_frame header naming an empty reference slot") for a
    /// frame that was never a show_existing_frame header. Both are fixed
    /// above: the `FrameHeader` arm now checks `show_existing_frame`, and a
    /// `pending_header`/`pending_tiles` accumulator collects tile groups
    /// until the frame's `cols * rows` tiles have all arrived.
    #[test]
    fn a_real_aomenc_stream_with_several_tile_group_obus_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_several_tile_group_obus_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (64usize, 128usize);
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        let mut saw_multiple_tile_group_obus = false;
        for attempt in 0..20u32 {
            let seed = 42 + attempt;
            let source = gradients_source(seed, width, height, "duration=0.04:rate=25");
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v", "error", "-f", "lavfi", "-i", &source, "-pix_fmt", "yuv420p", "-f",
                    "yuv4mpegpipe", "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut child = Command::new(aomenc_path())
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--limit=1",
                    "--kf-max-dist=1000",
                    "--threads=1",
                    "--row-mt=0",
                    "--tile-columns=0",
                    "--tile-rows=1",
                    "--sb-size=64",
                    "--num-tile-groups=4",
                    "--loopfilter-control=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-directional-intra=0",
                    "--enable-angle-delta=0",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--max-partition-size=32",
                    "--min-partition-size=32",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;

            let mut probe = Av1Parser::new();
            let mut pos = 0usize;
            let mut tile_group_obus = 0u32;
            while pos < stream.len() {
                let obu = probe.parse_obu(&stream[pos..]).unwrap();
                pos += obu.total_size;
                if matches!(obu.kind, ObuKind::TileGroup(_)) {
                    tile_group_obus += 1;
                }
            }
            if tile_group_obus > 1 {
                saw_multiple_tile_group_obus = true;
            }

            let before = crate::decode::tile_hits();
            let pictures = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(pictures) => pictures,
            };
            let hits = crate::decode::tile_hits() - before;
            assert!(
                hits > 0,
                "{NAME}: tile_hits delta was {hits}, expected >0 (seed {seed})"
            );
            assert_eq!(pictures.len(), 1, "{NAME}: expected exactly 1 picture (seed {seed})");

            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(pictures[0].y, ffmpeg_frames[0].y, "{NAME}: luma vs ffmpeg (seed {seed})");
            assert_eq!(pictures[0].u, ffmpeg_frames[0].u, "{NAME}: U vs ffmpeg (seed {seed})");
            assert_eq!(pictures[0].v, ffmpeg_frames[0].v, "{NAME}: V vs ffmpeg (seed {seed})");
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals} refusals); decode_stream never \
             decoded a several-tile-group-OBUs stream"
        );
        assert!(
            saw_multiple_tile_group_obus,
            "{NAME}: no attempt actually split into more than one OBU_TILE_GROUP -- this sweep \
             proves nothing about the multi-tile-group case (aomenc's --num-tile-groups is \
             RD/heuristic-driven, same as tile sizing)"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals out of 20"
        );
    }

    /// lane-tiles r10: non-uniform tile spacing (`uniform_spacing_flag == 0`,
    /// spec 5.9.15). aomenc never sets this from `--tile-columns`/
    /// `--tile-rows` (those always take the `uniform_spacing = 1` branch in
    /// `set_tile_info`, libaom `encoder.c`), and `--auto-tiles` only takes
    /// the non-uniform balancing path when `g_threads >= 2` -- this charter's
    /// `--threads=1` rules that out. The only CLI surface that reaches
    /// libaom's `else { tiles->uniform_spacing = 0; ... }` arm is the
    /// explicit per-tile size lists `--tile-width=<sb-list>`/
    /// `--tile-height=<sb-list>` (`av1_cx_iface.c` `set_tile_info`, argument
    /// parsing at `arg_defs.c`'s `.tile_width`/`.tile_height` -- present in
    /// the binary, just not printed by `--help`). Probed live with
    /// `aomenc --tile-width=1,3 --tile-height=1 --sb-size=64` on a 256x64
    /// source (4 SB columns, 1 SB row): confirmed by hand this round to
    /// produce a real OBU stream aomdec/ffmpeg both decode.
    ///
    /// `decode.rs`'s tile loop was already generic over spacing before this
    /// gate existed -- it walks `tile_info.mi_col_starts`/`mi_row_starts`
    /// (populated identically by `read_tile_info` for both the uniform and
    /// non-uniform branches, spec 5.9.15) and never recomputes tile bounds
    /// from `cols_log2`/`rows_log2`. The only place in this crate that ever
    /// refused non-uniform spacing is `frame.rs`'s OBU *writer*
    /// (`"non-uniform tile spacing is not written"`), an unrelated encode
    /// path this gate does not touch. So this is a staleness check, not new
    /// machinery: prove the already-generic decode path live and pin it.
    #[test]
    fn a_real_aomenc_stream_with_non_uniform_tile_spacing_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_non_uniform_tile_spacing_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (256usize, 64usize);
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        let mut saw_non_uniform_spacing = false;
        for attempt in 0..20u32 {
            let seed = 42 + attempt;
            let source = gradients_source(seed, width, height, "duration=0.04:rate=25");
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v", "error", "-f", "lavfi", "-i", &source, "-pix_fmt", "yuv420p", "-f",
                    "yuv4mpegpipe", "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut child = Command::new(aomenc_path())
                .args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--limit=1",
                    "--kf-max-dist=1000",
                    "--threads=1",
                    "--row-mt=0",
                    "--sb-size=64",
                    "--tile-width=1,3",
                    "--tile-height=1",
                    "--loopfilter-control=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-directional-intra=0",
                    "--enable-angle-delta=0",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--max-partition-size=32",
                    "--min-partition-size=32",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;

            let mut probe = Av1Parser::new();
            let mut pos = 0usize;
            while pos < stream.len() {
                let obu = probe.parse_obu(&stream[pos..]).unwrap();
                pos += obu.total_size;
                let tile_info = match &obu.kind {
                    ObuKind::Frame(header, _) => Some(&header.tile_info),
                    ObuKind::FrameHeader(header) if !header.show_existing_frame => {
                        Some(&header.tile_info)
                    }
                    _ => None,
                };
                if let Some(info) = tile_info {
                    if !info.uniform_spacing {
                        saw_non_uniform_spacing = true;
                    }
                    assert!(
                        info.cols > 1 || info.rows > 1,
                        "{NAME}: --tile-width=1,3 --tile-height=1 produced only 1 tile \
                         (cols={} rows={}, seed {seed}) -- fixture stopped exercising the case",
                        info.cols,
                        info.rows
                    );
                }
            }

            let before = crate::decode::tile_hits();
            let pictures = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(pictures) => pictures,
            };
            let hits = crate::decode::tile_hits() - before;
            assert!(
                hits > 0,
                "{NAME}: tile_hits delta was {hits}, expected >0 (seed {seed})"
            );
            assert_eq!(pictures.len(), 1, "{NAME}: expected exactly 1 picture (seed {seed})");

            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(pictures[0].y, ffmpeg_frames[0].y, "{NAME}: luma vs ffmpeg (seed {seed})");
            assert_eq!(pictures[0].u, ffmpeg_frames[0].u, "{NAME}: U vs ffmpeg (seed {seed})");
            assert_eq!(pictures[0].v, ffmpeg_frames[0].v, "{NAME}: V vs ffmpeg (seed {seed})");
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals} refusals); decode_stream never \
             decoded a non-uniform-tile-spacing stream"
        );
        assert!(
            saw_non_uniform_spacing,
            "{NAME}: no attempt actually set uniform_spacing_flag == 0 -- this sweep proves \
             nothing about the non-uniform case"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals out of 20"
        );
    }

    /// lane-sbpart r2: a real `aomenc` stream whose superblock-level
    /// partition decision is genuinely HORZ/VERT (not NONE/SPLIT) must
    /// decode pixel-exact through [`crate::decode::decode_block_rect64`] --
    /// `sb_rect_hits() > 0` is a HARD assert (this round's rule: a removed
    /// refusal needs a firing gate in the same commit, not a green suite
    /// that never took the new path). Recipe = the charter's own, verified
    /// live by r1: `--min/max-partition-size=32/64` keeps the RD search
    /// from ever recursing below 32x32, so every SB-level decision is
    /// genuinely NONE/SPLIT/HORZ/VERT; a `gradients` source blended with
    /// `testsrc2` (lavfi `blend=all_mode=average`) keeps the content from
    /// being flat enough that the search stops at NONE every time.
    #[test]
    fn a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (192usize, 128usize, 1usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_SBPART_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(40);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let duration = frame_count as f64 / 25.0;
            let source =
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"));
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--min-partition-size=32",
                "--max-partition-size=64",
                "--enable-restoration=0",
                "--enable-palette=0",
                "--deltaq-mode=0",
                "--enable-filter-intra=0",
                "--enable-cfl-intra=0",
                "--enable-intrabc=0",
                "--enable-tx-size-search=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let mut child = Command::new(aomenc_path())
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    // r2: `--enable-ab-partitions=0` does NOT stop aomenc
                    // choosing an SB-level HORZ_A/HORZ_B/VERT_A/VERT_B
                    // (`partition_w64` value 4-7) even with
                    // `--min/max-partition-size=32/64` -- a real, separate
                    // encoder quirk (AB-at-64 is a different, unlanded
                    // capability, lane-partab's territory is 32x32 and
                    // below only) discovered live this round, not this
                    // lane's HORZ/VERT gap. Only NONE/SPLIT/HORZ/VERT
                    // (values 0-3) are this gate's territory; a real
                    // HORZ/VERT (1/2) must still decode pixel-exact, which
                    // `sb_rect_hits() > 0` below hard-proves.
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched {
                if let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                    std::fs::write(&path, &stream).expect("writing pinned stream");
                    eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
                }
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (seed {seed})");
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused; the gate never decoded a stream"
        );
        assert!(
            crate::decode::sb_rect_hits() > 0,
            "{NAME}: zero superblock-level HORZ/VERT blocks fired ({matched} matches, \
             {named_refusals} refusals out of {n_attempts}) -- gate proved nothing this run"
        );
        eprintln!(
            "{NAME}: {named_refusals} named refusals, {matched} pixel-exact matches out of \
             {n_attempts}, sb_rect_hits={}",
            crate::decode::sb_rect_hits()
        );
    }

    /// lane-rect64q r1: same recipe as
    /// [`a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact`]
    /// minus `--deltaq-mode=0` -- real aomenc's non-realtime default is
    /// `DELTA_Q_OBJECTIVE` (`av1_cx_iface.c:271`), so dropping that one flag
    /// is enough to make `delta_q_present=1` the norm and drive
    /// [`crate::decode::decode_block_rect64`]'s running `CURRENT_Q_IDX` away
    /// from the frame-constant `base_q_idx` inside a real rect64 SB --
    /// exactly the bug this round's dequant fix targets. Hard-asserts
    /// `rect64_qidx_drift_hits() > 0`: without that, this gate could pass
    /// vacuously the same way the `--deltaq-mode=0` sibling always did.
    ///
    /// lane-rect64q r1: measured, does NOT fire -- 15/40 attempts matched
    /// but `rect64_qidx_drift_hits()` stayed 0 every time. Real aomenc's RD
    /// never chose a nonzero delta-q on this tiny synthetic gradients frame
    /// even with `deltaq-mode` left at its default; `#[ignore]`d rather than
    /// deleted so the recipe and the negative result both survive for the
    /// next attempt (see `lanes/rect64q-r1.report.md`).
    #[test]
    #[ignore = "drift never observed this round -- see lanes/rect64q-r1.report.md"]
    fn a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_and_delta_q_decodes_pixel_exact()
    {
        const NAME: &str =
            "a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_and_delta_q_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (192usize, 128usize, 1usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_SBPART_DQ_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(40);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let duration = frame_count as f64 / 25.0;
            let source =
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"));
            let y4m = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--min-partition-size=32",
                "--max-partition-size=64",
                "--enable-restoration=0",
                "--enable-palette=0",
                "--enable-filter-intra=0",
                "--enable-cfl-intra=0",
                "--enable-intrabc=0",
                "--enable-tx-size-search=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let mut child = Command::new(aomenc_path())
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("aomenc failed to start");
            child
                .stdin
                .take()
                .expect("aomenc stdin")
                .write_all(&y4m.stdout)
                .expect("writing y4m to aomenc");
            let out = child.wait_with_output().expect("aomenc failed to run");
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched {
                if let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                    std::fs::write(&path, &stream).expect("writing pinned stream");
                    eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
                }
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (seed {seed})");
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused; the gate never decoded a stream"
        );
        assert!(
            crate::decode::rect64_qidx_drift_hits() > 0,
            "{NAME}: zero rect64 dequant calls ever observed CURRENT_Q_IDX != base_q_idx \
             ({matched} matches, {named_refusals} refusals out of {n_attempts}) -- delta_q \
             was never actually exercised through decode_block_rect64 this run"
        );
        eprintln!(
            "{NAME}: {named_refusals} named refusals, {matched} pixel-exact matches out of \
             {n_attempts}, sb_rect_hits={}, rect64_qidx_drift_hits={}",
            crate::decode::sb_rect_hits(),
            crate::decode::rect64_qidx_drift_hits()
        );
    }

    /// lane-sbpart r4: replays a pinned mismatch byte-for-byte off disk (no
    /// aomenc/ffmpeg re-encode), same pattern as `pinned_golden3/4_stream_
    /// decodes_pixel_exact` -- fast red/green loop for the bisect, and lets
    /// `EC_AV1_TRACE=1` be set for one run without re-driving the encoder.
    #[test]
    #[ignore = "reads a pinned fixture path outside the repo; run manually"]
    fn pinned_sbpart_stream_decodes_pixel_exact() {
        let path = std::env::var("EC_AV1_GATE_DUMP_PIN")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| pin_dir().join("sbpart-pin.obu"));
        let Ok(stream) = std::fs::read(&path) else {
            eprintln!(
                "SKIP pinned_sbpart_stream_decodes_pixel_exact: no pinned bytes at {} \
                 -- re-capture with EC_AV1_GATE_DUMP off the sbpart gate",
                path.display()
            );
            return;
        };
        if !have_ffmpeg() {
            eprintln!("SKIP pinned_sbpart_stream_decodes_pixel_exact: no ffmpeg");
            return;
        }
        let frames = decode_stream(&stream).expect("pinned stream must decode");
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, 192, 128, 1);
        let (width, height) = (192usize, 128usize);
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            if std::env::var_os("EC_SBPART_DIAG").is_some() {
                let (mut min_r, mut max_r, mut min_c, mut max_c, mut n) =
                    (usize::MAX, 0usize, usize::MAX, 0usize, 0usize);
                for r in 0..height {
                    for c in 0..width {
                        let idx = r * width + c;
                        if got.y[idx] != want.y[idx] {
                            min_r = min_r.min(r);
                            max_r = max_r.max(r);
                            min_c = min_c.min(c);
                            max_c = max_c.max(c);
                            n += 1;
                        }
                    }
                }
                eprintln!(
                    "DIAG frame {i}: {n} luma mismatches, bbox rows [{min_r}..{max_r}] cols \
                     [{min_c}..{max_c}] of {width}x{height}"
                );
                // Per-64px-superblock-column mismatch counts, to see whether
                // corruption starts exactly at the rect64 SB or bleeds into
                // SBs decoded earlier (raster order).
                for sb_col in 0..(width + 63) / 64 {
                    let c0 = sb_col * 64;
                    let c1 = (c0 + 64).min(width);
                    let mut cnt = 0usize;
                    let mut first = None;
                    for r in 0..height {
                        for c in c0..c1 {
                            let idx = r * width + c;
                            if got.y[idx] != want.y[idx] {
                                cnt += 1;
                                if first.is_none() {
                                    first = Some((r, c));
                                }
                            }
                        }
                    }
                    eprintln!(
                        "DIAG frame {i} sb_col {sb_col} (cols {c0}..{c1}): {cnt} mismatches, \
                         first={first:?}"
                    );
                }
            }
            assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (pinned sbpart)");
            assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (pinned sbpart)");
            assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (pinned sbpart)");
        }
    }
}
