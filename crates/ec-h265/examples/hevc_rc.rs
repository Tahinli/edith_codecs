//! Rate-controlled sequence encode and a streaming decode, for the bitrate
//! measurements in `scripts/hevc-rc.sh`. Both sides stream: frames, pictures and
//! decoded access units are handled one at a time, so a 60 s 720p clip does not
//! have to fit in memory.
//!
//! ```text
//! hevc_rc encode <in.yuv> <width> <height> <keyint> <kbps> <fps> <out.hevc> <recon.yuv>
//! hevc_rc decode <in.hevc> <out.yuv>
//! ```
//!
//! `in.yuv` and `recon.yuv` are raw I420 at the displayed size.

use ec_core::frame::{PixelFormat, Plane, VideoFrame};
use ec_h265::Decoder;
use ec_h265::encoder::{Encoder, EncoderConfig, RateControl};
use std::io::{BufWriter, Read, Write};
use std::process::ExitCode;

fn frame_size(w: usize, h: usize) -> usize {
    w * h + 2 * w.div_ceil(2) * h.div_ceil(2)
}

fn split_frame(buf: &[u8], w: usize, h: usize) -> VideoFrame {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let y = buf[..w * h].to_vec();
    let cb = buf[w * h..w * h + cw * ch].to_vec();
    let cr = buf[w * h + cw * ch..].to_vec();
    VideoFrame::try_new(
        PixelFormat::I420,
        w as u32,
        h as u32,
        vec![Plane::new(y, w), Plane::new(cb, cw), Plane::new(cr, cw)],
    )
    .expect("I420 planes at the stated size")
}

/// Access units of an Annex-B stream. A new unit starts at a VPS, or at a PPS
/// that follows a slice (the P pictures carry their own PPS).
fn access_units(stream: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut last_slice = false;
    let mut i = 0;
    while i + 3 <= stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            let nal_type = stream.get(i + 3).map_or(0, |b| (b >> 1) & 0x3f);
            if nal_type == 32 || (nal_type == 34 && last_slice) {
                starts.push(i);
            }
            last_slice = nal_type < 32;
            i += 3;
        } else {
            i += 1;
        }
    }
    // Bytes before the first unit start are dropped; our streams begin with a VPS.
    let mut out: Vec<&[u8]> = starts.windows(2).map(|w| &stream[w[0]..w[1]]).collect();
    if let Some(&last) = starts.last() {
        out.push(&stream[last..]);
    }
    out
}

fn encode(args: &[String]) -> Result<(), String> {
    let arg = |i: usize| {
        args.get(i)
            .cloned()
            .ok_or_else(|| format!("missing argument {i}"))
    };
    let input = std::fs::File::open(arg(2)?).map_err(|e| e.to_string())?;
    let w: usize = arg(3)?.parse().map_err(|_| "width")?;
    let h: usize = arg(4)?.parse().map_err(|_| "height")?;
    let keyint: usize = arg(5)?.parse().map_err(|_| "keyint")?;
    let kbps: u32 = arg(6)?.parse().map_err(|_| "kbps")?;
    let fps: f64 = arg(7)?.parse().map_err(|_| "fps")?;
    let fs = frame_size(w, h);

    let mut cfg = EncoderConfig::new(w as u32, h as u32);
    cfg.rate_control = RateControl::Bitrate { kbps, fps };
    cfg.keyint = keyint;
    cfg.keep_recon = true;
    if let Ok(v) = std::env::var("EC_H265_THREADS") {
        cfg.threads = v.parse().map_err(|_| "EC_H265_THREADS")?;
    }
    let enc = Encoder::new(cfg).map_err(|e| e.to_string())?;

    let mut stream_out = BufWriter::new(std::fs::File::create(arg(8)?).map_err(|e| e.to_string())?);
    let mut recon_out = BufWriter::new(std::fs::File::create(arg(9)?).map_err(|e| e.to_string())?);
    let mut reader = std::io::BufReader::new(input);
    let mut buf = vec![0u8; fs];
    let frames = std::iter::from_fn(move || {
        // A short read at the end of the file ends the clip; a partial frame is dropped.
        match reader.read_exact(&mut buf) {
            Ok(()) => Some(split_frame(&buf, w, h)),
            Err(_) => None,
        }
    });

    let (mut count, mut bytes, mut qp_sum) = (0u64, 0u64, 0u64);
    let (mut qp_min, mut qp_max) = (i32::MAX, i32::MIN);
    enc.encode_sequence_with(frames, |pic| {
        stream_out
            .write_all(&pic.au)
            .map_err(|e| ec_core::Error::corrupt(e.to_string()))?;
        bytes += pic.au.len() as u64;
        qp_sum += pic.qp as u64;
        qp_min = qp_min.min(pic.qp);
        qp_max = qp_max.max(pic.qp);
        count += 1;
        if let Some(r) = &pic.recon {
            for p in &r.planes {
                recon_out
                    .write_all(&p.data)
                    .map_err(|e| ec_core::Error::corrupt(e.to_string()))?;
            }
        }
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    stream_out.flush().map_err(|e| e.to_string())?;
    recon_out.flush().map_err(|e| e.to_string())?;

    let seconds = count as f64 / fps;
    println!(
        "frames={count} bytes={bytes} kbps={:.1} mean_qp={:.2} qp_min={qp_min} qp_max={qp_max}",
        bytes as f64 * 8.0 / seconds / 1000.0,
        qp_sum as f64 / count.max(1) as f64
    );
    Ok(())
}

fn decode(args: &[String]) -> Result<(), String> {
    let arg = |i: usize| {
        args.get(i)
            .cloned()
            .ok_or_else(|| format!("missing argument {i}"))
    };
    let stream = std::fs::read(arg(2)?).map_err(|e| e.to_string())?;
    let mut out = BufWriter::new(std::fs::File::create(arg(3)?).map_err(|e| e.to_string())?);
    let mut dec = Decoder::new();
    let mut n = 0u64;
    let emit = |dec: &mut Decoder,
                out: &mut BufWriter<std::fs::File>,
                n: &mut u64|
     -> Result<(), String> {
        while let Some(f) = dec.next_frame() {
            let pic = &f.picture;
            let win = f.window;
            let (sw, sh) = (
                2 * (win.left + win.right) as usize,
                2 * (win.top + win.bottom) as usize,
            );
            let w = pic.width - sw;
            let h = pic.height - sh;
            let (x0, y0) = (2 * win.left as usize, 2 * win.top as usize);
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let ccw = pic.width / 2;
            let (cx0, cy0) = (x0 / 2, y0 / 2);
            for (plane, pw, ph, ox, oy, stride) in [
                (&pic.y, w, h, x0, y0, pic.width),
                (&pic.cb, cw, ch, cx0, cy0, ccw),
                (&pic.cr, cw, ch, cx0, cy0, ccw),
            ] {
                let mut row_buf = Vec::with_capacity(pw);
                for r in 0..ph {
                    row_buf.clear();
                    row_buf.extend(
                        plane[(oy + r) * stride + ox..][..pw]
                            .iter()
                            .map(|&s| s as u8),
                    );
                    out.write_all(&row_buf).map_err(|e| e.to_string())?;
                }
            }
            *n += 1;
        }
        Ok(())
    };
    for au in access_units(&stream) {
        dec.decode_annex_b(au).map_err(|e| e.to_string())?;
        emit(&mut dec, &mut out, &mut n)?;
    }
    dec.flush().map_err(|e| e.to_string())?;
    emit(&mut dec, &mut out, &mut n)?;
    out.flush().map_err(|e| e.to_string())?;
    println!("frames={n}");
    Ok(())
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("encode") => encode(&args),
        Some("decode") => decode(&args),
        other => Err(format!("unknown command {other:?}")),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hevc_rc: {e}");
            ExitCode::FAILURE
        }
    }
}
