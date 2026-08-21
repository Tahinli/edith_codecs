//! RIFF/WAVE and AVI audio chunk reading.
//!
//! [`WavWriter`] streams samples as they arrive and patches the RIFF, `data`
//! and `fact` sizes in [`WavWriter::finalize`] — a dropped writer therefore
//! leaves a file whose header lies, which is why finalizing is fallible and
//! explicit. [`WavReader`] walks the chunk list rather than assuming
//! `fmt `-then-`data`, so `LIST`/`INFO`, `fact`, `bext` and any other chunk a
//! recorder inserted are skipped, odd-sized chunks included (RIFF pads them to
//! an even boundary and the padding byte is not part of the chunk).
//!
//! Covered: 8/16/24/32-bit integer PCM and 32-bit float, 1 to 8 channels
//! (mono through 7.1), any sample rate. `WAVE_FORMAT_EXTENSIBLE` is written
//! whenever the file has more than two channels or more than 16 bits per
//! sample — the depth/layout combinations where players are entitled to
//! distrust a plain `WAVE_FORMAT_PCM` header — and is read whatever the file
//! declares.
//!
//! Not covered: RF64/BW64 (>4 GiB), ADPCM and other compressed WAVE tags,
//! 64-bit float, AVI video frames, and non-audio AVI chunks. Each unsupported
//! WAVE tag is [`Error::Unsupported`] naming itself, never a silent misread.
//!
//! The crate stands alone — [`WavSpec`] is its own type — but converts into the
//! family IR on request: [`WavSpec::sample_format`] and [`WavSpec::layout`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod avi;
mod read;
mod write;

pub use avi::{AviAudioStream, AviPacket, AviReader};
pub use ec_core::{Error, Result};
pub use read::WavReader;
pub use write::{Sample, WavWriter};

/// Whether samples are stored as integers (`WAVE_FORMAT_PCM`) or as floats
/// (`WAVE_FORMAT_IEEE_FLOAT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleType {
    /// Signed integers, except 8-bit which RIFF stores unsigned and biased.
    Int,
    /// IEEE floats, nominal range -1.0..=1.0.
    Float,
}

/// What the `fmt ` chunk says about the audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WavSpec {
    /// Channel count, 1..=8 for the layouts this crate names.
    pub channels: u16,
    /// Sample rate in Hz; any rate the source states.
    pub sample_rate: u32,
    /// Bits per sample: 8, 16, 24 or 32 for [`SampleType::Int`], 32 for float.
    pub bits_per_sample: u16,
    /// Integer or float storage.
    pub sample_format: SampleType,
}

/// `WAVE_FORMAT_PCM`.
const FORMAT_PCM: u16 = 1;
/// `WAVE_FORMAT_IEEE_FLOAT`.
const FORMAT_FLOAT: u16 = 3;
/// `WAVE_FORMAT_EXTENSIBLE`.
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;

impl WavSpec {
    /// Bytes one sample of one channel occupies in the file.
    pub fn bytes_per_sample(&self) -> usize {
        usize::from(self.bits_per_sample).div_ceil(8)
    }

    /// Bytes one frame (one sample of every channel) occupies.
    pub fn block_align(&self) -> usize {
        self.bytes_per_sample() * usize::from(self.channels)
    }

    /// The equivalent family sample format. 24-bit widens to
    /// [`ec_core::SampleFormat::S32`], as the IR documents.
    pub fn sample_format(&self) -> Result<ec_core::SampleFormat> {
        use ec_core::SampleFormat as F;
        match (self.sample_format, self.bits_per_sample) {
            (SampleType::Int, 8) => Ok(F::U8),
            (SampleType::Int, 16) => Ok(F::S16),
            (SampleType::Int, 24 | 32) => Ok(F::S32),
            (SampleType::Float, 32) => Ok(F::F32),
            (t, b) => Err(Error::unsupported(
                format!("{b}-bit {t:?} WAVE samples"),
                "ec-riff carries 8/16/24/32-bit integer and 32-bit float PCM",
            )),
        }
    }

    /// The channel layout a bare channel count implies (mono, stereo, 5.1, 7.1).
    pub fn layout(&self) -> ec_core::ChannelLayout {
        ec_core::ChannelLayout::from_count(usize::from(self.channels))
    }

    /// Validate what a header must satisfy before either direction trusts it.
    fn validate(&self) -> Result<()> {
        if self.channels == 0 {
            return Err(Error::corrupt("WAVE fmt: zero channels"));
        }
        if self.sample_rate == 0 {
            return Err(Error::corrupt("WAVE fmt: zero sample rate"));
        }
        self.sample_format().map(|_| ())
    }

    /// `WAVE_FORMAT_EXTENSIBLE` is due for more than two channels or more than
    /// 16 bits — the cases where a bare PCM tag is ambiguous about layout.
    fn is_extensible(&self) -> bool {
        self.channels > 2 || self.bits_per_sample > 16
    }

    /// The `dwChannelMask` for the implied layout; 0 when the count has no
    /// standard mask, which is legal and means "unassigned".
    fn channel_mask(&self) -> u32 {
        use ec_core::ChannelPosition as P;
        let layout = self.layout();
        let mut mask = 0u32;
        for p in layout.positions() {
            mask |= match p {
                P::FrontLeft => 0x1,
                P::FrontRight => 0x2,
                P::FrontCenter => 0x4,
                P::Lfe => 0x8,
                P::BackLeft => 0x10,
                P::BackRight => 0x20,
                P::FrontLeftOfCenter => 0x40,
                P::FrontRightOfCenter => 0x80,
                P::BackCenter => 0x100,
                P::SideLeft => 0x200,
                P::SideRight => 0x400,
            };
        }
        // `ChannelLayout::from_count` fills unknown counts with repeated
        // FrontCenter, which would collapse to a one-bit mask claiming fewer
        // channels than the header. Unassigned is the honest answer there.
        if mask.count_ones() as u16 != self.channels {
            0
        } else {
            mask
        }
    }

    /// The `wFormatTag` (or extensible sub-format tag) for this spec.
    fn format_tag(&self) -> u16 {
        match self.sample_format {
            SampleType::Int => FORMAT_PCM,
            SampleType::Float => FORMAT_FLOAT,
        }
    }
}
