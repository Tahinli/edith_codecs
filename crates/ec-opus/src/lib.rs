//! Opus decoding (RFC 6716, with the RFC 8251 corrections).
//!
//! Opus is two codecs behind one entropy coder: a linear-prediction layer
//! (SILK) for speech up to 8 kHz of bandwidth, and an MDCT layer (CELT) for
//! everything above that and for music. A packet's table-of-contents byte picks
//! one of them or both, and this crate decodes all three cases plus the
//! multistream framing that carries 5.1 and 7.1.
//!
//! Contracts worth knowing before implementing against this crate:
//!
//! - **The FFT is never optional.** CELT synthesis runs an `N/4`-point FFT
//!   through [`ec_dsp`], never a direct-form DFT. An incumbent Opus decoder in
//!   this product shipped its FFT behind a default-off feature and ran at 0.95x
//!   realtime on 5.1; there is no such switch here, and no code path that could
//!   answer to one. (CELT's transform sizes are `15*2^k`, so [`ec_dsp::Mdct`]
//!   itself — power-of-two only — cannot serve them; [`celt`] wraps
//!   [`ec_dsp::Fft`] with one radix-3/radix-5 stage instead.)
//! - **Malformed input is an error, never a panic.** Framing rules `[R1]`..`[R7]`
//!   are checked in [`packet`], and the range decoder feeds zeros past the end
//!   of a truncated frame exactly as Section 4.1.2.1 requires.
//! - **Output is `f32`, interleaved, at the rate you asked for.** 48, 24, 16,
//!   12 and 8 kHz are supported; SILK's internal rate is resampled up (by the
//!   normative resampler in [`silk`]) and CELT decimates on the way out.
//! - **There is no packet loss concealment.** A frame of one byte or none —
//!   a dropped or DTX frame — decodes to silence, where the reference
//!   extrapolates from the previous frame. This decoder is fed whole packets by
//!   a container, never a gap, so the concealment would be dead weight; a
//!   caller that needs it (an RTP receiver) has to add it.
//!
//! The crate also carries the family's Opus *encoder* — [`Encoder`] for one
//! stream, [`MultistreamEncoder`] for surround — CELT-only, fullband, mono
//! and stereo, every rate from 16 to 510 kbps, CBR and constrained VBR.
//!
//! **Why the incumbent encoder collapsed above ~128-165 kbps** (the measured
//! record this crate replaces; edith `engine/Cargo.toml:124-140` and
//! `export.rs:1834`): its packets above that rate were decodable only by its
//! *own* decoder — correlation 0.06 against libopus at 256 kbps while its
//! internal round trip read 0.999 — and the rate the cliff sat at moved with
//! the content. That signature — self-consistent, world-divergent, at a
//! *bits-per-band* threshold — is a shared encode/decode convention error in
//! exactly the code CELT only exercises when a band's budget is large: the
//! over-32-bit-codeword band split (RFC 6716 Section 4.3.4.1: a band whose bit
//! demand exceeds its cache row is halved recursively, with an entropy-coded
//! angle and a rebalance of the leftover bits). An encoder whose split
//! bookkeeping (`tell`-derived budgets, rebalance, theta pdf selection)
//! drifts one bit from the normative derivation still decodes its own output
//! — both halves share the drift — but a conformant decoder desynchronises,
//! and the threshold moves with content because the split only triggers on
//! loud, flat bands. Content-dependence is also why mono broke at *every*
//! rate: half the coefficients means every band hits the split threshold at
//! half the bitrate. This encoder mirrors the reference's allocation
//! arithmetic symbol for symbol, and the tests hold the closed loop to it:
//! every packet is range-state-exact against this crate's own
//! RFC-vector-verified decoder, and libopus (via ffmpeg) plus the reference
//! `opus_demo` decoder read the same packets at correlation >= 0.99 across
//! the whole rate table, mono, stereo and 5.1 alike.
//!
//! No unsafe, no allocation on the per-frame path beyond the codecs' own
//! buffers, no dependencies outside the family.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod celt;
pub mod celt_enc;
pub mod encoder;
pub mod multistream;
pub mod multistream_enc;
pub mod ogg;
pub mod packet;
pub mod range;
pub mod silk;

pub use celt::CeltDecoder;
pub use celt_enc::CeltEncoder;
pub use encoder::{Application, Encoder};
pub use multistream::MultistreamDecoder;
pub use multistream_enc::MultistreamEncoder;
pub use packet::{Bandwidth, Mode, Packet, Toc};
pub use range::{RangeDecoder, RangeEncoder};
pub use silk::SilkDecoder;

use ec_core::{Error, Result};

/// One Opus stream: mono or stereo, any mode, any frame size.
///
/// Feed whole Opus packets to [`Decoder::decode_float`]; the decoder tracks the
/// inter-frame state (energy history, MDCT overlap, LPC memory) that Opus needs
/// across packets, so packets must be delivered in order.
#[derive(Clone, Debug)]
pub struct Decoder {
    sample_rate: u32,
    /// Output channels, which may exceed what a given packet codes.
    channels: usize,
    downsample: usize,
    celt: CeltDecoder,
    silk: SilkDecoder,
    /// The CELT overlap window, reused for redundancy cross-fades.
    celt_window: Vec<f32>,
    /// Per-frame scratch, allocated once: the SILK layer's output and the
    /// redundancy frame.
    silk_pcm: Vec<i16>,
    redundant: Vec<f32>,
    prev_mode: Option<Mode>,
    prev_redundancy: bool,
    range_final: u32,
}

impl Decoder {
    /// A decoder for `channels` channels at `sample_rate`, which must be one of
    /// 8000, 12000, 16000, 24000 or 48000 Hz.
    pub fn new(sample_rate: u32, channels: usize) -> Result<Decoder> {
        let downsample = match sample_rate {
            48000 => 1,
            24000 => 2,
            16000 => 3,
            12000 => 4,
            8000 => 6,
            _ => {
                return Err(Error::unsupported(
                    format!("opus output rate {sample_rate}"),
                    "Opus decodes to 8, 12, 16, 24 or 48 kHz only",
                ));
            }
        };
        if !(1..=2).contains(&channels) {
            return Err(Error::unsupported(
                format!("{channels}-channel Opus stream"),
                "one stream carries at most two channels; use MultistreamDecoder",
            ));
        }
        Ok(Decoder {
            sample_rate,
            channels,
            downsample,
            celt: CeltDecoder::new(channels, downsample),
            silk: SilkDecoder::new(sample_rate, channels),
            celt_window: celt::overlap_window(),
            silk_pcm: vec![0; 5760 * channels],
            redundant: vec![0.0; 240 * channels],
            prev_mode: None,
            prev_redundancy: false,
            range_final: 0,
        })
    }

    /// Output sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Output channels.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The range coder state after the last packet. The RFC 6716 test vectors
    /// carry this value per packet, which makes it a bit-exact conformance
    /// check on everything the decoder read.
    pub fn final_range(&self) -> u32 {
        self.range_final
    }

    /// Drops all inter-packet state; call after a seek.
    pub fn reset(&mut self) {
        self.celt.reset();
        self.silk.reset();
        self.prev_mode = None;
        self.prev_redundancy = false;
        self.range_final = 0;
    }

    /// Decodes one packet into interleaved `f32`, returning samples per channel.
    ///
    /// `out` must hold `channels * 120 ms` worth of samples for the general
    /// case (5760 per channel at 48 kHz).
    pub fn decode_float(&mut self, packet: &[u8], out: &mut [f32]) -> Result<usize> {
        let (samples, _) = self.decode_impl(packet, out, false)?;
        Ok(samples)
    }

    /// Decodes one self-delimited packet (RFC 6716, Appendix B), returning the
    /// samples per channel and the bytes the packet occupied. This is what the
    /// multistream framing needs for every stream but the last.
    pub fn decode_self_delimited(
        &mut self,
        data: &[u8],
        out: &mut [f32],
    ) -> Result<(usize, usize)> {
        self.decode_impl(data, out, true)
    }

    fn decode_impl(
        &mut self,
        data: &[u8],
        out: &mut [f32],
        self_delimited: bool,
    ) -> Result<(usize, usize)> {
        let parsed = Packet::parse(data, self_delimited)?;
        let toc = parsed.toc;
        let frame_48k = toc.frame_size_48k();
        let frame_out = frame_48k / self.downsample;
        let needed = parsed.frames.len() * frame_out * self.channels;
        if out.len() < needed {
            return Err(Error::corrupt(format!(
                "opus: output buffer holds {} samples, packet needs {needed}",
                out.len()
            )));
        }
        let mut done = 0usize;
        let mut range = 0u32;
        for frame in &parsed.frames {
            let dst = &mut out[done * self.channels..(done + frame_out) * self.channels];
            range = self.decode_frame(frame, dst, toc, frame_48k)?;
            done += frame_out;
        }
        self.range_final = range;
        Ok((done, parsed.consumed))
    }

    /// Decodes one Opus frame (one entropy-coded unit) of a packet.
    ///
    /// The three modes share this path: SILK fills the low band, CELT the high
    /// one, and a hybrid frame is their sum. The order of everything read from
    /// the range decoder here is normative — SILK, then the redundancy flags,
    /// then CELT (RFC 6716, Sections 4.2, 4.5.1 and 4.3).
    fn decode_frame(
        &mut self,
        data: &[u8],
        out: &mut [f32],
        toc: Toc,
        frame_48k: usize,
    ) -> Result<u32> {
        let mode = toc.mode();
        let stream_channels = toc.channels();
        if data.len() <= 1 {
            // DTX or a dropped frame. There is no PLC here (the container
            // layer feeds this decoder whole packets), so the frame is silent.
            out.fill(0.0);
            self.prev_mode = None;
            return Ok(0);
        }
        let mut dec = RangeDecoder::new(data);
        let mut len = data.len();
        let frame_out = frame_48k / self.downsample;
        let f5 = 240 / self.downsample;
        let f2_5 = 120 / self.downsample;

        // --- SILK layer -----------------------------------------------------
        let mut silk_pcm = core::mem::take(&mut self.silk_pcm);
        silk_pcm.clear();
        silk_pcm.resize(frame_out * self.channels, 0);
        if mode != Mode::Celt {
            if self.prev_mode == Some(Mode::Celt) {
                self.silk.reset();
            }
            let internal_rate = if mode == Mode::Hybrid {
                16000
            } else {
                match toc.bandwidth() {
                    Bandwidth::Narrow => 8000,
                    Bandwidth::Medium => 12000,
                    _ => 16000,
                }
            };
            let payload_ms = (frame_48k / 48).max(10);
            let mut done = 0usize;
            let mut first = true;
            while done < frame_out {
                let n = self.silk.decode(
                    &mut dec,
                    &mut silk_pcm[done * self.channels..],
                    payload_ms,
                    internal_rate,
                    stream_channels,
                    first,
                )?;
                if n == 0 {
                    break;
                }
                done += n;
                first = false;
            }
        }

        // --- Redundancy (Section 4.5.1) -------------------------------------
        let mut redundancy = false;
        let mut celt_to_silk = false;
        let mut redundancy_bytes = 0usize;
        if mode != Mode::Celt
            && dec.tell() as usize + 17 + 20 * usize::from(mode == Mode::Hybrid) <= 8 * len
        {
            redundancy = if mode == Mode::Hybrid {
                dec.dec_bit_logp(12)
            } else {
                true
            };
            if redundancy {
                celt_to_silk = dec.dec_bit_logp(1);
                redundancy_bytes = if mode == Mode::Hybrid {
                    dec.dec_uint(256) as usize + 2
                } else {
                    len - ((dec.tell() as usize + 7) >> 3)
                };
                if redundancy_bytes > len {
                    redundancy_bytes = 0;
                    redundancy = false;
                } else {
                    len -= redundancy_bytes;
                    if len * 8 < dec.tell() as usize {
                        len = 0;
                        redundancy_bytes = 0;
                        redundancy = false;
                    }
                    dec.shrink(len);
                }
            }
        }

        let end_band = toc.bandwidth().celt_end_band();
        let start_band = if mode == Mode::Celt { 0 } else { 17 };
        let mut redundant = core::mem::take(&mut self.redundant);
        redundant.clear();
        redundant.resize(f5 * self.channels, 0.0);

        // A redundancy frame that precedes the main one is decoded first.
        let mut redundant_rng = 0u32;
        if redundancy && celt_to_silk {
            let tail = &data[len..len + redundancy_bytes];
            let mut rdec = RangeDecoder::new(tail);
            self.celt
                .decode(&mut rdec, &mut redundant, 240, 0, end_band, stream_channels)?;
            redundant_rng = rdec.range();
        }

        // --- CELT layer -----------------------------------------------------
        if mode != Mode::Silk {
            // A mode switch invalidates the MDCT overlap and energy history.
            if self.prev_mode.is_some_and(|p| p != mode) && !self.prev_redundancy {
                self.celt.reset();
            }
            self.celt.decode(
                &mut dec,
                out,
                frame_48k.min(960),
                start_band,
                end_band,
                stream_channels,
            )?;
        } else {
            out.fill(0.0);
            // Fade the CELT layer out when leaving hybrid, as the reference
            // does, by decoding one silent 2.5 ms frame.
            if self.prev_mode == Some(Mode::Hybrid) && !(redundancy && celt_to_silk) {
                let silence = [0xFFu8, 0xFF];
                let mut sdec = RangeDecoder::new(&silence);
                let mut tmp = vec![0.0f32; f2_5 * self.channels];
                self.celt
                    .decode(&mut sdec, &mut tmp, 120, 0, end_band, stream_channels)?;
            }
        }

        // Sum the two layers.
        if mode != Mode::Celt {
            for (o, s) in out.iter_mut().zip(silk_pcm.iter()) {
                *o += *s as f32 * (1.0 / 32768.0);
            }
        }

        // A redundancy frame that follows the main one is cross-faded in.
        if redundancy && !celt_to_silk {
            self.celt.reset();
            let tail = &data[len..len + redundancy_bytes];
            let mut rdec = RangeDecoder::new(tail);
            self.celt
                .decode(&mut rdec, &mut redundant, 240, 0, end_band, stream_channels)?;
            redundant_rng = rdec.range();
            let base = (frame_out - f2_5) * self.channels;
            smooth_fade(
                &mut out[base..],
                &redundant[f2_5 * self.channels..],
                f2_5,
                self.channels,
                &self.celt_window,
                self.downsample,
            );
        }
        if redundancy && celt_to_silk {
            for c in 0..self.channels {
                for i in 0..f2_5 {
                    out[self.channels * i + c] = redundant[self.channels * i + c];
                }
            }
            // Then fade from the redundancy frame into the frame proper.
            let split = f2_5 * self.channels;
            for c in 0..self.channels {
                for i in 0..f2_5 {
                    let w = {
                        let x = self.celt_window[i * self.downsample];
                        x * x
                    };
                    let idx = split + i * self.channels + c;
                    if idx < out.len() && idx < redundant.len() {
                        out[idx] = w * out[idx] + (1.0 - w) * redundant[idx];
                    }
                }
            }
        }

        self.prev_mode = Some(mode);
        self.prev_redundancy = redundancy && !celt_to_silk;
        self.silk_pcm = silk_pcm;
        self.redundant = redundant;
        Ok(dec.range() ^ redundant_rng)
    }
}

/// Cross-fades `b` over `a` across `overlap` samples using the CELT window,
/// the shape the reference uses for redundancy and mode transitions.
fn smooth_fade(
    a: &mut [f32],
    b: &[f32],
    overlap: usize,
    channels: usize,
    window: &[f32],
    downsample: usize,
) {
    let inc = downsample;
    for c in 0..channels {
        for i in 0..overlap {
            let w = window[i * inc] * window[i * inc];
            let idx = i * channels + c;
            if idx < a.len() && idx < b.len() {
                a[idx] = w * b[idx] + (1.0 - w) * a[idx];
            }
        }
    }
}
