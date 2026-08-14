//! The Opus packet encoder: CELT-only, fullband, 48 kHz.
//!
//! One [`Encoder`] produces ordinary code-0 Opus packets (RFC 6716 Section 3)
//! or the self-delimited variant of Appendix B that the multistream framing
//! needs. Every configuration this encoder emits is a fullband CELT one
//! (TOC configs 28..31), which is what music encoding uses at every rate the
//! product asks for; there is no SILK and no hybrid path — a speech-optimised
//! mode below 16 kbps is out of scope, not silently approximated.
//!
//! Rate control is CBR by default — every packet the same size, derived from
//! the bitrate — or constrained VBR, the reference's reservoir scheme, when
//! [`Encoder::set_vbr_constrained`] is on.

use ec_core::{Error, Result};

use crate::celt_enc::CeltEncoder;
use crate::range::RangeEncoder;

/// Most bytes one Opus frame may occupy (RFC 6716 `[R2]`).
const MAX_FRAME_BYTES: usize = 1275;

/// One elementary Opus encoder: mono or stereo, CELT-only, 48 kHz in.
#[derive(Clone, Debug)]
pub struct Encoder {
    celt: CeltEncoder,
    range: RangeEncoder,
    channels: usize,
    bitrate: u32,
    vbr: bool,
    final_range: u32,
}

impl Encoder {
    /// An encoder for `channels` channels (1 or 2) at `sample_rate`, which
    /// must be 48000 — CELT's native rate, and the only rate the product
    /// feeds it (RFC 7845 Section 5.1: every Opus decoder runs at 48 kHz
    /// regardless of what the container claims).
    pub fn new(sample_rate: u32, channels: usize) -> Result<Encoder> {
        if sample_rate != 48000 {
            return Err(Error::unsupported(
                format!("opus encode at {sample_rate} Hz"),
                "this CELT-only encoder takes 48 kHz input; resample first",
            ));
        }
        if !(1..=2).contains(&channels) {
            return Err(Error::unsupported(
                format!("{channels}-channel Opus stream"),
                "one stream carries at most two channels; use MultistreamEncoder",
            ));
        }
        Ok(Encoder {
            celt: CeltEncoder::new(channels),
            range: RangeEncoder::new(),
            channels,
            bitrate: 64_000 * channels as u32,
            vbr: false,
            final_range: 0,
        })
    }

    /// Channels per frame of input.
    pub fn channels(&self) -> usize {
        self.channels
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

    fn toc(&self, frame_size: usize) -> Result<u8> {
        let fs_idx: u8 = match frame_size {
            120 => 0,
            240 => 1,
            480 => 2,
            960 => 3,
            _ => {
                return Err(Error::unsupported(
                    format!("opus frame of {frame_size} samples"),
                    "CELT frames are 120, 240, 480 or 960 samples at 48 kHz",
                ));
            }
        };
        let config = 28 + fs_idx; // fullband CELT
        Ok((config << 3) | (u8::from(self.channels == 2) << 2))
    }

    /// Encodes one frame — `frame_size` samples per channel of interleaved
    /// `f32` at 48 kHz, `frame_size` one of 120/240/480/960 — as a code-0
    /// Opus packet in `out`, returning the packet length.
    pub fn encode_float(
        &mut self,
        pcm: &[f32],
        frame_size: usize,
        out: &mut [u8],
    ) -> Result<usize> {
        let toc = self.toc(frame_size)?;
        let n = self.encode_frame(pcm, frame_size, out.len().saturating_sub(1))?;
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

    /// [`Encoder::encode_float`] in the self-delimited framing of RFC 6716
    /// Appendix B: TOC, an explicit frame length, then the frame. This is
    /// what every stream but the last uses inside a multistream packet.
    pub fn encode_self_delimited(
        &mut self,
        pcm: &[f32],
        frame_size: usize,
        out: &mut [u8],
    ) -> Result<usize> {
        let toc = self.toc(frame_size)?;
        let n = self.encode_frame(pcm, frame_size, out.len().saturating_sub(3))?;
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
    /// byte length. `cap` bounds the frame (the caller's buffer minus
    /// headers).
    fn encode_frame(&mut self, pcm: &[f32], frame_size: usize, cap: usize) -> Result<usize> {
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
            .encode(&mut self.range, pcm, frame_size, vbr_rate)?;
        self.final_range = self.range.range();
        Ok(n)
    }
}
