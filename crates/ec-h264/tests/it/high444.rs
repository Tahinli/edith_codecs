//! High 4:4:4 Predictive, lossless (transform bypass) and monochrome decode,
//! byte-exact against ffmpeg, on streams libx264 writes from synthetic
//! sources: intra-only and long-GOP with B-frames, CABAC and CAVLC, the 8x8
//! transform, weighted prediction, scaling matrices, chroma QP offsets,
//! several slices, cropping, progressive and MBAFF, at 8 and 10 bits.
//!
//! Output formats: 4:4:4 -> I444 / I410, 4:2:0 and 4:2:2 lossless -> I420 /
//! I422 (and 10-bit), monochrome -> I420 / I010 with neutral chroma (compared
//! on the luma plane against ffmpeg's `gray` / `gray10le`; chroma must be 128,
//! or 512 at 10 bits). Separate colour planes cannot be written by x264; the
//! JVT vectors (`high444_jvt.rs`) cover them.
//!
//! Every case is generated in the test (no committed binaries). A case skips,
//! saying so, when ffmpeg or the needed libx264 is absent, and fails instead
//! when `EC_REQUIRE_FIXTURES` is set. The 1280x720 cases are opt-in:
//! set `EC_H264_HI444_HD=1`.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::frame::{PixelFormat, VideoFrame};
use ec_h264::{Decoder, NalOutcome};

use super::common::{ffmpeg_pix_frames, layout};

/// One decoded frame as planar bytes (16-bit little endian above 8 bits).
fn frame_bytes(frame: &VideoFrame) -> (Vec<u8>, PixelFormat) {
    let (bps, dx, dy) = layout(frame.format);
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (cw, ch) = (w.div_ceil(dx), h.div_ceil(dy));
    let mut out = Vec::new();
    for (plane, (pw, ph)) in frame.planes.iter().zip([(w, h), (cw, ch), (cw, ch)]) {
        for y in 0..ph {
            out.extend_from_slice(plane.row(y, pw * bps).expect("plane row"));
        }
    }
    (out, frame.format)
}

fn decode_all(bytes: &[u8]) -> Result<Vec<(Vec<u8>, PixelFormat)>, ec_h264::Error> {
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
        "ec-h264-hi444-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[derive(Clone, Copy)]
struct Case {
    tag: &'static str,
    src: &'static str,
    size: &'static str,
    frames: u32,
    /// ffmpeg pixel format given to libx264 and used for the reference decode.
    pix: &'static str,
    mbaff: bool,
    params: &'static str,
}

fn encode(dir: &Path, c: &Case) -> Option<PathBuf> {
    let stream = dir.join(format!("{}.264", c.tag));
    let (w, h) = c.size.split_once('x')?;
    // Sources are generated at an even size and cropped, so odd sizes work.
    let source = format!(
        "{}=size={}x{}:rate=25",
        c.src,
        (w.parse::<u32>().ok()? + 1) & !1,
        (h.parse::<u32>().ok()? + 1) & !1
    );
    let params = if c.mbaff {
        format!("interlaced=1:{}", c.params)
    } else {
        c.params.to_string()
    };
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
        .args(["-vf", &format!("crop={w}:{h}:0:0")])
        .args(["-pix_fmt", c.pix, "-c:v", "libx264"])
        .args([
            "-flags",
            if c.mbaff { "+ildct" } else { "+loop" },
            "-frames:v",
        ])
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
    let (first, format) = ours
        .first()
        .ok_or(format!("{}: no frames decoded", c.tag))?;
    let mono = c.pix.starts_with("gray");
    let theirs =
        ffmpeg_pix_frames(&stream, c.pix).ok_or(format!("{}: ffmpeg cannot decode", c.tag))?;
    let _ = std::fs::remove_file(&stream);

    let expect_format = match c.pix {
        "yuv444p" => PixelFormat::I444,
        "yuv444p10le" => PixelFormat::I410,
        "yuv422p" => PixelFormat::I422,
        "yuv422p10le" => PixelFormat::I210,
        "yuv420p" | "gray" => PixelFormat::I420,
        "yuv420p10le" | "gray10le" => PixelFormat::I010,
        other => panic!("unknown pix {other}"),
    };
    if *format != expect_format {
        return Err(format!(
            "{}: output {format:?}, expected {expect_format:?}",
            c.tag
        ));
    }
    let bps = layout(*format).0;
    // For monochrome only the luma plane exists in the reference.
    let cmp_len = if mono {
        theirs.len() / ours.len()
    } else {
        first.len()
    };
    if theirs.len() != cmp_len * ours.len() {
        return Err(format!(
            "{}: {} frames, ffmpeg {}",
            c.tag,
            ours.len(),
            theirs.len() as f64 / cmp_len as f64
        ));
    }
    for (i, (f, _)) in ours.iter().enumerate() {
        let t = &theirs[i * cmp_len..(i + 1) * cmp_len];
        if let Some(pos) = f[..cmp_len].iter().zip(t).position(|(a, b)| a != b) {
            return Err(format!(
                "{}: frame {i} differs at byte {pos}: ours {} ffmpeg {}",
                c.tag, f[pos], t[pos]
            ));
        }
        if mono {
            let neutral: &[u8] = if bps == 1 { &[128] } else { &[0x00, 0x02] };
            if let Some(pos) = f[cmp_len..].chunks(bps).position(|s| s != neutral) {
                return Err(format!(
                    "{}: frame {i} chroma not neutral at sample {pos}",
                    c.tag
                ));
            }
        }
    }
    Ok(ours.len())
}

fn sweep(label: &str, cases: &[Case]) {
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
        "{label} mismatches:\n{}",
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
        pix: "yuv444p",
        mbaff: false,
        params,
    }
}

fn with(c: &Case, pix: &'static str, prefix: &str, mbaff: bool) -> Case {
    let suffix = pix.trim_start_matches("yuv").trim_start_matches("444p");
    Case {
        tag: Box::leak(format!("{prefix}{}-{pix}{suffix}", c.tag).into_boxed_str()),
        pix,
        mbaff,
        ..*c
    }
}

const SMALL: &[Case] = &[
    case("i-cabac", "testsrc2", "192x128", 3, "keyint=1"),
    case("i-cavlc", "testsrc2", "192x128", 3, "keyint=1:cabac=0"),
    case(
        "i-cabac-no8x8",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:8x8dct=0",
    ),
    case(
        "i-cavlc-no8x8",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:cabac=0:8x8dct=0",
    ),
    case(
        "i-cabac-mandel",
        "mandelbrot",
        "176x144",
        3,
        "keyint=1:qp=18",
    ),
    case(
        "i-cavlc-mandel",
        "mandelbrot",
        "176x144",
        3,
        "keyint=1:cabac=0:qp=18",
    ),
    case("i-cabac-qp40", "testsrc2", "192x128", 3, "keyint=1:qp=40"),
    case(
        "i-cavlc-qp8",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:cabac=0:qp=8",
    ),
    case(
        "i-cabac-noise",
        "mandelbrot",
        "176x144",
        2,
        "keyint=1:qp=10:subme=9:trellis=2",
    ),
    case("i-cabac-cqm", "testsrc2", "192x128", 3, "keyint=1:cqm=jvt"),
    case(
        "i-cavlc-cqm",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:cabac=0:cqm=jvt",
    ),
    case(
        "i-cabac-cqo-neg",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:chroma-qp-offset=-5",
    ),
    case(
        "i-cavlc-cqo-pos",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:cabac=0:chroma-qp-offset=6",
    ),
    case(
        "i-cabac-slices",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:slices=4",
    ),
    case(
        "i-cabac-nodeblock",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:no-deblock=1",
    ),
    case(
        "i-cabac-cip",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:constrained-intra=1",
    ),
    case("i-cabac-crop", "testsrc2", "162x98", 3, "keyint=1"),
    case("i-cavlc-crop", "testsrc2", "162x98", 3, "keyint=1:cabac=0"),
    case("p-cabac", "testsrc2", "192x128", 8, "keyint=30:bframes=0"),
    case(
        "p-cavlc",
        "testsrc2",
        "192x128",
        8,
        "keyint=30:bframes=0:cabac=0",
    ),
    case(
        "p-cabac-no8x8",
        "testsrc2",
        "192x128",
        8,
        "keyint=30:bframes=0:8x8dct=0",
    ),
    case("b-cabac", "testsrc2", "192x128", 10, "keyint=30:bframes=3"),
    case(
        "b-cavlc",
        "testsrc2",
        "192x128",
        10,
        "keyint=30:bframes=3:cabac=0",
    ),
    case(
        "b-cavlc-no8x8",
        "testsrc2",
        "192x128",
        10,
        "keyint=30:bframes=3:cabac=0:8x8dct=0",
    ),
    case(
        "b-cabac-pyramid",
        "mandelbrot",
        "192x128",
        12,
        "keyint=30:bframes=3:b-pyramid=normal:ref=4",
    ),
    case(
        "b-cabac-temporal",
        "testsrc2",
        "192x128",
        10,
        "keyint=30:bframes=3:direct=temporal",
    ),
    case(
        "b-cabac-weight",
        "testsrc2",
        "192x128",
        10,
        "keyint=30:bframes=2:weightp=2:weightb=1",
    ),
    case(
        "b-cavlc-weight",
        "testsrc2",
        "192x128",
        10,
        "keyint=30:bframes=2:weightp=2:weightb=1:cabac=0",
    ),
    case(
        "b-cabac-cqm",
        "testsrc2",
        "192x128",
        8,
        "keyint=30:bframes=2:cqm=jvt",
    ),
    case(
        "b-cabac-slices",
        "testsrc2",
        "192x128",
        8,
        "keyint=30:bframes=2:slices=4",
    ),
    case(
        "b-cabac-qp40",
        "testsrc2",
        "192x128",
        8,
        "keyint=30:bframes=2:qp=40",
    ),
    case(
        "b-cabac-subme9",
        "testsrc2",
        "192x128",
        8,
        "keyint=30:bframes=2:subme=9:trellis=2",
    ),
    case(
        "b-cabac-noloop",
        "testsrc2",
        "192x128",
        8,
        "keyint=30:bframes=2:no-deblock=1",
    ),
    case(
        "b-cabac-crop",
        "testsrc2",
        "162x98",
        8,
        "keyint=30:bframes=2",
    ),
    case(
        "b-cavlc-fakeint",
        "testsrc2",
        "192x128",
        8,
        "keyint=30:bframes=2:cabac=0:fake-interlaced=1",
    ),
];

fn all(pix: &'static str, prefix: &str, mbaff: bool, step: usize) -> Vec<Case> {
    SMALL
        .iter()
        .enumerate()
        .filter(|(i, _)| i % step == 0)
        .map(|(_, c)| with(c, pix, prefix, mbaff))
        .collect()
}

#[test]
fn x264_hi444_8bit() {
    sweep("4:4:4 8-bit", &all("yuv444p", "", false, 1));
}

#[test]
fn x264_hi444_mbaff() {
    let mut cases = all("yuv444p", "mbaff-", true, 2);
    cases.extend(all("yuv444p10le", "mbaff-", true, 4));
    sweep("4:4:4 MBAFF", &cases);
}

// ---- lossless (qpprime_y_zero_transform_bypass) ----

const LOSSLESS: &[Case] = &[
    case("ll-i-cabac", "testsrc2", "192x128", 3, "keyint=1:qp=0"),
    case(
        "ll-i-cavlc",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:qp=0:cabac=0",
    ),
    case(
        "ll-i-cabac-no8x8",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:qp=0:8x8dct=0",
    ),
    case(
        "ll-i-cavlc-no8x8",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:qp=0:cabac=0:8x8dct=0",
    ),
    case("ll-i-noise", "mandelbrot", "176x144", 3, "keyint=1:qp=0"),
    case(
        "ll-i-noise-cavlc",
        "mandelbrot",
        "176x144",
        3,
        "keyint=1:qp=0:cabac=0",
    ),
    case("ll-i-crop", "testsrc2", "162x98", 3, "keyint=1:qp=0"),
    case(
        "ll-i-slices",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:qp=0:slices=4",
    ),
    case(
        "ll-p-cabac",
        "testsrc2",
        "192x128",
        8,
        "keyint=30:bframes=0:qp=0",
    ),
    case(
        "ll-p-cavlc",
        "testsrc2",
        "192x128",
        8,
        "keyint=30:bframes=0:qp=0:cabac=0",
    ),
    case(
        "ll-b-cabac",
        "mandelbrot",
        "192x128",
        8,
        "keyint=30:bframes=3:qp=0",
    ),
    case(
        "ll-b-cavlc",
        "mandelbrot",
        "192x128",
        8,
        "keyint=30:bframes=3:qp=0:cabac=0",
    ),
    case(
        "ll-b-cabac-no8x8",
        "testsrc2",
        "192x128",
        8,
        "keyint=30:bframes=2:qp=0:8x8dct=0",
    ),
    case(
        "ll-b-weight",
        "testsrc2",
        "192x128",
        8,
        "keyint=30:bframes=2:qp=0:weightp=2:weightb=1",
    ),
    case(
        "ll-b-cabac-noloop",
        "testsrc2",
        "192x128",
        8,
        "keyint=30:bframes=2:qp=0:no-deblock=1",
    ),
];

fn lossless(pix: &'static str) -> Vec<Case> {
    LOSSLESS.iter().map(|c| with(c, pix, "", false)).collect()
}

#[test]
fn x264_lossless_444_8bit() {
    sweep("lossless 4:4:4", &lossless("yuv444p"));
}

#[test]
fn x264_lossless_444_10bit() {
    sweep("lossless 4:4:4 10-bit", &lossless("yuv444p10le"));
}

/// Profile 244 lossless also carries 4:2:0 and 4:2:2.
#[test]
fn x264_lossless_420_422() {
    let mut cases: Vec<Case> = Vec::new();
    for (i, c) in LOSSLESS.iter().enumerate() {
        if i % 2 == 0 {
            cases.push(with(c, "yuv420p", "", false));
            cases.push(with(c, "yuv422p", "", false));
        }
        if i % 5 == 0 {
            cases.push(with(c, "yuv420p10le", "", false));
            cases.push(with(c, "yuv422p10le", "", false));
        }
    }
    sweep("lossless 4:2:0 / 4:2:2", &cases);
}

#[test]
fn x264_lossless_mbaff() {
    let cases: Vec<Case> = LOSSLESS
        .iter()
        .enumerate()
        .filter(|(i, _)| i % 4 == 0)
        .map(|(_, c)| with(c, "yuv444p", "mbaff-", true))
        .collect();
    sweep("lossless MBAFF", &cases);
}

// ---- monochrome ----

#[test]
fn x264_monochrome_8bit() {
    sweep("gray", &all("gray", "", false, 1));
}

#[test]
fn x264_monochrome_10bit() {
    sweep("gray10le", &all("gray10le", "", false, 2));
}

#[test]
fn x264_monochrome_mbaff_and_lossless() {
    let mut cases = all("gray", "mbaff-", true, 6);
    cases.extend(
        LOSSLESS
            .iter()
            .enumerate()
            .filter(|(i, _)| i % 3 == 0)
            .map(|(_, c)| with(c, "gray", "", false)),
    );
    sweep("gray MBAFF / lossless", &cases);
}

/// 1280x720 (opt-in, slow).
#[test]
fn x264_hi444_hd() {
    if std::env::var_os("EC_H264_HI444_HD").is_none() {
        eprintln!("SKIP x264_hi444_hd: set EC_H264_HI444_HD=1");
        return;
    }
    let base = [
        case("hd-i-cabac", "testsrc2", "1280x720", 2, "keyint=1"),
        case(
            "hd-b-cabac",
            "testsrc2",
            "1280x720",
            5,
            "keyint=30:bframes=2",
        ),
        case(
            "hd-b-cavlc",
            "testsrc2",
            "1280x720",
            5,
            "keyint=30:bframes=2:cabac=0",
        ),
        case("hd-ll-i", "testsrc2", "1280x720", 2, "keyint=1:qp=0"),
    ];
    let mut cases = Vec::new();
    for c in &base {
        cases.push(with(c, "yuv444p", "", false));
        cases.push(with(c, "yuv444p10le", "", false));
    }
    cases.push(with(&base[0], "gray", "", false));
    cases.push(with(&base[0], "yuv444p", "mbaff-", true));
    sweep("4:4:4 HD", &cases);
}
