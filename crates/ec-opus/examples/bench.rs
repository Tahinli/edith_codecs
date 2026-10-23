//! bench: Opus decode and encode throughput for lanes/bench.md ("lane benchaudio").
//!
//! usage:
//!   bench dec <in.ogg> <out.f32le> [reps]
//!   bench enc <in.f32le> <channels> <kbps> <out.ogg> [reps]
//!
//! `dec` demuxes an Ogg Opus file with `OggDemuxer` (the OpusHead and OpusTags packets are not
//! audio packets) and decodes every packet with `Decoder` at 48 kHz, `reps` times (default 5),
//! keeping the best wall time. The decoded interleaved f32 PCM of the last run, before the
//! pre-skip trim, is written to `out.f32le`.
//! `enc` encodes raw interleaved 48 kHz f32le PCM with `Encoder` (Application::Audio, CBR at
//! `kbps`, 20 ms frames) and muxes it to Ogg Opus, best of `reps` encode runs.
use std::fs::File;
use std::hint::black_box;
use std::io::BufReader;
use std::time::Instant;

use ec_core::{CodecId, Demuxer, Packet as CorePacket, StreamInfo, TimeBase, Muxer};
use ec_ogg::OggDemuxer;
use ec_opus::{Application, Decoder, Encoder, ogg::opus_head, Packet as OpusPacket};

const RATE: u32 = 48_000;
/// 20 ms at 48 kHz, per channel.
const FRAME: usize = 960;

/// The audio packets of the first Opus stream, and its channel count from OpusHead.
fn read_packets(path: &str) -> (usize, Vec<Vec<u8>>) {
    let mut demuxer = OggDemuxer::open(BufReader::new(File::open(path).expect("open"))).expect("ogg");
    let stream = demuxer
        .streams()
        .iter()
        .find(|s| s.params.codec == CodecId::Opus)
        .expect("an Opus stream")
        .clone();
    let head = stream.params.extradata.clone().expect("OpusHead");
    let channels = usize::from(head[9]);
    let mut packets = Vec::new();
    loop {
        match demuxer.next_packet() {
            Ok(p) if p.stream == stream.index => packets.push(p.data.to_vec()),
            Ok(_) => {}
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("demux: {e}"),
        }
    }
    (channels, packets)
}

/// Decode every packet once: (interleaved samples, packets that failed).
fn decode_once(packets: &[Vec<u8>], channels: usize) -> (Vec<f32>, usize) {
    let mut decoder = Decoder::new(RATE, channels).expect("decoder");
    let mut out = Vec::new();
    let mut buf = vec![0f32; 5760 * channels];
    let mut failed = 0usize;
    for p in packets {
        match decoder.decode_float(p, &mut buf) {
            Ok(n) => out.extend_from_slice(&buf[..n * channels]),
            Err(_) => failed += 1,
        }
    }
    (out, failed)
}

fn le_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>().0.iter()
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Encode interleaved 48 kHz PCM into (Opus packets, pre-skip) with 20 ms frames.
fn encode_once(pcm: &[f32], channels: usize, kbps: u32) -> (Vec<Vec<u8>>, usize) {
    let mut encoder = Encoder::new(RATE, channels, Application::Audio).expect("encoder");
    encoder.set_bitrate(kbps * 1000);
    encoder.set_vbr_constrained(false);
    let pre_skip = encoder.look_ahead(FRAME);
    let frames = (pcm.len() / channels).div_ceil(FRAME);
    let mut packets = Vec::with_capacity(frames);
    let mut frame = vec![0f32; FRAME * channels];
    let mut out = vec![0u8; 1500];
    for f in 0..frames {
        frame.fill(0.0);
        let start = f * FRAME * channels;
        let end = (start + FRAME * channels).min(pcm.len());
        frame[..end - start].copy_from_slice(&pcm[start..end]);
        let n = encoder.encode_float(&frame, FRAME, &mut out).expect("encode");
        packets.push(out[..n].to_vec());
    }
    (packets, pre_skip)
}

/// Mux the packets into Ogg Opus. The last packet's duration ends at the true input length.
fn write_ogg(path: &str, packets: &[Vec<u8>], channels: usize, total: usize, pre_skip: usize) {
    let file = std::io::BufWriter::new(File::create(path).expect("create"));
    let mut mux = ec_ogg::OggMuxer::new(file);
    let tb = TimeBase::from_rate(RATE);
    let mut params = ec_core::CodecParameters::new(CodecId::Opus);
    params.extradata = Some(opus_head(channels as u8, pre_skip as u16, RATE, 0, None).into());
    if let ec_core::MediaParameters::Audio(a) = &mut params.media {
        a.sample_rate = RATE;
        a.layout = ec_core::ChannelLayout::from_count(channels);
    }
    mux.add_stream(StreamInfo::new(0, tb, params)).expect("stream");
    let mut pts = 0i64;
    for (idx, p) in packets.iter().enumerate() {
        let dur = OpusPacket::parse(p, false).expect("packet").samples_48k() as i64;
        let dur = if idx == packets.len() - 1 {
            total as i64 + pre_skip as i64 - pts
        } else {
            dur
        };
        let pkt = CorePacket::new(0, tb, p.clone()).with_pts(pts).with_duration(dur);
        mux.write_packet(&pkt).expect("write");
        pts += OpusPacket::parse(p, false).expect("packet").samples_48k() as i64;
    }
    mux.finish().expect("finish");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("dec") if args.len() >= 4 => {
            let reps: usize = args.get(4).map_or(5, |s| s.parse().expect("reps"));
            let (channels, packets) = read_packets(&args[2]);
            let (pcm, failed) = decode_once(&packets, channels);
            let mut best = f64::MAX;
            for _ in 0..reps {
                let t = Instant::now();
                black_box(decode_once(black_box(&packets), channels).0.len());
                best = best.min(t.elapsed().as_secs_f64());
            }
            let mut bytes = Vec::with_capacity(pcm.len() * 4);
            for v in &pcm {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            std::fs::write(&args[3], &bytes).expect("write output");
            println!(
                "packets={} failed={failed} samples={} best_decode_s={best:.4} reps={reps}",
                packets.len(),
                pcm.len() / channels
            );
        }
        Some("enc") if args.len() >= 6 => {
            let channels: usize = args[3].parse().expect("channels");
            let kbps: u32 = args[4].parse().expect("kbps");
            let reps: usize = args.get(6).map_or(5, |s| s.parse().expect("reps"));
            let pcm = le_f32(&std::fs::read(&args[2]).expect("read input"));
            let total = pcm.len() / channels;
            let (packets, pre_skip) = encode_once(&pcm, channels, kbps);
            let mut best = f64::MAX;
            for _ in 0..reps {
                let t = Instant::now();
                black_box(encode_once(black_box(&pcm), channels, kbps).0.len());
                best = best.min(t.elapsed().as_secs_f64());
            }
            write_ogg(&args[5], &packets, channels, total, pre_skip);
            let bytes: usize = packets.iter().map(Vec::len).sum();
            println!(
                "packets={} bytes={bytes} best_encode_s={best:.4} reps={reps} audio_s={:.3}",
                packets.len(),
                total as f64 / f64::from(RATE)
            );
        }
        _ => {
            eprintln!(
                "usage: bench dec <in.ogg> <out.f32le> [reps]\n       bench enc <in.f32le> <channels> <kbps> <out.ogg> [reps]"
            );
            std::process::exit(2);
        }
    }
}
