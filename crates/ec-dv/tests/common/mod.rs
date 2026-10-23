//! Shared helpers: make fixtures with ffmpeg, decode them with ffmpeg and with us.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn require() -> bool {
    std::env::var_os("EC_REQUIRE_FIXTURES").is_some()
}

pub fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().map(|o| o.status.success()).unwrap_or(false)
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
    let dir = base.join(format!("ec-dv-{}-{}", std::process::id(), tag));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn run(args: &[&str]) -> bool {
    let out = Command::new("ffmpeg").args(["-v", "error", "-y", "-nostdin"]).args(args).output();
    match out {
        Ok(o) => {
            if !o.status.success() {
                eprintln!("ffmpeg {:?} failed: {}", args, String::from_utf8_lossy(&o.stderr));
            }
            o.status.success()
        }
        Err(_) => false,
    }
}

/// One DV flavour to synthesise with `-c:v dvvideo`.
#[derive(Clone, Copy, Debug)]
pub struct Flavour {
    pub name: &'static str,
    pub size: &'static str,
    pub rate: &'static str,
    pub pix_fmt: &'static str,
    /// 720p cannot be muxed with audio by ffmpeg's dv muxer.
    pub audio: bool,
    pub frames: u32,
}

pub const FLAVOURS: &[Flavour] = &[
    Flavour { name: "ntsc411", size: "720x480", rate: "30000/1001", pix_fmt: "yuv411p", audio: true, frames: 8 },
    Flavour { name: "pal420", size: "720x576", rate: "25", pix_fmt: "yuv420p", audio: true, frames: 8 },
    Flavour { name: "pal411", size: "720x576", rate: "25", pix_fmt: "yuv411p", audio: true, frames: 6 },
    Flavour { name: "ntsc422", size: "720x480", rate: "30000/1001", pix_fmt: "yuv422p", audio: true, frames: 6 },
    Flavour { name: "pal422", size: "720x576", rate: "25", pix_fmt: "yuv422p", audio: true, frames: 6 },
    Flavour { name: "hd1080i50", size: "1440x1080", rate: "25", pix_fmt: "yuv422p", audio: true, frames: 4 },
    Flavour { name: "hd1080i60", size: "1280x1080", rate: "30000/1001", pix_fmt: "yuv422p", audio: true, frames: 4 },
    Flavour { name: "hd720p50", size: "960x720", rate: "50", pix_fmt: "yuv422p", audio: false, frames: 6 },
    Flavour { name: "hd720p60", size: "960x720", rate: "60000/1001", pix_fmt: "yuv422p", audio: false, frames: 6 },
];

/// Encode a synthetic clip (`testsrc2` + temporal noise, so every quantiser class and both DCT
/// modes show up) as `format` (`dv`, `avi`, `mov`) with ffmpeg's `dvvideo` encoder.
pub fn make(f: &Flavour, tag: &str, format: &str, ilme: bool, noise: u32) -> Option<PathBuf> {
    let dir = scratch(f.name);
    let path = dir.join(format!("{tag}.{format}"));
    let vsrc = format!("testsrc2=size={}:rate={}:duration=3,noise=alls={noise}:allf=t", f.size, f.rate);
    let frames = f.frames.to_string();
    let mut args: Vec<&str> = vec!["-f", "lavfi", "-i", &vsrc];
    let asrc = "sine=frequency=440:sample_rate=48000:duration=3";
    if f.audio {
        args.extend(["-f", "lavfi", "-i", asrc, "-ac", "2"]);
    } else {
        args.push("-an");
    }
    args.extend(["-frames:v", &frames, "-c:v", "dvvideo", "-pix_fmt", f.pix_fmt]);
    if ilme {
        args.extend(["-flags", "+ildct+ilme"]);
    }
    if f.audio {
        args.extend(["-c:a", "pcm_s16le", "-ar", "48000", "-shortest"]);
    }
    args.extend(["-f", format]);
    let p = path.to_str().unwrap().to_string();
    args.push(&p);
    if run(&args) { Some(path) } else { None }
}

/// ffmpeg's decode of the first video stream: raw frames in the stream's native pix_fmt.
pub fn ffmpeg_raw_video(path: &Path, extra: &[&str]) -> Vec<u8> {
    let out = path.with_extension("ref.yuv");
    let p = path.to_str().unwrap();
    let o = out.to_str().unwrap();
    let mut args: Vec<&str> = extra.to_vec();
    args.extend(["-i", p, "-map", "0:v:0", "-f", "rawvideo", o]);
    assert!(run(&args), "ffmpeg decode failed");
    std::fs::read(&out).unwrap()
}

/// ffmpeg's decode of audio stream `n` as s16le.
pub fn ffmpeg_pcm(path: &Path, n: usize) -> Option<Vec<u8>> {
    let out = path.with_extension(format!("ref{n}.pcm"));
    let m = format!("0:a:{n}");
    if !run(&["-i", path.to_str().unwrap(), "-map", &m, "-f", "s16le", out.to_str().unwrap()]) {
        return None;
    }
    std::fs::read(&out).ok()
}
