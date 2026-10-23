//! Decode one AV1 OBU stream (low-overhead, `ffmpeg -f obu`) and report the
//! decode rate, for the 1080p30 realtime criterion in DONE.md.
//!
//! ```text
//! cargo run --release -p ec-av1 --example decode_bench -- s.obu [--threads N]
//! cargo run --release -p ec-av1 --example decode_bench -- s.obu --threads N --raw | md5sum
//! ```
//!
//! Without `--threads` the decoder's own default is used (`EC_AV1_THREADS`, or
//! the core-count default). `--raw` writes every shown frame as 8-bit planar
//! YUV to stdout instead of timing only, so its md5 can be compared with
//! `ffmpeg -i s.obu -f rawvideo -pix_fmt yuv420p - | md5sum`. Timing goes to
//! stderr in both modes.
use std::io::Write;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: decode_bench <stream.obu> [--threads N] [--raw]");
        std::process::exit(2);
    };
    let mut threads: Option<usize> = None;
    let mut raw = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--threads" => {
                threads = args.next().and_then(|v| v.parse().ok());
                if threads.is_none() {
                    eprintln!("--threads needs a number");
                    std::process::exit(2);
                }
            }
            "--raw" => raw = true,
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(2);
        }
    };
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    let mut frames = 0usize;
    let mut pixels = 0usize;
    let start = Instant::now();
    let mut sink = |picture: &ec_av1::encode::Picture, _decode_idx: usize, shown: bool| {
        if !shown {
            return Ok(());
        }
        frames += 1;
        pixels += picture.width * picture.height;
        if raw {
            for plane in [&picture.y, &picture.u, &picture.v] {
                let bytes: Vec<u8> = plane.iter().map(|&s| s as u8).collect();
                out.write_all(&bytes).map_err(|e| {
                    ec_core::Error::unsupported("decode_bench stdout", e.to_string())
                })?;
            }
        }
        Ok(())
    };
    let result = match threads {
        Some(n) => ec_av1::stream::decode_stream_with_threads(&data, n, &mut sink),
        None => ec_av1::stream::decode_stream_with(&data, &mut sink),
    };
    if let Err(e) = result {
        eprintln!("decode failed: {e}");
        std::process::exit(1);
    }
    let _ = out.flush();
    let secs = start.elapsed().as_secs_f64();
    eprintln!(
        "threads={} frames={frames} secs={secs:.3} fps={:.2} mp={:.2}",
        threads.map_or_else(
            || std::env::var("EC_AV1_THREADS").unwrap_or_else(|_| "default".to_string()),
            |n| n.to_string(),
        ),
        frames as f64 / secs,
        pixels as f64 / 1e6,
    );
}
