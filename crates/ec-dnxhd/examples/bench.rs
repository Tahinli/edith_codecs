//! `bench <file.mov> [threads] [seconds]`: decode the frames of a DNxHD/DNxHR
//! MOV in a loop and print frames per second.
use ec_core::Demuxer;
use ec_core::registry::{CodecId, CodecParameters};
use ec_dnxhd::DnxhdDecoder;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: bench <file.mov> [threads] [seconds]");
    let threads: usize = args.next().map_or(6, |s| s.parse().unwrap());
    let secs: f64 = args.next().map_or(5.0, |s| s.parse().unwrap());
    let f = std::io::BufReader::new(std::fs::File::open(path).unwrap());
    let mut d = ec_mp4::Mp4Demuxer::new(f).unwrap();
    let mut frames = Vec::new();
    while let Ok(p) = d.next_packet() {
        frames.push(p.data.to_vec());
    }
    let mut dec = DnxhdDecoder::new(CodecParameters::new(CodecId::Dnxhd)).unwrap();
    dec.set_threads(threads);
    let start = std::time::Instant::now();
    let mut n = 0u64;
    while start.elapsed().as_secs_f64() < secs {
        for fr in &frames {
            if std::env::var_os("NATIVE").is_some() { std::hint::black_box(dec.decode_native(fr).unwrap()); } else { std::hint::black_box(dec.decode_bytes(fr).unwrap()); }
            n += 1;
        }
    }
    let t = start.elapsed().as_secs_f64();
    println!("{} frames in {:.2}s: {:.1} fps ({} threads)", n, t, n as f64 / t, threads);
}
