//! Whole-file Ogg Vorbis decode through the `Decoder` trait, with the granule
//! trim the decoder does itself. The oracle is ffmpeg (labelled as such: this
//! is not a conformance claim).
//!
//! Files are encoded by ffmpeg's libvorbis from a sine of an exact length, so
//! the length the stream states is known independently of either decoder.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::{Buf, Decoder, Demuxer, Frame, Packet, TimeBase};
use ec_ogg::{OggDemuxer, granule_of, granule_side_data};
use ec_vorbis::VorbisDecoder;

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ec-vorbis-trim-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A sine of `seconds` at `rate`, libvorbis-encoded to `path`. Returns the
/// number of sample frames the source holds.
fn encode(path: &Path, rate: u32, channels: u32, seconds: f64) -> usize {
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!(
            "sine=frequency=440:sample_rate={rate}:duration={seconds}"
        ))
        .args([
            "-ac",
            &channels.to_string(),
            "-c:a",
            "libvorbis",
            "-q:a",
            "4",
        ])
        .arg(path)
        .status()
        .expect("ffmpeg");
    assert!(
        status.success(),
        "ffmpeg could not encode {}",
        path.display()
    );
    (f64::from(rate) * seconds).round() as usize
}

/// ffmpeg's own decode, planar f32, one vector per channel.
fn ffmpeg_decode(path: &Path, channels: usize) -> Vec<Vec<f32>> {
    let out = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-f", "f32le", "-"])
        .output()
        .expect("ffmpeg");
    assert!(out.status.success(), "ffmpeg refused {}", path.display());
    deinterleave(&out.stdout, channels)
}

fn deinterleave(bytes: &[u8], channels: usize) -> Vec<Vec<f32>> {
    let mut planes = vec![Vec::new(); channels];
    for (i, chunk) in bytes.as_chunks::<4>().0.iter().enumerate() {
        planes[i % channels].push(f32::from_le_bytes(*chunk));
    }
    planes
}

/// The Ogg Vorbis file's headers and audio packets, in order, with the
/// granule each packet carries (if any).
struct Stream {
    extradata: Buf,
    channels: usize,
    packets: Vec<(Vec<u8>, Option<i64>)>,
}

fn open(path: &Path) -> Stream {
    let mut demuxer = OggDemuxer::open(File::open(path).expect("open")).expect("ogg");
    let stream = demuxer
        .streams()
        .iter()
        .find(|s| s.params.codec == ec_core::CodecId::Vorbis)
        .expect("a Vorbis stream")
        .clone();
    let extradata = stream.params.extradata.clone().expect("headers");
    let channels = stream.params.audio().expect("audio").layout.channel_count();
    let mut packets = Vec::new();
    loop {
        let packet = match demuxer.next_packet() {
            Ok(packet) => packet,
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("demux {}: {e}", path.display()),
        };
        if packet.stream == stream.index {
            packets.push((packet.data.to_vec(), granule_of(&packet)));
        }
    }
    Stream {
        extradata,
        channels,
        packets,
    }
}

/// Run every packet through the `Decoder` trait, then flush, and answer the
/// planar output. Frames are collected exactly as a consumer receives them.
fn decode_through_trait(stream: &Stream, packets: &[(Vec<u8>, Option<i64>)]) -> Vec<Vec<f32>> {
    let mut decoder = VorbisDecoder::from_extradata(&stream.extradata).expect("headers");
    let mut planes = vec![Vec::new(); stream.channels];
    let take = |decoder: &mut VorbisDecoder, planes: &mut Vec<Vec<f32>>| loop {
        match decoder.receive_frame() {
            Ok(Frame::Audio(frame)) => {
                for (channel, plane) in planes.iter_mut().enumerate() {
                    let bytes = &frame.data[channel];
                    plane.extend(
                        bytes
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .take(frame.samples)
                            .map(|c| f32::from_le_bytes(*c)),
                    );
                }
            }
            Ok(Frame::Video(_)) => unreachable!("Vorbis decodes to audio only"),
            Err(e) if e.is_need_more() || e.is_eof() => break,
            Err(e) => panic!("receive_frame: {e}"),
        }
    };
    let base = TimeBase::new(1, 1);
    for (data, granule) in packets {
        let mut packet = Packet::new(0, base, data.clone());
        if let Some(granule) = granule {
            packet.side_data.push(granule_side_data(*granule));
        }
        decoder.send_packet(&packet).expect("send_packet");
        take(&mut decoder, &mut planes);
    }
    decoder.flush().expect("flush");
    take(&mut decoder, &mut planes);
    planes
}

/// Normalised cross-correlation of two signals over their common length.
fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (&x, &y) in a.iter().zip(b) {
        let (x, y) = (f64::from(x), f64::from(y));
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return if na == nb { 1.0 } else { 0.0 };
    }
    dot / (na.sqrt() * nb.sqrt())
}

fn encoded(name: &str, rate: u32, channels: u32, seconds: f64) -> (PathBuf, usize) {
    let path = scratch().join(name);
    let len = encode(&path, rate, channels, seconds);
    (path, len)
}

/// Multi-page files: the page granules say where each page ends, and the
/// decoder's output must be exactly ffmpeg's length, sample for sample.
#[test]
fn whole_file_length_and_samples_match_ffmpeg() {
    let mut failures = Vec::new();
    for (rate, channels, seconds) in [
        (44_100u32, 1u32, 1.234f64),
        (44_100, 2, 2.5),
        (48_000, 1, 2.5),
        (48_000, 2, 3.0),
        (48_000, 1, 1.234),
        (44_100, 2, 1.234),
    ] {
        let name = format!("multi-{rate}-{channels}ch-{seconds}s.ogg");
        let (path, _) = encoded(&name, rate, channels, seconds);
        let stream = open(&path);
        let theirs = ffmpeg_decode(&path, channels as usize);
        let ours = decode_through_trait(&stream, &stream.packets);
        if ours[0].len() != theirs[0].len() {
            failures.push(format!(
                "{name}: {} frames, ffmpeg {}",
                ours[0].len(),
                theirs[0].len()
            ));
            continue;
        }
        for (channel, (mine, reference)) in ours.iter().zip(theirs.iter()).enumerate() {
            let corr = correlation(mine, reference);
            if corr < 0.999 {
                failures.push(format!("{name}: ch{channel} correlation {corr:.6}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A file whose audio fits on one Ogg page carries only the final granule, so
/// the decoder has nothing to anchor a head trim on: it emits exactly the
/// granule's length from sample zero. ffmpeg 8 (libavformat) starts its own
/// packet timeline 128 samples earlier on such files and decodes 128 fewer
/// frames. The granule is the stream's own statement of its length, so the
/// decoder follows it; the test checks that our samples agree with ffmpeg's
/// where both have them.
#[test]
fn single_page_file_ends_at_its_granule() {
    let mut failures = Vec::new();
    for (rate, channels) in [(44_100u32, 1usize), (48_000, 2)] {
        let name = format!("single-{rate}-{channels}ch.ogg");
        let (path, want) = encoded(&name, rate, channels as u32, 1.0);
        let stream = open(&path);
        let theirs = ffmpeg_decode(&path, channels);
        let ours = decode_through_trait(&stream, &stream.packets);
        if ours[0].len() != want {
            failures.push(format!(
                "{name}: {} frames, granule states {want}",
                ours[0].len()
            ));
        }
        for (channel, (mine, reference)) in ours.iter().zip(theirs.iter()).enumerate() {
            let corr = correlation(&mine[..reference.len().min(mine.len())], reference);
            if corr < 0.999 {
                failures.push(format!("{name}: ch{channel} correlation {corr:.6}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A granule that lands before the decoded count on the first packet that
/// produces output is a head trim: the decoder drops the samples before the
/// stream's start. Built by moving the granule of that packet, so the expected
/// output is known exactly from an untrimmed decode.
#[test]
fn granule_behind_the_first_output_trims_the_head() {
    const K: i64 = 300;
    let (path, want) = encoded("head-44100-2ch.ogg", 44_100, 2, 2.5);
    let stream = open(&path);
    let mut raw = VorbisDecoder::from_extradata(&stream.extradata).expect("headers");
    let mut untrimmed = vec![Vec::new(); stream.channels];
    let mut counts = Vec::new();
    for (data, _) in &stream.packets {
        let samples = raw.decode_audio(data).expect("decode");
        counts.push(samples[0].len() as i64);
        for (channel, plane) in samples.iter().enumerate() {
            untrimmed[channel].extend_from_slice(plane);
        }
    }
    // The first packet that produces output (packet 0 produces none).
    let first_output = counts.iter().position(|&c| c > 0).expect("some output");
    let decoded_through: i64 = counts[..=first_output].iter().sum();
    assert!(
        decoded_through > K,
        "first output packet too short to trim {K} from"
    );

    let mut packets = stream.packets.clone();
    packets[first_output].1 = Some(decoded_through - K);
    let ours = decode_through_trait(&stream, &packets);

    assert_eq!(ours[0].len(), want, "head trim changed the length");
    for (channel, plane) in ours.iter().enumerate() {
        let expect = &untrimmed[channel][K as usize..K as usize + want];
        assert!(
            plane == expect,
            "ch{channel}: output is not the untrimmed decode from sample {K}"
        );
    }
}
