//! Helpers shared by the integration-test modules of the `it` binary.

/// Fixture-presence probe for the committed-or-media-gated fixtures a test
/// decodes. Returns whether the fixture is present, but never silently: under
/// `EC_REQUIRE_FIXTURES=1` an absent fixture is a hard failure naming the path
/// and the script that regenerates it, so a host whose fixture library drifted
/// reports RED instead of a green SKIP (class: gate-skips-on-its-own-failure;
/// model `have_ffmpeg` in crates/ec-av1/src/stream.rs, which was silently
/// short-circuited because the probe ran first in a compound `if`).
///
/// Order is load-bearing: probe, assert, return. Never merge the probe into
/// the same `if` as the escape.
pub(crate) fn require_fixture(path: &std::path::Path, generator: &str) -> bool {
    let present = path.exists();
    assert!(
        present || std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
        "EC_REQUIRE_FIXTURES=1 but fixture {} is absent -- regenerate with: {}",
        path.display(),
        generator
    );
    if !present {
        eprintln!("SKIP: fixture {} absent", path.display());
    }
    present
}

/// ffmpeg's decoded rawvideo byte stream (frame-packed I420, `w`x`h`).
pub(crate) fn ffmpeg_raw_yuv(path: &std::path::Path, w: usize, h: usize) -> Vec<u8> {
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
