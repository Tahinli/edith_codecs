#![no_main]

use ec_flac::decode::FlacReader;
use ec_flac::encode::{EncoderConfig, encode};
use libfuzzer_sys::fuzz_target;

// Header bytes pick the shape: channels, bit depth, sample rate, block size. The
// rest is i32 samples, each clamped into the bit depth's range. Lossless means the
// decode equals the input.
fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }
    let channels = usize::from(data[0] % 8) + 1;
    let bits = u32::from(data[1] % 29) + 4;
    let rate = u32::from(u16::from_le_bytes([data[2], data[3]])) % 655_350 + 1;
    let block_size = usize::from(u16::from_le_bytes([data[4], data[5]])) % 4096 + 16;
    let config = EncoderConfig {
        block_size,
        ..EncoderConfig::default()
    };

    let (low, high) = match bits {
        32 => (i32::MIN as i64, i32::MAX as i64),
        n => (-(1i64 << (n - 1)), (1i64 << (n - 1)) - 1),
    };
    let body = &data[6..];
    let frames = body.len() / 4 / channels;
    if frames == 0 {
        return;
    }
    let samples: Vec<i32> = body
        .chunks_exact(4)
        .take(frames * channels)
        .map(|b| i64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]])).clamp(low, high) as i32)
        .collect();

    let Ok(stream) = encode(&config, &samples, channels, bits, rate) else {
        return;
    };
    let mut reader = FlacReader::new(&stream).expect("encoder output opens");
    let decoded = reader.decode_all().expect("encoder output decodes");
    assert_eq!(decoded.bits_per_sample, bits);
    assert_eq!(decoded.sample_rate, rate);
    assert_eq!(decoded.channels.len(), channels);
    let mut got = Vec::with_capacity(samples.len());
    for i in 0..frames {
        for plane in &decoded.channels {
            got.push(plane[i]);
        }
    }
    assert!(got == samples, "FLAC roundtrip mismatch ({channels}ch {bits}bit {rate}Hz)");
});
