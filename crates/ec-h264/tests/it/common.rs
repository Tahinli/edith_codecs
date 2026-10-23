//! Helpers shared by the merged integration-test modules: byte layouts, the
//! ffmpeg pipes and stream utilities that were identical in each file.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::frame::{PixelFormat, VideoFrame};

/// One decoded frame as planar bytes (16-bit little endian samples for High 10).
pub(super) fn frame_bytes(frame: &VideoFrame) -> Vec<u8> {
    let bytes_per_sample = match frame.format {
        PixelFormat::I422 => 1,
        PixelFormat::I210 => 2,
        other => panic!("unexpected output format {other:?}"),
    };
    let (w, h) = (frame.width as usize, frame.height as usize);
    let dims = [(w, h), (w.div_ceil(2), h), (w.div_ceil(2), h)];
    let mut out = Vec::new();
    for (plane, (pw, ph)) in frame.planes.iter().zip(dims) {
        for y in 0..ph {
            out.extend_from_slice(plane.row(y, pw * bytes_per_sample).expect("plane row"));
        }
    }
    out
}

pub(super) fn ffmpeg_frames(stream: &Path, pix_fmt: &str, frame_len: usize) -> Option<Vec<u8>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(stream)
        .args(["-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
        .output()
        .ok()?;
    (out.status.success() && !out.stdout.is_empty() && out.stdout.len() % frame_len == 0)
        .then_some(out.stdout)
}

pub(super) fn escape(rbsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rbsp.len() + 8);
    let mut zeros = 0;
    for &b in rbsp {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

pub(super) fn find_stream(dir: &Path) -> Option<PathBuf> {
    let mut fallback = None;
    for entry in std::fs::read_dir(dir).ok()? {
        let p = entry.ok()?.path();
        let ext = p.extension()?.to_str()?.to_ascii_lowercase();
        if matches!(ext.as_str(), "jsv" | "264" | "avc" | "26l" | "h264" | "jvt") {
            return Some(p);
        }
        if ext == "bit" {
            fallback = Some(p);
        }
    }
    fallback
}

/// (bytes per sample, chroma width divisor, chroma height divisor)
pub(super) fn layout(f: PixelFormat) -> (usize, usize, usize) {
    match f {
        PixelFormat::I444 => (1, 1, 1),
        PixelFormat::I410 => (2, 1, 1),
        PixelFormat::I422 => (1, 2, 1),
        PixelFormat::I210 => (2, 2, 1),
        PixelFormat::I420 => (1, 2, 2),
        PixelFormat::I010 => (2, 2, 2),
        other => panic!("unexpected output format {other:?}"),
    }
}

pub(super) fn ffmpeg_pix_frames(stream: &Path, pix_fmt: &str) -> Option<Vec<u8>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-f", "h264", "-i"])
        .arg(stream)
        .args(["-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
        .output()
        .ok()?;
    (out.status.success() && !out.stdout.is_empty()).then_some(out.stdout)
}
