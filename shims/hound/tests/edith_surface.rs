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
