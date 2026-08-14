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

use common::{natural_frame, test_frame};
use ec_h265::encoder::{Encoder, EncoderConfig, RateControl};
use std::time::Instant;

fn measure(width: u32, height: u32, threads: usize, frames: usize) -> f64 {
    measure_with(width, height, threads, frames, natural_frame, None)
}

/// The 1-minute load average, which decides whether a number measured here
/// means anything.
fn load_average() -> f64 {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse().ok())
        .unwrap_or(0.0)
}

fn measure_with(
    width: u32,
    height: u32,
    threads: usize,
    frames: usize,
    make: fn(u32, u32, u32) -> ec_core::frame::VideoFrame,
    ctb: Option<usize>,
) -> f64 {
    let mut cfg = EncoderConfig::new(width, height);
    cfg.rate_control = RateControl::ConstantQp(27);
    cfg.threads = threads;
    if let Some(ctb) = ctb {
        cfg.ctb_size = ctb;
    }
    let encoder = Encoder::new(cfg).expect("encoder");
    let pictures: Vec<_> = (0..frames)
        .map(|i| make(width, height, i as u32 * 7))
        .collect();
    // One untimed picture so the allocator and the caches are warm, then the
    // best of three passes: this machine runs other work, and the fastest pass
    // is the one that measures the encoder rather than the neighbours.
    encoder.encode_idr(&pictures[0]).expect("warm-up encode");
    let mut best = f64::MAX;
    for _ in 0..3 {
        let start = Instant::now();
        for picture in &pictures {
            encoder.encode_idr(picture).expect("encode");
        }
        best = best.min(start.elapsed().as_secs_f64());
    }
    frames as f64 / best
}

#[test]
fn intra_1080p_beats_the_incumbent_on_twelve_threads() {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let frames = if cfg!(debug_assertions) { 1 } else { 6 };
    let single = measure(1920, 1080, 1, frames);
    let many = measure(1920, 1080, cores, frames);
    println!(
        "1080p intra, camera-like fixture: {single:.2} fps on 1 thread, {many:.2} fps on {cores} threads, speed-up {:.2}x",
        many / single
    );
    // The same measurement on the worst-case noise fixture, printed rather than
    // asserted: it is not what the 4.30 fps bar was measured on.
    let noisy = measure_with(1920, 1080, cores, frames, test_frame, None);
    println!("1080p intra, noise fixture: {noisy:.2} fps on {cores} threads");
    if cfg!(debug_assertions) {
        println!("debug build: not asserting the bar");
        return;
    }
    let load = load_average();
    if load > cores as f64 / 2.0 {
        println!(
            "load average {load:.1} on {cores} cores: the bar is not asserted against a busy machine"
        );
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

/// Why [`EncoderConfig::new`] defaults to 32x32 trees: the wavefront hands one
/// CTB *row* to a worker, so 1080 lines are 34 rows at 32 against 17 at 64, and
/// 17 rows dealt round-robin over twelve workers leaves seven of them with half
/// the work of the other five. Sizes are measured alternately so a machine whose
/// load drifts during the run cannot favour whichever went first.
#[test]
fn ctb_32_is_the_faster_tree_at_1080p() {
    if cfg!(debug_assertions) {
        println!("debug build: not measuring the tree size");
        return;
    }
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let (mut at32, mut at64) = (0.0f64, 0.0f64);
    for _ in 0..2 {
        at32 = at32.max(measure_with(1920, 1080, cores, 4, natural_frame, Some(32)));
        at64 = at64.max(measure_with(1920, 1080, cores, 4, natural_frame, Some(64)));
    }
    let single64 = measure_with(1920, 1080, 1, 4, natural_frame, Some(64));
    let single32 = measure_with(1920, 1080, 1, 4, natural_frame, Some(32));
    println!(
        "1080p on {cores} threads: CTB 32 {at32:.2} fps, CTB 64 {at64:.2} fps ({:.2}x); \
         on 1 thread: 32 {single32:.2} fps, 64 {single64:.2} fps ({:.2}x)",
        at32 / at64,
        single32 / single64
    );
    let load = load_average();
    if load > cores as f64 / 2.0 {
        println!("load average {load:.1} on {cores} cores: not asserting against a busy machine");
        return;
    }
    assert!(
        at32 > at64,
        "CTB 32 {at32:.2} fps did not beat CTB 64 {at64:.2} fps on {cores} threads"
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
        println!(
            "threads {threads:2}: {fps:6.2} fps  speed-up {:.2}x",
            fps / base
        );
    }
}
