//! AAC-LC multichannel parity against FFmpeg: mono, stereo, 5.1 and 7.1, in raw
//! ADTS and in mp4 (raw access units with an AudioSpecificConfig).
//!
//! Every fixture is a synthetic multitone with a distinct frequency per channel,
//! so a channel mapping error (SCE/CPE/LFE order, PCE, 7.1 config 7 vs 12) shows up
//! as a wrong per-channel correlation rather than passing silently.
//!
//! Each fixture is encoded by ffmpeg into a per-test temporary directory, decoded
//! by ec-aac, and decoded by ffmpeg as f32le. The two decodes are aligned on the
//! best integer lag (encoder and decoder priming differ between the two paths),
//! then each channel's correlation must reach 0.999. For 5.1 and 7.1 the channel
//! order is also checked: our channel k must correlate best with FFmpeg's channel k.
//!
//! Fixtures are generated at test time, so no media is committed. Without ffmpeg
//! the test skips loudly; with `EC_REQUIRE_FIXTURES=1` a missing ffmpeg is a failure.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_aac::{AacDecoder, is_adts, parse_adts};
use ec_core::{CodecId, Demuxer, Packet};
use ec_mp4::Mp4Demuxer;

const RATE: u32 = 48_000;
const SECONDS: u32 = 2;
/// Largest lag searched in either direction, in samples. Covers encoder delay
/// (1024 samples for AAC-LC) and the decoder's own priming with margin.
const LAG_MAX: i64 = 4096;
/// Samples used for the coarse lag search.
const LAG_WINDOW: usize = 8192;
const MIN_CORR: f64 = 0.999;

#[derive(Clone, Copy, Debug)]
enum Container {
    Adts,
    Mp4,
}

impl Container {
    fn label(self) -> &'static str {
        match self {
            Container::Adts => "adts",
            Container::Mp4 => "m4a",
        }
    }
    fn ext(self) -> &'static str {
        match self {
            Container::Adts => "aac",
            Container::Mp4 => "m4a",
        }
    }
    fn muxer(self) -> &'static str {
        match self {
            Container::Adts => "adts",
            Container::Mp4 => "mp4",
        }
    }
}

/// One layout: ffmpeg channel-layout name, channel count, and a distinct tone per channel.
struct Layout {
    name: &'static str,
    channels: usize,
    /// Tone frequency per channel, in film order as ffmpeg lays it out.
    freqs: &'static [u32],
}

const LAYOUTS: &[Layout] = &[
    Layout { name: "mono", channels: 1, freqs: &[300] },
    Layout { name: "stereo", channels: 2, freqs: &[300, 450] },
    Layout { name: "5.1", channels: 6, freqs: &[300, 450, 620, 780, 960, 1130] },
    Layout { name: "7.1", channels: 8, freqs: &[300, 450, 620, 780, 960, 1130, 1310, 1500] },
];

/// The encoders under test. Native `aac` is always present; `libfdk_aac` is used
/// only when the local ffmpeg lists it.
const ENCODERS: &[&str] = &["aac", "libfdk_aac"];

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("aac_multichannel")
}

fn ffmpeg_ok(args: &[&str]) -> bool {
    Command::new("ffmpeg")
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn have_ffmpeg() -> bool {
    ffmpeg_ok(&["-version"])
}

fn have_encoder(name: &str) -> bool {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .any(|l| l.split_whitespace().nth(1) == Some(name)),
        Err(_) => false,
    }
}

/// Encodes a multitone of `layout` with `encoder` into `container`. Returns the path,
/// or `None` when the encoder refuses the configuration.
fn encode(layout: &Layout, encoder: &str, container: Container) -> Option<PathBuf> {
    let dir = fixtures_dir();
    std::fs::create_dir_all(&dir).expect("fixture dir");
    let out = dir.join(format!(
        "{}_{}_{}.{}",
        layout.name.replace('.', ""),
        encoder,
        container.label(),
        container.ext()
    ));
    let expr = layout
        .freqs
        .iter()
        .map(|f| format!("0.25*sin(2*PI*{f}*t)"))
        .collect::<Vec<_>>()
        .join("|");
    let source = format!(
        "aevalsrc={expr}:s={RATE}:d={SECONDS}:c={}",
        layout.name
    );
    // 48 kbit/s per channel keeps every layout in a comfortable quality range.
    let bitrate = format!("{}k", 48 * layout.channels);
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", &source])
        .args(["-c:a", encoder, "-profile:a", "aac_low", "-b:a", &bitrate])
        .args(["-ac", &layout.channels.to_string(), "-f", container.muxer()])
        .arg(&out)
        .status()
        .expect("ffmpeg runs");
    status.success().then_some(out)
}

/// FFmpeg's decode of `path` as planar-by-channel `f32`, in its own channel order.
fn ffmpeg_decode(path: &Path, channels: usize) -> Vec<Vec<f32>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-acodec", "pcm_f32le", "-ac"])
        .arg(channels.to_string())
        .arg("-")
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "ffmpeg decode of {} failed: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let samples: Vec<f32> = out
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect();
    deinterleave(&samples, channels)
}

fn deinterleave(samples: &[f32], channels: usize) -> Vec<Vec<f32>> {
    let mut planes = vec![Vec::with_capacity(samples.len() / channels); channels];
    for (i, &v) in samples.iter().enumerate() {
        planes[i % channels].push(v);
    }
    planes
}

/// Decodes an ADTS file frame by frame, the way a container path feeds a packet per frame.
fn ours_adts(path: &Path) -> (Vec<Vec<f32>>, u16, u32) {
    let data = std::fs::read(path).expect("fixture reads");
    assert!(is_adts(&data), "{} is not ADTS", path.display());
    let mut decoder = AacDecoder::new();
    let mut interleaved: Vec<f32> = Vec::new();
    let mut channels = 0u16;
    let mut rate = 0u32;
    let mut at = 0usize;
    while at + 7 <= data.len() {
        let header = parse_adts(&data[at..]).expect("ADTS header parses");
        let end = (at + header.frame_length).min(data.len());
        let frame = decoder
            .decode(&data[at..end], None)
            .unwrap_or_else(|e| panic!("ADTS frame at {at} failed: {e:?}"));
        channels = frame.channels;
        rate = frame.sample_rate;
        interleaved.extend_from_slice(&frame.samples);
        if header.frame_length == 0 {
            break;
        }
        at += header.frame_length;
    }
    (deinterleave(&interleaved, usize::from(channels)), channels, rate)
}

/// Decodes raw access units from an mp4 with the AudioSpecificConfig its esds carries.
fn ours_mp4(path: &Path) -> (Vec<Vec<f32>>, u16, u32) {
    let mut demux = Mp4Demuxer::new(BufReader::new(File::open(path).expect("mp4 opens")))
        .expect("mp4 parses");
    let (idx, asc) = {
        let aac = demux
            .streams()
            .iter()
            .find(|s| s.params.codec == CodecId::Aac)
            .expect("mp4 has an AAC track");
        let asc = aac.params.extradata.as_ref().expect("esds carries the ASC").to_vec();
        (aac.index, asc)
    };
    let mut decoder = AacDecoder::with_config_bytes(&asc).expect("ASC parses");
    let mut interleaved: Vec<f32> = Vec::new();
    let mut channels = 0u16;
    let mut rate = 0u32;
    while let Ok(Packet { stream, data, .. }) = demux.next_packet() {
        if stream != idx {
            continue;
        }
        let frame = decoder
            .decode(&data, None)
            .unwrap_or_else(|e| panic!("access unit failed to decode: {e:?}"));
        channels = frame.channels;
        rate = frame.sample_rate;
        interleaved.extend_from_slice(&frame.samples);
    }
    (deinterleave(&interleaved, usize::from(channels)), channels, rate)
}

/// Pearson correlation of `a[..n]` against `b[lag..]` (or `a[-lag..]` against `b`).
fn correlation_at(a: &[f32], b: &[f32], lag: i64, len: usize) -> Option<f64> {
    let (oa, ob) = if lag >= 0 {
        (0usize, lag as usize)
    } else {
        ((-lag) as usize, 0usize)
    };
    if oa + len > a.len() || ob + len > b.len() {
        return None;
    }
    let (mut num, mut da, mut db) = (0f64, 0f64, 0f64);
    for i in 0..len {
        let (x, y) = (f64::from(a[oa + i]), f64::from(b[ob + i]));
        num += x * y;
        da += x * x;
        db += y * y;
    }
    // Two silent spans agree; a silent span against a tone does not.
    if da == 0.0 && db == 0.0 {
        return Some(1.0);
    }
    Some(num / (da.sqrt() * db.sqrt()).max(1e-30))
}

/// Finds the integer lag that best aligns `ours` with `theirs`.
///
/// A coarse search scores every lag in `±LAG_MAX` on a window from the middle of the
/// signal. A steady tone repeats its samples at every multiple of its period, so
/// several lags tie there. The near-tied candidates are then scored over the whole
/// overlap, where only the true alignment matches at both ends.
fn best_lag(ours: &[Vec<f32>], theirs: &[Vec<f32>]) -> i64 {
    let len = ours[0].len().min(theirs[0].len());
    let start = len / 2;
    let window = LAG_WINDOW.min(len / 4);
    let coarse = |lag: i64| -> Option<f64> {
        let mut score = 0f64;
        for (a, b) in ours.iter().zip(theirs) {
            score += correlation_at(&a[start..], &b[start..], lag, window)?;
        }
        Some(score)
    };
    let scored: Vec<(i64, f64)> = (-LAG_MAX..=LAG_MAX)
        .filter_map(|lag| coarse(lag).map(|c| (lag, c)))
        .collect();
    let top = scored.iter().map(|&(_, c)| c).fold(f64::MIN, f64::max);
    let full = |lag: i64| -> f64 {
        let shift = lag.unsigned_abs() as usize;
        let n = len.saturating_sub(shift);
        ours.iter()
            .zip(theirs)
            .map(|(a, b)| correlation_at(a, b, lag, n).unwrap_or(f64::MIN))
            .sum()
    };
    scored
        .iter()
        .filter(|&&(_, c)| c >= top - 1e-3)
        .map(|&(lag, _)| (lag, full(lag)))
        .fold((0i64, f64::MIN), |acc, (lag, c)| if c > acc.1 { (lag, c) } else { acc })
        .0
}

/// Correlation of every our-channel `k` against every FFmpeg channel `j` at `lag`.
fn matrix(ours: &[Vec<f32>], theirs: &[Vec<f32>], lag: i64) -> Vec<Vec<f64>> {
    let shift = lag.unsigned_abs() as usize;
    ours.iter()
        .map(|a| {
            theirs
                .iter()
                .map(|b| {
                    let len = a.len().min(b.len()).saturating_sub(shift);
                    correlation_at(a, b, lag, len).unwrap_or(f64::MIN)
                })
                .collect()
        })
        .collect()
}

fn check_case(layout: &Layout, encoder: &str, container: Container, failures: &mut Vec<String>) {
    let label = format!("{:<6} {:<5} {:<10}", layout.name, container.label(), encoder);
    let Some(path) = encode(layout, encoder, container) else {
        println!("SKIP {label}: encoder refused the configuration");
        return;
    };
    let theirs = ffmpeg_decode(&path, layout.channels);
    let (ours, channels, rate) = match container {
        Container::Adts => ours_adts(&path),
        Container::Mp4 => ours_mp4(&path),
    };
    if usize::from(channels) != layout.channels || rate != RATE {
        failures.push(format!(
            "{label}: decoded {channels} ch at {rate} Hz, expected {} ch at {RATE} Hz",
            layout.channels
        ));
        return;
    }
    let lag = best_lag(&ours, &theirs);
    let grid = matrix(&ours, &theirs, lag);
    let diag: Vec<String> = grid
        .iter()
        .enumerate()
        .map(|(k, row)| format!("{:.6}", row[k]))
        .collect();
    println!("{label} lag {lag:>5}  corr [{}]", diag.join(", "));
    if std::env::var_os("EC_AAC_PARITY_DEBUG").is_some() {
        let rms = |p: &[f32]| (p.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / p.len() as f64).sqrt();
        let fmt = |planes: &[Vec<f32>]| planes.iter().map(|p| format!("{:.4}", rms(p))).collect::<Vec<_>>().join(" ");
        println!("  rms ours   [{}]", fmt(&ours));
        println!("  rms ffmpeg [{}]", fmt(&theirs));
        for (k, row) in grid.iter().enumerate() {
            let cells: Vec<String> = row.iter().map(|c| format!("{c:+.3}")).collect();
            println!("  ours {k}: {}", cells.join(" "));
        }
    }

    for (k, row) in grid.iter().enumerate() {
        let own = row[k];
        let (best_j, best_c) = row
            .iter()
            .enumerate()
            .fold((0usize, f64::MIN), |acc, (j, &c)| if c > acc.1 { (j, c) } else { acc });
        if own < MIN_CORR {
            failures.push(format!(
                "{label}: channel {k} correlates {own:.6} with ffmpeg channel {k}, best is ffmpeg channel {best_j} at {best_c:.6}"
            ));
        }
        // The order check applies where the task names it (5.1 and 7.1).
        // A silent channel matches every silent one, so the order is wrong only when
        // some other channel correlates strictly better than the one at this position.
        if layout.channels >= 6 && own + 1e-6 < best_c {
            failures.push(format!(
                "{label}: our channel {k} matches ffmpeg channel {best_j} better than ffmpeg channel {k}, channel order differs"
            ));
        }
    }
}

#[test]
fn multichannel_matches_ffmpeg_per_channel() {
    if !have_ffmpeg() {
        assert!(
            std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
            "EC_REQUIRE_FIXTURES=1 but ffmpeg is not installed"
        );
        eprintln!("SKIP: ffmpeg not installed");
        return;
    }
    let mut failures = Vec::new();
    for encoder in ENCODERS {
        if *encoder != "aac" && !have_encoder(encoder) {
            println!("SKIP encoder {encoder}: not built into this ffmpeg");
            continue;
        }
        for layout in LAYOUTS {
            for container in [Container::Adts, Container::Mp4] {
                check_case(layout, encoder, container, &mut failures);
            }
        }
    }
    assert!(
        failures.is_empty(),
        "multichannel parity failures:\n  {}",
        failures.join("\n  ")
    );
}
