//! Robustness witness: malformed VP8 input must produce an error, never a
//! panic. Every IVF fixture is decoded truncated at 10 positions and with 50
//! seeded single-byte corruptions; each decode runs under `catch_unwind` so a
//! failure names the file and the damage that caused it.

// Only `fixture_dir` is used here; the IVF parser serves the other test crates.
#[allow(dead_code)]
#[path = "it/common.rs"]
mod ivf;

use std::panic::{AssertUnwindSafe, catch_unwind};

use ec_vp8::stream::decode_stream;

/// The 12 IVF fixtures of the witness set (same list as `stream_exact.rs`).
const FIXTURES: &[&str] = &[
    "kf-16x16-black-q0.ivf",
    "kf-32x16-q35.ivf",
    "kf-76x52-q30.ivf",
    "kf-96x80-q63.ivf",
    "kf-160x96-q20.ivf",
    "kf-160x96-q40.ivf",
    "kf-172x144-q25.ivf",
    "gop-96x80-v1.ivf",
    "gop-160x96.ivf",
    "mparts-160x96.ivf",
    "altref-160x96.ivf",
    "clip-obs-320x192.ivf",
];

const TRUNCATIONS: usize = 10;
const CORRUPTIONS: usize = 50;
const SEED: u64 = 0x5EED_0F08;

/// splitmix64: a small, fixed-seed generator so every run sees the same damage.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Decodes `data` and fails the test if the decoder panics. Errors are fine.
fn decode_must_not_panic(data: &[u8], what: &str) {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let _ = decode_stream(data);
    }));
    assert!(outcome.is_ok(), "decoder panicked on {what}");
}

#[test]
fn truncated_and_corrupted_fixtures_never_panic() {
    let Some(dir) = ivf::fixture_dir() else {
        panic!("fixtures missing; run scripts/gen_vp8_fixtures.sh");
    };
    // Silence the default panic hook while probing; failures are reported
    // by the assertion above, which names the file and the damage.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    let mut rng = Rng(SEED);
    let mut failures = Vec::new();
    for name in FIXTURES {
        let bytes = std::fs::read(dir.join(name)).unwrap();
        assert!(bytes.len() > 32, "{name}: fixture too small");

        for k in 1..=TRUNCATIONS {
            let cut = bytes.len() * k / (TRUNCATIONS + 1);
            let what = format!("{name} truncated to {cut} of {} bytes", bytes.len());
            if catch_unwind(AssertUnwindSafe(|| {
                let _ = decode_stream(&bytes[..cut]);
            }))
            .is_err()
            {
                failures.push(what);
            }
        }

        for i in 0..CORRUPTIONS {
            let at = rng.below(bytes.len());
            let flip = (rng.below(255) + 1) as u8;
            let mut damaged = bytes.clone();
            damaged[at] ^= flip;
            let what = format!("{name} corruption {i}: byte {at} ^= {flip:#04x}");
            if catch_unwind(AssertUnwindSafe(|| {
                let _ = decode_stream(&damaged);
            }))
            .is_err()
            {
                failures.push(what);
            }
        }
    }

    std::panic::set_hook(hook);
    assert!(failures.is_empty(), "decoder panicked on {} inputs: {failures:#?}", failures.len());
}

#[test]
fn decode_must_not_panic_helper_accepts_errors() {
    // Garbage that no VP8 decoder can accept; the contract is only "no panic".
    decode_must_not_panic(&[0xff; 64], "all-0xff buffer");
    decode_must_not_panic(&[], "empty buffer");
}
