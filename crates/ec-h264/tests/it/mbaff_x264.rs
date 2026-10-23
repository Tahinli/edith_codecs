//! MBAFF decode, byte-exact against ffmpeg, on streams libx264 writes with
//! `interlaced=1` (macroblock-adaptive frame/field coding) from synthetic
//! sources: CABAC and CAVLC, top and bottom field first, B-frames with
//! spatial and temporal direct, weighted prediction, the 8x8 transform,
//! several slices, and High 10.
//!
//! Every case is generated in the test (no committed binaries). A case skips,
//! saying so, when ffmpeg or the needed libx264 is absent, and fails instead
//! when `EC_REQUIRE_FIXTURES` is set. The 1920x1080 cases are slow enough to be
//! opt-in: set `EC_H264_MBAFF_HD=1` (the lane gate does).

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::frame::{PixelFormat, VideoFrame};
use ec_h264::{Decoder, NalOutcome};

use super::common::ffmpeg_frames;

/// One decoded frame as planar bytes (16-bit little endian samples for High 10).
fn frame_bytes(frame: &VideoFrame) -> Vec<u8> {
    let bytes_per_sample = match frame.format {
        PixelFormat::I420 => 1,
        PixelFormat::I010 => 2,
        other => panic!("unexpected output format {other:?}"),
    };
    let (w, h) = (frame.width as usize, frame.height as usize);
    let dims = [
        (w, h),
        (w.div_ceil(2), h.div_ceil(2)),
        (w.div_ceil(2), h.div_ceil(2)),
    ];
    let mut out = Vec::new();
    for (plane, (pw, ph)) in frame.planes.iter().zip(dims) {
        for y in 0..ph {
            out.extend_from_slice(plane.row(y, pw * bytes_per_sample).expect("plane row"));
        }
    }
    out
}

fn decode_all(bytes: &[u8]) -> Result<Vec<Vec<u8>>, ec_h264::Error> {
    let mut dec = Decoder::new();
    let mut frames = Vec::new();
    for nal in ec_h264_syntax::AnnexBIter::new(bytes) {
        if dec.push_nal(nal)? == NalOutcome::PictureBoundary {
            dec.end_picture()?;
            dec.push_nal(nal)?;
        }
        while let Some(f) = dec.next_frame() {
            frames.push(frame_bytes(&f));
        }
    }
    dec.flush()?;
    while let Some(f) = dec.next_frame() {
        frames.push(frame_bytes(&f));
    }
    Ok(frames)
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ec-h264-mbaff-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Case {
    tag: &'static str,
    /// lavfi source.
    src: &'static str,
    size: &'static str,
    frames: u32,
    /// 10-bit High 10 instead of High 8-bit.
    ten_bit: bool,
    /// `-x264-params` (after `interlaced=1`).
    params: &'static str,
}

fn encode(dir: &Path, c: &Case) -> Option<PathBuf> {
    let stream = dir.join(format!("{}.264", c.tag));
    let source = format!("{}=size={}:rate=25", c.src, c.size);
    let (pix, profile) = if c.ten_bit {
        ("yuv420p10le", "high10")
    } else {
        ("yuv420p", "high")
    };
    let params = format!("interlaced=1:{}", c.params);
    let st = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(source)
        .args(["-pix_fmt", pix, "-c:v", "libx264", "-profile:v", profile])
        .args(["-flags", "+ildct", "-frames:v"])
        .arg(c.frames.to_string())
        .args(["-x264-params", &params, "-f", "h264"])
        .arg(&stream)
        .status()
        .ok()?;
    st.success().then_some(stream)
}

fn check(c: &Case) -> Result<usize, String> {
    let dir = scratch();
    let Some(stream) = encode(&dir, c) else {
        if std::env::var_os("EC_REQUIRE_FIXTURES").is_some() {
            panic!("{}: ffmpeg/libx264 cannot encode this case", c.tag);
        }
        eprintln!(
            "SKIP {}: ffmpeg with the needed libx264 is not available",
            c.tag
        );
        return Ok(0);
    };
    let bytes = std::fs::read(&stream).map_err(|e| e.to_string())?;
    let ours = decode_all(&bytes).map_err(|e| format!("{}: {e}", c.tag))?;
    let first = ours
        .first()
        .ok_or(format!("{}: no frames decoded", c.tag))?;
    let pix = if c.ten_bit { "yuv420p10le" } else { "yuv420p" };
    let theirs = ffmpeg_frames(&stream, pix, first.len())
        .ok_or(format!("{}: ffmpeg cannot decode", c.tag))?;
    let _ = std::fs::remove_file(&stream);
    if theirs.len() != first.len() * ours.len() {
        return Err(format!(
            "{}: {} frames, ffmpeg {}",
            c.tag,
            ours.len(),
            theirs.len() / first.len()
        ));
    }
    for (i, f) in ours.iter().enumerate() {
        let t = &theirs[i * f.len()..(i + 1) * f.len()];
        if let Some(pos) = f.iter().zip(t).position(|(a, b)| a != b) {
            return Err(format!(
                "{}: frame {i} differs at byte {pos}: ours {} ffmpeg {}",
                c.tag, f[pos], t[pos]
            ));
        }
    }
    Ok(ours.len())
}

fn sweep(cases: &[Case]) {
    let mut failed = Vec::new();
    for c in cases {
        match check(c) {
            Ok(n) => eprintln!("ok   {} ({n} frames)", c.tag),
            Err(e) => failed.push(e),
        }
    }
    let _ = std::fs::remove_dir_all(scratch());
    assert!(
        failed.is_empty(),
        "MBAFF mismatches:\n{}",
        failed.join("\n")
    );
}

const fn case(
    tag: &'static str,
    src: &'static str,
    size: &'static str,
    frames: u32,
    params: &'static str,
) -> Case {
    Case {
        tag,
        src,
        size,
        frames,
        ten_bit: false,
        params,
    }
}

/// Small pictures: every coding tool once, quick enough for every run.
#[test]
fn x264_mbaff_small() {
    sweep(&[
        case("i-cabac-tff", "testsrc2", "192x128", 3, "tff=1:keyint=1"),
        case("i-cabac-bff", "testsrc2", "192x128", 3, "bff=1:keyint=1"),
        case(
            "i-cavlc-tff",
            "testsrc2",
            "192x128",
            3,
            "cabac=0:tff=1:keyint=1",
        ),
        case(
            "i-cavlc-bff",
            "mandelbrot",
            "192x128",
            3,
            "cabac=0:bff=1:keyint=1",
        ),
        case("p-cabac-tff", "testsrc2", "192x128", 8, "tff=1:bframes=0"),
        case(
            "p-cavlc-bff",
            "testsrc2",
            "192x128",
            8,
            "cabac=0:bff=1:bframes=0",
        ),
        case(
            "b-cabac-spatial",
            "testsrc2",
            "192x128",
            10,
            "tff=1:bframes=3:direct=spatial",
        ),
        case(
            "b-cabac-temporal",
            "testsrc2",
            "192x128",
            10,
            "tff=1:bframes=3:direct=temporal",
        ),
        case(
            "b-cavlc-spatial",
            "mandelbrot",
            "192x128",
            10,
            "cabac=0:bff=1:bframes=3:direct=spatial",
        ),
        case(
            "b-cavlc-temporal",
            "mandelbrot",
            "192x128",
            10,
            "cabac=0:bff=1:bframes=3:direct=temporal",
        ),
        case(
            "b-pyramid-ref4",
            "testsrc2",
            "192x128",
            12,
            "tff=1:bframes=3:b-pyramid=normal:ref=4",
        ),
        case(
            "weightp",
            "testsrc2",
            "192x128",
            10,
            "tff=1:bframes=2:weightp=2:weightb=1",
        ),
        case(
            "weightp-cavlc",
            "testsrc2",
            "192x128",
            10,
            "cabac=0:bff=1:bframes=2:weightp=2:weightb=1",
        ),
        case(
            "no8x8dct",
            "testsrc2",
            "192x128",
            8,
            "tff=1:bframes=2:8x8dct=0",
        ),
        case(
            "no8x8dct-cavlc",
            "mandelbrot",
            "192x128",
            8,
            "cabac=0:bff=1:bframes=2:8x8dct=0",
        ),
        case(
            "slices4",
            "testsrc2",
            "192x128",
            8,
            "tff=1:bframes=2:slices=4",
        ),
        case(
            "slices4-cavlc",
            "testsrc2",
            "192x128",
            8,
            "cabac=0:tff=1:bframes=2:slices=3",
        ),
        case(
            "noise-qp12",
            "testsrc2",
            "192x128",
            6,
            "tff=1:bframes=2:qp=12",
        ),
        case(
            "noise-qp40",
            "testsrc2",
            "192x128",
            6,
            "bff=1:bframes=2:qp=40",
        ),
        case(
            "subme9-trellis",
            "mandelbrot",
            "192x128",
            8,
            "tff=1:bframes=3:subme=9:trellis=2:me=umh",
        ),
    ]);
}

/// High 10 MBAFF.
#[test]
fn x264_mbaff_high10() {
    let t = |tag, params| Case {
        tag,
        src: "testsrc2",
        size: "192x128",
        frames: 8,
        ten_bit: true,
        params,
    };
    sweep(&[
        t("h10-i-cabac", "tff=1:keyint=1"),
        t("h10-i-cavlc", "cabac=0:bff=1:keyint=1"),
        t("h10-b-cabac", "tff=1:bframes=3"),
        t("h10-b-cavlc", "cabac=0:bff=1:bframes=3"),
        t("h10-weightp", "tff=1:bframes=2:weightp=2:weightb=1"),
    ]);
}

/// 720x576 (the broadcast interlaced size).
#[test]
fn x264_mbaff_sd() {
    sweep(&[
        case("sd-cabac-tff", "testsrc2", "720x576", 6, "tff=1:bframes=3"),
        case(
            "sd-cabac-bff",
            "mandelbrot",
            "720x576",
            6,
            "bff=1:bframes=3",
        ),
        case(
            "sd-cavlc-tff",
            "testsrc2",
            "720x576",
            6,
            "cabac=0:tff=1:bframes=3",
        ),
        case(
            "sd-cavlc-bff",
            "mandelbrot",
            "720x576",
            6,
            "cabac=0:bff=1:bframes=3",
        ),
        case(
            "sd-weightp",
            "testsrc2",
            "720x576",
            6,
            "tff=1:bframes=2:weightp=2:weightb=1",
        ),
    ]);
}

/// 1920x1080: opt-in (`EC_H264_MBAFF_HD=1`).
#[test]
fn x264_mbaff_hd() {
    if std::env::var_os("EC_H264_MBAFF_HD").is_none() {
        eprintln!("SKIP 1080p MBAFF sweep: set EC_H264_MBAFF_HD=1");
        return;
    }
    sweep(&[
        case(
            "hd-cabac-tff",
            "testsrc2",
            "1920x1080",
            5,
            "tff=1:bframes=3",
        ),
        case(
            "hd-cabac-bff",
            "mandelbrot",
            "1920x1080",
            5,
            "bff=1:bframes=3",
        ),
        case(
            "hd-cavlc-tff",
            "testsrc2",
            "1920x1080",
            5,
            "cabac=0:tff=1:bframes=3",
        ),
        case(
            "hd-cavlc-bff",
            "mandelbrot",
            "1920x1080",
            5,
            "cabac=0:bff=1:bframes=3",
        ),
        case(
            "hd-temporal",
            "testsrc2",
            "1920x1080",
            5,
            "tff=1:bframes=3:direct=temporal",
        ),
        case(
            "hd-weightp",
            "testsrc2",
            "1920x1080",
            5,
            "bff=1:bframes=2:weightp=2:weightb=1",
        ),
    ]);
}
