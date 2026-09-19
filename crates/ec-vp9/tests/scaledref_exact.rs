//! Scaled-reference witness: an inter frame whose reference slot holds a
//! picture of a DIFFERENT coded size must scale that reference during
//! prediction (`vp9_setup_scale_factors_for_frame` + the scaled convolve
//! walk) and come out byte-identical to libvpx. `scaledref.ivf` scales
//! 1280x720 -> 640x360 (`x_step_q4 == y_step_q4 == 32`); the paired
//! `scaledref_odd.ivf` is the same stream with the keyframe patched to
//! 1279x719, so the reference is an ODD coded extent and its chroma crop
//! ceils — the interaction with the odd-dimension layout.
//!
//! The fixtures are committed under `tests/data/` because no ffmpeg
//! invocation can produce a size change: libvpx's encoder emits one only
//! through its rate-control resize path (ffmpeg exposes neither
//! `rc_resize_allowed` nor SVC and crashes outright on a changing filter
//! output), and a hand-spliced explicit-size inter frame trips libvpx's own
//! header reparsing. The generator is recorded in
//! `lanes/vp9refsetup.report.md`: a 1280x720 one-pass CBR encode at 200 kbps,
//! whose `vp9_resize_one_pass_cbr` downscales frame 1 to 640x360 against the
//! keyframe.
//!
//! Oracle caveat: ffmpeg locks its rawvideo output size to the FIRST decoded
//! frame's, so a two-size stream cannot be dumped in one pass. Frame 0 is
//! decoded on its own (`-frames:v 1`, identity); frames 2..N are decoded with
//! `select=gte(n,1)` at the small size, where the filtergraph has
//! re-initialised and every frame passes through unchanged. The size-change
//! frame (file frame 1) is the one ffmpeg mangles during that
//! re-initialisation, so it is checked INDIRECTLY: every later frame predicts
//! from its reconstruction, so a wrong scaler fails frames 2..N.

mod ivf;

use ec_vp9::decode::Decoder;
use ec_vp9_syntax::{FrameType, Vp9Parser};

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

/// Run ffmpeg over `path` with `args` and return its rawvideo stdout.
fn ffmpeg_raw(path: &std::path::Path, args: &[&str]) -> Vec<u8> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(path)
        .args(args)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("ffmpeg on PATH");
    assert!(out.status.success(), "ffmpeg decode failed: {out:?}");
    out.stdout
}

fn frame_bytes(w: usize, h: usize) -> usize {
    w * h + 2 * w.div_ceil(2) * h.div_ceil(2)
}

fn assert_planes(pic: &ec_vp9::decode::Picture, want: &[u8], label: &str) {
    let mut got = Vec::with_capacity(want.len());
    got.extend_from_slice(&pic.y);
    got.extend_from_slice(&pic.u);
    got.extend_from_slice(&pic.v);
    assert_eq!(got.len(), want.len(), "{label}: plane size");
    let diff = got.iter().zip(want).position(|(a, b)| a != b);
    assert!(
        diff.is_none(),
        "{label}: pixel mismatch at byte {:?} (ours {} libvpx {})",
        diff,
        diff.map(|d| got[d]).unwrap_or(0),
        diff.map(|d| want[d]).unwrap_or(0)
    );
}

fn check(name: &str) {
    let path = fixture(name);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let (_, cw, ch, frames) = ivf::parse_ivf(&bytes);

    // Non-vacuity: the stream must carry an inter frame whose coded size
    // differs from the frame before it — the only shape that makes a
    // reference scale.
    let mut parser = Vp9Parser::new();
    let mut sizes: Vec<(u32, u32, FrameType)> = Vec::new();
    for f in &frames {
        for sub in ec_vp9_syntax::split(&f.data).unwrap() {
            let hdr = parser.parse_frame(sub).unwrap();
            sizes.push((hdr.width, hdr.height, hdr.frame_type));
        }
    }
    let changed = sizes
        .windows(2)
        .filter(|p| p[0].0 != p[1].0 || p[0].1 != p[1].1)
        .count();
    assert!(
        changed >= 1,
        "{name}: the fixture must change coded size mid-stream (sizes: {sizes:?})"
    );
    assert!(
        sizes
            .iter()
            .any(|s| s.2 == FrameType::Inter && (s.0, s.1) != (cw as u32, ch as u32)),
        "{name}: an inter frame must differ from the container's declared size"
    );

    let mut decoder = Decoder::new();
    let ours: Vec<_> = frames
        .iter()
        .map(|f| decoder.decode(&f.data).expect("frame decodes"))
        .collect();
    let shown: Vec<_> = ours.iter().filter_map(|p| p.as_ref()).collect();
    assert_eq!(shown.len(), 10, "{name}: the fixture has 10 visible frames");

    // Frame 0 (the keyframe) alone; identity.
    let oracle0 = ffmpeg_raw(&path, &["-frames:v", "1"]);
    assert_eq!(
        oracle0.len(),
        frame_bytes(shown[0].width as usize, shown[0].height as usize),
        "{name}: frame 0 oracle size"
    );
    assert_planes(shown[0], &oracle0, &format!("{name} frame 0"));

    // Frames 2..N: after the size change every frame passes through the
    // filtergraph unchanged at the small size.
    let small = (shown[2].width as usize, shown[2].height as usize);
    assert!(
        small != (shown[0].width as usize, shown[0].height as usize),
        "{name}: frames after the change must be resized"
    );
    let oracle_rest = ffmpeg_raw(
        &path,
        &[
            "-vf",
            "select=gte(n\\,1)",
            "-fps_mode",
            "passthrough",
            "-s",
            &format!("{}x{}", small.0, small.1),
        ],
    );
    let fbs = frame_bytes(small.0, small.1);
    assert_eq!(
        oracle_rest.len(),
        fbs * (shown.len() - 2),
        "{name}: ffmpeg must emit frames 2..{}",
        shown.len() - 1
    );
    for (j, pic) in shown[2..].iter().enumerate() {
        assert_eq!(
            (pic.width as usize, pic.height as usize),
            small,
            "{name}: frame {} size",
            j + 2
        );
        assert_planes(
            pic,
            &oracle_rest[j * fbs..(j + 1) * fbs],
            &format!("{name} frame {}", j + 2),
        );
    }
}

#[test]
fn size_changing_reference_decodes_byte_exact() {
    check("scaledref.ivf");
}

/// The same size change with an ODD coded reference: 1279x719 -> 640x360,
/// where the reference's chroma crop ceils (`(1279 + 1) / 2`).
#[test]
fn size_changing_odd_reference_decodes_byte_exact() {
    check("scaledref_odd.ivf");
}
