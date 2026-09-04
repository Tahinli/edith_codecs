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
    "a block that actually uses a palette (UV) -- reconstruction is out of scope",
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
    // The same arm's screen-content gate: palette/intrabc syntax is consumed
    // for square blocks only, so a strip in such a frame would skip symbols.
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

    /// Every entry of [`PROVEN`] must still name a live refusal and a test
    /// that exists, and the count it prints is the inventory's numerator.
    #[test]
    fn every_proven_refusal_names_a_test_that_exists() {
        let found = decode_path_refusals();
        let src = include_str!("stream.rs");
        for (reason, gate) in PROVEN {
            assert!(
                found.contains(*reason),
                "{reason:?} is listed as proven but is no longer a decode-path refusal --                  drop it from PROVEN (the capability landed) or fix the string"
            );
            assert!(
                src.contains(&format!("fn {gate}(")),
                "{reason:?} names the proving test {gate}, which does not exist in stream.rs"
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
