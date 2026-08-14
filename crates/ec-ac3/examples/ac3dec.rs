//! Decode an AC-3 / E-AC-3 elementary stream to interleaved 32-bit float.
//!
//! ```text
//! cargo run --release --example ac3dec -- in.ac3 out.f32 [--downmix stereo|mono] [--drc 0.0]
//! ```
//!
//! Prints the stream's layout and the decode speed relative to realtime, which
//! is how the crate's perf claim is measured.

use std::io::{BufWriter, Write};
use std::time::Instant;

use ec_ac3::{Ac3Decoder, Downmix, Options};

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.len() < 2 {
        eprintln!(
            "usage: ac3dec <in.ac3> <out.f32> [--downmix stereo|mono] [--drc SCALE] \
             [--dither on|off]"
        );
        std::process::exit(2);
    }
    let mut options = Options::default();
    let mut i = 2;
    while i + 1 < argv.len() {
        match argv[i].as_str() {
            "--downmix" => {
                options.downmix = match argv[i + 1].as_str() {
                    "stereo" => Downmix::Stereo,
                    "mono" => Downmix::Mono,
                    other => {
                        eprintln!("ac3dec: unknown downmix {other}");
                        std::process::exit(2);
                    }
                }
            }
            "--drc" => options.drc_scale = argv[i + 1].parse().unwrap_or(1.0),
            "--dither" => options.dither = argv[i + 1] != "off",
            other => {
                eprintln!("ac3dec: unknown flag {other}");
                std::process::exit(2);
            }
        }
        i += 2;
    }

    let data = match std::fs::read(&argv[0]) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("ac3dec: {}: {e}", argv[0]);
            std::process::exit(2);
        }
    };
    let mut decoder = Ac3Decoder::with_options(options);
    let file = match std::fs::File::create(&argv[1]) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("ac3dec: {}: {e}", argv[1]);
            std::process::exit(2);
        }
    };
    // Streamed, because a two-hour 5.1 track is ten gigabytes of `f32`.
    let mut out = BufWriter::with_capacity(1 << 20, file);
    let (mut pos, mut frames, mut samples, mut errors) = (0usize, 0u64, 0u64, Vec::new());
    let mut rate = 0;
    let mut channels = 0;
    let start = Instant::now();
    while pos + 6 <= data.len() {
        if data[pos] != 0x0B || data[pos + 1] != 0x77 {
            pos += 1;
            continue;
        }
        let size = match ec_ac3::frame_size(&data[pos..]) {
            Ok(size) if pos + size <= data.len() => size,
            Ok(_) => break,
            Err(e) => {
                errors.push(format!("frame at {pos}: {e}"));
                pos += 2;
                continue;
            }
        };
        match decoder.decode_frame(&data[pos..pos + size]) {
            Ok(frame) => {
                rate = frame.rate;
                channels = frame.channels();
                samples += frame.samples as u64;
                frames += 1;
                let _ = out.write_all(&frame.data[0]);
            }
            Err(e) => {
                // Keep the timeline: a frame that will not decode becomes the
                // silence it represents, so a comparison stays sample-aligned.
                errors.push(format!("frame at {pos}: {e}"));
                if channels > 0 {
                    let _ = out.write_all(&vec![0u8; channels * 1536 * 4]);
                    samples += 1536;
                }
            }
        }
        pos += size;
    }
    let elapsed = start.elapsed().as_secs_f64();

    if let Err(e) = out.flush() {
        eprintln!("ac3dec: {}: {e}", argv[1]);
        std::process::exit(2);
    }
    let seconds = if rate > 0 {
        samples as f64 / f64::from(rate)
    } else {
        0.0
    };
    println!("frames\t{frames}");
    println!("channels\t{channels}");
    println!("sample_rate\t{rate}");
    println!("samples\t{samples}");
    println!("seconds\t{seconds:.3}");
    println!("decode_seconds\t{elapsed:.3}");
    println!(
        "realtime\t{:.1}",
        if elapsed > 0.0 {
            seconds / elapsed
        } else {
            0.0
        }
    );
    println!("errors\t{}", errors.len());
    for e in errors.iter().take(5) {
        println!("error\t{e}");
    }
    if frames == 0 {
        std::process::exit(1);
    }
}
