//! Non-4:2:0 subsampling and 12-bit witnesses: profiles 1 and 3 (4:4:4 /
//! 4:2:2 / 4:4:0 at 8 and 10 bits) and profile 2/3 12-bit decode byte-exact
//! against ffmpeg's libvpx.
//!
//! Every fixture under `tests/data/` is committed (see the lane report for
//! the exact generator commands), so these cases always run; the two
//! `corpus_*` cases additionally cover the media-gated 4:4:4 corpus streams
//! and print SKIP when the fixture is absent -- RED instead under
//! `EC_REQUIRE_FIXTURES=1`, so a drifted fixture library can never read as a
//! pass.
//!
//! The comparison is the whole flattened plane stream (u16 samples) against
//! ffmpeg's `rawvideo` output for the matching `pix_fmt` — full-resolution
//! chroma for 4:4:4, half-width for 4:2:2, half-height for 4:4:0, and the
//! 16-bit LE sample layout for 10/12-bit.

mod ivf;

use ec_vp9::decode::Decoder;
use std::path::{Path, PathBuf};

/// Fixture-presence probe for the media-gated 4:4:4 corpus IVFs the `corpus_*`
/// cases decode (`corpus()` below, and any path a `scripts/*` generator makes).
/// Returns whether the fixture is present, but never silently: under
/// `EC_REQUIRE_FIXTURES=1` an absent fixture is a hard failure naming the path
/// and the script that regenerates it, so a host whose fixture library drifted
/// reports RED instead of a green SKIP (class: gate-skips-on-its-own-failure;
/// model `have_ffmpeg` in crates/ec-av1/src/stream.rs, which was silently
/// short-circuited because the probe ran first in a compound `if`).
///
/// Order is load-bearing: probe, assert, return. Never merge the probe into
/// the same `if` as the escape.
fn require_fixture(path: &std::path::Path, generator: &str) -> bool {
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

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn corpus(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p.join("fixtures/bitstreams").join(name)
}

/// ffmpeg's decoded samples as a flat u16 stream (8-bit zero-extended,
/// 10/12-bit little-endian pairs).
fn ffmpeg_samples(path: &Path, pix_fmt: &str, bd: u8) -> Vec<u16> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
        .output()
        .expect("ffmpeg must be on PATH for the witnesses");
    assert!(out.status.success(), "ffmpeg failed on {path:?}");
    if bd == 8 {
        out.stdout.iter().map(|&b| b as u16).collect()
    } else {
        out.stdout
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect()
    }
}

fn check(path: &Path, pix_fmt: &str, bd: u8) {
    let bytes = std::fs::read(path).expect("committed fixture");
    let (_, _, _, frames) = ivf::parse_ivf(&bytes);
    let want = ffmpeg_samples(path, pix_fmt, bd);

    let mut decoder = Decoder::new();
    let mut ours: Vec<u16> = Vec::new();
    let mut shown = 0usize;
    for (i, frame) in frames.iter().enumerate() {
        match decoder
            .decode(&frame.data)
            .unwrap_or_else(|e| panic!("{}: frame {i}: {e}", path.display()))
        {
            Some(pic) => {
                assert_eq!(pic.bit_depth, bd, "{}: frame {i} bit depth", path.display());
                ours.extend(pic.y.iter().copied());
                ours.extend(pic.u.iter().copied());
                ours.extend(pic.v.iter().copied());
                shown += 1;
            }
            None => {}
        }
    }
    assert!(shown >= 2, "{}: the stream shows frames", path.display());
    assert_eq!(
        ours.len(),
        want.len(),
        "{}: plane stream length",
        path.display()
    );
    if let Some(i) = ours.iter().zip(&want).position(|(a, b)| a != b) {
        panic!(
            "{}: sample {i} differs (ours {} want {})",
            path.display(),
            ours[i],
            want[i]
        );
    }
}

#[test]
fn profile1_444_8bit_is_byte_exact() {
    check(&data("vp9-profile1-444.ivf"), "yuv444p", 8);
}

#[test]
fn profile3_444_10bit_is_byte_exact() {
    check(&data("vp9-profile3-444-10bit.ivf"), "yuv444p10le", 10);
}

#[test]
fn profile3_444_12bit_is_byte_exact() {
    check(&data("vp9-profile3-444-12bit.ivf"), "yuv444p12le", 12);
}

#[test]
fn profile1_422_8bit_is_byte_exact() {
    check(&data("vp9-profile1-422.ivf"), "yuv422p", 8);
}

#[test]
fn profile3_422_10bit_is_byte_exact() {
    check(&data("vp9-profile3-422-10bit.ivf"), "yuv422p10le", 10);
}

#[test]
fn profile1_440_8bit_is_byte_exact() {
    check(&data("vp9-profile1-440.ivf"), "yuv440p", 8);
}

#[test]
fn profile3_440_10bit_is_byte_exact() {
    check(&data("vp9-profile3-440-10bit.ivf"), "yuv440p10le", 10);
}

#[test]
fn profile2_420_12bit_is_byte_exact() {
    check(&data("vp9-profile2-420-12bit.ivf"), "yuv420p12le", 12);
}

#[test]
fn corpus_profile1_444_matches_ffmpeg() {
    let p = corpus("vp9-profile1-444.ivf");
    if !require_fixture(&p, "scripts/gen-bitstream-fixtures.sh") {
        return;
    }
    check(&p, "yuv444p", 8);
}

#[test]
fn corpus_profile3_444_10bit_matches_ffmpeg() {
    let p = corpus("vp9-profile3-444-10bit.ivf");
    if !require_fixture(&p, "scripts/gen-bitstream-fixtures.sh") {
        return;
    }
    check(&p, "yuv444p10le", 10);
}
