//! Shared helpers: generate streams with ffmpeg, decode them with ffmpeg and with us.
//!
//! The reference decode is `ffmpeg -threads 1 -cpuflags 0`. Single-threaded because libavcodec's
//! frame-threaded MPEG-4 decoder loses the interlaced direct-mode field-MV table across threads
//! (its output then differs from run to run); `-cpuflags 0` because the x86 SIMD `put_no_rnd_pixels8_x2/y2`
//! are not bit-exact with ffmpeg's own C code (`psubusb`-based rounding, +-1 in rare pixels).
//! This decoder matches the C code, which is the reference every other platform runs.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::frame::VideoFrame;
use ec_core::registry::{CodecId, CodecParameters};
use ec_mpeg4::{Mpeg4Decoder, PictureInfo};

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

/// Whether the local ffmpeg lists `name` as an encoder.
pub fn have_encoder(name: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.split_whitespace().nth(1) == Some(name))
        })
        .unwrap_or(false)
}

/// `true` when the test may run; under `EC_REQUIRE_FIXTURES` a missing ffmpeg is a failure.
pub fn gate() -> bool {
    if have_ffmpeg() {
        return true;
    }
    assert!(
        !require(),
        "ffmpeg is required (EC_REQUIRE_FIXTURES is set)"
    );
    eprintln!("skipped: no ffmpeg");
    false
}

pub fn scratch(tag: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("ec-mpeg4-{}-{}", std::process::id(), tag));
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

/// The usual synthetic source: `testsrc2` of `size` at `rate`, `frames` frames.
pub fn src(size: &str, frames: u32) -> Vec<String> {
    vec![
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!("testsrc2=size={size}:rate=25"),
        "-frames:v".into(),
        frames.to_string(),
    ]
}

/// Encode with `enc` (ffmpeg output args) into `path`.
pub fn encode(size: &str, frames: u32, enc: &[&str], path: &Path) -> bool {
    let s = src(size, frames);
    let mut args: Vec<&str> = s.iter().map(|x| x.as_str()).collect();
    args.extend_from_slice(enc);
    args.push(path.to_str().unwrap());
    run(&args)
}

/// ffmpeg's raw I420 decode of `input` (any container or `-f m4v`-style raw ES, `in_args` first).
pub fn reference(input: &Path, in_args: &[&str], out: &Path) -> Result<Vec<u8>, String> {
    let mut args: Vec<&str> = vec!["-threads", "1", "-cpuflags", "0"];
    args.extend_from_slice(in_args);
    args.extend_from_slice(&[
        "-i",
        input.to_str().unwrap(),
        "-map",
        "0:v:0",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-fps_mode",
        "passthrough",
        out.to_str().unwrap(),
    ]);
    if !run(&args) {
        return Err("ffmpeg decode failed".into());
    }
    std::fs::read(out).map_err(|e| e.to_string())
}

/// Frame planes as tight bytes.
pub fn frame_bytes(f: &VideoFrame) -> Vec<u8> {
    let mut v = Vec::new();
    for p in &f.planes {
        v.extend_from_slice(&p.data);
    }
    v
}

/// Split an MPEG-4 elementary stream into access units at VOP / GOP / VOS start codes.
pub fn split_m4v(es: &[u8]) -> Vec<&[u8]> {
    let mut cuts = vec![0usize];
    let mut have_vop = false;
    let mut i = 0;
    while i + 3 < es.len() {
        if es[i] == 0 && es[i + 1] == 0 && es[i + 2] == 1 {
            match es[i + 3] {
                0xB6 => {
                    if have_vop {
                        cuts.push(i);
                    }
                    have_vop = true;
                }
                0xB0 | 0xB3 if have_vop => {
                    cuts.push(i);
                    have_vop = false;
                }
                _ => {}
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    cuts.push(es.len());
    cuts.windows(2).map(|w| &es[w[0]..w[1]]).collect()
}

/// Split an H.263 stream at picture start codes (`00 00 8x`).
pub fn split_h263(es: &[u8]) -> Vec<&[u8]> {
    let mut cuts = vec![];
    let mut i = 0;
    while i + 3 < es.len() {
        if es[i] == 0 && es[i + 1] == 0 && es[i + 2] & 0xFC == 0x80 {
            cuts.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    cuts.push(es.len());
    cuts.windows(2).map(|w| &es[w[0]..w[1]]).collect()
}

pub fn new_decoder() -> Mpeg4Decoder {
    Mpeg4Decoder::new(CodecParameters::new(CodecId::Mpeg4Part2)).unwrap()
}

/// Decode `packets` in order and drain; returns every picture.
pub fn decode_packets<'a>(
    dec: &mut Mpeg4Decoder,
    packets: impl IntoIterator<Item = &'a [u8]>,
) -> Result<Vec<(VideoFrame, PictureInfo)>, String> {
    let mut out = Vec::new();
    for p in packets {
        dec.send_bytes(p, None).map_err(|e| e.to_string())?;
        while let Some(x) = dec.pop_picture() {
            out.push(x);
        }
    }
    dec.flush_stream().map_err(|e| e.to_string())?;
    while let Some(x) = dec.pop_picture() {
        out.push(x);
    }
    Ok(out)
}

/// Compare decoded pictures with the reference bytes; `Err` describes the first difference.
pub fn compare_frames(
    ours: &[(VideoFrame, PictureInfo)],
    reference: &[u8],
) -> Result<usize, String> {
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
            let (plane, off) = if pos < w * h {
                ("Y", pos)
            } else {
                ("C", pos - w * h)
            };
            let nbad = b.iter().zip(r).filter(|(a, c)| a != c).count();
            return Err(format!(
                "frame {i} ({}) differs: {nbad} bytes, first at {plane} +{off} (x={} y={}) ours {} ffmpeg {}",
                info.pict_type,
                off % w.max(1),
                off / w.max(1),
                b[pos],
                r[pos]
            ));
        }
    }
    Ok(ours.len())
}

/// Encode `enc` into a raw ES (`-f m4v` or `-f h263`), decode with both, compare.
pub fn parity_es(
    tag: &str,
    size: &str,
    frames: u32,
    enc: &[&str],
    fmt: &str,
) -> Result<usize, String> {
    let dir = scratch(tag);
    let es_path = dir.join(format!("{tag}.es"));
    let mut e: Vec<&str> = enc.to_vec();
    e.extend_from_slice(&["-f", fmt]);
    if !encode(size, frames, &e, &es_path) {
        return Err(format!("ffmpeg encode failed ({enc:?})"));
    }
    let reference = reference(&es_path, &["-f", fmt], &dir.join("ref.yuv"))?;
    let es = std::fs::read(&es_path).map_err(|e| e.to_string())?;
    let pkts = if fmt == "h263" {
        split_h263(&es)
    } else {
        split_m4v(&es)
    };
    let mut dec = new_decoder();
    let ours = decode_packets(&mut dec, pkts)?;
    let n = compare_frames(&ours, &reference)?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(n)
}
