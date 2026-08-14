//! ALAC is lossless, so the oracle is exact: every fixture must decode to the
//! same bytes ffmpeg's decoder produces, sample for sample, in the same channel
//! order.
//!
//! Fixtures come from `scripts/gen-fixtures.sh`; a missing one skips its row
//! rather than failing, so a checkout without generated fixtures still builds.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_alac::{AlacDecoder, MagicCookie};
use ec_core::registry::{CodecId, Demuxer};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio")
}

/// Every sample of `path`'s ALAC track, interleaved, as this crate decodes it.
fn decode(path: &Path) -> (MagicCookie, Vec<i32>) {
    let mut demuxer =
        ec_mp4::Mp4Demuxer::new(BufReader::new(File::open(path).expect("open"))).expect("mp4");
    let stream = demuxer
        .streams()
        .iter()
        .find(|s| s.params.codec == CodecId::Alac)
        .expect("an ALAC track")
        .clone();
    let mut decoder = AlacDecoder::from_parameters(stream.params.clone()).expect("cookie");
    let cookie = *decoder.cookie();
    let mut out = Vec::new();
    loop {
        match demuxer.next_packet() {
            Ok(packet) if packet.stream != stream.index => continue,
            Ok(packet) => out.extend_from_slice(decoder.decode(&packet.data).expect("decode")),
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("{}: {e}", path.display()),
        }
    }
    (cookie, out)
}

/// ffmpeg's own decode of the same file, in the raw format `fmt` names.
fn ffmpeg(path: &Path, fmt: &str) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(path)
        .args(["-f", fmt, "-"])
        .output()
        .expect("ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

#[test]
fn every_alac_fixture_decodes_bit_exactly() {
    let mut checked = 0;
    for name in [
        "alac-mp4-mono-44100.m4a",
        "alac-mp4-mono-48000.m4a",
        "alac-mp4-stereo-44100.m4a",
        "alac-mp4-stereo-48000.m4a",
        "alac-mp4-5.1-44100.m4a",
        "alac-mp4-5.1-48000.m4a",
        "alac24-mp4-stereo-48000.m4a",
        "alac24-mp4-5.1-48000.m4a",
    ] {
        let path = fixtures().join(name);
        if !path.exists() {
            continue;
        }
        let (cookie, got) = decode(&path);
        let (fmt, width) = match cookie.bit_depth <= 16 {
            true => ("s16le", 2),
            false => ("s32le", 4),
        };
        let want = ffmpeg(&path, fmt);
        assert_eq!(
            got.len(),
            want.len() / width,
            "{name}: {} samples decoded, ffmpeg says {}",
            got.len(),
            want.len() / width
        );
        let mine: Vec<u8> = got
            .iter()
            .flat_map(|&s| match width {
                2 => (s as i16).to_le_bytes().to_vec(),
                _ => s.to_le_bytes().to_vec(),
            })
            .collect();
        let bad = mine
            .chunks_exact(width)
            .zip(want.chunks_exact(width))
            .position(|(a, b)| a != b);
        assert_eq!(bad, None, "{name}: first differing sample");
        checked += 1;
    }
    assert!(checked > 0, "no ALAC fixtures found — run gen-fixtures.sh");
    eprintln!("alac oracle: {checked} fixtures bit-exact vs ffmpeg");
}

/// A truncated frame is an error, not a panic — the fuzz floor in miniature.
#[test]
fn truncated_frames_are_refused_without_panicking() {
    let path = fixtures().join("alac-mp4-stereo-44100.m4a");
    if !path.exists() {
        return;
    }
    let mut demuxer =
        ec_mp4::Mp4Demuxer::new(BufReader::new(File::open(&path).expect("open"))).expect("mp4");
    let stream = demuxer.streams()[0].clone();
    let mut decoder = AlacDecoder::from_parameters(stream.params).expect("cookie");
    let packet = demuxer.next_packet().expect("a packet");
    for len in 0..packet.data.len().min(64) {
        let _ = decoder.decode(&packet.data[..len]);
    }
    // And it still decodes the whole frame afterwards.
    assert!(
        !decoder
            .decode(&packet.data)
            .expect("whole frame")
            .is_empty()
    );
}
