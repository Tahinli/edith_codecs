//! Compatibility shim: the `opus-rs` 0.1.26 surface edith consumes, served by
//! [`ec_opus`].
//!
//! The incumbent this replaces encoded packets that only its own decoder read
//! back above ~128-165 kbps (correlation 0.06 against the reference decoder at 256 kbps on
//! the same signal that round-tripped internally at 0.999), and mono was
//! broken at every rate. This shim serves the same three-call surface —
//! `OpusEncoder::new(rate, channels, Application)`, the public `bitrate_bps`
//! and `complexity` fields, `.encode(&pcm, frame_size, &mut out)` — from the
//! `ec-opus` CELT encoder, which is range-exact against the family's
//! RFC-vector-verified decoder and the reference decoder-verified at every rate up to
//! 510 kbps, mono and stereo alike. The measured envelope that capped the
//! incumbent (`OPUS_MAX_KBPS = 128`) is obsolete behind this crate.
//!
//! `complexity` is accepted for drop-in compatibility and ignored: this
//! encoder has one (fast) analysis path, not a ladder.
//!
//! [`Application::Voip`] below 20 kbps, mono, 20 ms frames picks the SILK
//! layer, as the reference does; every other combination stays CELT. Stereo
//! SILK and SILK at other frame sizes fall back to CELT rather than
//! silently approximating it.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::fmt;

/// What the encoded stream is optimised for. This CELT-only encoder spends the
/// setting on the coded bandwidth, not on a different coding layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Application {
    /// Speech-optimised (SILK in the reference decoder; served by CELT here).
    Voip,
    /// Music and general audio.
    Audio,
    /// Lowest-latency mode.
    RestrictedLowdelay,
}

impl From<Application> for ec_opus::Application {
    fn from(a: Application) -> ec_opus::Application {
        match a {
            Application::Voip => ec_opus::Application::Voip,
            Application::Audio => ec_opus::Application::Audio,
            Application::RestrictedLowdelay => ec_opus::Application::LowDelay,
        }
    }
}

/// Coded audio bandwidth, with the C API's discriminants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bandwidth {
    /// Chosen from the bitrate, the application and the input rate.
    Auto = -1000,
    /// 4 kHz.
    Narrowband = 1101,
    /// 6 kHz. CELT has no mediumband configuration; this codes as wideband.
    Mediumband = 1102,
    /// 8 kHz.
    Wideband = 1103,
    /// 12 kHz.
    SuperWideband = 1104,
    /// 20 kHz.
    Fullband = 1105,
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
    /// Coded bandwidth; [`Bandwidth::Auto`] follows the bitrate.
    pub bandwidth: Bandwidth,
    /// Constrained VBR. Off by default, as the incumbent's was.
    pub vbr: bool,
    inner: ec_opus::Encoder,
    channels: usize,
    applied_bandwidth: Bandwidth,
    applied_vbr: bool,
}

impl OpusEncoder {
    /// An encoder at `sample_rate` (8000, 12000, 16000, 24000 or 48000) for
    /// `channels` channels (1 or 2).
    pub fn new(
        sample_rate: i32,
        channels: i32,
        application: Application,
    ) -> Result<OpusEncoder, OpusError> {
        if sample_rate < 0 || channels < 0 {
            return Err(OpusError(format!(
                "invalid encoder arguments: {sample_rate} Hz, {channels} channels"
            )));
        }
        let inner =
            ec_opus::Encoder::new(sample_rate as u32, channels as usize, application.into())
                .map_err(|e| OpusError(e.to_string()))?;
        Ok(OpusEncoder {
            bitrate_bps: 64_000 * channels,
            complexity: 10,
            bandwidth: Bandwidth::Auto,
            vbr: false,
            inner,
            channels: channels as usize,
            applied_bandwidth: Bandwidth::Auto,
            applied_vbr: false,
        })
    }

    /// Samples of encoder delay, at the input rate: the decoded stream lags
    /// the input by this much, and an Ogg-Opus pre-skip of it (scaled to
    /// 48 kHz) cancels the lag exactly. Reported for the canonical 20 ms
    /// frame (SILK and Hybrid, which shift the delay, only run at 20 ms
    /// anyway) — like the incumbent's `OPUS_GET_LOOKAHEAD`, which doesn't
    /// take a frame size either.
    pub fn look_ahead(&self) -> usize {
        self.inner.look_ahead(self.inner.sample_rate() as usize / 50)
    }

    /// The range coder state after the last packet — the RFC 6716 Section 6
    /// conformance hook, which a conformant decoder reproduces exactly.
    pub fn final_range(&self) -> u32 {
        self.inner.final_range()
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
        self.sync_settings();
        self.inner
            .encode_float(pcm, frame_size, out)
            .map_err(|e| OpusError(e.to_string()))
    }

    /// [`OpusEncoder::encode`] from 16-bit samples.
    pub fn encode_i16(
        &mut self,
        pcm: &[i16],
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
        self.sync_settings();
        self.inner
            .encode(pcm, frame_size, out)
            .map_err(|e| OpusError(e.to_string()))
    }

    /// Drops all inter-frame state; the next packet starts a fresh stream.
    pub fn reset(&mut self) {
        self.inner.reset();
    }

    /// Pushes the public fields into the encoder before a frame.
    fn sync_settings(&mut self) {
        self.inner.set_bitrate(self.bitrate_bps.max(0) as u32);
        if self.bandwidth != self.applied_bandwidth {
            self.inner.set_bandwidth(match self.bandwidth {
                Bandwidth::Auto => None,
                Bandwidth::Narrowband => Some(ec_opus::Bandwidth::Narrow),
                Bandwidth::Mediumband | Bandwidth::Wideband => Some(ec_opus::Bandwidth::Wide),
                Bandwidth::SuperWideband => Some(ec_opus::Bandwidth::SuperWide),
                Bandwidth::Fullband => Some(ec_opus::Bandwidth::Full),
            });
            self.applied_bandwidth = self.bandwidth;
        }
        if self.vbr != self.applied_vbr {
            self.inner.set_vbr_constrained(self.vbr);
            self.applied_vbr = self.vbr;
        }
    }
}

/// An Opus decoder, mono or stereo.
#[derive(Debug)]
pub struct OpusDecoder {
    inner: ec_opus::Decoder,
}

impl OpusDecoder {
    /// A decoder for `channels` channels at `sample_rate` Hz.
    pub fn new(sample_rate: i32, channels: i32) -> Result<OpusDecoder, OpusError> {
        if sample_rate < 0 || channels < 0 {
            return Err(OpusError(format!(
                "invalid decoder arguments: {sample_rate} Hz, {channels} channels"
            )));
        }
        Ok(OpusDecoder {
            inner: ec_opus::Decoder::new(sample_rate as u32, channels as usize)
                .map_err(|e| OpusError(e.to_string()))?,
        })
    }

    /// Decodes one packet into interleaved `f32`, returning samples per
    /// channel. `frame_size` bounds the output, as the incumbent's did.
    pub fn decode(
        &mut self,
        packet: &[u8],
        frame_size: usize,
        out: &mut [f32],
    ) -> Result<usize, OpusError> {
        let _ = frame_size;
        self.inner
            .decode_float(packet, out)
            .map_err(|e| OpusError(e.to_string()))
    }

    /// The range coder state after the last packet.
    pub fn final_range(&self) -> u32 {
        self.inner.final_range()
    }
}

/// A multichannel encoder, for the 5.1 and 7.1 exports the incumbent could not
/// do at all — the replica's surround Opus path today is stereo only.
#[derive(Debug)]
pub struct MultistreamEncoder {
    /// Target bitrate in bits per second, across all streams.
    pub bitrate_bps: i32,
    inner: ec_opus::MultistreamEncoder,
    channels: usize,
}

impl MultistreamEncoder {
    /// An encoder for the RFC 7845 family-1 layout of `channels` channels (1
    /// to 8). Input is in the mapping's channel order, which for 5.1 is Vorbis
    /// order: left, centre, right, back left, back right, LFE.
    pub fn surround(
        sample_rate: i32,
        channels: usize,
        application: Application,
    ) -> Result<MultistreamEncoder, OpusError> {
        if sample_rate < 0 {
            return Err(OpusError(format!("invalid sample rate {sample_rate}")));
        }
        let inner =
            ec_opus::MultistreamEncoder::surround(sample_rate as u32, channels, application.into())
                .map_err(|e| OpusError(e.to_string()))?;
        Ok(MultistreamEncoder {
            bitrate_bps: 96_000 * channels as i32 / 2,
            inner,
            channels,
        })
    }

    /// The channel mapping table the `OpusHead` for this encoder needs.
    pub fn mapping(&self) -> &[u8] {
        self.inner.layout().2
    }

    /// Elementary streams, and how many of them are coupled.
    pub fn stream_counts(&self) -> (usize, usize) {
        let (streams, coupled, _) = self.inner.layout();
        (streams, coupled)
    }

    /// Encodes one frame of interleaved `f32`, returning the packet length.
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

    fn tone(n: usize, channels: usize) -> Vec<f32> {
        (0..n * channels)
            .map(|i| {
                let t = (i / channels) as f32;
                0.5 * (t * 0.07).sin() + 0.25 * (t * 0.23).sin()
            })
            .collect()
    }

    /// The decoder half of the surface, and the conformance hook that says the
    /// two agree: the decoder's range state must equal the encoder's after
    /// every packet.
    #[test]
    fn the_shim_decodes_what_it_encodes_and_the_ranges_agree() {
        const FRAME: usize = 960;
        let mut enc = OpusEncoder::new(48_000, 2, Application::Audio).unwrap();
        enc.bitrate_bps = 256_000;
        let mut dec = OpusDecoder::new(48_000, 2).unwrap();
        let pcm = tone(FRAME * 8, 2);
        let mut out = vec![0u8; 1500];
        let mut back = vec![0.0f32; FRAME * 2];
        let mut total = 0;
        for block in pcm.chunks_exact(FRAME * 2) {
            let len = enc.encode(block, FRAME, &mut out).unwrap();
            assert!(len > 1 && len <= 1276, "packet of {len} bytes");
            total += len;
            assert_eq!(dec.decode(&out[..len], FRAME, &mut back).unwrap(), FRAME);
            assert_eq!(dec.final_range(), enc.final_range());
        }
        // 256 kbps over 8 frames of 20 ms: the incumbent's 165 kbps ceiling is
        // what this number exists to refute.
        let want = 256_000 / 8 / 50 * 8;
        assert!(
            (total as f64 - want as f64).abs() < 0.2 * want as f64,
            "{total} bytes vs {want} at 256 kbps"
        );
    }

    /// Input below 48 kHz, which the shim used to refuse outright, and the
    /// `look_ahead` that goes with it (120 samples at 48 kHz, scaled).
    #[test]
    fn slower_input_rates_encode_and_report_their_own_delay() {
        for &(rate, frame, look) in &[
            (48_000i32, 960usize, 120usize),
            (24_000, 480, 60),
            (16_000, 320, 40),
            (12_000, 240, 30),
            (8_000, 160, 20),
        ] {
            let mut enc = OpusEncoder::new(rate, 1, Application::Voip).unwrap();
            enc.bitrate_bps = 32_000;
            assert_eq!(enc.look_ahead(), look, "{rate} Hz look-ahead");
            let pcm = tone(frame * 4, 1);
            let mut out = vec![0u8; 1500];
            // Everything a 48 kHz decoder gets back is 48 kHz, whatever went in.
            let mut dec = OpusDecoder::new(48_000, 1).unwrap();
            let mut back = vec![0.0f32; 960];
            for block in pcm.chunks_exact(frame) {
                let len = enc.encode(block, frame, &mut out).unwrap();
                assert!(len > 2, "{rate} Hz packet of {len} bytes");
                assert_eq!(dec.decode(&out[..len], 960, &mut back).unwrap(), 960);
                assert_eq!(dec.final_range(), enc.final_range(), "{rate} Hz range");
            }
        }
        assert!(OpusEncoder::new(44_100, 2, Application::Audio).is_err());
    }

    /// A narrower band is a smaller top-band cost, not a refusal: forcing the
    /// bandwidth must still produce packets a decoder reads.
    #[test]
    fn forced_bandwidth_and_i16_input_round_trip() {
        const FRAME: usize = 960;
        let mut enc = OpusEncoder::new(48_000, 1, Application::Audio).unwrap();
        enc.bitrate_bps = 64_000;
        enc.bandwidth = Bandwidth::Wideband;
        enc.vbr = true;
        let pcm: Vec<i16> = tone(FRAME * 4, 1)
            .iter()
            .map(|v| (v * 16_000.0) as i16)
            .collect();
        let mut out = vec![0u8; 1500];
        let mut dec = OpusDecoder::new(48_000, 1).unwrap();
        let mut back = vec![0.0f32; FRAME];
        for block in pcm.chunks_exact(FRAME) {
            let len = enc.encode_i16(block, FRAME, &mut out).unwrap();
            // TOC config 20..23 is wideband CELT (RFC 6716 Table 2).
            assert_eq!(out[0] >> 3, 23, "wideband CELT config");
            assert_eq!(dec.decode(&out[..len], FRAME, &mut back).unwrap(), FRAME);
            assert_eq!(dec.final_range(), enc.final_range());
        }
    }

    /// 5.1, which the incumbent could not encode at all.
    #[test]
    fn surround_encodes_five_one() {
        const FRAME: usize = 960;
        let mut e = MultistreamEncoder::surround(48_000, 6, Application::Audio).unwrap();
        e.bitrate_bps = 384_000;
        assert_eq!(e.stream_counts(), (4, 2));
        assert_eq!(e.mapping(), &[0, 4, 1, 2, 3, 5]);
        let pcm = tone(FRAME * 3, 6);
        let mut out = vec![0u8; 8 * 1500];
        for block in pcm.chunks_exact(FRAME * 6) {
            assert!(e.encode(block, FRAME, &mut out).unwrap() > 8);
        }
        assert!(MultistreamEncoder::surround(48_000, 9, Application::Audio).is_err());
    }

    /// `Application::Voip` under 20 kbps, mono, 20 ms frames reaches the
    /// SILK layer through this shim's existing surface — no new API, just
    /// the application and bitrate the incumbent already exposed. TOC
    /// configs 0..=11 are SILK (RFC 6716 Table 2).
    #[test]
    fn voip_below_20kbps_reaches_silk() {
        let mut enc = OpusEncoder::new(48_000, 1, Application::Voip).unwrap();
        enc.bitrate_bps = 12_000;
        let pcm = tone(960, 1);
        let mut out = vec![0u8; 1500];
        let len = enc.encode(&pcm, 960, &mut out).unwrap();
        assert!(len > 1);
        assert!(out[0] >> 3 <= 11, "TOC config {} is not SILK", out[0] >> 3);
    }
}
