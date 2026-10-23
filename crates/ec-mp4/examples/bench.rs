//! bench: MP4 demux and mux throughput for lanes/bench.md ("lane benchaudio").
//!
//! usage:
//!   bench demux <in.mp4> <out_prefix> [reps]
//!   bench mux <in.mp4> <out.mp4> [reps]
//!
//! `demux` opens the file with `Mp4Demuxer` and reads every packet of every track, `reps` times
//! (default 5), and keeps the best wall time. It prints packets per second and payload MB/s. After
//! timing, one untimed pass writes each track's packet payloads, concatenated in order, to
//! `<out_prefix>.s<N>.bin` so the bytes can be compared with ffmpeg's `-c copy -f data` output.
//! `mux` demuxes the input once (untimed), then times writing all packets into a new MP4 with
//! `Mp4Muxer`, best of `reps`; the last output is kept at `out.mp4`.
use std::fs::File;
use std::hint::black_box;
use std::io::BufReader;
use std::time::Instant;

use ec_core::{Demuxer, Muxer, Packet, StreamInfo};
use ec_mp4::{Mp4Demuxer, Mp4Muxer};

/// Read every packet of the file once, in file order.
fn read_all(path: &str) -> (Vec<StreamInfo>, Vec<Packet>) {
    let mut demux = Mp4Demuxer::new(BufReader::new(File::open(path).expect("open"))).expect("mp4");
    let streams = demux.streams().to_vec();
    let mut packets = Vec::new();
    loop {
        match demux.next_packet() {
            Ok(p) => packets.push(p),
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("demux: {e}"),
        }
    }
    (streams, packets)
}

/// One timed demux pass: sums the payload bytes so the work cannot be skipped.
fn demux_pass(path: &str) -> (usize, u64) {
    let mut demux = Mp4Demuxer::new(BufReader::new(File::open(path).expect("open"))).expect("mp4");
    let (mut n, mut bytes) = (0usize, 0u64);
    loop {
        match demux.next_packet() {
            Ok(p) => {
                n += 1;
                bytes += p.data.len() as u64;
                black_box(&p.data);
            }
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("demux: {e}"),
        }
    }
    (n, bytes)
}

/// One timed mux pass into `out`.
fn mux_pass(out: &str, streams: &[StreamInfo], packets: &[Packet]) {
    let file = File::create(out).expect("create");
    let mut mux = Mp4Muxer::new(file).expect("muxer");
    for s in streams {
        mux.add_stream(s.clone()).expect("add stream");
    }
    for p in packets {
        mux.write_packet(p).expect("write packet");
    }
    mux.finish().expect("finish");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("demux") if args.len() >= 4 => {
            let reps: usize = args.get(4).map_or(5, |s| s.parse().expect("reps"));
            let file_bytes = std::fs::metadata(&args[2]).expect("stat").len();
            let (mut packets, mut bytes) = (0usize, 0u64);
            let mut best = f64::MAX;
            for _ in 0..reps {
                let t = Instant::now();
                (packets, bytes) = demux_pass(black_box(&args[2]));
                best = best.min(t.elapsed().as_secs_f64());
            }
            // Untimed: per-track payload dumps for the ffmpeg byte comparison.
            let (streams, all) = read_all(&args[2]);
            for s in &streams {
                let mut dump = Vec::new();
                for p in all.iter().filter(|p| p.stream == s.index) {
                    dump.extend_from_slice(&p.data);
                }
                std::fs::write(format!("{}.s{}.bin", args[3], s.index), &dump).expect("write dump");
            }
            println!(
                "packets={packets} payload_bytes={bytes} file_bytes={file_bytes} best_demux_s={best:.4} packets_per_s={:.0} mb_per_s={:.2} reps={reps}",
                packets as f64 / best,
                bytes as f64 / best / 1e6
            );
        }
        Some("mux") if args.len() >= 4 => {
            let reps: usize = args.get(4).map_or(5, |s| s.parse().expect("reps"));
            let (streams, packets) = read_all(&args[2]);
            let bytes: u64 = packets.iter().map(|p| p.data.len() as u64).sum();
            let mut best = f64::MAX;
            for _ in 0..reps {
                let t = Instant::now();
                mux_pass(black_box(&args[3]), &streams, &packets);
                best = best.min(t.elapsed().as_secs_f64());
            }
            println!(
                "packets={} payload_bytes={bytes} best_mux_s={best:.4} packets_per_s={:.0} mb_per_s={:.2} reps={reps}",
                packets.len(),
                packets.len() as f64 / best,
                bytes as f64 / best / 1e6
            );
        }
        _ => {
            eprintln!(
                "usage: bench demux <in.mp4> <out_prefix> [reps]\n       bench mux <in.mp4> <out.mp4> [reps]"
            );
            std::process::exit(2);
        }
    }
}
