//! No-panic floor for the decoder on malformed input.
//!
//! For each of the first 10 JVT conformance streams (the same name-sorted walk
//! `conformance.rs` uses), truncate at 8 positions and flip one byte at 30
//! fixed-seed positions. Every mutant is decoded end to end. An `Err` is fine;
//! a panic fails the test.
//!
//! Fixture-gated like the other conformance tests: without `fixtures/` the test
//! skips loudly, or fails when `EC_REQUIRE_FIXTURES` is set.

use std::path::{Path, PathBuf};

use ec_h264::{Decoder, NalOutcome};

use super::common::find_stream;

const STREAMS: usize = 10;
const TRUNCATIONS: usize = 8;
const CORRUPTIONS: usize = 30;
const SEED: u32 = 0x9E37_79B9;

fn vectors_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vectors/h264-jvt")
}

/// Every vector directory, in name order (the order `conformance.rs` walks).
fn vector_names(base: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Decode a whole byte stream, ignoring every error. Only a panic matters here.
fn decode_everything(bytes: &[u8]) {
    let mut dec = Decoder::new();
    for nal in ec_h264_syntax::AnnexBIter::new(bytes) {
        if let Ok(NalOutcome::PictureBoundary) = dec.push_nal(nal) {
            let _ = dec.end_picture();
            let _ = dec.push_nal(nal);
        }
        while dec.next_frame().is_some() {}
    }
    let _ = dec.flush();
    while dec.next_frame().is_some() {}
}

#[test]
fn truncated_and_corrupted_conformance_streams_never_panic() {
    let base = vectors_dir();
    if !base.exists() {
        assert!(
            std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
            "JVT vectors missing at {} — run scripts/link-fixtures.sh or scripts/fetch-vectors.sh",
            base.display()
        );
        eprintln!("SKIP JVT vectors missing at {}", base.display());
        return;
    }
    let mut streams = 0usize;
    for name in vector_names(&base).into_iter().take(STREAMS) {
        let Some(stream) = find_stream(&base.join(&name)) else {
            continue;
        };
        let bytes = std::fs::read(&stream).expect("read conformance stream");
        if bytes.is_empty() {
            continue;
        }
        streams += 1;

        for k in 1..=TRUNCATIONS {
            decode_everything(&bytes[..bytes.len() * k / (TRUNCATIONS + 1)]);
        }

        let mut seed = SEED;
        for _ in 0..CORRUPTIONS {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let pos = seed as usize % bytes.len();
            // Never xor by zero: that would leave the stream unchanged.
            let flip = ((seed >> 24) as u8) | 1;
            let mut m = bytes.clone();
            m[pos] ^= flip;
            decode_everything(&m);
        }
    }
    eprintln!(
        "no-panic sweep: {streams} streams, {TRUNCATIONS} truncations + {CORRUPTIONS} corruptions each"
    );
}
