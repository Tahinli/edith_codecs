//! Compatibility shim: the `opus-rs` 0.1.26 surface edith consumes, served by
//! [`ec_opus`].
//!
//! The incumbent this replaces encoded packets that only its own decoder read
//! back above ~128-165 kbps (correlation 0.06 against libopus at 256 kbps on
//! the same signal that round-tripped internally at 0.999), and mono was
//! broken at every rate. This shim serves the same three-call surface —
//! `OpusEncoder::new(rate, channels, Application)`, the public `bitrate_bps`
//! and `complexity` fields, `.encode(&pcm, frame_size, &mut out)` — from the
//! `ec-opus` CELT encoder, which is range-exact against the family's
//! RFC-vector-verified decoder and libopus-verified at every rate up to
//! 510 kbps, mono and stereo alike. The measured envelope that capped the
//! incumbent (`OPUS_MAX_KBPS = 128`) is obsolete behind this crate.
//!
//! `complexity` is accepted for drop-in compatibility and ignored: this
//! encoder has one (fast) analysis path, not a ladder.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::fmt;

/// What the encoded stream is optimised for. This CELT-only encoder treats
/// every application as [`Application::Audio`]; the variants exist for call
/// compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Application {
    /// Speech-optimised (SILK in libopus; served by CELT here).
    Voip,
    /// Music and general audio.
    Audio,
    /// Lowest-latency mode.
    RestrictedLowdelay,
}

/// The error type of this shim: a message, as the incumbent's was.
#[derive(Debug, Clone)]
pub struct OpusError(String);

impl fmt::Display for OpusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OpusError {}

/// An Opus encoder with the incumbent's field-configured surface.
#[derive(Debug)]
pub struct OpusEncoder {
    /// Target bitrate in bits per second, applied at the next
    /// [`OpusEncoder::encode`] call.
    pub bitrate_bps: i32,
    /// Accepted for compatibility; this encoder has a single analysis path.
    pub complexity: i32,
    inner: ec_opus::Encoder,
    channels: usize,
}

impl OpusEncoder {
    /// An encoder at `sample_rate` (48000 only — the rate the product feeds)
    /// for `channels` channels (1 or 2).
    pub fn new(
        sample_rate: i32,
        channels: i32,
        _application: Application,
    ) -> Result<OpusEncoder, OpusError> {
        if sample_rate < 0 || channels < 0 {
            return Err(OpusError(format!(
                "invalid encoder arguments: {sample_rate} Hz, {channels} channels"
            )));
        }
        let inner = ec_opus::Encoder::new(sample_rate as u32, channels as usize)
            .map_err(|e| OpusError(e.to_string()))?;
        Ok(OpusEncoder {
            bitrate_bps: 64_000 * channels,
            complexity: 10,
            inner,
            channels: channels as usize,
        })
    }

    /// Encodes `frame_size` samples per channel of interleaved `f32` into
    /// `out`, returning the packet length in bytes.
    pub fn encode(
        &mut self,
        pcm: &[f32],
        frame_size: usize,
        out: &mut [u8],
    ) -> Result<usize, OpusError> {
        if pcm.len() < frame_size * self.channels {
            return Err(OpusError(format!(
                "{} samples is short of {} channels x {frame_size}",
                pcm.len(),
                self.channels
            )));
        }
        self.inner.set_bitrate(self.bitrate_bps.max(0) as u32);
        self.inner
            .encode_float(pcm, frame_size, out)
            .map_err(|e| OpusError(e.to_string()))
    }

    /// Drops all inter-frame state; the next packet starts a fresh stream.
    pub fn reset(&mut self) {
        self.inner.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact call shape edith's export path uses (export.rs:1863-1896).
    #[test]
    fn edith_call_shape_compiles_and_encodes() {
        let mut encoder = OpusEncoder::new(48000, 2, Application::Audio)
            .map_err(|e| format!("the Opus encoder refused 48 kHz stereo: {e}"))
            .unwrap();
        encoder.bitrate_bps = 256_000;
        encoder.complexity = 10;
        let frame = vec![0.1f32; 960 * 2];
        let mut out = vec![0u8; 1500];
        let len = encoder
            .encode(&frame, 960, &mut out)
            .map_err(|e| format!("Opus encode failed: {e}"))
            .unwrap();
        assert!(len > 2 && len <= 1276);
        // Mono — broken at every rate in the incumbent — encodes too.
        let mut mono = OpusEncoder::new(48000, 1, Application::Audio).unwrap();
        mono.bitrate_bps = 96_000;
        let frame = vec![0.1f32; 960];
        let len = mono.encode(&frame, 960, &mut out).unwrap();
        assert!(len > 2);
    }
}
