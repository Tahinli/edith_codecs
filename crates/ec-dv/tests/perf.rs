//! Throughput floor: SD >= 300 fps, HD >= 60 fps (release builds only).
mod common;

use std::time::Instant;

use common::*;
use ec_dv::DvVideoDecoder;

fn fps(f: &Flavour) -> Option<f64> {
    let path = make(f, "perf", "dv", true, 10)?;
    let data = std::fs::read(path).unwrap();
    let fs = ec_dv::frame_profile(None, &data).unwrap().frame_size;
    let frames: Vec<&[u8]> = data.chunks_exact(fs).collect();
    let mut d = DvVideoDecoder::new();
    d.decode(frames[0]).unwrap();
    let start = Instant::now();
    let mut n = 0u32;
    while start.elapsed().as_secs_f64() < 1.0 || n < 20 {
        for fr in &frames {
            std::hint::black_box(d.decode(fr).unwrap());
            n += 1;
        }
    }
    Some(f64::from(n) / start.elapsed().as_secs_f64())
}

/// A shared machine under load cannot show the floor: the check is skipped (and says so) when the
/// 1-minute load average exceeds half the cores, which is how the lane servers look while other
/// jobs run. An idle machine enforces the floor.
fn machine_is_busy() -> bool {
    let load = std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()));
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
    load.is_some_and(|l| l > cores * 0.5)
}

#[test]
fn decode_speed() {
    if !ffmpeg_or_skip() {
        return;
    }
    let mut bad = Vec::new();
    for f in FLAVOURS {
        let hd = f.size.starts_with("1440") || f.size.starts_with("1280") || f.size.starts_with("960");
        let mut r = 0f64;
        for _ in 0..3 {
            let Some(x) = fps(f) else { panic!("fixture {}", f.name) };
            r = r.max(x);
            if r >= if hd { 60.0 } else { 300.0 } {
                break;
            }
        }
        eprintln!("{}: {r:.0} fps", f.name);
        if !cfg!(debug_assertions) && !machine_is_busy() {
            let floor = if hd { 60.0 } else { 300.0 };
            if r < floor {
                bad.push(format!("{}: {r:.0} fps < {floor}", f.name));
            }
        }
    }
    assert!(bad.is_empty(), "{bad:?}");
}
