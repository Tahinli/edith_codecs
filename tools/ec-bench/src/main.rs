//! Standing performance harness: real media in, each codec's own public
//! one-shot API exercised, a ranked realtime-factor table out.
//!
//! Realtime factor = media-seconds decoded (or encoded) per wall-second;
//! bigger is faster. ffmpeg/ffprobe are used only to extract elementary
//! streams and raw planes and to read duration, exactly as the crates' own
//! tests already do -- this binary never decodes anything itself.
//!
//! Run: `cargo run -p ec-bench --release`

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use ec_av1::encode::Picture as Av1Picture;
use ec_av1::encoder::{Av1Encoder, Colour, EncoderConfig as Av1Config};
use ec_core::frame::{PixelFormat, Plane, VideoFrame};
use ec_core::registry::MediaType;
use ec_h264::{
    Decoder as H264Decoder, Encoder as H264Encoder, EncoderConfig as H264Config, NalOutcome,
    PictureView,
};
use ec_h264_syntax::AnnexBIter;
use ec_h265::encoder::{Encoder as H265Encoder, EncoderConfig as H265Config};
use ec_probe::Reader;

struct Row {
    component: &'static str,
    direction: &'static str,
    content: String,
    media: String,
    wall_ms: f64,
    rtf: Option<f64>,
}

fn main() {
    let mut rows = Vec::new();

    bench_h264(&mut rows);
    bench_h265_encode(&mut rows);
    bench_av1_encode(&mut rows);
    bench_audio_decode(&mut rows);

    rows.sort_by(|a, b| {
        a.rtf
            .unwrap_or(f64::INFINITY)
            .partial_cmp(&b.rtf.unwrap_or(f64::INFINITY))
            .unwrap()
    });

    println!("| component | direction | content | media | wall ms | realtime factor |");
    println!("|---|---|---|---|---|---|");
    for r in &rows {
        let rtf = r
            .rtf
            .map_or_else(|| "-".to_string(), |v| format!("{v:.2}x"));
        println!(
            "| {} | {} | {} | {} | {:.1} | {} |",
            r.component, r.direction, r.content, r.media, r.wall_ms, rtf
        );
    }
}

// ---------------------------------------------------------------------------
// media discovery
// ---------------------------------------------------------------------------

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME"))
}

/// A handful of real-library candidates, largest-effort first, that ffprobe
/// confirms carry the wanted video codec. Read-only: never touches the file
/// beyond `ffprobe`/`ffmpeg -i ... -o <scratch>`.
fn real_video(codec: &str) -> Option<PathBuf> {
    let roots = [home().join("Videos"), home().join("Downloads")];
    let mut found = Vec::new();
    for root in roots {
        let Ok(walker) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in walker.flatten() {
            let p = entry.path();
            let Some(ext) = p.extension().and_then(|e| e.to_str()) else {
                continue;
            };
            if !matches!(
                ext.to_ascii_lowercase().as_str(),
                "mp4" | "mkv" | "mov" | "avi"
            ) {
                continue;
            }
            found.push(p);
        }
    }
    found.sort();
    for p in found {
        if ffprobe_field(&p, "v:0", "codec_name").as_deref() == Some(codec) {
            return Some(p);
        }
    }
    None
}

fn ffprobe_field(path: &Path, stream: &str, key: &str) -> Option<String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", stream, "-show_entries"])
        .arg(format!("stream={key}"))
        .args(["-of", "default=nw=1:nk=1"])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

// ---------------------------------------------------------------------------
// h264: decode (real film, 10s) + encode (raw planes from the same clip)
// ---------------------------------------------------------------------------

fn bench_h264(rows: &mut Vec<Row>) {
    let Some(src) = real_video("h264") else {
        rows.push(missing("ec-h264", "decode"));
        rows.push(missing("ec-h264", "encode"));
        return;
    };
    let w: u32 = ffprobe_field(&src, "v:0", "width")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let h: u32 = ffprobe_field(&src, "v:0", "height")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    // decode: 10s Annex-B extract, trimmed inside ffmpeg (never decode-then-trim).
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-t", "10", "-i"])
        .arg(&src)
        .args([
            "-an",
            "-c:v",
            "copy",
            "-bsf:v",
            "h264_mp4toannexb",
            "-f",
            "h264",
            "-",
        ])
        .output()
        .expect("ffmpeg extract h264");
    if out.status.success() && !out.stdout.is_empty() {
        let bytes = out.stdout;
        let start = Instant::now();
        let mut dec = H264Decoder::new();
        let mut frames = 0u64;
        for nal in AnnexBIter::new(&bytes) {
            if let Ok(NalOutcome::PictureBoundary) = dec.push_nal(nal) {
                let _ = dec.end_picture();
                let _ = dec.push_nal(nal);
            }
            while dec.next_frame().is_some() {
                frames += 1;
            }
        }
        let _ = dec.flush();
        while dec.next_frame().is_some() {
            frames += 1;
        }
        let wall = start.elapsed().as_secs_f64();
        let fps = ffprobe_field(&src, "v:0", "r_frame_rate")
            .and_then(|s| parse_ratio(&s))
            .unwrap_or(25.0);
        let media_s = frames as f64 / fps;
        rows.push(Row {
            component: "ec-h264",
            direction: "decode",
            content: format!("{w}x{h} film, {frames} frames"),
            media: format!("{media_s:.1}s"),
            wall_ms: wall * 1000.0,
            rtf: (wall > 0.0).then_some(media_s / wall),
        });
    } else {
        rows.push(missing("ec-h264", "decode"));
    }

    // encode: 30 raw yuv420p frames scaled to 640x360 from the same clip.
    let (planes, ew, eh, n) = extract_yuv420p(&src, 640, 360, 30);
    if n > 0 {
        let mut cfg = H264Config::new(ew, eh);
        cfg.threads = 1; // one worker: single-core cost, not a parallelism claim
        let mut enc = H264Encoder::new(cfg).expect("h264 encoder");
        let frame_len = (ew * eh + 2 * (ew.div_ceil(2) * eh.div_ceil(2))) as usize;
        let start = Instant::now();
        for frame in planes.chunks_exact(frame_len) {
            let (y, rest) = frame.split_at((ew * eh) as usize);
            let (u, v) = rest.split_at(rest.len() / 2);
            let view = PictureView::i420(ew, eh, y, u, v);
            enc.encode(&view).expect("h264 encode");
        }
        let wall = start.elapsed().as_secs_f64();
        let media_s = n as f64 / 30.0;
        rows.push(Row {
            component: "ec-h264",
            direction: "encode",
            content: format!("{ew}x{eh}, {n} frames, 1 thread"),
            media: format!("{media_s:.1}s"),
            wall_ms: wall * 1000.0,
            rtf: (wall > 0.0).then_some(media_s / wall),
        });
    } else {
        rows.push(missing("ec-h264", "encode"));
    }
}

/// `n` progressive frames of raw I420 at `w x h`, scaled and trimmed inside
/// ffmpeg.
fn extract_yuv420p(src: &Path, w: u32, h: u32, n: u32) -> (Vec<u8>, u32, u32, u32) {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(src)
        .args([
            "-vf",
            &format!("scale={w}:{h}"),
            "-frames:v",
            &n.to_string(),
            "-pix_fmt",
            "yuv420p",
            "-f",
            "rawvideo",
            "-",
        ])
        .output()
        .expect("ffmpeg extract yuv420p");
    if !out.status.success() || out.stdout.is_empty() {
        return (Vec::new(), w, h, 0);
    }
    let frame_len = (w * h + 2 * (w.div_ceil(2) * h.div_ceil(2))) as usize;
    let frames = (out.stdout.len() / frame_len) as u32;
    (out.stdout, w, h, frames)
}

fn parse_ratio(s: &str) -> Option<f64> {
    let (n, d) = s.split_once('/')?;
    let (n, d) = (n.parse::<f64>().ok()?, d.parse::<f64>().ok()?);
    (d != 0.0).then_some(n / d)
}

// ---------------------------------------------------------------------------
// h265: encode only -- this crate has no decoder (intra-only HEVC encoder).
// ---------------------------------------------------------------------------

fn bench_h265_encode(rows: &mut Vec<Row>) {
    rows.push(missing_direction(
        "ec-h265",
        "decode",
        "encode-only crate, no decoder present",
    ));
    let Some(src) = real_video("h264").or_else(|| real_video("hevc")) else {
        rows.push(missing("ec-h265", "encode"));
        return;
    };
    let (planes, w, h, n) = extract_yuv420p(&src, 640, 360, 10);
    if n == 0 {
        rows.push(missing("ec-h265", "encode"));
        return;
    }
    let cfg = H265Config::new(w, h);
    let enc = H265Encoder::new(cfg).expect("h265 encoder");
    let frame_len = (w * h + 2 * (w.div_ceil(2) * h.div_ceil(2))) as usize;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let start = Instant::now();
    for frame in planes.chunks_exact(frame_len) {
        let (y, rest) = frame.split_at((w * h) as usize);
        let (u, v) = rest.split_at(rest.len() / 2);
        let vf = VideoFrame::try_new(
            PixelFormat::I420,
            w,
            h,
            vec![
                Plane::new(y.to_vec(), w as usize),
                Plane::new(u.to_vec(), cw as usize),
                Plane::new(v.to_vec(), cw as usize),
            ],
        )
        .expect("i420 frame");
        let _ = ch; // silence unused if plane_geometry ignores it
        enc.encode_idr(&vf).expect("h265 encode");
    }
    let wall = start.elapsed().as_secs_f64();
    let media_s = n as f64 / 30.0;
    rows.push(Row {
        component: "ec-h265",
        direction: "encode",
        content: format!("{w}x{h}, {n} frames, IDR-only"),
        media: format!("{media_s:.1}s"),
        wall_ms: wall * 1000.0,
        rtf: (wall > 0.0).then_some(media_s / wall),
    });
}

// ---------------------------------------------------------------------------
// av1: encode only -- same shape as h265.
// ---------------------------------------------------------------------------

fn bench_av1_encode(rows: &mut Vec<Row>) {
    rows.push(missing_direction(
        "ec-av1",
        "decode",
        "encode-only crate, no decoder present",
    ));
    let Some(src) = real_video("h264") else {
        rows.push(missing("ec-av1", "encode"));
        return;
    };
    // AV1's block grid is 32x32 superblocks: pick a size already a multiple of it.
    let (planes, w, h, n) = extract_yuv420p(&src, 640, 384, 10);
    if n == 0 {
        rows.push(missing("ec-av1", "encode"));
        return;
    }
    let cfg = Av1Config {
        width: w as usize,
        height: h as usize,
        base_q_idx: 100,
        gop: 10,
        colour: Colour::default(),
    };
    let mut enc = Av1Encoder::new(cfg).expect("av1 encoder");
    let frame_len = (w * h + 2 * (w.div_ceil(2) * h.div_ceil(2))) as usize;
    let start = Instant::now();
    for frame in planes.chunks_exact(frame_len) {
        let (y, rest) = frame.split_at((w * h) as usize);
        let (u, v) = rest.split_at(rest.len() / 2);
        let pic = Av1Picture {
            width: w as usize,
            height: h as usize,
            y: y.to_vec(),
            u: u.to_vec(),
            v: v.to_vec(),
        };
        enc.encode(&pic).expect("av1 encode");
    }
    let wall = start.elapsed().as_secs_f64();
    let media_s = n as f64 / 30.0;
    rows.push(Row {
        component: "ec-av1",
        direction: "encode",
        content: format!("{w}x{h}, {n} frames, gop=10"),
        media: format!("{media_s:.1}s"),
        wall_ms: wall * 1000.0,
        rtf: (wall > 0.0).then_some(media_s / wall),
    });
}

// ---------------------------------------------------------------------------
// audio decode: one loop over ec-probe's unified reader covers every codec
// it wraps (mp3/flac/aac/alac/ac3/truehd/opus/vorbis via ogg/mp4/matroska).
// ---------------------------------------------------------------------------

fn bench_audio_decode(rows: &mut Vec<Row>) {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(fixtures().join("audio")) {
        candidates.extend(entries.flatten().map(|e| e.path()));
    }
    // Real-library samples: mp3 and wav are on disk; ac3 lives inside the
    // Battle Royale mux (5.1 AC-3), extracted as its own elementary file.
    for f in ["8a3b6d1d19.mp3", "test.wav"] {
        let p = home().join("Downloads").join(f);
        if p.exists() {
            candidates.push(p);
        }
    }
    if let Some(src) = real_video("h264") {
        if ffprobe_field(&src, "a:0", "codec_name").as_deref() == Some("ac3") {
            let out = Command::new("ffmpeg")
                .args(["-v", "error", "-t", "10", "-i"])
                .arg(&src)
                .args(["-vn", "-c:a", "copy", "-f", "ac3"])
                .arg(
                    Path::new(&std::env::var("HOME").unwrap())
                        .join(".cache/bench-media/sample.ac3"),
                )
                .output();
            if let Ok(o) = out {
                if o.status.success() {
                    candidates.push(home().join(".cache/bench-media/sample.ac3"));
                }
            }
        }
    }
    candidates.sort();
    candidates.dedup();

    for path in candidates {
        let Ok(mut reader) = Reader::open(&path) else {
            continue;
        };
        let Some(stream) = reader.default_stream(MediaType::Audio).map(|s| s.index) else {
            continue;
        };
        let Ok(mut dec) = reader.make_decoder(stream) else {
            continue;
        };
        let codec = format!("{:?}", dec.codec());
        let mut samples = 0u64;
        let start = Instant::now();
        let mut out = Vec::new();
        loop {
            match reader.next_packet() {
                Ok(pkt) if pkt.stream == stream => {
                    out.clear();
                    if dec.decode(&pkt, &mut out).is_ok() {
                        samples += (out.len() / dec.channels().max(1)) as u64;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let _ = dec.flush(&mut out);
        let wall = start.elapsed().as_secs_f64();
        let media_s = samples as f64 / dec.sample_rate().max(1) as f64;
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        rows.push(Row {
            component: Box::leak(format!("ec-probe/{codec}").into_boxed_str()),
            direction: "decode",
            content: name,
            media: format!("{media_s:.1}s"),
            wall_ms: wall * 1000.0,
            rtf: (wall > 0.0 && media_s > 0.0).then_some(media_s / wall),
        });
    }
}

fn missing(component: &'static str, direction: &'static str) -> Row {
    missing_direction(component, direction, "no real-media candidate found")
}

fn missing_direction(component: &'static str, direction: &'static str, why: &str) -> Row {
    Row {
        component,
        direction,
        content: why.to_string(),
        media: "-".to_string(),
        wall_ms: 0.0,
        rtf: None,
    }
}
