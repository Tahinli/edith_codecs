//! Helpers shared by the integration tests: ffmpeg encode/decode, container demux.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::Demuxer;
use ec_core::frame::VideoFrame;

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

/// True when the test may run; panics instead of skipping when fixtures are required.
pub fn can_run() -> bool {
    if have_ffmpeg() {
        return true;
    }
    assert!(!require(), "ffmpeg is required (EC_REQUIRE_FIXTURES set)");
    eprintln!("skipping: no ffmpeg");
    false
}

pub fn scratch(tag: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("ec-dnxhd-{}-{}", std::process::id(), tag));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn ffmpeg(args: &[&str]) -> bool {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-nostdin"])
        .args(args)
        .output();
    match out {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            eprintln!("ffmpeg {args:?}: {}", String::from_utf8_lossy(&o.stderr));
            false
        }
        Err(_) => false,
    }
}

/// Frame payloads of the first video stream of an MP4/MOV file.
pub fn mov_frames(path: &Path) -> Vec<Vec<u8>> {
    let f = std::io::BufReader::new(std::fs::File::open(path).unwrap());
    let mut d = ec_mp4::Mp4Demuxer::new(f).unwrap();
    assert_eq!(d.streams()[0].params.codec, ec_core::registry::CodecId::Dnxhd);
    let mut out = Vec::new();
    while let Ok(p) = d.next_packet() {
        out.push(p.data.to_vec());
    }
    out
}

pub fn mkv_frames(path: &Path) -> Vec<Vec<u8>> {
    let f = std::io::BufReader::new(std::fs::File::open(path).unwrap());
    let mut d = ec_matroska::MatroskaDemuxer::new(f).unwrap();
    assert_eq!(d.streams()[0].params.codec, ec_core::registry::CodecId::Dnxhd);
    let mut out = Vec::new();
    while let Ok(p) = d.next_packet() {
        out.push(p.data.to_vec());
    }
    out
}

pub fn avi_frames(path: &Path) -> Vec<Vec<u8>> {
    let f = std::io::BufReader::new(std::fs::File::open(path).unwrap());
    let mut d = ec_riff::AviDemuxer::new(f).unwrap();
    assert_eq!(d.streams()[0].params.codec, ec_core::registry::CodecId::Dnxhd);
    let mut out = Vec::new();
    while let Ok(p) = d.next_packet() {
        out.push(p.data.to_vec());
    }
    out
}

/// Tight bytes of a frame's planes.
pub fn frame_bytes(f: &VideoFrame) -> Vec<u8> {
    let mut v = Vec::new();
    for (p, (row, rows)) in f.planes.iter().zip(f.format.plane_geometry(f.width, f.height)) {
        for y in 0..rows {
            v.extend_from_slice(&p.data[y * p.stride..y * p.stride + row]);
        }
    }
    v
}
