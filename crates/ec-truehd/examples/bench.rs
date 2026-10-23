//! bench: TrueHD decode throughput for lanes/bench.md ("lane benchaudio").
//!
//! usage: bench dec <in.thd> <out.s32le> [reps]
//!
//! Splits a raw TrueHD elementary stream (as ffmpeg's `truehd` muxer writes it) into access
//! units with `frame_length`, decodes each with `TrueHdDecoder`, `reps` times (default 5), and
//! keeps the best wall time. The decoded interleaved S32 samples of the last run are written to
//! `out.s32le`. The lossless self-checks (`check_stats`) are printed; any failure count other
//! than zero means the decode is not trustworthy.
use std::hint::black_box;
use std::time::Instant;

use ec_truehd::{TrueHdDecoder, frame_length};

/// Decode the whole stream once: (S32 bytes, channels, rate, access units, lossless failures).
fn decode_once(data: &[u8]) -> (Vec<u8>, usize, u32, usize, u64) {
    let mut decoder = TrueHdDecoder::new();
    let mut out = Vec::new();
    let (mut channels, mut rate, mut units) = (0usize, 0u32, 0usize);
    let mut at = 0usize;
    while at < data.len() {
        let len = match frame_length(&data[at..]) {
            Ok(len) if len > 0 => len.min(data.len() - at),
            _ => break,
        };
        if let Some(frame) = decoder
            .decode_access_unit(&data[at..at + len])
            .expect("truehd access unit")
        {
            channels = frame.layout.channel_count();
            rate = frame.rate;
            out.extend_from_slice(&frame.data[0]);
        }
        units += 1;
        at += len;
    }
    let stats = decoder.check_stats();
    let failures = stats.lossless_check_failures + stats.parity_failures + stats.restart_crc_failures;
    (out, channels, rate, units, failures)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 || args[1] != "dec" {
        eprintln!("usage: bench dec <in.thd> <out.s32le> [reps]");
        std::process::exit(2);
    }
    let reps: usize = args.get(4).map_or(5, |s| s.parse().expect("reps"));
    let data = std::fs::read(&args[2]).expect("read input");
    let (pcm, channels, rate, units, failures) = decode_once(&data);
    let mut best = f64::MAX;
    for _ in 0..reps {
        let t = Instant::now();
        black_box(decode_once(black_box(&data)).0.len());
        best = best.min(t.elapsed().as_secs_f64());
    }
    std::fs::write(&args[3], &pcm).expect("write output");
    let samples = pcm.len() / 4;
    println!(
        "access_units={units} channels={channels} rate={rate} samples={} check_failures={failures} best_decode_s={best:.4} reps={reps} audio_s={:.3}",
        samples / channels.max(1),
        samples as f64 / channels.max(1) as f64 / f64::from(rate.max(1))
    );
}
