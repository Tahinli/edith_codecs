//! No-panic gate for the decode path: malformed input must come back as `Err`,
//! never as a panic. Covers truncated and byte-corrupted copies of the committed
//! ADTS fixtures, plus random ADTS-looking frames (valid sync header, random payload).
//!
//! Every input is decoded inside `catch_unwind` so a panic reports which input
//! produced it instead of aborting the whole run.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use ec_aac::AacDecoder;

const ADTS_FIXTURES: [&str; 6] = [
    "aac-adts-mono-44100.aac",
    "aac-adts-mono-48000.aac",
    "aac-adts-stereo-44100.aac",
    "aac-adts-stereo-48000.aac",
    "aac-adts-5.1-44100.aac",
    "aac-adts-5.1-48000.aac",
];

/// Fixed seed so a failure reproduces exactly.
const SEED: u64 = 0x5EED_AAC0_0D15_EA5E;

/// Small xorshift64 generator; the crate has no RNG dev-dependency.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

fn audio_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio")
}

/// Decodes one packet with a fresh decoder. Returns false if it panicked.
fn decode_no_panic(label: &str, packet: &[u8]) -> bool {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let mut dec = AacDecoder::new();
        let _ = dec.decode(packet, None);
    }));
    if outcome.is_err() {
        eprintln!("PANIC: {label} ({} bytes)", packet.len());
        return false;
    }
    true
}

/// Byte offsets of ADTS frame starts, walked with each header's frame_length.
fn adts_frame_starts(data: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut at = 0usize;
    while at + 7 <= data.len() && data[at] == 0xFF && data[at + 1] & 0xF6 == 0xF0 {
        starts.push(at);
        let len = (((data[at + 3] & 0x03) as usize) << 11)
            | ((data[at + 4] as usize) << 3)
            | ((data[at + 5] as usize) >> 5);
        if len < 7 {
            break;
        }
        at += len;
    }
    starts
}

#[test]
fn fixtures_truncated_and_corrupted_never_panic() {
    let dir = audio_dir();
    let mut found = 0usize;
    let mut failures: Vec<String> = Vec::new();
    let mut rng = Rng(SEED);

    for name in ADTS_FIXTURES {
        let path = dir.join(name);
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(_) => {
                if std::env::var_os("EC_REQUIRE_FIXTURES").is_some() {
                    panic!(
                        "EC_REQUIRE_FIXTURES=1 but fixture {} is absent -- regenerate with: scripts/gen-fixtures.sh",
                        path.display()
                    );
                }
                eprintln!("SKIP: fixture {} absent", path.display());
                continue;
            }
        };
        found += 1;
        assert!(!adts_frame_starts(&data).is_empty(), "{name} is not an ADTS stream");

        // Ten truncation points spread across the file (never the full length).
        for i in 1..=10usize {
            let cut = data.len() * i / 11;
            let label = format!("{name} truncated at {cut}");
            if !decode_no_panic(&label, &data[..cut]) {
                failures.push(label);
            }
        }

        // Fifty single-byte corruptions at seeded positions with seeded values.
        for i in 0..50 {
            let pos = rng.below(data.len() as u64) as usize;
            let mut value = rng.below(256) as u8;
            if value == data[pos] {
                value = value.wrapping_add(1);
            }
            let mut copy = data.clone();
            copy[pos] = value;
            let label = format!("{name} corruption #{i} at byte {pos} -> {value:#04x}");
            if !decode_no_panic(&label, &copy) {
                failures.push(label);
            }
        }
    }

    if found == 0 {
        if std::env::var_os("EC_REQUIRE_FIXTURES").is_some() {
            panic!("no ADTS fixtures found under {}", dir.display());
        }
        eprintln!("SKIP: no ADTS fixtures under {}", dir.display());
    }
    assert!(failures.is_empty(), "{} inputs panicked: {:?}", failures.len(), failures);
}

#[test]
fn random_adts_frames_never_panic() {
    let mut rng = Rng(SEED ^ 0xAD75);
    let mut failures: Vec<String> = Vec::new();

    for i in 0..500usize {
        // Valid syncword, MPEG-4, layer 0, protection absent (7-byte header).
        let sf_index = rng.below(13) as u8; // 13..=15 are reserved; keep the rate table in range
        let channel_config = rng.below(8) as u8;
        let payload_len = rng.below(600) as usize;
        let frame_length = 7 + payload_len;
        let object_type_bits = rng.below(4) as u8; // profile field; decoder maps it to object type
        let mut frame = vec![0u8; frame_length];
        frame[0] = 0xFF;
        frame[1] = 0xF1;
        frame[2] = (object_type_bits << 6) | (sf_index << 2) | ((channel_config >> 2) & 0x01);
        frame[3] = ((channel_config & 0x03) << 6) | ((frame_length >> 11) as u8 & 0x03);
        frame[4] = (frame_length >> 3) as u8;
        frame[5] = (((frame_length & 0x07) as u8) << 5) | 0x1F;
        frame[6] = 0xFC;
        for b in frame.iter_mut().skip(7) {
            *b = rng.below(256) as u8;
        }
        let label = format!("random ADTS frame #{i} (sf {sf_index}, ch {channel_config}, {frame_length} bytes)");
        if !decode_no_panic(&label, &frame) {
            failures.push(label);
        }
    }

    assert!(failures.is_empty(), "{} random frames panicked: {:?}", failures.len(), failures);
}
