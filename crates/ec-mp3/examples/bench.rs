//! bench: MP3 (Layer III) encode throughput for lanes/bench.md ("lane benchaudio").
//!
//! usage: bench enc <in.f32le> <channels> <rate> <kbps> <out.mp3> [reps]
//!
//! Encodes raw interleaved f32le PCM (values in [-1, 1]) to CBR Layer III at `kbps`, `reps`
//! times (default 5), keeping the best wall time. The stream of the last run is written to
//! `out.mp3`.
use std::hint::black_box;
use std::time::Instant;

use ec_mp3::{Mp3Encoder, Mp3EncoderConfig};

fn encode_once(pcm: &[f32], channels: u16, rate: u32, kbps: u32) -> Vec<u8> {
    let mut encoder = Mp3Encoder::new(Mp3EncoderConfig {
        bitrate_kbps: kbps,
        vbr_quality: None,
    });
    encoder.push_pcm_f32(pcm, channels, rate).expect("push");
    encoder.finish();
    let mut out = Vec::new();
    loop {
        match encoder.next_packet() {
            Ok(frame) => out.extend_from_slice(&frame),
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("encode: {e}"),
        }
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 7 || args[1] != "enc" {
        eprintln!("usage: bench enc <in.f32le> <channels> <rate> <kbps> <out.mp3> [reps]");
        std::process::exit(2);
    }
    let channels: u16 = args[3].parse().expect("channels");
    let rate: u32 = args[4].parse().expect("rate");
    let kbps: u32 = args[5].parse().expect("kbps");
    let reps: usize = args.get(7).map_or(5, |s| s.parse().expect("reps"));
    let pcm: Vec<f32> = std::fs::read(&args[2])
        .expect("read input")
        .as_chunks::<4>().0.iter()
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let out = encode_once(&pcm, channels, rate, kbps);
    let mut best = f64::MAX;
    for _ in 0..reps {
        let t = Instant::now();
        black_box(encode_once(black_box(&pcm), channels, rate, kbps).len());
        best = best.min(t.elapsed().as_secs_f64());
    }
    std::fs::write(&args[6], &out).expect("write output");
    println!(
        "bytes={} best_encode_s={best:.4} reps={reps} audio_s={:.3}",
        out.len(),
        pcm.len() as f64 / f64::from(channels) / f64::from(rate)
    );
}
