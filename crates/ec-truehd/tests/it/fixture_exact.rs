//! Sample-for-sample equality with ffmpeg's TrueHD decoder on the committed
//! fixtures in `crates/ec-truehd/fixtures/`.
//!
//! The committed fixtures are made with ffmpeg's own TrueHD encoder
//! (`-c:a truehd -strict -2`): 2.0 and 5.1 at 16-bit (s16p input) and 24-bit
//! (s32p input), 48 kHz. The file name carries the channel count: `2ch` or `51`.
//!
//! The 8-channel (7.1) case is not committed: ffmpeg's encoder refuses 7.1, and
//! the only real 7.1 TrueHD source is a copyrighted remux. It is tested by
//! [`remux_71_is_bit_exact_against_ffmpeg`], which reads the remux from its
//! local path (env `EC_TRUEHD_71_REMUX`, or the `truehd_71_remux` key of
//! `real_library.toml`) and skips loudly when it is absent.
//!
//! Each file is decoded here and by ffmpeg (`-f s32le`), and every sample must
//! be equal. Lossless decoding has no tolerance. ffmpeg is test tooling and is
//! driven through `std::process`; without it the test skips loudly.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::{CodecId, Demuxer};
use ec_matroska::MatroskaDemuxer;
use ec_truehd::TrueHdDecoder;
use crate::common::have_ffmpeg;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn s32le(bytes: &[u8]) -> Vec<i32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| i32::from_le_bytes(*c))
        .collect()
}

/// Channel count from the file name: `2ch` or `51`.
fn expected_channels(name: &str) -> usize {
    if name.contains("2ch") {
        2
    } else if name.contains("51") {
        6
    } else {
        panic!("fixture {name}: no channel count in the name")
    }
}

/// Decodes every access unit of the file's TrueHD track: (stream index,
/// interleaved S32 samples, channels, sample rate).
fn decode_ours(path: &Path) -> (u32, Vec<i32>, usize, u32) {
    let mut demux = MatroskaDemuxer::new(BufReader::new(File::open(path).unwrap())).unwrap();
    let stream = demux
        .streams()
        .iter()
        .find(|s| s.params.codec == CodecId::TrueHd)
        .expect("a TrueHD track")
        .index;
    let mut decoder = TrueHdDecoder::new();
    let mut samples = Vec::new();
    let mut channels = 0;
    let mut rate = 0;
    loop {
        let packet = match demux.next_packet() {
            Ok(p) => p,
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("{}: demux: {e}", path.display()),
        };
        if packet.stream != stream {
            continue;
        }
        let frame = decoder
            .decode_access_unit(&packet.data)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        if let Some(f) = frame {
            channels = f.channels();
            rate = f.rate;
            samples.extend(s32le(&f.data[0]));
        }
    }
    let stats = decoder.check_stats();
    assert_eq!(stats.lossless_check_failures, 0, "{}: {stats:?}", path.display());
    assert_eq!(stats.parity_failures, 0, "{}: {stats:?}", path.display());
    assert_eq!(stats.restart_crc_failures, 0, "{}: {stats:?}", path.display());
    assert_eq!(stats.length_mismatches, 0, "{}: {stats:?}", path.display());
    (stream, samples, channels, rate)
}

/// ffmpeg's decode of track `stream` as interleaved S32 little-endian.
fn decode_ffmpeg(path: &Path, stream: u32) -> Vec<i32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-map", &format!("0:{stream}"), "-f", "s32le", "-"])
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    s32le(&out.stdout)
}

#[test]
fn fixtures_are_bit_exact_against_ffmpeg() {
    let dir = fixtures();
    if !have_ffmpeg() {
        eprintln!("skip: ffmpeg not on PATH");
        return;
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("fixtures dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "mkv"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no TrueHD fixtures in {}", dir.display());

    for path in &paths {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let want_channels = expected_channels(&name);
        let (stream, ours, channels, rate) = decode_ours(path);
        let reference = decode_ffmpeg(path, stream);

        assert_eq!(channels, want_channels, "{name}: channels");
        assert_eq!(rate, 48_000, "{name}: rate");
        assert!(!ours.is_empty(), "{name}: no samples decoded");
        assert_eq!(
            ours.len(),
            reference.len(),
            "{name}: sample count vs ffmpeg"
        );
        let mismatch = ours.iter().zip(&reference).position(|(a, b)| a != b);
        assert_eq!(
            mismatch, None,
            "{name}: first mismatch at sample {mismatch:?} (channel {:?})",
            mismatch.map(|i| i % channels)
        );
        eprintln!(
            "{name}: {channels} ch, {} frames, {} samples, bit-exact",
            ours.len() / channels,
            ours.len()
        );
    }
}

/// The local 7.1 remux: env `EC_TRUEHD_71_REMUX`, else the `truehd_71_remux`
/// key of `real_library.toml` (env `EC_REAL_LIBRARY`, default under
/// `~/Documents/Code/Rust/edith`).
fn remux_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("EC_TRUEHD_71_REMUX") {
        return Some(PathBuf::from(p));
    }
    let toml = std::env::var("EC_REAL_LIBRARY").map(PathBuf::from).unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        PathBuf::from(home).join("Documents/Code/Rust/edith/real_library.toml")
    });
    let text = std::fs::read_to_string(toml).ok()?;
    text.lines().find_map(|line| {
        let (key, value) = line.trim().split_once('=')?;
        if key.trim() != "truehd_71_remux" {
            return None;
        }
        Some(PathBuf::from(value.trim().strip_prefix('"')?.strip_suffix('"')?))
    })
}

/// The first 2 s of a real 7.1 TrueHD remux, decoded from the start of the
/// file and compared sample-for-sample with ffmpeg's decode of the same span.
#[test]
fn remux_71_is_bit_exact_against_ffmpeg() {
    let Some(path) = remux_path() else {
        eprintln!("skip: no 7.1 remux (set EC_TRUEHD_71_REMUX or truehd_71_remux in real_library.toml)");
        return;
    };
    if !path.exists() {
        eprintln!("skip: 7.1 remux {} not on disk", path.display());
        return;
    }
    if !have_ffmpeg() {
        eprintln!("skip: ffmpeg not on PATH");
        return;
    }
    const SECONDS: usize = 2;
    let (stream, ours, channels, rate) = decode_ours(&path);
    assert_eq!(channels, 8, "remux channels");
    assert_eq!(rate, 48_000, "remux rate");
    let want = SECONDS * 48_000 * channels;
    assert!(ours.len() >= want, "only {} samples decoded", ours.len());
    let ours = &ours[..want];

    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(&path)
        .args(["-map", &format!("0:{stream}"), "-t", &SECONDS.to_string(), "-f", "s32le", "-"])
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    let reference = s32le(&out.stdout);
    assert!(reference.len() >= want, "ffmpeg gave {} samples", reference.len());
    let reference = &reference[..want];

    let mismatch = ours.iter().zip(reference).position(|(a, b)| a != b);
    assert_eq!(
        mismatch,
        None,
        "first mismatch at sample {mismatch:?} (channel {:?})",
        mismatch.map(|i| i % channels)
    );
    eprintln!("7.1 remux: {channels} ch, {SECONDS} s, {want} samples, bit-exact");
}
