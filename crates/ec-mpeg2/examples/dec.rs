#![allow(clippy::needless_range_loop, clippy::field_reassign_with_default, clippy::too_many_arguments, clippy::type_complexity)]
//! Decode an MPEG-1/2 elementary stream and print per-picture facts; with a
//! second argument write raw planar frames. Also a throughput measurement.
use std::io::Write;
use std::time::Instant;

use ec_core::registry::{CodecId, CodecParameters};
use ec_mpeg2::Mpeg2Decoder;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&args[1]).expect("read");
    let mut out = args.get(2).map(|p| std::fs::File::create(p).expect("create"));
    let quiet = std::env::var_os("QUIET").is_some();
    let reps: usize = std::env::var("REPS").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    let t0 = Instant::now();
    let mut frames = 0usize;
    for _ in 0..reps {
        let mut dec = Mpeg2Decoder::new(CodecParameters::new(CodecId::Mpeg2Video)).unwrap();
        for chunk in data.chunks(65536) {
            if let Err(e) = dec.send_bytes(chunk, None) {
                eprintln!("error: {e}");
            }
            while let Some((f, info)) = dec.pop_picture() {
                frames += 1;
                if !quiet {
                    println!("{:?} tr {} {}x{} {:?} corrupt {} tff {} rff {} prog {}", info.picture_type, info.temporal_reference, f.width, f.height, f.interlace, info.corrupt, info.top_field_first, info.repeat_first_field, info.progressive_frame);
                }
                if let Some(o) = out.as_mut() {
                    for p in &f.planes {
                        o.write_all(&p.data).unwrap();
                    }
                }
            }
        }
        if let Err(e) = dec.flush_stream() {
            eprintln!("error: {e}");
        }
        while let Some((f, info)) = dec.pop_picture() {
            frames += 1;
            if !quiet {
                println!("{:?} tr {} {}x{} {:?} corrupt {}", info.picture_type, info.temporal_reference, f.width, f.height, f.interlace, info.corrupt);
            }
            if let Some(o) = out.as_mut() {
                for p in &f.planes {
                    o.write_all(&p.data).unwrap();
                }
            }
        }
    }
    let dt = t0.elapsed().as_secs_f64();
    eprintln!("{frames} frames in {dt:.3}s = {:.1} fps", frames as f64 / dt);
}
