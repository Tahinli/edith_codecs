//! bench: Vorbis decode throughput for lanes/bench.md ("lane benchaudio").
//!
//! usage: bench dec <in.ogg> <out.f32le> [reps]
//!
//! Demuxes the first Vorbis stream of an Ogg file with `OggDemuxer`, builds `VorbisDecoder` from
//! the stream's header packets, and decodes every audio packet, `reps` times (default 5), keeping
//! the best wall time. The decoded interleaved f32 PCM of the last run is written to `out.f32le`
//! (no trimming: the comparison with ffmpeg aligns the two streams).
use std::fs::File;
use std::hint::black_box;
use std::io::BufReader;
use std::time::Instant;

use ec_core::{CodecId, Demuxer};
use ec_ogg::OggDemuxer;
use ec_vorbis::VorbisDecoder;

/// Read the file's Vorbis packets and its header bytes once: (extradata, audio packets, channels).
fn read_packets(path: &str) -> (Vec<u8>, Vec<Vec<u8>>, usize) {
    let mut demuxer = OggDemuxer::open(BufReader::new(File::open(path).expect("open"))).expect("ogg");
    let stream = demuxer
        .streams()
        .iter()
        .find(|s| s.params.codec == CodecId::Vorbis)
        .expect("a Vorbis stream")
        .clone();
    let extradata = stream.params.extradata.clone().expect("headers").to_vec();
    let channels = stream.params.audio().map_or(0, |a| a.layout.channel_count());
    let mut packets = Vec::new();
    loop {
        match demuxer.next_packet() {
            Ok(p) if p.stream == stream.index => packets.push(p.data.to_vec()),
            Ok(_) => {}
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("demux: {e}"),
        }
    }
    (extradata, packets, channels)
}

/// Decode every packet once: (interleaved samples, packets that failed).
fn decode_once(extradata: &[u8], packets: &[Vec<u8>]) -> (Vec<f32>, usize) {
    let mut decoder = VorbisDecoder::from_extradata(extradata).expect("headers parse");
    let mut out = Vec::new();
    let mut failed = 0usize;
    for p in packets {
        match decoder.decode_audio(p) {
            Ok(planes) => {
                let frames = planes.first().map_or(0, Vec::len);
                for i in 0..frames {
                    for plane in &planes {
                        out.push(plane[i]);
                    }
                }
            }
            Err(_) => failed += 1,
        }
    }
    (out, failed)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 || args[1] != "dec" {
        eprintln!("usage: bench dec <in.ogg> <out.f32le> [reps]");
        std::process::exit(2);
    }
    let reps: usize = args.get(4).map_or(5, |s| s.parse().expect("reps"));
    let (extradata, packets, channels) = read_packets(&args[2]);
    let (pcm, failed) = decode_once(&extradata, &packets);
    let mut best = f64::MAX;
    for _ in 0..reps {
        let t = Instant::now();
        black_box(decode_once(black_box(&extradata), black_box(&packets)).0.len());
        best = best.min(t.elapsed().as_secs_f64());
    }
    let mut bytes = Vec::with_capacity(pcm.len() * 4);
    for v in &pcm {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(&args[3], &bytes).expect("write output");
    println!(
        "packets={} failed={failed} channels={channels} samples={} best_decode_s={best:.4} reps={reps}",
        packets.len(),
        pcm.len() / channels.max(1)
    );
}
