//! Our encoder against the fixture matrix, proven the only way a lossless
//! codec can be: encode, decode the result with ffmpeg, compare with the input
//! byte for byte.
//!
//! The size column is informational — it is the same corpus measured against
//! `ffmpeg -c:a flac` at its default compression level.
//!
//! Run the table:
//!   cargo test -p ec-flac --release --test encode_matrix -- --nocapture

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_flac::encode::{EncoderConfig, encode};

struct Source {
    path: PathBuf,
    channels: usize,
    rate: u32,
    bits: u32,
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// A working directory keyed by test name: these tests are threads of one
/// process, so a shared temp directory would have them deleting each other's
/// files.
fn workdir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ec-flac-{}-{test}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create work dir");
    dir
}

fn probe(path: &Path) -> Option<Source> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=channels,sample_rate,bits_per_raw_sample,sample_fmt",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(path)
        .output()
        .ok()?;
    // Key=value, because ffprobe prints its own field order, not the requested
    // one — a positional parse silently swaps rate and channels.
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .map(str::trim)
    };
    let bits = match field("bits_per_raw_sample").and_then(|v| v.parse::<u32>().ok()) {
        Some(n) => n,
        // WAV states no `bits_per_raw_sample`; its sample format carries it.
        None => match field("sample_fmt")? {
            "s16" | "s16p" => 16,
            "s32" | "s32p" => 32,
            "u8" | "u8p" => 8,
            _ => return None,
        },
    };
    Some(Source {
        path: path.to_path_buf(),
        channels: field("channels")?.parse().ok()?,
        rate: field("sample_rate")?.parse().ok()?,
        bits,
    })
}

fn ffmpeg_pcm(path: &Path, bits: u32) -> Vec<u8> {
    let format = if bits <= 16 { "s16le" } else { "s32le" };
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", format, "-"])
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg failed on {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// Raw PCM as the samples FLAC codes: the container shift undone.
fn samples_from_pcm(pcm: &[u8], bits: u32) -> Vec<i32> {
    match bits <= 16 {
        true => pcm
            .chunks_exact(2)
            .map(|c| i32::from(i16::from_le_bytes([c[0], c[1]])) >> (16 - bits))
            .collect(),
        false => pcm
            .chunks_exact(4)
            .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) >> (32 - bits))
            .collect(),
    }
}

fn ffmpeg_flac_size(src: &Path, out: &Path) -> Option<u64> {
    let _ = std::fs::remove_file(out);
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(src)
        .args(["-c:a", "flac"])
        .arg(out)
        .status()
        .ok()?;
    match status.success() {
        true => std::fs::metadata(out).ok().map(|m| m.len()),
        false => None,
    }
}

fn matrix() -> Vec<Source> {
    let audio = fixtures().join("audio");
    let vectors = fixtures().join("vectors/flac-xiph/flac-test-files-main/subset");
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&audio) {
        let mut found: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                name.starts_with("flac-") || name.starts_with("wav16-")
            })
            .collect();
        found.sort();
        paths.append(&mut found);
    }
    // Depth coverage the generated fixtures do not have: 24- and 20-bit.
    for name in [
        "28 - high resolution audio, default settings.flac",
        "37 - 20 bit per sample.flac",
    ] {
        let p = vectors.join(name);
        if p.exists() {
            paths.push(p);
        }
    }
    paths.iter().filter_map(|p| probe(p)).collect()
}

#[test]
fn encoded_streams_decode_back_bit_exact_through_ffmpeg() {
    let sources = matrix();
    if sources.is_empty() {
        eprintln!("skipped: fixtures not generated");
        return;
    }
    let dir = workdir("roundtrip");
    let config = EncoderConfig::default();
    let mut failures = Vec::new();
    let (mut ours_total, mut theirs_total) = (0u64, 0u64);
    println!(
        "{:<34} {:>3} {:>3} {:>7}  {:>10} {:>10} {:>6}",
        "fixture", "ch", "bit", "rate", "ours", "ffmpeg", "ratio"
    );
    for source in &sources {
        let name = source
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let pcm = ffmpeg_pcm(&source.path, source.bits);
        let samples = samples_from_pcm(&pcm, source.bits);
        let encoded = match encode(&config, &samples, source.channels, source.bits, source.rate) {
            Ok(bytes) => bytes,
            Err(e) => {
                failures.push(format!("{name}: encode failed: {e}"));
                continue;
            }
        };
        let out = dir.join(format!("{name}.ours.flac"));
        std::fs::write(&out, &encoded).expect("write encoded");
        let decoded = ffmpeg_pcm(&out, source.bits);
        if decoded != pcm {
            let at = decoded
                .iter()
                .zip(&pcm)
                .position(|(a, b)| a != b)
                .map_or_else(|| "length only".to_string(), |i| format!("byte {i}"));
            failures.push(format!(
                "{name}: ffmpeg decoded our stream differently at {at} ({} vs {} bytes)",
                decoded.len(),
                pcm.len()
            ));
            continue;
        }
        let theirs = ffmpeg_flac_size(&source.path, &dir.join(format!("{name}.ffmpeg.flac")));
        ours_total += encoded.len() as u64;
        theirs_total += theirs.unwrap_or(0);
        println!(
            "{:<34} {:>3} {:>3} {:>7}  {:>10} {:>10} {:>6}",
            name,
            source.channels,
            source.bits,
            source.rate,
            encoded.len(),
            theirs.map_or("-".to_string(), |n| n.to_string()),
            theirs.map_or("-".to_string(), |n| format!(
                "{:.3}",
                encoded.len() as f64 / n as f64
            )),
        );
    }
    println!(
        "totals: ours {ours_total}, ffmpeg {theirs_total}, ratio {:.3}",
        ours_total as f64 / theirs_total.max(1) as f64
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // Informational bar from the slice charter: within 10% of ffmpeg's default.
    assert!(
        ours_total as f64 <= theirs_total as f64 * 1.10,
        "corpus is {:.1}% larger than ffmpeg's default FLAC",
        (ours_total as f64 / theirs_total as f64 - 1.0) * 100.0
    );
}

#[test]
fn our_streams_survive_a_probe_and_state_their_shape() {
    let Some(source) = matrix().into_iter().find(|s| s.channels == 6) else {
        eprintln!("skipped: no 5.1 fixture");
        return;
    };
    let dir = workdir("probe");
    let pcm = ffmpeg_pcm(&source.path, source.bits);
    let samples = samples_from_pcm(&pcm, source.bits);
    let encoded = encode(
        &EncoderConfig::default(),
        &samples,
        source.channels,
        source.bits,
        source.rate,
    )
    .expect("encode");
    let out = dir.join("surround.flac");
    std::fs::write(&out, &encoded).expect("write");
    let probed = probe(&out).expect("ffprobe our own output");
    assert_eq!(probed.channels, source.channels);
    assert_eq!(probed.rate, source.rate);
    assert_eq!(probed.bits, source.bits);
    // `-v error` clean: ffmpeg must find nothing to complain about.
    let out_err = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&out)
        .args(["-f", "null", "-"])
        .output()
        .expect("run ffmpeg");
    assert!(
        out_err.stderr.is_empty(),
        "ffmpeg complained: {}",
        String::from_utf8_lossy(&out_err.stderr)
    );
}
