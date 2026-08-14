//! The speed bar, measured the way the claim is made: one `encode_idr` call per
//! picture, zero threads on the caller's side.
//!
//! The incumbent this replaces (`oxideav-h265`) has no threads at all inside its
//! encoder — edith fans whole access units out across twelve cores to reach 4.30
//! fps at 1080p. The bar here is to beat that from *inside* one call, so the
//! measurement below hands one picture at a time to one encoder.
//!
//! Run under `--release`; in a debug build this prints the numbers and skips the
//! assertion, because a debug build measures the compiler, not the encoder.

mod common;

use common::test_frame;
use ec_h265::encoder::{Encoder, EncoderConfig, RateControl};
use std::time::Instant;

fn measure(width: u32, height: u32, threads: usize, frames: usize) -> f64 {
    let mut cfg = EncoderConfig::new(width, height);
    cfg.rate_control = RateControl::ConstantQp(27);
    cfg.threads = threads;
    let encoder = Encoder::new(cfg).expect("encoder");
    let pictures: Vec<_> = (0..frames)
        .map(|i| test_frame(width, height, i as u32 * 7))
        .collect();
    // One untimed picture so the allocator and the caches are warm.
    encoder.encode_idr(&pictures[0]).expect("warm-up encode");
    let start = Instant::now();
    for picture in &pictures {
        encoder.encode_idr(picture).expect("encode");
    }
    let elapsed = start.elapsed().as_secs_f64();
    frames as f64 / elapsed
}

#[test]
fn intra_1080p_beats_the_incumbent_on_twelve_threads() {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let frames = if cfg!(debug_assertions) { 1 } else { 6 };
    let single = measure(1920, 1080, 1, frames);
    let many = measure(1920, 1080, cores, frames);
    println!("1080p intra: {single:.2} fps on 1 thread, {many:.2} fps on {cores} threads, speed-up {:.2}x", many / single);
    if cfg!(debug_assertions) {
        println!("debug build: not asserting the bar");
        return;
    }
    assert!(
        many > 4.30,
        "1080p on {cores} threads: {many:.2} fps, incumbent bar is 4.30"
    );
    assert!(
        many > single * 2.0,
        "wavefront gained only {:.2}x",
        many / single
    );
}

#[test]
fn speedup_curve() {
    if cfg!(debug_assertions) {
        println!("debug build: skipping the speed-up curve");
        return;
    }
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut base = 0.0;
    for threads in [1usize, 2, 4, 8, cores] {
        if threads > cores {
            continue;
        }
        let fps = measure(1920, 1080, threads, 4);
        if threads == 1 {
            base = fps;
        }
        println!("threads {threads:2}: {fps:6.2} fps  speed-up {:.2}x", fps / base);
    }
}
