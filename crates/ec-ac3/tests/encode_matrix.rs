//! A1 skeleton coverage: the fixed-allocation encoder writes frames our own
//! decoder round-trips, and ffmpeg accepts as valid AC-3 when it is present.

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

/// Lag-searched correlation on one channel, +/- 64 samples: the encoder's
/// analysis window and the decoder's overlap-add each carry their own group
/// delay.
fn best_lag_correlation(a: &[f32], b: &[f32]) -> f64 {
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

fn encode(config: EncoderConfig, pcm: &[f32]) -> Vec<u8> {
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
    out
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
        for ch in 0..channels as usize {
            let want = take_channel(&pcm, channels as usize, ch);
            let got = take_channel(&decoded, channels as usize, ch);
            let corr = best_lag_correlation(&want, &got);
            assert!(
                corr >= 0.95,
                "{sample_rate} Hz {channels}ch channel {ch}: corr {corr}"
            );
        }
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

