//! The replica's `write_ogg` (`export.rs:2895-2985`) with both of its
//! corrections deleted, run end to end.
//!
//! That deletion *is* the test: no hop of silence in front of the mix, no hop
//! subtracted from every granule, no hand-written tail granule, and a mono mix
//! written as mono rather than widened to dual mono. What comes out has to
//! decode in ffmpeg without a word of complaint and be exactly as long as the
//! timeline it came from — which is the promise the replica makes for every
//! other audio format.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::{
    AudioParameters, Buf, ChannelLayout, CodecId, CodecParameters, MediaParameters, Muxer, Packet,
    StreamInfo, TimeBase,
};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rusty-vorbis-shim-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir.join(name)
}

/// The replica's own mix, as 16-bit samples: a tone per channel so a channel
/// that came out of the wrong hole is visible rather than subtle.
fn mix(channels: u16, rate: u32, frames: usize) -> Vec<i16> {
    let mut samples = Vec::with_capacity(frames * usize::from(channels));
    for i in 0..frames {
        for channel in 0..channels {
            let hz = 220.0 * f64::from(channel + 1);
            let t = i as f64 / f64::from(rate);
            let value = (2.0 * std::f64::consts::PI * hz * t).sin() * 0.6;
            samples.push((value * f64::from(i16::MAX)) as i16);
        }
    }
    samples
}

/// `write_ogg`, shorn of the workarounds.
fn write_ogg(out: &Path, samples: &[i16], channels: u16, rate: u32) {
    let mut encoder = rusty_vorbis::VorbisEncoder::new(rusty_vorbis::VorbisEncoderConfig {
        bitrate_bps: rusty_vorbis::BITRATE_NOMINAL,
        quality: 0.85,
    });
    encoder
        .push_pcm_s16(samples, channels, rate)
        .expect("vorbis encode");
    encoder.finish();
    let mut packets = Vec::new();
    loop {
        match encoder.next_packet() {
            Ok(packet) => packets.push(packet),
            Err(rusty_vorbis::Error::Eof) => break,
            Err(e) => panic!("vorbis encode: {e}"),
        }
    }
    assert!(
        packets.len() >= 4,
        "{} packets is not a stream",
        packets.len()
    );

    let extradata = ec_ogg::xiph_lace(&[
        &packets[0].data[..],
        &packets[1].data[..],
        &packets[2].data[..],
    ])
    .expect("the three headers lace");

    let time_base = TimeBase::new(1, i64::from(rate));
    let mut params = CodecParameters::new(CodecId::Vorbis);
    params.media = MediaParameters::Audio(AudioParameters {
        sample_rate: rate,
        layout: ChannelLayout::from_count(usize::from(channels)),
        format: None,
        bits_per_sample: None,
    });
    params.extradata = Some(Buf::from_vec(extradata));
    let mut muxer = ec_ogg::OggMuxer::new(File::create(out).expect("create"));
    muxer
        .add_stream(StreamInfo::new(0, time_base, params))
        .expect("stream");
    muxer.write_headers().expect("headers");
    for packet in &packets[3..] {
        // The granule the encoder states, written through: no hop subtracted,
        // and no special case for the last packet.
        let mut out = Packet::new(0, time_base, packet.data.clone());
        out.side_data.push(ec_ogg::granule_side_data(packet.pts));
        out.duration = Some(packet.duration);
        muxer.write_packet(&out).expect("packet");
    }
    muxer.finish().expect("trailer");
}

fn probe(path: &Path, field: &str) -> String {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            &format!("stream={field}"),
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .expect("ffprobe");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn writes_an_ogg_the_way_the_replica_would_without_its_corrections() {
    let rate = 48_000u32;
    // Deliberately not a whole number of blocks: the grid has to close over it
    // and the granule has to trim the overshoot back off.
    let frames = 33_333usize;
    for channels in [1u16, 2, 6] {
        let path = scratch(&format!("write-ogg-{channels}ch.ogg"));
        write_ogg(&path, &mix(channels, rate, frames), channels, rate);

        // ffmpeg decodes it silently...
        let decode = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-f", "f32le", "-"])
            .output()
            .expect("ffmpeg");
        assert!(
            decode.stderr.is_empty(),
            "{channels} ch: ffmpeg said {}",
            String::from_utf8_lossy(&decode.stderr)
        );
        // ...for exactly the timeline's own length...
        assert_eq!(
            probe(&path, "duration_ts"),
            frames.to_string(),
            "{channels} ch: duration"
        );
        // ...at the channel count that was pushed, mono included.
        assert_eq!(
            probe(&path, "channels"),
            channels.to_string(),
            "{channels} ch: channel count"
        );
        let decoded = decode.stdout.len() / 4 / usize::from(channels);
        assert_eq!(decoded, frames, "{channels} ch: decoded sample count");
    }
}
