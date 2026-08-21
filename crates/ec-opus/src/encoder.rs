//! The Opus packet encoder: CELT, SILK and hybrid, 8 to 48 kHz in.
//!
//! One [`Encoder`] produces ordinary code-0 Opus packets (RFC 6716 Section 3)
//! or the self-delimited variant of Appendix B that the multistream framing
//! needs. [`Encoder::set_mode`] (or the automatic choice in [`Mode`]'s
//! default) picks CELT (TOC configs 16..31, every rate and frame size), SILK
//! (TOC configs 0/1/4/5/8/9, mono or stereo NB/MB/WB 10 or 20 ms; mono also
//! 40/60 ms through [`SilkEncoder`]), or Hybrid (TOC configs 13/15, mono
//! SWB/FB 20 ms: SILK at 16 kHz under CELT from band 17 up, one range-coded
//! stream; stereo and other frame sizes fall back to CELT).
//!
//! Input below 48 kHz is zero-stuffed to CELT's or SILK's native rate and
//! the coded bandwidth is capped at the input's own Nyquist, so the images
//! the stuffing puts there are never coded and the decoder's decimation
//! drops them. [`Encoder::look_ahead`] reports the delay in *input* samples.
//!
//! Rate control is CBR by default — every packet the same size, derived from
//! the bitrate — or constrained VBR, the reference's reservoir scheme, when
//! [`Encoder::set_vbr_constrained`] is on. SILK packets are not yet budgeted
//! against the target rate (D2b).

use ec_core::{Error, Result};

use crate::celt_enc::CeltEncoder;
use crate::packet::{Bandwidth, Mode};
use crate::range::RangeEncoder;
use crate::silk_enc_write::{SilkEncoder, SilkStereoEncoder};

/// Most bytes one Opus frame may occupy (RFC 6716 `[R2]`).
const MAX_FRAME_BYTES: usize = 1275;

/// SILK's round-trip algorithmic delay in 48 kHz samples: the analysis
/// resampler (`SilkEncoder::delay_samples`) on the way in plus the decoder's
/// own synthesis resampler on the way out. Click-measured, one impulse
/// through `Encoder`/`Decoder` at each bandwidth, not derived from a
/// formula — `Resampler`'s (decode-side) and `Resampler48`'s (encode-side)
/// FIR lengths differ per bandwidth, so narrowband and wideband delays
/// don't have to and don't match.
const SILK_LOOK_AHEAD_48K_NB: usize = 58;
const SILK_LOOK_AHEAD_48K_MB: usize = 54;
const SILK_LOOK_AHEAD_48K_WB: usize = 50;
/// How far the hybrid path delays the SILK layer's input so its output lines
/// up with the CELT layer's at the decoder (which sums them as-is): CELT's
/// overlap (120) minus SILK's WB round trip, click-measured through the
/// decoder with one layer muted (`hybrid_layers_align` in conformance.rs).
const HYBRID_SILK_DELAY_48K: usize = 120 - SILK_LOOK_AHEAD_48K_WB;
/// Bytes the CELT layer of a hybrid packet always keeps, whatever SILK spent.
const HYBRID_CELT_MIN_BYTES: usize = 8;

/// What the caller is encoding.
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

/// One elementary Opus encoder: mono or stereo.
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
    /// [`None`] picks SILK, CELT or (once D4 lands) Hybrid from the
    /// application and bitrate; `Some` forces one.
    mode: Option<Mode>,
    /// Lazily created on first use so the mode can flip between packets
    /// without paying for the unused layer's state.
    silk_nb: Option<SilkEncoder>,
    silk_mb: Option<SilkEncoder>,
    silk_wb: Option<SilkEncoder>,
    silk_stereo_nb: Option<SilkStereoEncoder>,
    silk_stereo_mb: Option<SilkStereoEncoder>,
    silk_stereo_wb: Option<SilkStereoEncoder>,
    /// Scratch space for the SILK payload, sized once so the steady-state
    /// loop (`steady_state_encode_loop_zero_alloc`) doesn't allocate.
    silk_buf: [u8; MAX_FRAME_BYTES + 1],
    /// Hybrid: the last [`HYBRID_SILK_DELAY_48K`] samples of SILK-layer input.
    silk_delay: Vec<f32>,
    /// Scratch space for the zero-stuffed SILK/hybrid input, sized once so
    /// the steady-state loop doesn't allocate a `Vec` per frame.
    silk_stuff: Vec<f32>,
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
            mode: None,
            silk_nb: None,
            silk_mb: None,
            silk_wb: None,
            silk_stereo_nb: None,
            silk_stereo_mb: None,
            silk_stereo_wb: None,
            silk_buf: [0u8; MAX_FRAME_BYTES + 1],
            silk_delay: Vec::new(),
            silk_stuff: Vec::new(),
        })
    }

    /// Overrides the automatic SILK/Hybrid/CELT choice `wants_silk` and
    /// `hybrid_choice` make from the application and bitrate; pass `None` to
    /// restore the automatic pick. Requests this encoder cannot honour
    /// (SILK longer than 20 ms through this wrapper or 10 ms Hybrid) fall back to CELT.
    pub fn set_mode(&mut self, mode: Option<Mode>) {
        self.mode = mode;
    }

    /// Channels per frame of input.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The rate the caller feeds.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Encoder delay in *input* samples for a `frame_size`-sample (per
    /// channel, native rate) frame: the decoded stream lags the input by
    /// this much, and an Ogg-Opus pre-skip of `look_ahead * 48000/rate`
    /// cancels it exactly. CELT: one MDCT overlap, 120 samples at 48 kHz.
    /// SILK (mono, 10 or 20 ms frames, when the application/bitrate or an
    /// explicit [`Encoder::set_mode`] select it — the same
    /// [`Encoder::silk_choice`] predicate `encode_toc_and_payload` dispatches
    /// on, so this always matches which layer actually codes the frame):
    /// [`SILK_LOOK_AHEAD_48K_NB`], [`SILK_LOOK_AHEAD_48K_MB`] or
    /// [`SILK_LOOK_AHEAD_48K_WB`]. Hybrid: CELT's 120, the SILK layer being
    /// delayed to meet it ([`HYBRID_SILK_DELAY_48K`]). Any other frame size
    /// (or SILK/Hybrid's mono requirement unmet) falls back to CELT here
    /// exactly as `encode_toc_and_payload` does.
    pub fn look_ahead(&self, frame_size: usize) -> usize {
        let frame_48k = frame_size * self.upsample;
        if let Some(fs_khz) = self.silk_choice(frame_48k) {
            let delay = match fs_khz {
                8 => SILK_LOOK_AHEAD_48K_NB,
                12 => SILK_LOOK_AHEAD_48K_MB,
                _ => SILK_LOOK_AHEAD_48K_WB,
            };
            return delay / self.upsample;
        }
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
        self.silk_nb = None;
        self.silk_mb = None;
        self.silk_wb = None;
        self.silk_stereo_nb = None;
        self.silk_stereo_mb = None;
        self.silk_stereo_wb = None;
        self.silk_delay.clear();
        self.final_range = 0;
    }

    /// Whether the application/bitrate (or an explicit [`Encoder::set_mode`])
    /// call for SILK, and if so, which SILK internal sample rate in kHz.
    /// `None` means CELT or Hybrid (`hybrid_choice` tells those apart).
    fn wants_silk(&self) -> Option<usize> {
        let forced = || match self.bandwidth.unwrap_or_else(|| self.auto_bandwidth()) {
            Bandwidth::Narrow => 8,
            Bandwidth::Medium => 12,
            _ => 16,
        };
        match self.mode {
            Some(Mode::Celt) | Some(Mode::Hybrid) => None,
            Some(Mode::Silk) => Some(forced()),
            None => {
                if self.application == Application::Voip && self.bitrate < 20_000 {
                    Some(if matches!(self.bandwidth, Some(Bandwidth::Medium)) {
                        12
                    } else if self.bitrate > 10_000 {
                        16
                    } else {
                        8
                    })
                } else {
                    None
                }
            }
        }
    }

    /// `wants_silk` narrowed to what this encoder can actually code as SILK:
    /// mono or stereo, 10 or 20 ms (480 or 960 samples at 48 kHz) frames.
    fn silk_choice(&self, frame_48k: usize) -> Option<usize> {
        let fs_khz = self.wants_silk()?;
        if !matches!(frame_48k, 480 | 960) {
            return None;
        }
        Some(fs_khz)
    }

    /// Whether this frame goes out as a hybrid packet, and at which of the two
    /// hybrid bandwidths. Automatic: VoIP from 20 kbps per coded channel
    /// (where SILK alone stops) to 40 kbps per coded channel (where CELT alone
    /// is the better speech coder too), at the bitrate's own bandwidth
    /// (SWB/FB; narrower requests are CELT's). Mono and stereo, 10 or 20 ms.
    fn hybrid_choice(&self, frame_48k: usize) -> Option<Bandwidth> {
        let lo = 20_000 * self.channels as u32;
        let hi = 40_000 * self.channels as u32;
        let wanted = match self.mode {
            Some(Mode::Hybrid) => true,
            Some(_) => false,
            None => self.application == Application::Voip && (lo..hi).contains(&self.bitrate),
        };
        if !wanted || !matches!(frame_48k, 480 | 960) {
            return None;
        }
        let bandwidth = self.bandwidth.unwrap_or_else(|| self.auto_bandwidth());
        match bandwidth {
            Bandwidth::SuperWide | Bandwidth::Full => Some(bandwidth),
            // A forced Hybrid codes at least SWB; an automatic one that the
            // bitrate would cap narrower is CELT's.
            _ if self.mode == Some(Mode::Hybrid) => Some(Bandwidth::SuperWide),
            _ => None,
        }
    }

    /// The SILK layer's share of a mono hybrid packet's bitrate, the
    /// reference's 20 ms table interpolated: the rest is CELT's.
    fn hybrid_silk_rate(total: u32, bandwidth: Bandwidth) -> u32 {
        const TABLE: [(u32, u32, u32); 7] = [
            (0, 0, 0),
            (12000, 10000, 11000),
            (16000, 13500, 14500),
            (20000, 16000, 17000),
            (24000, 18000, 19000),
            (32000, 22000, 24000),
            (64000, 38000, 42000),
        ];
        let pick = |row: &(u32, u32, u32)| {
            if bandwidth == Bandwidth::Full {
                row.2
            } else {
                row.1
            }
        };
        let total = total.min(64000);
        let i = TABLE
            .iter()
            .rposition(|r| r.0 <= total)
            .unwrap()
            .min(TABLE.len() - 2);
        let (lo, hi) = (&TABLE[i], &TABLE[i + 1]);
        pick(lo) + (pick(hi) - pick(lo)) * (total - lo.0) / (hi.0 - lo.0)
    }

    /// Zero-stuffs a mono, `frame_size`-sample native-rate frame up to
    /// `frame_size * upsample` samples at 48 kHz, the rate [`SilkEncoder`]
    /// takes — the same technique [`CeltEncoder::encode`] uses, scaled by
    /// `upsample` so the passband gain survives the stuffing. Writes into
    /// `out` (an `Encoder` scratch field, not a fresh `Vec` per call) rather
    /// than returning one, so the caller doesn't allocate every frame; the
    /// slots between the `upsample`-strided samples stay zero from one call
    /// to the next once `out` first grows to size, so only the strided
    /// samples themselves need rewriting.
    fn zero_stuff_mono(pcm: &[f32], frame_size: usize, upsample: usize, out: &mut Vec<f32>) {
        if upsample == 1 {
            out.clear();
            out.extend_from_slice(&pcm[..frame_size]);
            return;
        }
        out.resize(frame_size * upsample, 0.0);
        for i in 0..frame_size {
            out[i * upsample] = pcm[i] * upsample as f32;
        }
    }

    fn zero_stuff_stereo(pcm: &[f32], frame_size: usize, upsample: usize, out: &mut Vec<f32>) {
        if upsample == 1 {
            out.clear();
            out.extend_from_slice(&pcm[..2 * frame_size]);
            return;
        }
        out.resize(2 * frame_size * upsample, 0.0);
        out.fill(0.0);
        for i in 0..frame_size {
            let dst = 2 * i * upsample;
            out[dst] = pcm[2 * i] * upsample as f32;
            out[dst + 1] = pcm[2 * i + 1] * upsample as f32;
        }
    }

    /// Picks SILK or CELT for one frame and encodes it, returning the TOC
    /// byte and the payload bytes (excluding TOC), borrowed from scratch
    /// space owned by `self` — no allocation on the steady-state path.
    /// `cap` bounds the CELT path only — SILK isn't budgeted against a byte
    /// cap yet (D2b).
    fn encode_toc_and_payload(
        &mut self,
        pcm: &[f32],
        frame_size: usize,
        cap: usize,
    ) -> Result<(u8, &[u8])> {
        let frame_48k = frame_size * self.upsample;
        if let Some(fs_khz) = self.silk_choice(frame_48k) {
            if self.channels == 2 {
                Self::zero_stuff_stereo(pcm, frame_size, self.upsample, &mut self.silk_stuff);
                let enc = match fs_khz {
                    8 => self
                        .silk_stereo_nb
                        .get_or_insert_with(|| SilkStereoEncoder::new(false)),
                    12 => self
                        .silk_stereo_mb
                        .get_or_insert_with(SilkStereoEncoder::new_mediumband),
                    _ => self
                        .silk_stereo_wb
                        .get_or_insert_with(|| SilkStereoEncoder::new(true)),
                };
                enc.set_bitrate(self.bitrate);
                let n =
                    enc.encode_frame_ms(&self.silk_stuff, &mut self.silk_buf, frame_48k / 48)?;
                self.final_range = enc.final_range();
                return Ok((self.silk_buf[0], &self.silk_buf[1..n]));
            }
            Self::zero_stuff_mono(pcm, frame_size, self.upsample, &mut self.silk_stuff);
            let enc = match fs_khz {
                8 => self.silk_nb.get_or_insert_with(|| SilkEncoder::new(false)),
                12 => self.silk_mb.get_or_insert_with(SilkEncoder::new_mediumband),
                _ => self.silk_wb.get_or_insert_with(|| SilkEncoder::new(true)),
            };
            // Keep SILK's own reservoir-based rate control tracking the
            // Encoder's current target on every frame — cheap (an Option<u32>
            // store) and catches set_bitrate calls made between frames.
            enc.set_bitrate(self.bitrate);
            let n = enc.encode_frame_ms(&self.silk_stuff, &mut self.silk_buf, frame_48k / 48)?;
            self.final_range = enc.final_range();
            return Ok((self.silk_buf[0], &self.silk_buf[1..n]));
        }
        if let Some(bandwidth) = self.hybrid_choice(frame_48k) {
            let n = self.encode_hybrid(pcm, frame_size, bandwidth, cap)?;
            let config = 12
                + if bandwidth == Bandwidth::Full { 2u8 } else { 0 }
                + if frame_48k == 960 { 1 } else { 0 };
            return Ok((
                (config << 3) | (u8::from(self.channels == 2) << 2),
                &self.range.data()[..n],
            ));
        }
        let (toc, frame_48k, end) = self.toc(frame_size)?;
        let n = self.encode_frame(pcm, frame_48k, end, cap)?;
        Ok((toc, &self.range.data()[..n]))
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
        let (toc, payload) =
            self.encode_toc_and_payload(pcm, frame_size, out.len().saturating_sub(1))?;
        let n = payload.len();
        if out.len() < 1 + n {
            return Err(Error::corrupt(format!(
                "opus encode: packet needs {} bytes, buffer holds {}",
                1 + n,
                out.len()
            )));
        }
        out[0] = toc;
        out[1..1 + n].copy_from_slice(payload);
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
        let (toc, payload) =
            self.encode_toc_and_payload(pcm, frame_size, out.len().saturating_sub(3))?;
        let n = payload.len();
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
        out[1 + len_bytes..1 + len_bytes + n].copy_from_slice(payload);
        Ok(1 + len_bytes + n)
    }

    /// One hybrid frame (10 or 20 ms, mono or stereo, `bandwidth` SWB or FB)
    /// into the internal range coder, returning its byte length: the SILK
    /// layer's symbols (WB, 16 kHz internal, fed the input delayed by
    /// [`HYBRID_SILK_DELAY_48K`]), the no-redundancy flag under the decoder's
    /// exact presence rule, then the CELT layer from band 17 in the same
    /// coder. CBR: the packet is the bitrate's size unless SILK alone needs
    /// more, in which case CELT keeps [`HYBRID_CELT_MIN_BYTES`] on top.
    fn encode_hybrid(
        &mut self,
        pcm: &[f32],
        frame_size: usize,
        bandwidth: Bandwidth,
        cap: usize,
    ) -> Result<usize> {
        let cap = cap.min(MAX_FRAME_BYTES);
        let frame_48k = frame_size * self.upsample;
        let frame_ms = frame_48k / 48;
        let budget =
            ((self.bitrate as u64 * frame_48k as u64 + 4 * 48000) / (8 * 48000)) as usize;
        let budget = budget.saturating_sub(1).clamp(2, cap);
        if cap < HYBRID_CELT_MIN_BYTES + 2 {
            return Err(Error::corrupt(
                "opus encode: output buffer smaller than the minimum hybrid packet",
            ));
        }
        if self.channels == 2 {
            Self::zero_stuff_stereo(pcm, frame_size, self.upsample, &mut self.silk_stuff);
        } else {
            Self::zero_stuff_mono(pcm, frame_size, self.upsample, &mut self.silk_stuff);
        }
        let d = HYBRID_SILK_DELAY_48K * self.channels;
        if self.silk_delay.len() != d {
            self.silk_delay.clear();
            self.silk_delay.resize(d, 0.0);
        }
        let silk_len = frame_48k * self.channels;
        let mut delayed = Vec::with_capacity(silk_len);
        delayed.extend_from_slice(&self.silk_delay);
        delayed.extend_from_slice(&self.silk_stuff[..silk_len - d]);
        self.silk_delay
            .copy_from_slice(&self.silk_stuff[silk_len - d..silk_len]);

        self.range.reset(cap);
        let silk_rate = Self::hybrid_silk_rate(self.bitrate, bandwidth);
        if self.channels == 2 {
            let silk = self
                .silk_stereo_wb
                .get_or_insert_with(|| SilkStereoEncoder::new(true));
            silk.set_bitrate(silk_rate);
            silk.encode_hybrid_ms(&delayed, &mut self.range, frame_ms)?;
        } else {
            let silk = self.silk_wb.get_or_insert_with(|| SilkEncoder::new(true));
            silk.set_bitrate(silk_rate);
            silk.encode_hybrid_ms(&delayed, &mut self.range, frame_ms)?;
        }
        let silk_bytes = (self.range.tell() as usize).div_ceil(8);
        let budget = budget.max(silk_bytes + HYBRID_CELT_MIN_BYTES).min(cap);
        self.range.shrink(budget);
        // Redundancy flag: present under exactly the decoder's condition.
        if self.range.tell() as usize + 17 + 20 <= 8 * budget {
            self.range.enc_bit_logp(false, 12);
        }
        let n = self.celt.encode(
            &mut self.range,
            pcm,
            frame_48k,
            17,
            bandwidth.celt_end_band(),
            0,
        )?;
        self.final_range = self.range.range();
        let payload = &self.range.data()[..n];
        if self.channels == 2 {
            self.silk_stereo_wb
                .as_mut()
                .unwrap()
                .replay_hybrid(payload, frame_ms)?;
        } else {
            self.silk_wb
                .as_mut()
                .unwrap()
                .replay_hybrid(payload, frame_ms)?;
        }
        Ok(n)
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
            .encode(&mut self.range, pcm, frame_size, 0, end, vbr_rate)?;
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
        assert_eq!(e.look_ahead(960), 60);
    }

    /// `look_ahead` must use the same mono/10-or-20 ms-frame predicate
    /// dispatch does (`silk_choice`/`hybrid_choice`), not just `wants_silk`:
    /// 2.5/5 ms frames and stereo still code as CELT, while mono 10 ms
    /// speech follows SILK.
    #[test]
    fn look_ahead_matches_the_frame_size_dispatch_actually_uses() {
        let mut e = Encoder::new(48000, 1, Application::Voip).unwrap();
        e.set_bitrate(8000);
        assert_eq!(e.look_ahead(480), 58, "10ms NB SILK");
        assert_eq!(e.look_ahead(240), 120, "5ms frame falls back to CELT");
        assert_eq!(e.look_ahead(960), 58, "20ms NB SILK");

        e.set_bitrate(16000);
        assert_eq!(e.look_ahead(960), 50, "20ms WB SILK");

        e.set_bitrate(32000);
        assert_eq!(e.look_ahead(960), 120, "20ms hybrid, CELT's overlap");

        let mut e = Encoder::new(48000, 2, Application::Voip).unwrap();
        e.set_bitrate(8000);
        assert_eq!(e.look_ahead(960), 120, "stereo never codes as SILK/Hybrid");
    }
}
