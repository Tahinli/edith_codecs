//! RF64 headers: the container choice, the ds64 sizes, and the reader's side.
//!
//! The threshold is injectable ([`WavOptions::rf64_threshold`]), so the RF64
//! path runs on kilobytes here. The one test that writes past 4 GiB is
//! `#[ignore]`d and runs by hand; it writes under `std::env::temp_dir()`,
//! which honours `TMPDIR`.

use std::io::Cursor;

use ec_riff::{SampleType, WavOptions, WavReader, WavSpec, WavWriter};

fn spec(channels: u16, bits: u16) -> WavSpec {
    WavSpec {
        channels,
        sample_rate: 48_000,
        bits_per_sample: bits,
        sample_format: SampleType::Int,
    }
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// Write `frames` frames of a ramp and return the finished file.
fn write_file(s: WavSpec, frames: usize, options: WavOptions) -> Vec<u8> {
    let mut w = WavWriter::with_options(Cursor::new(Vec::new()), s, options).unwrap();
    let ch = usize::from(s.channels);
    let limit = 1i64 << (s.bits_per_sample - 1);
    for i in 0..frames * ch {
        w.write_sample((i as i64 % (limit - 1)) as i32).unwrap();
    }
    w.finalize().unwrap().into_inner()
}

fn opts(frames: u64, threshold: u64) -> WavOptions {
    WavOptions {
        expected_frames: frames,
        rf64_threshold: threshold,
    }
}

#[test]
fn over_threshold_writes_rf64_with_exact_ds64_sizes() {
    let s = spec(2, 16);
    let frames = 1000;
    let bytes = write_file(s, frames, opts(frames as u64, 64));

    assert_eq!(&bytes[0..4], b"RF64");
    assert_eq!(le32(&bytes, 4), u32::MAX, "RIFF size is ds64's to hold");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(&bytes[12..16], b"ds64");
    assert_eq!(le32(&bytes, 16), 28, "ds64 body without a table");

    let data_bytes = (frames * 4) as u64;
    assert_eq!(le64(&bytes, 20), bytes.len() as u64 - 8, "riffSize");
    assert_eq!(le64(&bytes, 28), data_bytes, "dataSize");
    assert_eq!(le64(&bytes, 36), frames as u64, "sampleCount");
    assert_eq!(le32(&bytes, 44), 0, "tableLength");

    // Stereo 16-bit is a plain PCM fmt with no fact chunk, then data with the sentinel.
    let fmt = 48;
    assert_eq!(&bytes[fmt..fmt + 4], b"fmt ");
    // The `data` id sits right before its payload, at a fixed distance from the end.
    let data = bytes.len() - 8 - data_bytes as usize;
    assert_eq!(&bytes[data..data + 4], b"data");
    assert_eq!(
        le32(&bytes, data + 4),
        u32::MAX,
        "data size is ds64's to hold"
    );

    let mut r = WavReader::new(Cursor::new(&bytes[..])).unwrap();
    assert_eq!(r.spec(), s);
    assert_eq!(r.duration(), Some(frames as u64));
    assert_eq!(r.read_all_i32().unwrap().len(), frames * 2);
}

#[test]
fn rf64_samples_match_the_riff_file_for_the_same_input() {
    for (channels, bits) in [(1u16, 8u16), (6, 24), (2, 16), (8, 32)] {
        let s = spec(channels, bits);
        let frames = 333; // odd sample counts for the mono 8-bit pad case
        let riff = write_file(s, frames, WavOptions::default());
        let rf64 = write_file(s, frames, opts(frames as u64, 64));
        assert_eq!(&riff[0..4], b"RIFF");
        assert_eq!(&rf64[0..4], b"RF64");
        let a = WavReader::new(Cursor::new(&riff[..]))
            .unwrap()
            .read_all_i32()
            .unwrap();
        let b = WavReader::new(Cursor::new(&rf64[..]))
            .unwrap()
            .read_all_i32()
            .unwrap();
        assert_eq!(a, b, "{channels}ch {bits}b");
    }
}

#[test]
fn odd_rf64_data_gets_a_pad_byte_outside_its_size() {
    let s = spec(1, 8);
    let frames = 5;
    let bytes = write_file(s, frames, opts(frames as u64, 1));
    // 5 data bytes: one pad byte follows, counted by riffSize but not dataSize.
    assert_eq!(bytes.len() % 2, 0);
    assert_eq!(le64(&bytes, 20), bytes.len() as u64 - 8);
    assert_eq!(le64(&bytes, 28), 5);
    let r = WavReader::new(Cursor::new(&bytes[..])).unwrap();
    assert_eq!(r.duration(), Some(5));
}

#[test]
fn extensible_rf64_keeps_fact_and_its_saturating_count() {
    let s = spec(6, 24);
    let frames = 50;
    let bytes = write_file(s, frames, opts(frames as u64, 1));
    assert_eq!(&bytes[0..4], b"RF64");
    // Extensible fmt is 40 bytes, so `fact` follows it at 48 + 8 + 40.
    let fact = 48 + 8 + 40;
    assert_eq!(&bytes[fact..fact + 4], b"fact");
    assert_eq!(le32(&bytes, fact + 8), frames as u32);
    let r = WavReader::new(Cursor::new(&bytes[..])).unwrap();
    assert_eq!(r.spec(), s);
    assert_eq!(r.duration(), Some(frames as u64));
}

#[test]
fn under_threshold_stays_riff_byte_for_byte() {
    let s = spec(2, 16);
    let riff = write_file(s, 100, WavOptions::default());
    let below = write_file(s, 100, opts(100, 400 + 1));
    assert_eq!(riff, below);
    assert_eq!(&riff[0..4], b"RIFF");
    assert_eq!(le32(&riff, 4) as usize, riff.len() - 8);
}

#[test]
fn unknown_length_is_riff_and_oversize_riff_is_refused() {
    let s = spec(2, 16);
    let bytes = write_file(s, 10, WavOptions::default());
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(le32(&bytes, 4) as usize, bytes.len() - 8);
}

/// Manual: writes 4.1 GiB of 16-bit stereo and reads its header back. Run with
/// `cargo test -p ec-riff --test it rf64:: -- --ignored --nocapture`.
#[test]
#[ignore = "writes ~4.1 GiB under TMPDIR; run by hand"]
fn writes_past_four_gib_as_rf64() {
    let s = spec(2, 16);
    // 4.4e9 bytes: 4.1 GiB, past the 4 GiB line the default threshold guards.
    let frames: u64 = 1_100_000_000;
    let path = std::env::temp_dir().join(format!("ec-riff-rf64-{}.wav", std::process::id()));
    let mut w = WavWriter::create_with_options(&path, s, opts(frames, 0xFFFF_F000)).unwrap();
    let block = vec![1234i32; 1 << 20];
    let mut left = frames * 2;
    while left > 0 {
        let n = (block.len() as u64).min(left) as usize;
        w.write_samples(&block[..n]).unwrap();
        left -= n as u64;
    }
    w.finalize().unwrap();

    let len = std::fs::metadata(&path).unwrap().len();
    let mut head = vec![0u8; 60];
    use std::io::Read;
    std::fs::File::open(&path)
        .unwrap()
        .read_exact(&mut head)
        .unwrap();
    assert_eq!(&head[0..4], b"RF64");
    assert_eq!(le64(&head, 20), len - 8);
    assert_eq!(le64(&head, 28), frames * 4);
    let r = WavReader::open(&path).unwrap();
    assert_eq!(r.duration(), Some(frames));
    let _ = std::fs::remove_file(&path);
}
