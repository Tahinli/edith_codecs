//! bench: AAC decode and encode throughput for lanes/bench.md ("lane benchaudio").
//!
//! usage:
//!   bench dec <in.adts> <out.f32le> [reps]
//!   bench enc <in.f32le> <channels> <rate> <kbps> <out.adts> [reps]
//!
//! `dec` decodes every ADTS frame of the file with `AacDecoder` (implicit SBR is detected by the
//! decoder; an HE-AAC ADTS stream needs no config), `reps` times (default 5), and keeps the best
//! wall time. The decoded interleaved f32 PCM of the last run is written to `out.f32le`.
//! `enc` encodes raw interleaved f32le PCM to ADTS AAC-LC with `AacEncoder`, best of `reps`.
use std::hint::black_box;
use std::time::Instant;

use ec_aac::{AacDecoder, AacEncoder, AacEncoderConfig, parse_adts};

/// Decode the whole ADTS stream once: (interleaved samples, frames decoded, frames that failed).
fn decode_once(data: &[u8]) -> (Vec<f32>, usize, usize) {
    let mut decoder = AacDecoder::new();
    let mut out = Vec::new();
    let (mut ok, mut failed) = (0usize, 0usize);
    let mut at = 0usize;
    while at + 7 <= data.len() {
        let Ok(header) = parse_adts(&data[at..]) else {
            break;
        };
        let end = (at + header.frame_length).min(data.len());
        match decoder.decode(&data[at..end], None) {
            Ok(frame) => {
                out.extend_from_slice(&frame.samples);
                ok += 1;
            }
            Err(_) => failed += 1,
        }
        at = end;
    }
    (out, ok, failed)
}

/// Encode interleaved f32 PCM to one ADTS byte stream.
fn encode_once(pcm: &[f32], channels: u16, rate: u32, kbps: u32) -> Vec<u8> {
    let config = AacEncoderConfig {
        bitrate_bps: kbps * 1000,
        adts: true,
        ..AacEncoderConfig::default()
    };
    let mut encoder = AacEncoder::new(config);
    encoder.push_pcm(pcm, channels, rate).expect("push");
    encoder.finish();
    let mut out = Vec::new();
    loop {
        match encoder.next_packet() {
            Ok(packet) => out.extend_from_slice(&packet.data),
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("encode: {e}"),
        }
    }
    out
}

fn le_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>().0.iter()
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("dec") if args.len() >= 4 => {
            let reps: usize = args.get(4).map_or(5, |s| s.parse().expect("reps"));
            let data = std::fs::read(&args[2]).expect("read input");
            let (pcm, ok, failed) = decode_once(&data);
            let mut best = f64::MAX;
            for _ in 0..reps {
                let t = Instant::now();
                black_box(decode_once(black_box(&data)).0.len());
                best = best.min(t.elapsed().as_secs_f64());
            }
            let mut bytes = Vec::with_capacity(pcm.len() * 4);
            for v in &pcm {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            std::fs::write(&args[3], &bytes).expect("write output");
            println!(
                "frames_ok={ok} frames_failed={failed} samples={} best_decode_s={best:.4} reps={reps}",
                pcm.len()
            );
        }
        Some("enc") if args.len() >= 7 => {
            let channels: u16 = args[3].parse().expect("channels");
            let rate: u32 = args[4].parse().expect("rate");
            let kbps: u32 = args[5].parse().expect("kbps");
            let reps: usize = args.get(7).map_or(5, |s| s.parse().expect("reps"));
            let pcm = le_f32(&std::fs::read(&args[2]).expect("read input"));
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
        _ => {
            eprintln!(
                "usage: bench dec <in.adts> <out.f32le> [reps]\n       bench enc <in.f32le> <channels> <rate> <kbps> <out.adts> [reps]"
            );
            std::process::exit(2);
        }
    }
}
