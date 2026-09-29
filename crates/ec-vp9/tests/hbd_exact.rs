//! Profile-2 high-bit-depth witness: 10-bit 4:2:0 decode is byte-exact
//! against ffmpeg's libvpx (`ffmpeg -v error -i in.ivf -f rawvideo -pix_fmt
//! yuv420p10le -`). Pre-fix the decoder refuses `profile 2` by name, so this
//! test fails at the first frame; post-fix every shown frame matches.
//!
//! Media-gated: the 10-bit corpus streams are gitignored, so the test prints
//! SKIP when the fixture is absent (set the fixture tree up per the lane
//! notes) -- and goes RED instead under `EC_REQUIRE_FIXTURES=1`, so a drifted
//! fixture library can never read as a pass. The 4:4:4 and 12-bit shapes are
//! covered by `subsampling_exact.rs`.

mod ivf;

use ec_vp9::decode::Decoder;
use std::path::{Path, PathBuf};

/// Fixture-presence probe for the 10-bit corpus IVF `profile2_10bit_is_byte_exact`
/// decodes and compares against ffmpeg.
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

fn fixture(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p.join("fixtures/bitstreams").join(name)
}

/// ffmpeg's rawvideo `yuv420p10le` bytes (little-endian u16 samples,
/// stride == visible width).
fn ffmpeg_raw_yuv10(path: &Path) -> Vec<u8> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-"])
        .output()
        .expect("ffmpeg must be on PATH for the witnesses");
    assert!(out.status.success(), "ffmpeg failed on {path:?}");
    out.stdout
}

fn decode_all(path: &Path) -> (Vec<Vec<u8>>, u8) {
    let bytes = std::fs::read(path).unwrap();
    let (_f, _w, _h, frames) = ivf::parse_ivf(&bytes);
    let mut decoder = Decoder::new();
    let mut out = Vec::new();
    let mut bd = 0u8;
    for frame in &frames {
        if let Some(pic) = decoder.decode(&frame.data).expect("decode must succeed") {
            bd = pic.bit_depth;
            let mut v = Vec::with_capacity((pic.y.len() + pic.u.len() + pic.v.len()) * 2);
            for &s in pic.y.iter().chain(pic.u.iter()).chain(pic.v.iter()) {
                v.extend_from_slice(&s.to_le_bytes());
            }
            out.push(v);
        }
    }
    (out, bd)
}

/// Both 10-bit corpus streams decode to the same pixels ffmpeg's libvpx
/// produces, sample for sample.
#[test]
fn profile2_10bit_is_byte_exact() {
    let a = fixture("vp9-1080p-23.976-10bit.ivf");
    if !require_fixture(&a, "scripts/gen-bitstream-fixtures.sh") {
        return;
    }
    let (ours, bd) = decode_all(&a);
    assert_eq!(bd, 10, "the fixture must decode at 10 bits");
    let want = ffmpeg_raw_yuv10(&a);
    assert_eq!(ours.len(), 48, "shown frames");
    let mut off = 0usize;
    for (i, got) in ours.iter().enumerate() {
        let n = got.len();
        let end = off + n;
        assert!(end <= want.len(), "frame {i}: ffmpeg stream too short");
        assert_eq!(
            got.as_slice(),
            &want[off..end],
            "frame {i}: 10-bit planes differ from ffmpeg"
        );
        off = end;
    }
    assert_eq!(off, want.len(), "frame count / plane size mismatch");
}
