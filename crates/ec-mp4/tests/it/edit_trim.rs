//! The audio edit's duration: an `elst` that plays a track's media for a stated
//! length rather than to its end, written by [`Mp4Muxer::set_edit`].
//!
//! Also the default-output pin: a file muxed without `set_edit` is byte-identical to
//! what the writer produced before the call existed, held by an MD5.

use std::io::Cursor;
use std::path::Path;
use std::process::Command;

use ec_core::{Buf, CodecId, Muxer, Packet, StreamInfo, TimeBase};
use ec_mp4::{Mp4Demuxer, Mp4Muxer};

/// A 48 kHz stereo AAC-LC track's stream description.
fn aac_stream() -> StreamInfo {
    let rate = TimeBase::from_rate(48_000);
    let mut info = StreamInfo::new(0, rate, ec_core::CodecParameters::new(CodecId::Aac));
    if let ec_core::MediaParameters::Audio(a) = &mut info.params.media {
        a.sample_rate = 48_000;
        a.layout = ec_core::ChannelLayout::Stereo;
    }
    info.params.extradata = Some(Buf::copy_from_slice(&[0x11, 0x90])); // AAC-LC 48k stereo ASC
    info
}

/// `frames` AAC frames of 1024 samples each, laid end to end, the way
/// `aac_priming_round_trips_through_an_elst` writes them.
fn aac_packets(frames: i64) -> Vec<Packet> {
    let rate = TimeBase::from_rate(48_000);
    (0..frames)
        .map(|_| {
            let mut packet = Packet::new(0, rate, vec![0u8; 8]);
            packet.duration = Some(1024);
            packet.flags.keyframe = true;
            packet
        })
        .collect()
}

/// The default path: AAC with a priming delay and no edit duration, as the file
/// stood before `set_edit` existed.
fn default_output() -> Vec<u8> {
    let mut muxer = Mp4Muxer::new(Cursor::new(Vec::new())).expect("muxer");
    let stream = muxer.add_stream(aac_stream()).expect("stream");
    muxer.set_audio_delay(stream, 1024).expect("declare delay");
    for packet in aac_packets(47) {
        muxer.write_packet(&packet).expect("packet");
    }
    muxer.finish().expect("finish");
    muxer.into_inner().into_inner()
}

/// The MD5 of `bytes`, from the `md5sum` tool. A missing tool skips the check.
fn md5_of(bytes: &[u8]) -> Option<String> {
    let dir = std::env::temp_dir().join("ec-mp4-edit-trim");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("md5-{}.mp4", std::process::id()));
    std::fs::write(&path, bytes).ok()?;
    let out = Command::new("md5sum").arg(&path).output().ok()?;
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string)
}

/// Pinned before `set_edit` was added: the default output of the AAC priming
/// case must not change by a byte.
const DEFAULT_AAC_PRIMING_MD5: &str = "2ba0fd44a7dfa3b69cb60d7aceb71753";

#[test]
fn default_aac_priming_output_is_byte_identical() {
    let bytes = default_output();
    match md5_of(&bytes) {
        Some(sum) => assert_eq!(sum, DEFAULT_AAC_PRIMING_MD5, "default output changed"),
        None => eprintln!("skip: md5sum is not installed"),
    }
}

/// The big-endian `u32` at `offset` into the payload of the first `kind` box in
/// `bytes`, where a full box's `version/flags` word is the first four bytes.
fn payload_u32(bytes: &[u8], kind: &[u8; 4], offset: usize) -> Option<u32> {
    let at = bytes.windows(4).position(|w| w == kind)? + 4;
    let raw = bytes.get(at + offset..at + offset + 4)?;
    Some(u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

/// `key=value` lines from `ffprobe`, keyed by name. `None` where ffprobe is not
/// installed, which skips the check.
fn ffprobe_fields(path: &Path, select: &str, entries: &str) -> Option<Vec<(String, String)>> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            select,
            "-show_entries",
            entries,
        ])
        .args(["-of", "default=nw=1"])
        .arg(path)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    Some(
        text.lines()
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
}

fn field(fields: &[(String, String)], key: &str) -> f64 {
    fields
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(f64::NAN)
}

#[test]
fn set_edit_trims_the_audio_edit_to_its_stated_length() {
    // 47 AAC frames of 1024 samples is 48128 samples of media; the edit plays
    // 40000 of them after the 1024-sample priming, a picture that ends early.
    const PRIMING: u32 = 1024;
    const PICTURE: u32 = 40_000;
    let mut muxer = Mp4Muxer::new(Cursor::new(Vec::new())).expect("muxer");
    let stream = muxer.add_stream(aac_stream()).expect("stream");
    muxer.set_edit(stream, PRIMING, PICTURE).expect("edit set");
    for packet in aac_packets(47) {
        muxer.write_packet(&packet).expect("packet");
    }
    muxer.finish().expect("finish");
    let bytes = muxer.into_inner().into_inner();

    // The movie clock is 1000 Hz, so the picture's length is 833.33 ms, rounded up
    // to 834. The track's `tkhd` and the movie's `mvhd` state the same length.
    let expected_movie = 834;
    assert_eq!(
        payload_u32(&bytes, b"tkhd", 20),
        Some(expected_movie),
        "tkhd"
    );
    assert_eq!(
        payload_u32(&bytes, b"mvhd", 16),
        Some(expected_movie),
        "mvhd"
    );
    assert_eq!(payload_u32(&bytes, b"mvhd", 12), Some(1000), "movie clock");

    // This crate reads the edit back as it was written.
    let demuxer = Mp4Demuxer::new(Cursor::new(bytes.clone())).expect("reopens");
    let tracks = demuxer.tracks();
    let edits = &tracks[0].edits;
    assert_eq!(edits.len(), 1, "one edit entry");
    assert_eq!(edits[0].segment_duration, u64::from(expected_movie));
    assert_eq!(edits[0].media_time, i64::from(PRIMING));
    assert_eq!(tracks[0].media_time, Some(i64::from(PRIMING)));

    // ffprobe reads the stream as the picture's length, starting at zero, to
    // within one AAC frame.
    let path = std::env::temp_dir()
        .join("ec-mp4-edit-trim")
        .join(format!("trim-{}.mp4", std::process::id()));
    std::fs::create_dir_all(path.parent().expect("dir")).expect("work dir");
    std::fs::write(&path, &bytes).expect("write");
    let stream_fields = ffprobe_fields(&path, "a:0", "stream=start_time,duration");
    if let Some(fields) = stream_fields {
        let frame = 1024.0 / 48_000.0;
        let want = f64::from(PICTURE) / 48_000.0;
        let duration = field(&fields, "duration");
        let start = field(&fields, "start_time");
        eprintln!("ffprobe a:0 start_time {start} duration {duration} (picture {want})");
        assert!(
            (duration - want).abs() <= frame,
            "stream duration {duration} against the picture's {want}"
        );
        assert!(start.abs() < 1e-6, "start_time {start} is not zero");
    } else {
        eprintln!("skip ffprobe: not installed");
    }
    let _ = std::fs::remove_file(&path);
}
