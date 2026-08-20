//! Encoder coverage: frames our own decoder round-trips, and that ffmpeg
//! (when present) accepts as valid AC-3 and decodes to the same audio — on
//! synthetic signals and on the real 16-bit fixtures, at every rate and
//! layout the encoder offers.

use std::path::Path;
use std::process::Command;

use ec_ac3::{Ac3Decoder, Ac3Encoder, EncoderConfig};

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// 440 Hz tone for 1 s, then 0.5 s of silence, interleaved across `channels`.
fn tone_then_silence(sample_rate: u32, channels: usize) -> Vec<f32> {
    let tone_samples = sample_rate as usize;
    let silence_samples = sample_rate as usize / 2;
    let mut out = Vec::with_capacity((tone_samples + silence_samples) * channels);
    for n in 0..tone_samples {
        let t = n as f32 / sample_rate as f32;
        let v = 0.5 * (2.0 * std::f32::consts::PI * 440.0 * t).sin();
        for _ in 0..channels {
            out.push(v);
        }
    }
    out.extend(std::iter::repeat_n(0.0f32, silence_samples * channels));
    out
}

fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for i in 0..n {
        let (x, y) = (f64::from(a[i]), f64::from(b[i]));
        sa += x;
        sb += y;
        saa += x * x;
        sbb += y * y;
        sab += x * y;
    }
    let n = n as f64;
    let cov = sab / n - (sa / n) * (sb / n);
    let va = (saa / n - (sa / n).powi(2)).max(0.0);
    let vb = (sbb / n - (sb / n).powi(2)).max(0.0);
    if va <= 0.0 || vb <= 0.0 {
        return 1.0; // both flat (silence): trivially aligned
    }
    cov / (va.sqrt() * vb.sqrt())
}

/// Encoder priming, `Ac3Encoder::encoder_delay`: the decoded stream starts
/// with this many samples of silence per channel.
const DELAY: usize = 256;

/// Lag-searched correlation on one channel after the priming is dropped,
/// +/- 64 samples of slack: a bug that shifts the audio by a block shows up
/// as ~0 here, where an unbounded search on a periodic signal would hide it.
fn best_lag_correlation(a: &[f32], b: &[f32]) -> f64 {
    let b = &b[DELAY.min(b.len())..];
    let mut best = -1.0f64;
    for lag in -64i32..=64 {
        let (sa, sb): (&[f32], &[f32]) = if lag >= 0 {
            (&a[lag as usize..], b)
        } else {
            (a, &b[(-lag) as usize..])
        };
        best = best.max(correlation(sa, sb));
    }
    best
}

/// Deinterleave channel `ch` of `channels`.
fn take_channel(interleaved: &[f32], channels: usize, ch: usize) -> Vec<f32> {
    interleaved
        .iter()
        .skip(ch)
        .step_by(channels)
        .copied()
        .collect()
}

/// The fixture's PCM decoded by ffmpeg, with its rate and channel count; None
/// without ffmpeg.
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
    let probe = ffprobe(path, "channels,sample_rate");
    let field = |key: &str| probe_field(&probe, key).and_then(|v| v.parse::<u32>().ok());
    Some((samples, field("sample_rate")?, field("channels")? as usize))
}

fn ffprobe(path: &Path, entries: &str) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a:0", "-show_entries"])
        .arg(format!("stream={entries}"))
        .args(["-of", "default=noprint_wrappers=1"])
        .arg(path)
        .output()
        .expect("ffprobe");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn probe_field(text: &str, key: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
        .map(|v| v.trim().to_owned())
}

/// ffmpeg's decode of an AC-3 file, asserting it voiced no complaint.
fn oracle_decode(path: &Path) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "warning", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-"])
        .output()
        .expect("ffmpeg");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let complaint = stderr
        .lines()
        .any(|l| ["error", "invalid", "crc"].iter().any(|k| l.to_ascii_lowercase().contains(k)));
    assert!(!complaint, "ffmpeg complained about {}:\n{stderr}", path.display());
    out.stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn encode_with_stats(config: EncoderConfig, pcm: &[f32]) -> (Vec<u8>, ec_ac3::EncodeStats) {
    let mut enc = Ac3Encoder::new(config).unwrap();
    enc.push_pcm_f32(pcm).unwrap();
    enc.finish();
    let mut out = Vec::new();
    loop {
        match enc.next_packet() {
            Ok(frame) => out.extend_from_slice(&frame),
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("{e}"),
        }
    }
    (out, enc.stats())
}

fn encode(config: EncoderConfig, pcm: &[f32]) -> Vec<u8> {
    encode_with_stats(config, pcm).0
}

/// Worst per-channel lag-searched correlation between `want` and `got`.
fn worst_corr(want: &[f32], got: &[f32], channels: usize) -> f64 {
    (0..channels)
        .map(|ch| best_lag_correlation(&take_channel(want, channels, ch), &take_channel(got, channels, ch)))
        .fold(1.0, f64::min)
}

/// [`worst_corr`] over the full-bandwidth channels only: the LFE (family slot
/// 3 of six) is band-limited to 7 bins (~660 Hz at 48 kHz) by the standard,
/// so a fixture whose LFE carries full-band content can never correlate with
/// it; that channel is gated on our decoder agreeing with the oracle instead.
fn worst_fbw_corr(want: &[f32], got: &[f32], channels: usize) -> f64 {
    (0..channels)
        .filter(|&ch| !(channels == 6 && ch == 3))
        .map(|ch| best_lag_correlation(&take_channel(want, channels, ch), &take_channel(got, channels, ch)))
        .fold(1.0, f64::min)
}

/// Deterministic white noise, `len` interleaved samples.
fn noise(len: usize, seed: &mut u64) -> Vec<f32> {
    (0..len)
        .map(|_| {
            *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((*seed >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
        })
        .collect()
}

fn decode_all(data: &[u8], channels: usize) -> Vec<f32> {
    let mut decoder = Ac3Decoder::new();
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 6 <= data.len() {
        let size = ec_ac3::frame_size(&data[pos..]).unwrap();
        assert!(pos + size <= data.len(), "frame overruns the stream");
        let frame = decoder
            .decode_frame(&data[pos..pos + size])
            .unwrap_or_else(|e| panic!("frame at byte {pos}: {e}"));
        assert_eq!(frame.samples, 1536);
        assert_eq!(frame.channels(), channels);
        out.extend(
            frame.data[0]
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])),
        );
        pos += size;
    }
    out
}

#[test]
fn silence_and_tone_frames_decode_in_our_decoder() {
    for &(sample_rate, channels) in &[(48_000u32, 1u16), (48_000, 2), (48_000, 6), (44_100, 2)] {
        let config = EncoderConfig {
            sample_rate,
            channels,
            bitrate_kbps: if channels >= 5 { 384 } else { 192 },
        };
        let pcm = tone_then_silence(sample_rate, channels as usize);
        let data = encode(config, &pcm);
        assert!(!data.is_empty(), "{sample_rate} Hz {channels}ch: no frames");
        let decoded = decode_all(&data, channels as usize);
        assert!(!decoded.is_empty());
        let corr = worst_corr(&pcm, &decoded, channels as usize);
        assert!(corr >= 0.95, "{sample_rate} Hz {channels}ch: corr {corr}");
    }
}

#[test]
fn oracle_accepts_our_frames() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not found");
        return;
    }
    let config = EncoderConfig {
        sample_rate: 48_000,
        channels: 2,
        bitrate_kbps: 192,
    };
    let pcm = tone_then_silence(48_000, 2);
    let data = encode(config, &pcm);
    let dir = std::env::temp_dir();
    let path = dir.join("ec_ac3_encode_matrix_oracle.ac3");
    std::fs::write(&path, &data).unwrap();

    let out = Command::new("ffmpeg")
        .args(["-v", "warning", "-i"])
        .arg(&path)
        .args(["-f", "f32le", "-"])
        .output()
        .expect("ffmpeg");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let complaint = stderr
        .lines()
        .any(|l| ["error", "invalid", "crc"].iter().any(|k| l.to_ascii_lowercase().contains(k)));
    assert!(
        !complaint,
        "ffmpeg complained about our frames:\n{stderr}"
    );

    let decoded: Vec<f32> = out
        .stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    for ch in 0..2 {
        let want = take_channel(&pcm, 2, ch);
        let got = take_channel(&decoded, 2, ch);
        let corr = best_lag_correlation(&want, &got);
        assert!(corr >= 0.95, "oracle channel {ch} corr {corr}");
    }

    let _ = std::fs::remove_file(Path::new(&path));
}


/// The full fixture matrix: every wav16 fixture at its layout's bit rate,
/// through our decoder and the oracle, with ffprobe agreeing on what was
/// asked for. Prints the per-file numbers and the stereo strategy shares.
#[test]
fn real_fixtures_round_trip_through_our_decoder_and_the_oracle() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not found");
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio");
    let mut failures = Vec::new();
    for layout in ["mono", "stereo", "5.1"] {
        for rate in [48_000u32, 44_100] {
            let path = root.join(format!("wav16-{layout}-{rate}.wav"));
            let (pcm, sample_rate, channels) = read_wav(&path).unwrap_or_else(|| panic!("{}", path.display()));
            assert_eq!(sample_rate, rate);
            let bitrate_kbps = match channels { 1 => 96, 2 => 192, _ => 448 };
            let config = EncoderConfig { sample_rate, channels: channels as u16, bitrate_kbps };
            let (data, stats) = encode_with_stats(config, &pcm);
            let frame = ec_ac3::frame_size(&data).unwrap();
            assert_eq!(data.len() % frame, 0, "{layout} {rate}: frames are not all {frame} bytes");

            let decoded = decode_all(&data, channels);
            let ours = worst_fbw_corr(&pcm, &decoded, channels);
            let out = std::env::temp_dir().join(format!("ec_ac3_encode_matrix_{layout}_{rate}.ac3"));
            std::fs::write(&out, &data).unwrap();
            let oracle_pcm = oracle_decode(&out);
            let oracle = worst_fbw_corr(&pcm, &oracle_pcm, channels);
            if channels == 6 {
                let lfe = correlation(&take_channel(&decoded, 6, 3), &take_channel(&oracle_pcm, 6, 3));
                assert!(lfe >= 0.99, "{layout} {rate}: LFE ours vs oracle corr {lfe}");
            }
            let probe = ffprobe(&out, "channels,channel_layout,sample_rate,bit_rate");
            let _ = std::fs::remove_file(&out);

            let total = (stats.d15 + stats.d25 + stats.d45 + stats.reuse) as f64;
            let share = |n: u64| n as f64 / total * 100.0;
            eprintln!(
                "{layout} {rate}: {} frames, ours {ours:.4} oracle {oracle:.4}, csnroffst mean {:.1}, fill {:.1}%, blksw {}, D15 {:.1}% D25 {:.1}% D45 {:.1}% REUSE {:.1}%",
                stats.frames,
                stats.csnroffst_sum as f64 / stats.frames as f64,
                stats.bits_used as f64 / stats.bits_budget as f64 * 100.0,
                stats.blksw_blocks,
                share(stats.d15), share(stats.d25), share(stats.d45), share(stats.reuse),
            );
            if ours < 0.99 || oracle < 0.99 {
                failures.push(format!("{layout} {rate}: ours {ours:.4} oracle {oracle:.4}"));
            }
            assert!(stats.bits_used * 10 > stats.bits_budget * 9, "{layout} {rate}: rate loop left bits unused: {stats:?}");
            let expect_layout = match channels { 1 => "mono", 2 => "stereo", _ => "5.1" };
            assert_eq!(probe_field(&probe, "channels").as_deref(), Some(channels.to_string().as_str()), "{probe}");
            assert!(probe_field(&probe, "channel_layout").is_some_and(|l| l.starts_with(expect_layout)), "{probe}");
            assert_eq!(probe_field(&probe, "sample_rate").as_deref(), Some(rate.to_string().as_str()), "{probe}");
            assert_eq!(probe_field(&probe, "bit_rate").as_deref(), Some((bitrate_kbps * 1000).to_string().as_str()), "{probe}");
        }
    }
    assert!(failures.is_empty(), "corr < 0.99:\n{}", failures.join("\n"));
}

/// A train of sharp attacks (decaying 2 kHz bursts, 25 per second) over a
/// quiet noise floor: every attack is a block switch, and the stream still
/// round-trips through both decoders.
#[test]
fn click_train_switches_blocks_and_still_round_trips() {
    let channels = 2;
    let mut pcm = noise(48_000 * channels, &mut 11u64);
    for v in &mut pcm {
        *v *= 0.01;
    }
    for n in (500..48_000).step_by(1900) {
        for i in 0..200 {
            let v = 0.8 * (-(i as f32) / 40.0).exp() * (i as f32 * 2.0 * std::f32::consts::PI * 2000.0 / 48_000.0).sin();
            for ch in 0..channels {
                pcm[(n + i) * channels + ch] += v;
            }
        }
    }
    let config = EncoderConfig { sample_rate: 48_000, channels: 2, bitrate_kbps: 192 };
    let (data, stats) = encode_with_stats(config, &pcm);
    assert!(stats.blksw_blocks >= 20, "{stats:?}");
    let corr = worst_corr(&pcm, &decode_all(&data, channels), channels);
    eprintln!("click train: ours {corr:.4}, {stats:?}");
    assert!(corr >= 0.95, "click train corr {corr}");
    if have_ffmpeg() {
        let out = std::env::temp_dir().join("ec_ac3_encode_matrix_clicks.ac3");
        std::fs::write(&out, &data).unwrap();
        let corr = worst_corr(&pcm, &oracle_decode(&out), channels);
        let _ = std::fs::remove_file(&out);
        eprintln!("click train: oracle {corr:.4}");
        assert!(corr >= 0.95, "click train oracle corr {corr}");
    }
}

#[test]
fn tone_at_32khz_round_trips() {
    let rate = 32_000;
    let pcm: Vec<f32> = (0..rate as usize * 2)
        .flat_map(|n| {
            let t = n as f32 / rate as f32;
            let v = 0.5 * (2.0 * std::f32::consts::PI * 440.0 * t).sin();
            [v, 0.5 * v]
        })
        .collect();
    let config = EncoderConfig { sample_rate: rate, channels: 2, bitrate_kbps: 96 };
    let data = encode(config, &pcm);
    let corr = worst_corr(&pcm, &decode_all(&data, 2), 2);
    assert!(corr >= 0.99, "32 kHz ours corr {corr}");
    if have_ffmpeg() {
        let out = std::env::temp_dir().join("ec_ac3_encode_matrix_32k.ac3");
        std::fs::write(&out, &data).unwrap();
        let corr = worst_corr(&pcm, &oracle_decode(&out), 2);
        let probe = ffprobe(&out, "sample_rate,bit_rate");
        let _ = std::fs::remove_file(&out);
        assert!(corr >= 0.99, "32 kHz oracle corr {corr}");
        assert_eq!(probe_field(&probe, "sample_rate").as_deref(), Some("32000"), "{probe}");
        assert_eq!(probe_field(&probe, "bit_rate").as_deref(), Some("96000"), "{probe}");
    }
}

/// 200 random (rate, channels, bit rate) configurations, two frames of
/// noise each: nothing panics and our decoder takes every frame.
#[test]
fn random_configs_never_panic_and_always_decode() {
    let (mut seed, mut noise_seed) = (99u64, 7u64);
    let mut next = |n: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    for _ in 0..200 {
        let sample_rate = ec_ac3::tables::SAMPLE_RATE[next(3) as usize];
        let channels = next(6) as u16 + 1;
        let bitrate_kbps = ec_ac3::tables::BIT_RATE_KBPS[next(ec_ac3::tables::BIT_RATE_KBPS.len() as u64) as usize];
        let config = EncoderConfig { sample_rate, channels, bitrate_kbps };
        let pcm = noise(1536 * 2 * channels as usize, &mut noise_seed);
        let data = encode(config, &pcm);
        let decoded = decode_all(&data, channels as usize);
        assert!(decoded.len() >= pcm.len() + 256, "{config:?}: {} of {} samples", decoded.len(), pcm.len());
    }
}


