//! Lists the programs, tracks and streams of a transport stream and counts its
//! packets: `cargo run -p ec-mpegts --example dump -- file.m2ts`.

use std::fs::File;
use std::io::BufReader;

use ec_core::registry::Demuxer;
use ec_mpegts::TsDemuxer;

fn main() {
    let path = std::env::args().nth(1).expect("usage: dump <file>");
    let mut d = TsDemuxer::new(BufReader::new(File::open(&path).expect("open"))).expect("demux");
    println!("packet size {} bdav {}", d.packet_size(), d.is_bdav());
    for p in d.programs() {
        println!(
            "program {} pmt {:#x} pcr {:?} streams {}",
            p.number,
            p.pmt_pid,
            p.pcr_pid,
            p.streams.len()
        );
    }
    for t in d.tracks() {
        println!(
            "track pid {:#x} type {:#x} {:?} stream {:?} refusal {:?}",
            t.pid, t.stream_type, t.kind, t.stream, t.refusal
        );
    }
    for s in d.streams() {
        println!(
            "stream {} {} start {:?} duration {:?}",
            s.index,
            s.params.codec.name(),
            s.start_time,
            s.duration
        );
    }
    println!("duration {:?}", d.duration_secs());
    let mut counts = vec![0u64; d.streams().len()];
    while let Ok(p) = d.next_packet() {
        counts[p.stream as usize] += 1;
    }
    println!("packets {counts:?} stats {:?}", d.stats());
}
