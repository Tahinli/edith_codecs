//! Throughput of 1080p 4:2:2 HQ (ffmpeg-encoded, light noise so the entropy coder has work).
//! Prints fps for 1 and 4 threads; set `EC_PRORES_MIN_FPS` to assert the single-thread rate.

mod common;
use common::*;

#[test]
fn hd_hq_throughput() {
    if !ffmpeg_or_skip() {
        return;
    }
    let dir = scratch("perf");
    let mov = dir.join("hd.mov");
    assert!(run(&[
        "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=25,noise=alls=12:allf=t",
        "-frames:v", "24", "-c:v", "prores_ks", "-profile", "3", "-pix_fmt", "yuv422p10le",
        mov.to_str().unwrap(),
    ]));
    let bytes = std::fs::read(&mov).unwrap();
    let pk = packets(&bytes);
    let mut single = 0.0;
    for threads in [1usize, 4] {
        let mut dec = decoder(threads);
        // Warm up, then best of 10 passes.
        for p in &pk {
            dec.decode(p).unwrap();
        }
        let mut best = f64::MAX;
        for _ in 0..10 {
            let t0 = std::time::Instant::now();
            for p in &pk {
                std::hint::black_box(dec.decode(p).unwrap());
            }
            best = best.min(t0.elapsed().as_secs_f64());
        }
        let fps = pk.len() as f64 / best;
        eprintln!("1080p 422 HQ, {} bytes/frame, {threads} thread(s): {fps:.1} fps", bytes.len() / pk.len());
        if threads == 1 {
            single = fps;
        }
    }
    if let Some(min) = std::env::var("EC_PRORES_MIN_FPS").ok().and_then(|v| v.parse::<f64>().ok()) {
        assert!(single >= min, "single-thread {single:.1} fps below {min}");
    }
    let _ = std::fs::remove_dir_all(dir);
}
