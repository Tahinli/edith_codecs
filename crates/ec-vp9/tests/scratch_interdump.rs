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

#[test]
fn inter_syntax_dump() {
    let path = std::env::var("INTER_IVF").unwrap_or_else(|_| "/tmp/inter1.ivf".to_string());
    let expect_raw = std::env::var("INTER_EXPECT").unwrap_or_else(|_| "1213,1851".to_string());
    let expect: Vec<usize> = if expect_raw == "none" {
        Vec::new()
    } else {
        expect_raw
            .split(',')
            .map(|t| t.trim().parse().expect("INTER_EXPECT entries are block counts"))
            .collect()
    };
    let Ok(bytes) = std::fs::read(&path) else {
        println!("SKIP: fixture {path} not found - set INTER_IVF=<file> to run");
        return;
    };
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
