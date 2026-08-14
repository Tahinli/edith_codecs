//! The call edith makes, verbatim in shape (engine/src/export.rs `write_wav`):
//! `WavWriter::create(path, WavSpec { .. sample_format: Int })`, a run of
//! `write_sample(x as i16)`, then `finalize()` — with the `?` conversion into
//! edith's `Box<dyn Error + Send + Sync>` exercised too.

type EdithError = Box<dyn std::error::Error + Send + Sync>;

fn write_wav(
    out: &std::path::Path,
    samples: &[i32],
    channels: u16,
    rate: u32,
) -> Result<(), EdithError> {
    let mut writer = hound::WavWriter::create(
        out,
        hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )?;
    for &sample in samples {
        writer.write_sample(sample as i16)?;
    }
    writer.finalize()?;
    Ok(())
}

#[test]
fn edith_write_wav_round_trips() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(dir).unwrap();
    for (channels, rate) in [(1u16, 44100u32), (2, 48000), (6, 48000)] {
        let path = dir.join(format!("edith-{channels}ch-{rate}.wav"));
        let samples: Vec<i32> = (0..usize::from(channels) * 512)
            .map(|i| ((i as i32 * 733) % 65536) - 32768)
            .collect();
        write_wav(&path, &samples, channels, rate).unwrap();

        let mut r = ec_riff::WavReader::open(&path).unwrap();
        let spec = r.spec();
        assert_eq!(spec.channels, channels);
        assert_eq!(spec.sample_rate, rate);
        assert_eq!(spec.bits_per_sample, 16);
        assert_eq!(spec.sample_format, ec_riff::SampleType::Int);
        assert_eq!(r.read_all_i32().unwrap(), samples);
    }
}

/// The read half edith's tests make: `WavReader::open`, `spec()`, and
/// `samples::<i16>()` collected — plus the two refusals hound answers with
/// when the requested type cannot carry the file.
#[test]
fn edith_read_wav_round_trips() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("read-back.wav");
    let samples: Vec<i32> = (0..2048).map(|i| ((i * 733) % 65536) - 32768).collect();
    write_wav(&path, &samples, 2, 48000).unwrap();

    let mut r = hound::WavReader::open(&path).unwrap();
    let spec = r.spec();
    assert_eq!(spec.channels, 2);
    assert_eq!(spec.sample_rate, 48000);
    assert_eq!(spec.bits_per_sample, 16);
    assert_eq!(spec.sample_format, hound::SampleFormat::Int);
    let read: Vec<i32> = r
        .samples::<i16>()
        .map(|s| i32::from(s.expect("a sample")))
        .collect();
    assert_eq!(read, samples);

    // An integer file read as floats, and a 16-bit file read as 8: the two
    // refusals, not silent conversions.
    let mut r = hound::WavReader::open(&path).unwrap();
    assert!(matches!(
        r.samples::<f32>().next(),
        Some(Err(hound::Error::InvalidSampleFormat))
    ));
    let mut r = hound::WavReader::open(&path).unwrap();
    assert!(matches!(
        r.samples::<i8>().next(),
        Some(Err(hound::Error::TooWide))
    ));
}

/// A `data` chunk that ends before its header said is hound's `IoError`
/// (`UnexpectedEof`), never a short read the caller mistakes for the whole file.
#[test]
fn truncated_data_chunk_errors() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("truncated.wav");
    let samples: Vec<i32> = (0..1024).map(|i| (i * 17) % 4096).collect();
    write_wav(&path, &samples, 2, 48000).unwrap();

    // Half the samples away, the header still promising all of them.
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() - 1024]).unwrap();

    let mut r = hound::WavReader::open(&path).unwrap();
    let first = r.samples::<i16>().next();
    assert!(
        matches!(&first, Some(Err(hound::Error::IoError(e))) if e.kind() == std::io::ErrorKind::UnexpectedEof),
        "expected UnexpectedEof, got {first:?}"
    );
}

/// A minimal PCM WAVE header with `block_align` and `bits_per_sample` stated
/// independently, so a padded container can be handed to the reader.
fn wav_bytes(channels: u16, bits: u16, block_align: u16, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // WAVE_FORMAT_PCM
    v.extend_from_slice(&channels.to_le_bytes());
    v.extend_from_slice(&48000u32.to_le_bytes());
    v.extend_from_slice(&(48000 * u32::from(block_align)).to_le_bytes());
    v.extend_from_slice(&block_align.to_le_bytes());
    v.extend_from_slice(&bits.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&(data.len() as u32).to_le_bytes());
    v.extend_from_slice(data);
    v
}

/// The header pair the reader must not paper over: a container wider than the
/// depth (hound: `Unsupported`) and a depth that is not whole bytes (hound:
/// `FormatError`). Reading either as if it were packed would silently double
/// the sample count and halve the duration.
#[test]
fn container_width_and_odd_depths_are_refused() {
    let padded = wav_bytes(1, 8, 2, &[0x80, 0x00, 0x90, 0x00]);
    assert!(
        matches!(
            hound::WavReader::new(std::io::Cursor::new(padded)),
            Err(hound::Error::Unsupported)
        ),
        "8-bit samples in a 2-byte container must be refused"
    );

    for bits in [12u16, 20] {
        let odd = wav_bytes(1, bits, bits.div_ceil(8), &[0; 8]);
        assert!(
            matches!(
                hound::WavReader::new(std::io::Cursor::new(odd)),
                Err(hound::Error::FormatError(_))
            ),
            "{bits}-bit must be a FormatError"
        );
    }

    // The well-formed shape of the same helper still opens, so the refusals
    // above are about the fields under test and not about the fixture.
    let ok = wav_bytes(2, 16, 4, &[0; 16]);
    let r = hound::WavReader::new(std::io::Cursor::new(ok)).expect("a packed 16-bit stereo header");
    assert_eq!(r.spec().bits_per_sample, 16);
}

#[test]
fn errors_carry_the_incumbent_variants() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 48000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(dir.join("errors.wav"), spec).unwrap();
    // Wider than the declared depth, and the wrong sample family: the two
    // failures callers of hound match on.
    assert!(matches!(
        w.write_sample(40_000i32),
        Err(hound::Error::TooWide)
    ));
    assert!(matches!(
        w.write_sample(0.5f32),
        Err(hound::Error::InvalidSampleFormat)
    ));
    w.write_sample(1i16).unwrap();
    assert!(matches!(w.finalize(), Err(hound::Error::UnfinishedSample)));
    // A directory is not a file: I/O keeps its identity through the shim.
    assert!(matches!(
        hound::WavWriter::create(dir, spec),
        Err(hound::Error::IoError(_))
    ));
}
