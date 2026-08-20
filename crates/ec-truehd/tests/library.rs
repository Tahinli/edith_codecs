//! Walks a real Blu-ray remux's TrueHD track and parses every access unit --
//! a live discovery over `~/Downloads`, absent files skip loudly, nothing is
//! generated or bundled.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::Command;

use ec_core::{CodecId, Demuxer};
use ec_matroska::MatroskaDemuxer;
use ec_truehd::sync::AccessUnitHeader;

/// The user's own library, not a fixture: a glob over `~/Downloads` for a
/// known 7.1 TrueHD Blu-ray remux.
fn find_file() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let downloads = PathBuf::from(home).join("Downloads");
    for entry in glob_dir(&downloads) {
        let name = entry.file_name()?.to_str()?;
        if name.contains("Book of Dragons") && name.ends_with(".mkv") {
            return Some(entry);
        }
    }
    None
}

/// A one-level-deep, non-recursive-but-good-enough directory walk (the file
/// this test wants sits in a subdirectory of `~/Downloads`) -- no `glob`
/// crate in this family, so a plain `read_dir` walk stands in for one.
fn glob_dir(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(glob_dir(&path));
        } else {
            out.push(path);
        }
    }
    out
}

fn have_ffprobe() -> bool {
    Command::new("ffprobe")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// ffprobe's own packet count for the file's TrueHD stream, by absolute
/// stream index.
fn ffprobe_packet_count(path: &std::path::Path, stream: usize) -> Option<u64> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams"])
        .arg(stream.to_string())
        .args([
            "-count_packets",
            "-show_entries",
            "stream=nb_read_packets",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

#[test]
fn walks_every_truehd_access_unit_of_a_real_7_1_remux() {
    let Some(path) = find_file() else {
        eprintln!("skip: no 'Book of Dragons ... TrueHD ...mkv' under ~/Downloads");
        return;
    };

    let file = File::open(&path).expect("open the file");
    let mut demux = MatroskaDemuxer::new(BufReader::new(file)).expect("open the container");

    let track = demux
        .streams()
        .iter()
        .find(|s| s.params.codec == CodecId::TrueHd)
        .expect("a TrueHD track");
    let stream_index = track.index;
    let audio = track.params.audio().expect("audio parameters");
    assert_eq!(audio.sample_rate, 48_000, "rate");
    assert_eq!(audio.layout.channel_count(), 8, "channels");
    // Matroska's `BitDepth` element is optional and this remux's TrueHD
    // track does not carry one; report what the container states rather
    // than asserting a value it never claimed.
    eprintln!("container-reported bit depth: {:?}", audio.bits_per_sample);

    let mut au_count = 0u64;
    let mut major_syncs = 0u64;
    loop {
        let packet = match demux.next_packet() {
            Ok(p) => p,
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("demux error: {e}"),
        };
        if packet.stream != stream_index {
            continue;
        }
        let header = AccessUnitHeader::parse(&packet.data)
            .unwrap_or_else(|e| panic!("access unit {au_count}: {e}"));
        assert_eq!(
            header.length,
            packet.data.len(),
            "access unit {au_count} claims a length that does not match its packet"
        );
        if header.major_sync.is_some() {
            major_syncs += 1;
        }
        au_count += 1;
    }

    eprintln!("{au_count} access units, {major_syncs} carrying a major sync");
    assert!(au_count > 0, "found no TrueHD access units at all");

    if have_ffprobe() {
        // Absolute demuxer stream index in the file, for ffprobe's own
        // `-select_streams`: the track this family's stream list found.
        let absolute = stream_index as usize;
        if let Some(expected) = ffprobe_packet_count(&path, absolute) {
            assert_eq!(au_count, expected, "AU count vs ffprobe's packet count");
        } else {
            eprintln!("skip: ffprobe packet count unavailable for stream {absolute}");
        }
    } else {
        eprintln!("skip: ffprobe not on PATH, no oracle comparison");
    }
}
