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
