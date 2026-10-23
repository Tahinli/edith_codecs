//! bench: zlib inflate rows for lanes/bench.md ("lane benchrest").
//!
//! usage:
//!   bench zlib <in.zlib> <out.bin> [reps]
//!   bench blocks <in.blocks> <out.bin> [reps]
//!
//! `zlib` inflates one zlib stream with `inflate_zlib`, `reps` times (default 5), and keeps the
//! best wall time. `blocks` reads a sequence of `[u32 little-endian length][zlib stream]` records
//! (one per subtitle-sized packet) and inflates every record with `inflate_zlib`, timing the whole
//! loop. Both write the concatenated output of the last pass to `<out.bin>`, untimed, for the md5
//! check against python's `zlib.decompress`.
use std::hint::black_box;
use std::time::Instant;

/// Output ceiling passed to every call: far above the inputs used here, but not "no ceiling".
const LIMIT: usize = 1 << 30;

fn split_blocks(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < data.len() {
        let len = u32::from_le_bytes(data[at..at + 4].try_into().expect("4 bytes")) as usize;
        at += 4;
        out.push(&data[at..at + len]);
        at += len;
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some(mode @ ("zlib" | "blocks")) if args.len() >= 4 => {
            let reps: usize = args.get(4).map_or(5, |s| s.parse().expect("reps"));
            assert!(reps >= 1, "reps must be at least 1");
            let data = std::fs::read(&args[2]).expect("read input");
            let blocks = if mode == "blocks" { split_blocks(&data) } else { Vec::new() };
            let mut best = f64::MAX;
            let mut out = Vec::new();
            for _ in 0..reps {
                let t = Instant::now();
                out = Vec::new();
                if mode == "zlib" {
                    out = ec_inflate::inflate_zlib(black_box(&data), LIMIT).expect("inflate");
                } else {
                    for b in &blocks {
                        let v = ec_inflate::inflate_zlib(black_box(b), LIMIT).expect("inflate block");
                        out.extend_from_slice(&v);
                    }
                }
                best = best.min(t.elapsed().as_secs_f64());
            }
            std::fs::write(&args[3], &out).expect("write output");
            let streams = if mode == "blocks" { blocks.len() } else { 1 };
            println!(
                "mode={mode} streams={streams} in_bytes={} out_bytes={} best_inflate_s={best:.4} mb_per_s={:.2} reps={reps}",
                data.len(),
                out.len(),
                out.len() as f64 / best / 1e6
            );
        }
        _ => {
            eprintln!(
                "usage: bench zlib <in.zlib> <out.bin> [reps]\n       bench blocks <in.blocks> <out.bin> [reps]"
            );
            std::process::exit(2);
        }
    }
}
