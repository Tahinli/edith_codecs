//! E-AC-3 dependent substreams, checked against ffmpeg's own decode of the same
//! spliced stream: its channel placement (the replace-or-add rule of Table E2.5)
//! and its channel order, which ffprobe names in `channel_layout`.
//!
//! The fixtures come from `scripts/gen-ac3dep-fixtures.sh`. Missing fixtures or
//! a missing ffmpeg skip the comparison, as in `decode_matrix.rs`, unless
//! `EC_REQUIRE_FIXTURES=1`, which makes an absent fixture a failure.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_ac3::{Ac3Decoder, Error, Options, eac3, frame_size};
use ec_core::{ChannelLayout, ChannelPosition};

/// Per-channel correlation floor and RMS ceiling, as in `decode_matrix.rs`.
const MIN_CORR: f64 = 0.999;
const MAX_RMS: f64 = 1e-3;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio")
}

/// Whether every path is present. A missing one skips the test, or fails it
/// under `EC_REQUIRE_FIXTURES=1`.
fn present(name: &str, paths: &[PathBuf]) -> bool {
    let missing: Vec<_> = paths.iter().filter(|p| !p.exists()).collect();
    if missing.is_empty() {
        return true;
    }
    assert!(
        std::env::var("EC_REQUIRE_FIXTURES").as_deref() != Ok("1"),
        "{name}: fixture missing: {missing:?}"
    );
    eprintln!("skipping {name}: fixture missing: {missing:?}");
    false
}

/// Whether ffmpeg and ffprobe run. Missing tools skip, or fail under
/// `EC_REQUIRE_FIXTURES=1`.
fn have_ffmpeg(name: &str) -> bool {
    let ok = |tool: &str| {
        Command::new(tool)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    };
    if ok("ffmpeg") && ok("ffprobe") {
        return true;
    }
    assert!(
        std::env::var("EC_REQUIRE_FIXTURES").as_deref() != Ok("1"),
        "{name}: ffmpeg and ffprobe are required"
    );
    eprintln!("skipping {name}: ffmpeg or ffprobe missing");
    false
}

/// ffprobe's `channel_layout` name for the first audio stream of `path`.
fn ffprobe_layout(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-f", "eac3", "-select_streams", "a:0"])
        .args(["-show_entries", "stream=channel_layout", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// ffmpeg's decode of `path` as interleaved `f32`, in ffmpeg's channel order.
fn reference(path: &Path, channels: usize) -> Vec<Vec<f32>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-f", "eac3", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-"])
        .output()
        .expect("ffmpeg");
    let all: Vec<f32> = out
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    (0..channels)
        .map(|ch| all[ch..].iter().step_by(channels).copied().collect())
        .collect()
}

/// Split a raw E-AC-3 stream into packets: an independent syncframe and the
/// dependent substreams that follow it.
fn packets(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut pos = 0;
    while pos + 6 <= data.len() {
        let Ok(size) = frame_size(&data[pos..]) else {
            break;
        };
        let independent = eac3::bsi::parse(&data[pos + 2..])
            .is_ok_and(|h| h.strmtyp == eac3::StreamType::Independent);
        if independent {
            starts.push(pos);
        }
        pos += size;
    }
    starts
        .iter()
        .enumerate()
        .map(|(i, &s)| &data[s..starts.get(i + 1).copied().unwrap_or(data.len())])
        .collect()
}

/// Our decode of every packet: interleaved samples, channels and the layout.
fn decode(data: &[u8]) -> (Vec<f32>, usize, ChannelLayout) {
    let mut decoder = Ac3Decoder::with_options(Options {
        dither: false,
        ..Options::default()
    });
    let (mut out, mut channels, mut layout) = (Vec::new(), 0, ChannelLayout::Mono);
    for packet in packets(data) {
        let frame = decoder
            .decode_frame(packet)
            .unwrap_or_else(|e| panic!("packet decode: {e}"));
        channels = frame.channels();
        layout = frame.layout.clone();
        out.extend(
            frame.data[0]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])),
        );
    }
    (out, channels, layout)
}

fn correlation(a: &[f32], b: &[f32]) -> (f64, f64, f64) {
    let n = a.len().min(b.len());
    let (mut sa, mut sb, mut saa, mut sbb, mut sab, mut sd) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    let mut max_abs = 0.0f64;
    for i in 0..n {
        let (x, y) = (f64::from(a[i]), f64::from(b[i]));
        sa += x;
        sb += y;
        saa += x * x;
        sbb += y * y;
        sab += x * y;
        sd += (x - y) * (x - y);
        max_abs = max_abs.max((x - y).abs());
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
    (corr, (sd / n).sqrt(), max_abs)
}

/// The per-channel comparison of our decode of `name` against ffmpeg's decode of
/// the same file. The layout name is ffprobe's, so a wrong channel order fails
/// here even when every channel holds the right signal.
///
/// With `side_from_core`, channels 6 and 7 (the core's Ls and Rs in a 7.1 layout)
/// are compared against ffmpeg's decode of the 5.1 source instead. ffmpeg 8.1
/// does not place them correctly once a dependent substream is appended to a
/// core with a side pair: its side-left slot is silent and its side-right slot
/// repeats the dependent's left channel. The core decode is ffmpeg's own
/// unchanged 5.1 path, so it is the reference that ffmpeg itself gets right.
fn check_against_ffmpeg(name: &str, ffprobe_name: &str, layout: &ChannelLayout, side_from_core: bool) {
    let stream = fixtures().join(name);
    let core = fixtures().join("ac3dep-src-5.1.eac3");
    let mut paths = vec![stream.clone()];
    if side_from_core {
        paths.push(core.clone());
    }
    if !present(name, &paths) || !have_ffmpeg(name) {
        return;
    }
    assert_eq!(ffprobe_layout(&stream), ffprobe_name, "{name}: ffprobe channel_layout");
    let data = std::fs::read(&stream).expect("fixture");
    let (ours, channels, ours_layout) = decode(&data);
    assert_eq!(&ours_layout, layout, "{name}: layout");
    assert_eq!(channels, layout.channel_count(), "{name}: channels");

    let theirs = reference(&stream, channels);
    let core_side = side_from_core.then(|| reference(&core, 6));
    let frames = ours.len() / channels;
    assert_eq!(frames, theirs[0].len(), "{name}: frame count vs ffmpeg");

    println!("{name} ({ffprobe_name}): per-channel correlation / RMS delta / max abs error");
    for (ch, from_ffmpeg) in theirs.iter().enumerate() {
        let (expect, note) = match (&core_side, ch) {
            (Some(c), 6) => (&c[4], " (core Ls, from the 5.1 decode)"),
            (Some(c), 7) => (&c[5], " (core Rs, from the 5.1 decode)"),
            _ => (from_ffmpeg, ""),
        };
        let a: Vec<f32> = ours[ch..].iter().step_by(channels).copied().collect();
        let (corr, rms, max_abs) = correlation(&a, expect);
        println!("  ch{ch}: {corr:.6} / {rms:.3e} / {max_abs:.3e}{note}");
        assert!(corr >= MIN_CORR, "{name} ch{ch}: correlation {corr:.6}");
        assert!(rms <= MAX_RMS, "{name} ch{ch}: RMS delta {rms:.8}");
    }
}

#[test]
fn seven_one_matches_ffmpeg() {
    // Lrs/Rrs (BL/BR) are added to the core's side pair: 8 channels in 7.1 order.
    check_against_ffmpeg("ac3dep-7.1-48000.eac3", "7.1", &ChannelLayout::Surround7_1, true);
}

#[test]
fn seven_one_wide_matches_ffmpeg() {
    // Lc/Rc (FLC/FRC) are added; canonical order puts them before the side pair.
    let wide = ChannelLayout::Custom(vec![
        ChannelPosition::FrontLeft,
        ChannelPosition::FrontRight,
        ChannelPosition::FrontCenter,
        ChannelPosition::Lfe,
        ChannelPosition::FrontLeftOfCenter,
        ChannelPosition::FrontRightOfCenter,
        ChannelPosition::SideLeft,
        ChannelPosition::SideRight,
    ]);
    check_against_ffmpeg("ac3dep-7.1wide-48000.eac3", "7.1(wide-side)", &wide, true);
}

#[test]
fn side_pair_replacement_matches_ffmpeg() {
    // Ls/Rs replace the core's side pair: 6 channels, not 8.
    let side = ChannelLayout::Custom(vec![
        ChannelPosition::FrontLeft,
        ChannelPosition::FrontRight,
        ChannelPosition::FrontCenter,
        ChannelPosition::Lfe,
        ChannelPosition::SideLeft,
        ChannelPosition::SideRight,
    ]);
    check_against_ffmpeg("ac3dep-repl-48000.eac3", "5.1(side)", &side, false);
}

#[test]
fn side_pair_replacement_carries_the_dependent_signal() {
    // The replaced pair is the dependent stereo source (880 Hz and 1320 Hz
    // sines), not the core's: compare the side channels with ffmpeg's decode
    // of that source on its own.
    let dir = fixtures();
    let repl = dir.join("ac3dep-repl-48000.eac3");
    let src20 = dir.join("ac3dep-src-2.0.eac3");
    if !present("ac3dep-repl-48000.eac3", &[repl.clone(), src20.clone()]) || !have_ffmpeg("replacement signal") {
        return;
    }
    let data = std::fs::read(&repl).expect("fixture");
    let (ours, channels, _) = decode(&data);
    assert_eq!(channels, 6, "replacement keeps the 5.1 channel count");
    let source = reference(&src20, 2);
    for (side, src) in [(4usize, 0usize), (5, 1)] {
        let a: Vec<f32> = ours[side..].iter().step_by(channels).copied().collect();
        let (corr, rms, _) = correlation(&a, &source[src]);
        println!("side channel {side} vs dependent source ch{src}: {corr:.6} / {rms:.3e}");
        assert!(corr >= MIN_CORR, "side ch{side}: correlation {corr:.6} against the dependent source");
    }
}

#[test]
fn dependent_without_an_independent_is_named_not_decoded() {
    let dir = fixtures();
    let stream = dir.join("ac3dep-7.1-48000.eac3");
    if !present("ac3dep-7.1-48000.eac3", std::slice::from_ref(&stream)) {
        return;
    }
    let data = std::fs::read(&stream).expect("fixture");
    let packet = packets(&data)[0];
    // The dependent frame alone, as a first substream.
    let size = frame_size(packet).expect("frame size");
    let dep = &packet[size..];
    let mut decoder = Ac3Decoder::new();
    assert!(matches!(
        decoder.decode_frame(dep),
        Err(Error::Unsupported { .. })
    ));
}

#[test]
fn truncated_dependent_substream_is_an_error_not_a_panic() {
    let dir = fixtures();
    let stream = dir.join("ac3dep-7.1-48000.eac3");
    if !present("ac3dep-7.1-48000.eac3", std::slice::from_ref(&stream)) {
        return;
    }
    let data = std::fs::read(&stream).expect("fixture");
    let packet = packets(&data)[0];
    for cut in [1, 2, 8, 100, 500] {
        let mut decoder = Ac3Decoder::new();
        assert!(decoder.decode_frame(&packet[..packet.len() - cut]).is_err());
    }
}

#[test]
fn a_second_independent_substream_in_the_packet_is_still_refused() {
    let dir = fixtures();
    let src20 = dir.join("ac3dep-src-2.0.eac3");
    if !present("ac3dep-src-2.0.eac3", std::slice::from_ref(&src20)) {
        return;
    }
    let data = std::fs::read(&src20).expect("fixture");
    let first = packets(&data)[0];
    let mut two = first.to_vec();
    two.extend_from_slice(first);
    let mut decoder = Ac3Decoder::new();
    assert!(matches!(
        decoder.decode_frame(&two),
        Err(Error::Unsupported { .. })
    ));
}
