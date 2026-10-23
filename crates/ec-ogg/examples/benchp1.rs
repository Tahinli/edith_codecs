//! benchp1: Ogg demux throughput for lanes/bench.md ("lane benchp1").
//!
//! usage: benchp1 <file.ogg> [reps]
//!
//! Opens the file and reads every packet with `OggDemuxer`, `reps` times (default 5), keeping the
//! best wall time. Prints the packet count and byte total (checked against ffprobe) and the time.
use std::fs::File;
use std::hint::black_box;
use std::io::BufReader;
use std::time::Instant;

use ec_core::Demuxer;
use ec_ogg::OggDemuxer;

/// Demux the whole file once: (packets, bytes).
fn demux_once(path: &str) -> (u64, u64) {
    let mut demux = OggDemuxer::open(BufReader::new(File::open(path).expect("open"))).expect("open ogg");
    let (mut packets, mut bytes) = (0u64, 0u64);
    loop {
        match demux.next_packet() {
            Ok(p) => {
                packets += 1;
                bytes += p.data.len() as u64;
                black_box(&p);
            }
            Err(e) if e.is_eof() => break,
            Err(e) => panic!("demux: {e}"),
        }
    }
    (packets, bytes)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let reps: usize = args.get(2).map_or(5, |s| s.parse().expect("reps"));
    let (packets, bytes) = demux_once(&args[1]);
    let mut best = f64::MAX;
    for _ in 0..reps {
        let t = Instant::now();
        black_box(demux_once(black_box(&args[1])));
        best = best.min(t.elapsed().as_secs_f64());
    }
    println!(
        "packets={packets} bytes={bytes} best_demux_s={best:.4} mb_s={:.2} reps={reps}",
        bytes as f64 / best / 1e6
    );
}
