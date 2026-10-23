//! Shared helpers: encode with ffmpeg, decode with ffmpeg and with us.
#![allow(dead_code)]

use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;

use ec_core::registry::{CodecId, CodecParameters, Demuxer};
use ec_mp4::Mp4Demuxer;
use ec_prores::{DecodedFrame, ProResDecoder};

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

/// Skip (or fail under EC_REQUIRE_FIXTURES) when ffmpeg is missing.
pub fn ffmpeg_or_skip() -> bool {
    if have_ffmpeg() {
        return true;
    }
    assert!(!require(), "EC_REQUIRE_FIXTURES set but ffmpeg is not available");
    eprintln!("skipping: no ffmpeg");
    false
}

pub fn scratch(tag: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("ec-prores-{}-{}", std::process::id(), tag));
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

/// ProRes packets (frame bytes) of the first ProRes track of an mp4/mov.
pub fn packets(mov: &[u8]) -> Vec<Vec<u8>> {
    let mut dm = Mp4Demuxer::new(Cursor::new(mov.to_vec())).expect("demux");
    let vs = dm.streams().iter().position(|s| s.params.codec == CodecId::ProRes).expect("no ProRes stream") as u32;
    let mut out = Vec::new();
    while let Ok(p) = dm.next_packet() {
        if p.stream == vs {
            out.push(p.data.to_vec());
        }
    }
    out
}

pub fn decoder(threads: usize) -> ProResDecoder {
    let mut d = ProResDecoder::new(CodecParameters::new(CodecId::ProRes)).unwrap();
    d.set_threads(threads);
    d
}

/// Our decode as ffmpeg's rawvideo layout: planes tight, little-endian 16-bit; alpha (when
/// present) as a fourth plane scaled to 12 bits (`>> 4`), as ffmpeg's yuva444p12 holds it.
pub fn raw_ours(frames: &[DecodedFrame]) -> Vec<u8> {
    let mut v = Vec::new();
    for f in frames {
        let (w, h) = (f.video.width as usize, f.video.height as usize);
        let (sx, sy) = f.video.format.chroma_shift();
        for (i, pl) in f.video.planes.iter().enumerate() {
            let (pw, ph) = if i == 0 { (w, h) } else { (w.div_ceil(1 << sx), h.div_ceil(1 << sy)) };
            for y in 0..ph {
                v.extend_from_slice(&pl.data[y * pl.stride..y * pl.stride + pw * 2]);
            }
        }
        if let Some(a) = &f.alpha {
            for y in 0..h {
                for x in 0..w {
                    v.extend_from_slice(&(a.data[y * a.stride + x] >> 4).to_le_bytes());
                }
            }
        }
    }
    v
}

pub fn decode_ours(mov: &[u8], threads: usize) -> Result<Vec<DecodedFrame>, String> {
    let mut dec = decoder(threads);
    packets(mov).iter().map(|p| dec.decode_full(p).map_err(|e| e.to_string())).collect()
}

/// ffmpeg's decode of `path` as raw `pix_fmt`.
pub fn raw_ffmpeg(tag: &str, path: &std::path::Path, pix_fmt: &str) -> Result<Vec<u8>, String> {
    let out = scratch(tag).join("ref.raw");
    let ok = run(&[
        "-i",
        path.to_str().unwrap(),
        "-f",
        "rawvideo",
        "-pix_fmt",
        pix_fmt,
        "-fps_mode",
        "passthrough",
        out.to_str().unwrap(),
    ]);
    if !ok {
        return Err("ffmpeg decode failed".into());
    }
    std::fs::read(&out).map_err(|e| e.to_string())
}

/// First differing byte offset, if any.
pub fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    if a.len() != b.len() {
        return Some(a.len().min(b.len()));
    }
    a.iter().zip(b).position(|(x, y)| x != y)
}
