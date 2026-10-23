//! Open, full read and seek timings: `cargo run --release -p ec-mpegts --example bench -- file`.
use std::fs::File;
use std::io::BufReader;
use std::time::Instant;

use ec_core::registry::{Demuxer, SeekMode};
use ec_core::{TimeBase, Timestamp};

fn main() {
    let path = std::env::args().nth(1).expect("usage: bench <file>");
    let t = Instant::now();
    let mut d =
        ec_mpegts::TsDemuxer::new(BufReader::new(File::open(&path).expect("open"))).expect("demux");
    println!("open {:?}, duration {:?}", t.elapsed(), d.duration_secs());
    let t = Instant::now();
    let (mut n, mut bytes) = (0u64, 0u64);
    while let Ok(p) = d.next_packet() {
        n += 1;
        bytes += p.data.len() as u64;
    }
    let e = t.elapsed();
    println!(
        "read {n} packets, {} MB in {e:?} ({:.0} MB/s)",
        bytes >> 20,
        bytes as f64 / 1e6 / e.as_secs_f64()
    );
    let dur = d.duration_secs().unwrap_or(1.0);
    for frac in [0.1, 0.5, 0.9, 0.999] {
        let t = Instant::now();
        let target = (dur * frac * 90_000.0) as i64 + d.streams()[0].start_time.unwrap_or(0);
        d.seek(
            0,
            Timestamp::new(target, TimeBase::new(1, 90_000)),
            SeekMode::SyncBefore,
        )
        .unwrap();
        let p = d.next_packet().unwrap();
        println!(
            "seek {frac}: {:?} -> pts {:?} key {}",
            t.elapsed(),
            p.pts,
            p.flags.keyframe
        );
    }
}
