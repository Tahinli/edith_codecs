//! The Opus packet encoder: CELT-only, 8 to 48 kHz in.
//!
//! One [`Encoder`] produces ordinary code-0 Opus packets (RFC 6716 Section 3)
//! or the self-delimited variant of Appendix B that the multistream framing
//! needs. Every configuration this encoder emits is a CELT one (TOC configs
//! 16..31), which is what music encoding uses at every rate the product asks
//! for; there is no SILK and no hybrid path — a speech-optimised mode below
//! 16 kbps is out of scope, not silently approximated.
//!
//! Input below 48 kHz is zero-stuffed to CELT's native rate and the coded
//! bandwidth is capped at the input's own Nyquist, so the images the stuffing
//! puts there are never coded and the decoder's decimation drops them.
//! [`Encoder::look_ahead`] reports the delay in *input* samples.
//!
//! Rate control is CBR by default — every packet the same size, derived from
//! the bitrate — or constrained VBR, the reference's reservoir scheme, when
//! [`Encoder::set_vbr_constrained`] is on.

use ec_core::{Error, Result};

use crate::celt_enc::CeltEncoder;
use crate::packet::Bandwidth;
use crate::range::RangeEncoder;

/// Most bytes one Opus frame may occupy (RFC 6716 `[R2]`).
const MAX_FRAME_BYTES: usize = 1275;

/// What the caller is encoding. This encoder is CELT-only, so the setting
/// biases the coded bandwidth rather than selecting a different layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Application {
    /// Speech: one step narrower at a given rate, where the top octave costs
    /// most and carries least.
    Voip,
    /// Music and general audio, the default.
    Audio,
    /// Lowest delay.
    LowDelay,
}

/// One elementary Opus encoder: mono or stereo, CELT-only.
#[derive(Clone, Debug)]
pub struct Encoder {
    celt: CeltEncoder,
    range: RangeEncoder,
    sample_rate: u32,
    /// 48000 / `sample_rate`: the zero-stuffing factor into CELT.
    upsample: usize,
    channels: usize,
    bitrate: u32,
    vbr: bool,
    application: Application,
    bandwidth: Option<Bandwidth>,
    final_range: u32,
}

impl Encoder {
    /// An encoder for `channels` channels (1 or 2) at `sample_rate`, which
    /// must be one of 8000, 12000, 16000, 24000 or 48000 Hz — the rates
    /// RFC 6716 Section 2 admits. CELT itself runs at 48 kHz whatever the
    /// input rate is (RFC 7845 Section 5.1).
    pub fn new(sample_rate: u32, channels: usize, application: Application) -> Result<Encoder> {
        let upsample = match sample_rate {
            48000 => 1,
            24000 => 2,
            16000 => 3,
            12000 => 4,
            8000 => 6,
            _ => {
                return Err(Error::unsupported(
                    format!("opus encode at {sample_rate} Hz"),
                    "Opus encodes from 8, 12, 16, 24 or 48 kHz; resample first",
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
            celt: CeltEncoder::new(channels, upsample),
            range: RangeEncoder::new(),
            sample_rate,
            upsample,
            channels,
            bitrate: 64_000 * channels as u32,
            vbr: false,
            application,
            bandwidth: None,
            final_range: 0,
        })
    }

    /// Channels per frame of input.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The rate the caller feeds.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Encoder delay in *input* samples: the decoded stream lags the input by
    /// this much, and an Ogg-Opus pre-skip of `look_ahead * 48000/rate`
    /// cancels it exactly. One MDCT overlap, 120 samples at 48 kHz.
    pub fn look_ahead(&self) -> usize {
        120 / self.upsample
    }

    /// Forces the coded bandwidth; [`None`] (the default) picks it from the
    /// bitrate, the application and the input's Nyquist.
    pub fn set_bandwidth(&mut self, bandwidth: Option<Bandwidth>) {
        self.bandwidth = bandwidth;
    }

    /// Target bitrate in bits per second for the whole packet stream,
    /// including the packet headers. Clamped to 500..=510000.
    pub fn set_bitrate(&mut self, bps: u32) {
        self.bitrate = bps.clamp(500, 510_000);
    }

    /// Current target bitrate.
    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }

    /// Switches between CBR (default, every packet the target size) and
    /// constrained VBR (packets vary, a reservoir holds the average to the
    /// target).
    pub fn set_vbr_constrained(&mut self, vbr: bool) {
        self.vbr = vbr;
    }

    /// The range coder state after the last packet — the same value RFC 6716
    /// test vectors carry, and what a conformant decoder's `final_range`
    /// must equal after decoding that packet.
    pub fn final_range(&self) -> u32 {
        self.final_range
    }

    /// Drops all inter-frame state; the next packet starts like the first.
    pub fn reset(&mut self) {
        self.celt.reset();
        self.final_range = 0;
    }

    /// The TOC byte, the frame size in 48 kHz samples and the CELT end band
    /// for a frame of `frame_size` input samples per channel.
    fn toc(&self, frame_size: usize) -> Result<(u8, usize, usize)> {
        let frame_48k = frame_size * self.upsample;
        let fs_idx: u8 = match frame_48k {
            120 => 0,
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
        let bandwidth = self.bandwidth.unwrap_or_else(|| self.auto_bandwidth());
        let bw_idx: u8 = match bandwidth {
            Bandwidth::Narrow => 0,
            // CELT has no mediumband configuration; the next one up covers it.
            Bandwidth::Medium | Bandwidth::Wide => 1,
            Bandwidth::SuperWide => 2,
            Bandwidth::Full => 3,
        };
        let config = 16 + 4 * bw_idx + fs_idx;
        Ok((
            (config << 3) | (u8::from(self.channels == 2) << 2),
            frame_48k,
            bandwidth.celt_end_band(),
        ))
    }

    /// Bandwidth from the bitrate: coding 20 kHz at 24 kbps spends bits on air.
    ///
    /// The thresholds are on the mono-equivalent rate — a coupled stereo stream
    /// needs about 1.6x a mono one for the same per-channel quality — and VoIP
    /// asks for one step narrower. The input's own Nyquist bounds the result:
    /// coding above it would code the images the zero-stuffing put there.
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

    /// Encodes one frame — `frame_size` samples per channel of interleaved
    /// `f32` at the input rate, 2.5, 5, 10 or 20 ms of it — as a code-0 Opus
    /// packet in `out`, returning the packet length.
    pub fn encode_float(
        &mut self,
        pcm: &[f32],
        frame_size: usize,
        out: &mut [u8],
    ) -> Result<usize> {
        let (toc, frame_48k, end) = self.toc(frame_size)?;
        let n = self.encode_frame(pcm, frame_48k, end, out.len().saturating_sub(1))?;
        if out.len() < 1 + n {
            return Err(Error::corrupt(format!(
                "opus encode: packet needs {} bytes, buffer holds {}",
                1 + n,
                out.len()
            )));
        }
        out[0] = toc;
        out[1..1 + n].copy_from_slice(&self.range.data()[..n]);
        Ok(1 + n)
    }

    /// [`Encoder::encode_float`] from 16-bit samples.
    pub fn encode(&mut self, pcm: &[i16], frame_size: usize, out: &mut [u8]) -> Result<usize> {
        let float: Vec<f32> = pcm.iter().map(|&v| v as f32 * (1.0 / 32768.0)).collect();
        self.encode_float(&float, frame_size, out)
    }

    /// [`Encoder::encode_float`] in the self-delimited framing of RFC 6716
    /// Appendix B: TOC, an explicit frame length, then the frame. This is
    /// what every stream but the last uses inside a multistream packet.
    pub fn encode_self_delimited(
        &mut self,
        pcm: &[f32],
        frame_size: usize,
        out: &mut [u8],
    ) -> Result<usize> {
        let (toc, frame_48k, end) = self.toc(frame_size)?;
        let n = self.encode_frame(pcm, frame_48k, end, out.len().saturating_sub(3))?;
        let len_bytes = if n < 252 { 1 } else { 2 };
        if out.len() < 1 + len_bytes + n {
            return Err(Error::corrupt(format!(
                "opus encode: self-delimited packet needs {} bytes, buffer holds {}",
                1 + len_bytes + n,
                out.len()
            )));
        }
        out[0] = toc;
        if n < 252 {
            out[1] = n as u8;
        } else {
            let b0 = 252 + ((n - 252) & 3);
            out[1] = b0 as u8;
            out[2] = ((n - b0) / 4) as u8;
        }
        out[1 + len_bytes..1 + len_bytes + n].copy_from_slice(&self.range.data()[..n]);
        Ok(1 + len_bytes + n)
    }

    /// Encodes the CELT frame into the internal range coder and returns its
    /// byte length. `frame_size` is in 48 kHz samples, `end` is the last coded
    /// band, and `cap` bounds the frame (the caller's buffer minus headers).
    fn encode_frame(
        &mut self,
        pcm: &[f32],
        frame_size: usize,
        end: usize,
        cap: usize,
    ) -> Result<usize> {
        let cap = cap.min(MAX_FRAME_BYTES);
        if cap < 2 {
            return Err(Error::corrupt(
                "opus encode: output buffer smaller than the minimum packet",
            ));
        }
        // One byte per packet goes to the TOC; the CELT layer sees the rest.
        let toc_bps = 8 * 48000 / frame_size as u32;
        let (budget, vbr_rate) = if self.vbr {
            (cap, self.bitrate.saturating_sub(toc_bps).max(500))
        } else {
            let b = ((self.bitrate as u64 * frame_size as u64 + 4 * 48000) / (8 * 48000)) as usize;
            (b.saturating_sub(1).clamp(2, cap), 0)
        };
        self.range.reset(budget);
        let n = self
            .celt
            .encode(&mut self.range, pcm, frame_size, end, vbr_rate)?;
        self.final_range = self.range.range();
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The private half of the bandwidth decision: the thresholds themselves,
    /// which the TOC config only reports.
    #[test]
    fn bandwidth_follows_the_bitrate_the_application_and_the_input_rate() {
        let mut e = Encoder::new(48000, 1, Application::Audio).unwrap();
        e.set_bitrate(64000);
        assert_eq!(e.auto_bandwidth(), Bandwidth::Full);
        e.set_bitrate(16000);
        assert_eq!(e.auto_bandwidth(), Bandwidth::SuperWide);
        e.set_bitrate(10000);
        assert_eq!(e.auto_bandwidth(), Bandwidth::Wide);
        e.set_bitrate(8000);
        assert_eq!(e.auto_bandwidth(), Bandwidth::Narrow);
        // Speech asks for one step narrower at the same rate.
        let mut v = Encoder::new(48000, 1, Application::Voip).unwrap();
        v.set_bitrate(16000);
        assert_eq!(v.auto_bandwidth(), Bandwidth::Wide);
        // The input's own Nyquist bounds it whatever the rate says.
        let mut e = Encoder::new(24000, 2, Application::Audio).unwrap();
        e.set_bitrate(256_000);
        assert_eq!(e.auto_bandwidth(), Bandwidth::SuperWide);
        assert_eq!(e.look_ahead(), 60);
    }
}
