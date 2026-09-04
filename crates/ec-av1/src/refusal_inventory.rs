//! An inventory of what this decoder refuses, and a check on the claims those
//! refusals make.
//!
//! Two rules this repository keeps learning the hard way:
//!
//! 1. A refusal string is a **claim**. "This encoder never writes one" asserts
//!    that a real encoder cannot emit the case -- and one such string was
//!    disproved by the very gate that carried it (the wedge-interintra hang,
//!    2026-08-30): the refusal was written from a mismatching frame's header
//!    state, so it named a correlate rather than the defect. Any refusal
//!    phrased as a capability claim about the ENCODER, rather than about this
//!    decoder, needs evidence, and this module makes the set of such strings
//!    explicit instead of letting it grow quietly.
//! 2. Refusals accumulate silently. A lane that adds one to unblock itself is
//!    doing the right thing (refuse by name rather than desync), but the total
//!    is the distance to a decoder that handles a default-settings stream, and
//!    nobody sees a total that nothing prints.
//!
//! So the test below re-derives every `unsupported(...)` reason reachable on
//! the decode path -- `decode.rs` and `stream.rs` -- and pins it. Adding or
//! removing a refusal fails until the list is updated, which is the point: the
//! diff of this file is the decoder's capability delta, in one place, in
//! English.

#[cfg(test)]
const CAPABILITY_CLAIMS: &[&str] = &[
    "filter intra on a superblock-level HORZ/VERT strip (never expected -- av1_filter_intra_allowed_bsize caps at 32x32)",
];

#[cfg(test)]
const REFUSALS: &[&str] = &[
    "a coded HORZ/VERT strip whose chroma transform has no rect coefficient tables here",
    "a split intra strip whose transform unit is {tx_w}x{tx_h} (no luma coefficient tables for that shape here)",
    "an OBMC neighbour whose switchable interp filter was never recorded",
    "a sub-8x8 inter block under a scaled reference (superres, unimplemented)",
    "a rectangular inter luma transform unit whose shape has no coefficient table set here",
    "a rectangular inter chroma transform unit whose shape has no coefficient table set here",
    "a rectangular transform unit whose shape has no coefficient scan table here",
    "a 32x32 partition type this decoder does not code (value={part32})",
    "a Golomb tail longer than this decoder reads",
    "a tx_type symbol outside its CDF's own set: {t}",
    "an INTER 32x32 partition type this decoder does not code (value={part32})",
    "a superblock-level partition value outside PARTITION_NONE..PARTITION_VERT_4",
    "a 128x128 superblock partition value outside the 8-symbol alphabet",
    "an inter var-tx tree with a leaf transform larger than 64x64",
    "CfL, filter intra or a palette on a 128-root HORZ/VERT intra block (every one of their size gates caps at 64x64 or below, so none of these symbols exists there)",
    "intrabc under a 128x128 superblock (libaom's av1_is_dv_valid derives the block-vector delay from sb_size, which this decoder hardcodes to 64)",
    "a palette block with a real transform on a superblock-level HORZ/VERT strip (corner-cropped luma coefficients not ported for palette)",
    // lane-t900 r23: palette RECONSTRUCTION now covers the key-frame paths
    // (earlier lanes), the intra-in-inter square/rect paths (r22) and the
    // 8x8 LEAF inside an inter frame (this round, gate
    // `a_screen_stream_with_palette_8x8_leaves_in_inter_frames_decodes_pixel_exact`),
    // which is why the (UV) string is gone from the decoder entirely -- the
    // leaf was its only site. The (Y) one survives ONLY as the defensive
    // `palette.is_none()` guard in `read_intra_mode`/`read_intra_mode_rect`;
    // every caller now passes a real ctx/cache pair, so no stream reaches it.
    "a block that actually uses a palette (Y) -- reconstruction is out of scope",
    "intra block copy on a HORZ/VERT/1:4 rect intra strip (reconstruction is not ported at this shape)",
    "a sub-8x8 leaf that uses intrabc (this reader has no block-vector path; the 8x8-and-up reader reconstructs one)",
    "an intrabc block under TxMode::Select (its transform size is coded by the inter var-tx partition tree, which this decoder never reads)",
    "a bit depth of 12 (this decoder is gated at 8 and 10 only: warp/MC/wiener rounding shifts change at 12-bit and no 12-bit gate exists)",
    "a frame OBU with no tile group",
    "a frame naming primary_ref_frame at a reference slot with no saved CDF state",
    "a frame with no mode-info grid",
    "a frame whose segmentation enables SEG_LVL_REF_FRAME/SKIP/GLOBALMV (this decoder reads segment_id but never lets a segment override a block's reference, skip or mode)",
    // Still live, but NARROWED: lane-rectres r1 added the 32x32-level 1:4
    // strips (32x8 / 8x32) to `rect_inter_residual_supported` -- THE shape both
    // 10-bit 3840x1608 film cuts and every measured 1080p offset stopped on
    // (gate `a_real_aomenc_inter_sequence_with_32x32_level_1to4_strips_codes_\
    // their_rect_residual`). The string stays because the shapes below 8x8 and
    // the AB-partition footprints still have no rectangular residual path.
    "a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding",
    "a reference frame selected with no picture at this frame's own ref_frame_idx slot for it",
    "a reference picture whose height does not match this frame's own true size",
    "a show_existing_frame header naming an empty reference slot",
    "an inter var-tx tree with a leaf transform larger than 32x32",
    // lane-intrainter r1 lifted the >=16x16 square case (per-TU intra
    // prediction + coefficients, gate
    // `a_real_aomenc_inter_sequence_with_a_split_transform_intra_block_decodes_pixel_exact`);
    // lane-leaf8tx r4 lifted the 8x8 leaf's TX_4X4 2x2 grid (gate
    // `a_real_aomenc_inter_sequence_with_an_angle_delta_8x8_intra_leaf_decodes_pixel_exact`).
    "an inter frame with no key frame before it",
    // lane-inter16ab r1 lifted the AB half and r2 the 1:4 half (four 16x4 /
    // 4x16 inter strips, their 8x4/4x8 chroma pair built from BOTH strips'
    // motion vectors -- gate
    // `a_real_aomenc_inter_sequence_with_16x16_level_1to4_partitions_decodes_pixel_exact`).
    // Two narrower refusals are what is left of that shape:
    "an inter 16x16-level partition value outside NONE/HORZ/VERT/SPLIT/AB/1:4",
    // r2 residue: `vartx_leaves` is a list of SQUARE units, and
    // `sub_tx_size_map[TX_16X4]` is the rectangular TX_8X4 -- the split tree
    // and its per-unit residual reader both need rect leaves.
    // r2 residue: `decode_intra_rect_in_inter` codes chroma at the strip's own
    // halved footprint, which for a 16x4 strip is an 8x2 transform libaom
    // never wrote (its chroma is the PAIR's 8x4, coded by the odd strip).
    // lane-sub8x4 r3 narrowed this to "a strip with no chroma-pair record";
    // lane-wit16x4 r1 put it back at FULL width (r3's witness counters were
    // phantoms of a desync, class `refusal-lifted-without-a-gate`) behind
    // an opt-in. lane-t900 r6 NARROWED it again -- the opt-in is
    // gone -- on a real 10-bit 1920x792 128-superblock film witness that
    // decodes 14/14 decode-order frames exact vs aomdec and 12/12 shown frames
    // exact vs ffmpeg while the arm fires 78 16x4 + 149 4x16 (108 chroma-
    // paired): gate
    // `a_10bit_128sb_film_frames_with_warp_cdef_and_interintra_decode_pixel_exact`.
    // What still refuses is a 16x4/4x16 strip with NO chroma-pair record and
    // every other sub-8 shape.
    "an intra 16x4/4x16 strip inside an inter 16x16-level 1:4 partition (its 4:2:0 chroma pair is coded once for two strips; only the inter path implements that pairing)",
    "an intra mode this decoder does not code (round 2)",
    // lane-intrarect r1 lifted the whole-shape refusal that stood here (the
    // inter path's intra arm now routes 2:1 strips through `decode_rect_split`);
    // what is left is the 1:4 shape, whose `bsize_to_tx_size_cat` breaks the
    // size-group/category diagonal the 2:1 shapes share.
    "an intra-coded {bw}x{bh} block on the inter block path (no size-group/tx-category row for that shape here)",
    // lane-t900 r21 NARROWED this: the string used to guard EVERY rect intra
    // strip of a screen-content inter frame, and a real
    // `--tune-content=screen` stream reached it on an 8x16 (8-bit) / 16x8
    // (10-bit) strip. The 2:1 reader now codes the palette syntax itself and
    // the 1:4 arm goes through `read_intra_mode_rect`, which already did --
    // both arms of gate
    // `a_real_aomenc_screen_inter_sequence_codes_palette_syntax_on_rect_intra_strips`
    // decode ten frames pixel-exact with the strips counted. What still
    // lane-t900 r22 NARROWED it again: the 16x4/4x16 arm is LIFTED (that arm
    // routes through `decode_rect4_16_strip`, which reads the palette syntax
    // itself -- the premise was stale), gated by
    // `a_screen_stream_with_16x4_intra_strips_in_inter_frames_decodes_pixel_exact`.
    // What still refuses is the 128-root half of a screen-content frame, whose
    // body (`decode_block_128rect`) reads no palette syntax and for which no
    // `--sb-size=128` screen witness exists yet.
    // lane-t900 r23 CENSUS for this string's remaining 128-root arm (the
    // 16x4/4x16 arm was lifted in r22): its premise is already known to be
    // false by the spec -- `read_palette_mode_info` (5.11.46) codes palette
    // only for `Block_Width <= 64 && Block_Height <= 64`, so a 128x64/64x128
    // half consumes NO palette syntax and the screen flag changes nothing
    // about it. It stays because nothing witnesses it: 6 aomenc encodes at
    // `--sb-size=128 --tune-content=screen` (704x320 = 5*128+64 by 2*128+64,
    // the edge-forced size r18 proved for the non-screen twin; screen cell
    // content at cq 10/20/40, and r18's own panning-texture-plus-fresh-ramp
    // shape with `--max-partition-size=128 --enable-ab-partitions=0
    // --enable-1to4-partitions=0`) code ZERO 128-root halves of any kind
    // (`intra128_in_inter` and `sb128_rect` both 0 in every stream), so
    // lifting it would be a capability claim no gate exercises (class
    // `refusal-lifted-without-a-gate`). What is still untried: a 10-bit
    // source, and screen content in the forced bands with a NON-screen
    // interior.
    "a HORZ/VERT intra strip in a screen-content frame (palette syntax is consumed for square blocks only)",
    "warp prediction with a scaled reference (superres, unimplemented)",
    "an 8x8 partition leaf under a scaled reference (superres, unimplemented)",
    "a motion_mode symbol for a block shape with no CDF row here",
];

/// The refusals that have a PROVING test, and its name.
///
/// A refusal with no such test is an unmeasured claim: nothing in the suite
/// distinguishes "this case never happens" from "no stream we decode has
/// reached it yet". A proving test is one of two shapes:
///
/// * a WITNESS gate -- a real stream reaches the shape, decodes exact, and the
///   refusal is gone (the good case), or
/// * a CENSUS gate -- N real streams decode with the refusal firing 0 times
///   while the counters for the sibling shapes it names fire, so the claim is
///   measured against the domain the decoder is actually handed.
///
/// The count printed by [`tests::every_proven_refusal_names_a_test_that_exists`]
/// is the honest numerator over the inventory below.
#[cfg(test)]
const PROVEN: &[(&str, &str)] = &[
    // lane-t900 r20, census: three real streams present exactly the 18
    // `(side, write_w, write_h)` triples `INTER_BLOCK_SHAPES` lists, and
    // `rect_inter_residual_supported` covers every rectangular one -- the only
    // footprints `reject_residual` is set for.
    (
        "a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding",
        "a_block_shape_census_over_three_real_streams_leaves_the_rect_residual_refusal_unreachable",
    ),
    // lane-t900 r20, census: the shapes that can reach `decode_intra_rect_in_inter`'s
    // size-group/tx-category lookup are the ten rectangular census shapes with
    // `8 <= min` and `max <= 64` (a sub-8 footprint and a 128-px one take
    // earlier branches), and every one of them has a row.
    (
        "an intra-coded {bw}x{bh} block on the inter block path (no size-group/tx-category row for that shape here)",
        "every_intra_in_inter_shape_the_census_lists_has_a_size_group_row",
    ),
    // lane-t900 r20, census: the only sub-8 footprints three real streams
    // present are the 16x4/4x16 strips of a 16x16-level 1:4 partition, whose
    // caller records the chroma pair for every strip, while the supported path
    // fires on both orientations inside streams that decode with no refusal.
    (
        "an intra 16x4/4x16 strip inside an inter 16x16-level 1:4 partition (its 4:2:0 chroma pair is coded once for two strips; only the inter path implements that pairing)",
        "a_sub8_footprint_census_over_real_streams_leaves_the_intra_16x4_pairing_refusal_unreachable",
    ),
    // lane-t900 r20, census: the rect transform tables and scans, enumerated
    // over the shapes the measured block-shape domain hands each helper (block
    // footprint, 4:2:0 chroma unit, rect var-tx leaf, clamped coded corner).
    (
        "a rectangular inter luma transform unit whose shape has no coefficient table set here",
        "every_rect_transform_shape_the_census_lists_has_a_coefficient_table_and_scan",
    ),
    (
        "a rectangular inter chroma transform unit whose shape has no coefficient table set here",
        "every_rect_transform_shape_the_census_lists_has_a_coefficient_table_and_scan",
    ),
    (
        "a rectangular transform unit whose shape has no coefficient scan table here",
        "every_rect_transform_shape_the_census_lists_has_a_coefficient_table_and_scan",
    ),
    // lane-t900 r21, enumeration: a var-tx leaf is never larger than the unit
    // the tree was entered with, and both callers enter at or below their own
    // ceiling (the one case that does not, a 64px block at TX_64X64, is the
    // `single` whole-block case the refusal is guarded by).
    (
        "an inter var-tx tree with a leaf transform larger than 32x32",
        "a_var_tx_tree_never_presents_a_leaf_larger_than_the_unit_it_entered",
    ),
    (
        "an inter var-tx tree with a leaf transform larger than 64x64",
        "a_var_tx_tree_never_presents_a_leaf_larger_than_the_unit_it_entered",
    ),
    // Pre-existing proofs, registered by lane-t900 r21: a negative gate (a
    // hand-built 12-bit sequence header is refused BY THIS EXACT STRING rather
    // than decoded wrong) and a witness gate (a real aomenc screen key frame
    // reaches intrabc on a rect strip, so the refusal names a shape a real
    // encoder does write).
    // lane-t900 r21, enumeration: `partition_w32` is a ten-symbol CDF and
    // `partition_w128` an eight-symbol one, and every `match` carrying one of
    // these three fallbacks has an arm for each value of its own alphabet, so
    // no stream can reach them.
    (
        "a 32x32 partition type this decoder does not code (value={part32})",
        "every_partition_value_of_an_enumerated_alphabet_has_an_arm",
    ),
    (
        "an INTER 32x32 partition type this decoder does not code (value={part32})",
        "every_partition_value_of_an_enumerated_alphabet_has_an_arm",
    ),
    (
        "a 128x128 superblock partition value outside the 8-symbol alphabet",
        "every_partition_value_of_an_enumerated_alphabet_has_an_arm",
    ),
    // lane-t900 r24, enumeration: the key-frame superblock root's own `match`
    // has an arm for all ten `partition_w64` values.
    (
        "a superblock-level partition value outside PARTITION_NONE..PARTITION_VERT_4",
        "every_partition_value_of_an_enumerated_alphabet_has_an_arm",
    ),
    // lane-t900 r24, enumeration: the inter 16x16-level if/else chain's
    // branches name all ten `partition_w16` values between them.
    (
        "an inter 16x16-level partition value outside NONE/HORZ/VERT/SPLIT/AB/1:4",
        "every_partition_value_of_an_if_chain_alphabet_is_named_by_a_branch",
    ),
    // lane-t900 r24, enumeration: every (CDF width, symbol) pair a tx_type row
    // can present maps to a distinct member of that width's own set.
    (
        "a tx_type symbol outside its CDF's own set: {t}",
        "every_tx_type_symbol_of_every_cdf_width_maps_into_its_own_set",
    ),
    // lane-t900 r24, enumeration: the reader covers spec 5.11.40's whole value
    // domain (`0..=(1 << 20) - 2`, both ends of every prefix length), so the
    // refusal is outside it. What it still names is a bit pattern no encoder
    // writes -- a 20th prefix bit of 0, which the spec's `length == 20` break
    // makes a don't-care.
    (
        "a Golomb tail longer than this decoder reads",
        "read_golomb_reads_every_value_a_conformant_stream_can_carry",
    ),
    // lane-t900 r25, enumeration: `parse_tile_group` is the only producer of
    // the `tiles` vector `decode_stream` tests, and its success path always
    // pushes at least one `Tile` -- swept over a real frame OBU's tile payload
    // truncated from empty upwards.
    (
        "a frame OBU with no tile group",
        "a_frame_obu_that_parses_always_carries_at_least_one_tile",
    ),
    // lane-t900 r25, negative gates: hand-built streams that reach each
    // refusal by name and output no picture (a `show_existing_frame` header
    // over all 8 empty slots; a stream that opens on an inter frame).
    (
        "a show_existing_frame header naming an empty reference slot",
        "a_show_existing_frame_header_naming_an_empty_slot_is_refused_by_name",
    ),
    (
        "an inter frame with no key frame before it",
        "an_inter_frame_opening_a_stream_is_refused_by_name",
    ),
    // lane-t900 r25, enumeration: a motion_mode/obmc symbol is read only under
    // `is_motion_variation_allowed_bsize` (min side >= 8), and each of the 17
    // `BLOCK_SIZES_ALL` footprints that clause admits maps to a distinct row of
    // the 17-row table.
    (
        "a motion_mode symbol for a block shape with no CDF row here",
        "every_shape_that_allows_motion_variation_has_a_motion_mode_cdf_row",
    ),
    (
        "a bit depth of 12 (this decoder is gated at 8 and 10 only: warp/MC/wiener rounding shifts change at 12-bit and no 12-bit gate exists)",
        "a_twelve_bit_sequence_header_is_refused_by_name",
    ),
    (
        "intra block copy on a HORZ/VERT/1:4 rect intra strip (reconstruction is not ported at this shape)",
        "a_real_aomenc_screen_key_frame_reads_use_intrabc_on_rect_strips",
    ),
    // lane-t900 r26, enumeration: a y_mode symbol comes from a 13-symbol CDF
    // and all three guards refuse 13 and above.
    (
        "an intra mode this decoder does not code (round 2)",
        "every_intra_mode_symbol_of_the_y_mode_alphabet_is_coded",
    ),
    // lane-t900 r26, enumeration: `compute_image_size` is 0 only for a zero
    // frame width/height, which no path a header can code produces (swept
    // through the real reader per axis, superres division enumerated).
    (
        "a frame with no mode-info grid",
        "every_frame_size_a_header_can_code_has_a_mode_info_grid",
    ),
    // lane-t900 r26, negative gates: written streams that reach each refusal
    // by name (a 64x32 inter frame over a 64x64 reference; a stream opening on
    // a non-error-resilient inter frame whose primary_ref slot is empty, over
    // all 8 slots) with the picture/CDF slot invariant enumerated alongside.
    (
        "a reference picture whose height does not match this frame's own true size",
        "an_inter_frame_shorter_than_its_reference_is_refused_by_name",
    ),
    (
        "a frame naming primary_ref_frame at a reference slot with no saved CDF state",
        "an_inter_frame_naming_an_unrefreshed_primary_ref_slot_is_refused_by_name",
    ),
];

/// Gates whose `Err` arm turns a decode failure into a printed SKIP rather than
/// a test failure.
///
/// A gate named `..._decodes_pixel_exact` that cannot decode has failed, and
/// swallowing the error keeps the suite green while the gate proves nothing --
/// the same vacuum as a gate whose feature never fires, arrived at from the
/// other side. These four predate the attempt-loop pattern the newer gates use
/// (encode several fixtures, require at least one to decode, hard-assert the
/// firing count). They are pinned here so the set cannot grow while they are
/// converted one at a time.
#[cfg(test)]
const GATES_THAT_SKIP_ON_A_DECODE_ERROR: &[&str] = &[
    "a_real_aomenc_filter_intra_stream_decodes_pixel_exact",
    "a_real_aomenc_inter_sequence_with_deblocking_decodes_pixel_exact",
    "a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact",
    "a_real_libaom_gradients_stream_with_cdef_decodes_pixel_exact",
];

#[cfg(test)]
mod tests {
    use super::{CAPABILITY_CLAIMS, GATES_THAT_SKIP_ON_A_DECODE_ERROR, PROVEN, REFUSALS};
    use std::collections::BTreeSet;

    /// Every distinct `unsupported(...)` reason on the decode path.
    ///
    /// Both sources use the same shape -- `stream.rs` calls
    /// `Error::unsupported(context, reason)` and `decode.rs` a local
    /// `unsupported(reason)` helper -- so the reason is always the LAST string
    /// literal of the call, and Rust's `\` line continuations inside it are
    /// joined the way rustc joins them.
    fn decode_path_refusals() -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        for src in [include_str!("decode.rs"), include_str!("stream.rs")] {
            let bytes = src.as_bytes();
            for (start, _) in src.match_indices("unsupported(") {
                // `Error::unsupported(` and the bare helper both end here.
                let mut i = start + "unsupported(".len();
                // `unsupported(format!("..."))` is the same refusal, one layer in.
                if src[i..].starts_with("format!(") {
                    i += "format!(".len();
                }
                let mut last: Option<String> = None;
                loop {
                    while i < bytes.len() && (bytes[i] as char).is_whitespace() {
                        i += 1;
                    }
                    if i >= bytes.len() || bytes[i] != b'"' {
                        break;
                    }
                    i += 1;
                    let mut lit = String::new();
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            // A backslash-newline continuation drops the newline
                            // and the indentation that follows it.
                            if bytes.get(i + 1) == Some(&b'\n') {
                                i += 2;
                                while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
                                    i += 1;
                                }
                                continue;
                            }
                            lit.push(bytes[i] as char);
                            lit.push(bytes[i + 1] as char);
                            i += 2;
                            continue;
                        }
                        lit.push(bytes[i] as char);
                        i += 1;
                    }
                    i += 1;
                    last = Some(lit);
                    while i < bytes.len() && (bytes[i] as char).is_whitespace() {
                        i += 1;
                    }
                    if i < bytes.len() && bytes[i] == b',' {
                        i += 1;
                        continue;
                    }
                    break;
                }
                if let Some(reason) = last {
                    // `Error::unsupported`'s first argument is the context
                    // ("AV1 tile"), which is not a reason.
                    if reason != "AV1 tile" {
                        found.insert(reason);
                    }
                }
            }
        }
        found
    }

    #[test]
    fn the_decode_path_refuses_exactly_the_listed_cases() {
        let found = decode_path_refusals();
        let listed: BTreeSet<String> = CAPABILITY_CLAIMS
            .iter()
            .chain(REFUSALS)
            .map(|&s| s.to_owned())
            .collect();
        assert!(found.len() >= 30, "the refusal scan found only {} reasons -- it is broken, not the decoder", found.len());

        let added: Vec<&String> = found.difference(&listed).collect();
        assert!(
            added.is_empty(),
            "new decode-path refusals are not in the inventory: {added:#?}\n\
             Add each to REFUSALS -- or, if it claims something about what an ENCODER can \
             emit, to CAPABILITY_CLAIMS, which requires evidence that the case is genuinely \
             unreachable rather than merely unseen."
        );
        let removed: Vec<&String> = listed.difference(&found).collect();
        assert!(
            removed.is_empty(),
            "these refusals are listed but no longer in the decode path: {removed:#?}\n\
             Delete them from the inventory -- the capability landed, which is the good case."
        );
    }

    /// A refusal may say what THIS DECODER does not do. Saying what an encoder
    /// never emits is a claim about the world, and this repository has already
    /// shipped one that was false.
    #[test]
    fn capability_claims_are_declared_not_scattered() {
        let claimy: Vec<String> = decode_path_refusals()
            .into_iter()
            .filter(|r| r.contains("never writes") || r.contains("never expected"))
            .collect();
        let declared: BTreeSet<&str> = CAPABILITY_CLAIMS.iter().copied().collect();
        for reason in &claimy {
            assert!(
                declared.contains(reason.as_str()),
                "{reason:?} claims an encoder never emits this case but is not in \
                 CAPABILITY_CLAIMS. Such a claim needs a gate that proves the case is \
                 unreachable; until it has one, phrase the refusal as what this decoder \
                 does not decode."
            );
        }
    }

    /// No NEW gate may quietly turn a decode error into a skip.
    #[test]
    fn gates_that_swallow_a_decode_error_are_declared() {
        let src = include_str!("stream.rs");
        let mut found: BTreeSet<String> = BTreeSet::new();
        for (i, _) in src.match_indices("\"SKIP ") {
            let rest = &src[i + "\"SKIP ".len()..];
            let Some(colon) = rest.find(':') else { continue };
            let name = &rest[..colon];
            if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
                continue;
            }
            // The tail of the message says what was skipped. Only "{e}" -- the
            // decode error itself -- is the shape this test is about; "no
            // ffmpeg"/"no aomenc" are tool absence, governed by
            // EC_AV1_REQUIRE_AOMENC instead.
            let Some(end) = rest.find("\"") else { continue };
            if rest[colon..end].contains("{e}") {
                found.insert(name.to_owned());
            }
        }
        let listed: BTreeSet<&str> = GATES_THAT_SKIP_ON_A_DECODE_ERROR.iter().copied().collect();
        let added: Vec<&String> = found.iter().filter(|n| !listed.contains(n.as_str())).collect();
        assert!(
            added.is_empty(),
            "these gates turn a decode error into a printed SKIP and are not declared: \
             {added:#?}\nA gate that cannot decode has failed -- assert on the error, or use \
             the attempt-loop pattern (several fixtures, at least one must decode, firing count \
             hard-asserted)."
        );
        let stale: Vec<&&str> = listed.iter().filter(|n| !found.contains(**n)).collect();
        assert!(
            stale.is_empty(),
            "{stale:#?} no longer swallow a decode error -- delete them from the list."
        );
    }

    /// The partition alphabet, in CDF symbol order.
    const PARTITION_NAMES: [&str; 10] = [
        "PARTITION_NONE",
        "PARTITION_HORZ",
        "PARTITION_VERT",
        "PARTITION_SPLIT",
        "PARTITION_HORZ_A",
        "PARTITION_HORZ_B",
        "PARTITION_VERT_A",
        "PARTITION_VERT_B",
        "PARTITION_HORZ_4",
        "PARTITION_VERT_4",
    ];

    /// The partition values a `match` arm pattern -- or an `if` chain's
    /// condition -- names: its own tokens, plus every value of an inclusive
    /// range (`(PARTITION_HORZ_A..=PARTITION_VERT_B).contains(&p)` is four
    /// values in one). Tokenised, never `contains()`d: a misspelled constant
    /// in a match pattern is an irrefutable binding that compiles and swallows
    /// the alphabet, and a substring check would call that covered.
    fn partition_values_named(head: &str) -> BTreeSet<usize> {
        let mut out = BTreeSet::new();
        let idx: Vec<usize> = head
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .filter_map(|tok| PARTITION_NAMES.iter().position(|n| *n == tok))
            .collect();
        if head.contains("..=") && idx.len() == 2 {
            for v in idx[0]..=idx[1] {
                out.insert(v);
            }
        }
        out.extend(idx);
        out
    }

    /// The partition alphabets, enumerated against the arms that code them.
    ///
    /// A partition value is a CDF symbol, and `SymbolDecoder::symbol_fixed`
    /// cannot return one outside `0..nsyms` -- so a `match` whose arms cover
    /// its whole alphabet has an unreachable fallback, and the refusal that
    /// fallback carries names a value no stream can present. This test finds
    /// each such `match` by the refusal in its OWN fallback arm (rather than
    /// by line number, which drifts) and enumerates the alphabet against the
    /// arm patterns, ranges included.
    ///
    /// The refusals stay in the code: a partition value is a `usize`, so the
    /// match needs a fallback arm, and a named refusal is a better one than a
    /// panic (class `branch-dropped-as-unreachable`). What this removes is the
    /// UNMEASURED part of the claim.
    ///
    /// Not every partition fallback belongs here: the ones that name a shape
    /// this decoder genuinely does not code yet (the 128-root consumers) are
    /// live refusals, and the 16x16-level inter chain is an `if`/`else if`
    /// ladder rather than a match, so it needs its own enumerator.
    #[test]
    fn every_partition_value_of_an_enumerated_alphabet_has_an_arm() {
        // (the refusal in the fallback arm, the alphabet its CDF codes)
        let blocks: [(&str, usize); 4] = [
            ("a 32x32 partition type this decoder does not code", crate::cdf::PARTITION_W32[0].len() - 1),
            ("an INTER 32x32 partition type this decoder does not code", crate::cdf::PARTITION_W32[0].len() - 1),
            ("a 128x128 superblock partition value outside the 8-symbol alphabet", crate::cdf::PARTITION_W128[0].len() - 1),
            // lane-t900 r24: the key-frame superblock root. Its `part` is
            // either a `partition_w64` symbol or, at a frame edge, one of the
            // three gathered outcomes (PARTITION_HORZ, PARTITION_VERT,
            // PARTITION_SPLIT) -- all inside the same ten-value alphabet.
            (
                "a superblock-level partition value outside PARTITION_NONE..PARTITION_VERT_4",
                crate::cdf::PARTITION_W64[0].len() - 1,
            ),
        ];
        let src = include_str!("decode.rs");
        let lines: Vec<&str> = src.lines().collect();
        let indent = |l: &str| l.len() - l.trim_start().len();
        let mut found = 0usize;
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            if !(t.starts_with("match part") && t.ends_with('{')) {
                continue;
            }
            let outer = indent(line);
            let close = " ".repeat(outer) + "}";
            let (mut arms, mut fallback) = (BTreeSet::new(), String::new());
            let mut in_fallback = false;
            for l in &lines[i + 1..] {
                if *l == close {
                    break;
                }
                // Arm patterns sit exactly one level in; anything deeper is an
                // arm's body, nested matches and `part == PARTITION_*` reads
                // included.
                if indent(l) != outer + 4 && !l.trim().is_empty() {
                    if in_fallback {
                        fallback.push_str(l.trim());
                    }
                    continue;
                }
                if !l.contains("=>") {
                    continue;
                }
                in_fallback = l.trim_start().starts_with("_ =>");
                arms.extend(partition_values_named(l.split("=>").next().unwrap_or("")));
            }
            let Some((reason, alphabet)) =
                blocks.iter().find(|(reason, _)| fallback.contains(reason))
            else {
                continue;
            };
            found += 1;
            let missing: Vec<usize> = (0..*alphabet).filter(|v| !arms.contains(v)).collect();
            assert!(
                missing.is_empty(),
                "the `match` at decode.rs:{} refuses {reason:?} but has no arm for {:?} -- that \
                 is a real gap in its {alphabet}-value alphabet, not a dead refusal",
                i + 1,
                missing.iter().map(|v| PARTITION_NAMES[*v]).collect::<Vec<_>>()
            );
        }
        assert_eq!(
            found,
            blocks.len(),
            "not every enumerated partition refusal was located in decode.rs"
        );
    }

    /// The same enumeration for a partition refusal that sits in an `if`
    /// chain rather than a `match` fallback.
    ///
    /// The inter 16x16-level ladder tests `part16` against constants arm by
    /// arm and refuses in its second-to-last branch, with `PARTITION_SPLIT`
    /// left to the final `else`. Nothing about that shape is weaker than a
    /// `match`: `part16` is a `partition_w16` symbol (or, at a frame edge, a
    /// gathered value of the same alphabet), so if the chain's conditions
    /// between them name every value of that alphabet, the refusing branch is
    /// unreachable.
    ///
    /// The conditions are read from the source, tokenised (never
    /// substring-matched -- lane-t900 r21), and a condition carrying `&&` is
    /// rejected outright: a value it names is only conditionally handled, and
    /// counting it would overstate the coverage.
    #[test]
    fn every_partition_value_of_an_if_chain_alphabet_is_named_by_a_branch() {
        // (the refusal in the chain's refusing branch, the alphabet its CDF codes)
        let chains: [(&str, usize); 1] = [(
            "an inter 16x16-level partition value outside NONE/HORZ/VERT/SPLIT/AB/1:4",
            crate::cdf::PARTITION_W16[0].len() - 1,
        )];

        let src = include_str!("decode.rs");
        let lines: Vec<&str> = src.lines().collect();
        let indent = |l: &str| l.len() - l.trim_start().len();
        let mut found = 0usize;
        for (reason, alphabet) in chains {
            let refusal = lines
                .iter()
                .position(|l| l.contains(reason))
                .unwrap_or_else(|| panic!("{reason:?} is not in decode.rs"));
            // The branch that carries the refusal: the nearest `} else if`
            // above it. Its indentation is the chain's own.
            let head = (0..refusal)
                .rev()
                .find(|&i| lines[i].trim_start().starts_with("} else if "))
                .unwrap_or_else(|| panic!("{reason:?} is not inside an if/else chain"));
            let outer = indent(lines[head]);
            let var = lines[head].trim_start()["} else if ".len()..]
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_owned();
            assert!(!var.is_empty(), "the chain at decode.rs:{} tests nothing", head + 1);

            // Walk up collecting every branch head of this chain, each read to
            // the line that opens its body.
            let mut named = BTreeSet::new();
            let mut i = head + 1;
            let mut heads = 0usize;
            loop {
                i -= 1;
                let t = lines[i].trim_start();
                if indent(lines[i]) != outer || !(t.starts_with("} else if ") || t.starts_with("if "))
                {
                    if i == 0 {
                        break;
                    }
                    continue;
                }
                let mut cond = String::new();
                for l in &lines[i..] {
                    cond.push(' ');
                    cond.push_str(l.trim());
                    if l.trim_end().ends_with('{') {
                        break;
                    }
                }
                assert!(
                    cond.contains(&var),
                    "the branch at decode.rs:{} sits in the chain that refuses {reason:?} but \
                     does not test {var}",
                    i + 1
                );
                assert!(
                    !cond.contains("&&"),
                    "the branch at decode.rs:{} handles its partition values only conditionally \
                     ({cond:?}) -- the enumeration below would overstate what the chain covers",
                    i + 1
                );
                named.extend(partition_values_named(&cond));
                heads += 1;
                if t.starts_with("if ") {
                    break;
                }
                assert!(i > 0, "the chain that refuses {reason:?} has no start");
            }
            // The value the refusing branch excludes (`part16 != PARTITION_SPLIT`)
            // is handled by the chain's final `else`, so require one.
            let close = " ".repeat(outer) + "} else {";
            assert!(
                lines[refusal..].iter().any(|l| *l == close),
                "the chain that refuses {reason:?} has no final `else`, so the value its \
                 refusing branch excludes is handled nowhere"
            );
            assert!(heads >= 2, "only {heads} branch(es) found for {reason:?}");
            found += 1;
            let missing: Vec<usize> = (0..alphabet).filter(|v| !named.contains(v)).collect();
            assert!(
                missing.is_empty(),
                "the chain at decode.rs:{} refuses {reason:?} but no branch names {:?} -- that \
                 is a real gap in its {alphabet}-value alphabet, not a dead refusal",
                head + 1,
                missing.iter().map(|v| PARTITION_NAMES[*v]).collect::<Vec<_>>()
            );
        }
        assert_eq!(found, chains.len());
    }

    /// Every `tx_type` symbol of every CDF width, mapped through the set that
    /// width names.
    ///
    /// The reader decodes `t` from a row of `n` slots, so `t` is in
    /// `0..n - 1`, and hands `(n, t)` to [`crate::decode::tx_type_from_symbol`]
    /// -- the readers' own mapping, called here rather than transcribed
    /// (class `shared-oracle-blindness`: a second copy of a table can be
    /// self-consistently wrong). The widths are read from `cdf_state.rs`'s
    /// `tx_type` field declarations, so a new table with a width the mapping
    /// does not name fails this test rather than reaching the refusal at
    /// runtime.
    ///
    /// Distinctness is part of the claim: a set of `n - 1` symbols must map to
    /// `n - 1` DIFFERENT transform types, or the mapping has lost a member of
    /// the spec's `Tx_Type_*_Inv_Set*` even while every symbol resolves.
    #[test]
    fn every_tx_type_symbol_of_every_cdf_width_maps_into_its_own_set() {
        let mut widths: BTreeSet<usize> = BTreeSet::new();
        for line in include_str!("cdf_state.rs").lines() {
            let t = line.trim();
            if !(t.starts_with("pub ") && t.contains("tx_type") && t.contains("[u16;")) {
                continue;
            }
            let tail = t.rsplit("[u16;").next().unwrap_or("");
            let n: usize = tail
                .split(']')
                .next()
                .unwrap_or("")
                .trim()
                .parse()
                .unwrap_or_else(|_| panic!("cannot read a CDF width out of {t:?}"));
            widths.insert(n);
        }
        assert!(
            widths.len() >= 4,
            "the cdf_state.rs scan found only {widths:?} -- it is broken, not the decoder"
        );
        for &n in &widths {
            let nsyms = n - 1;
            let mut set: Vec<crate::transform::TxType> = Vec::new();
            for t in 0..nsyms {
                let ty = crate::decode::tx_type_from_symbol(n, t).unwrap_or_else(|| {
                    panic!(
                        "a {n}-slot tx_type CDF codes symbol {t}, which maps to no transform \
                         type -- the refusal \"a tx_type symbol outside its CDF's own set\" is \
                         reachable, and that is a mapping-table gap to fix"
                    )
                });
                assert!(
                    !set.contains(&ty),
                    "a {n}-slot tx_type CDF maps symbol {t} to {ty:?}, which another symbol of \
                     the same set already names -- a member of the spec's inverse table is lost"
                );
                set.push(ty);
            }
            assert_eq!(set.len(), nsyms);
        }
    }

    /// The Golomb tail, enumerated over every value a conformant stream can
    /// carry.
    ///
    /// Spec 5.11.40 `read_golomb` counts its unary prefix and BREAKS at
    /// `length == 20`, then reads `length - 1` payload bits into an `x` whose
    /// top bit is implicit -- so no conformant stream carries a value above
    /// `(1 << 20) - 2`, and spec 5.11.39 agrees from the other side by masking
    /// the level it feeds with `0xFFFFF`. This walks both ends of every
    /// prefix length 1..=20 (plus the small values every real stream uses)
    /// through [`crate::tile::write_golomb`] and back through the decoder's own
    /// [`crate::decode::read_golomb`], so the refusal "a Golomb tail longer
    /// than this decoder reads" is proved to sit strictly OUTSIDE the value
    /// domain, not inside it.
    ///
    /// The residue is a bit pattern, not a value: the spec's break makes the
    /// 20th prefix bit a don't-care, so a stream whose 20th prefix bit is 0
    /// decodes to the same value there and is refused here. Every encoder
    /// writes the terminating 1 (our own writer tops out at 19 zeros), and
    /// lifting the cap is blocked on the defect it currently masks -- see the
    /// comment on `read_golomb`.
    #[test]
    fn read_golomb_reads_every_value_a_conformant_stream_can_carry() {
        let roundtrip = |value: u32| -> ec_core::Result<u32> {
            let mut enc = crate::msac::SymbolEncoder::new();
            crate::tile::write_golomb(&mut enc, value);
            let data = enc.finish();
            let mut dec = crate::msac::SymbolDecoder::new(&data);
            crate::decode::read_golomb(&mut dec)
        };
        let mut values: BTreeSet<u32> = (0..=64u32).collect();
        for length in 1..=20u32 {
            // `x` has `length` bits with its top bit set, and the value is
            // `x - 1`: the two ends of what this prefix length can express.
            values.insert((1u32 << (length - 1)) - 1);
            values.insert(((1u64 << length) - 2) as u32);
        }
        let max = *values.iter().max().unwrap();
        assert_eq!(max, (1u32 << 20) - 2, "the enumeration misses the spec's own ceiling");
        // A coefficient is masked to 20 bits (spec 5.11.39), and the base and
        // base-range syntax contribute at most 15 of that before the tail, so
        // this is the largest tail a legal coefficient can need.
        assert!(max >= 0xF_FFFF - 15, "the ceiling is below the largest legal tail");
        for value in values {
            match roundtrip(value) {
                Ok(read) => assert_eq!(read, value, "the Golomb tail {value} read back as {read}"),
                Err(e) => panic!(
                    "the Golomb tail {value} is inside the domain spec 5.11.40 can write but \
                     this decoder refuses it: {e}"
                ),
            }
        }
        // And the refusal still names something: 20 zero prefix bits.
        let mut enc = crate::msac::SymbolEncoder::new();
        for _ in 0..20 {
            enc.literal(0, 1);
        }
        for _ in 0..20 {
            enc.literal(1, 1);
        }
        let data = enc.finish();
        let mut dec = crate::msac::SymbolDecoder::new(&data);
        assert!(
            crate::decode::read_golomb(&mut dec).is_err(),
            "the Golomb refusal is dead code -- drop it from the inventory"
        );
    }

    /// The intra-mode alphabet, enumerated against the guard that refuses a
    /// symbol outside it.
    ///
    /// Three readers -- the rect-in-inter arm, the intra-in-inter square arm
    /// and the 8x8 leaf -- read a y_mode symbol and refuse `mode >= 13` with
    /// "an intra mode this decoder does not code (round 2)". A `y_mode` symbol
    /// comes from `Cdfs::y_mode[size_group]`, whose alphabet is
    /// `Y_MODE[g].len() - 1` (the last slot is the adaptation counter), and
    /// `SymbolDecoder::symbol` cannot return a value outside `0..nsyms` -- so
    /// the guard is unreachable exactly when every size group's alphabet is at
    /// most the threshold.
    ///
    /// Found by the refusal in the guard's own body rather than by line
    /// number, and the CDF the symbol was read from is checked too: a
    /// threshold of 13 over a 14-symbol table would be a live refusal, and a
    /// site reading a WIDER alphabet (`uv_mode` with CfL is 14) under the same
    /// guard would be one as well.
    #[test]
    fn every_intra_mode_symbol_of_the_y_mode_alphabet_is_coded() {
        const REFUSAL: &str = "an intra mode this decoder does not code (round 2)";
        // Both uv_mode alphabets are listed so the CfL-allowed one (14
        // symbols, `UV_MODE_CFL_ALLOWED`) cannot silently become the source of
        // a symbol tested against 13.
        let alphabets: [(&str, usize); 3] = [
            ("y_mode", crate::cdf::Y_MODE[0].len() - 1),
            ("uv_mode_no_cfl", crate::cdf::UV_MODE_NO_CFL[0].len() - 1),
            ("uv_mode_cfl", crate::cdf::UV_MODE_CFL[0].len() - 1),
        ];
        assert_eq!(alphabets[0].1, 13, "the y_mode alphabet changed size");
        for group in &crate::cdf::Y_MODE {
            assert_eq!(group.len() - 1, alphabets[0].1, "the y_mode rows disagree on width");
        }

        let src = include_str!("decode.rs");
        let lines: Vec<&str> = src.lines().collect();
        let mut sites = 0usize;
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            let Some(rest) = t.strip_prefix("if mode >= ") else { continue };
            let Some(threshold) = rest.trim_end_matches(" {").parse::<usize>().ok() else {
                continue;
            };
            // Only the guards carrying THIS refusal.
            if !lines[i..(i + 4).min(lines.len())].join("\n").contains(REFUSAL) {
                continue;
            }
            sites += 1;
            // The symbol's own CDF: the nearest `dec.symbol(&mut cdfs.X[..])`
            // above the guard.
            let read = lines[i.saturating_sub(12)..i]
                .iter()
                .rev()
                .find(|l| l.contains("dec.symbol(&mut cdfs."))
                .unwrap_or_else(|| panic!("line {}: no symbol read above the guard", i + 1));
            let table = read
                .split("dec.symbol(&mut cdfs.")
                .nth(1)
                .unwrap()
                .split('[')
                .next()
                .unwrap();
            let (name, nsyms) = alphabets
                .iter()
                .find(|(n, _)| *n == table)
                .unwrap_or_else(|| panic!("line {}: unknown mode alphabet {table:?}", i + 1));
            assert!(
                *nsyms <= threshold,
                "line {}: the guard refuses mode >= {threshold} on a symbol read from a \
                 {nsyms}-symbol {name} CDF -- symbol {threshold} is inside that alphabet, so \
                 the refusal is live and this enumeration does not prove it",
                i + 1
            );
        }
        assert_eq!(sites, 3, "the intra-mode guard is at three sites, found {sites}");
    }

    /// Every entry of [`PROVEN`] must still name a live refusal and a test
    /// that exists, and the count it prints is the inventory's numerator.
    #[test]
    fn every_proven_refusal_names_a_test_that_exists() {
        let found = decode_path_refusals();
        let sources = [
            include_str!("stream.rs"),
            include_str!("decode.rs"),
            include_str!("refusal_inventory.rs"),
        ];
        for (reason, gate) in PROVEN {
            assert!(
                found.contains(*reason),
                "{reason:?} is listed as proven but is no longer a decode-path refusal --                  drop it from PROVEN (the capability landed) or fix the string"
            );
            assert!(
                sources.iter().any(|src| src.contains(&format!("fn {gate}("))),
                "{reason:?} names the proving test {gate}, which exists in neither stream.rs \
                 nor decode.rs"
            );
        }
        eprintln!(
            "refusal inventory: {} refusals + {} capability claims, {} proven",
            REFUSALS.len(),
            CAPABILITY_CLAIMS.len(),
            PROVEN.len()
        );
    }
}
