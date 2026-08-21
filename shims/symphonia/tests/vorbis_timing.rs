use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::{
    AudioParameters, Buf, ChannelLayout, CodecId, CodecParameters, Demuxer, MediaParameters, Muxer,
    Packet, StreamInfo, TimeBase,
};
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoderOptions};
use symphonia_core::formats::probe::Hint;
use symphonia_core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia_core::io::MediaSourceStream;
use symphonia_core::meta::MetadataOptions;

const CHANNELS: u16 = 2;
const FIRST_PACKET_FRAMES: usize = 1024;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("symphonia-timing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir.join(name)
}

fn samples(rate: u32) -> Vec<i16> {
    let tone = (rate / 2) as usize;
    let gap = rate as usize;
    let frames = tone + gap + tone;
    let mut out = Vec::with_capacity(frames * usize::from(CHANNELS));
    for frame in 0..frames {
        let active = frame < tone || frame >= tone + gap;
        for ch in 0..CHANNELS {
            let hz = if ch == 0 { 440.0 } else { 660.0 };
            let v = if active {
                (2.0 * std::f64::consts::PI * hz * frame as f64 / f64::from(rate)).sin() * 0.45
            } else {
                0.0
            };
            out.push((v * f64::from(i16::MAX)) as i16);
        }
    }
    out
}

fn write_ogg(out: &Path, samples: &[i16], rate: u32) {
    let mut encoder = rusty_vorbis::VorbisEncoder::new(rusty_vorbis::VorbisEncoderConfig {
        bitrate_bps: rusty_vorbis::BITRATE_NOMINAL,
        quality: 0.85,
    });
    encoder
        .push_pcm_s16(samples, CHANNELS, rate)
        .expect("encode");
    encoder.finish();
    let mut packets = Vec::new();
    loop {
        match encoder.next_packet() {
            Ok(packet) => packets.push(packet),
            Err(rusty_vorbis::Error::Eof) => break,
            Err(e) => panic!("encode: {e}"),
        }
    }

    let extradata = ec_ogg::xiph_lace(&[
        &packets[0].data[..],
        &packets[1].data[..],
        &packets[2].data[..],
    ])
    .expect("headers lace");
    let time_base = TimeBase::new(1, i64::from(rate));
    let mut params = CodecParameters::new(CodecId::Vorbis);
    params.media = MediaParameters::Audio(AudioParameters {
        sample_rate: rate,
        layout: ChannelLayout::from_count(usize::from(CHANNELS)),
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
        let mut out = Packet::new(0, time_base, packet.data.clone());
        out.side_data.push(ec_ogg::granule_side_data(packet.pts));
        out.duration = Some(packet.duration);
        muxer.write_packet(&out).expect("packet");
    }
    muxer.finish().expect("trailer");
}

fn ffmpeg_decode(path: &Path) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-acodec", "pcm_f32le", "-"])
        .output()
        .expect("ffmpeg");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn probe_decode(path: &Path) -> Vec<f32> {
    let mut demuxer = ec_ogg::OggDemuxer::open(File::open(path).expect("open")).expect("demux");
    let params = demuxer.streams()[0].params.clone();
    let mut decoder = ec_probe::AudioDecoder::new(&params).expect("decoder");
    let mut out = Vec::new();
    let mut chunk = Vec::new();
    loop {
        match demuxer.next_packet() {
            Ok(packet) => {
                decoder.decode(&packet, &mut chunk).expect("decode");
                out.extend_from_slice(&chunk);
            }
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("demux: {e}"),
        }
    }
    decoder.flush(&mut chunk).expect("flush");
    out.extend_from_slice(&chunk);
    out
}

fn shim_reader(path: &Path) -> Box<dyn FormatReader> {
    let mss = MediaSourceStream::new(
        Box::new(File::open(path).expect("open")),
        Default::default(),
    );
    let mut hint = Hint::new();
    hint.with_extension("ogg");
    symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .expect("probe")
}

fn audio_track(reader: &dyn FormatReader) -> (u32, AudioCodecParameters) {
    let track = reader.default_track(TrackType::Audio).expect("track");
    let track_id = track.id;
    let params = match track.codec_params.clone().expect("params") {
        symphonia_core::codecs::CodecParameters::Audio(params) => params,
        _ => panic!("audio params"),
    };
    (track_id, params)
}

fn decode_from_reader(
    reader: &mut dyn FormatReader,
    track_id: u32,
    params: &AudioCodecParameters,
) -> Vec<f32> {
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .expect("decoder");
    let mut out = Vec::new();
    let mut chunk = Vec::new();
    while let Some(packet) = reader.next_packet().expect("packet") {
        if packet.track_id != track_id {
            continue;
        }
        decoder
            .decode(&packet)
            .expect("decode")
            .copy_to_vec_interleaved::<f32>(&mut chunk);
        out.extend_from_slice(&chunk);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PacketTiming {
    data: Vec<u8>,
    ts: i64,
    granule: Option<i64>,
}

fn packet_prefix(reader: &mut dyn FormatReader, track_id: u32, n: usize) -> Vec<PacketTiming> {
    let mut out = Vec::new();
    while out.len() < n {
        let packet = reader
            .next_packet()
            .expect("packet")
            .expect("enough packets");
        if packet.track_id == track_id && !packet.data.is_empty() {
            out.push(PacketTiming {
                data: packet.data.as_ref().to_vec(),
                ts: packet.pts.value() as i64,
                granule: packet.granule,
            });
        }
    }
    out
}

fn probe_packet_prefix(path: &Path, n: usize) -> Vec<PacketTiming> {
    let mut demuxer = ec_ogg::OggDemuxer::open(File::open(path).expect("open")).expect("demux");
    let mut out = Vec::new();
    while out.len() < n {
        match demuxer.next_packet() {
            Ok(packet) => {
                if packet.stream == 0 {
                    out.push(PacketTiming {
                        data: packet.data.as_ref().to_vec(),
                        ts: packet.pts.unwrap_or(0),
                        granule: ec_ogg::granule_of(&packet),
                    });
                }
            }
            Err(e) if e.is_eof() => panic!("enough packets"),
            Err(e) => panic!("demux: {e}"),
        }
    }
    out
}

fn assert_packet_timing(rate: u32, label: &str, left: &[PacketTiming], right: &[PacketTiming]) {
    assert_eq!(left.len(), right.len(), "{rate}: {label} packet count");
    for (i, (left, right)) in left.iter().zip(right).enumerate() {
        assert_eq!(left.ts, right.ts, "{rate}: {label} packet {i} ts");
        assert_eq!(
            left.granule, right.granule,
            "{rate}: {label} packet {i} granule"
        );
        assert_eq!(left.data, right.data, "{rate}: {label} packet {i} payload");
    }
}

fn seek_to_start(reader: &mut dyn FormatReader, track_id: u32) {
    reader
        .seek(
            SeekMode::Accurate,
            SeekTo::TimeStamp {
                ts: symphonia_core::units::Timestamp::new(0),
                track_id,
            },
        )
        .expect("seek");
}

fn shim_decode(path: &Path) -> Vec<f32> {
    let mut reader = shim_reader(path);
    let (track_id, params) = audio_track(&*reader);
    decode_from_reader(&mut *reader, track_id, &params)
}

fn rms(window: &[f32]) -> f64 {
    (window
        .iter()
        .map(|&v| f64::from(v) * f64::from(v))
        .sum::<f64>()
        / window.len() as f64)
        .sqrt()
}

fn tone_rms(samples: &[f32], rate: u32) -> f64 {
    let channels = usize::from(CHANNELS);
    let start = FIRST_PACKET_FRAMES * channels;
    let end = (rate / 2) as usize * channels;
    rms(&samples[start..end.min(samples.len())])
}

fn gap_rms(samples: &[f32], rate: u32) -> f64 {
    let channels = usize::from(CHANNELS);
    let onset = ((rate / 2 + rate) as usize) * channels;
    let start = onset - ((rate / 20) as usize) * channels;
    rms(&samples[start..onset.min(samples.len())])
}

fn first_tone_onset(samples: &[f32], rate: u32) -> usize {
    let channels = usize::from(CHANNELS);
    let threshold = (tone_rms(samples, rate) * 0.25) as f32;
    let search = rate as usize * channels;
    samples[search..]
        .chunks_exact(channels)
        .position(|frame| frame.iter().any(|v| v.abs() >= threshold))
        .map(|frame| frame + search / channels)
        .expect("tone onset")
}

fn max_abs_after_first_packet(a: &[f32], b: &[f32]) -> f32 {
    let skip = FIRST_PACKET_FRAMES * usize::from(CHANNELS);
    a[skip..]
        .iter()
        .zip(&b[skip..])
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

#[test]
fn ogg_timing_matches_reference_decoder() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skip: ffmpeg not on PATH");
        return;
    }

    for rate in [44_100, 48_000] {
        let path = scratch(&format!("timing-{rate}.ogg"));
        write_ogg(&path, &samples(rate), rate);
        let shim = shim_decode(&path);
        let probe = probe_decode(&path);
        let reference = ffmpeg_decode(&path);

        let shim_gap = gap_rms(&shim, rate);
        let probe_gap = gap_rms(&probe, rate);
        let reference_gap = gap_rms(&reference, rate);
        let shim_tone = tone_rms(&shim, rate);
        let probe_tone = tone_rms(&probe, rate);
        let reference_tone = tone_rms(&reference, rate);
        let gap_bound = reference_tone * 0.001;
        let shim_onset = first_tone_onset(&shim, rate);
        let probe_onset = first_tone_onset(&probe, rate);
        let reference_onset = first_tone_onset(&reference, rate);
        let shim_max = max_abs_after_first_packet(&shim, &reference);
        let probe_max = max_abs_after_first_packet(&probe, &reference);
        eprintln!(
            "rate={rate} frames shim={} probe={} ref={} tone shim={shim_tone:.8} probe={probe_tone:.8} ref={reference_tone:.8} gap_bound={gap_bound:.8} gap shim={shim_gap:.8} probe={probe_gap:.8} ref={reference_gap:.8} onset shim={shim_onset} probe={probe_onset} ref={reference_onset} max shim={shim_max:.8} probe={probe_max:.8}",
            shim.len() / usize::from(CHANNELS),
            probe.len() / usize::from(CHANNELS),
            reference.len() / usize::from(CHANNELS),
        );

        assert_eq!(shim.len(), reference.len(), "{rate}: shim length");
        assert_eq!(probe.len(), reference.len(), "{rate}: probe length");
        assert!(
            shim_gap <= gap_bound,
            "{rate}: shim gap RMS {shim_gap:.8} exceeds 60 dB-down absolute bound {gap_bound:.8}; tone RMS {shim_tone:.8}"
        );
        assert!(
            probe_gap <= gap_bound,
            "{rate}: probe gap RMS {probe_gap:.8} exceeds 60 dB-down absolute bound {gap_bound:.8}; tone RMS {probe_tone:.8}"
        );
        assert!(
            reference_gap <= gap_bound,
            "{rate}: reference gap RMS {reference_gap:.8} exceeds 60 dB-down absolute bound {gap_bound:.8}; tone RMS {reference_tone:.8}"
        );
        assert!(
            shim_onset.abs_diff(probe_onset) <= 1,
            "{rate}: shim onset {shim_onset} vs probe onset {probe_onset}"
        );
        assert!(
            shim_onset.abs_diff(reference_onset) <= 1,
            "{rate}: shim onset {shim_onset} vs reference onset {reference_onset}"
        );
        assert!(shim_max <= 1e-6, "{rate}: shim differs by {shim_max}");
        assert!(probe_max <= 1e-6, "{rate}: probe differs by {probe_max}");
    }
}

#[test]
fn seek_to_start_replays_fresh_packet_timing() {
    for rate in [44_100, 48_000] {
        let path = scratch(&format!("seek-start-{rate}.ogg"));
        write_ogg(&path, &samples(rate), rate);

        let mut fresh_reader = shim_reader(&path);
        let (track_id, params) = audio_track(&*fresh_reader);
        let fresh_packets = packet_prefix(&mut *fresh_reader, track_id, 8);
        let probe_packets = probe_packet_prefix(&path, 8);
        assert_packet_timing(rate, "shim vs probe", &fresh_packets, &probe_packets);

        let mut seeked_reader = shim_reader(&path);
        let (seeked_track, seeked_params) = audio_track(&*seeked_reader);
        assert_eq!(seeked_track, track_id);
        assert_eq!(seeked_params.sample_rate, params.sample_rate);
        let _ = packet_prefix(&mut *seeked_reader, seeked_track, 5);
        seek_to_start(&mut *seeked_reader, seeked_track);
        let seeked_packets = packet_prefix(&mut *seeked_reader, seeked_track, 8);
        assert_packet_timing(rate, "seek(0) vs probe", &seeked_packets, &probe_packets);

        eprintln!(
            "rate={rate} fresh_first_granule={:?} seek_first_granule={:?}",
            fresh_packets[0].granule, seeked_packets[0].granule
        );
        assert_packet_timing(rate, "seek(0) vs fresh", &seeked_packets, &fresh_packets);

        let fresh_pcm = shim_decode(&path);
        let mut decode_reader = shim_reader(&path);
        let (decode_track, decode_params) = audio_track(&*decode_reader);
        let _ = packet_prefix(&mut *decode_reader, decode_track, 5);
        seek_to_start(&mut *decode_reader, decode_track);
        let seeked_pcm = decode_from_reader(&mut *decode_reader, decode_track, &decode_params);
        assert_eq!(seeked_pcm, fresh_pcm, "{rate}: decoded PCM after seek(0)");
        let fresh_onset = first_tone_onset(&fresh_pcm, rate);
        let seeked_onset = first_tone_onset(&seeked_pcm, rate);
        assert!(
            seeked_onset.abs_diff(fresh_onset) <= 1,
            "{rate}: seek(0) onset {seeked_onset} vs fresh onset {fresh_onset}"
        );

        let mut mid_reader = shim_reader(&path);
        let (mid_track, mid_params) = audio_track(&*mid_reader);
        let target = i64::from(rate);
        let landed = mid_reader
            .seek(
                SeekMode::Accurate,
                SeekTo::TimeStamp {
                    ts: symphonia_core::units::Timestamp::new(target),
                    track_id: mid_track,
                },
            )
            .expect("mid seek")
            .actual_ts
            .value();
        let mid_pcm = decode_from_reader(&mut *mid_reader, mid_track, &mid_params);
        let channels = usize::from(CHANNELS);
        let start = landed.max(0) as usize * channels;
        let suffix = &fresh_pcm[start..];
        let trim = mid_pcm.len().saturating_sub(suffix.len());
        eprintln!(
            "rate={rate} mid_landed={landed} mid_trim_frames={}",
            trim / channels
        );
        assert!(
            trim <= FIRST_PACKET_FRAMES * channels,
            "{rate}: mid-stream trim too large: {} samples",
            trim
        );
    }
}
