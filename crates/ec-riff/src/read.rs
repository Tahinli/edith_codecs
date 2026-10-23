//! WAVE reader: chunk-walking, tolerant of everything but a broken `fmt `.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use ec_core::{Error, Result};

use crate::{FORMAT_EXTENSIBLE, FORMAT_FLOAT, FORMAT_PCM, SampleType, WavCompressed, WavSpec};

/// Reads the header of a WAVE stream, then its samples.
///
/// Construction stops at the `data` chunk, so the reader needs only [`Read`]:
/// chunks before `data` (`LIST`, `INFO`, `fact`, `bext`, anything) are skipped
/// with their pad byte, and chunks after it are never looked at.
pub struct WavReader<R: Read> {
    inner: R,
    spec: WavSpec,
    compressed: Option<WavCompressed>,
    /// Bytes the `data` header claimed, or `None` when it claimed a streaming
    /// placeholder and the file itself is the bound.
    data_bytes: Option<u64>,
}

impl WavReader<BufReader<File>> {
    /// Open `path` and parse its header.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        WavReader::new(BufReader::new(File::open(path)?))
    }
}

impl<R: Read> WavReader<R> {
    /// Parse the header of an already-open stream, leaving it at the samples.
    pub fn new(mut inner: R) -> Result<Self> {
        let riff = read_exact::<12, _>(&mut inner)?;
        let is_rf64 = &riff[0..4] == b"RF64" || &riff[0..4] == b"BW64";
        if !(is_rf64 || &riff[0..4] == b"RIFF") || &riff[8..12] != b"WAVE" {
            return Err(Error::corrupt("not a RIFF/WAVE file"));
        }

        let mut spec: Option<(WavSpec, Option<WavCompressed>)> = None;
        // The `ds64` chunk's data size, which an RF64 `data` chunk defers to.
        let mut ds64_data: Option<u64> = None;
        loop {
            let head = read_exact::<8, _>(&mut inner)?;
            let id = [head[0], head[1], head[2], head[3]];
            let size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as u64;
            match &id {
                b"fmt " => spec = Some(parse_fmt(&mut inner, size)?),
                b"ds64" if is_rf64 => {
                    // riffSize, dataSize, sampleCount, then a table we do not need.
                    if size < 24 {
                        return Err(Error::corrupt(format!("RF64 ds64 chunk is {size} bytes")));
                    }
                    let body = read_exact::<24, _>(&mut inner)?;
                    ds64_data = Some(u64::from_le_bytes(body[8..16].try_into().unwrap_or([0; 8])));
                    skip(&mut inner, size - 24 + size % 2)?;
                }
                b"data" => {
                    let (spec, compressed) = spec.ok_or_else(|| {
                        Error::corrupt("WAVE data chunk arrived before any fmt chunk")
                    })?;
                    // 0 and 0xFFFFFFFF are both used by streamed writers that
                    // never came back to patch the size. In RF64 the 0xFFFFFFFF
                    // is a sentinel for the ds64 value; without one it is
                    // the same placeholder as in RIFF.
                    let data_bytes = if size == u64::from(u32::MAX) && is_rf64 {
                        ds64_data
                    } else if size == 0 || size == u64::from(u32::MAX) {
                        None
                    } else {
                        Some(size)
                    };
                    return Ok(WavReader {
                        inner,
                        spec,
                        compressed,
                        data_bytes: data_bytes.filter(|&n| n != 0),
                    });
                }
                _ => skip(&mut inner, size + size % 2)?,
            }
        }
    }

    /// The `fmt ` chunk as parsed.
    pub fn spec(&self) -> WavSpec {
        self.spec
    }

    /// Frames (one sample per channel) the `data` header claims, when it claims
    /// one. A truncated file reports fewer from [`Self::read_all_i32`].
    pub fn duration(&self) -> Option<u64> {
        match &self.compressed {
            Some(c) => self.data_bytes.map(|b| c.frames_in(b)),
            None => self.data_bytes.map(|b| b / self.spec.block_align() as u64),
        }
    }

    /// The format, when it is one of the compressed tags (IMA/MS ADPCM,
    /// A-law, mu-law) rather than PCM. [`Self::spec`] then describes the
    /// decoded 16-bit output.
    pub fn compressed(&self) -> Option<&WavCompressed> {
        self.compressed.as_ref()
    }

    /// The `data` chunk's raw bytes, whatever the format.
    pub fn read_data_bytes(&mut self) -> Result<Vec<u8>> {
        self.read_data()
    }

    /// Read the whole `data` chunk as interleaved bytes, stopping at whichever
    /// comes first: the declared size or the end of the stream.
    fn read_data(&mut self) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        match self.data_bytes {
            Some(n) => {
                // `take` + `read_to_end` grows as data actually arrives, so a
                // header claiming 4 GiB over a 1 KiB file allocates 1 KiB.
                (&mut self.inner).take(n).read_to_end(&mut buf)?;
            }
            None => {
                self.inner.read_to_end(&mut buf)?;
            }
        }
        let align = match &self.compressed {
            Some(c) if c.tag == 0x0006 || c.tag == 0x0007 => usize::from(c.channels.max(1)),
            Some(_) => 1,
            None => self.spec.block_align(),
        };
        buf.truncate(buf.len() / align * align);
        Ok(buf)
    }

    /// Read every integer sample, interleaved, sign-extended to `i32`.
    ///
    /// Refuses float files: [`Self::read_all_f32`] is their reader.
    pub fn read_all_i32(&mut self) -> Result<Vec<i32>> {
        if self.spec.sample_format == SampleType::Float {
            return Err(Error::unsupported(
                "reading a float WAVE file as integers",
                "use read_all_f32; the scale factor is the caller's decision",
            ));
        }
        let bits = self.spec.bits_per_sample;
        let width = self.spec.bytes_per_sample();
        let buf = self.read_data()?;
        Ok(buf
            .chunks_exact(width)
            .map(|c| match bits {
                // Unsigned and biased, alone among the depths.
                8 => i32::from(c[0]) - 128,
                16 => i32::from(i16::from_le_bytes([c[0], c[1]])),
                24 => i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8,
                _ => i32::from_le_bytes([c[0], c[1], c[2], c[3]]),
            })
            .collect())
    }

    /// Read every sample as `f32`, interleaved.
    ///
    /// Float files pass through untouched; integer files are normalised by
    /// `2^(bits-1)` — the same scaling the oracle's `pcm_f32le` conversion applies,
    /// so the two outputs are comparable sample for sample.
    pub fn read_all_f32(&mut self) -> Result<Vec<f32>> {
        if self.spec.sample_format == SampleType::Float {
            let buf = self.read_data()?;
            return Ok(buf
                .as_chunks::<4>().0.iter()
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect());
        }
        let scale = 1.0f32 / (1i64 << (self.spec.bits_per_sample - 1)) as f32;
        Ok(self
            .read_all_i32()?
            .into_iter()
            .map(|v| v as f32 * scale)
            .collect())
    }
}

/// Read exactly `N` bytes; a short read is a truncated file, not a panic.
fn read_exact<const N: usize, R: Read>(r: &mut R) -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    r.read_exact(&mut buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::corrupt("WAVE header ends mid-chunk")
        } else {
            Error::Io(e)
        }
    })?;
    Ok(buf)
}

/// Discard `n` bytes without seeking, so the reader works over pipes too.
fn skip<R: Read>(r: &mut R, n: u64) -> Result<()> {
    let copied = std::io::copy(&mut r.take(n), &mut std::io::sink())?;
    if copied != n {
        return Err(Error::corrupt("WAVE chunk ends past the end of the file"));
    }
    Ok(())
}

/// Parse a `fmt ` chunk body of `size` bytes, extensible or not.
fn parse_fmt<R: Read>(r: &mut R, size: u64) -> Result<(WavSpec, Option<WavCompressed>)> {
    if size < 16 {
        return Err(Error::corrupt(format!("WAVE fmt chunk is {size} bytes")));
    }
    let f = read_exact::<16, _>(r)?;
    let mut tag = u16::from_le_bytes([f[0], f[1]]);
    let channels = u16::from_le_bytes([f[2], f[3]]);
    let sample_rate = u32::from_le_bytes([f[4], f[5], f[6], f[7]]);
    let block_align = u16::from_le_bytes([f[12], f[13]]);
    let bits_per_sample = u16::from_le_bytes([f[14], f[15]]);
    let mut rest = size - 16;
    let was_extensible = tag == FORMAT_EXTENSIBLE;

    if tag == FORMAT_EXTENSIBLE {
        if rest < 24 {
            return Err(Error::corrupt(
                "WAVE_FORMAT_EXTENSIBLE fmt chunk without its 22-byte extension",
            ));
        }
        let ext = read_exact::<24, _>(r)?;
        rest -= 24;
        // Only the first two bytes of the sub-format GUID name the encoding;
        // the rest is the fixed WAVE tail, and drivers in the wild vary it
        // enough that rejecting on it would refuse valid files.
        tag = u16::from_le_bytes([ext[8], ext[9]]);
    }
    let mut extra = Vec::new();
    if !was_extensible && matches!(tag, 0x0011 | 0x0002 | 0x0006 | 0x0007) && rest >= 2 {
        // cbSize then its payload.
        let cb = read_exact::<2, _>(r)?;
        rest -= 2;
        let cb = u64::from(u16::from_le_bytes(cb)).min(rest);
        extra.resize(cb as usize, 0);
        r.read_exact(&mut extra)?;
        rest -= cb;
    }
    skip(r, rest + size % 2)?;
    if let Some(c) = WavCompressed::new(tag, channels, block_align, &extra) {
        let spec = WavSpec {
            channels,
            sample_rate,
            bits_per_sample: 16,
            sample_format: SampleType::Int,
        };
        spec.validate()?;
        return Ok((spec, Some(c)));
    }

    let sample_format = match tag {
        FORMAT_PCM => SampleType::Int,
        FORMAT_FLOAT => SampleType::Float,
        other => {
            return Err(Error::unsupported(
                format!("WAVE format tag {other:#06x}"),
                "ec-riff carries integer and IEEE-float PCM only",
            ));
        }
    };
    // A depth that is not a whole number of bytes (12-bit, 20-bit) is
    // ill-formed for these tags, not merely unimplemented: nothing in the
    // header says how the odd bits sit in their container.
    if bits_per_sample % 8 != 0 {
        return Err(Error::corrupt(format!(
            "WAVE fmt: {bits_per_sample} bits per sample is not a multiple of eight"
        )));
    }
    let spec = WavSpec {
        channels,
        sample_rate,
        bits_per_sample,
        sample_format,
    };
    spec.validate()?;
    // The stride the file declares must be the stride the depth implies —
    // otherwise every frame after the first is read from the wrong offset.
    // Padded containers (8-bit samples in 2-byte slots) live here.
    if usize::from(block_align) != spec.block_align() {
        return Err(Error::unsupported(
            format!("a WAVE block align of {block_align} for {channels}x{bits_per_sample}-bit"),
            "ec-riff reads samples packed at their declared depth, without container padding",
        ));
    }
    Ok((spec, None))
}
