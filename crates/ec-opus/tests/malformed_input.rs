//! Malformed input must surface as `Err`, never as a panic.
//!
//! The packets here are random byte strings and truncations of them, generated from
//! a fixed-seed LCG so a failure reproduces exactly. The TOC byte is random too, so the
//! SILK, CELT and hybrid paths, every bandwidth and every frame-count code are reached.

use std::panic::{catch_unwind, AssertUnwindSafe};

use ec_opus::Decoder;

/// Fixed-seed 64-bit LCG (Knuth's MMIX constants).
struct Lcg(u64);

impl Lcg {
    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    /// Uniform-ish value in `lo..=hi`.
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + (self.next_u32() as usize) % (hi - lo + 1)
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next_u32() as u8).collect()
    }
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decodes `packet` on a fresh decoder and reports whether it panicked.
fn decode_must_not_panic(sample_rate: u32, channels: usize, packet: &[u8]) {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let mut dec = Decoder::new(sample_rate, channels).expect("decoder construction");
        // 120 ms at 48 kHz is the largest possible output per packet.
        let mut out = vec![0f32; 5760 * channels];
        let _ = dec.decode_float(packet, &mut out);
    }));
    assert!(
        outcome.is_ok(),
        "decoder panicked on {sample_rate} Hz / {channels} ch packet: {}",
        hex(packet)
    );
}

#[test]
fn random_packets_never_panic() {
    let mut rng = Lcg(0x5eed_0bad_cafe_f00d);
    let rates = [48_000u32, 24_000, 16_000, 12_000, 8_000];
    for _ in 0..1000 {
        let len = rng.range(1, 1500);
        let packet = rng.bytes(len);
        let rate = rates[rng.range(0, rates.len() - 1)];
        let channels = rng.range(1, 2);
        decode_must_not_panic(rate, channels, &packet);
    }
}

#[test]
fn truncated_packets_never_panic() {
    let mut rng = Lcg(0x7247_4e43_4154_4544);
    for _ in 0..1000 {
        let len = rng.range(1, 1500);
        let packet = rng.bytes(len);
        // Every prefix length of a shorter random packet, so the cut lands on each
        // header and range-coded boundary the decoder reads.
        let cut = rng.range(0, len - 1);
        decode_must_not_panic(48_000, 2, &packet[..cut]);
        decode_must_not_panic(48_000, 1, &packet[..cut]);
    }
}

#[test]
fn empty_and_single_byte_packets_never_panic() {
    decode_must_not_panic(48_000, 2, &[]);
    for toc in 0..=255u8 {
        decode_must_not_panic(48_000, 1, &[toc]);
        decode_must_not_panic(48_000, 2, &[toc]);
    }
}
