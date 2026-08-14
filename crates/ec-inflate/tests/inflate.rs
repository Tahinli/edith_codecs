//! Round-trip, ceiling and malformed-input tests.
//!
//! flate2's default backend *is* miniz_oxide, so compressing here at every
//! level exercises exactly the encoder whose output edith has to inflate; the
//! comparison is against the original bytes, which is stricter than agreeing
//! with another decoder.

use std::io::Write;

use ec_inflate::{Error, Format, inflate, inflate_into, inflate_zlib};
use flate2::Compression;
use flate2::write::{DeflateEncoder, ZlibEncoder};

const NO_LIMIT: usize = usize::MAX;

/// xorshift64*, so the "random" inputs are the same on every run.
fn pseudo_random(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 33) as u8
        })
        .collect()
}

/// The shapes that pull DEFLATE down different paths: stored blocks, long
/// matches, far distances, literal-only blocks.
fn corpus() -> Vec<(&'static str, Vec<u8>)> {
    let mut mixed = pseudo_random(7, 40_000);
    mixed.extend_from_slice(&mixed.clone()[..20_000]);
    mixed.extend(std::iter::repeat_n(b'x', 5_000));
    vec![
        ("empty", Vec::new()),
        ("one byte", vec![0x42]),
        (
            "text",
            "the quick brown fox jumps over the lazy dog. "
                .repeat(2_000)
                .into_bytes(),
        ),
        ("zeros", vec![0u8; 200_000]),
        ("random", pseudo_random(1, 100_000)),
        ("run of one", vec![0xab; 3]),
        ("mixed", mixed),
    ]
}

fn zlib(data: &[u8], level: u32) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(level));
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn raw(data: &[u8], level: u32) -> Vec<u8> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(level));
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn round_trips_every_level_bit_exactly() {
    for (name, data) in corpus() {
        for level in 0..=9 {
            let got = inflate_zlib(&zlib(&data, level), NO_LIMIT)
                .unwrap_or_else(|e| panic!("zlib {name} level {level}: {e}"));
            assert_eq!(got, data, "zlib {name} level {level}");

            let got = inflate(&raw(&data, level), NO_LIMIT)
                .unwrap_or_else(|e| panic!("raw {name} level {level}: {e}"));
            assert_eq!(got, data, "raw {name} level {level}");
        }
    }
}

#[test]
fn limit_is_a_boundary_not_a_panic() {
    let data = "a limit is checked before every write. "
        .repeat(500)
        .into_bytes();
    let stream = zlib(&data, 6);

    assert_eq!(inflate_zlib(&stream, data.len()).unwrap(), data);
    assert_eq!(
        inflate_zlib(&stream, data.len() - 1),
        Err(Error::LimitExceeded {
            limit: data.len() - 1
        })
    );
    assert_eq!(
        inflate_zlib(&stream, 0),
        Err(Error::LimitExceeded { limit: 0 })
    );

    // Stored blocks take a different write path, so they get their own check.
    let stored = zlib(&data, 0);
    assert_eq!(inflate_zlib(&stored, data.len()).unwrap(), data);
    assert!(inflate_zlib(&stored, data.len() - 1).is_err());

    // What was decompressed before the ceiling survives, for callers reporting
    // partial progress.
    let mut partial = Vec::new();
    let error = inflate_into(&stream, &mut partial, 4_096, Format::Zlib).unwrap_err();
    assert_eq!(error, Error::LimitExceeded { limit: 4_096 });
    assert!(!partial.is_empty() && partial.len() <= 4_096);
    assert_eq!(partial[..], data[..partial.len()]);
}

#[test]
fn adler32_mismatch_is_detected() {
    let data = b"the trailer has to be checked, or the ceiling is the only guard".to_vec();
    let mut stream = zlib(&data, 6);
    let last = stream.len() - 1;
    stream[last] ^= 0xff;
    assert!(matches!(
        inflate_zlib(&stream, NO_LIMIT),
        Err(Error::Adler32Mismatch { .. })
    ));

    // The same bytes as raw deflate have no trailer to disagree with.
    let body = &zlib(&data, 6)[2..];
    assert_eq!(inflate(body, NO_LIMIT).unwrap(), data);
}

#[test]
fn truncation_is_an_error() {
    let data = pseudo_random(3, 20_000);
    let stream = zlib(&data, 6);
    for cut in [0, 1, 2, 3, 10, 100, stream.len() - 5, stream.len() - 1] {
        assert!(
            inflate_zlib(&stream[..cut], NO_LIMIT).is_err(),
            "cut at {cut} should not decode"
        );
    }
}

#[test]
fn malformed_input_never_panics() {
    let data = "content encodings carry whatever the muxer felt like"
        .repeat(40)
        .into_bytes();
    let seeds: Vec<Vec<u8>> = vec![
        zlib(&data, 9),
        zlib(&data, 0),
        zlib(&data, 1),
        raw(&data, 6),
    ];
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    let mut decoded = 0usize;
    for i in 0..10_000 {
        let seed = &seeds[i % seeds.len()];
        let mut stream = seed.clone();
        // One to four bit-level mutations, plus the occasional truncation.
        for _ in 0..=(next() % 4) {
            let at = (next() as usize) % stream.len();
            stream[at] ^= 1 << (next() % 8);
        }
        if next() % 4 == 0 {
            stream.truncate((next() as usize) % seed.len());
        }
        let limit = if next() % 8 == 0 { 1_024 } else { 1 << 20 };
        if inflate_zlib(&stream, limit).is_ok() {
            decoded += 1;
        }
        // Raw entry too: no header to reject the garbage early.
        let _ = inflate(&stream, limit);
    }
    // Bit flips inside the compressed body land in a valid stream sometimes;
    // if *nothing* ever decoded the loop would be testing the header check only.
    assert!(decoded > 0, "no mutated stream ever decoded");

    for len in [0, 1, 2, 3, 8, 64, 1_000] {
        let garbage = pseudo_random(len as u64 + 11, len);
        let _ = inflate_zlib(&garbage, NO_LIMIT);
        let _ = inflate(&garbage, NO_LIMIT);
    }
}

#[test]
fn refuses_what_it_does_not_implement() {
    // FDICT set: header check bits still valid, preset dictionary unsupported.
    assert!(matches!(
        inflate_zlib(&[0x78, 0xbb, 0, 0, 0, 0, 0, 0], NO_LIMIT),
        Err(Error::Unsupported { .. })
    ));
    // Block type 3 is reserved.
    assert!(matches!(
        inflate(&[0b111, 0, 0, 0], NO_LIMIT),
        Err(Error::Corrupt { .. })
    ));
    // Stored block whose complement does not match.
    assert!(matches!(
        inflate(
            &[0x01, 0x05, 0x00, 0x05, 0x00, b'h', b'e', b'l', b'l', b'o'],
            NO_LIMIT
        ),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
fn stored_block_decodes_by_hand() {
    // Final stored block, len 5, ~len, "hello": the one case with no Huffman.
    let stream = [0x01, 0x05, 0x00, 0xfa, 0xff, b'h', b'e', b'l', b'l', b'o'];
    assert_eq!(inflate(&stream, NO_LIMIT).unwrap(), b"hello");
}
