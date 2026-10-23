//! JVT professional-profile conformance: High 4:4:4 Predictive, CAVLC 4:4:4
//! Intra and High 4:4:4 Intra streams (PPCV444I*, PPH444I*, PPH444P*), which
//! between them cover 4:4:4 at 8 and 10 bits, separate colour planes,
//! lossless transform bypass and monochrome.
//!
//! Each stream is decoded and compared frame by frame with ffmpeg's decode
//! (pixel format taken from ffprobe). When ffmpeg cannot decode a stream, or
//! the archive ships a reference YUV, that YUV is used instead of / as well as
//! ffmpeg; a stream with neither is reported `unchecked` (and counted as a
//! failure only if it also fails to decode). Fixtures live in the gitignored
//! root `fixtures/vectors/h264-jvt-hi444`; a missing set skips with a message,
//! or fails when `EC_REQUIRE_FIXTURES` is set. Heavy streams are skipped in
//! debug builds.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ec_core::frame::{PixelFormat, VideoFrame};
use ec_h264::{Decoder, NalOutcome};

use super::common::layout;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vectors/h264-jvt-hi444")
}

const STREAM_EXT: &[&str] = &["264", "jsv", "avc", "26l", "h264", "bit", "bits", "jvt"];

/// Every elementary stream below `dir`, sorted.
fn streams(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| STREAM_EXT.contains(&x.to_ascii_lowercase().as_str()))
            {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

/// FNV-1a, enough to tell frames apart without keeping gigabytes of them.
fn fnv(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
    }
    h
}

/// What is kept of one decoded frame.
struct Ours {
    /// Whole frame (planar bytes) and its luma plane only: sizes and hashes.
    full: usize,
    luma: usize,
    hash_full: u64,
    hash_luma: u64,
    /// Every chroma sample is the neutral value (monochrome output).
    neutral: bool,
}

fn summarize(frame: &VideoFrame) -> Ours {
    let (bps, dx, dy) = layout(frame.format);
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (cw, ch) = (w.div_ceil(dx), h.div_ceil(dy));
    let mut out = Vec::new();
    let mut luma = 0;
    for (i, (plane, (pw, ph))) in frame
        .planes
        .iter()
        .zip([(w, h), (cw, ch), (cw, ch)])
        .enumerate()
    {
        for y in 0..ph {
            out.extend_from_slice(plane.row(y, pw * bps).expect("plane row"));
        }
        if i == 0 {
            luma = out.len();
        }
    }
    let neutral_sample: &[u8] = if bps == 1 { &[128] } else { &[0x00, 0x02] };
    Ours {
        full: out.len(),
        luma,
        hash_full: fnv(&out),
        hash_luma: fnv(&out[..luma]),
        neutral: out[luma..].chunks(bps).all(|s| s == neutral_sample),
    }
}

type Decoded = (Vec<Ours>, PixelFormat);

fn decode_all(bytes: &[u8]) -> Result<Decoded, ec_h264::Error> {
    let mut dec = Decoder::new();
    dec.set_wide_samples(true);
    let mut frames = Vec::new();
    let mut format = PixelFormat::I444;
    for nal in ec_h264_syntax::AnnexBIter::new(bytes) {
        if dec.push_nal(nal)? == NalOutcome::PictureBoundary {
            dec.end_picture()?;
            dec.push_nal(nal)?;
        }
        while let Some(f) = dec.next_frame() {
            format = f.format;
            frames.push(summarize(&f));
        }
    }
    dec.flush()?;
    while let Some(f) = dec.next_frame() {
        format = f.format;
        frames.push(summarize(&f));
    }
    Ok((frames, format))
}

fn probe_pix_fmt(stream: &Path) -> Option<String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-f", "h264", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=pix_fmt", "-of", "csv=p=0"])
        .arg(stream)
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

/// ffmpeg's raw decode as a pipe, so a long 14-bit 4:4:4 stream is compared a
/// frame at a time instead of held in memory.
fn ffmpeg_pipe(stream: &Path, pix_fmt: &str) -> Option<std::process::Child> {
    Command::new("ffmpeg")
        .args(["-v", "error", "-f", "h264", "-i"])
        .arg(stream)
        .args(["-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
        .stdout(Stdio::piped())
        .spawn()
        .ok()
}

/// Compare `ours` with a raw reference read from `theirs`: full frames, or
/// luma only (monochrome; our chroma must then be neutral).
fn compare(
    name: &str,
    ours: &[Ours],
    theirs: &mut dyn Read,
    luma_only: bool,
    who: &str,
) -> Result<(), String> {
    let mut buf = Vec::new();
    for (i, f) in ours.iter().enumerate() {
        let n = if luma_only { f.luma } else { f.full };
        buf.resize(n, 0);
        theirs.read_exact(&mut buf).map_err(|_| {
            format!(
                "{name}: {who} ends before frame {i} ({} frames ours)",
                ours.len()
            )
        })?;
        let want = if luma_only { f.hash_luma } else { f.hash_full };
        if fnv(&buf) != want {
            return Err(format!("{name}: frame {i} differs from {who}"));
        }
        if luma_only && !f.neutral {
            return Err(format!("{name}: frame {i} chroma is not neutral"));
        }
    }
    let mut extra = [0u8; 1];
    if theirs.read(&mut extra).is_ok_and(|k| k > 0) {
        return Err(format!("{name}: {who} has more than {} frames", ours.len()));
    }
    Ok(())
}

/// The reference YUV shipped next to a stream, if there is a clear match.
fn reference_yuv(stream: &Path) -> Option<PathBuf> {
    let dir = stream.parent()?;
    let yuvs: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("yuv")))
        .collect();
    let stem = stream.file_stem()?.to_string_lossy().to_ascii_lowercase();
    yuvs.iter()
        .find(|p| {
            p.file_stem()
                .is_some_and(|s| s.to_string_lossy().to_ascii_lowercase() == stem)
        })
        .or_else(|| (yuvs.len() == 1).then(|| &yuvs[0]))
        .cloned()
}

#[test]
fn jvt_hi444_vectors_bit_exact() {
    let list = streams(&root());
    if list.is_empty() {
        assert!(
            std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
            "h264-jvt-hi444 missing at {} - this gate would prove nothing; run \
             scripts/fetch-vectors.sh h264-jvt-hi444",
            root().display()
        );
        eprintln!("SKIP h264-jvt-hi444 missing at {}", root().display());
        return;
    }
    let have_ffmpeg = Command::new("ffmpeg").arg("-version").output().is_ok();
    let mut failed = Vec::new();
    let mut summary = Vec::new();
    for path in &list {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        if cfg!(debug_assertions) && std::fs::metadata(path).map_or(0, |m| m.len()) > 2_000_000 {
            eprintln!("SKIP {name}: large stream, release builds only");
            continue;
        }
        let bytes = std::fs::read(path).unwrap();
        let (ours, format) = match decode_all(&bytes) {
            Ok((f, fmt)) if !f.is_empty() => (f, fmt),
            Ok(_) => {
                failed.push(format!("{name}: no frames decoded"));
                eprintln!("FAIL {name}: no frames decoded");
                continue;
            }
            Err(e) if e.to_string().contains("FMO") => {
                // The Sejong Univ. streams use slice groups, which this
                // decoder refuses by name (not part of this lane).
                eprintln!("SKIP {name}: {e}");
                summary.push(format!("{name} fmo-refused"));
                continue;
            }
            Err(e) => {
                failed.push(format!("{name}: {e}"));
                eprintln!("FAIL {name}: {e}");
                continue;
            }
        };
        let mut checked = Vec::new();
        let mut errors = Vec::new();
        if let Some(yuv) = reference_yuv(path) {
            let theirs = std::fs::read(&yuv).unwrap();
            let full: usize = ours.iter().map(|f| f.full).sum();
            let luma_only = theirs.len() != full;
            match compare(
                &name,
                &ours,
                &mut theirs.as_slice(),
                luma_only,
                "reference yuv",
            ) {
                Ok(()) => checked.push("yuv"),
                Err(e) => errors.push(e),
            }
        }
        if have_ffmpeg {
            // 9-bit streams are left-aligned to 10 bits by us.
            let pix = match probe_pix_fmt(path).as_deref() {
                Some("gray") => "gray".to_string(),
                Some("gray10le" | "gray12le" | "gray9le") => "gray10le".to_string(),
                Some(p) if p.contains("p9") => p.replace("p9", "p10"),
                Some(p) => p.to_string(),
                None => match format {
                    PixelFormat::I410 => "yuv444p10le".into(),
                    PixelFormat::I444 => "yuv444p".into(),
                    _ => String::new(),
                },
            };
            let luma_only = pix.starts_with("gray");
            match (!pix.is_empty() && pix != "unknown")
                .then(|| ffmpeg_pipe(path, &pix))
                .flatten()
            {
                Some(mut child) => {
                    let res = compare(
                        &name,
                        &ours,
                        child.stdout.as_mut().unwrap(),
                        luma_only,
                        "ffmpeg",
                    );
                    let _ = child.kill();
                    let _ = child.wait();
                    match res {
                        Ok(()) => checked.push("ffmpeg"),
                        Err(e) => errors.push(e),
                    }
                }
                None => eprintln!("note {name}: ffmpeg cannot decode (pix_fmt {pix:?})"),
            }
        }
        if !errors.is_empty() {
            eprintln!("FAIL {name}: {}", errors.join("; "));
            failed.extend(errors);
        } else if checked.is_empty() {
            eprintln!(
                "PASS? {name}: decoded {} frames ({format:?}), UNCHECKED (no ffmpeg decode, no reference yuv)",
                ours.len()
            );
            summary.push(format!("{name} unchecked"));
        } else {
            eprintln!(
                "PASS {name}: {} frames {format:?} vs {}",
                ours.len(),
                checked.join("+")
            );
            summary.push(format!("{name} ok"));
        }
    }
    eprintln!("summary: {}", summary.join(", "));
    assert!(
        failed.is_empty(),
        "High 4:4:4 vectors that do not match:\n{}",
        failed.join("\n")
    );
}
