//! lane-h265rdoq r3: the sub-block frozen-prefix cursor must not change a
//! single output byte. Hashes two natural-content encodes (1920x1080 and
//! 416x240) with FNV-1a; the number itself means nothing, only that it is the
//! same before and after this round's change to `rdoq()` (checked by hand:
//! stash `residual.rs`, rerun, compare).

mod common;

use common::natural_frame;
use ec_h265::encoder::{Encoder, EncoderConfig, RateControl};

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn encode_hash(width: u32, height: u32) -> u64 {
    let mut cfg = EncoderConfig::new(width, height);
    cfg.rate_control = RateControl::ConstantQp(24);
    cfg.threads = 1;
    let encoder = Encoder::new(cfg).expect("encoder");
    let mut all = Vec::new();
    for i in 0..3u32 {
        let frame = natural_frame(width, height, i * 5);
        all.extend(encoder.encode_idr(&frame).expect("encode").au);
    }
    fnv1a(&all)
}

#[test]
fn rdoq_cache_hash_1920x1080() {
    println!("hash 1920x1080 = {:016x}", encode_hash(1920, 1080));
}

#[test]
fn rdoq_cache_hash_416x240() {
    println!("hash 416x240 = {:016x}", encode_hash(416, 240));
}

/// lane-h265simd: pinned bit-identity gate for the transform's hot path. The
/// value is the same hash the two tests above print, verified by hand
/// (`git stash` the transform.rs change, rerun, compare) to be identical
/// before and after replacing `m32()`'s per-element branch with a
/// compile-time-mirrored flat table in `forward_1d`/`inverse_1d` — this pins
/// that equality so a future change to the hot path is caught if it ever
/// produces a different byte.
///
/// lane-h265prune: re-pinned after the RQT split trial gained an all-zero-CBF
/// early out (mirrors the CU-quadtree's existing `last_cu_empty` skip one
/// level down at the transform tree): a coding unit whose luma+chroma already
/// quantised to nothing is committed as one leaf transform unit without
/// trying the four-child split first, since splitting a zero-SSD block can
/// only add split_transform_flag and mode-signalling bits. This changes the
/// bitstream on any content where that trial used to flip a leaf to split
/// (BD-gated, not bit-identical — see `EncoderConfig::rqt` and this crate's
/// `bd_psnr_vs_x265`).
///
/// lane-h265rdoq: re-pinned after RDOQ's trial costing switched from exact
/// per-trial CABAC replay to an estimated per-context fracbits table
/// (`EncoderConfig::rdoq_estimate`, now on by default) — deliberately
/// bit-different, not exact-mode-preserving; see that field's doc comment
/// for the BD/speed numbers that justified flipping the default.
#[test]
fn rdoq_cache_hash_is_pinned() {
    assert_eq!(encode_hash(416, 240), 0x3dc2876c9104b7b3);
    assert_eq!(encode_hash(1920, 1080), 0x03188155f21c2174);
}
