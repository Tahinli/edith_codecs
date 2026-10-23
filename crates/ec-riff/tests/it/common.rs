//! Helpers shared by the integration-test modules in this binary.

use std::process::Command;

use ec_core::frame::VideoFrame;

pub(crate) fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend((body.len() as u32).to_le_bytes());
    v.extend(body);
    if body.len() % 2 == 1 {
        v.push(0);
    }
    v
}

pub(crate) fn list(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = kind.to_vec();
    b.extend(body);
    chunk(b"LIST", &b)
}

pub(crate) fn tool_ok(name: &str) -> bool {
    Command::new(name)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub(crate) fn ffmpeg(args: &[&str]) {
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-nostdin"])
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "ffmpeg {args:?} failed");
}

pub(crate) fn frame_bytes(f: &VideoFrame) -> Vec<u8> {
    let mut out = Vec::new();
    for (p, (row, rows)) in f
        .planes
        .iter()
        .zip(f.format.plane_geometry(f.width, f.height))
    {
        for y in 0..rows {
            out.extend_from_slice(p.row(y, row).unwrap());
        }
    }
    out
}
