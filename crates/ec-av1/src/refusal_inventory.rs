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
    "a non-DC chroma mode on an 8x8 inter-frame leaf (this encoder never writes one)",
];

#[cfg(test)]
const REFUSALS: &[&str] = &[
    "a nonzero angle delta on an 8x8 intra leaf in an inter frame (no gate reaches this leaf with one; the >=16x16 arm decodes deltas)",
    "a coded HORZ/VERT strip whose chroma transform has no rect coefficient tables here",
    "a split intra strip whose transform unit is {tx_w}x{tx_h} (no luma coefficient tables for that shape here)",
    "an intra 4x4 block inside an inter frame's sub-8x8 split (this decoder codes only inter 4x4 sub-blocks there)",
    "an intra 8x4/4x8 block inside an inter frame's sub-8x8 HORZ/VERT partition (this decoder codes only inter sub-blocks there)",
    "a sub-8x8 inter block under a scaled reference (superres, unimplemented)",
    "a COMPOUND_WEDGE mask on a non-square inter block (rect wedge codebook unimplemented)",
    "a 1:4 rect strip that actually uses a palette (reconstruction is not ported at this shape)",
    "a 16x16 block whose true edge cuts through both axes needs a rectangular transform this decoder does not code yet",
    "a 16x16 inter block whose true edge cuts through both axes needs a rectangular transform this decoder does not code yet",
    "a 32x32 partition type this decoder does not code (value={part32})",
    "a Golomb tail longer than this decoder reads",
    "a tx_type symbol outside its CDF's own set: {t}",
    "an INTER 32x32 partition type this decoder does not code (value={part32})",
    "a superblock-level partition value outside PARTITION_NONE..PARTITION_VERT_4",
    "a 128x128 superblock HORZ/VERT or AB partition (only SPLIT and NONE are decoded at the 128 root)",
    "a 128x128 superblock PARTITION_NONE root on an inter frame (only the key-frame path codes a whole 128x128 block)",
    "intrabc under a 128x128 superblock (libaom's av1_is_dv_valid derives the block-vector delay from sb_size, which this decoder hardcodes to 64)",
    "a palette block on a HORZ/VERT intra strip below 16x16 (reconstruction not ported)",
    "a palette block with a real transform on a superblock-level HORZ/VERT strip (corner-cropped luma coefficients not ported for palette)",
    "a block that actually uses a palette (UV) -- reconstruction is out of scope",
    "a block that actually uses a palette (Y) -- reconstruction is out of scope",
    "a sub-8x8 leaf that uses intrabc (this reader has no block-vector path; the 8x8-and-up reader reconstructs one)",
    "an intrabc block under TxMode::Select (its transform size is coded by the inter var-tx partition tree, which this decoder never reads)",
    "a bit depth of 12 (this decoder is gated at 8 and 10 only: warp/MC/wiener rounding shifts change at 12-bit and no 12-bit gate exists)",
    "a frame OBU with no tile group",
    "a frame naming primary_ref_frame at a reference slot with no saved CDF state",
    "a frame with no mode-info grid",
    "a frame whose segmentation enables SEG_LVL_REF_FRAME/SKIP/GLOBALMV (this decoder reads segment_id but never lets a segment override a block's reference, skip or mode)",
    "a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding",
    "a reference frame selected with no picture at this frame's own ref_frame_idx slot for it",
    "a reference picture whose height does not match this frame's own true size",
    "a show_existing_frame header naming an empty reference slot",
    "an inter var-tx tree with a leaf transform larger than 32x32",
    // lane-intrainter r1 lifted the >=16x16 square case (per-TU intra
    // prediction + coefficients, gate
    // `a_real_aomenc_inter_sequence_with_a_split_transform_intra_block_decodes_pixel_exact`);
    // the 8x8 leaf's TX_4X4 2x2 grid is still refused here.
    "an 8x8 intra leaf in an inter frame whose tx_depth splits it into 4x4 transform units",
    "an inter frame with no key frame before it",
    "an inter SB-level AB partition (HORZ_A/HORZ_B/VERT_A/VERT_B; this decoder's inter tile path codes a superblock as NONE, SPLIT, HORZ, VERT, HORZ_4 or VERT_4)",
    "an inter 16x16-level AB or 1:4 partition (HORZ_A/HORZ_B/VERT_A/VERT_B/HORZ_4/VERT_4; this decoder's inter path codes a 16x16 as NONE, HORZ, VERT or SPLIT)",
    "an intra mode this decoder does not code (round 2)",
    // lane-intrarect r1 lifted the whole-shape refusal that stood here (the
    // inter path's intra arm now routes 2:1 strips through `decode_rect_split`);
    // what is left is the 1:4 shape, whose `bsize_to_tx_size_cat` breaks the
    // size-group/category diagonal the 2:1 shapes share.
    "an intra-coded 1:4 (or other non-2:1) rect strip on the inter block path",
    "a split (nonzero tx_depth) transform on an intra HORZ/VERT strip in an inter frame",
    // The same arm's screen-content gate: palette/intrabc syntax is consumed
    // for square blocks only, so a strip in such a frame would skip symbols.
    "a HORZ/VERT intra strip in a screen-content frame (palette syntax is consumed for square blocks only)",
    "warp prediction with a scaled reference (superres, unimplemented)",
    "an 8x8 partition leaf under a scaled reference (superres, unimplemented)",
    "a motion_mode symbol for a block shape with no CDF row here",
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
    use super::{CAPABILITY_CLAIMS, GATES_THAT_SKIP_ON_A_DECODE_ERROR, REFUSALS};
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
}
