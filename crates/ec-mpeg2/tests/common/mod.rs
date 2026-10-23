//! Shared helpers: generate streams with ffmpeg, decode with ffmpeg and with us.
#![allow(dead_code)]

pub mod synth;

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::frame::VideoFrame;
use ec_mpeg2::Mpeg2Decoder;

pub fn require() -> bool {
    std::env::var_os("EC_REQUIRE_FIXTURES").is_some()
}

pub fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn scratch(tag: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("ec-mpeg2-{}-{}", std::process::id(), tag));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn run(args: &[&str]) -> bool {
    Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-nostdin"])
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Frame planes as tight bytes.
pub fn frame_bytes(f: &VideoFrame) -> Vec<u8> {
    let mut v = Vec::new();
    for p in &f.planes {
        v.extend_from_slice(&p.data);
    }
    v
}

pub fn decode_ours(es: &[u8], chunk: usize) -> Result<Vec<(VideoFrame, ec_mpeg2::PictureInfo)>, String> {
    let mut dec = Mpeg2Decoder::new(ec_core::registry::CodecParameters::new(ec_core::registry::CodecId::Mpeg2Video))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for c in es.chunks(chunk.max(1)) {
        dec.send_bytes(c, None).map_err(|e| e.to_string())?;
        while let Some(p) = dec.pop_picture() {
            out.push(p);
        }
    }
    dec.flush_stream().map_err(|e| e.to_string())?;
    while let Some(p) = dec.pop_picture() {
        out.push(p);
    }
    Ok(out)
}

/// Compare ours with ffmpeg's rawvideo decode of `es_path`.
pub fn compare(tag: &str, es_path: &Path, pix_fmt: &str, extra_dec: &[&str]) -> Result<usize, String> {
    let dir = scratch(tag);
    let refp = dir.join("ref.yuv");
    let mut args: Vec<&str> = vec![];
    args.extend_from_slice(extra_dec);
    let es_s = es_path.to_str().unwrap();
    let ref_s = refp.to_str().unwrap();
    args.extend_from_slice(&["-i", es_s, "-f", "rawvideo", "-pix_fmt", pix_fmt, "-fps_mode", "passthrough", ref_s]);
    if !run(&args) {
        return Err("ffmpeg decode failed".into());
    }
    let reference = std::fs::read(&refp).map_err(|e| e.to_string())?;
    let es = std::fs::read(es_path).map_err(|e| e.to_string())?;
    let ours = decode_ours(&es, 4093)?;
    if ours.is_empty() {
        return Err("no frames".into());
    }
    let fsz = frame_bytes(&ours[0].0).len();
    let n_ref = reference.len() / fsz;
    if n_ref != ours.len() {
        return Err(format!("frame count: ours {} ffmpeg {}", ours.len(), n_ref));
    }
    for (i, (f, info)) in ours.iter().enumerate() {
        let b = frame_bytes(f);
        let r = &reference[i * fsz..(i + 1) * fsz];
        if b != r {
            let pos = b.iter().zip(r).position(|(a, c)| a != c).unwrap();
            let w = f.width as usize;
            let h = f.height as usize;
            let (plane, off) = if pos < w * h { ("Y", pos) } else { ("C", pos - w * h) };
            let nbad = b.iter().zip(r).filter(|(a, c)| a != c).count();
            if std::env::var_os("EC_DIFFS").is_some() {
                let d: Vec<(usize, usize, i32)> = b.iter().zip(r).enumerate().filter(|(_, (a, c))| a != c).take(60).map(|(i, (a, c))| if i < w * h { (i % w, i / w, *a as i32 - *c as i32) } else { (i, 0, 0) }).collect();
                eprintln!("DIFFS {d:?}");
            }
            return Err(format!(
                "frame {i} ({:?} tr {}) differs: {} bytes, first at {plane} +{off} (x={} y={}) ours {} ffmpeg {}",
                info.picture_type,
                info.temporal_reference,
                nbad,
                off % w,
                off / w,
                b[pos],
                r[pos]
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(ours.len())
}
