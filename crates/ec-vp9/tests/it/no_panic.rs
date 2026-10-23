//! No-panic robustness: truncated and corrupted streams must come back as
//! `Ok` or `Err`, never as a panic. Every mutation comes from a fixed-seed
//! generator, so a reported case replays bit-for-bit.
//!
//! The inputs are the crate's own committed fixtures (`tests/data/*.ivf`),
//! so the test needs no fixtures checkout.

use crate::ivf;

use ec_vp9::decode::Decoder;
use ec_vp9::stream::decode_stream;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

/// Fixed seed: a failing case is reproduced by re-running with the same seed.
const SEED: u64 = 0x5EED_00F0_0009;
/// Random container-level truncations per fixture.
const STREAM_CUTS: usize = 24;
/// Random container-level byte corruptions per fixture.
const STREAM_FLIPS: usize = 24;
/// Frames probed per fixture (the first few, where the reference state is
/// still small enough to replay cheaply).
const FRAMES_PER_FIXTURE: usize = 3;
/// Random mutations applied to each probed frame.
const FRAME_MUTATIONS: usize = 24;

/// xorshift64*: small, deterministic, dependency-free.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform-ish value in `0..n` (`n > 0`).
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// Flip 1..=8 random bytes of `buf` to random values.
    fn corrupt(&mut self, buf: &mut [u8]) {
        if buf.is_empty() {
            return;
        }
        for _ in 0..1 + self.below(8) {
            let at = self.below(buf.len());
            buf[at] = self.next_u64() as u8;
        }
    }
}

/// Every `*.ivf` in the crate's `tests/data`, sorted for a stable order.
fn fixtures() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "ivf"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no .ivf fixtures in {}", dir.display());
    paths
}

/// The panic payload as text, for the failure report.
fn payload_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

/// Run `f` and record a failure if it panics. The decoder's result is
/// discarded: `Ok` and `Err` are both acceptable outcomes here.
fn no_panic(failures: &mut Vec<String>, case: &str, f: impl FnOnce()) {
    if let Err(payload) = catch_unwind(AssertUnwindSafe(f)) {
        failures.push(format!("{case}: {}", payload_text(&*payload)));
    }
}

#[test]
fn truncated_and_corrupted_streams_never_panic() {
    let mut rng = Rng::new(SEED);
    let mut failures = Vec::new();
    let mut cases = 0usize;

    for path in fixtures() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(&path).expect("committed fixture");

        // Container level: whole-stream truncation and byte corruption.
        for _ in 0..STREAM_CUTS {
            let cut = rng.below(bytes.len());
            cases += 1;
            no_panic(&mut failures, &format!("{name} stream cut={cut}"), || {
                let _ = decode_stream(&bytes[..cut]);
            });
        }
        for flip in 0..STREAM_FLIPS {
            let mut buf = bytes.clone();
            rng.corrupt(&mut buf);
            cases += 1;
            no_panic(&mut failures, &format!("{name} stream flip={flip}"), || {
                let _ = decode_stream(&buf);
            });
        }

        // Frame level: a valid prefix, then one mutated frame.
        let (_, _, _, frames) = ivf::parse_ivf(&bytes);
        for f in 0..FRAMES_PER_FIXTURE.min(frames.len()) {
            for m in 0..FRAME_MUTATIONS {
                let mut frame = frames[f].data.clone();
                if m % 2 == 0 {
                    frame.truncate(rng.below(frame.len()));
                } else {
                    rng.corrupt(&mut frame);
                }
                cases += 1;
                let case = format!("{name} frame={f} mutation={m}");
                no_panic(&mut failures, &case, || {
                    let mut dec = Decoder::new();
                    for prior in &frames[..f] {
                        let _ = dec.decode(&prior.data);
                    }
                    let _ = dec.decode(&frame);
                });
            }
        }
    }

    assert!(cases > 0, "no mutation cases ran");
    assert!(
        failures.is_empty(),
        "{} of {cases} mutated inputs panicked (seed {SEED:#x}):\n{}",
        failures.len(),
        failures.iter().take(20).cloned().collect::<Vec<_>>().join("\n"),
    );
}
