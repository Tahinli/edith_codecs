//! `rusty_mp3` 0.6.1's surface, over [`ec_mp3`].
//!
//! This crate exists to be swapped in by `[patch.crates-io]`: it carries the
//! incumbent's name and version so a consumer's `use rusty_mp3::…` compiles
//! unchanged. Only what edith consumes plus the streaming decoder beside it is
//! here — [`Mp3Encoder`] with [`Mp3EncoderConfig`], [`Mp3Decoder`] with
//! [`DecodedAudio`], and an [`Error`] carrying the incumbent's `Eof`/`Again`
//! contract — because a shim that grows a surface nobody calls is a second
//! implementation to keep honest.
//!
//! Behaviour differences from the incumbent, all stated: `vbr_quality` picks a
//! constant bitrate rather than varying the rate per frame (see
//! [`ec_mp3::encode::Mp3EncoderConfig`]), and the encoder writes the `Info`
//! header frame first, as the incumbent does.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::VecDeque;
use std::fmt;

/// The incumbent's error type.
#[derive(Debug)]
pub enum Error {
    /// No more frames will arrive.
    Eof,
    /// Not enough data buffered yet; push more and retry.
    Again,
    /// The bitstream is malformed, or the configuration is not codable.
    Format(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Eof => write!(f, "end of stream"),
            Error::Again => write!(f, "need more data"),
            Error::Format(why) => write!(f, "mp3: {why}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<ec_core::error::Error> for Error {
    fn from(error: ec_core::error::Error) -> Error {
        match error {
            ec_core::error::Error::Eof => Error::Eof,
            ec_core::error::Error::NeedMore => Error::Again,
            other => Error::Format(other.to_string()),
        }
    }
}

/// The incumbent's result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// One decoded frame of audio.
#[derive(Debug, Clone)]
pub struct DecodedAudio {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u16,
    /// Interleaved samples in `[-1, 1]`.
    pub samples: Vec<f32>,
}

/// A streaming MP3 decoder.
#[derive(Debug, Default)]
pub struct Mp3Decoder {
    reader: ec_mp3::Mp3Reader,
    queue: VecDeque<DecodedAudio>,
    eof: bool,
}

impl Mp3Decoder {
    /// A decoder with nothing buffered.
    pub fn new() -> Mp3Decoder {
        Mp3Decoder::default()
    }

    /// Buffers bytes and decodes whatever whole frames they complete.
    pub fn push(&mut self, bytes: &[u8]) {
        self.reader.push(bytes);
        while let Ok(frame) = self.reader.next_frame() {
            self.queue.push_back(DecodedAudio {
                sample_rate: frame.sample_rate,
                channels: frame.channels as u16,
                samples: frame.samples,
            });
        }
    }

    /// The next decoded frame, [`Error::Again`] until one is complete and
    /// [`Error::Eof`] once [`Mp3Decoder::flush`] has been called and drained.
    pub fn next_frame(&mut self) -> Result<DecodedAudio> {
        match self.queue.pop_front() {
            Some(frame) => Ok(frame),
            None if self.eof => Err(Error::Eof),
            None => Err(Error::Again),
        }
    }

    /// Marks the end of input.
    pub fn flush(&mut self) {
        self.eof = true;
    }
}

/// Encoder settings, field for field as the incumbent declares them.
#[derive(Debug, Clone, Default)]
pub struct Mp3EncoderConfig {
    /// Constant bitrate in kbit/s.
    pub bitrate_kbps: u32,
    /// Quality on a normalised `[0, 1]` scale instead of a bitrate.
    pub vbr_quality: Option<f32>,
}

/// The incumbent's quality-to-bitrate curve, in kbit/s.
pub fn vbr_quality_index(q: f32) -> f32 {
    ec_mp3::encode::bitrate_for_quality(q) as f32
}

/// A streaming MP3 encoder.
#[derive(Debug)]
pub struct Mp3Encoder {
    inner: ec_mp3::Mp3Encoder,
}

impl Mp3Encoder {
    /// An encoder that configures itself from the first PCM it is given.
    pub fn new(config: Mp3EncoderConfig) -> Mp3Encoder {
        Mp3Encoder {
            inner: ec_mp3::Mp3Encoder::new(ec_mp3::Mp3EncoderConfig {
                bitrate_kbps: config.bitrate_kbps,
                vbr_quality: config.vbr_quality,
            }),
        }
    }

    /// Feeds interleaved `f32` samples.
    pub fn push_pcm_f32(
        &mut self,
        interleaved: &[f32],
        channels: u16,
        sample_rate: u32,
    ) -> Result<()> {
        Ok(self
            .inner
            .push_pcm_f32(interleaved, channels, sample_rate)?)
    }

    /// Feeds interleaved 16-bit samples.
    pub fn push_pcm_s16(
        &mut self,
        interleaved: &[i16],
        channels: u16,
        sample_rate: u32,
    ) -> Result<()> {
        Ok(self
            .inner
            .push_pcm_s16(interleaved, channels, sample_rate)?)
    }

    /// Ends the stream and flushes the tail.
    pub fn finish(&mut self) {
        self.inner.finish();
    }

    /// The next complete frame; [`Error::Eof`] once drained after
    /// [`Mp3Encoder::finish`].
    pub fn next_packet(&mut self) -> Result<Vec<u8>> {
        Ok(self.inner.next_packet()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The call shape edith uses, end to end: config in, frames out, and our
    /// own decoder reads them back.
    #[test]
    fn encodes_and_decodes_through_the_incumbent_surface() {
        let rate = 44100;
        let pcm: Vec<i16> = (0..rate * 2 / 4)
            .map(|i| ((i as f32 * 0.05).sin() * 8000.0) as i16)
            .collect();
        let mut encoder = Mp3Encoder::new(Mp3EncoderConfig {
            bitrate_kbps: 192,
            vbr_quality: None,
        });
        encoder.push_pcm_s16(&pcm, 2, rate as u32).unwrap();
        encoder.finish();
        let mut bytes = Vec::new();
        while let Ok(frame) = encoder.next_packet() {
            bytes.extend_from_slice(&frame);
        }
        assert!(bytes.len() > 1000, "wrote {} bytes", bytes.len());

        let mut decoder = Mp3Decoder::new();
        decoder.push(&bytes);
        decoder.flush();
        let mut frames = 0;
        let mut samples = 0;
        while let Ok(frame) = decoder.next_frame() {
            assert_eq!(frame.sample_rate, rate as u32);
            assert_eq!(frame.channels, 2);
            frames += 1;
            samples += frame.samples.len();
        }
        assert!(frames > 5, "decoded {frames} frames");
        assert!(samples >= pcm.len() / 2, "decoded {samples} samples");
        assert!(matches!(decoder.next_frame(), Err(Error::Eof)));
    }
}
