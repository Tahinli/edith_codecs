//! benchp1: SubRip and WebVTT parse throughput for lanes/bench.md ("lane benchp1").
//!
//! usage: benchp1 <srt|vtt> <file> [reps]
//!
//! Parses the whole file `reps` times (default 5). Each timed run repeats the parse until it
//! takes at least 0.2 s, so the per-parse time is not timer noise. Prints the best per-parse
//! time, the throughput in MB/s and the cue count of the parsed track (checked against ffprobe).
use std::hint::black_box;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let kind = args.get(1).map(String::as_str).unwrap_or("");
    let bytes = std::fs::read(&args[2]).expect("read subtitle file");
    let reps: usize = args.get(3).map_or(5, |s| s.parse().expect("reps"));
    let parse = |b: &[u8]| match kind {
        "srt" => ec_subs::srt::parse(b),
        "vtt" => ec_subs::webvtt::parse(b),
        _ => panic!("kind must be srt or vtt"),
    };

    let first = parse(&bytes).expect("parse");
    let cues = first.cues.len();
    let skipped = first.skipped;
    let t0 = Instant::now();
    black_box(parse(black_box(&bytes)).expect("parse"));
    let one = t0.elapsed().as_secs_f64().max(1e-6);
    let inner = ((0.2 / one).ceil() as usize).max(1);

    let mut best = f64::MAX;
    for _ in 0..reps {
        let t = Instant::now();
        for _ in 0..inner {
            black_box(parse(black_box(&bytes)).expect("parse"));
        }
        best = best.min(t.elapsed().as_secs_f64() / inner as f64);
    }
    let mb_s = bytes.len() as f64 / best / 1e6;
    println!(
        "kind={kind} bytes={} cues={cues} skipped={skipped} best_parse_ms={:.3} mb_s={mb_s:.2} reps={reps} inner={inner}",
        bytes.len(),
        best * 1e3
    );
}
