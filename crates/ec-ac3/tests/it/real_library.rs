//! AC-3 and E-AC-3 decode of the real files named in `real_library.toml`,
//! compared against ffmpeg.
//!
//! Local media only: the file list is the user's `real_library.toml` (path in
//! `EC_REAL_LIBRARY`, default `~/Documents/Code/Rust/edith/real_library.toml`).
//! Each entry whose first audio track is `ac3` or `eac3` has its first
//! `EC_REAL_SECONDS` seconds (default 20) of that track copied out as a raw
//! elementary stream (`-c copy`, no re-encode), decoded by `ec-ac3` and by
//! ffmpeg, and compared per channel. Entries that are missing on disk, or that
//! hold no AC-3 or E-AC-3 track, are skipped loudly. Without ffmpeg the whole
//! test skips.
//!
//! Gate: per-channel correlation >= 0.999 (the matrix floor), and the
//! exact-sample share and RMS delta are reported for each file.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_ac3::{Ac3Decoder, Options};
use crate::common::have_ffmpeg;

const MIN_CORR: f64 = 0.999;
/// Reference RMS below which a channel counts as silent (-80 dBFS).
const SILENT_RMS: f64 = 1e-4;
/// RMS delta ceiling on a silent channel: 1/30 of a 16-bit LSB.
const SILENT_DELTA: f64 = 1e-6;

fn library_toml() -> PathBuf {
    if let Ok(p) = std::env::var("EC_REAL_LIBRARY") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("Documents/Code/Rust/edith/real_library.toml")
}

/// `key = "path"` lines of the TOML file, as (key, path). Only the flat form
/// the library file uses; comments and other lines are ignored.
fn entries(toml: &Path) -> Vec<(String, PathBuf)> {
    let Ok(text) = std::fs::read_to_string(toml) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            let value = value.trim().strip_prefix('"')?.strip_suffix('"')?;
            Some((key.trim().to_string(), PathBuf::from(value)))
        })
        .collect()
}

/// The codec and channel count of the file's first audio track.
fn first_audio(path: &Path) -> Option<(String, usize)> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_name,channels",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.trim().split(',');
    Some((parts.next()?.to_string(), parts.next()?.parse().ok()?))
}

/// Copies the first `seconds` of the first audio track to `out` as a raw
/// elementary stream of the same codec. Returns false when that fails.
fn extract(path: &Path, codec: &str, seconds: u32, out: &Path) -> bool {
    let muxer = if codec == "eac3" { "eac3" } else { "ac3" };
    Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-y", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-t", &seconds.to_string(), "-c", "copy", "-f", muxer])
        .arg(out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// ffmpeg's decode of the raw stream as interleaved `f32`.
fn reference(path: &Path) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-"])
        .output()
        .expect("ffmpeg");
    out.stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

/// Our decode of the raw stream (dither off, as in the matrix test) and the
/// channel count.
fn decode(path: &Path) -> (Vec<f32>, usize) {
    let data = std::fs::read(path).expect("extracted stream");
    let options = Options {
        dither: false,
        ..Options::default()
    };
    let mut decoder = Ac3Decoder::with_options(options);
    let (mut out, mut channels, mut pos) = (Vec::new(), 0, 0usize);
    while pos + 6 <= data.len() {
        if data[pos] != 0x0B || data[pos + 1] != 0x77 {
            pos += 1;
            continue;
        }
        let Ok(size) = ec_ac3::frame_size(&data[pos..]) else {
            break;
        };
        if pos + size > data.len() {
            break;
        }
        let frame = decoder
            .decode_frame(&data[pos..pos + size])
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        channels = frame.channels();
        out.extend(
            frame
                .data[0]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_le_bytes(*c)),
        );
        pos += size;
    }
    (out, channels)
}

/// (correlation, RMS delta, exact-sample count, compared-sample count).
fn compare(a: &[f32], b: &[f32]) -> (f64, f64, usize, usize) {
    let n = a.len().min(b.len());
    let (mut sa, mut sb, mut saa, mut sbb, mut sab, mut sd) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    let mut exact = 0;
    for i in 0..n {
        let (x, y) = (f64::from(a[i]), f64::from(b[i]));
        if a[i] == b[i] {
            exact += 1;
        }
        sa += x;
        sb += y;
        saa += x * x;
        sbb += y * y;
        sab += x * y;
        sd += (x - y) * (x - y);
    }
    let nf = n.max(1) as f64;
    let cov = sab / nf - (sa / nf) * (sb / nf);
    let va = saa / nf - (sa / nf).powi(2);
    let vb = sbb / nf - (sb / nf).powi(2);
    let corr = if va <= 0.0 || vb <= 0.0 {
        1.0
    } else {
        cov / (va * vb).sqrt()
    };
    (corr, (sd / nf).sqrt(), exact, n)
}

#[test]
fn real_library_ac3_and_eac3_match_ffmpeg() {
    if !have_ffmpeg() {
        eprintln!("skip: ffmpeg not on PATH");
        return;
    }
    let toml = library_toml();
    let seconds: u32 = std::env::var("EC_REAL_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("real-library-ac3");
    std::fs::create_dir_all(&work).expect("work dir");

    let mut checked = 0;
    for (key, path) in entries(&toml) {
        if !path.exists() {
            eprintln!("skip {key}: {} not on disk", path.display());
            continue;
        }
        let Some((codec, channels)) = first_audio(&path) else {
            eprintln!("skip {key}: ffprobe found no audio track");
            continue;
        };
        if codec != "ac3" && codec != "eac3" {
            eprintln!("skip {key}: first audio track is {codec}, not AC-3 or E-AC-3");
            continue;
        }
        let raw = work.join(format!("{key}.{codec}"));
        assert!(extract(&path, &codec, seconds, &raw), "{key}: extract failed");

        let (ours, decoded_channels) = decode(&raw);
        let theirs = reference(&raw);
        assert_eq!(decoded_channels, channels, "{key}: channel count");
        assert_eq!(ours.len(), theirs.len(), "{key}: sample count vs ffmpeg");
        assert!(!ours.is_empty(), "{key}: nothing decoded");

        let mut worst = 1.0f64;
        let mut worst_rms = 0.0f64;
        let mut exact_total = 0;
        let mut total = 0;
        for ch in 0..decoded_channels {
            let a: Vec<f32> = ours[ch..].iter().step_by(decoded_channels).copied().collect();
            let b: Vec<f32> = theirs[ch..].iter().step_by(decoded_channels).copied().collect();
            let (corr, rms, exact, n) = compare(&a, &b);
            eprintln!("{key} {codec} ch{ch}: corr {corr:.6} rms {rms:.3e} exact {exact}/{n}");
            // A channel whose reference is numerically silent (an LFE that
            // carries nothing but rounding) has no meaningful correlation, so
            // it is gated on RMS against one 16-bit LSB / 30 instead.
            let ref_rms = (b.iter().map(|y| f64::from(*y).powi(2)).sum::<f64>()
                / b.len().max(1) as f64)
                .sqrt();
            if ref_rms < SILENT_RMS {
                assert!(rms <= SILENT_DELTA, "{key} ch{ch}: silent-channel RMS delta {rms:.3e}");
            } else {
                assert!(corr >= MIN_CORR, "{key} ch{ch}: correlation {corr:.6}");
            }
            worst = worst.min(corr);
            worst_rms = worst_rms.max(rms);
            exact_total += exact;
            total += n;
        }
        eprintln!(
            "RESULT {key} {codec} {decoded_channels}ch {seconds}s: min corr {worst:.6}, max rms {worst_rms:.3e}, exact {:.4}%",
            100.0 * exact_total as f64 / total.max(1) as f64
        );
        checked += 1;
    }
    eprintln!("{checked} real-library AC-3/E-AC-3 entries checked");
}
