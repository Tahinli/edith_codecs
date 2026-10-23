//! Out-of-scope HEVC streams are refused by name: `Error::Unsupported` whose `what` names the
//! feature. Never a panic, never `Corrupt`, never pixels.
//!
//! The streams in `data/refusals/` are 64x64, four frames each, made with ffmpeg and libx265
//! (`-pix_fmt` as in the file name, `-profile:v main444-8` for `main444_8.265`). The SCC, RExt,
//! multi-layer and 3D cases are the 8-bit 4:2:0 control stream with one parameter set's
//! extension flags set (the writer emits the flags, not the extension bodies, which the
//! decoder never reads), so each case isolates one feature.
//!
//! Multi-layer policy: NAL units with `nuh_layer_id > 0` are skipped, so a multi-layer stream
//! decodes its base layer. That is only refused when a base-layer SPS or PPS itself carries the
//! multilayer extension flag.

use ec_core::error::Error;
use ec_h265::Decoder;
use ec_h265::decode::DecodedPicture;
use ec_h265_syntax::nal::{split_annex_b, write_annex_b};
use ec_h265_syntax::ps::{Pps, PpsRangeExt, Sps, SpsRangeExt};

const MAIN_8: &[u8] = include_bytes!("../data/refusals/main420_8.265");
const MAIN_10: &[u8] = include_bytes!("../data/refusals/main10_420.265");
const YUV444_8: &[u8] = include_bytes!("../data/refusals/yuv444_8.265");
const MAIN444_8: &[u8] = include_bytes!("../data/refusals/main444_8.265");
const YUV420_12: &[u8] = include_bytes!("../data/refusals/yuv420_12.265");
const GRAY: &[u8] = include_bytes!("../data/refusals/gray_400.265");

const NAL_SPS: u8 = 33;
const NAL_PPS: u8 = 34;

/// Decode a whole Annex-B stream and return the output pictures in order.
fn decode_all(data: &[u8]) -> Result<Vec<DecodedPicture>, Error> {
    let mut dec = Decoder::new();
    dec.set_threads(1);
    dec.decode_annex_b(data)?;
    dec.flush()?;
    let mut pictures = Vec::new();
    while let Some(frame) = dec.next_frame() {
        pictures.push((*frame.picture).clone());
    }
    Ok(pictures)
}

/// Assert that `data` is refused with `Error::Unsupported` and that the refused feature
/// (the `what` field) contains `feature`.
fn assert_refused(name: &str, data: &[u8], feature: &str) {
    match decode_all(data) {
        Err(Error::Unsupported { what, why }) => {
            assert!(
                what.contains(feature),
                "{name}: refusal does not name {feature:?}: what = {what:?}, why = {why:?}"
            );
        }
        Err(other) => {
            panic!("{name}: expected Error::Unsupported naming {feature:?}, got {other:?}")
        }
        Ok(pictures) => panic!(
            "{name}: expected a refusal naming {feature:?}, decoded {} pictures",
            pictures.len()
        ),
    }
}

/// Rewrite every NAL unit of `data` with `edit` applied to each unit of type `nal_type`.
/// `edit` receives the unit's RBSP and returns the replacement RBSP.
fn rewrite_nals(data: &[u8], nal_type: u8, edit: impl Fn(&[u8]) -> Vec<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    for nal in split_annex_b(data) {
        let rbsp = nal.rbsp();
        let rbsp = if nal.header.nal_type.code() == nal_type {
            edit(&rbsp)
        } else {
            rbsp
        };
        write_annex_b(&mut out, nal.header, &rbsp, true);
    }
    out
}

/// The control stream with `flags` as its SPS extension flags (packed as `Sps::extension_flags`).
fn with_sps_flags(data: &[u8], flags: u8) -> Vec<u8> {
    rewrite_nals(data, NAL_SPS, |rbsp| {
        let mut sps = Sps::parse(rbsp).expect("control SPS parses");
        sps.extension_flags = flags;
        sps.to_rbsp()
    })
}

/// The control stream with `flags` as its PPS extension flags.
fn with_pps_flags(data: &[u8], flags: u8) -> Vec<u8> {
    rewrite_nals(data, NAL_PPS, |rbsp| {
        let mut pps = Pps::parse(rbsp).expect("control PPS parses");
        pps.extension_flags = flags;
        pps.to_rbsp()
    })
}

/// The control stream with an SPS range extension whose flags `edit` sets.
fn with_sps_range(data: &[u8], edit: impl Fn(&mut SpsRangeExt)) -> Vec<u8> {
    rewrite_nals(data, NAL_SPS, |rbsp| {
        let mut sps = Sps::parse(rbsp).expect("control SPS parses");
        sps.extension_flags = 0x80;
        edit(&mut sps.range);
        sps.to_rbsp()
    })
}

/// The control stream with a PPS range extension whose fields `edit` sets.
fn with_pps_range(data: &[u8], edit: impl Fn(&mut PpsRangeExt)) -> Vec<u8> {
    rewrite_nals(data, NAL_PPS, |rbsp| {
        let mut pps = Pps::parse(rbsp).expect("control PPS parses");
        pps.extension_flags = 0x80;
        edit(&mut pps.range);
        pps.to_rbsp()
    })
}

#[test]
fn control_streams_decode() {
    for (name, data) in [("main 8-bit", MAIN_8), ("main 10-bit", MAIN_10)] {
        let pictures = decode_all(data).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(pictures.len(), 4, "{name}: picture count");
    }
    // The re-written control streams (no extension flags) must still decode: this checks that
    // the flag-setting rewrite used by the synthetic cases is faithful.
    let rewritten = with_sps_flags(MAIN_8, 0);
    assert_eq!(
        decode_all(&rewritten).expect("rewritten SPS decodes").len(),
        4
    );
    let rewritten = with_pps_flags(MAIN_8, 0);
    assert_eq!(
        decode_all(&rewritten).expect("rewritten PPS decodes").len(),
        4
    );
}

#[test]
fn range_extension_formats_decode() {
    // 4:4:4, 4:0:0 and 12-bit streams are decoded (see tests/it/rext_generated.rs for the
    // bit-exact checks against ffmpeg); only the planes and depths are checked here.
    for (name, data, idc, depth, planes) in [
        ("yuv444p", YUV444_8, 3, 8, 3),
        ("main444-8", MAIN444_8, 3, 8, 3),
        ("gray", GRAY, 0, 8, 1),
        ("yuv420p12le", YUV420_12, 1, 12, 3),
    ] {
        let pictures = decode_all(data).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(pictures.len(), 4, "{name}: picture count");
        for p in &pictures {
            assert_eq!(p.chroma_format_idc, idc, "{name}: chroma_format_idc");
            assert_eq!(p.bit_depth, depth, "{name}: bit depth");
            assert_eq!(p.y.len(), p.width * p.height, "{name}: luma plane");
            let (cw, ch) = p.chroma_dims();
            let chroma = if planes == 1 { 0 } else { cw * ch };
            assert_eq!(p.cb.len(), chroma, "{name}: Cb plane");
            assert_eq!(p.cr.len(), chroma, "{name}: Cr plane");
        }
    }
}

#[test]
fn sixteen_bit_is_refused_by_name() {
    let data = rewrite_nals(MAIN_8, NAL_SPS, |rbsp| {
        let mut sps = Sps::parse(rbsp).expect("control SPS parses");
        sps.bit_depth_luma_minus8 = 8;
        sps.bit_depth_chroma_minus8 = 8;
        sps.to_rbsp()
    });
    assert_refused("16-bit", &data, "bit depth 16");
}

#[test]
fn separate_colour_planes_are_refused_by_name() {
    let data = rewrite_nals(MAIN_8, NAL_SPS, |rbsp| {
        let mut sps = Sps::parse(rbsp).expect("control SPS parses");
        sps.chroma_format_idc = 3;
        sps.separate_colour_plane = true;
        sps.to_rbsp()
    });
    assert_refused("separate planes", &data, "separate_colour_plane_flag");
}

#[test]
fn sps_extended_precision_is_refused_by_name() {
    let data = with_sps_range(MAIN_8, |r| r.extended_precision_processing = true);
    assert_refused("SPS RExt", &data, "extended_precision_processing_flag");
}

#[test]
fn sps_cabac_bypass_alignment_is_refused_by_name() {
    let data = with_sps_range(MAIN_8, |r| r.cabac_bypass_alignment_enabled = true);
    assert_refused("SPS RExt", &data, "cabac_bypass_alignment_enabled_flag");
}

#[test]
fn pps_range_extension_tools_are_accepted() {
    // Every PPS range extension tool is implemented, so the headers alone are not refused.
    let data = with_pps_range(MAIN_8, |r| r.log2_sao_offset_scale_luma = 0);
    decode_all(&data).expect("PPS range extension without tools decodes");
}

/// Each feature flag written into the control stream, the parameter set that carries it, the
/// refusal text it must produce, and the old test name it was filed under.
type FlagCase = (&'static str, fn(&[u8], u8) -> Vec<u8>, u8, &'static str);

const FLAG_CASES: &[FlagCase] = &[
    (
        "sps_scc_extension_is_refused_by_name",
        with_sps_flags,
        0x10,
        "SCC",
    ),
    (
        "pps_scc_extension_is_refused_by_name",
        with_pps_flags,
        0x10,
        "SCC",
    ),
    (
        "sps_multilayer_extension_is_refused_by_name",
        with_sps_flags,
        0x40,
        "multi-layer",
    ),
    (
        "pps_multilayer_extension_is_refused_by_name",
        with_pps_flags,
        0x40,
        "multi-layer",
    ),
    (
        "sps_3d_extension_is_refused_by_name",
        with_sps_flags,
        0x20,
        "3D",
    ),
    (
        "pps_3d_extension_is_refused_by_name",
        with_pps_flags,
        0x20,
        "3D",
    ),
];

#[test]
fn flag_extensions_are_refused_by_name() {
    let mut failures = Vec::new();
    for &(name, make, flags, feature) in FLAG_CASES {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_refused(name, &make(MAIN_8, flags), feature)
        }));
        if outcome.is_err() {
            failures.push(name);
        }
    }
    assert!(failures.is_empty(), "failed cases: {failures:?}");
}

/// Layer-1 copies of every slice, and of the SPS with its multilayer flag set, are skipped, so
/// the base layer decodes to exactly the pictures the plain stream decodes to.
#[test]
fn layer_one_nal_units_are_skipped_and_base_layer_decodes() {
    let base = decode_all(MAIN_8).expect("base decodes");
    let mut layered = Vec::new();
    for nal in split_annex_b(MAIN_8) {
        let rbsp = nal.rbsp();
        write_annex_b(&mut layered, nal.header, &rbsp, true);
        let code = nal.header.nal_type.code();
        let mut header = nal.header;
        header.layer_id = 1;
        if code == NAL_SPS {
            let mut sps = Sps::parse(&rbsp).expect("control SPS parses");
            sps.extension_flags = 0x40;
            write_annex_b(&mut layered, header, &sps.to_rbsp(), false);
        } else if code <= 21 {
            write_annex_b(&mut layered, header, &rbsp, false);
        }
    }
    let pictures = decode_all(&layered).expect("layer-1 NAL units are skipped, not refused");
    assert_eq!(pictures.len(), base.len(), "picture count");
    for (got, want) in pictures.iter().zip(&base) {
        assert_eq!(
            (got.y.as_slice(), got.cb.as_slice(), got.cr.as_slice()),
            (want.y.as_slice(), want.cb.as_slice(), want.cr.as_slice())
        );
    }
}
