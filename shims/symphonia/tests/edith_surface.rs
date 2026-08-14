//! The shim driven exactly as edith drives symphonia.
//!
//! Every call here is copied in shape from `edith/crates/engine/src/audio.rs`
//! (the `SymTrack` reader at 1461-1668 and the `SymDecoder` at 1692-1740), so a
//! green run means the replica's audio path compiles and *works* against this
//! shim rather than merely type-checking against it.

use std::fs::File;
use std::path::{Path, PathBuf};

use symphonia_codec_aac::AacDecoder;
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions,
    well_known::{CODEC_ID_AAC, CODEC_ID_OPUS},
};
use symphonia_core::formats::probe::Hint;
use symphonia_core::formats::{
    FormatOptions, FormatReader, SeekMode, SeekTo, TrackType as SymKind,
};
use symphonia_core::io::MediaSourceStream;
use symphonia_core::meta::MetadataOptions;
use symphonia_core::units::{Time, TimeBase, Timestamp};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio")
}

/// edith's `sym_reader`, verbatim in shape.
fn sym_reader(path: &Path) -> Box<dyn FormatReader> {
    let mss = MediaSourceStream::new(
        Box::new(File::open(path).expect("open")),
        Default::default(),
    );
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .expect("probe")
}

/// edith's `SymTrack::open_inner`, verbatim in shape.
struct SymTrack {
    reader: Box<dyn FormatReader>,
    track_id: u32,
    sample_rate: u32,
    channels: u16,
    total_samples: Option<u64>,
    time_base: TimeBase,
    params: AudioCodecParameters,
    codec: &'static str,
}

impl SymTrack {
    fn open(path: &Path) -> Option<SymTrack> {
        let reader = sym_reader(path);
        let track = reader.default_track(SymKind::Audio)?;
        let (track_id, num_frames) = (track.id, track.num_frames);
        let time_base = track.time_base.expect("a time base");
        let Some(CodecParameters::Audio(params)) = track.codec_params.clone() else {
            panic!("no audio codec parameters");
        };
        let sample_rate = params.sample_rate.expect("a sample rate");
        let channels = params
            .channels
            .as_ref()
            .map(|c| c.count() as u16)
            .expect("a layout");
        let codec = symphonia::default::get_codecs()
            .get_audio_decoder(params.codec)
            .map_or("an unsupported format", |d| d.codec.info.short_name);
        Some(SymTrack {
            reader,
            track_id,
            sample_rate,
            channels,
            total_samples: num_frames,
            time_base,
            params,
            codec,
        })
    }

    /// edith's `SymTrack::decoder`, minus the two routes it takes away from
    /// symphonia (multichannel AAC and Opus) — which is the point: this shim
    /// serves them itself.
    fn decoder(&self) -> Box<dyn AudioDecoder> {
        if self.params.codec == CODEC_ID_AAC {
            return Box::new(
                AacDecoder::try_new(&self.params, &AudioDecoderOptions::default()).expect("aac"),
            );
        }
        symphonia::default::get_codecs()
            .make_audio_decoder(&self.params, &AudioDecoderOptions::default())
            .expect("decoder")
    }

    /// edith's `samples_at`.
    fn samples_at(&self, ts: Timestamp) -> u64 {
        let secs = self.time_base.calc_time_saturating(ts).as_secs_f64();
        (secs.max(0.0) * f64::from(self.sample_rate)) as u64
    }

    /// edith's `seek_to`, on **one** reader — the loop that reopens the file
    /// per attempt exists because the incumbent's mkv seek works once.
    fn seek_to(&mut self, secs: f64) -> f64 {
        let time = Time::try_from_secs_f64(secs).expect("a time");
        let to = SeekTo::Time {
            time,
            track_id: Some(self.track_id),
        };
        let landed = self.reader.seek(SeekMode::Accurate, to).expect("seek");
        self.time_base
            .calc_time_saturating(landed.actual_ts)
            .as_secs_f64()
    }
}

/// Open, describe, decode: what edith does for every standalone audio file and
/// for the sound of a Matroska one.
#[test]
fn every_file_opens_describes_and_decodes_through_the_shim() {
    let files = [
        ("wav16-stereo-48000.wav", "pcm_s16le", 2),
        ("flac-5.1-44100.flac", "flac", 6),
        ("mp3-stereo-48000.mp3", "mp3", 2),
        ("aac-adts-stereo-48000.aac", "aac", 2),
        ("alac-mp4-stereo-44100.m4a", "alac", 2),
        ("vorbis-ogg-stereo-48000.ogg", "vorbis", 2),
        ("opus-ogg-5.1-48000.opus", "opus", 6),
        ("aac-mka-stereo-48000.mka", "aac", 2),
        ("av-h264-aac-stereo-48000.mkv", "aac", 2),
    ];
    let mut checked = 0;
    for (name, codec, channels) in files {
        let path = fixtures().join(name);
        if !path.exists() {
            continue;
        }
        let mut track = SymTrack::open(&path).unwrap_or_else(|| panic!("{name}: no audio track"));
        assert_eq!(track.codec, codec, "{name}: codec name");
        assert_eq!(track.channels, channels, "{name}: channels");
        assert!(track.total_samples.unwrap_or(0) > 0, "{name}: no duration");

        let mut decoder = track.decoder();
        let mut out = Vec::new();
        let mut frames = 0u64;
        let mut first_pts = None;
        while let Some(packet) = track.reader.next_packet().expect("demux") {
            if packet.track_id != track.track_id {
                continue;
            }
            first_pts.get_or_insert_with(|| track.samples_at(packet.pts));
            decoder
                .decode(&packet)
                .expect("decode")
                .copy_to_vec_interleaved::<f32>(&mut out);
            frames += (out.len() / usize::from(track.channels)) as u64;
        }
        assert_eq!(first_pts, Some(0), "{name}: first packet is not at zero");
        let want = track.total_samples.unwrap_or(0);
        assert!(
            frames * 20 > want * 19,
            "{name}: decoded {frames} of {want} frames"
        );
        checked += 1;
    }
    assert!(checked > 0, "no fixtures — run scripts/gen-fixtures.sh");
    eprintln!("edith's symphonia surface: {checked} files opened and decoded");
}

/// The defect this shim exists to fix: the incumbent's Matroska seek is
/// reliable exactly once per reader, so edith opens a new reader per attempt.
/// Here twenty seeks in a row are served on one reader, each landing at or
/// before its target, and the audio still decodes from every landing.
#[test]
fn twenty_seeks_on_one_reader_all_land() {
    for name in ["av-h264-aac-stereo-48000.mkv", "aac-mka-stereo-48000.mka"] {
        let path = fixtures().join(name);
        if !path.exists() {
            continue;
        }
        let mut track = SymTrack::open(&path).expect("track");
        let mut decoder = track.decoder();
        let mut out = Vec::new();
        for i in 0..20 {
            let target = 0.1 + (i as f64 * 0.13) % 2.5;
            let landed = track.seek_to(target);
            assert!(
                landed <= target + 0.001,
                "{name}: seek {i} to {target:.3}s landed at {landed:.3}s"
            );
            let mut decoded = 0;
            for _ in 0..8 {
                let Some(packet) = track.reader.next_packet().expect("demux") else {
                    break;
                };
                if packet.track_id != track.track_id {
                    continue;
                }
                decoder
                    .decode(&packet)
                    .expect("decode after a seek")
                    .copy_to_vec_interleaved::<f32>(&mut out);
                decoded += out.len();
            }
            assert!(decoded > 0, "{name}: nothing decoded after seek {i}");
        }
        eprintln!("{name}: 20 seeks, one reader, every landing at or before its target");
    }
}

/// The other incumbent defect: `aac: aac too complex` for anything wider than
/// stereo. A 5.1 AAC track decodes here, through the AAC shim, as 6 channels.
#[test]
fn multichannel_aac_decodes_rather_than_being_refused() {
    let path = fixtures().join("aac-mp4-5.1-44100.mp4");
    if !path.exists() {
        return;
    }
    let mut track = SymTrack::open(&path).expect("track");
    assert_eq!(track.channels, 6);
    let mut decoder = track.decoder();
    let mut out = Vec::new();
    let packet = loop {
        let packet = track
            .reader
            .next_packet()
            .expect("demux")
            .expect("a packet");
        if packet.track_id == track.track_id {
            break packet;
        }
    };
    decoder
        .decode(&packet)
        .expect("5.1 AAC decodes")
        .copy_to_vec_interleaved::<f32>(&mut out);
    assert_eq!(out.len() % 6, 0);
    assert!(out.len() >= 6 * 1024, "a 5.1 frame is 1024 samples wide");
}

/// Opus: the incumbent has no decoder for it at any version, and edith carries
/// a second crate for that alone. Here the registry answers.
#[test]
fn opus_has_a_decoder_in_the_registry() {
    let registration = symphonia::default::get_codecs().get_audio_decoder(CODEC_ID_OPUS);
    assert_eq!(
        registration.map(|r| r.codec.info.short_name),
        Some("opus"),
        "the registry must name an Opus decoder"
    );
    // ...and a TrueHD track has none, which is how a caller learns to say so.
    let truehd = symphonia_core::codecs::audio::well_known::CODEC_ID_TRUEHD;
    assert!(
        symphonia::default::get_codecs()
            .get_audio_decoder(truehd)
            .is_none()
    );
    let params = {
        let mut p = AudioCodecParameters::new();
        p.for_codec(truehd).with_sample_rate(48_000);
        p
    };
    let refusal = match symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
    {
        Ok(_) => panic!("a TrueHD decoder was built out of nothing"),
        Err(e) => e.to_string(),
    };
    assert!(
        refusal.contains("truehd"),
        "the refusal names the codec: {refusal}"
    );
}
