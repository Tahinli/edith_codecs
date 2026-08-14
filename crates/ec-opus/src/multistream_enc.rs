//! Multichannel Opus encoding: several elementary streams in one packet.
//!
//! The mirror of [`crate::MultistreamDecoder`]. Every stream but the last is
//! written in the self-delimiting framing of RFC 6716 Appendix B, so the
//! boundaries survive without a container.
//!
//! Channel order is the *mapping's*: with the RFC 7845 family 1 defaults from
//! [`crate::ogg::default_mapping`] that is Vorbis order, which for 5.1 is
//! left, centre, right, back left, back right, LFE. Feeding this encoder what
//! [`crate::MultistreamDecoder`] produced is therefore a round trip; feeding it
//! film order (FL, FR, FC, LFE, BL, BR) needs the `[0, 2, 1, 4, 5, 3]`
//! permutation first.

use ec_core::{Error, Result};

use crate::encoder::{Application, Encoder};
use crate::ogg::default_mapping;
use crate::packet::Bandwidth;

/// An encoder for a multichannel Opus stream.
#[derive(Clone, Debug)]
pub struct MultistreamEncoder {
    streams: Vec<Encoder>,
    coupled: usize,
    mapping: Vec<u8>,
    channels: usize,
    /// The stream carrying LFE, if the layout has one: a 120 Hz channel does
    /// not need — and cannot use — a full share of the bitrate.
    lfe_stream: Option<usize>,
    bitrate: u32,
    /// One stream's interleaved input.
    buf: Vec<f32>,
}

impl MultistreamEncoder {
    /// An encoder for `streams` elementary streams of which the first
    /// `coupled` are stereo, with `mapping` naming the coded channel each
    /// input channel feeds.
    pub fn new(
        sample_rate: u32,
        streams: usize,
        coupled: usize,
        mapping: &[u8],
        application: Application,
    ) -> Result<MultistreamEncoder> {
        if streams == 0 || streams > 255 || coupled > streams {
            return Err(Error::corrupt(format!(
                "opus multistream: {streams} streams of which {coupled} coupled"
            )));
        }
        let coded = coupled + streams;
        if mapping.is_empty() || mapping.len() > 255 {
            return Err(Error::corrupt(format!(
                "opus multistream: {} channels",
                mapping.len()
            )));
        }
        if let Some(&bad) = mapping.iter().find(|&&m| m != 255 && m as usize >= coded) {
            return Err(Error::corrupt(format!(
                "opus multistream: mapping entry {bad} names a channel of {coded}"
            )));
        }
        let mut encs = Vec::with_capacity(streams);
        for s in 0..streams {
            let ch = if s < coupled { 2 } else { 1 };
            encs.push(Encoder::new(sample_rate, ch, application)?);
        }
        let mut me = MultistreamEncoder {
            streams: encs,
            coupled,
            mapping: mapping.to_vec(),
            channels: mapping.len(),
            lfe_stream: None,
            bitrate: 0,
            buf: vec![0.0; 2 * 5760],
        };
        me.set_bitrate(64_000 * mapping.len().min(8) as u32 / 2);
        Ok(me)
    }

    /// An encoder for the RFC 7845 family 1 default layout of `channels`
    /// channels — the one an Ogg-Opus 5.1 or 7.1 file uses.
    pub fn surround(
        sample_rate: u32,
        channels: usize,
        application: Application,
    ) -> Result<MultistreamEncoder> {
        let (_, streams, coupled, table) = default_mapping(channels).ok_or_else(|| {
            Error::unsupported(
                format!("{channels}-channel Opus"),
                "RFC 7845 defines mappings for 1 to 8 channels",
            )
        })?;
        let mut me = Self::new(
            sample_rate,
            streams as usize,
            coupled as usize,
            &table,
            application,
        )?;
        // In the family 1 layouts the LFE is the last channel, and it is
        // always a mono stream.
        if channels == 6 || channels == 8 {
            let last = *table.last().expect("non-empty mapping") as usize;
            if last >= 2 * coupled as usize {
                me.lfe_stream = Some(last - coupled as usize);
                let rate = me.bitrate;
                me.set_bitrate(rate);
            }
        }
        Ok(me)
    }

    /// Output channels.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Elementary streams.
    pub fn streams(&self) -> usize {
        self.streams.len()
    }

    /// Coupled (stereo) streams; they come first.
    pub fn coupled(&self) -> usize {
        self.coupled
    }

    /// The channel mapping table.
    pub fn mapping(&self) -> &[u8] {
        &self.mapping
    }

    /// Total target bitrate, split across the streams: a coupled stream gets
    /// about 1.6 times a mono one (two channels share a lot, but not
    /// everything), and an LFE stream a third of a mono one — it carries one
    /// octave.
    pub fn set_bitrate(&mut self, bits_per_second: u32) {
        self.bitrate = bits_per_second;
        let weight = |s: usize, coupled: usize, lfe: Option<usize>| -> f32 {
            if Some(s) == lfe {
                0.3
            } else if s < coupled {
                1.6
            } else {
                1.0
            }
        };
        let total: f32 = (0..self.streams.len())
            .map(|s| weight(s, self.coupled, self.lfe_stream))
            .sum();
        for s in 0..self.streams.len() {
            let share = weight(s, self.coupled, self.lfe_stream) / total;
            self.streams[s].set_bitrate((bits_per_second as f32 * share) as u32);
        }
    }

    /// Constrained VBR (the default) or CBR, for every stream.
    pub fn set_vbr(&mut self, vbr: bool) {
        for s in self.streams.iter_mut() {
            s.set_vbr(vbr);
        }
    }

    /// Forces the coded bandwidth of every stream.
    pub fn set_bandwidth(&mut self, bandwidth: Option<Bandwidth>) {
        for s in self.streams.iter_mut() {
            s.set_bandwidth(bandwidth);
        }
    }

    /// Encoder delay in input samples, the same for every stream.
    pub fn look_ahead(&self) -> usize {
        self.streams[0].look_ahead()
    }

    /// The combined range coder state, as RFC 6716 Section 6 combines it.
    pub fn final_range(&self) -> u32 {
        self.streams.iter().fold(0, |a, s| a ^ s.final_range())
    }

    /// Drops all inter-frame state.
    pub fn reset(&mut self) {
        for s in self.streams.iter_mut() {
            s.reset();
        }
    }

    /// Encodes one frame of interleaved `channels`-channel `f32` into `out`,
    /// returning the bytes written.
    pub fn encode_float(
        &mut self,
        pcm: &[f32],
        frame_size: usize,
        out: &mut [u8],
    ) -> Result<usize> {
        if pcm.len() < frame_size * self.channels {
            return Err(Error::corrupt(format!(
                "opus multistream encode: {} samples for {frame_size} x {}",
                pcm.len(),
                self.channels
            )));
        }
        let nb = self.streams.len();
        let mut pos = 0usize;
        for s in 0..nb {
            let stream_channels = if s < self.coupled { 2 } else { 1 };
            let need = frame_size * stream_channels;
            if self.buf.len() < need {
                self.buf.resize(need, 0.0);
            }
            self.buf[..need].fill(0.0);
            // Gather this stream's channels out of the interleaved input.
            for (c, &m) in self.mapping.iter().enumerate() {
                if m == 255 {
                    continue;
                }
                let m = m as usize;
                let dst = if s < self.coupled {
                    if m == 2 * s {
                        Some(0)
                    } else if m == 2 * s + 1 {
                        Some(1)
                    } else {
                        None
                    }
                } else if m == s + self.coupled {
                    Some(0)
                } else {
                    None
                };
                if let Some(off) = dst {
                    for i in 0..frame_size {
                        self.buf[stream_channels * i + off] = pcm[self.channels * i + c];
                    }
                }
            }
            let last = s == nb - 1;
            let n = if last {
                self.streams[s].encode_float(&self.buf[..need], frame_size, &mut out[pos..])?
            } else {
                self.streams[s].encode_self_delimited(
                    &self.buf[..need],
                    frame_size,
                    &mut out[pos..],
                )?
            };
            pos += n;
        }
        Ok(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MultistreamDecoder;

    #[test]
    fn five_one_round_trips_channel_for_channel() {
        // One distinct tone per channel: a routing error shows up as energy in
        // the wrong output.
        let frame = 960usize;
        let frames = 10;
        let ch = 6usize;
        let freqs = [0.05f32, 0.11, 0.17, 0.23, 0.31, 0.02];
        let src: Vec<f32> = (0..frame * frames * ch)
            .map(|i| {
                let c = i % ch;
                let t = (i / ch) as f32;
                0.5 * (t * freqs[c]).sin()
            })
            .collect();
        let mut e = MultistreamEncoder::surround(48000, ch, Application::Audio).unwrap();
        e.set_bitrate(320_000);
        let mut d = MultistreamDecoder::with_rate(48000, e.streams(), e.coupled(), e.mapping());
        let mut buf = vec![0u8; 8 * 1500];
        let mut out = vec![0.0f32; frame * ch];
        let mut got = vec![0.0f32; frame * frames * ch];
        for t in 0..frames {
            let pcm = &src[t * frame * ch..(t + 1) * frame * ch];
            let n = e.encode_float(pcm, frame, &mut buf).unwrap();
            let samples = d.decode_float(&buf[..n], &mut out).unwrap();
            assert_eq!(samples, frame);
            assert_eq!(d.final_range(), e.final_range(), "packet {t}: rng desynced");
            got[t * frame * ch..(t + 1) * frame * ch].copy_from_slice(&out);
        }
        // Correlate each output channel against its own input, skipping the
        // encoder's delay and the first frames.
        for c in 0..ch {
            let (mut num, mut da, mut db) = (0.0f64, 0.0f64, 0.0f64);
            for i in 2 * frame..frame * frames {
                let a = got[i * ch + c] as f64;
                let b = src[(i - 120) * ch + c] as f64;
                num += a * b;
                da += a * a;
                db += b * b;
            }
            let corr = num / (da.sqrt() * db.sqrt());
            assert!(corr > 0.9, "channel {c}: correlation {corr}");
        }
    }
}
