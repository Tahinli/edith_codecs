//! bench: FLAC decode and encode rows for lanes/bench.md ("lane benchrest").
//!
//! usage:
//!   bench dec <in.flac> <out.pcm> [reps]
//!   bench enc <in.pcm> <channels> <bits> <rate> <out.flac> [reps]
//!
//! `dec` decodes a whole `.flac` buffer with `FlacReader::decode_all` and `to_pcm_bytes` (the
//! container view: s16le for 16-bit, s32le for 24-bit, as ffmpeg writes), `reps` times (default 5),
//! and keeps the best wall time. The last PCM is written to `<out.pcm>` (untimed) for the md5 check.
//! `enc` reads raw interleaved PCM (`bits` 16: s16le; `bits` 24: packed s24le, 3 bytes per sample),
//! encodes it with `EncoderConfig::default()` (the level edith's export uses, see
//! `tests/level_depth_parity.rs`), best of `reps`, and writes the last stream to `<out.flac>`.
use std::hint::black_box;
use std::time::Instant;

use ec_flac::decode::FlacReader;
use ec_flac::encode::{encode, EncoderConfig};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("dec") if args.len() >= 4 => {
            let reps: usize = args.get(4).map_or(5, |s| s.parse().expect("reps"));
            assert!(reps >= 1, "reps must be at least 1");
            let data = std::fs::read(&args[2]).expect("read input");
            let mut best = f64::MAX;
            let mut pcm = Vec::new();
            let (mut frames, mut channels, mut bits, mut rate) = (0usize, 0usize, 0u32, 0u32);
            for _ in 0..reps {
                let t = Instant::now();
                let mut reader = FlacReader::new(black_box(&data)).expect("flac");
                let stream = reader.decode_all().expect("decode");
                pcm = stream.to_pcm_bytes();
                best = best.min(t.elapsed().as_secs_f64());
                frames = stream.len();
                channels = stream.channels.len();
                bits = stream.bits_per_sample;
                rate = stream.sample_rate;
            }
            std::fs::write(&args[3], &pcm).expect("write output");
            let audio_s = frames as f64 / f64::from(rate);
            println!(
                "frames={frames} channels={channels} bits={bits} rate={rate} audio_s={audio_s:.3} best_decode_s={best:.4} x_realtime={:.1} pcm_bytes={} reps={reps}",
                audio_s / best,
                pcm.len()
            );
        }
        Some("enc") if args.len() >= 7 => {
            let reps: usize = args.get(7).map_or(5, |s| s.parse().expect("reps"));
            assert!(reps >= 1, "reps must be at least 1");
            let channels: usize = args[3].parse().expect("channels");
            let bits: u32 = args[4].parse().expect("bits");
            let rate: u32 = args[5].parse().expect("rate");
            let raw = std::fs::read(&args[2]).expect("read input");
            let samples: Vec<i32> = match bits {
                16 => raw
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| i32::from(i16::from_le_bytes(*b)))
                    .collect(),
                24 => raw
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|b| i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8)
                    .collect(),
                _ => panic!("bits must be 16 or 24"),
            };
            let config = EncoderConfig::default();
            let mut best = f64::MAX;
            let mut out = Vec::new();
            for _ in 0..reps {
                let t = Instant::now();
                out = encode(&config, black_box(&samples), channels, bits, rate).expect("encode");
                best = best.min(t.elapsed().as_secs_f64());
            }
            std::fs::write(&args[6], &out).expect("write output");
            let audio_s = (samples.len() / channels) as f64 / f64::from(rate);
            println!(
                "audio_s={audio_s:.3} best_encode_s={best:.4} x_realtime={:.1} out_bytes={} kbps={:.1} reps={reps}",
                audio_s / best,
                out.len(),
                out.len() as f64 * 8.0 / audio_s / 1000.0
            );
        }
        _ => {
            eprintln!(
                "usage: bench dec <in.flac> <out.pcm> [reps]\n       bench enc <in.pcm> <channels> <bits> <rate> <out.flac> [reps]"
            );
            std::process::exit(2);
        }
    }
}
