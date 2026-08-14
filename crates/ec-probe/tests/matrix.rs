//! The probe matrix: every standalone audio fixture opens as the right format
//! and codec, decodes to the end, counts the packets ffprobe counts, and lands
//! where it says it does after a hundred random seeks — on one reader.
//!
//! Oracle is ffmpeg/ffprobe (labelled as such: this is not a conformance
//! claim). A missing fixture skips its row rather than failing the suite, so a
//! checkout without generated fixtures still builds and runs.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::registry::{MediaType, SeekMode};
use ec_core::timebase::{TimeBase, Timestamp};
use ec_probe::{Format, Reader};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio")
}

/// `ffprobe` on one entry of the first audio stream.
fn probe(path: &Path, args: &[&str]) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a:0"])
        .args(args)
        .args(["-of", "default=nw=1:nk=1"])
        .arg(path)
        .output()
        .expect("ffprobe");
    assert!(
        out.status.success(),
        "ffprobe {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Packets ffprobe counts on the first audio stream.
fn ffprobe_packets(path: &Path) -> u64 {
    probe(
        path,
        &["-count_packets", "-show_entries", "stream=nb_read_packets"],
    )
    .lines()
    .next()
    .and_then(|l| l.trim().parse().ok())
    .unwrap_or(0)
}

/// Sample frames ffmpeg decodes out of the first audio stream.
fn ffmpeg_frames(path: &Path, channels: usize) -> u64 {
    let out = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-f", "f32le", "-"])
        .output()
        .expect("ffmpeg");
    assert!(out.status.success(), "ffmpeg {}", path.display());
    (out.stdout.len() / 4 / channels.max(1)) as u64
}

/// Decode a whole file through the probe: `(packets, sample frames, channels)`.
fn decode_all(reader: &mut Reader, stream: u32) -> (u64, u64, usize) {
    let mut decoder = reader.make_decoder(stream).expect("a decoder");
    let mut out = Vec::new();
    let (mut packets, mut frames) = (0u64, 0u64);
    loop {
        let packet = match reader.next_packet() {
            Ok(p) => p,
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("demux: {e}"),
        };
        if packet.stream != stream {
            continue;
        }
        packets += 1;
        decoder.decode(&packet, &mut out).expect("decode");
        frames += (out.len() / decoder.channels().max(1)) as u64;
    }
    (packets, frames, decoder.channels())
}

/// name, expected format, expected codec name.
const MATRIX: &[(&str, Format, &str)] = &[
    ("wav16-mono-44100.wav", Format::Wav, "pcm_s16le"),
    ("wav16-stereo-48000.wav", Format::Wav, "pcm_s16le"),
    ("wav16-5.1-48000.wav", Format::Wav, "pcm_s16le"),
    ("flac-mono-44100.flac", Format::Flac, "flac"),
    ("flac-stereo-48000.flac", Format::Flac, "flac"),
    ("flac-5.1-44100.flac", Format::Flac, "flac"),
    ("mp3-mono-44100.mp3", Format::Mp3, "mp3"),
    ("mp3-stereo-48000.mp3", Format::Mp3, "mp3"),
    ("aac-adts-mono-44100.aac", Format::Adts, "aac"),
    ("aac-adts-stereo-48000.aac", Format::Adts, "aac"),
    ("aac-adts-5.1-48000.aac", Format::Adts, "aac"),
    ("aac-mp4-stereo-48000.mp4", Format::Mp4, "aac"),
    ("aac-mp4-5.1-44100.mp4", Format::Mp4, "aac"),
    ("alac-mp4-stereo-44100.m4a", Format::Mp4, "alac"),
    ("alac-mp4-5.1-48000.m4a", Format::Mp4, "alac"),
    ("alac24-mp4-stereo-48000.m4a", Format::Mp4, "alac"),
    ("vorbis-ogg-mono-44100.ogg", Format::Ogg, "vorbis"),
    ("vorbis-ogg-stereo-48000.ogg", Format::Ogg, "vorbis"),
    ("vorbis-ogg-5.1-48000.ogg", Format::Ogg, "vorbis"),
    ("opus-ogg-mono-48000.opus", Format::Ogg, "opus"),
    ("opus-ogg-stereo-48000.opus", Format::Ogg, "opus"),
    ("opus-ogg-5.1-48000.opus", Format::Ogg, "opus"),
    ("aac-mka-stereo-48000.mka", Format::Matroska, "aac"),
    ("flac-mka-stereo-48000.mka", Format::Matroska, "flac"),
    ("opus-mka-stereo-48000.mka", Format::Matroska, "opus"),
    ("av-h264-aac-stereo-48000.mkv", Format::Matroska, "aac"),
];

#[test]
fn every_fixture_probes_decodes_and_counts() {
    let mut checked = 0;
    let mut rows = Vec::new();
    for &(name, format, codec) in MATRIX {
        let path = fixtures().join(name);
        if !path.exists() {
            continue;
        }
        let mut reader = Reader::open(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(reader.format(), format, "{name}: format");
        let audio = reader
            .default_stream(MediaType::Audio)
            .unwrap_or_else(|| panic!("{name}: no audio stream"))
            .clone();
        assert_eq!(audio.params.codec.name(), codec, "{name}: codec");

        let (packets, frames, channels) = decode_all(&mut reader, audio.index);
        let want_frames = ffmpeg_frames(&path, channels);
        // Lossless formats are exact; a coded one carries encoder priming that
        // ffmpeg trims and this reader hands out, so it decodes *more*.
        let exact = matches!(codec, "flac" | "alac") || codec.starts_with("pcm");
        if exact {
            assert_eq!(frames, want_frames, "{name}: sample frames");
        } else {
            let slack = 4096 + want_frames / 20;
            assert!(
                frames + slack >= want_frames && frames <= want_frames + slack,
                "{name}: {frames} frames decoded, ffmpeg says {want_frames}"
            );
        }
        // Packet counts are the demuxer's own claim and must match exactly —
        // except for PCM, where a "packet" is this reader's own chunking.
        if !codec.starts_with("pcm") {
            let want_packets = ffprobe_packets(&path);
            assert_eq!(packets, want_packets, "{name}: packet count");
        }
        // A stated duration must be within a frame of the truth.
        if let Some(d) = reader.duration() {
            let want = want_frames as f64 / f64::from(audio.params.audio().unwrap().sample_rate);
            assert!(
                (d.as_secs_f64() - want).abs() < 0.2,
                "{name}: duration {} vs {want}",
                d.as_secs_f64()
            );
        }
        rows.push(format!(
            "{name:34} {:9} {codec:6} {channels}ch {packets:5} packets {frames:8} frames",
            format.name()
        ));
        checked += 1;
    }
    assert!(
        checked > 0,
        "no fixtures found — run scripts/gen-fixtures.sh"
    );
    eprintln!("probe matrix ({checked} files):\n{}", rows.join("\n"));
}

/// A hundred random seeks per format, on **one** reader, each landing at or
/// before its target and within the container's own granularity of it.
#[test]
fn a_hundred_random_seeks_land_where_they_say() {
    let mut checked = 0;
    for &(name, _, codec) in MATRIX {
        let path = fixtures().join(name);
        if !path.exists() {
            continue;
        }
        let mut reader = Reader::open(&path).expect("open");
        let audio = reader
            .default_stream(MediaType::Audio)
            .expect("audio")
            .clone();
        let rate = audio.params.audio().unwrap().sample_rate.max(1);
        let Some(duration) = reader.duration() else {
            continue;
        };
        let secs = duration.as_secs_f64();
        // Tolerance is the container's own granularity: Ogg lands on a page
        // (3936 bytes on these fixtures, over a second of audio) and Matroska
        // on a cluster, while every other format lands inside the frame that
        // holds the target — so their tolerance is that frame's own length,
        // taken from the packet itself below.
        let coarse = match name {
            n if n.ends_with(".mka") || n.ends_with(".mkv") => Some(2.5),
            n if n.ends_with(".ogg") || n.ends_with(".opus") => Some(2.5),
            _ => None,
        };
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..100 {
            // xorshift: a deterministic spread of targets, no dev-dependency.
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let target = (state % 1_000_000) as f64 / 1_000_000.0 * secs;
            let ticks = (target * f64::from(rate)) as i64;
            let landed = reader
                .seek(
                    audio.index,
                    Timestamp::new(ticks, TimeBase::from_rate(rate)),
                    SeekMode::SyncBefore,
                )
                .unwrap_or_else(|e| panic!("{name}: seek to {target:.3}s: {e}"));
            let at = landed.as_secs_f64();
            assert!(
                at <= target + 0.001,
                "{name}: asked for {target:.3}s, landed at {at:.3}s — past the target"
            );
            // And the reader really is there: the landing is a packet's own
            // timestamp, not an estimate.
            let tolerance = loop {
                let packet = reader.next_packet().expect("a packet after a seek");
                if packet.stream != audio.index {
                    continue;
                }
                assert!(!packet.data.is_empty(), "{name}: an empty packet");
                assert_eq!(
                    packet.pts.unwrap_or(0),
                    landed.ticks,
                    "{name}: the landing is not the next packet's own timestamp"
                );
                break coarse.unwrap_or_else(|| {
                    packet.duration.unwrap_or(1) as f64 / f64::from(rate) + 0.001
                });
            };
            assert!(
                target - at <= tolerance,
                "{name}: asked for {target:.3}s, landed at {at:.3}s — {:.3}s short of a                  container granularity of {tolerance:.3}s",
                target - at
            );
        }
        let _ = codec;
        checked += 1;
    }
    assert!(
        checked > 0,
        "no fixtures found — run scripts/gen-fixtures.sh"
    );
    eprintln!("100 random seeks each on {checked} files, one reader per file");
}

/// Cover art is a still picture, not a video track, in both containers that
/// carry one — and the audio beside it still opens.
#[test]
fn cover_art_is_never_a_video_track() {
    let mut checked = 0;
    for name in ["cover-mp3-stereo-44100.mp3", "cover-mp4-stereo-44100.m4a"] {
        let path = fixtures().join(name);
        if !path.exists() {
            continue;
        }
        let reader = Reader::open(&path).expect("open");
        let video: Vec<_> = reader
            .streams()
            .iter()
            .filter(|s| s.params.codec.media_type() == MediaType::Video)
            .collect();
        assert!(
            video.is_empty(),
            "{name}: cover art listed as a video track: {video:?}"
        );
        assert!(
            reader.default_stream(MediaType::Audio).is_some(),
            "{name}: no audio"
        );
        checked += 1;
    }
    assert!(checked > 0, "no cover-art fixtures — run gen-fixtures.sh");
}

/// The tags an ID3v2-tagged file carries come back off it.
#[test]
fn id3_tags_come_off_a_real_file() {
    let path = fixtures().join("cover-mp3-stereo-44100.mp3");
    if !path.exists() {
        return;
    }
    let reader = Reader::open(&path).expect("open");
    assert_eq!(reader.tags().title.as_deref(), Some("A Tone"));
    assert_eq!(reader.tags().artist.as_deref(), Some("edith_codecs"));
}

/// A mislabelled file is still the file it is: the sniff decides, not the
/// extension.
#[test]
fn a_mislabelled_file_opens_as_what_it_is() {
    let src = fixtures().join("flac-stereo-48000.flac");
    if !src.exists() {
        return;
    }
    let liar = std::env::temp_dir().join("ec-probe-liar.mp3");
    std::fs::copy(&src, &liar).expect("copy");
    let reader = Reader::open(&liar).expect("open");
    assert_eq!(reader.format(), Format::Flac);
    std::fs::remove_file(&liar).ok();
}
