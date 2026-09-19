//! Inter-frame pixel witness: a GOP (key frame + inter frames, with and
//! without tile columns) must decode byte-identically to ffmpeg's libvpx
//! decoder, whose rawvideo output is the oracle. Fixtures are generated
//! deterministically with ffmpeg, so the witness is self-contained.
//!
//! The 1080p private streams the lane developed against are compared through
//! `tests/scratch_pixdump.rs` (env-gated) instead; see `lanes/vp9pix.report.md`.

mod ivf;

use ec_vp9::decode::Decoder;

/// Generate a GOP fixture with libvpx: 6 frames of testsrc2 at 320x240,
/// `-g 10` so frames 1..5 are inter frames.
fn ensure_fixture(path: &std::path::Path, tile_columns: u32) {
    if path.exists() {
        return;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    let st = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=24:duration=0.25",
            "-c:v",
            "libvpx-vp9",
            "-g",
            "10",
            "-crf",
            "32",
            "-deadline",
            "good",
            "-cpu-used",
            "4",
            "-tile-columns",
        ])
        .arg(tile_columns.to_string())
        .args(["-pix_fmt", "yuv420p", "-f", "ivf"])
        .arg(path)
        .output()
        .expect("ffmpeg with libvpx-vp9 must be on PATH for the witnesses");
    assert!(st.status.success(), "fixture generation failed: {st:?}");
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

/// Decode every frame with our decoder and compare each SHOWN frame's planes
/// against ffmpeg's stream; returns the number of shown frames compared.
fn compare(path: &std::path::Path) -> usize {
    let bytes = std::fs::read(path).unwrap();
    let (_, w, h, frames) = ivf::parse_ivf(&bytes);
    let (w, h) = (w as usize, h as usize);
    let expected = ffmpeg_raw_yuv(path, w, h);
    let frame_bytes = w * h + 2 * (w / 2) * (h / 2);

    let mut decoder = Decoder::new();
    let mut shown = 0usize;
    let mut seen_inter = false;
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
            assert_eq!(
                got.len(),
                frame_bytes,
                "frame {i}: our planes are {w}x{h} I420"
            );
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
        if i > 0 {
            seen_inter = true;
        }
    }
    assert_eq!(shown * frame_bytes, expected.len(), "frame count mismatch");
    assert!(seen_inter, "the fixture must contain inter frames");
    shown
}

#[test]
fn inter_gop_matches_ffmpeg() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/vp9-witness");
    let path = dir.join("inter-gop.ivf");
    ensure_fixture(&path, 0);
    assert!(compare(&path) >= 2);
}

#[test]
fn inter_gop_with_tile_columns_matches_ffmpeg() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/vp9-witness");
    let path = dir.join("inter-gop-tc1.ivf");
    ensure_fixture(&path, 1);
    assert!(compare(&path) >= 2);
}

/// Real-content regression for the last-partial-SB-row context bug: this
/// 120-frame 1080p stream used to abort at input frame 72 with
/// `tile bool decoder desync`, because a chroma transform block whose
/// write-back window runs past the frame bottom edge kept the window's
/// TAIL slot set (`write_tx_context`), and the next row of blocks read it
/// as an entropy context. Every shown frame must match ffmpeg.
///
/// Media-gated: the corpus fixture is not versioned, so the witness skips
/// when `fixtures/bitstreams/vp9-1080p-60-8bit.ivf` is absent.
#[test]
fn corpus_1080p_60fps_matches_ffmpeg_past_frame_72() {
    let Some(dir) = ivf::fixture_dir() else {
        println!("SKIP: fixtures/bitstreams absent (media-gated witness)");
        return;
    };
    let path = dir.join("vp9-1080p-60-8bit.ivf");
    if !path.exists() {
        println!("SKIP: {} absent (media-gated witness)", path.display());
        return;
    }
    let shown = compare(&path);
    assert!(
        shown >= 73,
        "the witness must decode past input frame 72 (got {shown} shown frames)"
    );
}
