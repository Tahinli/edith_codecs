//! Per-channel parity with ffmpeg on dense, independent channels.
//!
//! The fixtures are generated here with ffmpeg, never committed: six
//! independent pink-noise channels (one per 5.1 speaker, or two for 2.0) are
//! encoded by ffmpeg's AC-3 encoder at a given bitrate, then decoded both by
//! ffmpeg and by `ec-ac3` with default options. Noise at these bitrates leaves
//! many zero-bit mantissas, so this exercises the dither for them as well as
//! the coupled and rematrixed paths.
//!
//! Gate: per-channel correlation >= 0.999 for every layout and bitrate below,
//! with the LFE (which carries only rounding-level content here) also held to
//! the same correlation since it is not silent.
//!
//! Needs ffmpeg on PATH; without it the test skips loudly.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_ac3::{Ac3Decoder, Options};
use crate::common::have_ffmpeg;

const MIN_CORR: f64 = 0.999;
/// Seconds of noise per fixture. Long enough for a few hundred frames.
const SECONDS: u32 = 10;

/// Encodes `channels` independent pink-noise sources as one AC-3 stream at
/// `bitrate` (e.g. "448k"), and returns the path of the `.ac3` file.
fn encode(work: &Path, name: &str, channels: usize, bitrate: &str) -> PathBuf {
    let out = work.join(format!("{name}.ac3"));
    let mut args: Vec<String> = vec!["-nostdin".into(), "-v".into(), "error".into(), "-y".into()];
    for seed in 1..=channels {
        args.extend([
            "-f".into(),
            "lavfi".into(),
            "-i".into(),
            format!("anoisesrc=d={SECONDS}:c=pink:r=48000:a=0.2:seed={seed}"),
        ]);
    }
    let graph = if channels == 6 {
        let inputs: String = (0..6).map(|i| format!("[{i}:a]")).collect();
        format!(
            "{inputs}amerge=inputs=6,pan=5.1(side)|c0=c0|c1=c1|c2=c2|c3=c3|c4=c4|c5=c5[a]"
        )
    } else {
        "[0:a][1:a]amerge=inputs=2,pan=stereo|c0=c0|c1=c1[a]".to_string()
    };
    args.extend([
        "-filter_complex".into(),
        graph,
        "-map".into(),
        "[a]".into(),
        "-ar".into(),
        "48000".into(),
        "-c:a".into(),
        "ac3".into(),
        "-b:a".into(),
        bitrate.into(),
        "-f".into(),
        "ac3".into(),
    ]);
    args.push(out.display().to_string());
    let status = Command::new("ffmpeg").args(&args).status().expect("ffmpeg");
    assert!(status.success(), "ffmpeg encode of {name} failed");
    out
}

/// ffmpeg's decode of the stream as interleaved `f32`.
fn reference(path: &Path, channels: usize) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-acodec", "pcm_f32le", "-ac"])
        .arg(channels.to_string())
        .arg("-")
        .output()
        .expect("ffmpeg");
    assert!(out.status.success(), "ffmpeg decode failed");
    out.stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

/// Our decode with default options, and the channel count.
fn decode(path: &Path) -> (Vec<f32>, usize) {
    let data = std::fs::read(path).expect("encoded stream");
    let mut decoder = Ac3Decoder::with_options(Options::default());
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

/// Pearson correlation of one channel of two interleaved streams.
fn correlation(ours: &[f32], theirs: &[f32], channels: usize, ch: usize) -> f64 {
    let a: Vec<f64> = ours[ch..].iter().step_by(channels).map(|v| f64::from(*v)).collect();
    let b: Vec<f64> = theirs[ch..].iter().step_by(channels).map(|v| f64::from(*v)).collect();
    let n = a.len().min(b.len()).max(1) as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut cov, mut va, mut vb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(&b) {
        cov += (x - ma) * (y - mb);
        va += (x - ma) * (x - ma);
        vb += (y - mb) * (y - mb);
    }
    if va <= 0.0 || vb <= 0.0 {
        1.0
    } else {
        cov / (va * vb).sqrt()
    }
}

fn check(name: &str, channels: usize, bitrate: &str) {
    if !have_ffmpeg() {
        eprintln!("skip {name}: ffmpeg not on PATH");
        return;
    }
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ffmpeg-parity");
    std::fs::create_dir_all(&work).expect("work dir");
    let stream = encode(&work, name, channels, bitrate);
    let (ours, decoded_channels) = decode(&stream);
    let theirs = reference(&stream, channels);
    assert_eq!(decoded_channels, channels, "{name}: channel count");
    assert_eq!(ours.len(), theirs.len(), "{name}: sample count vs ffmpeg");

    let mut worst = 1.0f64;
    let mut per_channel = Vec::new();
    for ch in 0..channels {
        let corr = correlation(&ours, &theirs, channels, ch);
        per_channel.push(format!("{corr:.6}"));
        worst = worst.min(corr);
    }
    eprintln!("PARITY {name} {channels}ch {bitrate}: min corr {worst:.6} [{}]", per_channel.join(" "));
    assert!(
        worst >= MIN_CORR,
        "{name}: min per-channel correlation {worst:.6} < {MIN_CORR} [{}]",
        per_channel.join(" ")
    );
}

/// (old test name, fixture name, channels, bitrate). Every case runs; all
/// failing names are reported together.
const CASES: &[(&str, &str, usize, &str)] = &[
    ("ffmpeg_parity_5_1_448k_pink_noise", "pink-5.1-448k", 6, "448k"),
    ("ffmpeg_parity_2_0_192k_pink_noise", "pink-2.0-192k", 2, "192k"),
    ("ffmpeg_parity_5_1_640k_pink_noise", "pink-5.1-640k", 6, "640k"),
];

#[test]
fn ffmpeg_parity_pink_noise() {
    let mut failures = Vec::new();
    for &(old, name, channels, bitrate) in CASES {
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(name, channels, bitrate)));
        if ran.is_err() {
            failures.push(old);
        }
    }
    assert!(failures.is_empty(), "failed cases: {failures:?}");
}
