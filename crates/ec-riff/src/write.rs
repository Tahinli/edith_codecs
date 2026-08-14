//! Streaming WAVE writer.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use ec_core::{Error, Result};

use crate::{FORMAT_EXTENSIBLE, SampleType, WavSpec};

/// A sample a [`WavWriter`] accepts.
///
/// Integer types write to integer files and `f32` writes to float files; the
/// crossing combinations are refused rather than guessed at, because the scale
/// factor between them is a decision the caller owns.
pub trait Sample: Copy {
    /// True for the float carriers.
    const FLOAT: bool;
    /// The value as a 32-bit integer, at the file's own depth.
    fn as_i32(self) -> i32;
    /// The value as an `f32`.
    fn as_f32(self) -> f32;
}

macro_rules! int_sample {
    ($($t:ty),*) => {$(
        impl Sample for $t {
            const FLOAT: bool = false;
            fn as_i32(self) -> i32 {
                i32::from(self)
            }
            fn as_f32(self) -> f32 {
                self as f32
            }
        }
    )*};
}
int_sample!(i8, i16);

impl Sample for i32 {
    const FLOAT: bool = false;
    fn as_i32(self) -> i32 {
        self
    }
    fn as_f32(self) -> f32 {
        self as f32
    }
}

impl Sample for f32 {
    const FLOAT: bool = true;
    fn as_i32(self) -> i32 {
        self as i32
    }
    fn as_f32(self) -> f32 {
        self
    }
}

/// Writes a WAVE file sample by sample, patching the sizes in [`Self::finalize`].
///
/// Samples are interleaved in channel order: one call per channel per frame.
pub struct WavWriter<W: Write + Seek> {
    /// `None` only after [`WavWriter::finalize`] has taken it, which is also
    /// what tells `Drop` there is nothing left to patch.
    inner: Option<W>,
    spec: WavSpec,
    /// Absolute offset of the `RIFF` size field.
    riff_size_pos: u64,
    /// Absolute offset of the `fact` sample-count field, when one was written.
    fact_pos: Option<u64>,
    /// Absolute offset of the `data` size field.
    data_size_pos: u64,
    /// Samples (not frames) written so far.
    samples: u64,
}

impl WavWriter<BufWriter<File>> {
    /// Create `path`, truncating it, and write the header for `spec`.
    pub fn create<P: AsRef<Path>>(path: P, spec: WavSpec) -> Result<Self> {
        WavWriter::new(BufWriter::new(File::create(path)?), spec)
    }
}

impl<W: Write + Seek> WavWriter<W> {
    /// Write the header for `spec` at the writer's current position.
    pub fn new(mut inner: W, spec: WavSpec) -> Result<Self> {
        spec.validate()?;
        let start = inner.stream_position()?;
        let extensible = spec.is_extensible();
        let fmt_len: u32 = if extensible { 40 } else { 16 };
        let mut h: Vec<u8> = Vec::with_capacity(60);

        h.extend_from_slice(b"RIFF");
        let riff_size_pos = start + h.len() as u64;
        h.extend_from_slice(&0u32.to_le_bytes()); // patched by finalize
        h.extend_from_slice(b"WAVE");

        h.extend_from_slice(b"fmt ");
        h.extend_from_slice(&fmt_len.to_le_bytes());
        let tag = if extensible {
            FORMAT_EXTENSIBLE
        } else {
            spec.format_tag()
        };
        let block_align = spec.block_align() as u32;
        h.extend_from_slice(&tag.to_le_bytes());
        h.extend_from_slice(&spec.channels.to_le_bytes());
        h.extend_from_slice(&spec.sample_rate.to_le_bytes());
        h.extend_from_slice(&(spec.sample_rate * block_align).to_le_bytes());
        h.extend_from_slice(&(block_align as u16).to_le_bytes());
        h.extend_from_slice(&spec.bits_per_sample.to_le_bytes());
        if extensible {
            h.extend_from_slice(&22u16.to_le_bytes()); // cbSize
            h.extend_from_slice(&spec.bits_per_sample.to_le_bytes()); // valid bits
            h.extend_from_slice(&spec.channel_mask().to_le_bytes());
            // KSDATAFORMAT_SUBTYPE_{PCM,IEEE_FLOAT}: the tag, then the fixed
            // `0000-0010-8000-00aa00389b71` tail every WAVE sub-format shares.
            h.extend_from_slice(&spec.format_tag().to_le_bytes());
            h.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00]);
            h.extend_from_slice(&[0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71]);
        }

        // A `fact` chunk is required for every non-PCM tag and harmless for the
        // extensible PCM one; it holds the frame count, which finalize knows.
        let fact_pos = if extensible || spec.sample_format == SampleType::Float {
            h.extend_from_slice(b"fact");
            h.extend_from_slice(&4u32.to_le_bytes());
            let pos = start + h.len() as u64;
            h.extend_from_slice(&0u32.to_le_bytes());
            Some(pos)
        } else {
            None
        };

        h.extend_from_slice(b"data");
        let data_size_pos = start + h.len() as u64;
        h.extend_from_slice(&0u32.to_le_bytes());
        inner.write_all(&h)?;

        Ok(WavWriter {
            inner: Some(inner),
            spec,
            riff_size_pos,
            fact_pos,
            data_size_pos,
            samples: 0,
        })
    }

    /// The stream being written; present until `finalize` takes it.
    fn w(&mut self) -> &mut W {
        self.inner.as_mut().expect("WavWriter used after finalize")
    }

    /// The spec the header was written with.
    pub fn spec(&self) -> WavSpec {
        self.spec
    }

    /// Frames (one sample per channel) written so far.
    pub fn duration(&self) -> u64 {
        self.samples / u64::from(self.spec.channels)
    }

    /// Write one sample of one channel.
    pub fn write_sample<S: Sample>(&mut self, sample: S) -> Result<()> {
        let bits = self.spec.bits_per_sample;
        match self.spec.sample_format {
            SampleType::Float => {
                if !S::FLOAT {
                    return Err(Error::unsupported(
                        "integer samples into a float WAVE file",
                        "the integer-to-float scale factor is the caller's decision",
                    ));
                }
                self.w().write_all(&sample.as_f32().to_le_bytes())?;
            }
            SampleType::Int => {
                if S::FLOAT {
                    return Err(Error::unsupported(
                        "float samples into an integer WAVE file",
                        "the float-to-integer scale factor is the caller's decision",
                    ));
                }
                let v = sample.as_i32();
                let limit = 1i64 << (bits - 1);
                if i64::from(v) < -limit || i64::from(v) >= limit {
                    return Err(Error::corrupt(format!(
                        "sample {v} does not fit {bits} bits"
                    )));
                }
                match bits {
                    // 8-bit WAVE is unsigned, biased by 128.
                    8 => self.w().write_all(&[(v + 128) as u8])?,
                    16 => self.w().write_all(&(v as i16).to_le_bytes())?,
                    24 => self.w().write_all(&v.to_le_bytes()[..3])?,
                    _ => self.w().write_all(&v.to_le_bytes())?,
                }
            }
        }
        self.samples += 1;
        Ok(())
    }

    /// Write a run of interleaved samples.
    pub fn write_samples<S: Sample>(&mut self, samples: &[S]) -> Result<()> {
        for &s in samples {
            self.write_sample(s)?;
        }
        Ok(())
    }

    /// Pad to an even boundary, patch the sizes, flush, and hand the stream back.
    ///
    /// Dropping instead patches on a best-effort basis and swallows the error;
    /// only this reports the difference between a finished file and a truncated
    /// one, which is why it exists next to `Drop`.
    pub fn finalize(mut self) -> Result<W> {
        if !self.samples.is_multiple_of(u64::from(self.spec.channels)) {
            return Err(Error::corrupt(format!(
                "{} samples is not a whole number of {}-channel frames",
                self.samples, self.spec.channels
            )));
        }
        self.patch()?;
        self.w().flush()?;
        Ok(self.inner.take().expect("WavWriter finalized twice"))
    }

    /// The size patching both `finalize` and `Drop` go through.
    fn patch(&mut self) -> Result<()> {
        let data_bytes = self.samples * self.spec.bytes_per_sample() as u64;
        let end = self.data_size_pos + 4 + data_bytes;
        // RIFF chunks are word-aligned: an odd `data` gets a pad byte that its
        // own size field does not count.
        let pad = data_bytes % 2;
        let riff_size = end + pad - (self.riff_size_pos + 4);
        if riff_size > u64::from(u32::MAX) {
            return Err(Error::unsupported(
                "a WAVE file larger than 4 GiB",
                "the 64-bit RF64/BW64 extension is not implemented",
            ));
        }
        let (riff_pos, data_pos) = (self.riff_size_pos, self.data_size_pos);
        let fact = self
            .fact_pos
            .map(|pos| (pos, (self.samples / u64::from(self.spec.channels)) as u32));
        let w = self.w();
        if pad == 1 {
            w.seek(SeekFrom::Start(end))?;
            w.write_all(&[0])?;
        }
        w.seek(SeekFrom::Start(riff_pos))?;
        w.write_all(&(riff_size as u32).to_le_bytes())?;
        if let Some((pos, frames)) = fact {
            w.seek(SeekFrom::Start(pos))?;
            w.write_all(&frames.to_le_bytes())?;
        }
        w.seek(SeekFrom::Start(data_pos))?;
        w.write_all(&(data_bytes as u32).to_le_bytes())?;
        w.seek(SeekFrom::Start(end + pad))?;
        Ok(())
    }
}

impl<W: Write + Seek> Drop for WavWriter<W> {
    fn drop(&mut self) {
        if self.inner.is_some() {
            let _ = self.patch();
            let _ = self.w().flush();
        }
    }
}
