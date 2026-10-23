//! VA-API bench for ec-hw (lane benchhw, DONE.md gate G6). Local GPU only.
//!
//! Drives the same public API engine-hw's callers use: `Decoder::new` /
//! `decode` / `next_frame` / `flush`, and `Encoder::new` / `encode`.
//!
//! ```text
//! bench dec <h264|hevc|vp9|av1> <stream> [--out FILE] [--runs N]
//! bench enc <h264|hevc> <raw-yuv420p> <width> <height> <frames> <bitrate> [--out FILE] [--runs N]
//! ```
//!
//! `dec` splits an elementary stream (Annex B for H.264/HEVC, IVF for VP9/AV1) into access
//! units. Each timed run decodes the whole stream and waits for the GPU on every picture
//! (`Surface::sync`) before dropping it, so the time covers the GPU work and not only the
//! submission. The readback is not timed. With `--out`, one extra untimed pass writes every
//! picture as planar I420 (8-bit bytes, or 10-bit little-endian u16) for the md5 check.
//!
//! `enc` reads `frames` pictures from a raw 8-bit 4:2:0 file. Only the `encode` calls are
//! timed. `--out` writes the coded stream of the last run.
//!
//! Each run prints one `key=value` line; the best run is the one with the least time.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use ec_hw::{
    Codec, Decoder, EncCodec, Encoder, EncoderConfig, FrameMetadata, I420, RateControlMode,
};
use ec_va::Display;

/// Media seconds per frame: every clip in the bench is 30 pictures per second.
const FPS: f64 = 30.0;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bench: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (pos, opts) = split_args(&args)?;
    let runs: usize = opts.runs.unwrap_or(5);
    let display = ec_va::open().map_err(|e| format!("no VA display: {e}"))?;
    match pos.first().map(String::as_str) {
        Some("dec") if pos.len() == 3 => {
            let codec = parse_codec(&pos[1])?;
            let data = std::fs::read(&pos[2]).map_err(|e| format!("{}: {e}", pos[2]))?;
            bench_dec(&display, codec, &data, runs, opts.out.as_deref())
        }
        Some("enc") if pos.len() == 7 => {
            let codec = match pos[1].as_str() {
                "h264" => EncCodec::H264,
                "hevc" => EncCodec::H265,
                other => return Err(format!("encode codec {other}: want h264 or hevc")),
            };
            let num = |i: usize| -> Result<u32, String> {
                pos[i].parse().map_err(|_| format!("bad number {}", pos[i]))
            };
            let (w, h, frames, bitrate) = (num(3)?, num(4)?, num(5)? as usize, num(6)?);
            bench_enc(
                &display,
                codec,
                &pos[2],
                w,
                h,
                frames,
                bitrate,
                runs,
                opts.out.as_deref(),
            )
        }
        _ => Err(usage()),
    }
}

fn usage() -> String {
    "usage: bench dec <h264|hevc|vp9|av1> <stream> [--out FILE] [--runs N]\n       \
     bench enc <h264|hevc> <raw-yuv420p> <w> <h> <frames> <bitrate> [--out FILE] [--runs N]"
        .to_string()
}

#[derive(Default)]
struct Opts {
    out: Option<String>,
    runs: Option<usize>,
}

fn split_args(args: &[String]) -> Result<(Vec<String>, Opts), String> {
    let mut pos = Vec::new();
    let mut opts = Opts::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => opts.out = Some(it.next().ok_or("--out needs a path")?.clone()),
            "--runs" => {
                let v = it.next().ok_or("--runs needs a number")?;
                opts.runs = Some(v.parse().map_err(|_| format!("bad --runs {v}"))?);
            }
            _ => pos.push(a.clone()),
        }
    }
    Ok((pos, opts))
}

fn parse_codec(s: &str) -> Result<Codec, String> {
    match s {
        "h264" => Ok(Codec::H264),
        "hevc" => Ok(Codec::H265),
        "vp9" => Ok(Codec::Vp9),
        "av1" => Ok(Codec::Av1),
        other => Err(format!("unknown codec {other}")),
    }
}

// ---------------------------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------------------------

/// Decode the whole stream once, waiting on the GPU for each picture. Returns the picture count
/// and the time spent (demux splitting is excluded).
fn decode_timed(
    display: &std::rc::Rc<Display>,
    codec: Codec,
    units: &[&[u8]],
) -> Result<(usize, Duration), String> {
    let mut decoder = Decoder::new(display, codec).map_err(|e| e.to_string())?;
    let mut count = 0usize;
    let start = Instant::now();
    for (i, unit) in units.iter().enumerate() {
        decoder
            .decode(unit, i as i64)
            .map_err(|e| format!("decode unit {i}: {e}"))?;
        while let Some(frame) = decoder.next_frame() {
            frame.surface().sync().map_err(|e| e.to_string())?;
            count += 1;
        }
    }
    decoder.flush().map_err(|e| e.to_string())?;
    while let Some(frame) = decoder.next_frame() {
        frame.surface().sync().map_err(|e| e.to_string())?;
        count += 1;
    }
    Ok((count, start.elapsed()))
}

/// Decode with readback and write every picture as planar I420 (the check output).
fn decode_dump(
    display: &std::rc::Rc<Display>,
    codec: Codec,
    units: &[&[u8]],
    out: &str,
) -> Result<usize, String> {
    let mut decoder = Decoder::new(display, codec).map_err(|e| e.to_string())?;
    let mut file = BufWriter::new(File::create(out).map_err(|e| format!("{out}: {e}"))?);
    let mut count = 0usize;
    let emit = |frame: ec_hw::Frame, file: &mut BufWriter<File>| -> Result<(), String> {
        if frame.bit_depth == 10 {
            let p = frame.to_i420_16().map_err(|e| e.to_string())?;
            for plane in [&p.y, &p.u, &p.v] {
                for s in plane.iter() {
                    file.write_all(&s.to_le_bytes())
                        .map_err(|e| e.to_string())?;
                }
            }
        } else {
            let p = frame.to_i420().map_err(|e| e.to_string())?;
            for plane in [&p.y, &p.u, &p.v] {
                file.write_all(plane).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    };
    for (i, unit) in units.iter().enumerate() {
        decoder
            .decode(unit, i as i64)
            .map_err(|e| format!("decode unit {i}: {e}"))?;
        while let Some(frame) = decoder.next_frame() {
            emit(frame, &mut file)?;
            count += 1;
        }
    }
    decoder.flush().map_err(|e| e.to_string())?;
    while let Some(frame) = decoder.next_frame() {
        emit(frame, &mut file)?;
        count += 1;
    }
    file.flush().map_err(|e| e.to_string())?;
    Ok(count)
}

fn bench_dec(
    display: &std::rc::Rc<Display>,
    codec: Codec,
    data: &[u8],
    runs: usize,
    out: Option<&str>,
) -> Result<(), String> {
    let units: Vec<&[u8]> = match codec {
        Codec::H264 | Codec::H265 => split_access_units(data, codec),
        Codec::Vp9 | Codec::Av1 => split_ivf(data),
    };
    if units.is_empty() {
        return Err("no access units in the stream".into());
    }
    let mut best: Option<(usize, Duration)> = None;
    let mut times = Vec::new();
    for run in 1..=runs {
        let (n, t) = decode_timed(display, codec, &units)?;
        let s = t.as_secs_f64();
        println!(
            "RUN|dec|run={run} pictures={n} secs={s:.4} fps={:.2}",
            n as f64 / s
        );
        times.push(s);
        if best.is_none_or(|(_, b)| t < b) {
            best = Some((n, t));
        }
    }
    let (n, t) = best.ok_or("no runs")?;
    let s = t.as_secs_f64();
    let media = n as f64 / FPS;
    println!(
        "BEST|dec|pictures={n} best_s={s:.4} fps={:.2} x_realtime={:.2} runs={runs} units={} stream_bytes={}",
        n as f64 / s,
        media / s,
        units.len(),
        data.len()
    );
    if let Some(path) = out {
        let written = decode_dump(display, codec, &units, path)?;
        println!("DUMP|dec|pictures={written} file={path}");
    }
    Ok(())
}

/// Split an Annex B stream into access units: a new one starts at the first VCL NAL of a
/// picture (a slice whose first-slice flag is set, after an earlier slice of the same picture).
fn split_access_units(data: &[u8], codec: Codec) -> Vec<&[u8]> {
    let mut starts: Vec<usize> = Vec::new();
    let mut seen_slice = false;
    let mut i = 0usize;
    while i + 3 < data.len() {
        if data[i] != 0 || data[i + 1] != 0 || data[i + 2] != 1 {
            i += 1;
            continue;
        }
        let nal = i + 3;
        let (is_vcl, first_slice) = match codec {
            Codec::H264 => {
                let t = data[nal] & 0x1f;
                let first = data.get(nal + 1).is_some_and(|&b| b & 0x80 != 0);
                ((1..=5).contains(&t), first)
            }
            _ => {
                let t = (data[nal] >> 1) & 0x3f;
                let first = data.get(nal + 2).is_some_and(|&b| b & 0x80 != 0);
                (t <= 31, first)
            }
        };
        if is_vcl {
            if seen_slice && first_slice {
                starts.push(i);
                seen_slice = false;
            }
            if first_slice {
                seen_slice = true;
            }
        }
        i = nal + 1;
    }
    let mut out = Vec::new();
    let mut prev = 0usize;
    for &start in &starts {
        out.push(&data[prev..start]);
        prev = start;
    }
    if prev < data.len() {
        out.push(&data[prev..]);
    }
    out
}

/// Split an IVF file into its frames.
fn split_ivf(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    if data.len() < 32 || &data[..4] != b"DKIF" {
        return out;
    }
    let header = u16::from_le_bytes([data[6], data[7]]) as usize;
    let mut pos = header.max(32);
    while pos + 12 <= data.len() {
        let size =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 12;
        if pos + size > data.len() {
            break;
        }
        out.push(&data[pos..pos + size]);
        pos += size;
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Encode
// ---------------------------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn bench_enc(
    display: &std::rc::Rc<Display>,
    codec: EncCodec,
    raw: &str,
    width: u32,
    height: u32,
    frames: usize,
    bitrate: u32,
    runs: usize,
    out: Option<&str>,
) -> Result<(), String> {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let frame_bytes = w * h + 2 * cw * ch;
    let mut file = File::open(raw).map_err(|e| format!("{raw}: {e}"))?;
    let mut buf = vec![0u8; frame_bytes];
    let mut best: Option<(usize, Duration, usize, usize)> = None;
    let mut last_stream: Vec<u8> = Vec::new();
    for run in 1..=runs {
        use std::io::Seek;
        file.rewind().map_err(|e| e.to_string())?;
        let mut cfg = EncoderConfig::new(codec, width, height);
        cfg.framerate = (30, 1);
        cfg.gop_size = 30;
        cfg.bitrate = bitrate;
        cfg.rate_control = RateControlMode::ConstantBitrate;
        let mut enc = Encoder::new(display, cfg).map_err(|e| e.to_string())?;
        let mut stream = Vec::new();
        let mut keyframes = 0usize;
        let mut spent = Duration::ZERO;
        for t in 0..frames {
            file.read_exact(&mut buf)
                .map_err(|e| format!("frame {t}: {e}"))?;
            let picture = I420 {
                y: buf[..w * h].to_vec(),
                u: buf[w * h..w * h + cw * ch].to_vec(),
                v: buf[w * h + cw * ch..].to_vec(),
                width,
                height,
            };
            let meta = FrameMetadata {
                timestamp: t as i64,
                force_keyframe: false,
            };
            let start = Instant::now();
            let coded = enc
                .encode(&picture, meta)
                .map_err(|e| format!("encode frame {t}: {e}"))?;
            spent += start.elapsed();
            keyframes += usize::from(coded.is_keyframe);
            stream.extend_from_slice(&coded.data);
        }
        let s = spent.as_secs_f64();
        println!(
            "RUN|enc|run={run} pictures={frames} secs={s:.4} fps={:.2} bytes={} keyframes={keyframes}",
            frames as f64 / s,
            stream.len()
        );
        if best.is_none_or(|(_, b, _, _)| spent < b) {
            best = Some((frames, spent, stream.len(), keyframes));
        }
        last_stream = stream;
    }
    let (n, t, bytes, keyframes) = best.ok_or("no runs")?;
    let s = t.as_secs_f64();
    let media = n as f64 / FPS;
    println!(
        "BEST|enc|pictures={n} best_s={s:.4} fps={:.2} x_realtime={:.2} runs={runs} bytes={bytes} kbps={:.1} keyframes={keyframes}",
        n as f64 / s,
        media / s,
        bytes as f64 * 8.0 / media / 1000.0
    );
    if let Some(path) = out {
        std::fs::write(path, &last_stream).map_err(|e| format!("{path}: {e}"))?;
        println!("DUMP|enc|file={path} bytes={}", last_stream.len());
    }
    Ok(())
}
