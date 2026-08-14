//! Multichannel Opus encoding: several elementary streams in one packet.
//!
//! The exact mirror of [`crate::MultistreamDecoder`]: the first `coupled`
//! streams are stereo, the rest mono, all but the last written in the
//! self-delimited framing of RFC 6716 Appendix B. `mapping[c]` names the
//! coded channel that input channel `c` feeds — the same table, in the same
//! direction, that the decoder uses to route its output, so input channel
//! *order is the mapping's*: RFC 7845 family 1 (what a 5.1 film track uses)
//! is Vorbis order — left, centre, right, back left, back right, LFE.
//!
//! An input channel mapped to 255 is discarded; a coded channel no input
//! names is encoded as silence.

use ec_core::{Error, Result};

use crate::encoder::Encoder;

/// A multichannel Opus encoder, mono through 255 channels.
#[derive(Debug)]
pub struct MultistreamEncoder {
    streams: Vec<Encoder>,
    coupled: usize,
    mapping: Vec<u8>,
    channels: usize,
    /// Scratch for one stream's deinterleaved input.
    buf: Vec<f32>,
}

impl MultistreamEncoder {
    /// An encoder for `streams` elementary streams of which the first
    /// `coupled` are stereo, with `mapping` naming the coded channel each
    /// input channel feeds. Layout rules are the decoder's, checked the same
    /// way.
    pub fn new(
        sample_rate: u32,
        streams: usize,
        coupled: usize,
        mapping: &[u8],
    ) -> Result<MultistreamEncoder> {
        if streams == 0 || streams > 255 || coupled > streams {
            return Err(Error::corrupt(format!(
                "opus multistream: {streams} streams of which {coupled} coupled"
            )));
        }
        let coded = coupled + streams;
        if mapping.is_empty() || mapping.len() > 255 {
            return Err(Error::corrupt(format!(
                "opus multistream: {} input channels",
                mapping.len()
            )));
        }
        if let Some(&bad) = mapping.iter().find(|&&m| m != 255 && m as usize >= coded) {
            return Err(Error::corrupt(format!(
                "opus multistream: mapping entry {bad} names a channel of {coded}"
            )));
        }
        let mut encoders = Vec::with_capacity(streams);
        for s in 0..streams {
            let ch = if s < coupled { 2 } else { 1 };
            encoders.push(Encoder::new(sample_rate, ch)?);
        }
        Ok(MultistreamEncoder {
            streams: encoders,
            coupled,
            mapping: mapping.to_vec(),
            channels: mapping.len(),
            buf: vec![0.0; 2 * 960],
        })
    }

    /// The RFC 7845 mapping-family-1 layout for 5.1: four streams, two
    /// coupled, Vorbis channel order (FL, FC, FR, BL, BR, LFE).
    pub fn surround_5_1(sample_rate: u32) -> Result<MultistreamEncoder> {
        MultistreamEncoder::new(sample_rate, 4, 2, &[0, 4, 1, 2, 3, 5])
    }

    /// Input channels per frame.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The `(streams, coupled, mapping)` triple a RFC 7845 `OpusHead` needs.
    pub fn layout(&self) -> (usize, usize, &[u8]) {
        (self.streams.len(), self.coupled, &self.mapping)
    }

    /// Total target bitrate in bits per second, split over the streams with
    /// a coupled stream weighing twice a mono one.
    pub fn set_bitrate(&mut self, total_bps: u32) {
        let mono = self.streams.len() - self.coupled;
        let shares = (2 * self.coupled + mono) as u32;
        let share = total_bps / shares.max(1);
        for (s, enc) in self.streams.iter_mut().enumerate() {
            let w = if s < self.coupled { 2 } else { 1 };
            enc.set_bitrate(share * w);
        }
    }

    /// CBR (default) or constrained VBR, applied to every stream.
    pub fn set_vbr_constrained(&mut self, vbr: bool) {
        for enc in self.streams.iter_mut() {
            enc.set_vbr_constrained(vbr);
        }
    }

    /// Drops all inter-frame state in every stream.
    pub fn reset(&mut self) {
        for enc in self.streams.iter_mut() {
            enc.reset();
        }
    }

    /// The range coder states of all streams, combined the way RFC 6716
    /// Section 6 combines them for conformance checking.
    pub fn final_range(&self) -> u32 {
        self.streams.iter().fold(0, |acc, s| acc ^ s.final_range())
    }

    /// Encodes one frame — `frame_size` samples per channel of interleaved
    /// `f32` in the mapping's channel order — as one multistream Opus packet
    /// in `out`, returning the packet length.
    pub fn encode_float(
        &mut self,
        pcm: &[f32],
        frame_size: usize,
        out: &mut [u8],
    ) -> Result<usize> {
        if pcm.len() < frame_size * self.channels {
            return Err(Error::corrupt(format!(
                "opus multistream encode: {} samples for {} channels of {frame_size}",
                pcm.len(),
                self.channels
            )));
        }
        let nb_streams = self.streams.len();
        let mut pos = 0usize;
        for s in 0..nb_streams {
            let stream_channels = if s < self.coupled { 2 } else { 1 };
            // Gather this stream's channels; a coded channel nobody feeds is
            // silence.
            self.buf[..frame_size * stream_channels].fill(0.0);
            for off in 0..stream_channels {
                let coded = if s < self.coupled {
                    2 * s + off
                } else {
                    self.coupled + s
                };
                if let Some(c) = self.mapping.iter().position(|&m| m as usize == coded) {
                    for i in 0..frame_size {
                        self.buf[i * stream_channels + off] = pcm[i * self.channels + c];
                    }
                }
            }
            let last = s == nb_streams - 1;
            let (head, buf) = (&mut out[pos..], &self.buf[..frame_size * stream_channels]);
            // The borrow checker cannot see that `buf` and the encoder are
            // disjoint fields through `&mut self`; a split borrow does.
            let enc = &mut self.streams[s];
            let n = if last {
                enc.encode_float(buf, frame_size, head)?
            } else {
                enc.encode_self_delimited(buf, frame_size, head)?
            };
            pos += n;
        }
        Ok(pos)
    }
}
