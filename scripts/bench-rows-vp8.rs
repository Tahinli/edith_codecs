//! VP8 decode bench harness for scripts/bench-rows.sh. The script copies this file into
//! crates/ec-vp8/examples/ for the run and deletes the copy afterwards.
//!
//! usage: vp8_ivf <file.ivf> [reps] [raw-out|-]
//! Times `reps` full decodes with a fresh decoder each (best wall time, output not
//! written). With a third argument, one more untimed pass writes the visible planes as
//! raw yuv420p to that path (`/dev/stdout` for a pipe into md5sum).
use ec_vp8::decode::Decoder;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bytes = std::fs::read(&args[1]).expect("read ivf");
    let reps: usize = args.get(2).map_or(5, |s| s.parse().expect("reps"));
    let mut frames: Vec<&[u8]> = Vec::new();
    let mut pos = 32;
    while pos + 12 <= bytes.len() {
        let sz = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        frames.push(&bytes[pos + 12..pos + 12 + sz]);
        pos += 12 + sz;
    }
    let mut best = f64::MAX;
    let mut shown = 0usize;
    for _ in 0..reps {
        let mut dec = Decoder::new();
        let t = Instant::now();
        shown = 0;
        for f in &frames {
            if dec.decode(f).expect("vp8 decode").is_some() {
                shown += 1;
            }
        }
        best = best.min(t.elapsed().as_secs_f64());
    }
    println!(
        "frames={} shown={} best_secs={best:.3} fps={:.2}",
        frames.len(),
        shown,
        shown as f64 / best
    );
    if let Some(path) = args.get(3) {
        let mut out = Vec::new();
        let mut dec = Decoder::new();
        for f in &frames {
            if let Some(pic) = dec.decode(f).expect("vp8 decode") {
                let (w, h) = (usize::from(pic.width), usize::from(pic.height));
                let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
                for r in 0..h {
                    out.extend_from_slice(&pic.y[r * pic.stride..r * pic.stride + w]);
                }
                for plane in [&pic.u, &pic.v] {
                    for r in 0..ch {
                        out.extend_from_slice(&plane[r * pic.uv_stride..r * pic.uv_stride + cw]);
                    }
                }
            }
        }
        std::fs::write(path, out).expect("write raw");
    }
}
