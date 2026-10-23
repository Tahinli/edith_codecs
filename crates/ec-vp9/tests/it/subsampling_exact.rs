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

use crate::ivf;
use crate::common::require_fixture;

use ec_vp9::decode::Decoder;
use std::path::{Path, PathBuf};

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
            .as_chunks::<2>().0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
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
        if let Some(pic) = decoder
            .decode(&frame.data)
            .unwrap_or_else(|e| panic!("{}: frame {i}: {e}", path.display()))
        {
            assert_eq!(pic.bit_depth, bd, "{}: frame {i} bit depth", path.display());
            ours.extend(pic.y.iter().copied());
            ours.extend(pic.u.iter().copied());
            ours.extend(pic.v.iter().copied());
            shown += 1;
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

/// Old per-case test names, fixture, ffmpeg `pix_fmt`, bit depth.
const PROFILE_CASES: &[(&str, &str, &str, u8)] = &[
    ("profile1_444_8bit_is_byte_exact", "vp9-profile1-444.ivf", "yuv444p", 8),
    ("profile3_444_10bit_is_byte_exact", "vp9-profile3-444-10bit.ivf", "yuv444p10le", 10),
    ("profile3_444_12bit_is_byte_exact", "vp9-profile3-444-12bit.ivf", "yuv444p12le", 12),
    ("profile1_422_8bit_is_byte_exact", "vp9-profile1-422.ivf", "yuv422p", 8),
    ("profile3_422_10bit_is_byte_exact", "vp9-profile3-422-10bit.ivf", "yuv422p10le", 10),
    ("profile1_440_8bit_is_byte_exact", "vp9-profile1-440.ivf", "yuv440p", 8),
    ("profile3_440_10bit_is_byte_exact", "vp9-profile3-440-10bit.ivf", "yuv440p10le", 10),
    ("profile2_420_12bit_is_byte_exact", "vp9-profile2-420-12bit.ivf", "yuv420p12le", 12),
];

/// Runs every profile/subsampling/bit-depth case; a failing case does not
/// stop the others, and all failing case names are reported together.
#[test]
fn profile_matrix_is_byte_exact() {
    let mut failures = Vec::new();
    for &(name, file, pix_fmt, bd) in PROFILE_CASES {
        let case = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            check(&data(file), pix_fmt, bd);
        }));
        if case.is_err() {
            failures.push(name);
        }
    }
    assert!(failures.is_empty(), "failed cases: {failures:?}");
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
