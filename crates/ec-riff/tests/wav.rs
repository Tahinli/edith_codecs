//! Round-trip, header and tolerance checks, plus the fixtures on disk.
//!
//! The oracle artifacts this writes into `CARGO_TARGET_TMPDIR/oracle` are the
//! ffmpeg half of the story: after `cargo test -p ec-riff`, each `<name>.wav`
//! is compared against its `<name>.raw` (the samples that went in, as f32le)
//! with `oracle audio-compare <name>.raw <name>.wav`.

use std::io::Cursor;

use ec_riff::{AviReader, Error, SampleType, WavReader, WavSpec, WavWriter};

/// Deterministic samples in `bits`-bit range: a walking pattern that exercises
/// both signs and the extremes of the depth.
fn int_samples(n: usize, bits: u16) -> Vec<i32> {
    let limit = 1i64 << (bits - 1);
    (0..n)
        .map(|i| match i % 5 {
            0 => (limit - 1) as i32,
            1 => (-limit) as i32,
            2 => 0,
            3 => ((i as i64 * 2_654_435_761) % limit) as i32,
            _ => -(((i as i64 * 40_503) % limit) as i32),
        })
        .collect()
}

fn float_samples(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| match i % 4 {
            0 => 1.0,
            1 => -1.0,
            2 => 0.0,
            _ => (i as f32 * 0.017).sin(),
        })
        .collect()
}

fn spec(channels: u16, rate: u32, bits: u16, sample_format: SampleType) -> WavSpec {
    WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: bits,
        sample_format,
    }
}

#[test]
fn int_round_trip_is_bit_exact_every_depth_layout_and_rate() {
    for bits in [8u16, 16, 24, 32] {
        for channels in [1u16, 2, 6, 8] {
            for rate in [8000u32, 44100, 48000, 192_000] {
                let s = spec(channels, rate, bits, SampleType::Int);
                let samples = int_samples(usize::from(channels) * 97, bits);
                let mut w = WavWriter::new(Cursor::new(Vec::new()), s).unwrap();
                w.write_samples(&samples).unwrap();
                let bytes = w.finalize().unwrap().into_inner();
                let mut r = WavReader::new(Cursor::new(&bytes[..])).unwrap();
                assert_eq!(r.spec(), s, "spec round-trip {bits}b {channels}ch {rate}Hz");
                assert_eq!(
                    r.duration(),
                    Some(97),
                    "frame count {bits}b {channels}ch {rate}Hz"
                );
                assert_eq!(
                    r.read_all_i32().unwrap(),
                    samples,
                    "samples {bits}b {channels}ch {rate}Hz"
                );
                assert_eq!(bytes.len() % 2, 0, "RIFF stays word-aligned");
            }
        }
    }
}

#[test]
fn float_round_trip_is_bit_exact() {
    for channels in [1u16, 2, 6, 8] {
        let s = spec(channels, 48000, 32, SampleType::Float);
        let samples = float_samples(usize::from(channels) * 64);
        let mut w = WavWriter::new(Cursor::new(Vec::new()), s).unwrap();
        w.write_samples(&samples).unwrap();
        let bytes = w.finalize().unwrap().into_inner();
        let mut r = WavReader::new(Cursor::new(&bytes[..])).unwrap();
        assert_eq!(r.spec(), s);
        assert_eq!(r.read_all_f32().unwrap(), samples, "{channels}ch float");
    }
}

#[test]
fn header_says_extensible_exactly_when_it_must() {
    // <=2 channels and <=16 bits: plain WAVE_FORMAT_PCM, 16-byte fmt.
    for (channels, bits, extensible) in [
        (1u16, 8u16, false),
        (2, 16, false),
        (1, 24, true),
        (2, 32, true),
        (6, 16, true),
        (8, 16, true),
    ] {
        let s = spec(channels, 48000, bits, SampleType::Int);
        let mut w = WavWriter::new(Cursor::new(Vec::new()), s).unwrap();
        w.write_samples(&vec![0i32; usize::from(channels)]).unwrap();
        let bytes = w.finalize().unwrap().into_inner();

        let fmt_size = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
        let tag = u16::from_le_bytes(bytes[20..22].try_into().unwrap());
        let block_align = u16::from_le_bytes(bytes[32..34].try_into().unwrap());
        let byte_rate = u32::from_le_bytes(bytes[28..32].try_into().unwrap());
        assert_eq!(
            fmt_size,
            if extensible { 40 } else { 16 },
            "{channels}ch {bits}b"
        );
        assert_eq!(
            tag,
            if extensible { 0xFFFE } else { 1 },
            "{channels}ch {bits}b"
        );
        assert_eq!(u32::from(block_align), (bits as u32 / 8) * channels as u32);
        assert_eq!(byte_rate, 48000 * u32::from(block_align));
        if extensible {
            let mask = u32::from_le_bytes(bytes[40..44].try_into().unwrap());
            let expect = match channels {
                1 => 0x4,   // FC
                2 => 0x3,   // FL FR
                6 => 0x3F,  // FL FR FC LFE BL BR
                8 => 0x63F, // + SL SR
                _ => unreachable!(),
            };
            assert_eq!(mask, expect, "channel mask {channels}ch");
            // Sub-format GUID starts with the real tag, then the WAVE tail.
            assert_eq!(&bytes[44..46], &1u16.to_le_bytes());
            assert_eq!(&bytes[54..60], &[0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71]);
        }
        // Every file the writer produces reads back as itself.
        assert_eq!(WavReader::new(Cursor::new(&bytes[..])).unwrap().spec(), s);
    }
}

#[test]
fn reader_skips_junk_odd_and_list_chunks() {
    // A hand-built file: LIST/INFO before fmt, an odd-sized chunk with its pad
    // byte between fmt and data, and a trailing chunk after data.
    let mut f: Vec<u8> = Vec::new();
    f.extend_from_slice(b"RIFF\0\0\0\0WAVE");
    f.extend_from_slice(b"LIST");
    f.extend_from_slice(&12u32.to_le_bytes());
    f.extend_from_slice(b"INFOISFT\x04\0\0\0");
    f.extend_from_slice(b"fmt ");
    f.extend_from_slice(&16u32.to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes()); // PCM
    f.extend_from_slice(&2u16.to_le_bytes()); // stereo
    f.extend_from_slice(&44100u32.to_le_bytes());
    f.extend_from_slice(&(44100u32 * 4).to_le_bytes());
    f.extend_from_slice(&4u16.to_le_bytes());
    f.extend_from_slice(&16u16.to_le_bytes());
    f.extend_from_slice(b"junk");
    f.extend_from_slice(&3u32.to_le_bytes());
    f.extend_from_slice(&[1, 2, 3, 0]); // 3 bytes + RIFF pad
    f.extend_from_slice(b"data");
    f.extend_from_slice(&8u32.to_le_bytes());
    for v in [1i16, -1, 32767, -32768] {
        f.extend_from_slice(&v.to_le_bytes());
    }
    f.extend_from_slice(b"id3 ");
    f.extend_from_slice(&2u32.to_le_bytes());
    f.extend_from_slice(&[0, 0]);

    let mut r = WavReader::new(Cursor::new(&f[..])).unwrap();
    assert_eq!(r.spec().channels, 2);
    assert_eq!(r.spec().sample_rate, 44100);
    assert_eq!(r.duration(), Some(2));
    // The trailing chunk is not audio: exactly the declared 4 samples come out.
    assert_eq!(r.read_all_i32().unwrap(), vec![1, -1, 32767, -32768]);
}

#[test]
fn truncated_and_placeholder_sizes_read_what_is_there() {
    let s = spec(2, 48000, 16, SampleType::Int);
    let samples = int_samples(20, 16);
    let mut w = WavWriter::new(Cursor::new(Vec::new()), s).unwrap();
    w.write_samples(&samples).unwrap();
    let good = w.finalize().unwrap().into_inner();

    // Header claims the full length, file is cut in half.
    let cut = good[..good.len() - 20].to_vec();
    let mut r = WavReader::new(Cursor::new(&cut[..])).unwrap();
    assert_eq!(r.read_all_i32().unwrap(), samples[..10].to_vec());

    // A streaming writer that never patched: 0xFFFFFFFF means "to the end".
    let mut stream = good.clone();
    let n = stream.len();
    stream[n - 40 - 4..n - 40].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut r = WavReader::new(Cursor::new(&stream[..])).unwrap();
    assert_eq!(r.duration(), None);
    assert_eq!(r.read_all_i32().unwrap(), samples);
}

#[test]
fn avi_truncated_tail_is_end_of_stream() {
    fn chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(id);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        if body.len() % 2 == 1 {
            out.push(0);
        }
    }

    fn list(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(b"LIST");
        out.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        if body.len() % 2 == 1 {
            out.push(0);
        }
    }

    let mut strh = Vec::new();
    strh.extend_from_slice(b"auds");
    strh.extend_from_slice(&[0; 52]);
    let mut strf = Vec::new();
    strf.extend_from_slice(&0x2000u16.to_le_bytes());
    strf.extend_from_slice(&2u16.to_le_bytes());
    strf.extend_from_slice(&48_000u32.to_le_bytes());
    strf.extend_from_slice(&0u32.to_le_bytes());
    strf.extend_from_slice(&0u16.to_le_bytes());
    strf.extend_from_slice(&0u16.to_le_bytes());
    let mut strl = Vec::new();
    chunk(&mut strl, b"strh", &strh);
    chunk(&mut strl, b"strf", &strf);
    let mut hdrl = Vec::new();
    list(&mut hdrl, b"strl", &strl);
    chunk(&mut hdrl, b"dmlh", &[0, 0, 0, 0]);

    let mut rec = Vec::new();
    chunk(&mut rec, b"00wb", &[1, 2, 3]);
    let mut movi = Vec::new();
    chunk(&mut movi, b"00wb", &[0x0b, 0x77, 0, 0]);
    chunk(&mut movi, b"indx", &[0; 16]);
    list(&mut movi, b"rec ", &rec);
    movi.extend_from_slice(b"00");

    let mut body = Vec::new();
    list(&mut body, b"hdrl", &hdrl);
    list(&mut body, b"movi", &movi);
    let mut avi = Vec::new();
    avi.extend_from_slice(b"RIFF");
    avi.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
    avi.extend_from_slice(b"AVI ");
    avi.extend_from_slice(&body);

    let mut reader = AviReader::new(Cursor::new(&avi)).expect("AVI opens");
    assert_eq!(reader.audio_streams()[0].format_tag, 0x2000);
    assert_eq!(
        reader.next_packet().expect("first packet").data,
        [0x0b, 0x77, 0, 0]
    );
    assert_eq!(reader.next_packet().expect("nested packet").data, [1, 2, 3]);
    let err = reader
        .next_packet()
        .expect_err("truncated tail ends the stream");
    assert!(matches!(err, Error::Eof));
}

#[test]
fn refusals_name_a_real_absence() {
    // Not RIFF at all.
    assert!(WavReader::new(Cursor::new(&b"OggS\0\0\0\0\0\0\0\0"[..])).is_err());
    // ADPCM: a tag we genuinely do not decode.
    let mut f: Vec<u8> = Vec::new();
    f.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt ");
    f.extend_from_slice(&16u32.to_le_bytes());
    f.extend_from_slice(&2u16.to_le_bytes()); // WAVE_FORMAT_ADPCM
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&8000u32.to_le_bytes());
    f.extend_from_slice(&8000u32.to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&4u16.to_le_bytes());
    let e = WavReader::new(Cursor::new(&f[..]))
        .err()
        .expect("ADPCM was accepted")
        .to_string();
    assert!(e.contains("0x0002"), "{e}");

    // Writing a sample too wide for the declared depth is caught, not clipped.
    let mut w =
        WavWriter::new(Cursor::new(Vec::new()), spec(1, 48000, 16, SampleType::Int)).unwrap();
    assert!(w.write_sample(32768i32).is_err());
    assert!(w.write_sample(1.0f32).is_err(), "float into an int file");
    // A half-written frame is a truncated file, and finalize says so.
    let mut w =
        WavWriter::new(Cursor::new(Vec::new()), spec(2, 48000, 16, SampleType::Int)).unwrap();
    w.write_sample(1i16).unwrap();
    assert!(w.finalize().is_err());
}

#[test]
fn reads_every_wav_fixture() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/audio");
    let Ok(entries) = std::fs::read_dir(dir) else {
        eprintln!("fixtures/audio absent (gitignored) — skipping");
        return;
    };
    let mut seen = 0;
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some("wav") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let mut r = WavReader::open(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        let spec = r.spec();
        // The generator names layout and rate in the file name; the header must agree.
        let channels = if name.contains("mono") {
            1
        } else if name.contains("stereo") {
            2
        } else {
            6
        };
        assert_eq!(spec.channels, channels, "{name}");
        assert!(name.contains(&spec.sample_rate.to_string()), "{name}");
        assert_eq!(spec.sample_format, SampleType::Int, "{name}");
        let samples = r.read_all_i32().unwrap();
        assert!(!samples.is_empty(), "{name} decoded nothing");
        assert_eq!(samples.len() % usize::from(channels), 0, "{name}");
        assert!(
            samples.iter().any(|&v| v != 0),
            "{name} is silence — the fixture or the reader is wrong"
        );
        // What we read, as f32le, for `oracle audio-compare <name>.raw <fixture>`:
        // ffmpeg decodes the same file and the two must agree sample for sample.
        let mut r = WavReader::open(&path).unwrap();
        let raw: Vec<u8> = r
            .read_all_f32()
            .unwrap()
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("oracle");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.raw")), raw).unwrap();
        seen += 1;
    }
    assert!(seen >= 6, "expected the six wav16 fixtures, saw {seen}");
}

/// Write the pairs the `oracle` binary compares: our WAV, and the samples that
/// went into it as f32le. Not an assertion — the assertion is the oracle run.
#[test]
fn emit_oracle_artifacts() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("oracle");
    std::fs::create_dir_all(&dir).unwrap();
    for (name, channels, rate, bits, format) in [
        ("i16-stereo-48000", 2u16, 48000u32, 16u16, SampleType::Int),
        ("i24-5_1-44100", 6, 44100, 24, SampleType::Int),
        ("i32-mono-48000", 1, 48000, 32, SampleType::Int),
        ("i8-stereo-44100", 2, 44100, 8, SampleType::Int),
        ("f32-7_1-48000", 8, 48000, 32, SampleType::Float),
    ] {
        let s = spec(channels, rate, bits, format);
        let frames = 4096;
        let n = frames * usize::from(channels);
        let mut w = WavWriter::create(dir.join(format!("{name}.wav")), s).unwrap();
        let raw: Vec<f32> = if format == SampleType::Float {
            let samples = float_samples(n);
            w.write_samples(&samples).unwrap();
            samples
        } else {
            let samples = int_samples(n, bits);
            w.write_samples(&samples).unwrap();
            let scale = 1.0f32 / (1i64 << (bits - 1)) as f32;
            samples.iter().map(|&v| v as f32 * scale).collect()
        };
        w.finalize().unwrap();
        let bytes: Vec<u8> = raw.iter().flat_map(|v| v.to_le_bytes()).collect();
        std::fs::write(dir.join(format!("{name}.raw")), bytes).unwrap();
    }
    eprintln!("oracle artifacts in {}", dir.display());
}
