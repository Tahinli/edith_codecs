//! Decoder and encoder against ffmpeg, on the generated fixtures and on the
//! real library. Every test that needs a fixture or ffmpeg skips loudly when it
//! is absent rather than passing quietly.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_aac::{AacDecoder, AacEncoder, AacEncoderConfig};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// ffmpeg's decode of a file, as planar-by-channel `f32`.
fn ffmpeg_decode(path: &Path, channels: usize) -> Vec<Vec<f32>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-acodec", "pcm_f32le", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "ffmpeg decode failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    deinterleave(&bytes_to_f32(&out.stdout), channels)
}

fn bytes_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn deinterleave(samples: &[f32], channels: usize) -> Vec<Vec<f32>> {
    let mut out = vec![Vec::with_capacity(samples.len() / channels.max(1)); channels];
    for (i, &v) in samples.iter().enumerate() {
        out[i % channels].push(v);
    }
    out
}

/// Our decode of an ADTS file, as planar-by-channel `f32`.
fn our_decode(path: &Path) -> (Vec<Vec<f32>>, u32) {
    let data = std::fs::read(path).expect("fixture readable");
    let mut decoder = AacDecoder::new();
    let mut planes: Vec<Vec<f32>> = Vec::new();
    let mut rate = 0;
    let mut at = 0usize;
    while at + 7 <= data.len() {
        let header = ec_aac::parse_adts(&data[at..]).expect("adts header");
        let end = (at + header.frame_length).min(data.len());
        let frame = decoder.decode(&data[at..end], None).expect("frame decodes");
        rate = frame.sample_rate;
        let ch = usize::from(frame.channels);
        if planes.is_empty() {
            planes = vec![Vec::new(); ch];
        }
        for (i, v) in frame.samples.iter().enumerate() {
            planes[i % ch].push(*v);
        }
        at = end;
    }
    (planes, rate)
}

/// Pearson correlation over the overlap of two channels.
fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    assert!(n > 1024, "not enough overlap to compare: {n} samples");
    let (a, b) = (&a[..n], &b[..n]);
    let ma = a.iter().map(|v| f64::from(*v)).sum::<f64>() / n as f64;
    let mb = b.iter().map(|v| f64::from(*v)).sum::<f64>() / n as f64;
    let mut num = 0.0;
    let mut da = 0.0;
    let mut db = 0.0;
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

fn compare(path: &Path) -> Vec<f64> {
    let (ours, _rate) = our_decode(path);
    assert!(!ours.is_empty(), "no channels decoded from {path:?}");
    let theirs = ffmpeg_decode(path, ours.len());
    ours.iter()
        .zip(&theirs)
        .map(|(a, b)| correlation(a, b))
        .collect()
}

#[test]
fn adts_fixtures_match_ffmpeg_per_channel() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = fixtures().join("audio");
    let mut checked = 0;
    for name in [
        "aac-adts-mono-44100.aac",
        "aac-adts-mono-48000.aac",
        "aac-adts-stereo-44100.aac",
        "aac-adts-stereo-48000.aac",
        "aac-adts-5.1-44100.aac",
        "aac-adts-5.1-48000.aac",
    ] {
        let path = dir.join(name);
        if !path.exists() {
            eprintln!("SKIP {name}: fixture missing");
            continue;
        }
        let corr = compare(&path);
        println!("{name}: {corr:?}");
        for (ch, c) in corr.iter().enumerate() {
            assert!(
                *c >= 0.999,
                "{name} channel {ch} correlation {c:.6} < 0.999"
            );
        }
        checked += 1;
    }
    assert!(
        checked > 0,
        "no AAC fixtures found: run scripts/gen-fixtures.sh"
    );
}

#[test]
fn mp4_fixtures_match_ffmpeg_per_channel() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = fixtures().join("audio");
    let tmp = std::env::temp_dir().join("ec-aac-mp4-oracle");
    std::fs::create_dir_all(&tmp).expect("temp dir");
    let mut checked = 0;
    for name in [
        "aac-mp4-mono-48000.mp4",
        "aac-mp4-stereo-48000.mp4",
        "aac-mp4-5.1-48000.mp4",
    ] {
        let path = dir.join(name);
        if !path.exists() {
            eprintln!("SKIP {name}: fixture missing");
            continue;
        }
        // The mp4 side of the family is another slice; the elementary stream
        // comes out with a stream copy so this test stays about AAC.
        let adts = tmp.join(format!("{name}.aac"));
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(&path)
            .args(["-c:a", "copy", "-f", "adts"])
            .arg(&adts)
            .output()
            .expect("ffmpeg runs");
        assert!(out.status.success(), "remux failed for {name}");
        let corr = compare(&adts);
        println!("{name}: {corr:?}");
        for (ch, c) in corr.iter().enumerate() {
            assert!(
                *c >= 0.999,
                "{name} channel {ch} correlation {c:.6} < 0.999"
            );
        }
        checked += 1;
    }
    assert!(checked > 0, "no AAC-in-mp4 fixtures found");
}

/// The contract every downmix in this family folds: FL, FR, FC, LFE, BL, BR.
/// Each channel carries its own tone, mapped explicitly onto the layout, so a
/// wrong element order shows up as the wrong tone on the wrong channel.
#[test]
fn five_one_comes_out_in_film_order() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let tmp = std::env::temp_dir().join("ec-aac-order");
    std::fs::create_dir_all(&tmp).expect("temp dir");
    let src = tmp.join("order.aac");
    // The LFE tone stays low: the reference encoder band-limits that channel,
    // and a 1.5 kHz tone would simply not survive the trip.
    let tones = [300.0, 700.0, 1100.0, 60.0, 1900.0, 2300.0];
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-y"]);
    for f in tones {
        cmd.args([
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency={f}:duration=2:sample_rate=48000"),
        ]);
    }
    let out = cmd
        .args([
            "-filter_complex",
            "[0:a][1:a][2:a][3:a][4:a][5:a]join=inputs=6:channel_layout=5.1:\
map=0.0-FL|1.0-FR|2.0-FC|3.0-LFE|4.0-BL|5.0-BR[a]",
            "-map",
            "[a]",
            "-c:a",
            "aac",
            "-b:a",
            "320k",
            "-f",
            "adts",
        ])
        .arg(&src)
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "tone build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (ours, rate) = our_decode(&src);
    assert_eq!(ours.len(), 6, "5.1 decodes to six channels");
    for (ch, plane) in ours.iter().enumerate() {
        let peak = dominant_frequency(plane, rate);
        let want = tones[ch];
        assert!(
            (peak - want).abs() < 40.0,
            "channel {ch} peaks at {peak:.0} Hz, expected {want:.0} Hz: \
             the element order was not remapped to FL, FR, FC, LFE, BL, BR"
        );
    }
}

/// Crude DFT peak: enough to tell six well-separated tones apart.
fn dominant_frequency(samples: &[f32], rate: u32) -> f64 {
    let n = samples.len().min(16384);
    let start = samples.len() / 3;
    let seg = &samples[start..start + n.min(samples.len() - start)];
    let mut best = (0.0f64, 0.0f64);
    let mut f = 40.0;
    while f < 4000.0 {
        let w = 2.0 * std::f64::consts::PI * f / f64::from(rate);
        let (mut re, mut im) = (0.0, 0.0);
        for (i, &v) in seg.iter().enumerate() {
            re += f64::from(v) * (w * i as f64).cos();
            im += f64::from(v) * (w * i as f64).sin();
        }
        let mag = re * re + im * im;
        if mag > best.1 {
            best = (f, mag);
        }
        f += 5.0;
    }
    best.0
}

#[test]
fn asc_round_trips_every_sample_rate() {
    for (index, rate) in ec_aac::SAMPLE_RATES.iter().enumerate() {
        assert_eq!(ec_aac::sf_index_for_rate(*rate), Some(index as u8));
        assert_eq!(ec_aac::sample_rate_for_index(index as u8), *rate);
        for channels in [1u16, 2, 6, 8] {
            let bytes = ec_aac::audio_specific_config_bytes(*rate, channels);
            let cfg = ec_aac::parse_audio_specific_config(&bytes).expect("asc parses");
            assert_eq!(cfg.sample_rate, *rate, "rate round trip at {rate} Hz");
            assert_eq!(cfg.channels, channels, "channels round trip at {rate} Hz");
            assert_eq!(cfg.object_type, ec_aac::AOT_AAC_LC);
        }
    }
    assert_eq!(ec_aac::sf_index_for_rate(37_000), None);
}

/// A refusal is a claim: this one proves the SBR extension really is absent and
/// that the decoder says so instead of quietly claiming the doubled rate.
#[test]
fn sbr_is_reported_not_silently_upsampled() {
    // AOT 5 (SBR), core index 6 (24 kHz), stereo, extension index 3 (48 kHz),
    // then AOT 2 for the core: 00101 0110 0010 0011 00010.
    let asc = [0x2Bu8, 0x11, 0x88];
    let decoder = AacDecoder::with_config_bytes(&asc).expect("HE-AAC config parses");
    assert_eq!(decoder.sbr_support(), ec_aac::SbrSupport::CoreOnly);
    assert_eq!(
        decoder.output_sample_rate(),
        Some(24_000),
        "an HE-AAC stream must report the core rate it actually produces"
    );
    let plain = AacDecoder::with_config_bytes(&ec_aac::audio_specific_config_bytes(48_000, 2))
        .expect("LC config parses");
    assert_eq!(plain.sbr_support(), ec_aac::SbrSupport::NotSignalled);
}

/// The incumbent encoder's per-channel correlation on these exact fixtures,
/// measured by running `rusty_aac` 0.5.0 as a black box through the same
/// ffmpeg decode (2026-08-14). This is the bar the replacement has to clear.
const INCUMBENT_BAR: &[(&str, usize, u32, f64)] = &[
    ("mono", 1, 96, 0.9846),
    ("stereo", 2, 128, 0.9743),
    ("stereo", 2, 192, 0.9864),
    ("5.1", 6, 384, 0.9648),
    ("5.1", 6, 448, 0.9611),
];

fn encode_to_adts(pcm: &[f32], channels: u16, rate: u32, kbps: u32) -> Vec<u8> {
    let mut enc = AacEncoder::new(AacEncoderConfig {
        bitrate_bps: kbps * 1000,
        adts: true,
        ..Default::default()
    });
    enc.push_pcm(pcm, channels, rate).expect("pcm accepted");
    enc.finish();
    let mut out = Vec::new();
    while let Ok(p) = enc.next_packet() {
        out.extend_from_slice(&p.data);
    }
    out
}

/// Correlation at the encoder's own delay, which is one frame of lookahead.
fn best_correlation(source: &[f32], decoded: &[f32]) -> f64 {
    [0usize, ec_aac::FRAME_LEN, 2 * ec_aac::FRAME_LEN]
        .into_iter()
        .filter(|lag| decoded.len() > *lag + 1024)
        .map(|lag| correlation(source, &decoded[lag..]))
        .fold(f64::MIN, f64::max)
}

/// Our encode, through ffmpeg, against the incumbent's own numbers on the same
/// lossless fixtures at the same bitrates.
#[test]
fn encoder_beats_the_incumbent_bar() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = fixtures().join("audio");
    let tmp = std::env::temp_dir().join("ec-aac-bar");
    std::fs::create_dir_all(&tmp).expect("temp dir");
    let mut checked = 0;
    for (name, channels, kbps, bar) in INCUMBENT_BAR {
        let src = dir.join(format!("flac-{name}-48000.flac"));
        if !src.exists() {
            eprintln!("SKIP {name}: fixture missing");
            continue;
        }
        let source = ffmpeg_decode(&src, *channels);
        let mut interleaved = Vec::with_capacity(source[0].len() * channels);
        for i in 0..source[0].len() {
            for plane in &source {
                interleaved.push(plane[i]);
            }
        }
        let adts = encode_to_adts(&interleaved, *channels as u16, 48_000, *kbps);
        let path = tmp.join(format!("{name}-{kbps}k.aac"));
        std::fs::write(&path, &adts).expect("write");
        let rate = adts.len() as f64 * 8.0 * 48_000.0 / source[0].len() as f64 / 1000.0;
        let ours = ffmpeg_decode(&path, *channels);
        let corr: Vec<f64> = source
            .iter()
            .zip(&ours)
            .map(|(a, b)| best_correlation(a, b))
            .collect();
        let worst = corr.iter().copied().fold(f64::MAX, f64::min);
        println!(
            "enc {name} {kbps}k: {rate:.0} kbps actual, worst channel {worst:.4} \
             (incumbent {bar:.4}) per-channel {corr:?}"
        );
        assert!(
            rate <= f64::from(*kbps) * 1.12,
            "{name} {kbps}k overshot its target: {rate:.0} kbps"
        );
        assert!(
            worst >= *bar,
            "{name} {kbps}k worst channel {worst:.4} is under the incumbent's {bar:.4}"
        );
        checked += 1;
    }
    assert!(checked > 0, "no lossless fixtures found");
}

fn tone_pcm(rate: u32, channels: usize, secs: f64) -> Vec<f32> {
    let frames = (f64::from(rate) * secs) as usize;
    let mut pcm = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        let t = i as f64 / f64::from(rate);
        for c in 0..channels {
            let f = 220.0 * (c + 1) as f64;
            let v = (t * f * std::f64::consts::TAU).sin() * 0.35
                + (t * f * 2.5 * std::f64::consts::TAU).sin() * 0.15;
            pcm.push(v as f32);
        }
    }
    pcm
}

#[test]
fn encoder_output_decodes_in_ffmpeg_without_warnings() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let tmp = std::env::temp_dir().join("ec-aac-enc");
    std::fs::create_dir_all(&tmp).expect("temp dir");
    for (channels, kbps) in [(1u16, 96u32), (2, 128), (2, 192), (6, 384)] {
        let rate = 48_000;
        let pcm = tone_pcm(rate, usize::from(channels), 1.5);
        let adts = encode_to_adts(&pcm, channels, rate, kbps);
        assert!(!adts.is_empty(), "{channels}ch encode produced nothing");
        let path = tmp.join(format!("enc-{channels}ch-{kbps}k.aac"));
        std::fs::write(&path, &adts).expect("write");
        let out = Command::new("ffmpeg")
            .args(["-v", "warning", "-i"])
            .arg(&path)
            .args(["-f", "null", "-"])
            .output()
            .expect("ffmpeg runs");
        // ffmpeg says this about every ADTS file, its own included: the
        // container has no duration field. It is not a complaint about us.
        let log = String::from_utf8_lossy(&out.stderr);
        let log: String = log
            .lines()
            .filter(|l| !l.contains("Estimating duration from bitrate"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            out.status.success() && log.trim().is_empty(),
            "ffmpeg complained about our {channels}ch/{kbps}k ADTS:\n{log}"
        );
        // And the sound survives the trip.
        let theirs = ffmpeg_decode(&path, usize::from(channels));
        let ours = deinterleave(&pcm, usize::from(channels));
        for (ch, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            // The encoder's own frame of lookahead delays the output by one
            // frame; line the two up before comparing.
            let corr = correlation(a, &b[ec_aac::FRAME_LEN..]);
            println!("enc {channels}ch {kbps}k channel {ch}: corr {corr:.4}");
            assert!(
                corr >= 0.90,
                "{channels}ch/{kbps}k channel {ch} correlation {corr:.4} < 0.90"
            );
        }
    }
}
