//! The `rusty_aac` 0.5 surface, served by `ec-aac`.
//!
//! Only what edith consumes is here, at the incumbent's signatures:
//! `AacDecoder::with_config_bytes` (audio.rs:1731), `AacEncoder::new` with
//! `AacEncoderConfig` (mux.rs:2116, export.rs:1756) and `sf_index_for_rate`
//! (export.rs:1678).  The encoder's tool flags are kept as fields so a caller
//! written against the incumbent still compiles; each one names what it maps to.

#![forbid(unsafe_code)]

pub use ec_aac::{
    AdtsHeader, AudioSpecificConfig, SAMPLE_RATES, audio_specific_config_bytes, is_adts,
    parse_adts, parse_audio_specific_config, sample_rate_for_index, sf_index_for_rate,
    write_adts_header, write_audio_specific_config,
};

/// The incumbent's error type: one variant per refusal reason, `Eof` included
/// so `while let Ok(p) = next_packet()` still terminates.
#[derive(Debug)]
pub enum Error {
    Eof,
    NeedMore,
    Invalid(String),
    Unsupported(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Eof => write!(f, "end of stream"),
            Error::NeedMore => write!(f, "need more data"),
            Error::Invalid(m) => write!(f, "invalid: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    pub fn invalid(msg: impl Into<String>) -> Error {
        Error::Invalid(msg.into())
    }

    pub fn unsupported(msg: impl Into<String>) -> Error {
        Error::Unsupported(msg.into())
    }
}

impl From<ec_aac::Error> for Error {
    fn from(e: ec_aac::Error) -> Error {
        match e {
            ec_aac::Error::Eof => Error::Eof,
            ec_aac::Error::NeedMore => Error::NeedMore,
            ec_aac::Error::Unsupported { .. } => Error::Unsupported(e.to_string()),
            other => Error::Invalid(other.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// One decoded frame, interleaved, in film channel order.
#[derive(Clone, Debug, Default)]
pub struct DecodedAudio {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
    pub pts: Option<i64>,
}

impl DecodedAudio {
    pub fn frames(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / usize::from(self.channels)
        }
    }
}

/// What the decoder does with a High Efficiency stream.
pub use ec_aac::SbrSupport;

/// An AAC-LC decoder.
pub struct AacDecoder(ec_aac::AacDecoder);

impl AacDecoder {
    pub fn new() -> AacDecoder {
        AacDecoder(ec_aac::AacDecoder::new())
    }

    pub fn with_config(cfg: AudioSpecificConfig) -> AacDecoder {
        AacDecoder(ec_aac::AacDecoder::with_config(cfg))
    }

    pub fn with_config_bytes(data: &[u8]) -> Result<AacDecoder> {
        Ok(AacDecoder(ec_aac::AacDecoder::with_config_bytes(data)?))
    }

    pub fn sbr_support(&self) -> SbrSupport {
        self.0.sbr_support()
    }

    pub fn output_sample_rate(&self) -> Option<u32> {
        self.0.output_sample_rate()
    }

    pub fn decode(&mut self, packet: &[u8], pts: Option<i64>) -> Result<DecodedAudio> {
        let d = self.0.decode(packet, pts)?;
        Ok(DecodedAudio {
            sample_rate: d.sample_rate,
            channels: d.channels,
            samples: d.samples,
            pts: d.pts,
        })
    }
}

impl Default for AacDecoder {
    fn default() -> AacDecoder {
        AacDecoder::new()
    }
}

/// Window-shape policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowShape {
    #[default]
    Sine,
    Kbd,
    /// Per-frame dispatch on content tonality; served here as [`WindowShape::Sine`].
    Auto,
}

/// Encoder options, at the incumbent's field set.
#[derive(Debug, Clone, Copy)]
pub struct AacEncoderConfig {
    /// Target bitrate in bits per second.
    pub bitrate_bps: u32,
    /// Window-shape policy.
    pub window_shape: WindowShape,
    /// The `Auto` gate's threshold; unused here, `Auto` codes as sine.
    pub shape_tonality_pct: f32,
    /// Run the psychoacoustic model on short blocks: always on here.
    pub short_block_psy: bool,
    /// Make the signal-to-mask ratio a function of band tonality: always on here.
    pub tonality_smr: bool,
    /// Emit TNS on long blocks; not emitted by this encoder (decoding it is
    /// complete either way).
    pub tns: bool,
    /// Level-invariant transient detector: always on here.
    pub relative_transients: bool,
    /// Perceptual noise substitution; not emitted by this encoder.
    pub pns: bool,
    /// Intensity stereo; not emitted by this encoder.
    pub intensity: bool,
    /// Split a pair's bit budget by perceptual demand: always on here, the rate
    /// loop is joint over the whole frame.
    pub stereo_bit_split: bool,
}

impl Default for AacEncoderConfig {
    fn default() -> AacEncoderConfig {
        AacEncoderConfig {
            bitrate_bps: 128_000,
            window_shape: WindowShape::Sine,
            shape_tonality_pct: 0.5,
            short_block_psy: true,
            tonality_smr: true,
            tns: false,
            relative_transients: true,
            pns: false,
            intensity: false,
            stereo_bit_split: true,
        }
    }
}

/// One encoded raw access unit (a `raw_data_block`, no ADTS framing).
#[derive(Debug, Clone)]
pub struct EncodedPacket {
    pub data: Vec<u8>,
    pub pts: i64,
    pub duration: u32,
}

/// An AAC-LC encoder.
pub struct AacEncoder(ec_aac::AacEncoder);

impl AacEncoder {
    pub fn new(config: AacEncoderConfig) -> AacEncoder {
        AacEncoder(ec_aac::AacEncoder::new(ec_aac::AacEncoderConfig {
            bitrate_bps: config.bitrate_bps.max(1),
            window_shape: match config.window_shape {
                WindowShape::Kbd => ec_aac::WindowShape::Kbd,
                _ => ec_aac::WindowShape::Sine,
            },
            adts: false,
            mid_side: true,
            window_switching: true,
        }))
    }

    pub fn sample_rate(&self) -> u32 {
        self.0.sample_rate()
    }

    pub fn channels(&self) -> u16 {
        self.0.channels()
    }

    pub fn push_pcm(&mut self, interleaved: &[f32], channels: u16, sample_rate: u32) -> Result<()> {
        Ok(self.0.push_pcm(interleaved, channels, sample_rate)?)
    }

    pub fn push_pcm_planar(&mut self, planes: &[&[f32]], sample_rate: u32) -> Result<()> {
        Ok(self.0.push_pcm_planar(planes, sample_rate)?)
    }

    pub fn finish(&mut self) {
        self.0.finish();
    }

    pub fn next_packet(&mut self) -> Result<EncodedPacket> {
        let p = self.0.next_packet()?;
        Ok(EncodedPacket {
            data: p.data,
            pts: p.pts,
            duration: p.duration,
        })
    }
}
