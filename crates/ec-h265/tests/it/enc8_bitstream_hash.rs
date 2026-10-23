//! lane hevcmain10enc: the 8-bit bitstream is pinned. These hashes were taken on main
//! before the Main 10 colour/SEI additions and must not move: same input and config,
//! same bytes. Covers intra-only, P sequences, constant QP and bitrate mode, 1 and 4
//! threads, on `test_frame` (textured) and `natural_frame` (camera-like).

use crate::common::{fnv1a, natural_frame, test_frame};
use ec_core::frame::VideoFrame;
use ec_h265::encoder::{Encoder, EncoderConfig, RateControl};

fn run(w: u32, h: u32, rc: RateControl, keyint: usize, threads: usize, natural: bool) -> u64 {
    let mut cfg = EncoderConfig::new(w, h);
    cfg.rate_control = rc;
    cfg.keyint = keyint;
    cfg.threads = threads;
    let enc = Encoder::new(cfg).expect("encoder");
    let frames: Vec<VideoFrame> = (0..6u32)
        .map(|i| {
            if natural {
                natural_frame(w, h, i * 3)
            } else {
                test_frame(w, h, i * 3)
            }
        })
        .collect();
    let coded = if keyint > 1 {
        enc.encode_sequence(&frames).expect("encode")
    } else {
        enc.encode_batch(frames.iter()).expect("encode")
    };
    let all: Vec<u8> = coded.iter().flat_map(|p| p.au.clone()).collect();
    fnv1a(&all)
}

#[test]
fn eight_bit_bitstreams_are_unchanged() {
    let cases: [(&str, u64, u64); 6] = [
        (
            "intra qp24 test 1t",
            run(160, 96, RateControl::ConstantQp(24), 1, 1, false),
            0x44f740250df77771,
        ),
        (
            "intra qp30 natural 4t",
            run(192, 128, RateControl::ConstantQp(30), 1, 4, true),
            0x97a499142c7c87d9,
        ),
        (
            "gop4 qp26 test 1t",
            run(160, 96, RateControl::ConstantQp(26), 4, 1, false),
            0x94651ca52f27a5f7,
        ),
        (
            "gop4 qp32 natural 4t",
            run(192, 128, RateControl::ConstantQp(32), 4, 4, true),
            0x6bb6758782241aed,
        ),
        (
            "gop6 rc natural 1t",
            run(
                192,
                128,
                RateControl::Bitrate {
                    kbps: 400,
                    fps: 30.0,
                },
                6,
                1,
                true,
            ),
            0x6ec4111ae53fc865,
        ),
        (
            "gop8 rc test 4t",
            run(
                160,
                96,
                RateControl::Bitrate {
                    kbps: 300,
                    fps: 30.0,
                },
                8,
                4,
                false,
            ),
            0xaea3cfda3d2c60f8,
        ),
    ];
    let mut bad = false;
    for (name, got, want) in cases {
        println!("{name}: {got:016x}");
        bad |= got != want;
    }
    assert!(!bad, "8-bit bitstream hash changed");
}
