//! Decode a ProRes `.mov`/`.mp4`: print the facts, optionally write raw planar frames
//! (video planes, then 16-bit alpha when present) and measure throughput.
//! `dec in.mov [out.raw] ` with env `THREADS`, `REPS`, `QUIET`.
use std::io::{BufReader, Write};
use std::time::Instant;

use ec_core::registry::{CodecId, CodecParameters, Demuxer};
use ec_mp4::Mp4Demuxer;
use ec_prores::ProResDecoder;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let reps: usize = std::env::var("REPS").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    let threads: usize = std::env::var("THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    let quiet = std::env::var_os("QUIET").is_some();
    let mut out = args.get(2).map(|p| std::fs::File::create(p).expect("create"));
    let mut frames = 0usize;
    let t0 = Instant::now();
    for _ in 0..reps {
        let mut dm = Mp4Demuxer::new(BufReader::new(std::fs::File::open(&args[1]).unwrap())).unwrap();
        let vs = dm.streams().iter().position(|s| s.params.codec == CodecId::ProRes).expect("no ProRes stream") as u32;
        let mut dec = ProResDecoder::new(CodecParameters::new(CodecId::ProRes)).unwrap();
        dec.set_threads(threads);
        while let Ok(p) = dm.next_packet() {
            if p.stream != vs {
                continue;
            }
            let f = dec.decode_full(&p.data).unwrap_or_else(|e| panic!("frame {frames}: {e}"));
            frames += 1;
            if !quiet {
                println!("{:?} {}x{} {:?} alpha {:?} {:?}", f.video.format, f.video.width, f.video.height, f.video.interlace, f.alpha.as_ref().map(|_| f.info.alpha_bits), f.info);
            }
            if let Some(o) = out.as_mut() {
                let (w, h) = (f.video.width as usize, f.video.height as usize);
                let (sx, sy) = f.video.format.chroma_shift();
                for (i, pl) in f.video.planes.iter().enumerate() {
                    let (pw, ph) = if i == 0 { (w, h) } else { (w.div_ceil(1 << sx), h.div_ceil(1 << sy)) };
                    for y in 0..ph {
                        o.write_all(&pl.data[y * pl.stride..y * pl.stride + pw * 2]).unwrap();
                    }
                }
                if let Some(a) = &f.alpha {
                    for y in 0..h {
                        for x in 0..w {
                            o.write_all(&a.data[y * a.stride + x].to_le_bytes()).unwrap();
                        }
                    }
                }
            }
        }
    }
    let dt = t0.elapsed().as_secs_f64();
    eprintln!("{frames} frames in {dt:.3}s = {:.1} fps", frames as f64 / dt);
}
