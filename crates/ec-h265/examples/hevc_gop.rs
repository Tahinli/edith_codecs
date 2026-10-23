//! Drive the sequence encoder and the decoder from raw files, for the round-trip
//! and rate measurements in `scripts/`.
//!
//! ```text
//! hevc_gop encode <in.yuv> <width> <height> <keyint> <qp> <out.hevc> <recon.yuv> [sh]
//! hevc_gop decode <in.hevc> <out.yuv>
//! ```
//!
//! `in.yuv` and `recon.yuv` are raw I420 at the displayed size. A `keyint` of 1
//! codes every picture as an IDR, which is the intra-only stream.
//!
//! `EC_H265_BITDEPTH=10` makes the input and output 10-bit I010 (16-bit little-endian
//! samples), the Main 10 path. `EC_H265_RC=<kbps>@<fps>` codes at a bitrate instead of
//! `qp` (the `qp` argument is then ignored).

use ec_core::frame::{PixelFormat, Plane, VideoFrame};
use ec_h265::Decoder;
use ec_h265::encoder::{Encoder, EncoderConfig, RateControl};
use std::process::ExitCode;

/// Bytes per sample: 1 for 8-bit, 2 for 10-bit (`EC_H265_BITDEPTH`).
fn bytes_per_sample() -> usize {
    match std::env::var("EC_H265_BITDEPTH").ok().as_deref() {
        Some("10") => 2,
        _ => 1,
    }
}

fn frame_size(w: usize, h: usize) -> usize {
    (w * h + 2 * w.div_ceil(2) * h.div_ceil(2)) * bytes_per_sample()
}

fn split_frame(buf: &[u8], w: usize, h: usize) -> VideoFrame {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let bps = bytes_per_sample();
    let (ys, cs) = (w * h * bps, cw * ch * bps);
    let y = buf[..ys].to_vec();
    let cb = buf[ys..ys + cs].to_vec();
    let cr = buf[ys + cs..ys + 2 * cs].to_vec();
    let format = if bps == 2 {
        PixelFormat::I010
    } else {
        PixelFormat::I420
    };
    VideoFrame::try_new(
        format,
        w as u32,
        h as u32,
        vec![
            Plane::new(y, w * bps),
            Plane::new(cb, cw * bps),
            Plane::new(cr, cw * bps),
        ],
    )
    .expect("planes at the stated size")
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |i: usize| {
        args.get(i)
            .cloned()
            .ok_or_else(|| format!("missing argument {i}"))
    };
    match arg(1)?.as_str() {
        "encode" => {
            let input = std::fs::read(arg(2)?).map_err(|e| e.to_string())?;
            let w: usize = arg(3)?.parse().map_err(|_| "width")?;
            let h: usize = arg(4)?.parse().map_err(|_| "height")?;
            let keyint: usize = arg(5)?.parse().map_err(|_| "keyint")?;
            let qp: i32 = arg(6)?.parse().map_err(|_| "qp")?;
            let fs = frame_size(w, h);
            let frames: Vec<VideoFrame> = input
                .chunks_exact(fs)
                .map(|c| split_frame(c, w, h))
                .collect();
            let mut cfg = EncoderConfig::new(w as u32, h as u32);
            cfg.rate_control = RateControl::ConstantQp(qp);
            if let Some((kbps, fps)) = std::env::var("EC_H265_RC").ok().and_then(|v| {
                let (k, f) = v.split_once('@')?;
                Some((k.parse::<u32>().ok()?, f.parse::<f64>().ok()?))
            }) {
                cfg.rate_control = RateControl::Bitrate { kbps, fps };
            }
            cfg.bit_depth = if bytes_per_sample() == 2 { 10 } else { 8 };
            cfg.keyint = keyint;
            // EC_H265_PRESET=fast|faster selects the speed preset.
            cfg.speed = match std::env::var("EC_H265_PRESET").ok().as_deref() {
                Some("fast") => ec_h265::encoder::Preset::Fast,
                Some("faster") => ec_h265::encoder::Preset::Faster,
                _ => ec_h265::encoder::Preset::Default,
            };
            cfg.keep_recon = true;
            // Optional trailing flags: `sh` turns sign data hiding on.
            cfg.sign_hiding = args.iter().skip(9).any(|a| a == "sh");
            // Sweep override of the P-mode decision lambda scale (experiments only).
            if let Some(v) = std::env::var("EC_INTER_LAMBDA_SCALE")
                .ok()
                .and_then(|v| v.parse::<f64>().ok())
            {
                cfg.inter_lambda_scale = v;
            }
            // Experiments: worker threads and the P-picture wavefront (`0` or `off`).
            if let Some(v) = std::env::var("EC_H265_THREADS")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
            {
                cfg.threads = v;
            }
            if let Ok(v) = std::env::var("EC_H265_INTER_WPP") {
                cfg.inter_wpp = v != "0" && v != "off";
            }
            // EC_H265_HDR=pq|hlg: BT.2020 colour tags (+ PQ mastering/light SEI), full
            // range via EC_H265_FULL_RANGE=1. Used by scripts/hevc10-hdr-check.sh.
            if let Ok(mode) = std::env::var("EC_H265_HDR") {
                use ec_h265_syntax::vui::{ColourDescription, VideoSignalType};
                let transfer = if mode == "hlg" { 18 } else { 16 };
                cfg.video_signal_type = Some(VideoSignalType {
                    video_format: 5,
                    video_full_range_flag: std::env::var("EC_H265_FULL_RANGE")
                        .is_ok_and(|v| v == "1"),
                    colour_description: Some(ColourDescription {
                        colour_primaries: 9,
                        transfer_characteristics: transfer,
                        matrix_coeffs: 9,
                    }),
                });
                cfg.chroma_sample_loc = Some((2, 2));
                if mode == "pq" {
                    cfg.mastering_display = Some(ec_h265_syntax::sei::MasteringDisplay {
                        display_primaries: [(13250, 34500), (7500, 3000), (34000, 16000)],
                        white_point: (15635, 16450),
                        max_luminance: 10_000_000,
                        min_luminance: 50,
                    });
                    cfg.content_light.max_cll = Some(1000.0);
                    cfg.content_light.max_fall = Some(400.0);
                }
            }
            let enc = Encoder::new(cfg).map_err(|e| e.to_string())?;
            let t_enc = std::time::Instant::now();
            let coded = enc.encode_sequence(&frames).map_err(|e| e.to_string())?;
            if std::env::var("EC_H265_TIMING").is_ok() {
                eprintln!("encode_sequence: {:.3}s", t_enc.elapsed().as_secs_f64());
            }
            let mut stream = Vec::new();
            let mut recon = Vec::new();
            for pic in &coded {
                stream.extend_from_slice(&pic.au);
                if let Some(r) = &pic.recon {
                    for p in &r.planes {
                        recon.extend_from_slice(&p.data);
                    }
                }
            }
            std::fs::write(arg(7)?, &stream).map_err(|e| e.to_string())?;
            std::fs::write(arg(8)?, &recon).map_err(|e| e.to_string())?;
            println!("frames={} bytes={}", coded.len(), stream.len());
            Ok(())
        }
        "decode" => {
            let stream = std::fs::read(arg(2)?).map_err(|e| e.to_string())?;
            let mut dec = Decoder::new();
            dec.decode_annex_b(&stream).map_err(|e| e.to_string())?;
            dec.flush().map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            let mut n = 0;
            while let Some(f) = dec.next_frame() {
                let pic = &f.picture;
                let bit_depth = pic.bit_depth;
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
                    for r in 0..ph {
                        for c in 0..pw {
                            let v = plane[(oy + r) * stride + ox + c];
                            if bit_depth == 10 {
                                out.extend_from_slice(&v.to_le_bytes());
                            } else {
                                out.push(v as u8);
                            }
                        }
                    }
                }
                n += 1;
            }
            std::fs::write(arg(3)?, &out).map_err(|e| e.to_string())?;
            println!("frames={n}");
            Ok(())
        }
        other => Err(format!("unknown command {other}")),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hevc_gop: {e}");
            ExitCode::FAILURE
        }
    }
}
