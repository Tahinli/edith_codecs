//! Throwaway profiling harness for lane-h264perf: repeats the bench's own
//! 640x360x30-frame encode loop N times so a perf sample has enough hits to
//! be useful. Not wired into any product surface; delete before merge if
//! still present.
use ec_h264::{Encoder, EncoderConfig, PictureView};
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("path to raw yuv420p");
    let reps: u32 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(20);
    let (w, h) = (640u32, 360u32);
    let data = std::fs::read(&path).expect("read raw yuv");
    let frame_len = (w * h + 2 * (w.div_ceil(2) * h.div_ceil(2))) as usize;
    let n = data.len() / frame_len;
    let start = Instant::now();
    for _ in 0..reps {
        let mut cfg = EncoderConfig::new(w, h);
        cfg.threads = 1;
        let mut enc = Encoder::new(cfg).expect("h264 encoder");
        for frame in data.chunks_exact(frame_len).take(n) {
            let (y, rest) = frame.split_at((w * h) as usize);
            let (u, v) = rest.split_at(rest.len() / 2);
            let view = PictureView::i420(w, h, y, u, v);
            enc.encode(&view).expect("h264 encode");
        }
    }
    let wall = start.elapsed();
    eprintln!(
        "{reps} reps x {n} frames in {:.3}s ({:.2} ms/rep)",
        wall.as_secs_f64(),
        wall.as_secs_f64() * 1000.0 / f64::from(reps)
    );
}
