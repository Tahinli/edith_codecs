//! bench: Matroska demux throughput for lanes/bench.md ("lane benchrest").
//!
//! usage:
//!   bench demux <in.mkv> <out_prefix> [reps]
//!
//! `demux` opens the file with `MatroskaDemuxer` and reads every packet of every track, `reps`
//! times (default 5), and keeps the best wall time. It prints packets per second and payload MB/s.
//! After timing, one untimed pass writes each track's packet payloads, concatenated in order, to
//! `<out_prefix>.s<N>.bin` so the bytes can be compared with ffmpeg's `-c copy -f data` output.
use std::fs::File;
use std::hint::black_box;
use std::io::BufReader;
use std::time::Instant;

use ec_core::{Demuxer, Packet, StreamInfo};
use ec_matroska::MatroskaDemuxer;

/// Read every packet of the file once, in file order.
fn read_all(path: &str) -> (Vec<StreamInfo>, Vec<Packet>) {
    let mut demux =
        MatroskaDemuxer::new(BufReader::new(File::open(path).expect("open"))).expect("mkv");
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
    let mut demux =
        MatroskaDemuxer::new(BufReader::new(File::open(path).expect("open"))).expect("mkv");
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("demux") if args.len() >= 4 => {
            let reps: usize = args.get(4).map_or(5, |s| s.parse().expect("reps"));
            assert!(reps >= 1, "reps must be at least 1");
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
        _ => {
            eprintln!("usage: bench demux <in.mkv> <out_prefix> [reps]");
            std::process::exit(2);
        }
    }
}
