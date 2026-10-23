//! JVT FRExt High 4:2:2 and High 4:2:2 Intra conformance (the Sony Hi422FR*
//! and Hi422FREXT* streams): frame, field (PAFF) and MBAFF pictures, CAVLC
//! and CABAC, 8 and 10 bits.
//!
//! Where the archive carries a reference YUV (Hi422FR10..15) each frame is
//! compared to it; the other streams ship only a trace, so every stream is
//! also compared frame by frame with ffmpeg's decode (`yuv422p` or
//! `yuv422p10le`). Fixtures live in the gitignored root `fixtures/`
//! (`scripts/fetch-vectors.sh h264-jvt`); a missing set skips with a message,
//! or fails when `EC_REQUIRE_FIXTURES` is set. The two 1920x1088 field
//! streams are slow in a debug build and are skipped there.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::frame::PixelFormat;
use ec_h264::{Decoder, NalOutcome};

use super::common::{ffmpeg_pix_frames, frame_bytes};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vectors/h264-jvt-hi422")
}

/// Every elementary stream (`.264`, `.jsv`) below `dir`, sorted.
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
                .is_some_and(|x| matches!(x.to_ascii_lowercase().as_str(), "264" | "jsv"))
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

fn decode_all(bytes: &[u8]) -> Result<(Vec<Vec<u8>>, PixelFormat), ec_h264::Error> {
    let mut dec = Decoder::new();
    let mut frames = Vec::new();
    let mut format = PixelFormat::I422;
    for nal in ec_h264_syntax::AnnexBIter::new(bytes) {
        if dec.push_nal(nal)? == NalOutcome::PictureBoundary {
            dec.end_picture()?;
            dec.push_nal(nal)?;
        }
        while let Some(f) = dec.next_frame() {
            format = f.format;
            frames.push(frame_bytes(&f));
        }
    }
    dec.flush()?;
    while let Some(f) = dec.next_frame() {
        format = f.format;
        frames.push(frame_bytes(&f));
    }
    Ok((frames, format))
}

fn compare(name: &str, ours: &[Vec<u8>], theirs: &[u8], who: &str) -> Result<(), String> {
    let len = ours[0].len();
    if theirs.len() != len * ours.len() {
        return Err(format!(
            "{name}: {} frames, {who} {}",
            ours.len(),
            theirs.len() as f64 / len as f64
        ));
    }
    for (i, f) in ours.iter().enumerate() {
        let t = &theirs[i * len..(i + 1) * len];
        if let Some(pos) = f.iter().zip(t).position(|(a, b)| a != b) {
            return Err(format!(
                "{name}: frame {i} differs from {who} at byte {pos}: ours {} theirs {}",
                f[pos], t[pos]
            ));
        }
    }
    Ok(())
}

fn have_set() -> Option<Vec<PathBuf>> {
    let list = streams(&root());
    if list.is_empty() {
        assert!(
            std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
            "h264-jvt-hi422 missing at {} - this gate would prove nothing; run \
             scripts/fetch-vectors.sh h264-jvt",
            root().display()
        );
        eprintln!("SKIP h264-jvt-hi422 missing at {}", root().display());
        return None;
    }
    Some(list)
}

#[test]
fn jvt_hi422_vectors_bit_exact() {
    let Some(list) = have_set() else { return };
    if std::env::var_os("EC_REQUIRE_FIXTURES").is_some() {
        assert!(list.len() >= 18, "only {} Hi422 streams found", list.len());
    }
    let have_ffmpeg = Command::new("ffmpeg").arg("-version").output().is_ok();
    let mut failed = Vec::new();
    let mut exact = Vec::new();
    for path in &list {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        if cfg!(debug_assertions)
            && matches!(name.as_str(), "Hi422FREXT16_SONY_A" | "Hi422FREXT17_SONY_A")
        {
            eprintln!("SKIP {name}: 1920x1088 field stream, release builds only");
            continue;
        }
        let bytes = std::fs::read(path).unwrap();
        let (ours, format) = match decode_all(&bytes) {
            Ok(v) => v,
            Err(e) => {
                failed.push(format!("{name}: {e}"));
                continue;
            }
        };
        if ours.is_empty() {
            failed.push(format!("{name}: no frames decoded"));
            continue;
        }
        let mut ok = true;
        // The archive's own reference YUV, when it ships one.
        if let Some(yuv) = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|x| x == "yuv"))
        {
            let theirs = std::fs::read(yuv).unwrap();
            if let Err(e) = compare(&name, &ours, &theirs, "reference yuv") {
                failed.push(e);
                ok = false;
            }
        }
        if have_ffmpeg {
            let pix = if format == PixelFormat::I210 {
                "yuv422p10le"
            } else {
                "yuv422p"
            };
            match ffmpeg_pix_frames(path, pix) {
                Some(t) => {
                    if let Err(e) = compare(&name, &ours, &t, "ffmpeg") {
                        failed.push(e);
                        ok = false;
                    }
                }
                None => {
                    assert!(
                        std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
                        "{name}: ffmpeg cannot decode"
                    );
                }
            }
        }
        if ok {
            exact.push(format!("{name} ({} frames, {format:?})", ours.len()));
        }
    }
    eprintln!("exact: {}", exact.join(", "));
    assert!(
        failed.is_empty(),
        "High 4:2:2 vectors that do not match:\n{}",
        failed.join("\n")
    );
}
