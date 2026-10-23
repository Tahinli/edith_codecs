//! A packet holding several syncframes decodes to those frames back to back.
//!
//! `decode_frame` takes one syncframe and refuses a second by name; the packet
//! call (`decode_packet`, and `send_packet` which the shims use) walks every
//! syncframe in the packet against one decoder state. The reference is the
//! same bytes decoded one frame at a time on a fresh decoder, which the
//! per-frame matrix already holds to ffmpeg, so equality here is exact.
//!
//! Missing fixtures skip, or fail under `EC_REQUIRE_FIXTURES=1`.

use std::path::{Path, PathBuf};

use ec_ac3::{Ac3Decoder, Error, eac3, frame_size};
use ec_core::{Decoder, Frame, Packet, TimeBase};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio")
}

/// Whether the fixture is present. A missing one skips the test, or fails it
/// under `EC_REQUIRE_FIXTURES=1`.
fn present(path: &Path) -> bool {
    if path.exists() {
        return true;
    }
    assert!(
        std::env::var("EC_REQUIRE_FIXTURES").as_deref() != Ok("1"),
        "fixture missing: {path:?}"
    );
    eprintln!("skipping: fixture missing: {path:?}");
    false
}

/// Every syncframe of an AC-3 or E-AC-3 stream without dependent substreams.
fn frames(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let Ok(size) = frame_size(&data[pos..]) else {
            break;
        };
        out.push(&data[pos..pos + size]);
        pos += size;
    }
    out
}

/// Packets of an E-AC-3 stream whose independent substreams carry dependent
/// ones: each packet runs from one independent substream to the next.
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
    (0..starts.len())
        .map(|i| {
            let end = starts.get(i + 1).copied().unwrap_or(data.len());
            &data[starts[i]..end]
        })
        .collect()
}

/// One frame at a time on one decoder: the reference the packet walk must hit.
fn decode_each(packets: &[&[u8]]) -> Vec<u8> {
    let mut dec = Ac3Decoder::new();
    let mut out = Vec::new();
    for p in packets {
        let frame = dec.decode_frame(p).expect("single frame decodes");
        out.extend_from_slice(&frame.data[0]);
    }
    out
}

/// The packet walk over the concatenation, through the trait call the shims
/// make.
fn decode_packet_bytes(packets: &[&[u8]]) -> (Vec<u8>, u64) {
    let bytes: Vec<u8> = packets.concat();
    let mut dec = Ac3Decoder::new();
    dec.send_packet(&Packet::new(0, TimeBase::new(1, 48_000), bytes))
        .expect("multi-syncframe packet decodes");
    match dec.receive_frame() {
        Ok(Frame::Audio(audio)) => (audio.data[0].to_vec(), audio.samples as u64),
        other => panic!("expected one audio frame, got {other:?}"),
    }
}

#[test]
fn ac3_packets_of_two_and_three_syncframes_decode_to_their_frames() {
    let path = fixtures().join("ac3-5.1-48000.ac3");
    if !present(&path) {
        return;
    }
    let data = std::fs::read(&path).expect("fixture");
    let all = frames(&data);
    assert!(all.len() >= 3, "fixture has too few frames");
    for n in [2, 3] {
        let group = &all[..n];
        let (bytes, samples) = decode_packet_bytes(group);
        assert_eq!(bytes, decode_each(group), "{n} AC-3 frames: samples");
        assert_eq!(samples, (n * 1536) as u64, "{n} AC-3 frames: sample count");
    }
}

#[test]
fn eac3_packets_of_two_and_three_syncframes_decode_to_their_frames() {
    let path = fixtures().join("eac3-5.1-48000.eac3");
    if !present(&path) {
        return;
    }
    let data = std::fs::read(&path).expect("fixture");
    let all = frames(&data);
    assert!(all.len() >= 3, "fixture has too few frames");
    for n in [2, 3] {
        let group = &all[..n];
        let (bytes, _) = decode_packet_bytes(group);
        assert_eq!(bytes, decode_each(group), "{n} E-AC-3 frames: samples");
    }
}

/// Dependent substreams stay with their independent substream inside a packet
/// that carries several (7.1 DD+ assembled as 5.1 plus a dependent).
#[test]
fn e_ac3_dependent_packets_inside_a_multi_syncframe_packet_keep_their_channels() {
    let path = fixtures().join("ac3dep-7.1-48000.eac3");
    if !present(&path) {
        return;
    }
    let data = std::fs::read(&path).expect("fixture");
    let all = packets(&data);
    assert!(all.len() >= 3, "fixture has too few packets");
    for n in [2, 3] {
        let group = &all[..n];
        let (bytes, samples) = decode_packet_bytes(group);
        assert_eq!(bytes, decode_each(group), "{n} dependent packets");
        // 8 channels of f32 per sample frame after the 7.1 layout is kept.
        assert_eq!(bytes.len() as u64, samples * 8 * 4, "{n} packets: 7.1 layout");
    }
}

/// Bytes after the last syncframe are padding, and padding decodes like the
/// bare frames.
#[test]
fn padding_after_several_syncframes_is_ignored() {
    let path = fixtures().join("ac3-5.1-48000.ac3");
    if !present(&path) {
        return;
    }
    let data = std::fs::read(&path).expect("fixture");
    let all = frames(&data);
    let mut packet: Vec<u8> = all[..3].concat();
    packet.extend_from_slice(&[0u8; 7]);
    let mut dec = Ac3Decoder::new();
    let frame = dec.decode_packet(&packet).expect("padded packet decodes");
    assert_eq!(frame.data[0].to_vec(), decode_each(&all[..3]));
}

/// A second frame of another channel layout cannot share one output frame.
#[test]
fn syncframes_of_different_layouts_in_one_packet_are_refused_by_name() {
    let surround = fixtures().join("ac3-5.1-48000.ac3");
    let stereo = fixtures().join("ac3-stereo-48000.ac3");
    if !present(&surround) || !present(&stereo) {
        return;
    }
    let a = std::fs::read(&surround).expect("fixture");
    let b = std::fs::read(&stereo).expect("fixture");
    let mut packet = frames(&a)[0].to_vec();
    packet.extend_from_slice(frames(&b)[0]);
    let mut dec = Ac3Decoder::new();
    match dec.decode_packet(&packet) {
        Err(Error::Unsupported { what, .. }) => {
            assert!(what.contains("different channel layouts"), "{what}")
        }
        other => panic!("expected a named refusal, got {other:?}"),
    }
}

/// `decode_frame` keeps its one-syncframe contract: the same packet is refused
/// there and decoded by the packet call.
#[test]
fn decode_frame_still_takes_one_syncframe() {
    let path = fixtures().join("ac3-stereo-48000.ac3");
    if !present(&path) {
        return;
    }
    let data = std::fs::read(&path).expect("fixture");
    let all = frames(&data);
    let packet = [all[0], all[1]].concat();
    let mut dec = Ac3Decoder::new();
    assert!(matches!(
        dec.decode_frame(&packet),
        Err(Error::Unsupported { .. })
    ));
}
