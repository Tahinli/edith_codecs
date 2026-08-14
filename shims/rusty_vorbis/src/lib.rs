//! Drop-in replacement for the `rusty_vorbis` 0.1.1 surface the replica
//! consumes, implemented over [`ec_vorbis`].
//!
//! It carries the incumbent's package name and version so the swap is a
//! `[patch.crates-io]` entry and nothing else. The scope is the API
//! `engine/src/export.rs` calls (`export.rs:2850-2862`):
//!
//! ```no_run
//! let mut encoder = rusty_vorbis::VorbisEncoder::new(rusty_vorbis::VorbisEncoderConfig {
//!     bitrate_bps: rusty_vorbis::BITRATE_NOMINAL,
//!     quality: 0.85,
//! });
//! encoder.push_pcm_s16(&[0i16; 2048], 2, 48_000).unwrap();
//! encoder.finish();
//! loop {
//!     match encoder.next_packet() {
//!         Ok(packet) => { let _ = (&packet.data, packet.pts, packet.duration); }
//!         Err(rusty_vorbis::Error::Eof) => break,
//!         Err(e) => panic!("{e}"),
//!     }
//! }
//! ```
//!
//! Two of the incumbent's behaviours are deliberately *not* reproduced, because
//! they are the defects this replacement exists to remove — both are corrections
//! the replica currently applies by hand at `export.rs:2762-2815` and can delete:
//!
//! * **No pre-roll hop.** The incumbent's first audio packet advanced the
//!   granule by a full block hop while decoding to nothing, so every caller had
//!   to feed a hop of silence and subtract a hop from every granule. Here the
//!   first block is centred on input sample 0 and [`EncodedPacket::pts`] is the
//!   true granule, so a caller writes what it is given.
//! * **No stereo-only setup header.** The incumbent shipped one embedded stereo
//!   profile and refused mono ("bad coupling channels") and anything wider. Here
//!   the setup header is written per stream for whatever channel count is
//!   pushed, so mono stays mono and 5.1 stays 5.1.
//!
//! The last packet's `pts` is the input's own sample count, so a file decodes to
//! exactly as many samples as went in; the caller does not have to state the
//! tail granule itself either.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use ec_core::Error as EcError;
use ec_vorbis::{EncoderConfig, VorbisEncoder as EcEncoder};

/// Short block size, as a power of two, that this encoder states.
pub const BS0_LOG2: u8 = 11;
/// Long block size, as a power of two.
pub const BS1_LOG2: u8 = 11;
/// The nominal bitrate the replica asks for.
pub const BITRATE_NOMINAL: i32 = 128_000;

/// Errors this encoder can answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Something is not implemented.
    Unimplemented(String),
    /// No further packet: the encoder is finished and drained.
    Eof,
    /// More input is needed before another packet exists.
    Again,
    /// The input or the state does not make sense.
    InvalidData(String),
    /// A legal request this encoder does not serve.
    Unsupported(String),
}

impl Error {
    /// An [`Error::InvalidData`] with a message.
    pub fn invalid(msg: impl Into<String>) -> Self {
        Error::InvalidData(msg.into())
    }

    /// An [`Error::Unsupported`] with a message.
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Error::Unsupported(msg.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unimplemented(what) => write!(f, "not yet implemented: {what}"),
            Error::Eof => write!(f, "end of stream"),
            Error::Again => write!(f, "more input required"),
            Error::InvalidData(msg) => write!(f, "invalid data: {msg}"),
            Error::Unsupported(msg) => write!(f, "unsupported: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// This crate's result type.
pub type Result<T> = std::result::Result<T, Error>;

fn map(error: EcError) -> Error {
    match error {
        EcError::Eof => Error::Eof,
        EcError::NeedMore => Error::Again,
        EcError::Unsupported { what, why } => Error::Unsupported(format!("{what} ({why})")),
        other => Error::InvalidData(other.to_string()),
    }
}

/// One packet out of the encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedPacket {
    /// Packet payload, ready to page.
    pub data: Vec<u8>,
    /// Granule position this packet ends at: the number of input samples that
    /// are decodable once it has been decoded. Zero for the three headers and
    /// for the first audio packet, which decodes to no samples at all.
    pub pts: i64,
    /// Samples this packet finalises.
    pub duration: i64,
}

/// How the encoder is set up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VorbisEncoderConfig {
    /// Target bitrate in bits per second.
    pub bitrate_bps: i32,
    /// Quality on `[0, 1]`.
    pub quality: f32,
}

impl Default for VorbisEncoderConfig {
    fn default() -> VorbisEncoderConfig {
        VorbisEncoderConfig {
            bitrate_bps: BITRATE_NOMINAL,
            quality: 0.5,
        }
    }
}

/// The incumbent's `[0, 1]` quality scale from Vorbis's own `-q` scale.
pub fn quality01_from_vorbis_q(q: f64) -> f32 {
    (((q + 1.0) / 11.0).clamp(0.0, 1.0)) as f32
}

/// Vorbis encoder over [`ec_vorbis`].
///
/// Channel count and sample rate arrive with the first push, exactly as they do
/// in the incumbent, so the encoder is built lazily and the setup header is
/// written for the layout that actually turns up.
pub struct VorbisEncoder {
    config: VorbisEncoderConfig,
    inner: Option<EcEncoder>,
    /// Header packets not yet handed out.
    headers: std::collections::VecDeque<Vec<u8>>,
    finished: bool,
}

impl VorbisEncoder {
    /// A new encoder; nothing is written until the first push states a layout.
    pub fn new(config: VorbisEncoderConfig) -> Self {
        VorbisEncoder {
            config,
            inner: None,
            headers: std::collections::VecDeque::new(),
            finished: false,
        }
    }

    /// Change the quality; takes effect on the next stream.
    pub fn set_quality(&mut self, quality: f32) {
        self.config.quality = quality;
    }

    /// Change the target bitrate; takes effect on the next stream.
    pub fn set_bitrate_bps(&mut self, bps: i32) {
        self.config.bitrate_bps = bps;
    }

    /// The three header packets, once a push has stated the layout.
    pub fn headers(&self) -> Vec<Vec<u8>> {
        self.inner
            .as_ref()
            .map(|encoder| encoder.headers().into_iter().map(<[u8]>::to_vec).collect())
            .unwrap_or_default()
    }

    /// The three headers Xiph-laced, the form a container carries them in.
    pub fn extradata(&self) -> Vec<u8> {
        self.inner
            .as_ref()
            .map(EcEncoder::extradata)
            .unwrap_or_default()
    }

    /// Push interleaved float samples.
    pub fn push_pcm_f32(
        &mut self,
        interleaved: &[f32],
        channels: u16,
        sample_rate: u32,
    ) -> Result<()> {
        self.start(channels, sample_rate)?;
        let encoder = self.inner.as_mut().expect("started");
        encoder.push_interleaved(interleaved).map_err(map)
    }

    /// Push interleaved 16-bit samples, scaled by 1/32768 as every decoder in
    /// this family scales them.
    pub fn push_pcm_s16(
        &mut self,
        interleaved: &[i16],
        channels: u16,
        sample_rate: u32,
    ) -> Result<()> {
        let floats: Vec<f32> = interleaved
            .iter()
            .map(|&sample| f32::from(sample) / 32_768.0)
            .collect();
        self.push_pcm_f32(&floats, channels, sample_rate)
    }

    /// No more input; the tail is encoded and the last granule is the input's
    /// own sample count.
    pub fn finish(&mut self) {
        self.finished = true;
        if let Some(encoder) = self.inner.as_mut() {
            encoder.finish();
        }
    }

    /// Next packet: the three headers first, then audio, then [`Error::Eof`].
    pub fn next_packet(&mut self) -> Result<EncodedPacket> {
        if let Some(data) = self.headers.pop_front() {
            return Ok(EncodedPacket {
                data,
                pts: 0,
                duration: 0,
            });
        }
        let Some(encoder) = self.inner.as_mut() else {
            return Err(match self.finished {
                true => Error::Eof,
                false => Error::Again,
            });
        };
        let packet = encoder.next_packet().map_err(map)?;
        Ok(EncodedPacket {
            data: packet.data,
            pts: packet.granule,
            duration: packet.samples,
        })
    }

    /// Build the encoder on the first push and queue its headers.
    fn start(&mut self, channels: u16, sample_rate: u32) -> Result<()> {
        if self.inner.is_some() {
            return Ok(());
        }
        if self.finished {
            return Err(Error::invalid("push after finish"));
        }
        let encoder = EcEncoder::new(EncoderConfig {
            sample_rate,
            channels,
            bitrate_bps: self.config.bitrate_bps,
            quality: self.config.quality,
        })
        .map_err(map)?;
        self.headers = encoder.headers().into_iter().map(<[u8]>::to_vec).collect();
        self.inner = Some(encoder);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_incumbents_call_sequence_yields_headers_then_audio() {
        let mut encoder = VorbisEncoder::new(VorbisEncoderConfig {
            bitrate_bps: BITRATE_NOMINAL,
            quality: 0.85,
        });
        // Mono, which the incumbent refused outright.
        let pcm: Vec<i16> = (0..8_000)
            .map(|i| ((f64::from(i) * 0.05).sin() * 12_000.0) as i16)
            .collect();
        encoder.push_pcm_s16(&pcm, 1, 48_000).unwrap();
        encoder.finish();
        let mut packets = Vec::new();
        loop {
            match encoder.next_packet() {
                Ok(packet) => packets.push(packet),
                Err(Error::Eof) => break,
                Err(e) => panic!("{e}"),
            }
        }
        assert!(packets.len() > 4, "{} packets", packets.len());
        assert_eq!(packets[0].data[0], 1, "identification header first");
        assert_eq!(packets[1].data[0], 3, "comment header second");
        assert_eq!(packets[2].data[0], 5, "setup header third");
        // No pre-roll: the first audio packet ends at granule zero, and the
        // last states the input's own length.
        assert_eq!(packets[3].pts, 0);
        assert_eq!(packets.last().unwrap().pts, 8_000);
        assert_eq!(quality01_from_vorbis_q(-1.0), 0.0);
    }
}
