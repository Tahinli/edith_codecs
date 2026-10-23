//! benchp1: RIFF/WAVE read and write throughput for lanes/bench.md ("lane benchp1").
//!
//! usage:
//!   benchp1 read <in.wav> [reps]
//!       Opens the file and decodes every sample to i32 with `WavReader::read_all_i32`, `reps`
//!       times (default 5), keeping the best wall time.
//!   benchp1 write <in.wav> <out.wav> <riff|rf64> [reps]
//!       Reads the samples once (untimed), then writes them back with `WavWriter` at the same
//!       spec, `reps` times (default 5), keeping the best wall time. `riff` takes the default
//!       header; `rf64` forces the RF64 header (rf64_threshold 0). The last output is kept at
//!       <out.wav> for the md5 round-trip check.
use std::hint::black_box;
use std::time::Instant;

use ec_riff::{RF64_THRESHOLD, WavOptions, WavReader, WavWriter};

fn read_once(path: &str) -> (usize, f64) {
    let t = Instant::now();
    let mut reader = WavReader::open(path).expect("open wav");
    let samples = reader.read_all_i32().expect("read samples");
    let secs = t.elapsed().as_secs_f64();
    black_box(&samples);
    (samples.len(), secs)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("read") => {
            let reps: usize = args.get(3).map_or(5, |s| s.parse().expect("reps"));
            let bytes = std::fs::metadata(&args[2]).expect("stat").len();
            let (samples, _) = read_once(&args[2]);
            let best = (0..reps).map(|_| read_once(&args[2]).1).fold(f64::MAX, f64::min);
            println!(
                "samples={samples} bytes={bytes} best_read_s={best:.4} mb_s={:.2} reps={reps}",
                bytes as f64 / best / 1e6
            );
        }
        Some("write") => {
            let (input, out, mode) = (&args[2], &args[3], args[4].as_str());
            let reps: usize = args.get(5).map_or(5, |s| s.parse().expect("reps"));
            let mut reader = WavReader::open(input).expect("open wav");
            let spec = reader.spec();
            let samples = reader.read_all_i32().expect("read samples");
            let frames = samples.len() as u64 / u64::from(spec.channels);
            let options = match mode {
                "riff" => WavOptions { expected_frames: frames, rf64_threshold: RF64_THRESHOLD },
                "rf64" => WavOptions { expected_frames: frames, rf64_threshold: 0 },
                _ => panic!("mode must be riff or rf64"),
            };
            let data_bytes = samples.len() as u64 * u64::from(spec.bits_per_sample) / 8;
            let mut best = f64::MAX;
            for _ in 0..reps {
                let t = Instant::now();
                let mut w = WavWriter::create_with_options(out, spec, options).expect("create");
                w.write_samples(&samples).expect("write samples");
                w.finalize().expect("finalize");
                best = best.min(t.elapsed().as_secs_f64());
            }
            println!(
                "mode={mode} samples={} data_bytes={data_bytes} best_write_s={best:.4} mb_s={:.2} reps={reps}",
                samples.len(),
                data_bytes as f64 / best / 1e6
            );
        }
        _ => panic!("usage: benchp1 read <in.wav> [reps] | write <in.wav> <out.wav> <riff|rf64> [reps]"),
    }
}
