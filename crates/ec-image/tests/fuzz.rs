//! Malformed input never panics, never hangs and never allocates without
//! bound.
//!
//! Ten thousand mutations per format, from a fixed seed so a failure is
//! reproducible: bit flips, truncations, byte injections and pure noise behind
//! each format's signature. Every one of them must come back as `Ok` or as an
//! `Err` — a decoder reached by a file picker is reached by every broken file
//! on the disk.

use std::path::{Path, PathBuf};

const ROUNDS: usize = 10_000;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/stills")
}

/// xorshift64*, so every run mutates the same way.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }
}

/// One mutated copy of `seed`: a few bit flips, a truncation, or an injection.
///
/// `head` bounds where the flips land. Aiming them at the first few dozen
/// bytes is what actually reaches the geometry and colour-type paths: spread
/// over a whole file they nearly all land in the compressed data and stop at
/// the first integrity check.
fn mutate(rng: &mut Rng, seed: &[u8], head: Option<usize>) -> Vec<u8> {
    let mut data = seed.to_vec();
    let region = head.unwrap_or(data.len()).min(data.len());
    match rng.below(4) {
        0 => {
            for _ in 0..1 + rng.below(8) {
                let at = rng.below(region);
                data[at] ^= 1 << rng.below(8);
            }
        }
        1 => data.truncate(rng.below(data.len())),
        2 => {
            for _ in 0..1 + rng.below(16) {
                let at = rng.below(region);
                data[at] = (rng.next() & 0xff) as u8;
            }
        }
        _ => {
            // Keep the signature, replace the body with noise: this is the
            // shape that exercises header fields hardest.
            let keep = 12.min(data.len());
            data.truncate(keep);
            for _ in 0..rng.below(400) {
                data.push((rng.next() & 0xff) as u8);
            }
        }
    }
    data
}

/// Recompute every chunk CRC of a mutated PNG.
///
/// Without this the checksum catches every mutation at the door and the
/// decoder proper is never fuzzed at all — the sweep would prove only that
/// CRC-32 works.
fn repair_png_crcs(data: &mut [u8]) {
    let mut at = 8usize;
    while at + 12 <= data.len() {
        let len = u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as usize;
        if len > data.len() || at + 12 + len > data.len() {
            return;
        }
        let crc = crc32(&data[at + 4..at + 8 + len]);
        data[at + 8 + len..at + 12 + len].copy_from_slice(&crc.to_be_bytes());
        at += 12 + len;
    }
}

fn sweep(name: &str, seed_file: &str) {
    let Ok(seed) = std::fs::read(fixtures().join(seed_file)) else {
        eprintln!("skipped: fixtures/stills not generated; run scripts/gen-still-fixtures.sh");
        return;
    };
    let mut rng = Rng(0x2545_f491_4f6c_dd1d ^ name.len() as u64);
    let mut decoded = 0usize;
    for round in 0..ROUNDS {
        // Half the rounds aim at the header, half at the whole file.
        let head = if round % 2 == 0 { Some(48) } else { None };
        let mut data = mutate(&mut rng, &seed, head);
        if seed_file.ends_with(".png") {
            repair_png_crcs(&mut data);
        }
        // The call is the assertion: a panic here fails the test with the
        // round number, and the round number plus the fixed seed reproduces it.
        if let Ok(image) = ec_image::decode(&data) {
            decoded += 1;
            let pixels = image.to_rgba8();
            assert_eq!(
                pixels.len(),
                (image.width as usize) * (image.height as usize) * 4,
                "{name} round {round}: buffer does not match the dimensions"
            );
        }
        // Header-only parsing must survive the same input.
        let _ = ec_image::info(&data);
    }
    eprintln!("{name}: {ROUNDS} mutations, {decoded} still decoded");
}

#[test]
fn png_survives_mutation() {
    sweep("png", "tiny.png");
}

#[test]
fn jpeg_survives_mutation() {
    sweep("jpeg", "tiny.jpg");
}

#[test]
fn webp_lossy_survives_mutation() {
    sweep("webp-lossy", "tiny-lossy.webp");
}

#[test]
fn webp_lossless_survives_mutation() {
    sweep("webp-lossless", "tiny-lossless.webp");
}

#[test]
fn arbitrary_bytes_behind_a_signature_are_refused_not_believed() {
    let signatures: [&[u8]; 3] = [
        &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
        &[0xff, 0xd8, 0xff, 0xe0],
        b"RIFF\x40\x00\x00\x00WEBPVP8 ",
    ];
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for signature in signatures {
        for _ in 0..ROUNDS / 4 {
            let mut data = signature.to_vec();
            for _ in 0..rng.below(200) {
                data.push((rng.next() & 0xff) as u8);
            }
            let _ = ec_image::decode(&data);
            let _ = ec_image::info(&data);
        }
    }
}

#[test]
fn a_header_asking_for_more_pixels_than_the_limit_is_refused() {
    // A PNG IHDR claiming 65535x65535 is 4 gigapixels: the limit must refuse
    // it before any buffer is sized from those numbers.
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&65535u32.to_be_bytes());
    ihdr.extend_from_slice(&65535u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    png.extend_from_slice(&(ihdr.len() as u32).to_be_bytes());
    let start = png.len();
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&ihdr);
    let crc = crc32(&png[start..]);
    png.extend_from_slice(&crc.to_be_bytes());
    let err = ec_image::decode(&png).expect_err("4 gigapixels is past the limit");
    assert!(format!("{err}").contains("limit"), "{err}");
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    c ^ 0xffff_ffff
}
