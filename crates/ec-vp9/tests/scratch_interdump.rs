//! [scratch] Inter-frame syntax dump + count gate.
//!
//! Parses every frame of `INTER_IVF` (default `/tmp/inter1.ivf`, the clean
//! single-tile stream the pinned counts refer to). Keyframes decode as usual;
//! inter frames are parsed syntax-only (no reconstruction). With
//! `EC_VP9_INTERDUMP=1` every block prints one `MODE` line in the oracle's
//! shape, for a by-index diff. Point it at the two-tile-column stream
//! (`INTER_IVF=/tmp/inter.ivf INTER_EXPECT=none`) to exercise the failing-
//! keyframe path: that stream's keyframe desyncs its tile reader and the test
//! is expected to fail there.
//!
//! This is a real gate, not a printer:
//! - any frame error fails the test (keyframe failures are propagated too,
//!   because a half-updated frame context would make later dumps meaningless);
//! - every inter frame's mode-info block count must equal `INTER_EXPECT`
//!   (default `1213,1851` - the pinned counts of the single-tile fixture
//!   `/tmp/inter1.ivf`, sha256 376532734662ba5faba4c4e97a5aac915ce429c11701bcf6
//!   6439b61a77388fa8; set `INTER_EXPECT=none` for a different stream);
//! - the crate already asserts `overreads() == 0` per tile reader at the end of
//!   each tile (decode.rs, "tile bool decoder desync"), which is what would fire
//!   if the walk read past the tile's last bool.
//!
//! ```text
//! INTER_IVF=<file> EC_VP9_INTERDUMP=1 \
//!   cargo test -p ec-vp9 --test scratch_interdump -- --nocapture
//! ```
mod ivf;

use ec_vp9::decode::Decoder;
/// Fixture-presence probe for the inter-frame IVF this gate parses (the
/// `INTER_IVF` path, or its generated `/tmp/inter1.ivf` default).
/// Returns whether the fixture is present, but never silently: under
/// `EC_REQUIRE_FIXTURES=1` an absent fixture is a hard failure naming the path
/// and how to regenerate it, so a host whose fixture library drifted reports
/// RED instead of a green SKIP (class: gate-skips-on-its-own-failure; model
/// `have_ffmpeg` in crates/ec-av1/src/stream.rs, which was silently
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

#[test]
fn inter_syntax_dump() {
    // An explicitly set INTER_IVF that does not exist is a failure; only the
    // unset default may fall back to a loud SKIP (the fixture is generated,
    // not committed).
    let explicit = std::env::var("INTER_IVF").ok();
    let path = explicit
        .clone()
        .unwrap_or_else(|| "/tmp/inter1.ivf".to_string());
    let expect_raw = std::env::var("INTER_EXPECT").unwrap_or_else(|_| "1213,1851".to_string());
    let expect: Vec<usize> = if expect_raw == "none" {
        Vec::new()
    } else {
        expect_raw
            .split(',')
            .map(|t| {
                t.trim()
                    .parse()
                    .expect("INTER_EXPECT entries are block counts")
            })
            .collect()
    };
    if !require_fixture(
        std::path::Path::new(&path),
        "INTER_IVF=<file>; the default is generated, not committed \
         (ffmpeg -f lavfi -i testsrc2=... -c:v libvpx-vp9 -f ivf)",
    ) {
        assert!(
            explicit.is_none(),
            "INTER_IVF={path} was set explicitly but the file does not exist"
        );
        return;
    }
    let bytes = std::fs::read(&path).expect("fixture presence was just probed");
    let (_fourcc, w, h, frames) = ivf::parse_ivf(&bytes);
    println!("IVF {w}x{h} frames={}", frames.len());
    let mut dec = Decoder::new();
    let mut counts = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        match dec.decode_syntax(&f.data) {
            Ok(()) => {
                let n = dec.last_frame_blocks();
                println!("FRAME {i} parsed blocks={n}");
                if i > 0 {
                    counts.push(n);
                }
            }
            Err(e) => panic!("FRAME {i} failed: {e:?}"),
        }
    }
    if !expect.is_empty() {
        assert_eq!(
            counts, expect,
            "inter-frame mode-info block counts differ from INTER_EXPECT"
        );
        println!("block-count gate passed for {counts:?}");
    }
}
