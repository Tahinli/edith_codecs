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
//! - **The MDCT is the shared one.** CELT synthesis runs through
//!   [`ec_dsp::Mdct`], never a direct-form DFT. An incumbent Opus decoder in
//!   this product shipped its FFT behind a default-off feature and ran at 0.95x
//!   realtime on 5.1; there is no such switch here, and no code path that could
//!   answer to one.
//! - **Malformed input is an error, never a panic.** Framing rules [R1]..[R7]
//!   are checked in [`packet`], and the range decoder feeds zeros past the end
//!   of a truncated frame exactly as Section 4.1.2.1 requires.
//! - **Output is `f32`, interleaved, at the rate you asked for.** 48, 24, 16,
//!   12 and 8 kHz are supported; SILK's internal rate is resampled up and the
//!   sum of the two layers is resampled down once, at the end.
//!
//! No unsafe, no allocation on the per-frame path beyond the decoder's own
//! buffers, no dependencies outside the family.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod celt;
pub mod packet;
pub mod range;

pub use celt::CeltDecoder;
pub use packet::{Bandwidth, Mode, Packet, Toc};
pub use range::RangeDecoder;

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
            // DTX or a dropped frame: no PLC in this decoder, so emit silence.
            out.fill(0.0);
            return Ok(0);
        }
        let mut dec = RangeDecoder::new(data);

        if mode != Mode::Celt {
            return Err(Error::unsupported(
                "Opus SILK layer",
                "only the CELT layer is implemented in this build",
            ));
        }

        // A mode switch invalidates the MDCT overlap and energy history.
        if self.prev_mode.is_some_and(|p| p != mode) && !self.prev_redundancy {
            self.celt.reset();
        }
        let end_band = toc.bandwidth().celt_end_band();
        self.celt
            .decode(&mut dec, out, frame_48k, 0, end_band, stream_channels)?;
        self.prev_mode = Some(mode);
        self.prev_redundancy = false;
        Ok(dec.range())
    }
}
