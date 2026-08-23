//! Our encoder measured the only way a lossy one can be: encode, decode the
//! result with ffmpeg, correlate against the samples that went in.
//!
//! The bar is the incumbent MP3 encoder this crate replaces, 0.6.1, measured on
//! the same fixtures and the same metric (see `BAR`, and
//! `scripts/mp3-incumbent-bar.md` for how those numbers were produced).
//!
//! Run the table:
//!   cargo test -p ec-mp3 --release --test encode_matrix -- --nocapture

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_mp3::{FrameHeader, Mp3Encoder, Mp3EncoderConfig};

/// Correlation the incumbent MP3 encoder, 0.6.1, reaches on this corpus, measured with this
/// same alignment and metric (its own encoder, ffmpeg's decoder). The tone
/// fixtures reach 1.0 on both encoders and so cannot separate them; the
/// `mp3src-*` material — glide, broadband noise and a click every half second —
/// is what does.
const BAR: [(&str, [f64; 3]); 4] = [
    ("mp3src-mono-44100", [0.99920, 0.99983, 0.99999]),
    ("mp3src-stereo-44100", [0.99612, 0.99847, 0.99972]),
    ("mp3src-mono-48000", [0.99867, 0.99972, 0.99998]),
    ("mp3src-stereo-48000", [0.99461, 0.99787, 0.99956]),
];
const BITRATES: [u32; 3] = [128, 192, 320];

/// The incumbent's number for this fixture and bitrate. Fixtures it was not
/// measured on (the tones, which both encoders code perfectly) fall back to its
/// worst result at that rate.
fn bar_for(name: &str, kbps: u32) -> f64 {
    let index = BITRATES.iter().position(|k| *k == kbps).unwrap_or(0);
    for (fixture, values) in BAR {
        if name.starts_with(fixture) {
            return values[index];
        }
    }
    BAR.iter()
        .map(|(_, v)| v[index])
        .fold(f64::INFINITY, f64::min)
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn workdir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ec-mp3-{}-{test}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create work dir");
    dir
}

/// Reads a WAV fixture as interleaved f32 plus its rate and channel count.
fn read_wav(path: &Path) -> Option<(Vec<f32>, u32, usize)> {
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
        return 0.0;
    }
    ab / (aa * bb).sqrt()
}

/// Best correlation over the encoder delay, which no MP3 encoder avoids.
fn aligned(got: &[f32], want: &[f32], channels: usize) -> (f64, usize) {
    let mut best = (f64::MIN, 0usize);
    let mut offset = 0;
    while offset + channels < got.len() && offset <= 3000 * channels {
        let n = (got.len() - offset).min(want.len());
        let skip = 1152 * channels;
        if n <= 2 * skip {
            break;
        }
        let corr = correlation(
            &got[offset + skip..offset + n - skip],
            &want[skip..n - skip],
        );
        if corr > best.0 {
            best = (corr, offset);
        }
        offset += channels;
    }
    best
}

/// Worst short-window error energy, relative to the source, over the file.
///
/// Whole-file correlation cannot see pre-echo: quantisation noise smeared
/// backwards across a 576-sample granule before an attack is a few
/// milliseconds of audible hash inside a minute of otherwise clean signal, and
/// integrating over the minute divides it away. Measured on this crate, going
/// from one short block per 9196 granules to 526 of them moved whole-file
/// correlation by 0.00004 -- in the wrong direction -- while changing exactly
/// the thing short blocks exist to fix. So the sweep also reports the worst
/// window: error energy over source energy, in dB, over 20 ms windows,
/// which is the scale a transient's pre-echo lives on.
fn worst_window_db(got: &[f32], want: &[f32], offset: usize, channels: usize) -> f64 {
    let window = 20 * 44100 / 1000 * channels;
    let skip = 1152 * channels;
    let n = (got.len() - offset).min(want.len());
    let mut worst = f64::MIN;
    let mut start = skip;
    while start + window + skip <= n {
        let (mut err, mut sig) = (0.0f64, 0.0f64);
        for i in start..start + window {
            let d = f64::from(got[offset + i]) - f64::from(want[i]);
            err += d * d;
            sig += f64::from(want[i]) * f64::from(want[i]);
        }
        // A window with no signal in it has no pre-echo to hide, and its
        // ratio would be meaningless; the -60 dBFS gate skips digital silence
        // and the fade-ins around it.
        if sig / window as f64 > 1e-6 {
            let db = 10.0 * (err / sig.max(1e-30)).log10();
            if db > worst {
                worst = db;
                if std::env::var_os("EC_MP3_WORST_WHERE").is_some() {
                    eprintln!(
                        "    worst so far {db:+.1} dB at {:.2} s, level {:.1} dBFS",
                        start as f64 / (44100.0 * channels as f64),
                        10.0 * (sig / window as f64).log10()
                    );
                }
            }
        }
        start += window;
    }
    worst
}

fn encode(pcm: &[f32], rate: u32, channels: usize, kbps: u32) -> Vec<u8> {
    let mut encoder = Mp3Encoder::new(Mp3EncoderConfig {
        bitrate_kbps: kbps,
        vbr_quality: None,
    });
    encoder
        .push_pcm_f32(pcm, channels as u16, rate)
        .expect("encoder accepts the fixture");
    encoder.finish();
    let mut out = Vec::new();
    while let Ok(frame) = encoder.next_packet() {
        out.extend_from_slice(&frame);
    }
    out
}

/// CBR-192 of the stereo 48 kHz tone fixture decodes at least as well as it
/// did before the masking model was put in the quantiser's units (9ac19d6:
/// corr 0.999999, RMS error 0.020% of the signal). A byte pin stood here
/// while VBR landed; the psychoacoustic change legitimately moves the bytes,
/// so the gate is the quality those bytes carried.
#[test]
fn cbr_quality_holds_its_floor() {
    let path = fixtures().join("audio/wav16-stereo-48000.wav");
    let Some((pcm, rate, channels)) = read_wav(&path) else {
        eprintln!("no WAV fixtures: run scripts/gen-fixtures.sh");
        return;
    };
    let bytes = encode(&pcm, rate, channels, 192);
    let file = workdir("cbr").join("cbr192.mp3");
    std::fs::write(&file, &bytes).unwrap();
    let decoded = decode(&file);
    assert_eq!(decoded.len(), pcm.len());
    let corr = correlation(&decoded, &pcm);
    let (err, sig) = decoded
        .iter()
        .zip(&pcm)
        .fold((0.0f64, 0.0f64), |(e, s), (d, p)| {
            (e + f64::from(d - p).powi(2), s + f64::from(*p).powi(2))
        });
    let rms = (err / sig).sqrt();
    println!("cbr192 corr={corr:.6} rms={:.4}%", rms * 100.0);
    assert!(corr >= 0.999_99, "corr {corr:.6}");
    assert!(rms <= 0.000_25, "rms {:.4}%", rms * 100.0);
}

fn decode(file: &Path) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "warning", "-i"])
        .arg(file)
        .args(["-f", "f32le", "-"])
        .output()
        .expect("run ffmpeg");
    assert!(
        out.stderr.is_empty(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn encode_vbr(pcm: &[f32], rate: u32, channels: usize, quality: f32) -> Vec<u8> {
    let mut encoder = Mp3Encoder::new(Mp3EncoderConfig {
        bitrate_kbps: 0,
        vbr_quality: Some(quality),
    });
    encoder
        .push_pcm_f32(pcm, channels as u16, rate)
        .expect("encoder accepts the fixture");
    encoder.finish();
    let mut out = Vec::new();
    while let Ok(frame) = encoder.next_packet() {
        out.extend_from_slice(&frame);
    }
    out
}

/// Quality 0.5 asks for a 192 kbit/s mean; true VBR spends it unevenly across
/// frames, decodes at least as well as the incumbent's CBR-192, and announces
/// itself through a Xing tag ffprobe reads as VBR.
#[test]
fn vbr_frames_vary_their_bitrate_index() {
    let name = "mp3src-stereo-48000";
    let path = fixtures().join(format!("audio/{name}.wav"));
    let Some((pcm, rate, channels)) = read_wav(&path) else {
        eprintln!("no WAV fixtures: run scripts/gen-fixtures.sh");
        return;
    };
    let bytes = encode_vbr(&pcm, rate, channels, 0.5);
    // Walk the frame headers (MPEG-1, Layer III), skipping the Xing frame.
    let table = [
        0u32, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    let mut indices = std::collections::BTreeSet::new();
    let (mut pos, mut frames) = (0usize, 0usize);
    while pos + 4 <= bytes.len() {
        let h = &bytes[pos..pos + 4];
        assert!(h[0] == 0xFF && (h[1] & 0xE0) == 0xE0, "sync lost at {pos}");
        let index = usize::from(h[2] >> 4);
        let kbps = table[index.min(14)] as usize;
        assert!(kbps > 0, "free-format header at {pos}");
        if frames > 0 {
            indices.insert(index);
        }
        frames += 1;
        pos += 144 * kbps * 1000 / rate as usize + usize::from((h[2] >> 1) & 1);
    }
    let seconds = pcm.len() as f64 / (rate as f64 * channels as f64);
    let mean = bytes.len() as f64 * 8.0 / seconds / 1000.0;
    let target = f64::from(ec_mp3::encode::bitrate_for_quality(0.5));
    println!("vbr frames={frames} indices={indices:?} mean={mean:.1} kbit/s");
    assert!(indices.len() >= 3, "only {indices:?}");
    assert!(
        (mean - target).abs() <= target * 0.25,
        "mean {mean:.1} vs {target}"
    );

    let work = workdir("vbr");
    let file = work.join("vbr.mp3");
    std::fs::write(&file, &bytes).unwrap();
    let out = Command::new("ffmpeg")
        .args(["-v", "warning", "-i"])
        .arg(&file)
        .args(["-f", "f32le", "-"])
        .output()
        .expect("run ffmpeg");
    assert!(
        out.stderr.is_empty(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let decoded: Vec<f32> = out
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let corr = correlation(&decoded, &pcm);
    let bar = bar_for(name, 192);
    println!("vbr corr={corr:.5} bar={bar:.5}");
    assert!(corr >= bar, "corr {corr:.5} < bar {bar:.5}");
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_format"])
        .arg(&file)
        .output()
        .expect("run ffprobe");
    let probe = String::from_utf8_lossy(&probe.stdout);
    // "Xing" (not "Info") plus the VBR-method byte after the 9-byte version.
    assert!(
        &bytes[36..40] == b"Xing" && bytes[36 + 16 + 9] == 0x70,
        "no VBR-flagged Xing tag"
    );
    assert!(probe.contains("format_name=mp3"), "{probe}");
}

#[test]
fn vbr_mean_bitrate_tracks_quality_on_music() {
    let path = fixtures().join("audio/wav16-stereo-48000.wav");
    let Some((pcm, rate, channels)) = read_wav(&path) else {
        eprintln!("no WAV fixtures: run scripts/gen-fixtures.sh");
        return;
    };
    let mut previous = 0.0;
    for q in [0.25f32, 0.5, 0.75] {
        let bytes = encode_vbr(&pcm, rate, channels, q);
        let seconds = pcm.len() as f64 / (rate as f64 * channels as f64);
        let mean = bytes.len() as f64 * 8.0 / seconds / 1000.0;
        let target = f64::from(ec_mp3::encode::bitrate_for_quality(q));
        let tolerance = if q <= 0.5 { 0.70 } else { 0.25 };
        println!("q={q:.2} mean={mean:.1} target={target:.1}");
        assert!(
            mean > previous,
            "q={q:.2} mean {mean:.1} did not rise above {previous:.1}"
        );
        assert!(
            (mean - target).abs() <= target * tolerance,
            "fixture is sparse/tonal, so q={q:.2} allows {:.0}%: mean {mean:.1} vs {target:.1}",
            tolerance * 100.0,
        );
        previous = mean;
    }
}

#[test]
fn encodes_above_the_incumbent_bar() {
    let dir = fixtures().join("audio");
    let mut sources: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string());
            name.is_some_and(|n| n.starts_with("wav16-") || n.starts_with("mp3src-"))
                && p.extension().is_some_and(|e| e == "wav")
        })
        .collect();
    sources.sort();
    sources.retain(|p| {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        name.contains("mono") || name.contains("stereo")
    });
    if sources.is_empty() {
        eprintln!("no WAV fixtures: run scripts/gen-fixtures.sh");
        return;
    }
    let work = workdir("encode");
    println!(
        "{:<30} {:>5} {:>6} {:>3} {:>9} {:>8} {:>9}",
        "fixture", "kbps", "rate", "ch", "corr", "bar", "kbit/s"
    );
    let mut failures = Vec::new();
    for source in &sources {
        let Some((pcm, rate, channels)) = read_wav(source) else {
            continue;
        };
        for kbps in BITRATES {
            let bar = bar_for(&source.file_name().unwrap().to_string_lossy(), kbps);
            let bytes = encode(&pcm, rate, channels, kbps);
            let path = work.join(format!(
                "{}-{kbps}.mp3",
                source.file_stem().unwrap().to_string_lossy()
            ));
            std::fs::write(&path, &bytes).expect("write encoded file");
            let out = Command::new("ffmpeg")
                .args(["-v", "warning", "-i"])
                .arg(&path)
                .args(["-f", "f32le", "-"])
                .output()
                .expect("run ffmpeg");
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if !stderr.is_empty() {
                failures.push(format!("{}: ffmpeg said {stderr}", path.display()));
            }
            let decoded: Vec<f32> = out
                .stdout
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            // With the Info tag's delay and padding honoured, ffmpeg hands
            // back exactly the samples that went in: same count, no shift. That
            // is the gapless claim, so the correlation is measured at lag zero
            // rather than at whatever lag looks best.
            if decoded.len() != pcm.len() {
                failures.push(format!(
                    "{}: decoded {} samples, pushed {}",
                    source.file_name().unwrap().to_string_lossy(),
                    decoded.len(),
                    pcm.len()
                ));
            }
            let corr = correlation(&decoded, &pcm);
            // A periodic fixture correlates equally well a whole period out,
            // so a tie is not a misalignment; only a shift that is genuinely
            // better than lag zero is.
            let (searched, offset) = aligned(&decoded, &pcm, channels);
            if offset != 0 && searched > corr + 1e-4 {
                failures.push(format!(
                    "{}: lag {offset} correlates {searched:.6}, better than lag zero {corr:.6}",
                    source.file_name().unwrap().to_string_lossy(),
                ));
            }
            let seconds = pcm.len() as f64 / (rate as f64 * channels as f64);
            let measured = bytes.len() as f64 * 8.0 / seconds / 1000.0;
            let name = source.file_name().unwrap().to_string_lossy();
            println!(
                "{name:<30} {kbps:>5} {rate:>6} {channels:>3} {corr:>9.5} {bar:>8.5} {measured:>9.1}"
            );
            // Matching counts as passing: at 320 kbit/s both encoders are
            // within a part in 10^5 of the source and the difference between
            // them is smaller than the metric resolves.
            if corr < bar - 1e-5 {
                failures.push(format!("{name} at {kbps} kbit/s: {corr:.5} < bar {bar:.5}"));
            }
            if (measured - kbps as f64).abs() > kbps as f64 * 0.05 {
                failures.push(format!(
                    "{name} at {kbps} kbit/s: wrote {measured:.1} kbit/s"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

// ---------------------------------------------------------------------------
// The live reference: LAME on the user's own library, at matched constant
// bitrate. `encodes_above_the_incumbent_bar` measures against a frozen number
// from the crate this one replaces; that number cannot notice LAME moving, and
// the fixtures it covers are synthetic. This one encodes the same seconds of
// the same file with both encoders at the same bitrate and correlates each
// against the samples that went in.
// ---------------------------------------------------------------------------

/// Sources for [`real_library_sweep_vs_lame`], the same list the vorbis and
/// opus library gates use.
const LIBRARY: [(&str, &str); 7] = [
    ("nik", "~/Music/Yok - Nikbinler.mp4"),
    ("zaur", "~/Music/Zaur Xan- Dusun Meni.mp3"),
    ("her", "~/Music/Her Nerdeysen.mp3"),
    ("naz", "~/Music/naz_aglama_ben_aglarim.mp4"),
    ("sadie", "~/Music/sadie.wav"),
    ("dl8a", "~/Downloads/8a3b6d1d19.mp3"),
    (
        "hein",
        "~/Downloads/Sadie Sink Talks Her Little Known Singing Skills, Stranger Things 5 and Brendan Fraser.mp3",
    ),
];

/// How much worse than LAME a row's worst 20 ms window may be, in dB.
///
/// The gap was 12.6 dB when this floor went in, on the two spoken-word
/// sources; mid/side stereo and the demand-weighted frame split closed it.
/// Every row now beats libmp3lame's worst window except hein at 192 kbit/s,
/// which sits 0.2 dB behind, so the floor is one decibel.
const WORST_WINDOW_EXCESS_DB: f64 = 1.0;

/// How far under LAME a row may sit before the sweep fails, in correlation.
///
/// The first measurement put every row between -0.00103 and +0.00142. All
/// fourteen are now ahead of libmp3lame, the thinnest being hein at 192
/// kbit/s at +0.00002, so the floor is zero: a row falling behind the
/// reference at all is the regression.
const LAME_CORR_FLOOR: f64 = 0.0;

fn expand(path: &str) -> PathBuf {
    PathBuf::from(match path.strip_prefix("~/") {
        Some(rest) => format!(
            "{}/{rest}",
            std::env::var("HOME").unwrap_or_else(|_| "/home".into())
        ),
        None => path.to_string(),
    })
}

/// Decodes `seconds` of a source to interleaved stereo f32 at 44.1 kHz.
fn decode_source(path: &Path, seconds: u32) -> Option<Vec<f32>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-vn",
            "-t",
            &seconds.to_string(),
            "-ac",
            "2",
            "-ar",
            "44100",
            "-f",
            "f32le",
            "-",
        ])
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    Some(
        out.stdout
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect(),
    )
}

/// Ours against LAME on the user's library, at matched constant bitrate:
///
///     cargo test -p ec-mp3 --release --test encode_matrix \
///         real_library_sweep_vs_lame -- --ignored --nocapture
#[test]
#[ignore = "needs the user's library and ffmpeg's libmp3lame"]
fn real_library_sweep_vs_lame() {
    let seconds = 60u32;
    let work = workdir("lame-sweep");
    println!(
        "{:<7} {:>5} {:>9} {:>9} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "source",
        "kbps",
        "corr_ours",
        "corr_lame",
        "gap",
        "wworst_o",
        "wworst_r",
        "ours_kb",
        "lame_kb"
    );
    let mut rows = 0;
    let mut failures = Vec::new();
    // `EC_MP3_SWEEP_ONLY=zaur,dl8a` narrows the sweep to named sources, so a
    // constant can be swept against the rows it moves without paying for the
    // rows it does not.
    let only = std::env::var("EC_MP3_SWEEP_ONLY").unwrap_or_default();
    for (name, src) in LIBRARY {
        if !only.is_empty() && !only.split(',').any(|w| w.trim() == name) {
            continue;
        }
        let src = expand(src);
        if !src.exists() {
            println!("{name:<7} SKIP (missing)");
            continue;
        }
        let Some(pcm) = decode_source(&src, seconds) else {
            println!("{name:<7} SKIP (decode)");
            continue;
        };
        let secs = pcm.len() as f64 / (44100.0 * 2.0);
        for kbps in [128u32, 192] {
            let ours_bytes = encode(&pcm, 44100, 2, kbps);
            let ours_file = work.join(format!("ours-{name}-{kbps}.mp3"));
            std::fs::write(&ours_file, &ours_bytes).expect("write ours");
            let lame_file = work.join(format!("lame-{name}-{kbps}.mp3"));
            let status = Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-i"])
                .arg(&src)
                .args([
                    "-vn",
                    "-t",
                    &seconds.to_string(),
                    "-ac",
                    "2",
                    "-ar",
                    "44100",
                    "-c:a",
                    "libmp3lame",
                    "-b:a",
                    &format!("{kbps}k"),
                ])
                .arg(&lame_file)
                .status()
                .expect("ffmpeg runs");
            assert!(status.success(), "libmp3lame encode of {name} at {kbps}k");
            // Both decoded by the same decoder and aligned the same way: an
            // encoder delay is not a quality difference.
            let measure = |file: &Path| -> (f64, f64) {
                let decoded = decode(file);
                let (corr, offset) = aligned(&decoded, &pcm, 2);
                (corr, worst_window_db(&decoded, &pcm, offset, 2))
            };
            let ((ours_corr, ours_worst), (lame_corr, lame_worst)) =
                (measure(&ours_file), measure(&lame_file));
            let kb =
                |p: &Path| std::fs::metadata(p).expect("size").len() as f64 * 8.0 / secs / 1000.0;
            let gap = ours_corr - lame_corr;
            println!(
                "{name:<7} {kbps:>5} {ours_corr:>9.5} {lame_corr:>9.5} {gap:>+8.5} \
                 {ours_worst:>+8.1} {lame_worst:>+8.1} {:>8.1} {:>8.1}",
                kb(&ours_file),
                kb(&lame_file)
            );
            rows += 1;
            if gap < LAME_CORR_FLOOR {
                failures.push(format!(
                    "{name} at {kbps} kbit/s: {ours_corr:.5} against LAME's {lame_corr:.5}"
                ));
            }
            if ours_worst - lame_worst > WORST_WINDOW_EXCESS_DB {
                failures.push(format!(
                    "{name} at {kbps} kbit/s: worst window {ours_worst:.1} dB \
                     against LAME's {lame_worst:.1} dB"
                ));
            }
        }
    }
    assert!(rows > 0, "no library sources were readable");
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The `mode_extension` of every frame of a stream, in order.
fn mode_exts(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 4 <= bytes.len() {
        let Ok(header) = FrameHeader::parse(&bytes[at..]) else {
            at += 1;
            continue;
        };
        let Some(len) = header.frame_len() else { break };
        out.push(header.mode_ext);
        at += len;
    }
    out
}

/// Mid/side is chosen for the signal it helps and declined for the one it
/// hurts. Two channels carrying the same waveform put everything in the sum
/// and nothing in the difference, which is the case mid/side exists for; two
/// carrying opposite waveforms are its worst case, where the difference is
/// the loud channel and coding it as such would spend the frame's bits on the
/// quiet one. Both roundtrip, because the decoder undoes whichever was
/// written.
#[test]
fn mid_side_follows_the_channel_correlation() {
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut noise = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 40) as f32 / 8388608.0 - 0.5
    };
    let n = 44100;
    let (mut same, mut opposed) = (Vec::with_capacity(n * 2), Vec::with_capacity(n * 2));
    for i in 0..n {
        // Band-limited enough to be codeable: a slow tone under the noise.
        let tone = (i as f32 * 0.03).sin() * 0.4;
        let v = tone + noise() * 0.05;
        same.push(v);
        same.push(v);
        opposed.push(v);
        opposed.push(-v);
    }
    for (label, pcm, want_ms) in [("same", same, true), ("opposed", opposed, false)] {
        let bytes = encode(&pcm, 44100, 2, 128);
        let exts = mode_exts(&bytes);
        assert!(exts.len() > 20, "{label}: only {} frames", exts.len());
        let ms = exts.iter().filter(|e| *e & 2 != 0).count();
        // The first and last frames carry priming and a tail, so judge the body.
        let body = exts.len() - 2;
        if want_ms {
            assert!(
                ms >= body,
                "{label}: only {ms} of {} frames are mid/side",
                exts.len()
            );
        } else {
            assert_eq!(ms, 0, "{label}: {ms} frames chose mid/side");
        }
    }
}
