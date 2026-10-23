use ec_core::registry::Demuxer;
use ec_mpegts::PsDemuxer;
fn main() {
    let p = std::env::args().nth(1).unwrap();
    let mut d = PsDemuxer::new(std::io::BufReader::new(std::fs::File::open(p).unwrap())).unwrap();
    for s in d.streams() {
        println!(
            "stream {} {:?} start {:?} dur {:?}",
            s.index, s.params.codec, s.start_time, s.duration
        );
    }
    for t in d.tracks() {
        println!("track {:04x} {:?} {:?}", t.pid, t.kind, t.refusal);
    }
    println!("dur {:?} stats {:?}", d.duration_secs(), d.stats());
    if let Some(t) = std::env::args().nth(3) {
        use ec_core::registry::SeekMode;
        let t: i64 = t.parse().unwrap();
        d.seek(
            0,
            ec_core::Timestamp::new(t, ec_core::TimeBase::new(1, 90000)),
            SeekMode::SyncBefore,
        )
        .unwrap();
        println!("landing {}", d.last_packet_offset());
    }
    let n: usize = std::env::args()
        .nth(2)
        .map(|x| x.parse().unwrap())
        .unwrap_or(40);
    for _ in 0..n {
        let Ok(p) = d.next_packet() else { break };
        println!(
            "s{} pts {:?} dts {:?} k{} len {} off {}",
            p.stream,
            p.pts,
            p.dts,
            p.flags.keyframe as u8,
            p.data.len(),
            d.last_packet_offset()
        );
    }
}
