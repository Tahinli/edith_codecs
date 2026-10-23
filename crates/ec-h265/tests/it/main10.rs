//! Main 10 encode: 10-bit 4:2:0 round trips, the bit-depth gate, the Main 10 SPS and the
//! colour description passthrough.
//!
//! The encoder's own reconstruction is the oracle: an access unit it writes must decode
//! sample for sample to that reconstruction. The ffmpeg side of the same check lives in
//! `scripts/hevc10-roundtrip.sh`.

use ec_core::frame::{PixelFormat, Plane, VideoFrame};
use ec_h265::Decoder;
use ec_h265::decode::decode_idr_au;
use ec_h265::encoder::{Encoder, EncoderConfig, RateControl};
use ec_h265_syntax::vui::{ColourDescription, VideoSignalType};

/// A deterministic 10-bit picture with gradients, texture and noise, I010 layout
/// (16-bit little-endian samples). `seed` moves the texture from frame to frame.
fn picture10(w: usize, h: usize, seed: u32) -> VideoFrame {
    let (cw, ch) = (w / 2, h / 2);
    let mut state = 0x9e37_79b9u32 ^ seed.wrapping_mul(0x85eb_ca6b);
    let mut noise = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    let mut plane = |pw: usize, ph: usize, base: u32| -> Vec<u8> {
        let mut out = Vec::with_capacity(pw * ph * 2);
        for y in 0..ph {
            for x in 0..pw {
                let grad = (x as u32 * 3 + y as u32 * 2 + seed * 7) % 1024;
                let tex = ((x as u32 / 4 + y as u32 / 4) % 2) * 40;
                let n = noise() % 9;
                let v = (grad / 2 + base + tex + n).min(1023) as u16;
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out
    };
    let y = plane(w, h, 200);
    let cb = plane(cw, ch, 400);
    let cr = plane(cw, ch, 600);
    VideoFrame::try_new(
        PixelFormat::I010,
        w as u32,
        h as u32,
        vec![
            Plane::new(y, w * 2),
            Plane::new(cb, cw * 2),
            Plane::new(cr, cw * 2),
        ],
    )
    .expect("I010 planes at the stated size")
}

/// The reconstruction's planes as the 16-bit little-endian bytes the decoder's `u16`
/// samples serialise to.
fn plane_bytes(samples: &[u16]) -> Vec<u8> {
    samples.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn config(w: u32, h: u32) -> EncoderConfig {
    let mut cfg = EncoderConfig::new(w, h);
    cfg.bit_depth = 10;
    cfg.keep_recon = true;
    cfg.threads = 2;
    cfg
}

#[test]
fn main10_idr_decodes_to_the_encoder_reconstruction() {
    let (w, h) = (96u32, 64u32);
    let mut cfg = config(w, h);
    cfg.rate_control = RateControl::ConstantQp(22);
    let enc = Encoder::new(cfg).expect("10-bit encoder");
    let frame = picture10(w as usize, h as usize, 1);
    let coded = enc.encode_idr(&frame).expect("encode");
    let recon = coded.recon.expect("keep_recon");
    assert_eq!(recon.format, PixelFormat::I010);

    let pic = decode_idr_au(&coded.au).expect("decode");
    assert_eq!(pic.bit_depth, 10);
    assert_eq!(pic.bit_depth_c, 10);
    let mut out = Vec::new();
    out.extend(plane_bytes(&pic.y));
    out.extend(plane_bytes(&pic.cb));
    out.extend(plane_bytes(&pic.cr));
    let expected: Vec<u8> = recon
        .planes
        .iter()
        .flat_map(|p| p.data.as_ref().to_vec())
        .collect();
    assert_eq!(
        out, expected,
        "decoder output differs from the encoder reconstruction"
    );
}

#[test]
fn main10_sequence_with_p_pictures_decodes_to_the_reconstruction() {
    let (w, h) = (96u32, 64u32);
    let mut cfg = config(w, h);
    cfg.rate_control = RateControl::ConstantQp(30);
    cfg.keyint = 4;
    let enc = Encoder::new(cfg).expect("10-bit encoder");
    let frames: Vec<VideoFrame> = (0..8)
        .map(|i| picture10(w as usize, h as usize, i))
        .collect();
    let coded = enc.encode_sequence(&frames).expect("encode");

    let mut dec = Decoder::new();
    let mut expected = Vec::new();
    for pic in &coded {
        dec.decode_annex_b(&pic.au).expect("decode");
        let recon = pic.recon.as_ref().expect("keep_recon");
        expected.push(
            recon
                .planes
                .iter()
                .flat_map(|p| p.data.as_ref().to_vec())
                .collect::<Vec<u8>>(),
        );
    }
    dec.flush().expect("flush");
    let mut got = Vec::new();
    while let Some(f) = dec.next_frame() {
        let p = &f.picture;
        assert_eq!(p.bit_depth, 10);
        got.push([plane_bytes(&p.y), plane_bytes(&p.cb), plane_bytes(&p.cr)].concat());
    }
    assert_eq!(
        got.len(),
        expected.len(),
        "every picture comes out of the decoder"
    );
    for (i, (g, e)) in got.iter().zip(&expected).enumerate() {
        assert!(
            g == e,
            "picture {i} differs from the encoder reconstruction"
        );
    }
}

#[test]
fn bit_depth_other_than_8_or_10_is_refused() {
    let mut cfg = config(64, 64);
    cfg.bit_depth = 9;
    assert!(Encoder::new(cfg).is_err());
}

#[test]
fn eight_bit_planes_are_refused_by_a_10_bit_encoder() {
    let cfg = config(64, 64);
    let enc = Encoder::new(cfg).expect("10-bit encoder");
    let y = vec![128u8; 64 * 64];
    let c = vec![128u8; 32 * 32];
    assert!(enc.encode_idr_planes(&y, 64, &c, 32, &c, 32).is_err());
}

#[test]
fn main10_sps_carries_the_profile_depth_and_the_colour_description() {
    let mut cfg = config(64, 64);
    cfg.video_signal_type = Some(VideoSignalType {
        video_format: 5,
        video_full_range_flag: false,
        colour_description: Some(ColourDescription {
            colour_primaries: 9,
            transfer_characteristics: 16,
            matrix_coeffs: 9,
        }),
    });
    let enc = Encoder::new(cfg).expect("10-bit encoder");
    let sps = enc.sps();
    assert_eq!(sps.bit_depth_luma_minus8, 2);
    assert_eq!(sps.bit_depth_chroma_minus8, 2);
    assert_eq!(sps.ptl.profile_idc, 2, "Main 10");
    let vst = sps
        .vui
        .as_ref()
        .and_then(|v| v.video_signal_type)
        .expect("video signal type in the VUI");
    let cd = vst.colour_description.expect("colour description");
    assert_eq!(
        (
            cd.colour_primaries,
            cd.transfer_characteristics,
            cd.matrix_coeffs
        ),
        (9, 16, 9),
        "BT.2020 PQ metadata survives"
    );
}

#[test]
fn bitrate_mode_lands_within_five_percent_at_10_bits() {
    let (w, h, frames) = (128u32, 96u32, 30usize);
    let kbps = 600u32;
    let mut cfg = config(w, h);
    cfg.rate_control = RateControl::Bitrate { kbps, fps: 30.0 };
    cfg.keyint = 8;
    cfg.threads = 2;
    let enc = Encoder::new(cfg).expect("10-bit encoder");
    let clip: Vec<VideoFrame> = (0..frames)
        .map(|i| picture10(w as usize, h as usize, i as u32))
        .collect();
    let coded = enc.encode_sequence(&clip).expect("encode");
    let bits: f64 = coded.iter().map(|p| p.au.len() as f64 * 8.0).sum();
    let seconds = frames as f64 / 30.0;
    let achieved = bits / seconds / 1000.0;
    let err = (achieved - f64::from(kbps)) / f64::from(kbps);
    assert!(
        err.abs() <= 0.05,
        "achieved {achieved:.1} kb/s against {kbps} kb/s ({:+.2}%)",
        err * 100.0
    );
}

/// An I010 frame from a per-plane sample function `f(plane, x, y, seed)`.
fn frame_from(
    w: usize,
    h: usize,
    seed: u32,
    f: impl Fn(usize, usize, usize, u32) -> u16,
) -> VideoFrame {
    let mut planes = Vec::new();
    for p in 0..3 {
        let (pw, ph) = if p == 0 { (w, h) } else { (w / 2, h / 2) };
        let mut out = Vec::with_capacity(pw * ph * 2);
        for y in 0..ph {
            for x in 0..pw {
                out.extend_from_slice(&f(p, x, y, seed).min(1023).to_le_bytes());
            }
        }
        planes.push(Plane::new(out, pw * 2));
    }
    VideoFrame::try_new(PixelFormat::I010, w as u32, h as u32, planes).expect("I010")
}

/// Intra-only and GOP round trips of `frames` at `qp`: the decoder's output must equal the
/// reconstruction, and the reconstruction must stay inside 0..=1023.
fn assert_round_trip(name: &str, frames: &[VideoFrame], w: u32, h: u32, qp: i32) {
    for keyint in [1usize, 4] {
        let mut cfg = config(w, h);
        cfg.rate_control = RateControl::ConstantQp(qp);
        cfg.keyint = keyint;
        let enc = Encoder::new(cfg).expect("10-bit encoder");
        let coded = if keyint == 1 {
            enc.encode_batch(frames.iter()).expect("encode")
        } else {
            enc.encode_sequence(frames).expect("encode")
        };
        let mut dec = Decoder::new();
        let mut expected = Vec::new();
        for pic in &coded {
            dec.decode_annex_b(&pic.au).expect("decode");
            let recon = pic.recon.as_ref().expect("keep_recon");
            let bytes: Vec<u8> = recon
                .planes
                .iter()
                .flat_map(|p| p.data.as_ref().to_vec())
                .collect();
            assert!(
                bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .all(|c| u16::from_le_bytes(*c) <= 1023),
                "{name}: recon sample above 1023"
            );
            expected.push(bytes);
        }
        dec.flush().expect("flush");
        let mut n = 0;
        while let Some(f) = dec.next_frame() {
            let p = &f.picture;
            let got = [plane_bytes(&p.y), plane_bytes(&p.cb), plane_bytes(&p.cr)].concat();
            assert!(
                got == expected[n],
                "{name}: keyint {keyint} qp {qp} picture {n} differs"
            );
            n += 1;
        }
        assert_eq!(n, expected.len(), "{name}: picture count");
    }
}

#[test]
fn main10_smooth_gradient_has_no_overflow_or_mismatch() {
    let (w, h) = (128usize, 64usize);
    // A full-range ramp across 1024 codes, sliding by frame: banding and clip-overflow bait.
    let frames: Vec<VideoFrame> = (0..6)
        .map(|i| {
            frame_from(w, h, i, |p, x, y, s| {
                let span = if p == 0 { w } else { w / 2 };
                ((x * 1023 / (span - 1)) as u32 + y as u32 / 4 + s * 5).min(1023) as u16
            })
        })
        .collect();
    for qp in [-12, 0, 20, 36, 51] {
        assert_round_trip("gradient", &frames, w as u32, h as u32, qp);
    }
}

#[test]
fn main10_extremes_black_white_and_alternating() {
    let (w, h) = (64usize, 64usize);
    let black: Vec<VideoFrame> = (0..4)
        .map(|i| frame_from(w, h, i, |_, _, _, _| 0))
        .collect();
    let white: Vec<VideoFrame> = (0..4)
        .map(|i| frame_from(w, h, i, |_, _, _, _| 1023))
        .collect();
    // Black/white checker plus a flip between frames: maximum residual, clipping everywhere.
    let checker: Vec<VideoFrame> = (0..4)
        .map(|i| {
            frame_from(w, h, i, |_, x, y, s| {
                if ((x / 4 + y / 4) as u32 + s).is_multiple_of(2) {
                    0
                } else {
                    1023
                }
            })
        })
        .collect();
    for qp in [-12, 10, 30, 51] {
        assert_round_trip("black", &black, w as u32, h as u32, qp);
        assert_round_trip("white", &white, w as u32, h as u32, qp);
        assert_round_trip("checker", &checker, w as u32, h as u32, qp);
    }
}

#[test]
fn main10_rich_content_across_qp_range() {
    let (w, h) = (96usize, 64usize);
    let frames: Vec<VideoFrame> = (0..5).map(|i| picture10(w, h, i)).collect();
    for qp in [-12, -6, 0, 15, 30, 45, 51] {
        assert_round_trip("rich", &frames, w as u32, h as u32, qp);
    }
}

#[test]
fn main10_hdr_signalling_vui_and_sei() {
    use ec_h265_syntax::sei::MasteringDisplay;
    let mut cfg = config(64, 64);
    cfg.video_signal_type = Some(VideoSignalType {
        video_format: 5,
        video_full_range_flag: true,
        colour_description: Some(ColourDescription {
            colour_primaries: 9,
            transfer_characteristics: 18,
            matrix_coeffs: 9,
        }),
    });
    cfg.chroma_sample_loc = Some((2, 2));
    cfg.mastering_display = Some(MasteringDisplay {
        display_primaries: [(13250, 34500), (7500, 3000), (34000, 16000)],
        white_point: (15635, 16450),
        max_luminance: 40_000_000,
        min_luminance: 50,
    });
    cfg.content_light.max_cll = Some(1000.0);
    cfg.content_light.max_fall = Some(400.0);
    let enc = Encoder::new(cfg).expect("encoder");
    let vui = enc.sps().vui.expect("vui");
    assert_eq!(vui.chroma_sample_loc, Some((2, 2)));
    let vst = vui.video_signal_type.expect("vst");
    assert!(vst.video_full_range_flag);
    assert_eq!(vst.colour_description.unwrap().transfer_characteristics, 18);
    let coded = enc.encode_idr(&picture10(64, 64, 0)).expect("encode");
    let mut dec = Decoder::new();
    dec.decode_annex_b(&coded.au).expect("decode");
    let au = &coded.au;
    // The mdcv SEI (type 137, size 24) and clli SEI (type 144, size 4) are in the stream.
    let has = |pat: &[u8]| au.windows(pat.len()).any(|w| w == pat);
    assert!(
        has(&[137, 24, 0x33, 0xC2, 0x86, 0xC4]),
        "mdcv with the stated green primary"
    );
    assert!(has(&[144, 4, 0x03, 0xE8, 0x01, 0x90]), "clli 1000/400");
    // And ec-core reads the luminance back.
    let light = ec_core::color::hevc_sei_light(au);
    assert_eq!(light.mastering_max, Some(4000.0));
    assert_eq!(light.max_cll, Some(1000.0));
}

#[test]
fn defaults_write_no_hdr_signalling() {
    let enc = Encoder::new(config(64, 64)).expect("encoder");
    let vui = enc.sps().vui.expect("vui");
    assert!(vui.video_signal_type.is_none() && vui.chroma_sample_loc.is_none());
    let coded = enc.encode_idr(&picture10(64, 64, 0)).expect("encode");
    assert!(
        !coded.au.windows(2).any(|w| w == [137, 24]),
        "no mdcv by default"
    );
}
