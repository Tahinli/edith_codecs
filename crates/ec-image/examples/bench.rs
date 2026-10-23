//! bench: still-image decode and PNG encode rows for lanes/bench.md ("lane benchrest").
//!
//! usage:
//!   bench png-dec <in.png> <out.rgb> [reps]
//!   bench jpeg-dec <in.jpg> <out.rgb> [reps]
//!   bench png-enc <in.rgba> <width> <height> <out.png> [reps]
//!
//! `png-dec` and `jpeg-dec` decode the file with `ec_image::decode`, `reps` times (default 5), and
//! keep the best wall time. After timing, the last decode is converted with `to_rgb8` and written to
//! `<out.rgb>` (untimed) so the pixels can be compared with ffmpeg's `-f rawvideo -pix_fmt rgb24`.
//! `png-enc` encodes raw RGBA8 with `ec_image::png_encode::encode_rgba8`, best of `reps`, and writes
//! the last PNG to `<out.png>`.
use std::hint::black_box;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("png-dec" | "jpeg-dec") if args.len() >= 4 => {
            let reps: usize = args.get(4).map_or(5, |s| s.parse().expect("reps"));
            assert!(reps >= 1, "reps must be at least 1");
            let data = std::fs::read(&args[2]).expect("read input");
            let mut best = f64::MAX;
            let mut last = None;
            for _ in 0..reps {
                let t = Instant::now();
                let img = ec_image::decode(black_box(&data)).expect("decode");
                best = best.min(t.elapsed().as_secs_f64());
                last = Some(img);
            }
            let img = last.expect("at least one rep");
            let (w, h) = img.dimensions();
            let rgb = img.to_rgb8();
            std::fs::write(&args[3], &rgb).expect("write output");
            let mpix = f64::from(w) * f64::from(h) / 1e6;
            println!(
                "width={w} height={h} channels={} best_decode_s={best:.4} mpix_per_s={:.2} mb_per_s={:.2} out_bytes={} reps={reps}",
                img.pixels.channels(),
                mpix / best,
                rgb.len() as f64 / best / 1e6,
                rgb.len()
            );
        }
        Some("png-enc") if args.len() >= 6 => {
            let reps: usize = args.get(6).map_or(5, |s| s.parse().expect("reps"));
            assert!(reps >= 1, "reps must be at least 1");
            let w: u32 = args[3].parse().expect("width");
            let h: u32 = args[4].parse().expect("height");
            let raw = std::fs::read(&args[2]).expect("read input");
            let mut best = f64::MAX;
            let mut out = Vec::new();
            for _ in 0..reps {
                let t = Instant::now();
                out = ec_image::png_encode::encode_rgba8(w, h, black_box(&raw)).expect("encode");
                best = best.min(t.elapsed().as_secs_f64());
            }
            std::fs::write(&args[5], &out).expect("write output");
            let mpix = f64::from(w) * f64::from(h) / 1e6;
            println!(
                "width={w} height={h} best_encode_s={best:.4} mpix_per_s={:.2} mb_per_s={:.2} out_bytes={} reps={reps}",
                mpix / best,
                raw.len() as f64 / best / 1e6,
                out.len()
            );
        }
        _ => {
            eprintln!(
                "usage: bench png-dec <in.png> <out.rgb> [reps]\n       bench jpeg-dec <in.jpg> <out.rgb> [reps]\n       bench png-enc <in.rgba> <width> <height> <out.png> [reps]"
            );
            std::process::exit(2);
        }
    }
}
