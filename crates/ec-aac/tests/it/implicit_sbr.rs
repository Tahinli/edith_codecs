//! Implicit SBR signalling (ISO/IEC 14496-3 1.6.5.2): a stream whose config or
//! ADTS header says plain AAC-LC, but whose frames carry an SBR extension
//! payload, is decoded as HE-AAC at twice the core rate, as ffmpeg does.

use crate::shared::fixtures;
use crate::shared::have_ffmpeg;
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_aac::{AacDecoder, SbrSupport};

/// Same contract as `oracle.rs`: an absent fixture skips loudly, and under
/// `EC_REQUIRE_FIXTURES=1` it is a hard failure instead.
fn require_fixture(path: &Path) -> bool {
    let present = path.exists();
    assert!(
        present || std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
        "EC_REQUIRE_FIXTURES=1 but fixture {} is absent",
        path.display()
    );
    if !present {
        eprintln!("SKIP: fixture {} absent", path.display());
    }
    present
}

/// The implicit-signalling HE-AAC fixture: 2 s of `heaac_44100_48k.m4a` remuxed to
/// bare ADTS with `ffmpeg -c copy -f adts`. Its headers say AAC-LC at 22050 Hz.
fn implicit_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/heaac_44100_48k_implicit_2s.aac")
}

/// ffmpeg's decode of a file, as planar-by-channel `f32`.
fn ffmpeg_decode(path: &Path) -> Vec<Vec<f32>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-acodec", "pcm_f32le", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg decode failed");
    let samples: Vec<f32> = out
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    deinterleave(&samples, 2)
}

fn deinterleave(samples: &[f32], channels: usize) -> Vec<Vec<f32>> {
    let mut out = vec![Vec::with_capacity(samples.len() / channels); channels];
    for (i, &v) in samples.iter().enumerate() {
        out[i % channels].push(v);
    }
    out
}

/// Pearson correlation over the overlap of two channels.
fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    assert!(n > 1024, "not enough overlap to compare: {n} samples");
    let (a, b) = (&a[..n], &b[..n]);
    let ma = a.iter().map(|v| f64::from(*v)).sum::<f64>() / n as f64;
    let mb = b.iter().map(|v| f64::from(*v)).sum::<f64>() / n as f64;
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (x, y) = (f64::from(a[i]) - ma, f64::from(b[i]) - mb);
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da == 0.0 || db == 0.0 {
        return if da == db { 1.0 } else { 0.0 };
    }
    num / (da * db).sqrt()
}

/// Decodes an ADTS file frame by frame, reading the output rate after each
/// frame, as a caller would. Returns the planes, the rate before the first
/// frame, and the rate after each frame.
fn decode_adts(path: &Path) -> (Vec<Vec<f32>>, Option<u32>, Vec<u32>, AacDecoder) {
    let data = std::fs::read(path).expect("fixture readable");
    let mut decoder = AacDecoder::new();
    let before = decoder.output_sample_rate();
    let mut planes: Vec<Vec<f32>> = Vec::new();
    let mut rates = Vec::new();
    let mut at = 0usize;
    while at + 7 <= data.len() {
        let header = ec_aac::parse_adts(&data[at..]).expect("adts header");
        let end = (at + header.frame_length).min(data.len());
        let frame = decoder.decode(&data[at..end], None).expect("frame decodes");
        assert_eq!(
            Some(frame.sample_rate),
            decoder.output_sample_rate(),
            "a frame's rate must match the decoder's rate after that frame"
        );
        rates.push(frame.sample_rate);
        let ch = usize::from(frame.channels);
        if planes.is_empty() {
            planes = vec![Vec::new(); ch];
        }
        for (i, v) in frame.samples.iter().enumerate() {
            planes[i % ch].push(*v);
        }
        at = end;
    }
    (planes, before, rates, decoder)
}

#[test]
fn implicit_he_aac_adts_switches_to_double_rate() {
    let path = implicit_fixture();
    if !require_fixture(&path) {
        return;
    }
    let (ours, before, rates, decoder) = decode_adts(&path);

    // Headers say 22050 Hz AAC-LC: nothing is known about the rate before frame 1.
    assert_eq!(before, None, "no output rate before the first frame");
    assert!(decoder.sbr_implicit(), "the SBR payload must switch implicit SBR on");
    assert_eq!(decoder.sbr_support(), SbrSupport::V1);
    assert_eq!(decoder.config().map(|c| c.sample_rate), None, "ADTS carries no config");
    assert_eq!(decoder.output_sample_rate(), Some(44_100));

    // The first frame carries the payload, so it is already at the doubled rate.
    assert_eq!(rates[0], 44_100, "first frame is decoded with SBR");
    assert!(rates.iter().all(|&r| r == 44_100));
    assert_eq!(ours.len(), 2, "stereo");
    let frames = ours[0].len();
    assert!(frames > 80_000, "about 2 s at 44.1 kHz, got {frames}");
    assert_eq!(ours[1].len(), frames);
    for (ch, plane) in ours.iter().enumerate() {
        let rms = (plane.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / frames as f64).sqrt();
        assert!(rms > 1e-4, "channel {ch} is silent after SBR (rms {rms})");
    }
}

#[test]
fn implicit_he_aac_adts_matches_ffmpeg_per_channel() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let path = implicit_fixture();
    if !require_fixture(&path) {
        return;
    }
    let (ours, _, _, _) = decode_adts(&path);
    let theirs = ffmpeg_decode(&path);
    assert_eq!(theirs.len(), ours.len());
    for (ch, (a, b)) in ours.iter().zip(&theirs).enumerate() {
        let c = correlation(a, b);
        assert!(c >= 0.999, "channel {ch}: correlation with ffmpeg {c:.6} < 0.999");
    }
}

#[test]
fn plain_aac_lc_adts_is_unchanged_by_implicit_detection() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let path = fixtures().join("audio/aac-adts-stereo-44100.aac");
    if !require_fixture(&path) {
        return;
    }
    let (ours, _, rates, decoder) = decode_adts(&path);

    // A plain LC stream has no SBR payload: no switch, and the rate stays at the core.
    assert!(!decoder.sbr_implicit());
    assert_eq!(decoder.sbr_support(), SbrSupport::NotSignalled);
    assert!(rates.iter().all(|&r| r == 44_100));
    assert_eq!(decoder.output_sample_rate(), Some(44_100));

    let theirs = ffmpeg_decode(&path);
    for (ch, (a, b)) in ours.iter().zip(&theirs).enumerate() {
        let c = correlation(a, b);
        assert!(c >= 0.999, "channel {ch}: correlation with ffmpeg {c:.6} < 0.999");
    }
}

/// Core-only decoding of the implicit HE-AAC stream: every frame carries an SBR
/// payload, and `core_only` must skip it, so the output stays at the 22050 Hz core
/// rate with 1024 samples per channel per frame. Before the switch existed, a
/// core-only measurement built with `with_config` re-armed implicit SBR and got the
/// doubled-rate full chain back, labelled as core.
#[test]
fn core_only_decode_of_implicit_he_aac_stays_at_core_rate() {
    let path = implicit_fixture();
    if !require_fixture(&path) {
        return;
    }
    let data = std::fs::read(&path).expect("fixture readable");
    let mut decoder = AacDecoder::new().core_only();
    let mut frames = 0usize;
    let mut at = 0usize;
    while at + 7 <= data.len() {
        let header = ec_aac::parse_adts(&data[at..]).expect("adts header");
        let end = (at + header.frame_length).min(data.len());
        let frame = decoder.decode(&data[at..end], None).expect("frame decodes");
        assert_eq!(
            frame.sample_rate, 22_050,
            "frame {frames} must stay at the core rate"
        );
        assert_eq!(
            frame.samples.len(),
            1024 * usize::from(frame.channels),
            "frame {frames}: one core frame of 1024 samples per channel"
        );
        frames += 1;
        at = end;
    }
    assert!(frames > 0, "fixture has frames");
    assert!(
        !decoder.sbr_implicit(),
        "core-only must never arm implicit SBR"
    );
    assert_eq!(decoder.sbr_support(), SbrSupport::NotSignalled);
    assert_eq!(decoder.output_sample_rate(), Some(22_050));
}
