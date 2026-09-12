//! M4 witness: the whole-stream public API (`ec_vp8::stream`) decodes
//! every fixture byte-exact against ffmpeg and refuses malformed
//! containers with errors, not panics.

mod ivf;

use ec_vp8::decode::Decoder;
use ec_vp8::stream::decode_stream;

/// Every IVF fixture in the witness set (the WebP still is covered by
/// `keyframe_exact::vp8_in_webp_still_matches_ffmpeg`).
const FIXTURES: &[&str] = &[
    "kf-16x16-black-q0.ivf",
    "kf-32x16-q35.ivf",
    "kf-76x52-q30.ivf",
    "kf-96x80-q63.ivf",
    "kf-160x96-q20.ivf",
    "kf-160x96-q40.ivf",
    "kf-172x144-q25.ivf",
    "gop-96x80-v1.ivf",
    "gop-160x96.ivf",
    "mparts-160x96.ivf",
    "altref-160x96.ivf",
    "clip-obs-320x192.ivf",
];

#[test]
fn decode_stream_matches_ffmpeg_on_every_fixture() {
    let Some(dir) = ivf::fixture_dir() else {
        panic!("fixtures missing; run scripts/gen_vp8_fixtures.sh");
    };
    for name in FIXTURES {
        let path = dir.join(name);
        let bytes = std::fs::read(&path).unwrap();
        let (_, w, h, frames) = ivf::parse_ivf(&bytes);
        let all = ffmpeg_raw_yuv(&path);
        let frame_len = usize::from(w) * usize::from(h) * 3 / 2;
        let shown = frames.iter().filter(|f| ivf::show_frame(&f.data)).count();
        assert_eq!(all.len(), shown * frame_len, "{name}: ffmpeg output size");
        assert!(!frames[0].data.is_empty(), "{name}: empty first frame");

        let pictures =
            decode_stream(&bytes).unwrap_or_else(|e| panic!("{name}: decode failed: {e}"));
        assert_eq!(pictures.len(), shown, "{name}: picture count (shown only)");
        for (i, pic) in pictures.iter().enumerate() {
            let expect = &all[i * frame_len..(i + 1) * frame_len];
            let (cw, ch) = (usize::from(w + 1) / 2, usize::from(h + 1) / 2);
            assert_eq!(pic.y, &expect[..usize::from(w) * usize::from(h)], "{name} frame {i}: Y");
            assert_eq!(pic.u, &expect[usize::from(w) * usize::from(h)..][..cw * ch], "{name} frame {i}: U");
            assert_eq!(pic.v, &expect[usize::from(w) * usize::from(h) + cw * ch..], "{name} frame {i}: V");
        }
    }
}

/// The hidden-frame contract, end to end on a REAL auto-alt-ref stream:
/// `Decoder::decode` returns `Ok(None)` for a hidden frame while its
/// reference content stays live — the frames that follow (which the
/// encoder predicted across the hidden altref) stay byte-exact against
/// ffmpeg.
#[test]
fn hidden_frames_update_references_and_produce_no_picture() {
    let Some(dir) = ivf::fixture_dir() else {
        panic!("fixtures missing; run scripts/gen_vp8_fixtures.sh");
    };
    let path = dir.join("altref-160x96.ivf");
    let bytes = std::fs::read(&path).unwrap();
    let (_, w, h, frames) = ivf::parse_ivf(&bytes);
    let hidden_at: Vec<usize> = frames
        .iter()
        .enumerate()
        .filter(|(_, f)| !ivf::show_frame(&f.data))
        .map(|(i, _)| i)
        .collect();
    assert!(
        hidden_at.len() >= 6,
        "altref-160x96.ivf lost its hidden frames ({}) — regenerate",
        hidden_at.len()
    );

    // ffmpeg's display stream, shown-frame-indexed.
    let all = ffmpeg_raw_yuv(&path);
    let frame_len = usize::from(w) * usize::from(h) * 3 / 2;
    let mut dec = Decoder::new();
    let mut shown_seen = 0;
    for (i, f) in frames.iter().enumerate() {
        let pic = dec.decode(&f.data).expect("hidden frame decodes");
        if ivf::show_frame(&f.data) {
            let expect = &all[shown_seen * frame_len..(shown_seen + 1) * frame_len];
            let y = &pic.as_ref().expect("shown frame has picture").y;
            assert_eq!(y, &expect[..usize::from(w) * usize::from(h)], "frame {i}: Y");
            // The frame right after the FIRST hidden altref must already
            // be byte-exact: it predicts across the hidden reference.
            if i == hidden_at[0] + 1 {
                assert!(
                    y[..32] == expect[..32] && y == &expect[..y.len()],
                    "frame after hidden altref diverges"
                );
            }
            shown_seen += 1;
        } else {
            assert!(pic.is_none(), "hidden frame {i} produced a picture");
        }
    }
    assert_eq!(shown_seen * frame_len, all.len());
}

/// ffmpeg's decoded rawvideo byte stream for an IVF file.
fn ffmpeg_raw_yuv(path: &std::path::Path) -> Vec<u8> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("ffmpeg must be on PATH for the witnesses");
    assert!(out.status.success(), "ffmpeg failed on {path:?}");
    out.stdout
}

#[test]
fn decode_stream_is_error_not_panic_on_bad_containers() {
    let Some(dir) = ivf::fixture_dir() else {
        panic!("fixtures missing; run scripts/gen_vp8_fixtures.sh");
    };
    let bytes = std::fs::read(dir.join("gop-160x96.ivf")).unwrap();

    // Missing magic.
    let mut bad = bytes.clone();
    bad[0..4].copy_from_slice(b"JUNK");
    assert!(decode_stream(&bad).is_err(), "non-DKIF accepted");

    // Wrong fourcc (a WebP-lossless VP8L tag is not a VP8 video stream).
    let mut bad = bytes.clone();
    bad[8..12].copy_from_slice(b"VP8L");
    assert!(decode_stream(&bad).is_err(), "non-VP80 fourcc accepted");

    // A frame header claiming more payload than the stream carries.
    let mut bad = bytes.clone();
    bad[32..36].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decode_stream(&bad).is_err(), "truncated frame accepted");
}

