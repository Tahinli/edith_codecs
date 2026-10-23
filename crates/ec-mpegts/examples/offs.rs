use ec_core::registry::Demuxer;
use std::fs::File;
use std::io::BufReader;
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let mut d = ec_mpegts::TsDemuxer::new(BufReader::new(File::open(path).unwrap())).unwrap();
    let mut n = 0;
    while let Ok(p) = d.next_packet() {
        println!(
            "{} off {} pts {:?} dur {:?} len {}",
            p.stream,
            d.last_packet_offset(),
            p.pts,
            p.duration,
            p.data.len()
        );
        n += 1;
        if n > 14 {
            break;
        }
    }
}
