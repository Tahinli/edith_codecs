//! Intra-only witness: a VP9 intra-only frame — an INTER frame with
//! `intra_only = 1` and `show_frame = 0` — must decode byte-identically to
//! ffmpeg's libvpx decoder. Its body is a keyframe's (intra mode info, intra
//! prediction, the intra coefficient axis), but it loads its frame context
//! from storage and refreshes only the slots its `refresh_frame_flags`
//! names, so the frames that follow it prove the reconstruction.
//!
//! The fixture is committed under `tests/data/` because no ffmpeg invocation
//! emits an intra-only frame (libvpx's encoder produces them only through its
//! SVC path, which the libvpx-vp9 wrapper does not expose). The generator is
//! recorded in `lanes/vp9refsetup.report.md`; the stream is an ordinary
//! 10-frame 320x240 testsrc2 GOP whose visible keyframe is re-headed as an
//! intra-only inter frame with a matching `refresh_frame_flags`, so its
//! reconstruction must equal the keyframe's and the whole GOP must come out
//! identical to the source.

mod ivf;

use ec_vp9::decode::Decoder;
use ec_vp9_syntax::{FrameType, Vp9Parser};

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/intraonly.ivf")
}

/// ffmpeg's decoded rawvideo byte stream (frame-packed I420, `w`x`h`).
fn ffmpeg_raw_yuv(path: &std::path::Path, w: usize, h: usize) -> Vec<u8> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("ffmpeg on PATH");
    assert!(out.status.success(), "ffmpeg decode failed: {out:?}");
    let frame = w * h + 2 * (w / 2) * (h / 2);
    assert_eq!(
        out.stdout.len() % frame,
        0,
        "ffmpeg output is not frame-aligned"
    );
    out.stdout
}

#[test]
fn intra_only_frame_and_its_successors_match_ffmpeg() {
    let path = fixture();
    let bytes = std::fs::read(&path).expect("tests/data/intraonly.ivf is committed");
    let (_, w, h, frames) = ivf::parse_ivf(&bytes);
    let (w, h) = (w as usize, h as usize);

    // Non-vacuity: the stream must really carry an intra-only frame.
    let mut parser = Vp9Parser::new();
    let mut intra_only = 0usize;
    for f in &frames {
        for sub in ec_vp9_syntax::split(&f.data).unwrap() {
            let hdr = parser.parse_frame(sub).unwrap();
            if hdr.frame_type == FrameType::Inter && hdr.intra_only {
                intra_only += 1;
            }
        }
    }
    assert_eq!(
        intra_only, 1,
        "the fixture must carry exactly one intra-only frame"
    );

    let expected = ffmpeg_raw_yuv(&path, w, h);
    let frame_bytes = w * h + 2 * (w / 2) * (h / 2);
    let mut decoder = Decoder::new();
    let mut shown = 0usize;
    for (i, f) in frames.iter().enumerate() {
        if let Some(pic) = decoder.decode(&f.data).expect("frame decodes") {
            let off = shown * frame_bytes;
            assert!(
                off + frame_bytes <= expected.len(),
                "frame {i}: ffmpeg has fewer frames than we show"
            );
            let want = &expected[off..off + frame_bytes];
            let mut got = Vec::with_capacity(frame_bytes);
            got.extend(pic.y.iter().map(|&v| v as u8));
            got.extend(pic.u.iter().map(|&v| v as u8));
            got.extend(pic.v.iter().map(|&v| v as u8));
            let diff = got.iter().zip(want).position(|(a, b)| a != b);
            assert!(
                diff.is_none(),
                "frame {i}: pixel mismatch at byte {:?} (ours {} ffmpeg {})",
                diff,
                diff.map(|d| got[d]).unwrap_or(0),
                diff.map(|d| want[d]).unwrap_or(0)
            );
            shown += 1;
        }
    }
    assert_eq!(
        shown * frame_bytes,
        expected.len(),
        "shown frame count mismatch"
    );
    // 11 input frames: a visible keyframe, the hidden intra-only frame and 9
    // visible inter frames — 10 shown.
    assert_eq!(shown, 10, "the witness must show past the intra-only frame");
}
