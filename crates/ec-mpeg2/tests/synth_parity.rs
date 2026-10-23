#![allow(clippy::needless_range_loop, clippy::field_reassign_with_default, clippy::too_many_arguments, clippy::type_complexity)]
//! Differential test: random-syntax MPEG-2 streams (field pictures, dual
//! prime, 16x8, ...) decoded by us and by ffmpeg must agree byte for byte.

mod common;
use common::synth::{SynthCfg, generate};
use common::*;

fn run_one(tag: &str, cfg: &SynthCfg, pix: &str) -> Result<usize, String> {
    let dir = scratch(tag);
    let es = generate(cfg);
    let path = dir.join("syn.m2v");
    std::fs::write(&path, &es).map_err(|e| e.to_string())?;
    compare(tag, &path, pix, &[])
}

#[test]
fn synthetic_streams_match_ffmpeg() {
    if !have_ffmpeg() {
        assert!(!require(), "ffmpeg missing");
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let n: u64 = std::env::var("EC_SYNTH_N").ok().and_then(|v| v.parse().ok()).unwrap_or(24);
    let base: u64 = std::env::var("EC_SYNTH_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    let heavy = std::env::var_os("EC_SYNTH_LIGHT").is_none();
    let mut failures = Vec::new();
    for i in 0..n {
        let seed = base + i;
        let cfg = SynthCfg {
            mb_w: 3 + (seed % 3) as usize,
            mb_h: 2 + 2 * ((seed / 3) % 2) as usize,
            chroma422: seed % 5 == 4,
            progressive_sequence: seed % 7 == 6,
            frames: 6,
            seed,
            fields: heavy || std::env::var_os("EC_SYNTH_FIELDS").is_some(),
            heavy,
            load_matrices: seed.is_multiple_of(2),
        };
        let pix = if cfg.chroma422 { "yuv422p" } else { "yuv420p" };
        let tag = format!("syn{seed}");
        match run_one(&tag, &cfg, pix) {
            Ok(n) => eprintln!("OK   {tag} ({n} frames)"),
            Err(e) => {
                eprintln!("FAIL {tag}: {e}");
                failures.push(seed);
            }
        }
    }
    assert!(failures.is_empty(), "mismatching seeds: {failures:?}");
}
