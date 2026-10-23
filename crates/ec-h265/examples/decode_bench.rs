//! Decode benchmark for ec-h265: one raw Annex-B H.265 file in, ms/frame out.
//!
//! Run (release, from the repo root; fixtures from `scripts/gen-hevcperf-fixtures.sh`):
//!
//! ```text
//! cargo run -p ec-h265 --release --example decode_bench -- \
//!     ~/.cache/hevcperf-fixtures/hevc1080_moving.hevc
//! ```
//!
//! Decodes the whole file `EC_BENCH_RUNS` times (default 3) and prints the best run
//! as ms/frame and the realtime factor against the stream's frame rate (30 fps unless
//! `EC_BENCH_FPS` says otherwise). The FNV-1a checksum over every output plane is
//! printed too, so a speed change that alters any sample shows up here before the
//! conformance suite runs.

use std::time::Instant;

use ec_h265::Decoder;

fn fnv1a(hash: &mut u64, samples: &[u16]) {
    for &s in samples {
        for b in s.to_le_bytes() {
            *hash ^= u64::from(b);
            *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

/// Decode the whole stream once. Returns the frame count, and the output checksum when
/// `want_hash` is set. The checksum is kept out of the timed runs (it costs more than some
/// stages), so the timed runs only time the decoder.
fn decode_once(data: &[u8], want_hash: bool) -> Result<(usize, u64), String> {
    let mut dec = Decoder::new();
    dec.decode_annex_b(data)
        .map_err(|e| format!("decode: {e}"))?;
    dec.flush().map_err(|e| format!("flush: {e}"))?;
    let mut frames = 0usize;
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    while let Some(frame) = dec.next_frame() {
        if want_hash {
            let p = &frame.picture;
            fnv1a(&mut hash, &p.y);
            fnv1a(&mut hash, &p.cb);
            fnv1a(&mut hash, &p.cr);
        }
        frames += 1;
    }
    Ok((frames, hash))
}

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: decode_bench <file.hevc>");
        std::process::exit(2);
    };
    let runs: usize = std::env::var("EC_BENCH_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3)
        .max(1);
    let fps: f64 = std::env::var("EC_BENCH_FPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30.0);
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("decode_bench: cannot read {path}: {e}");
            std::process::exit(2);
        }
    };

    let mut best = f64::INFINITY;
    let mut frames = 0usize;
    for _ in 0..runs {
        let t = Instant::now();
        match decode_once(&data, false) {
            Ok((n, _)) => frames = n,
            Err(e) => {
                eprintln!("decode_bench: {e}");
                std::process::exit(1);
            }
        }
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
    }
    let checksum = match decode_once(&data, true) {
        Ok((_, h)) => h,
        Err(e) => {
            eprintln!("decode_bench: {e}");
            std::process::exit(1);
        }
    };
    let ms_per_frame = best / frames.max(1) as f64;
    let threads = std::env::var("EC_H265_THREADS").unwrap_or_else(|_| "default".into());
    println!(
        "{path}: frames={frames} threads={threads} best_of={runs} ms_total={best:.1} \
         ms_per_frame={ms_per_frame:.2} realtime_x={:.2} checksum={checksum:016x}",
        (1000.0 / fps) / ms_per_frame
    );
}
