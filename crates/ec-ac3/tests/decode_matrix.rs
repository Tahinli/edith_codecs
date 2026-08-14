//! Fixture matrix: every generated AC-3 and E-AC-3 fixture decoded and
//! compared against ffmpeg, plus the header surface a container reads and the
//! no-panic contract on damaged input.
//!
//! ffmpeg is test tooling — it is driven through `std::process` so the crate
//! itself never gains a media dependency. Missing fixtures or a missing ffmpeg
//! skip the comparison rather than fail it; `scripts/gen-fixtures.sh` writes
//! the fixtures.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_ac3::{Ac3Decoder, Downmix, Options, Syntax, bsi, eac3, syncinfo};

/// Per-channel correlation floor (rubric: audio-ac3 MUST).
const MIN_CORR: f64 = 0.999;
/// RMS delta ceiling on the fixture matrix, where both decoders see the same
/// synthetic material.
const MAX_RMS: f64 = 1e-3;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/audio")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("../../fixtures/audio"))
}

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// ffmpeg's decode of `path` as interleaved `f32`.
fn reference(path: &Path) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-"])
        .output()
        .expect("ffmpeg");
    out.stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Our decode of a raw elementary stream, and the channel count.
fn decode(path: &Path, options: Options) -> (Vec<f32>, usize) {
    let data = std::fs::read(path).expect("fixture");
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
            frame.data[0]
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])),
        );
        pos += size;
    }
    (out, channels)
}

fn correlation(a: &[f32], b: &[f32]) -> (f64, f64) {
    let n = a.len().min(b.len());
    let (mut sa, mut sb, mut saa, mut sbb, mut sab, mut sd) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for i in 0..n {
        let (x, y) = (f64::from(a[i]), f64::from(b[i]));
        sa += x;
        sb += y;
        saa += x * x;
        sbb += y * y;
        sab += x * y;
        sd += (x - y) * (x - y);
    }
    let n = n as f64;
    let cov = sab / n - (sa / n) * (sb / n);
    let va = saa / n - (sa / n).powi(2);
    let vb = sbb / n - (sb / n).powi(2);
    let corr = if va <= 0.0 || vb <= 0.0 {
        1.0
    } else {
        cov / (va * vb).sqrt()
    };
    (corr, (sd / n).sqrt())
}

#[test]
fn fixture_matrix_matches_ffmpeg() {
    let dir = fixtures();
    if !dir.exists() || !have_ffmpeg() {
        eprintln!("skipping: no fixtures or no ffmpeg");
        return;
    }
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("fixtures dir") {
        let path = entry.expect("entry").path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !(name.starts_with("ac3-") || name.starts_with("eac3-")) {
            continue;
        }
        // Dither is "any reasonably random sequence" (§7.3.4), so two
        // conformant decoders differ by exactly that noise. Comparing with it
        // off is what isolates everything else; the with-dither run below
        // bounds how much noise it adds.
        let options = Options {
            dither: false,
            ..Options::default()
        };
        let (ours, channels) = decode(&path, options);
        let theirs = reference(&path);
        assert!(channels > 0, "{name}: no channels");
        assert_eq!(
            ours.len(),
            theirs.len(),
            "{name}: {} samples vs ffmpeg's {}",
            ours.len(),
            theirs.len()
        );
        for ch in 0..channels {
            let a: Vec<f32> = ours[ch..].iter().step_by(channels).copied().collect();
            let b: Vec<f32> = theirs[ch..].iter().step_by(channels).copied().collect();
            let (corr, rms) = correlation(&a, &b);
            assert!(corr >= MIN_CORR, "{name} ch{ch}: correlation {corr:.6}");
            assert!(rms <= MAX_RMS, "{name} ch{ch}: RMS delta {rms:.8}");
        }
        checked += 1;
    }
    assert!(checked >= 12, "only {checked} fixtures checked");
}

#[test]
fn dither_only_adds_noise_below_the_signal() {
    let path = fixtures().join("ac3-5.1-48000.ac3");
    if !path.exists() {
        return;
    }
    let (with, channels) = decode(&path, Options::default());
    let (without, _) = decode(
        &path,
        Options {
            dither: false,
            ..Options::default()
        },
    );
    assert_eq!(with.len(), without.len());
    let (corr, rms) = correlation(&with, &without);
    assert!(channels == 6 && corr > 0.999, "correlation {corr:.6}");
    assert!(rms < 1e-3, "dither RMS {rms:.8}");
}

#[test]
fn headers_state_what_the_container_needs() {
    let path = fixtures().join("ac3-5.1-48000.ac3");
    if !path.exists() {
        return;
    }
    let data = std::fs::read(&path).expect("fixture");
    // The exact call shape a container makes: syncinfo from the frame start,
    // bsi from five bytes in.
    let sync = syncinfo::parse(&data).expect("syncinfo");
    assert_eq!(sync.sample_rate, 48_000);
    assert_eq!(sync.frame_size % 2, 0);
    let bsi = bsi::parse(&data[5..]).expect("bsi");
    assert_eq!(bsi.nfchans, 5);
    assert!(bsi.lfeon);
    assert_eq!(bsi.channels, 6);
    assert!((1..=31).contains(&bsi.dialnorm));

    let eac3_path = fixtures().join("eac3-5.1-48000.eac3");
    let data = std::fs::read(&eac3_path).expect("fixture");
    let hdr = eac3::bsi::parse(&data[2..]).expect("eac3 bsi");
    assert_eq!(hdr.sample_rate, 48_000);
    assert_eq!(hdr.nfchans, 5);
    assert!(hdr.bsi.lfeon);
    assert_eq!(
        hdr.frame_size,
        ec_ac3::frame_size(&data).expect("frame size")
    );
}

#[test]
fn frame_info_surfaces_the_metadata_the_rubric_asks_for() {
    let path = fixtures().join("ac3-5.1-48000.ac3");
    if !path.exists() {
        return;
    }
    let data = std::fs::read(&path).expect("fixture");
    let size = ec_ac3::frame_size(&data).expect("frame size");
    let mut decoder = Ac3Decoder::new();
    decoder.decode_frame(&data[..size]).expect("decode");
    let info = decoder.frame_info().expect("frame info");
    assert_eq!(info.syntax, Syntax::Ac3);
    assert_eq!(info.sample_rate, 48_000);
    assert!((1..=31).contains(&info.dialnorm));
    // ffmpeg's encoder writes cmixlev 1 (-4.5 dB) and surmixlev 1 (-6 dB).
    assert_eq!(info.center_mix_level, Some(0.595));
    assert_eq!(info.surround_mix_level, Some(0.5));
    assert_eq!(info.samples, 1536);
}

#[test]
fn stereo_downmix_folds_five_one_without_clipping() {
    let path = fixtures().join("ac3-5.1-48000.ac3");
    if !path.exists() {
        return;
    }
    let (stereo, channels) = decode(
        &path,
        Options {
            downmix: Downmix::Stereo,
            ..Options::default()
        },
    );
    assert_eq!(channels, 2);
    let (native, _) = decode(&path, Options::default());
    assert_eq!(stereo.len() * 3, native.len());
    assert!(stereo.iter().all(|v| v.abs() <= 1.0), "downmix clipped");
    assert!(stereo.iter().any(|v| v.abs() > 0.01), "downmix is silent");

    let (mono, channels) = decode(
        &path,
        Options {
            downmix: Downmix::Mono,
            ..Options::default()
        },
    );
    assert_eq!(channels, 1);
    assert_eq!(mono.len() * 6, native.len());
}

#[test]
fn damaged_input_never_panics() {
    let path = fixtures().join("ac3-stereo-48000.ac3");
    if !path.exists() {
        return;
    }
    let data = std::fs::read(&path).expect("fixture");
    let size = ec_ac3::frame_size(&data).expect("frame size");
    let frame = &data[..size];
    let mut decoder = Ac3Decoder::new();

    // Every truncation.
    for cut in 0..frame.len() {
        let _ = decoder.decode_frame(&frame[..cut]);
    }
    // Every single-byte corruption at a stride that still hits every field
    // class: header, side information, exponents, mantissas.
    for pos in (0..frame.len()).step_by(7) {
        let mut broken = frame.to_vec();
        broken[pos] ^= 0xA5;
        let _ = decoder.decode_frame(&broken);
    }
    // Garbage that happens to start with a sync word.
    let mut noise = vec![0x0B, 0x77];
    let mut state = 12345u32;
    for _ in 0..4096 {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12345);
        noise.push((state >> 16) as u8);
    }
    let _ = decoder.decode_frame(&noise);
}
