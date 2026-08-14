//! Our decoder against ffmpeg's, over every MP3 fixture: same sample count,
//! same samples.
//!
//! Correlation rather than a bit-exact compare, because Layer III's output is
//! defined by an inverse transform rather than by an integer recipe; ISO/IEC
//! 11172-4 states decoder accuracy the same way. The bar here is 0.999, and
//! what the fixtures actually reach is printed.
//!
//! Run the table:
//!   cargo test -p ec-mp3 --release --test decode_matrix -- --nocapture

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use ec_mp3::Mp3Reader;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn mp3_fixtures() -> Vec<PathBuf> {
    let dir = fixtures().join("audio");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "mp3"))
        .collect();
    out.sort();
    out
}

fn ffmpeg_decode(path: &Path) -> Option<(Vec<f32>, u32, usize)> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let samples = out
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=channels,sample_rate",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(path)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&probe.stdout);
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .and_then(|v| v.trim().parse::<u32>().ok())
    };
    Some((samples, field("sample_rate")?, field("channels")? as usize))
}

fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    let (mut ab, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..n {
        let (x, y) = (a[i] as f64, b[i] as f64);
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    if aa == 0.0 || bb == 0.0 {
        return f64::from(u8::from(aa == bb));
    }
    ab / (aa * bb).sqrt()
}

/// Best correlation over the priming shift ffmpeg applied, and that shift.
///
/// The score is the whole overlap rather than a window: fixture tones are
/// periodic, so a short window correlates almost as well one period out and
/// the search would settle on the wrong shift.
fn best_alignment(got: &[f32], want: &[f32], channels: usize) -> (f64, usize) {
    let mut best = (f64::MIN, 0usize);
    let mut offset = 0;
    while offset < got.len() && offset <= 2304 * channels {
        let n = (got.len() - offset).min(want.len()).min(200_000);
        let corr = correlation(&got[offset..offset + n], &want[..n]);
        if corr > best.0 {
            best = (corr, offset);
        }
        offset += channels;
    }
    let (_, offset) = best;
    // The first and last frames are the encoder-delay and padding regions that
    // gapless trimming exists for: the reservoir the first frame points back
    // into predates the stream, and the last frame's tail is padding ffmpeg
    // drops. Every frame between them is compared.
    let skip = 1152 * channels;
    let n = (got.len() - offset)
        .min(want.len())
        .saturating_sub(2 * skip);
    (
        correlation(
            &got[offset + skip..offset + skip + n],
            &want[skip..skip + n],
        ),
        offset,
    )
}

#[test]
fn decodes_every_fixture_like_ffmpeg() {
    let files = mp3_fixtures();
    if files.is_empty() {
        eprintln!("no MP3 fixtures: run scripts/gen-fixtures.sh and scripts/gen-mp3-fixtures.sh");
        return;
    }
    println!(
        "{:<34} {:>6} {:>3} {:>9} {:>9} {:>8}",
        "fixture", "rate", "ch", "samples", "corr", "x rt"
    );
    let mut worst = 1.0f64;
    let mut failures = Vec::new();
    for path in &files {
        let bytes = std::fs::read(path).expect("read fixture");
        let Some((want, rate, channels)) = ffmpeg_decode(path) else {
            panic!("ffmpeg could not decode {}", path.display());
        };
        let start = Instant::now();
        let mut reader = Mp3Reader::new();
        reader.push(&bytes);
        let frames = reader.decode_all();
        let elapsed = start.elapsed().as_secs_f64();
        let mut got = Vec::with_capacity(want.len());
        for frame in &frames {
            got.extend_from_slice(&frame.samples);
        }
        let name = path.file_name().unwrap().to_string_lossy();
        // ffmpeg honours the LAME delay/padding tags and drops the encoder's
        // priming samples; we hand back every frame we were given, so the
        // comparison finds the shift first. Gapless trimming is a container
        // concern, not a decoder one.
        let (corr, offset) = best_alignment(&got, &want, channels);
        let seconds = got.len() as f64 / (rate as f64 * channels as f64);
        let realtime = seconds / elapsed.max(1e-9);
        println!(
            "{name:<34} {rate:>6} {channels:>3} {:>9} {corr:>9.6} {realtime:>8.0} (+{offset})",
            got.len()
        );
        // A sample count that disagrees by more than the encoder delay plus
        // padding means we lost or invented audio, which correlation alone
        // would hide. Two frames covers both ends of the gapless trim.
        let slack = 2 * 1152 * channels;
        if got.len() + slack < want.len() || want.len() + slack < got.len() {
            failures.push(format!(
                "{name}: {} samples, ffmpeg {} ",
                got.len(),
                want.len()
            ));
        }
        if corr < 0.999 {
            failures.push(format!("{name}: correlation {corr:.6}"));
        }
        worst = worst.min(corr);
    }
    println!("worst correlation {worst:.6} over {} fixtures", files.len());
    assert!(failures.is_empty(), "{failures:#?}");
}
