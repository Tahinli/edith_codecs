//! The Opus encoder: CELT-only, every frame size, mono and stereo.
//!
//! What this layer owns is everything outside the MDCT: the TOC byte and the
//! packet framing (RFC 6716 Section 3), the coded bandwidth, and how many bytes
//! each frame may spend. [`crate::celt_enc::CeltEncoder`] owns the rest.
//!
//! Scope: **CELT only.** There is no SILK layer here, so a caller asking for a
//! speech-optimised mode gets CELT at that bitrate rather than a silent
//! downgrade to something else; the hybrid and SILK modes are a later slice.
//! CELT covers 2.5 to 20 ms frames at every bitrate the format allows, which is
//! what a music encoder needs.
//!
//! Delay: one MDCT overlap, 120 samples at 48 kHz ([`Encoder::look_ahead`]).
//! An Ogg-Opus pre-skip of that value cancels it exactly.

use ec_core::{Error, Result};

use crate::celt_enc::CeltEncoder;
use crate::packet::Bandwidth;
use crate::range_enc::RangeEncoder;

/// What the caller is encoding, which only selects defaults here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Application {
    /// Speech: biased towards a narrower band at a given rate.
    Voip,
    /// Music, the default.
    Audio,
    /// Lowest delay: 2.5 ms frames, no lookahead beyond the overlap.
    LowDelay,
}

/// Largest Opus frame, [R2] of Section 3.4.
const MAX_FRAME_BYTES: usize = 1275;

/// One Opus stream encoder: mono or coupled stereo.
#[derive(Clone, Debug)]
pub struct Encoder {
    sample_rate: u32,
    channels: usize,
    upsample: usize,
    celt: CeltEncoder,
    bitrate: u32,
    vbr: bool,
    application: Application,
    bandwidth: Option<Bandwidth>,
    /// Constrained-VBR credit, in bytes.
    reservoir: i64,
    avg_db: f32,
    started: bool,
    final_range: u32,
}

impl Encoder {
    /// An encoder for `channels` channels at `sample_rate`, which must be one
    /// of 8000, 12000, 16000, 24000 or 48000 Hz.
    pub fn new(sample_rate: u32, channels: usize, application: Application) -> Result<Encoder> {
        let upsample = match sample_rate {
            48000 => 1,
            24000 => 2,
            16000 => 3,
            12000 => 4,
            8000 => 6,
            _ => {
                return Err(Error::unsupported(
                    format!("opus input rate {sample_rate}"),
                    "Opus encodes from 8, 12, 16, 24 or 48 kHz",
                ));
            }
        };
        if !(1..=2).contains(&channels) {
            return Err(Error::unsupported(
                format!("{channels}-channel Opus stream"),
                "one stream carries at most two channels; use MultistreamEncoder",
            ));
        }
        Ok(Encoder {
            sample_rate,
            channels,
            upsample,
            celt: CeltEncoder::new(channels, upsample),
            // The rate libopus defaults to for this width.
            bitrate: (64000 * channels as u32).min(96000),
            vbr: true,
            application,
            bandwidth: None,
            reservoir: 0,
            avg_db: -30.0,
            started: false,
            final_range: 0,
        })
    }

    /// Target bitrate in bits per second, 500 to 512000.
    pub fn set_bitrate(&mut self, bits_per_second: u32) {
        self.bitrate = bits_per_second.clamp(500, 512_000);
    }

    /// The target bitrate.
    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }

    /// Constrained VBR (the default) or CBR.
    pub fn set_vbr(&mut self, vbr: bool) {
        self.vbr = vbr;
    }

    /// Forces the coded bandwidth; [`None`] picks it from the bitrate.
    pub fn set_bandwidth(&mut self, bandwidth: Option<Bandwidth>) {
        self.bandwidth = bandwidth;
    }

    /// Output channels.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Input sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Encoder delay in input samples: the decoder's output lags the input by
    /// this much, and an Ogg-Opus `pre-skip` of `look_ahead * 48000/rate`
    /// removes it.
    pub fn look_ahead(&self) -> usize {
        120 / self.upsample
    }

    /// The range coder state after the last packet, which the decoder
    /// reproduces exactly — the conformance hook of RFC 6716 Section 6.
    pub fn final_range(&self) -> u32 {
        self.final_range
    }

    /// Drops all inter-frame state.
    pub fn reset(&mut self) {
        self.celt.reset();
        self.reservoir = 0;
        self.started = false;
        self.final_range = 0;
    }

    /// Encodes one frame of interleaved `f32` into `out`, returning the bytes
    /// written. `frame_size` is samples per channel and must be 2.5, 5, 10 or
    /// 20 ms of the input rate.
    pub fn encode_float(
        &mut self,
        pcm: &[f32],
        frame_size: usize,
        out: &mut [u8],
    ) -> Result<usize> {
        self.encode_impl(pcm, frame_size, out, false)
    }

    /// [`Encoder::encode_float`] from 16-bit samples.
    pub fn encode(&mut self, pcm: &[i16], frame_size: usize, out: &mut [u8]) -> Result<usize> {
        let float: Vec<f32> = pcm.iter().map(|&v| v as f32 * (1.0 / 32768.0)).collect();
        self.encode_impl(&float, frame_size, out, false)
    }

    /// Encodes one frame in the self-delimiting framing of RFC 6716 Appendix B,
    /// which is what every stream but the last of a multistream packet uses.
    pub fn encode_self_delimited(
        &mut self,
        pcm: &[f32],
        frame_size: usize,
        out: &mut [u8],
    ) -> Result<usize> {
        self.encode_impl(pcm, frame_size, out, true)
    }

    fn encode_impl(
        &mut self,
        pcm: &[f32],
        frame_size: usize,
        out: &mut [u8],
        self_delimited: bool,
    ) -> Result<usize> {
        let frame_48k = frame_size * self.upsample;
        let size_idx = match frame_48k {
            120 => 0usize,
            240 => 1,
            480 => 2,
            960 => 3,
            _ => {
                return Err(Error::unsupported(
                    format!(
                        "opus frame of {frame_size} samples at {} Hz",
                        self.sample_rate
                    ),
                    "CELT frames are 2.5, 5, 10 or 20 ms",
                ));
            }
        };
        if pcm.len() < frame_size * self.channels {
            return Err(Error::corrupt(format!(
                "opus encode: {} samples for a {frame_size}-sample {}-channel frame",
                pcm.len(),
                self.channels
            )));
        }

        let bandwidth = self.bandwidth.unwrap_or_else(|| self.auto_bandwidth());
        let bw_idx = match bandwidth {
            Bandwidth::Narrow => 0,
            // CELT has no mediumband configuration; the next one up covers it.
            Bandwidth::Medium | Bandwidth::Wide => 1,
            Bandwidth::SuperWide => 2,
            Bandwidth::Full => 3,
        };
        let toc =
            ((16 + 4 * bw_idx + size_idx) as u8) << 3 | if self.channels == 2 { 0x4 } else { 0 };

        // Header cost: TOC, plus the self-delimiting length.
        let target = self.frame_bytes(frame_48k, pcm);
        let mut body = target.saturating_sub(1);
        if self_delimited {
            body = body.saturating_sub(if body >= 252 { 2 } else { 1 });
        }
        let header = 1 + if self_delimited {
            usize::from(body >= 252) + 1
        } else {
            0
        };
        let body = body.clamp(2, MAX_FRAME_BYTES);
        if out.len() < header + body {
            return Err(Error::corrupt(format!(
                "opus encode: output buffer holds {} bytes, the frame needs {}",
                out.len(),
                header + body
            )));
        }

        let mut enc = RangeEncoder::new(body);
        self.celt
            .encode(pcm, frame_48k, bandwidth.celt_end_band(), &mut enc)?;
        self.final_range = self.celt.rng();
        let frame = enc.done();

        out[0] = toc;
        let mut pos = 1;
        if self_delimited {
            if body < 252 {
                out[pos] = body as u8;
                pos += 1;
            } else {
                out[pos] = (252 + (body & 0x3)) as u8;
                out[pos + 1] = ((body - out[pos] as usize) >> 2) as u8;
                pos += 2;
            }
        }
        out[pos..pos + body].copy_from_slice(&frame);
        Ok(pos + body)
    }

    /// Bandwidth from the bitrate: coding 20 kHz at 24 kbps spends bits on air.
    ///
    /// The thresholds are on the mono-equivalent rate — a coupled stereo stream
    /// needs about 1.6x a mono one for the same per-channel quality — and VoIP
    /// asks for one step narrower, where the top octave carries least.
    fn auto_bandwidth(&self) -> Bandwidth {
        let equiv = if self.channels == 2 {
            (self.bitrate as f32 * 0.62) as u32
        } else {
            self.bitrate
        };
        let equiv = if self.application == Application::Voip {
            (equiv as f32 * 0.7) as u32
        } else {
            equiv
        };
        // The input's own Nyquist bounds it: coding above it would code the
        // images the upsampling put there.
        let ceiling = match self.sample_rate {
            48000 => Bandwidth::Full,
            24000 => Bandwidth::SuperWide,
            16000 => Bandwidth::Wide,
            _ => Bandwidth::Narrow,
        };
        let wanted = if equiv < 9000 {
            Bandwidth::Narrow
        } else if equiv < 13000 {
            Bandwidth::Wide
        } else if equiv < 20000 {
            Bandwidth::SuperWide
        } else {
            Bandwidth::Full
        };
        wanted.min(ceiling)
    }

    /// The packet's byte budget, TOC included.
    ///
    /// CBR is the arithmetic; constrained VBR spends the frame's deviation from
    /// the running loudness, bounded by a credit that the quiet frames build —
    /// so the long-run rate stays at the target instead of drifting above it.
    fn frame_bytes(&mut self, frame_48k: usize, pcm: &[f32]) -> usize {
        let base = (self.bitrate as u64 * frame_48k as u64 / (48000 * 8)) as i64;
        let base = base.clamp(2, MAX_FRAME_BYTES as i64);
        if !self.vbr {
            return base as usize;
        }
        let n = pcm.len().max(1);
        let energy: f32 = pcm.iter().map(|v| v * v).sum::<f32>() / n as f32;
        let db = 10.0 * (energy + 1e-12).log10();
        if !self.started {
            self.avg_db = db;
            self.started = true;
        }
        let dev = ((db - self.avg_db) / 6.0).clamp(-2.0, 2.0);
        self.avg_db += 0.05 * (db - self.avg_db);
        // Loudness sets the shape, the reservoir sets the level: what the quiet
        // frames did not spend is handed back over the next few frames, so the
        // long-run rate converges on the target instead of drifting under it.
        // The clamp is what makes this *constrained* VBR — no frame may take
        // more than two and a half times its share however much credit exists.
        let mut want = (base as f32 * (1.0 + 0.22 * dev)) as i64 + self.reservoir / 8;
        if db < -70.0 {
            // Digital silence costs a header and the silence bit.
            want = want.min(base / 8);
        }
        let bytes = want.clamp(
            (base / 4).max(2),
            (5 * base / 2).min(MAX_FRAME_BYTES as i64),
        );
        self.reservoir = (self.reservoir + base - bytes).clamp(-4 * base, 20 * base);
        bytes as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Decoder;

    fn tone(n: usize, channels: usize) -> Vec<f32> {
        (0..n * channels)
            .map(|i| {
                let t = (i / channels) as f32;
                let ch = (i % channels) as f32;
                0.5 * (t * 0.07 + ch).sin() + 0.25 * (t * 0.23).sin()
            })
            .collect()
    }

    #[test]
    fn every_frame_size_round_trips_through_the_decoder() {
        for &frame in &[120usize, 240, 480, 960] {
            for &channels in &[1usize, 2] {
                let frames = 12;
                let src = tone(frame * frames, channels);
                let mut e = Encoder::new(48000, channels, Application::Audio).unwrap();
                e.set_vbr(false);
                e.set_bitrate(96000);
                let mut d = Decoder::new(48000, channels).unwrap();
                let mut out = vec![0.0f32; frame * channels];
                let mut buf = [0u8; 1500];
                for t in 0..frames {
                    let pcm = &src[t * frame * channels..(t + 1) * frame * channels];
                    let n = e.encode_float(pcm, frame, &mut buf).unwrap();
                    let got = d.decode_float(&buf[..n], &mut out).unwrap();
                    assert_eq!(got, frame, "frame {frame} ch {channels}");
                    assert_eq!(
                        d.final_range(),
                        e.final_range(),
                        "frame {frame} ch {channels} packet {t}: range coder desynced"
                    );
                }
            }
        }
    }

    #[test]
    fn cbr_is_constant_and_vbr_averages_at_the_target() {
        let frame = 960usize;
        let frames = 50;
        let src = tone(frame * frames, 2);
        for &(vbr, rate) in &[(false, 96000u32), (true, 96000)] {
            let mut e = Encoder::new(48000, 2, Application::Audio).unwrap();
            e.set_vbr(vbr);
            e.set_bitrate(rate);
            let mut buf = [0u8; 1500];
            let mut total = 0usize;
            let mut sizes = Vec::new();
            for t in 0..frames {
                let pcm = &src[t * frame * 2..(t + 1) * frame * 2];
                let n = e.encode_float(pcm, frame, &mut buf).unwrap();
                total += n;
                sizes.push(n);
            }
            let want = rate as usize * frames / (50 * 8);
            if !vbr {
                assert!(
                    sizes.iter().all(|&s| s == sizes[0]),
                    "CBR frame sizes vary: {:?}",
                    &sizes[..5]
                );
            }
            let err = (total as f64 - want as f64).abs() / want as f64;
            assert!(err < 0.1, "vbr={vbr}: {total} bytes vs {want} wanted");
        }
    }

    #[test]
    fn self_delimited_frames_parse_back() {
        let frame = 480usize;
        let src = tone(frame, 2);
        let mut e = Encoder::new(48000, 2, Application::Audio).unwrap();
        e.set_bitrate(64000);
        let mut buf = [0u8; 1500];
        let n = e.encode_self_delimited(&src, frame, &mut buf).unwrap();
        let p = crate::packet::Packet::parse(&buf[..n], true).unwrap();
        assert_eq!(p.consumed, n);
        assert_eq!(p.frames.len(), 1);
        assert_eq!(p.toc.frame_size_48k(), frame);
    }

    #[test]
    fn bandwidth_follows_the_bitrate_and_the_input_rate() {
        let mut e = Encoder::new(48000, 1, Application::Audio).unwrap();
        e.set_bitrate(64000);
        assert_eq!(e.auto_bandwidth(), Bandwidth::Full);
        e.set_bitrate(16000);
        assert_eq!(e.auto_bandwidth(), Bandwidth::SuperWide);
        e.set_bitrate(10000);
        assert_eq!(e.auto_bandwidth(), Bandwidth::Wide);
        // 24 kHz input cannot carry a fullband stream whatever the rate.
        let mut e = Encoder::new(24000, 2, Application::Audio).unwrap();
        e.set_bitrate(256_000);
        assert_eq!(e.auto_bandwidth(), Bandwidth::SuperWide);
    }
}
