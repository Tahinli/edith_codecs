//! Decode a raw .dv file repeatedly and print frames per second (optional 2nd arg: frame count).
use ec_dv::DvVideoDecoder;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&a[1]).unwrap();
    let limit: Option<u32> = a.get(2).and_then(|s| s.parse().ok());
    let fs = ec_dv::frame_profile(None, &data).unwrap().frame_size;
    let frames: Vec<&[u8]> = data.chunks_exact(fs).collect();
    let mut d = DvVideoDecoder::new();
    let t = std::time::Instant::now();
    let mut n = 0;
    while limit.map_or(t.elapsed().as_secs_f64() < 3.0, |l| n < l) {
        for f in &frames {
            std::hint::black_box(d.decode(f).unwrap());
            n += 1;
        }
    }
    println!("{:.0} fps", f64::from(n) / t.elapsed().as_secs_f64());
}
