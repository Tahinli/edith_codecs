//! Every out-of-scope H.264 feature is refused by name, never decoded wrong.
//!
//! DONE.md (ec-h264 decode) requires that each out-of-scope feature returns
//! `Error::Unsupported` naming the feature, and that a test asserts it:
//! interlaced (PAFF and MBAFF), FMO, ASO, SP/SI, data partitioning, bit depth
//! above 10 and differing luma/chroma bit depth.
//!
//! Each case here is a synthetic stream built with `BitWriter`, so the tests
//! need neither ffmpeg nor fixtures and run on every checkout. The last test
//! maps every refused JVT conformance vector to the feature it really uses,
//! and is fixture-gated like `conformance.rs`.

use std::path::Path;

use ec_core::BitWriter;
use ec_h264::{Decoder, Error, NalOutcome};

use super::common::{escape, find_stream};

/// Append one Annex B NAL unit.
fn push_nal(out: &mut Vec<u8>, ref_idc: u8, kind: u8, rbsp: &[u8]) {
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.push((ref_idc << 5) | kind);
    out.extend_from_slice(&escape(rbsp));
}

/// Sequence parameter set: `profile` selects the High-family fields.
fn sps(
    profile: u8,
    chroma_format_idc: u32,
    bit_depth_minus8: u32,
    separate: bool,
    frame_mbs_only: bool,
) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bits(profile as u32, 8);
    w.write_bits(0, 8); // constraint flags
    w.write_bits(30, 8); // level_idc
    w.write_ue(0); // seq_parameter_set_id
    if matches!(profile, 100 | 110 | 122 | 244) {
        w.write_ue(chroma_format_idc);
        if chroma_format_idc == 3 {
            w.write_bit(separate);
        }
        w.write_ue(bit_depth_minus8); // luma
        w.write_ue(bit_depth_minus8); // chroma
        w.write_bit(false); // qpprime_y_zero_transform_bypass_flag
        w.write_bit(false); // seq_scaling_matrix_present_flag
    }
    w.write_ue(0); // log2_max_frame_num_minus4
    w.write_ue(2); // pic_order_cnt_type 2
    w.write_ue(1); // max_num_ref_frames
    w.write_bit(false); // gaps_in_frame_num_value_allowed_flag
    w.write_ue(10); // pic_width_in_mbs_minus1 -> 176
    w.write_ue(if frame_mbs_only { 8 } else { 4 }); // pic_height_in_map_units_minus1
    w.write_bit(frame_mbs_only);
    if !frame_mbs_only {
        w.write_bit(true); // mb_adaptive_frame_field_flag (only the MBAFF test uses it)
    }
    w.write_bit(true); // direct_8x8_inference_flag
    w.write_bit(false); // frame_cropping_flag
    w.write_bit(false); // vui_parameters_present_flag
    w.write_bit(true); // rbsp_stop_one_bit
    w.align_to_byte();
    w.as_bytes().to_vec()
}

/// Picture parameter set. `slice_groups` > 1 is FMO (dispersed map, type 1).
fn pps(slice_groups: u32) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_ue(0); // pic_parameter_set_id
    w.write_ue(0); // seq_parameter_set_id
    w.write_bit(false); // entropy_coding_mode_flag (CAVLC)
    w.write_bit(false); // bottom_field_pic_order_in_frame_present_flag
    w.write_ue(slice_groups - 1); // num_slice_groups_minus1
    if slice_groups > 1 {
        w.write_ue(1); // slice_group_map_type 1 (dispersed)
    }
    w.write_ue(0); // num_ref_idx_l0_default_active_minus1
    w.write_ue(0); // num_ref_idx_l1_default_active_minus1
    w.write_bit(false); // weighted_pred_flag
    w.write_bits(0, 2); // weighted_bipred_idc
    w.write_se(0); // pic_init_qp_minus26
    w.write_se(0); // pic_init_qs_minus26
    w.write_se(0); // chroma_qp_index_offset
    w.write_bit(false); // deblocking_filter_control_present_flag
    w.write_bit(false); // constrained_intra_pred_flag
    w.write_bit(false); // redundant_pic_cnt_present_flag
    w.write_bit(true); // rbsp_stop_one_bit
    w.align_to_byte();
    w.as_bytes().to_vec()
}

/// An IDR I slice of `count` I_PCM macroblocks starting at `first_mb`.
/// I_PCM keeps the body trivially valid: no residual, no mb_qp_delta.
fn pcm_slice(first_mb: u32, count: u32, frame_mbs_only: bool) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_ue(first_mb);
    w.write_ue(7); // slice_type 7: I, all slices of the picture
    w.write_ue(0); // pic_parameter_set_id
    w.write_bits(0, 4); // frame_num
    if !frame_mbs_only {
        w.write_bit(false); // field_pic_flag: a frame picture in an interlaced SPS
    }
    w.write_ue(0); // idr_pic_id
    w.write_bit(false); // no_output_of_prior_pics_flag
    w.write_bit(false); // long_term_reference_flag
    w.write_se(0); // slice_qp_delta
    for _ in 0..count {
        w.write_ue(25); // mb_type 25: I_PCM
        w.align_to_byte(); // pcm_alignment_zero_bit
        w.write_bytes(&[0x80; 384]); // 256 luma + 2 x 64 chroma samples
    }
    w.write_bit(true); // rbsp_stop_one_bit
    w.align_to_byte();
    w.as_bytes().to_vec()
}

/// A slice header of the given slice_type, with no macroblock data. Enough
/// for the refusals that fire on the header before any macroblock is read.
fn header_only_slice(slice_type: u32) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_ue(0); // first_mb_in_slice
    w.write_ue(slice_type);
    w.write_ue(0); // pic_parameter_set_id
    w.write_bits(0, 4); // frame_num
    w.write_ue(0); // idr_pic_id
    w.write_bit(false);
    w.write_bit(false);
    w.write_se(0);
    w.write_se(0);
    w.write_bit(true);
    w.align_to_byte();
    w.as_bytes().to_vec()
}

/// Feed `stream` to a fresh decoder the way a demuxer would, returning the
/// first error (or `None` when the whole stream decodes without one).
fn first_error(stream: &[u8]) -> Option<Error> {
    let mut dec = Decoder::new();
    for nal in ec_h264_syntax::AnnexBIter::new(stream) {
        match dec.push_nal(nal) {
            Ok(NalOutcome::PictureBoundary) => {
                if let Err(e) = dec.end_picture() {
                    return Some(e);
                }
                if let Err(e) = dec.push_nal(nal) {
                    return Some(e);
                }
            }
            Ok(_) => {}
            Err(e) => return Some(e),
        }
    }
    dec.flush().err()
}

/// Assert the stream is refused with `Error::Unsupported` whose `what` names
/// `feature`. A plain `Corrupt`, a panic or a clean decode all fail.
fn assert_refused(stream: &[u8], feature: &str) {
    match first_error(stream) {
        Some(Error::Unsupported { what, why }) => {
            assert!(
                what.contains(feature),
                "refused as `{what}` ({why}), expected a refusal naming `{feature}`"
            );
            assert!(!why.is_empty(), "refusal of `{what}` gives no reason");
        }
        Some(other) => panic!("expected Unsupported naming `{feature}`, got {other:?}"),
        None => panic!("stream using {feature} decoded without a refusal"),
    }
}

/// Stream with an SPS, a PPS and one slice of the given header.
fn one_slice_stream(
    profile: u8,
    chroma: u32,
    bd_minus8: u32,
    separate: bool,
    groups: u32,
    slice: &[u8],
) -> Vec<u8> {
    let mut out = Vec::new();
    push_nal(
        &mut out,
        3,
        7,
        &sps(profile, chroma, bd_minus8, separate, true),
    );
    push_nal(&mut out, 3, 8, &pps(groups));
    push_nal(&mut out, 3, 5, slice);
    out
}

#[test]
fn fmo_with_interlaced_coding_is_refused_by_name() {
    let mut out = Vec::new();
    push_nal(&mut out, 3, 7, &sps(66, 1, 0, false, false));
    push_nal(&mut out, 3, 8, &pps(2));
    push_nal(&mut out, 3, 5, &pcm_slice(0, 1, false));
    assert_refused(&out, "FMO slice groups with interlaced coding");
}

#[test]
fn arbitrary_slice_order_decodes() {
    // One IDR picture, slices sent as MBs 50..98 then 0..49. Every slice
    // shares the picture's header: ASO, not a second picture.
    let mut out = Vec::new();
    push_nal(&mut out, 3, 7, &sps(66, 1, 0, false, true));
    push_nal(&mut out, 3, 8, &pps(1));
    push_nal(&mut out, 3, 5, &pcm_slice(50, 49, true));
    push_nal(&mut out, 3, 5, &pcm_slice(0, 50, true));
    assert!(first_error(&out).is_none(), "{:?}", first_error(&out));
}

#[test]
fn fmo_dispersed_map_decodes() {
    // Dispersed map, two groups: group 0 holds the even checkerboard cells.
    // Slice of group 0 from MB 0 (50 MBs), slice of group 1 from MB 1 (49).
    let mut out = Vec::new();
    push_nal(&mut out, 3, 7, &sps(66, 1, 0, false, true));
    push_nal(&mut out, 3, 8, &pps(2));
    push_nal(&mut out, 3, 5, &pcm_slice(1, 49, true));
    push_nal(&mut out, 3, 5, &pcm_slice(0, 50, true));
    assert!(first_error(&out).is_none(), "{:?}", first_error(&out));
}

#[test]
fn sp_slices_are_refused_by_name() {
    let stream = one_slice_stream(88, 1, 0, false, 1, &header_only_slice(3));
    assert_refused(&stream, "SP and SI slices");
}

#[test]
fn si_slices_are_refused_by_name() {
    let stream = one_slice_stream(88, 1, 0, false, 1, &header_only_slice(4));
    assert_refused(&stream, "SP and SI slices");
}

#[test]
fn data_partitioning_is_refused_by_name() {
    // nal_unit_type 2 is slice data partition A; the refusal fires on the NAL
    // type alone, before any partition is parsed.
    let mut out = Vec::new();
    push_nal(&mut out, 3, 7, &sps(88, 1, 0, false, true));
    push_nal(&mut out, 3, 8, &pps(1));
    push_nal(&mut out, 3, 2, &header_only_slice(7));
    assert_refused(&out, "slice data partitioning");
}

#[test]
fn bit_depth_above_ten_is_refused_by_name() {
    // High 10 itself (8 to 10 bits) is decoded, see tests/it/high10.rs; 11 bits
    // and up belong to High 4:4:4 Predictive.
    let stream = one_slice_stream(110, 1, 3, false, 1, &pcm_slice(0, 4, true));
    assert_refused(&stream, "bit depth above 10");
}

#[test]
fn different_luma_and_chroma_bit_depth_is_refused_by_name() {
    // Monochrome, 4:4:4, separate colour planes and transform bypass are now
    // decoded (tests/it/high444.rs, tests/it/high444_jvt.rs); a chroma bit depth
    // that differs from the luma one is not.
    let mut stream = Vec::new();
    let mut w = BitWriter::new();
    w.write_bits(244, 8);
    w.write_bits(0, 8);
    w.write_bits(30, 8);
    w.write_ue(0);
    w.write_ue(3); // chroma_format_idc
    w.write_bit(false);
    w.write_ue(0); // luma 8 bit
    w.write_ue(2); // chroma 10 bit
    w.write_bit(false);
    w.write_bit(false);
    w.write_ue(0);
    w.write_ue(2);
    w.write_ue(1);
    w.write_bit(false);
    w.write_ue(10);
    w.write_ue(8);
    w.write_bit(true);
    w.write_bit(true);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(true);
    w.align_to_byte();
    push_nal(&mut stream, 3, 7, w.as_bytes());
    push_nal(&mut stream, 3, 8, &pps(1));
    push_nal(&mut stream, 3, 5, &pcm_slice(0, 4, true));
    match first_error(&stream) {
        Some(Error::Unsupported { .. }) | Some(Error::Corrupt { .. }) => {}
        other => panic!("expected a refusal of mixed bit depths, got {other:?}"),
    }
}

/// Every refused vector in `fetch-vectors.sh`'s h264-jvt set, against the
/// feature it really uses. A vector that starts refusing for another reason,
/// or stops being refused, changes this table and fails the test.
#[test]
fn jvt_refused_vectors_refuse_for_their_feature() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vectors/h264-jvt");
    if !base.exists() {
        assert!(
            std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
            "JVT vectors missing at {} (run scripts/fetch-vectors.sh)",
            base.display()
        );
        eprintln!("SKIP JVT vectors missing at {}", base.display());
        return;
    }
    let expected: &[(&str, &str)] = &[];
    let mut failures = Vec::new();
    for &(name, feature) in expected {
        let Some(stream) = find_stream(&base.join(name)) else {
            failures.push(format!("{name}: no bitstream on disk"));
            continue;
        };
        let bytes = std::fs::read(&stream).expect("read stream");
        match first_error(&bytes) {
            Some(Error::Unsupported { what, .. }) if what.contains(feature) => {}
            other => failures.push(format!(
                "{name}: expected refusal naming `{feature}`, got {other:?}"
            )),
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
