//! Perf-lane probe: decode an IVF repeatedly, report MP/s and fps.
//! Dims come from the IVF header; usage: `throughput <ivf> [rounds]`.
use ec_vp8::decode::Decoder;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bytes = std::fs::read(&args[1]).unwrap();
    let w = u16::from_le_bytes([bytes[12], bytes[13]]) as u64;
    let h = u16::from_le_bytes([bytes[14], bytes[15]]) as u64;
    let rounds: u64 = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(20);
    let mut frames = Vec::new();
    let mut pos = 32;
    while pos + 12 <= bytes.len() {
        let sz = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        frames.push(bytes[pos + 12..pos + 12 + sz].to_vec());
        pos += 12 + sz;
    }
    let nframes = frames.len();
    let shown = frames.iter().filter(|f| (f[0] >> 4) & 1 == 1).count() as u64;
    let t = Instant::now();
    for _ in 0..rounds {
        let mut dec = Decoder::new();
        for f in &frames {
            dec.decode(f).unwrap();
        }
    }
    let dt = t.elapsed().as_secs_f64();
    let dec_mpx = rounds as f64 * nframes as f64 * w as f64 * h as f64 / 1e6 / dt;
    let out_mpx = rounds as f64 * shown as f64 * w as f64 * h as f64 / 1e6 / dt;
    println!(
        "{}x{rounds} frames ({shown} shown/round) in {dt:.3}s: \
         decode {dec_mpx:.1} MP/s, output {out_mpx:.1} MP/s \
         ({:.1} shown fps @ {w}x{h})",
        nframes,
        rounds as f64 * shown as f64 / dt
    );
}
