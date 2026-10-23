//! Regression tests for what the fuzz targets found, and for the arithmetic and
//! allocation paths they reach. Each input is built here, inline. Every test asserts
//! the outcome (an error, or a value), not only the absence of a panic: the same
//! inputs wrap silently in release, so a test that only checks "no panic" would pass
//! there with the bug still in place.

use std::io::Cursor;

use ec_core::{Demuxer, Error, SeekMode, Timestamp};
use ec_matroska::MatroskaDemuxer;
use ec_matroska::fuzz_api;

// ----------------------------------------------------------------- building ---

/// An 8-byte EBML size: the marker byte, then 56 bits. Any size below 2^56 - 1 fits.
fn size8(n: u64) -> Vec<u8> {
    let mut out = vec![0x01];
    out.extend_from_slice(&n.to_be_bytes()[1..]);
    out
}

fn el(id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend(size8(body.len() as u64));
    out.extend_from_slice(body);
    out
}

fn uint(id: &[u8], value: u64) -> Vec<u8> {
    el(id, &value.to_be_bytes())
}

fn float(id: &[u8], value: f64) -> Vec<u8> {
    el(id, &value.to_be_bytes())
}

const EBML_HEADER: &[u8] = &[0x1A, 0x45, 0xDF, 0xA3];
const DOC_TYPE: &[u8] = &[0x42, 0x82];
const SEGMENT: &[u8] = &[0x18, 0x53, 0x80, 0x67];
const INFO: &[u8] = &[0x15, 0x49, 0xA9, 0x66];
const TIMESTAMP_SCALE: &[u8] = &[0x2A, 0xD7, 0xB1];
const TRACKS: &[u8] = &[0x16, 0x54, 0xAE, 0x6B];
const TRACK_ENTRY: &[u8] = &[0xAE];
const TRACK_NUMBER: &[u8] = &[0xD7];
const TRACK_TYPE: &[u8] = &[0x83];
const CODEC_ID: &[u8] = &[0x86];
const CODEC_DELAY: &[u8] = &[0x56, 0xAA];
const CONTENT_ENCODINGS: &[u8] = &[0x6D, 0x80];
const CONTENT_ENCODING: &[u8] = &[0x62, 0x40];
const CONTENT_ENCODING_SCOPE: &[u8] = &[0x50, 0x32];
const CONTENT_COMPRESSION: &[u8] = &[0x50, 0x34];
const CONTENT_COMP_ALGO: &[u8] = &[0x42, 0x54];
const CONTENT_COMP_SETTINGS: &[u8] = &[0x42, 0x55];
const AUDIO: &[u8] = &[0xE1];
const SAMPLING_FREQUENCY: &[u8] = &[0xB5];
const CHANNELS: &[u8] = &[0x9F];
const VIDEO: &[u8] = &[0xE0];
const PIXEL_WIDTH: &[u8] = &[0xB0];
const PIXEL_HEIGHT: &[u8] = &[0xBA];
const DISPLAY_WIDTH: &[u8] = &[0x54, 0xB0];
const DISPLAY_HEIGHT: &[u8] = &[0x54, 0xBA];
const SEEK_HEAD: &[u8] = &[0x11, 0x4D, 0x9B, 0x74];
const SEEK: &[u8] = &[0x4D, 0xBB];
const SEEK_ID: &[u8] = &[0x53, 0xAB];
const SEEK_POSITION: &[u8] = &[0x53, 0xAC];
const CUE_POINT: &[u8] = &[0xBB];
const CUE_TIME: &[u8] = &[0xB3];
const CUE_TRACK_POSITIONS: &[u8] = &[0xB7];
const CUE_TRACK: &[u8] = &[0xF7];
const CUE_CLUSTER_POSITION: &[u8] = &[0xF1];
const CLUSTER: &[u8] = &[0x1F, 0x43, 0xB6, 0x75];
const CLUSTER_TIMESTAMP: &[u8] = &[0xE7];
const SIMPLE_BLOCK: &[u8] = &[0xA3];

/// One Opus audio track, number 1.
fn opus_track(extra: &[u8]) -> Vec<u8> {
    let mut entry = uint(TRACK_NUMBER, 1);
    entry.extend(uint(TRACK_TYPE, 2));
    entry.extend(el(CODEC_ID, b"A_OPUS"));
    entry.extend_from_slice(extra);
    entry
}

fn audio(channels: u64, codec_delay: Option<u64>) -> Vec<u8> {
    let mut audio = float(SAMPLING_FREQUENCY, 48_000.0);
    audio.extend(uint(CHANNELS, channels));
    let mut entry = el(AUDIO, &audio);
    if let Some(delay) = codec_delay {
        entry.extend(uint(CODEC_DELAY, delay));
    }
    entry
}

/// A whole file: EBML header, then one Segment holding `Info` and the given
/// children. `TimestampScale` is one millisecond.
fn file(children: &[Vec<u8>]) -> Vec<u8> {
    file_scaled(1_000_000, children)
}

/// As [`file`], with `TimestampScale` given in nanoseconds.
fn file_scaled(scale_ns: u64, children: &[Vec<u8>]) -> Vec<u8> {
    let mut out = el(EBML_HEADER, &el(DOC_TYPE, b"matroska"));
    let mut segment = el(INFO, &uint(TIMESTAMP_SCALE, scale_ns));
    for child in children {
        segment.extend_from_slice(child);
    }
    out.extend(el(SEGMENT, &segment));
    out
}

/// A `Tracks` element holding one TrackEntry with the given body.
fn tracks(entry: &[u8]) -> Vec<u8> {
    el(TRACKS, &el(TRACK_ENTRY, entry))
}

/// A SimpleBlock on track 1 with a relative timecode and flags.
fn simple_block(rel: i16, flags: u8, payload: &[u8]) -> Vec<u8> {
    let mut body = vec![0x81];
    body.extend_from_slice(&rel.to_be_bytes());
    body.push(flags);
    body.extend_from_slice(payload);
    el(SIMPLE_BLOCK, &body)
}

// ------------------------------------------------------- lace size overflow ---

/// EBML lacing whose sizes accumulate past `i64::MAX`. The first size is 2^56 - 2 and
/// each of the 254 after it is the largest signed delta an 8-byte vint can state; the
/// running sum overflows. Was an unchecked add (a panic in debug, a wrap in release).
#[test]
fn ebml_lace_sizes_that_overflow_i64_are_corrupt() {
    let mut head = vec![255u8]; // 256 frames: 255 sizes follow
    head.extend(size8((1 << 56) - 2));
    for _ in 0..254 {
        head.extend(size8((1 << 56) - 2));
    }
    let err = fuzz_api::lace(&head, 3).expect_err("sizes past i64 are refused");
    assert!(matches!(err, Error::Corrupt { .. }), "{err:?}");
}

// --------------------------------------------------- block timestamp overflow ---

/// A cluster timestamp of `i64::MAX` with a relative timecode of +1 is a timestamp that
/// does not exist. Was `cluster_ts + rel` unchecked.
#[test]
fn a_block_timestamp_past_i64_is_an_error_not_a_panic() {
    let mut cluster = el(CLUSTER_TIMESTAMP, &i64::MAX.to_be_bytes());
    cluster.extend(simple_block(1, 0x80, b"frame"));
    let bytes = file(&[tracks(&opus_track(&audio(2, None))), el(CLUSTER, &cluster)]);
    let mut dmx = MatroskaDemuxer::new(Cursor::new(bytes)).expect("the header opens");
    assert!(dmx.next_packet().is_err(), "an overflowing timestamp is refused");
}

// --------------------------------------------------------- SeekHead overflow ---

/// A `SeekHead` entry whose position is `u64::MAX` must not be added to the segment's
/// offset. The entry is believed only where it lands on a `Cues`, and nothing does here.
#[test]
fn a_seek_head_position_near_u64_max_does_not_overflow() {
    let mut seek = el(SEEK_ID, &[0x1C, 0x53, 0xBB, 0x6B]);
    seek.extend(uint(SEEK_POSITION, u64::MAX));
    let head = el(SEEK_HEAD, &el(SEEK, &seek));
    let bytes = file(&[head, tracks(&opus_track(&audio(2, None)))]);
    let dmx = MatroskaDemuxer::new(Cursor::new(bytes)).expect("the file still opens");
    assert_eq!(dmx.streams().len(), 1);
}

/// A `SeekHead` naming `Tracks` past the file must not overflow either: the `Tracks`
/// element is then not found and the file opens with no streams, an error.
#[test]
fn a_seek_head_tracks_pointer_near_u64_max_is_refused_not_overflowed() {
    let mut seek = el(SEEK_ID, &[0x16, 0x54, 0xAE, 0x6B]);
    seek.extend(uint(SEEK_POSITION, u64::MAX - 1));
    let head = el(SEEK_HEAD, &el(SEEK, &seek));
    let bytes = file(&[head]);
    assert!(MatroskaDemuxer::new(Cursor::new(bytes)).is_err());
}

// ------------------------------------------------------------- cue overflow ---

/// A cue whose cluster position plus the segment offset overflows is dropped, not added.
#[test]
fn a_cue_cluster_position_past_u64_is_dropped() {
    let mut track = uint(CUE_TRACK, 1);
    track.extend(uint(CUE_CLUSTER_POSITION, u64::MAX));
    let mut point = uint(CUE_TIME, 0);
    point.extend(el(CUE_TRACK_POSITIONS, &track));
    let body = el(CUE_POINT, &point);
    assert_eq!(fuzz_api::cues(&body, 1), 0);
    assert_eq!(fuzz_api::cues(&body, u64::MAX), 0);
}

// ------------------------------------------------------ Opus CodecDelay overflow ---

/// A `CodecDelay` near `u64::MAX`, times a 48 kHz rate, overflowed a u64. The padding
/// saturates at `u32::MAX`, the largest value the field holds.
#[test]
fn a_codec_delay_near_u64_max_saturates_the_padding() {
    let bytes = file(&[tracks(&opus_track(&audio(2, Some(u64::MAX - 1))))]);
    let dmx = MatroskaDemuxer::new(Cursor::new(bytes)).expect("the file opens");
    assert_eq!(dmx.streams()[0].initial_padding, u32::MAX);
}

// ----------------------------------------------------- display aspect overflow ---

/// A `DisplayWidth` of 2^62 against a 1000-pixel height overflowed the i64 product that
/// the sample aspect was built from. The track still opens, with no aspect ratio.
#[test]
fn a_display_width_that_overflows_the_aspect_product_is_no_aspect() {
    let mut video = uint(PIXEL_WIDTH, 1000);
    video.extend(uint(PIXEL_HEIGHT, 1000));
    video.extend(uint(DISPLAY_WIDTH, 1 << 62));
    video.extend(uint(DISPLAY_HEIGHT, 1000));
    let mut entry = uint(TRACK_NUMBER, 1);
    entry.extend(uint(TRACK_TYPE, 1));
    entry.extend(el(CODEC_ID, b"V_VP9"));
    entry.extend(el(VIDEO, &video));
    let bytes = file(&[tracks(&entry)]);
    let dmx = MatroskaDemuxer::new(Cursor::new(bytes)).expect("the file opens");
    let ec_core::MediaParameters::Video(v) = &dmx.streams()[0].params.media else {
        panic!("a video track");
    };
    assert!(v.sample_aspect_ratio.is_none());
}

// ------------------------------------------------------- channel count ---

/// The channel limit at its edge: 255 opens, 256 is refused with a corrupt error.
#[test]
fn channel_counts_up_to_255_open_and_256_is_refused() {
    let bytes = file(&[tracks(&opus_track(&audio(255, None)))]);
    let dmx = MatroskaDemuxer::new(Cursor::new(bytes)).expect("255 channels open");
    assert_eq!(dmx.streams().len(), 1);

    let bytes = file(&[tracks(&opus_track(&audio(256, None)))]);
    let err = MatroskaDemuxer::new(Cursor::new(bytes)).err().expect("256 is refused");
    assert!(matches!(err, Error::Corrupt { .. }), "{err:?}");
}

// ----------------------------------------------- per-cluster unpacked budget ---

/// Header-stripped frames unpack to the settings' length each. A cluster of 80 such
/// blocks would hold 80 x 4 MiB at once; the budget refuses the block that passes it.
#[test]
fn a_cluster_whose_blocks_unpack_past_the_budget_is_refused() {
    let settings = vec![0u8; 4 << 20];
    // Header stripping (algorithm 3) on every frame: each block unpacks to the settings
    // plus its own payload.
    let mut compression = uint(CONTENT_COMP_ALGO, 3);
    compression.extend(el(CONTENT_COMP_SETTINGS, &settings));
    let mut encoding = uint(CONTENT_ENCODING_SCOPE, 1);
    encoding.extend(el(CONTENT_COMPRESSION, &compression));
    let mut extra = audio(2, None);
    extra.extend(el(CONTENT_ENCODINGS, &el(CONTENT_ENCODING, &encoding)));
    let entry = opus_track(&extra);
    let mut cluster = uint(CLUSTER_TIMESTAMP, 0);
    for _ in 0..80 {
        cluster.extend(simple_block(0, 0x80, b"x"));
    }
    let bytes = file(&[tracks(&entry), el(CLUSTER, &cluster)]);
    let mut dmx = MatroskaDemuxer::new(Cursor::new(bytes)).expect("the file opens");
    let mut packets = 0;
    let err = loop {
        match dmx.next_packet() {
            Ok(_) => packets += 1,
            Err(e) => break e,
        }
    };
    assert!(matches!(err, Error::Corrupt { .. }), "{err:?}");
    assert!(packets < 80, "the budget stops the cluster before all 80 blocks");
}

// ------------------------------------------------------- seek rescale overflow ---

/// A `TimestampScale` of 2^62 ns gives a time base numerator of 2^53, and a seek target
/// of 2^60 ticks in that base overflowed the i128 product the rescale to the file's base
/// is computed in. Found by the `seek` fuzz target. The rescale now saturates, so the
/// seek lands at the end of this clusterless file and the next packet is end-of-stream.
#[test]
fn a_seek_target_that_overflows_the_rescale_is_clamped_not_a_panic() {
    let bytes = file_scaled(1 << 62, &[tracks(&opus_track(&audio(2, None)))]);
    let mut dmx = MatroskaDemuxer::new(Cursor::new(bytes)).expect("the file opens");
    let target = Timestamp::new(1 << 60, dmx.time_base());
    dmx.seek(0, target, SeekMode::SyncBefore)
        .expect("a seek past every cluster lands at the end");
    assert!(matches!(dmx.next_packet(), Err(Error::Eof)));
}
