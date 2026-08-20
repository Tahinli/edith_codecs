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

/// Regression for `open: NeedMore` on a real mp3 whose ID3v2 header carries a
/// large APIC cover-art picture and whose tail carries an ID3v1 `TAG` block:
/// the probe must skip both rather than reading a bounded head short of the
/// first mp3 frame sync, or getting confused reading a 128-byte tail as audio.
/// Built out of a plain mp3 fixture wrapped in a synthetic 2 MB ID3v2 APIC
/// header and an ID3v1 tail, so this needs no real file on disk.
#[test]
fn cover_art_and_id3v1_tail_do_not_stop_the_probe() {
    let raw = std::fs::read(fixtures().join("mp3-stereo-48000.mp3")).expect("fixture mp3");
    // A 2 MB ID3v2.3 tag: one oversized `APIC` frame of filler bytes. The
    // probe only needs to skip past the tag's declared size, so the frame
    // body's content does not matter.
    let apic_body_len = 2_000_000usize;
    let mut id3 = Vec::new();
    id3.extend_from_slice(b"ID3");
    id3.extend_from_slice(&[3, 0, 0]); // version 2.3.0, no flags
    let frame_len = apic_body_len as u32;
    let tag_size = 10 + apic_body_len as u32; // one frame header (10) + body
    id3.extend_from_slice(&syncsafe(tag_size));
    id3.extend_from_slice(b"APIC");
    id3.extend_from_slice(&frame_len.to_be_bytes());
    id3.extend_from_slice(&[0, 0]); // frame flags
    id3.extend(std::iter::repeat_n(0u8, apic_body_len));

    let mut id3v1 = vec![0u8; 128];
    id3v1[0..3].copy_from_slice(b"TAG");

    let mut bytes = id3;
    bytes.extend_from_slice(&raw);
    bytes.extend_from_slice(&id3v1);

    let path = std::env::temp_dir().join("edith-cover-art-id3v1-tail.mp3");
    std::fs::write(&path, &bytes).expect("write synthetic mp3");

    let mut track = SymTrack::open(&path).expect("open past the ID3v2 APIC and ID3v1 tail");
    assert_eq!(track.codec, "mp3");
    let mut decoder = track.decoder();
    let mut out = Vec::new();
    let mut decoded_packets = 0;
    let mut total_samples = 0;
    while let Some(packet) = track.reader.next_packet().expect("demux") {
        if packet.track_id != track.track_id {
            continue;
        }
        decoder
            .decode(&packet)
            .expect("decode")
            .copy_to_vec_interleaved::<f32>(&mut out);
        // The reader's terminal, empty flush packet legitimately decodes to
        // nothing for mp3 (nothing is held back across EOS), and it is always
        // the last one seen — `out` alone, checked only after the loop, would
        // read that packet's empty result rather than the file's real audio.
        total_samples += out.len();
        decoded_packets += 1;
    }
    assert!(decoded_packets > 0, "no audio packets decoded");
    assert!(total_samples > 0, "no samples decoded");
    let _ = std::fs::remove_file(&path);
}

/// Write `source` (one `Vec<f32>` per channel, all the same length) as an Ogg
/// Vorbis file through `ec_vorbis`/`ec_ogg` — the same two crates
/// `edith_replica::export::write_ogg` writes through — at scratch/`name`.
fn encode_ogg(source: &[Vec<f32>], rate: u32, path: &Path) {
    use ec_core::{
        AudioParameters, Buf, ChannelLayout, CodecId, CodecParameters, MediaParameters, Muxer,
        Packet, StreamInfo, TimeBase,
    };
    use ec_vorbis::{EncoderConfig, VorbisEncoder};

    let channels = source.len() as u16;
    let mut encoder = VorbisEncoder::new(EncoderConfig {
        sample_rate: rate,
        channels,
        bitrate_bps: 128_000,
        quality: 0.6,
    })
    .expect("vorbis encoder");
    let borrowed: Vec<&[f32]> = source.iter().map(|c| &c[..]).collect();
    encoder.push_planar(&borrowed).expect("push");
    encoder.finish();
    let mut packets = Vec::new();
    loop {
        match encoder.next_packet() {
            Ok(p) => packets.push((p.data, p.granule)),
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("encode: {e}"),
        }
    }
    eprintln!(
        "encode_ogg: {} packets, last granule {}",
        packets.len(),
        packets.last().map_or(0, |(_, g)| *g)
    );

    let base = TimeBase::new(1, i64::from(rate));
    let mut params = CodecParameters::new(CodecId::Vorbis);
    params.media = MediaParameters::Audio(AudioParameters {
        sample_rate: rate,
        layout: ChannelLayout::from_count(usize::from(channels)),
        format: None,
        bits_per_sample: None,
    });
    params.extradata = Some(Buf::from_vec(encoder.extradata()));
    let file = File::create(path).expect("create");
    let mut muxer = ec_ogg::OggMuxer::new(file);
    muxer
        .add_stream(StreamInfo::new(0, base, params))
        .expect("add stream");
    muxer.write_headers().expect("headers");
    for (data, granule) in &packets {
        let mut packet = Packet::new(0, base, data.clone());
        packet.side_data.push(ec_ogg::granule_side_data(*granule));
        muxer.write_packet(&packet).expect("packet");
    }
    muxer.finish().expect("finish");
}

/// The engine's own read door (`SymTrack` above, mirroring
/// `edith_replica::engine::audio::run_sym`): open, decode every packet to
/// EOF, count frames. This is what the export tests' `decode()` helper
/// measures against.
fn decode_all(path: &Path) -> u64 {
    let mut track = SymTrack::open(path).expect("track");
    let mut decoder = track.decoder();
    let mut out = Vec::new();
    let mut frames = 0u64;
    let mut n = 0;
    while let Some(packet) = track.reader.next_packet().expect("demux") {
        if packet.track_id != track.track_id {
            continue;
        }
        let before = frames;
        decoder
            .decode(&packet)
            .expect("decode")
            .copy_to_vec_interleaved::<f32>(&mut out);
        frames += (out.len() / usize::from(track.channels)) as u64;
        n += 1;
        eprintln!(
            "packet {n}: {} bytes in, {} frames out (running {frames})",
            packet.data.len(),
            frames - before
        );
    }
    frames
}

/// The exact shape of `audio_export::exports_the_timeline_as_an_ogg_vorbis`
/// and `a_mono_timeline_exports_as_dual_mono_ogg` in `edith_replica`: write N
/// samples through the family's own Ogg Vorbis encoder, read them back
/// through this shim exactly as the engine's `SymTrack`/`run_sym` do, and the
/// count must come back to N — the terminal hop `VorbisDecoder::flush`
/// delivers has to actually reach the reader at EOS.
#[test]
fn ogg_vorbis_round_trip_reads_back_every_sample_written() {
    let rate = 44_100u32;
    for (name, channels, n) in [
        ("surface-mono-44100.ogg", 2usize, 44_100usize),
        ("surface-stereo-264600.ogg", 2, 264_600),
    ] {
        let source: Vec<Vec<f32>> = (0..channels)
            .map(|_| {
                (0..n)
                    .map(|i| 0.2 * (i as f32 * 0.05).sin())
                    .collect::<Vec<f32>>()
            })
            .collect();
        let path = std::env::temp_dir().join(format!("edith-surface-{}-{name}", std::process::id()));
        encode_ogg(&source, rate, &path);
        let frames = decode_all(&path);
        eprintln!("{name}: wrote {n} frames, read back {frames}");
        assert_eq!(frames, n as u64, "{name}: round trip sample count");
        let _ = std::fs::remove_file(&path);
    }
}

/// ID3v2's syncsafe integer: 4 bytes, 7 significant bits each (top bit clear),
/// big-endian.
fn syncsafe(mut n: u32) -> [u8; 4] {
    let mut out = [0u8; 4];
    for b in out.iter_mut().rev() {
        *b = (n & 0x7f) as u8;
        n >>= 7;
    }
    out
}
