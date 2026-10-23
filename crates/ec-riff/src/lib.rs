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
//! RF64 and BW64 (>4 GiB) are written when [`WavOptions::expected_frames`] says the
//! data will not fit in 32 bits, and read whatever the size fields say.
//!
//! [`AviDemuxer`] is the full AVI audio+video demuxer (OpenDML, idx1 or linear scan, keyframes,
//! field order); [`AviReader`] is the older audio-only reader.
//!
//! Not covered: ADPCM and other compressed WAVE tags,
//! 64-bit float, and non-audio/video AVI chunks. Each unsupported
//! WAVE tag is [`Error::Unsupported`] naming itself, never a silent misread.
//!
//! The crate stands alone — [`WavSpec`] is its own type — but converts into the
//! family IR on request: [`WavSpec::sample_format`] and [`WavSpec::layout`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod avi;
mod demux;
mod rawvideo;
mod read;
mod write;

pub use avi::{AviAudioStream, AviIndexPoint, AviPacket, AviReader};
pub use demux::{AviDemuxer, AviIndexKind, AviTrackInfo, is_avi};
pub use ec_core::{Error, Result};
pub use rawvideo::{MOV_MARKER, RawVideoDecoder};
pub use read::WavReader;
pub use write::{RF64_THRESHOLD, Sample, WavOptions, WavWriter};

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


/// A non-PCM WAVE/AVI audio format this family can decode: IMA ADPCM
/// (`0x0011`), MS ADPCM (`0x0002`), A-law (`0x0006`) and mu-law (`0x0007`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WavCompressed {
    /// The `wFormatTag`.
    pub tag: u16,
    /// Channel count.
    pub channels: u16,
    /// `nBlockAlign`: bytes per coded block (the packet size for ADPCM).
    pub block_align: u16,
    /// Decoded samples per channel in one full block.
    pub samples_per_block: u32,
    /// The `cbSize` tail of the `fmt ` chunk (samples per block, plus the MS
    /// ADPCM coefficient table), passed to the decoder as extradata.
    pub extra: Vec<u8>,
}

impl WavCompressed {
    /// The codec a tag names, if it is one of the four.
    pub fn codec_of(tag: u16) -> Option<ec_core::registry::CodecId> {
        use ec_core::registry::CodecId;
        match tag {
            0x0011 => Some(CodecId::ImaAdpcmWav),
            0x0002 => Some(CodecId::MsAdpcm),
            0x0006 => Some(CodecId::PcmAlaw),
            0x0007 => Some(CodecId::PcmMulaw),
            _ => None,
        }
    }

    /// Build from the fields of a `fmt ` chunk.
    pub fn new(tag: u16, channels: u16, block_align: u16, extra: &[u8]) -> Option<WavCompressed> {
        Self::codec_of(tag)?;
        let ch = u32::from(channels.max(1));
        let ba = u32::from(block_align);
        let spb = match tag {
            0x0011 => (ba.saturating_sub(4 * ch) / (4 * ch)) * 8 + 1,
            0x0002 => ba.saturating_sub(6 * ch) * 2 / ch,
            _ => 1,
        };
        Some(WavCompressed {
            tag,
            channels,
            block_align,
            samples_per_block: spb,
            extra: extra.to_vec(),
        })
    }

    /// Frames (samples per channel) in `bytes` bytes of coded data.
    pub fn frames_in(&self, bytes: u64) -> u64 {
        let ch = u64::from(self.channels.max(1));
        match self.tag {
            0x0006 | 0x0007 => bytes / ch,
            _ => {
                let ba = u64::from(self.block_align.max(1));
                bytes / ba * u64::from(self.samples_per_block)
            }
        }
    }
}
