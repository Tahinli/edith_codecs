//! Drop-in replacement for the `opus-rs` 0.1.26 surface the replica consumes,
//! implemented over [`ec_opus`].
//!
//! It carries the incumbent's package name and version so the swap is a
//! `[patch.crates-io]` entry and nothing else. The scope is what
//! `engine/src/export.rs` calls (`export.rs:1863-1900, 4379`):
//!
//! ```no_run
//! let mut encoder = opus_rs::OpusEncoder::new(48_000, 2, opus_rs::Application::Audio).unwrap();
//! encoder.bitrate_bps = 256_000;
//! encoder.complexity = 10;
//! let mut out = vec![0u8; 1500];
//! let len = encoder.encode(&[0.0f32; 960 * 2], 960, &mut out).unwrap();
//! ```
//!
//! Three of the incumbent's behaviours are deliberately *not* reproduced,
//! because they are the defects this replacement exists to remove. Each is a
//! workaround the replica currently applies by hand and can now delete:
//!
//! * **No 165 kbps ceiling.** The incumbent's output above roughly 165 kbps
//!   decoded to noise in any conformant decoder — 0.06 correlation against the
//!   source at 256 kbps while its own decoder round-tripped it happily, which
//!   is why the replica pins `OPUS_MAX_KBPS` and asserts the failure is still
//!   there. Here every rate from 16 to 510 kbps is decoded by libopus at a
//!   correlation above 0.99 (`crates/ec-opus/tests/conformance.rs`,
//!   `encoder_rate_quality_matrix`).
//! * **Mono works.** The incumbent was broken at every rate in mono, so the
//!   replica encodes stereo whatever it was handed.
//! * **No warm-up frame needed.** The incumbent's first frame came out ramped
//!   and out of phase (0.09 correlation on a 440 Hz fixture), so the replica
//!   feeds the first block twice and throws one packet away. Here the first
//!   frame is a frame like any other; feeding it twice is still harmless, so
//!   the workaround can go at the caller's convenience rather than in lockstep
//!   with this swap.
//!
//! One behaviour is genuinely narrower and is stated rather than hidden: this
//! encoder is **CELT-only**, so `Application::Voip` selects a narrower
//! bandwidth at a given rate but not the SILK layer the name implies. Speech
//! below about 32 kbps is where that costs the most.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use ec_opus::packet::Bandwidth as EcBandwidth;

/// What the caller is encoding. The discriminants are the `OPUS_APPLICATION_*`
/// values of the C API, as the incumbent's were.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Application {
    /// Speech.
    Voip = 2048,
    /// Music, and the replica's only setting.
    Audio = 2049,
    /// Lowest delay.
    RestrictedLowDelay = 2051,
}

/// Coded audio bandwidth, with the C API's discriminants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bandwidth {
    /// Chosen from the bitrate.
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

/// An Opus encoder.
///
/// `bitrate_bps` and `complexity` are public fields because the incumbent's
/// were, and the replica sets them after construction; both are read at the
/// next [`OpusEncoder::encode`].
pub struct OpusEncoder {
    /// Target bitrate in bits per second.
    pub bitrate_bps: i32,
    /// Complexity 0..10. Kept for source compatibility: this encoder runs its
    /// full analysis at every setting (it is two orders of magnitude faster
    /// than realtime), so the value selects nothing.
    pub complexity: i32,
    /// Coded bandwidth; [`Bandwidth::Auto`] follows the bitrate.
    pub bandwidth: Bandwidth,
    /// Constrained VBR, the incumbent's default.
    pub vbr: bool,
    inner: ec_opus::Encoder,
    applied_bitrate: i32,
    applied_bandwidth: Bandwidth,
    applied_vbr: bool,
}

impl OpusEncoder {
    /// An encoder for `channels` channels at `sampling_rate` Hz, which must be
    /// 8000, 12000, 16000, 24000 or 48000.
    pub fn new(
        sampling_rate: i32,
        channels: usize,
        application: Application,
    ) -> Result<Self, &'static str> {
        if ![8000, 12000, 16000, 24000, 48000].contains(&sampling_rate) {
            return Err("Invalid sampling rate");
        }
        if ![1, 2].contains(&channels) {
            return Err("Invalid number of channels");
        }
        let app = match application {
            Application::Voip => ec_opus::Application::Voip,
            Application::Audio => ec_opus::Application::Audio,
            Application::RestrictedLowDelay => ec_opus::Application::LowDelay,
        };
        let inner = ec_opus::Encoder::new(sampling_rate as u32, channels, app)
            .map_err(|_| "Failed to create encoder")?;
        let bitrate_bps = inner.bitrate() as i32;
        Ok(OpusEncoder {
            bitrate_bps,
            complexity: 10,
            bandwidth: Bandwidth::Auto,
            vbr: true,
            inner,
            applied_bitrate: bitrate_bps,
            applied_bandwidth: Bandwidth::Auto,
            applied_vbr: true,
        })
    }

    /// Samples of encoder delay: the decoded stream lags the input by this
    /// much, and an Ogg-Opus pre-skip of it cancels the lag exactly.
    pub fn look_ahead(&self) -> usize {
        self.inner.look_ahead()
    }

    /// The range coder state after the last packet — the RFC 6716 Section 6
    /// conformance hook, which a decoder reproduces exactly.
    pub fn final_range(&self) -> u32 {
        self.inner.final_range()
    }

    /// Encodes one frame of interleaved `f32`, returning the bytes written.
    pub fn encode(
        &mut self,
        input: &[f32],
        frame_size: usize,
        output: &mut [u8],
    ) -> Result<usize, &'static str> {
        self.sync_settings();
        self.inner
            .encode_float(input, frame_size, output)
            .map_err(|_| "Opus encode failed")
    }

    /// [`OpusEncoder::encode`] from 16-bit samples.
    pub fn encode_i16(
        &mut self,
        input: &[i16],
        frame_size: usize,
        output: &mut [u8],
    ) -> Result<usize, &'static str> {
        self.sync_settings();
        self.inner
            .encode(input, frame_size, output)
            .map_err(|_| "Opus encode failed")
    }

    fn sync_settings(&mut self) {
        if self.bitrate_bps != self.applied_bitrate {
            self.inner.set_bitrate(self.bitrate_bps.max(0) as u32);
            self.applied_bitrate = self.bitrate_bps;
        }
        if self.bandwidth != self.applied_bandwidth {
            self.inner.set_bandwidth(match self.bandwidth {
                Bandwidth::Auto => None,
                Bandwidth::Narrowband => Some(EcBandwidth::Narrow),
                Bandwidth::Mediumband | Bandwidth::Wideband => Some(EcBandwidth::Wide),
                Bandwidth::SuperWideband => Some(EcBandwidth::SuperWide),
                Bandwidth::Fullband => Some(EcBandwidth::Full),
            });
            self.applied_bandwidth = self.bandwidth;
        }
        if self.vbr != self.applied_vbr {
            self.inner.set_vbr(self.vbr);
            self.applied_vbr = self.vbr;
        }
    }
}

/// An Opus decoder, mono or stereo.
pub struct OpusDecoder {
    inner: ec_opus::Decoder,
}

impl OpusDecoder {
    /// A decoder for `channels` channels at `sampling_rate` Hz.
    pub fn new(sampling_rate: i32, channels: usize) -> Result<Self, &'static str> {
        Ok(OpusDecoder {
            inner: ec_opus::Decoder::new(sampling_rate as u32, channels)
                .map_err(|_| "Invalid decoder parameters")?,
        })
    }

    /// Decodes one packet into interleaved `f32`, returning samples per
    /// channel. `frame_size` bounds the output, as the incumbent's did.
    pub fn decode(
        &mut self,
        input: &[u8],
        frame_size: usize,
        output: &mut [f32],
    ) -> Result<usize, &'static str> {
        let _ = frame_size;
        self.inner
            .decode_float(input, output)
            .map_err(|_| "Opus decode failed")
    }

    /// The range coder state after the last packet.
    pub fn final_range(&self) -> u32 {
        self.inner.final_range()
    }
}

/// A multichannel encoder, for the 5.1 and 7.1 exports the incumbent could not
/// do at all — the replica's surround Opus path today is stereo only.
pub struct MultistreamEncoder {
    /// Target bitrate in bits per second, across all streams.
    pub bitrate_bps: i32,
    inner: ec_opus::MultistreamEncoder,
    applied_bitrate: i32,
}

impl MultistreamEncoder {
    /// An encoder for the RFC 7845 family 1 layout of `channels` channels (1
    /// to 8). Input is in the mapping's channel order, which for 5.1 is Vorbis
    /// order: left, centre, right, back left, back right, LFE.
    pub fn surround(
        sampling_rate: i32,
        channels: usize,
        application: Application,
    ) -> Result<Self, &'static str> {
        let app = match application {
            Application::Voip => ec_opus::Application::Voip,
            Application::Audio => ec_opus::Application::Audio,
            Application::RestrictedLowDelay => ec_opus::Application::LowDelay,
        };
        let inner = ec_opus::MultistreamEncoder::surround(sampling_rate as u32, channels, app)
            .map_err(|_| "Unsupported channel count")?;
        Ok(MultistreamEncoder {
            bitrate_bps: 96_000 * channels as i32 / 2,
            inner,
            applied_bitrate: 0,
        })
    }

    /// The channel mapping table for the `OpusHead` this encoder needs.
    pub fn mapping(&self) -> &[u8] {
        self.inner.mapping()
    }

    /// Elementary streams, and how many of them are coupled.
    pub fn stream_counts(&self) -> (usize, usize) {
        (self.inner.streams(), self.inner.coupled())
    }

    /// Encodes one frame of interleaved `f32`.
    pub fn encode(
        &mut self,
        input: &[f32],
        frame_size: usize,
        output: &mut [u8],
    ) -> Result<usize, &'static str> {
        if self.bitrate_bps != self.applied_bitrate {
            self.inner.set_bitrate(self.bitrate_bps.max(0) as u32);
            self.applied_bitrate = self.bitrate_bps;
        }
        self.inner
            .encode_float(input, frame_size, output)
            .map_err(|_| "Opus encode failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(n: usize, channels: usize) -> Vec<f32> {
        (0..n * channels)
            .map(|i| {
                let t = (i / channels) as f32;
                0.5 * (t * 0.07).sin() + 0.25 * (t * 0.23).sin()
            })
            .collect()
    }

    /// The replica's call sequence, verbatim from `export.rs:1863-1900`,
    /// including the settings it writes straight into the public fields.
    #[test]
    fn the_replica_call_sequence_works() {
        const FRAME: usize = 960;
        let mut encoder = OpusEncoder::new(48_000, 2, Application::Audio).unwrap();
        encoder.bitrate_bps = 256_000;
        encoder.complexity = 10;
        let pcm = tone(FRAME * 8, 2);
        let mut out = vec![0u8; 1500];
        let mut dec = OpusDecoder::new(48_000, 2).unwrap();
        let mut back = vec![0.0f32; FRAME * 2];
        let mut total = 0;
        for block in pcm.chunks_exact(FRAME * 2) {
            let len = encoder.encode(block, FRAME, &mut out).unwrap();
            assert!(len > 1 && len <= 1276, "packet of {len} bytes");
            total += len;
            let n = dec.decode(&out[..len], FRAME, &mut back).unwrap();
            assert_eq!(n, FRAME);
            assert_eq!(dec.final_range(), encoder.final_range());
        }
        // 256 kbps for 8 frames of 20 ms, within the VBR tolerance. The
        // incumbent's 165 kbps ceiling is what this number exists to refute.
        let want = 256_000 / 8 / 50 * 8;
        assert!(
            (total as f64 - want as f64).abs() < 0.2 * want as f64,
            "{total} bytes vs {want} at 256 kbps"
        );
    }

    /// Mono, which the incumbent got wrong at every rate.
    #[test]
    fn mono_is_not_broken() {
        const FRAME: usize = 960;
        let mut encoder = OpusEncoder::new(48_000, 1, Application::Audio).unwrap();
        encoder.bitrate_bps = 96_000;
        let pcm = tone(FRAME * 6, 1);
        let mut out = vec![0u8; 1500];
        let mut dec = OpusDecoder::new(48_000, 1).unwrap();
        let mut back = vec![0.0f32; FRAME];
        let mut got = Vec::new();
        for block in pcm.chunks_exact(FRAME) {
            let len = encoder.encode(block, FRAME, &mut out).unwrap();
            let n = dec.decode(&out[..len], FRAME, &mut back).unwrap();
            got.extend_from_slice(&back[..n]);
        }
        // Past the encoder's delay and the first frames, the decode follows the
        // input.
        let skip = 2 * FRAME;
        let (mut num, mut da, mut db) = (0.0f64, 0.0f64, 0.0f64);
        for i in skip..got.len() {
            let a = got[i] as f64;
            let b = pcm[i - encoder.look_ahead()] as f64;
            num += a * b;
            da += a * a;
            db += b * b;
        }
        let corr = num / (da.sqrt() * db.sqrt());
        assert!(corr > 0.95, "mono correlation {corr}");
    }

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
            let len = e.encode(block, FRAME, &mut out).unwrap();
            assert!(len > 8);
        }
    }

    #[test]
    fn bad_parameters_are_errors() {
        assert!(OpusEncoder::new(44_100, 2, Application::Audio).is_err());
        assert!(OpusEncoder::new(48_000, 3, Application::Audio).is_err());
        assert!(OpusDecoder::new(44_100, 2).is_err());
    }
}
