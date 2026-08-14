//! Multichannel Opus: several elementary streams in one packet.
//!
//! A multistream packet is just the ordinary packets of each stream laid end to
//! end. All but the last use the self-delimiting framing of RFC 6716
//! Appendix B, which carries one extra length so the boundary can be found
//! without a container; the last uses the ordinary framing, so it takes
//! whatever is left. That is the whole of the format — everything else is the
//! channel mapping table, which says where each stream's channels land.
//!
//! Streams come in two kinds: the first `coupled` streams are stereo, the rest
//! mono. `mapping[c]` names the coded channel that output channel `c` reads
//! from: values below `2*coupled` are a coupled stream's left (even) or right
//! (odd) channel, values above index the mono streams, and 255 means the
//! channel is silent.
//!
//! Channel *order* is the mapping's, not this decoder's: for RFC 7845 mapping
//! family 1 — what a 5.1 or 7.1 film track uses — that is Vorbis order
//! (left, centre, right, back left, back right, LFE for 5.1), and a caller that
//! wants the product's FL/FR/FC/LFE/BL/BR order permutes it.

use ec_core::{Error, Result};

use crate::Decoder;

/// A decoder for a multichannel Opus stream, mono through 255 channels.
#[derive(Debug)]
pub struct MultistreamDecoder {
    streams: Vec<Decoder>,
    coupled: usize,
    mapping: Vec<u8>,
    channels: usize,
    sample_rate: u32,
    /// Scratch for one stream's output.
    buf: Vec<f32>,
}

impl MultistreamDecoder {
    /// A decoder for `streams` elementary streams of which the first `coupled`
    /// are stereo, with `mapping` naming the coded channel each output channel
    /// comes from.
    ///
    /// # Panics
    /// On a layout that breaks the RFC's limits — more coupled streams than
    /// streams, a mapping entry naming a channel no stream codes, an
    /// unsupported rate. The layout comes from a container header, so callers
    /// that read one from a file should check it (or use
    /// [`MultistreamDecoder::try_with_rate`]) rather than pass it straight in.
    pub fn with_rate(
        sample_rate: u32,
        streams: usize,
        coupled: usize,
        mapping: &[u8],
    ) -> MultistreamDecoder {
        match Self::try_with_rate(sample_rate, streams, coupled, mapping) {
            Ok(d) => d,
            Err(e) => panic!("invalid Opus multistream layout: {e}"),
        }
    }

    /// [`MultistreamDecoder::with_rate`] with the layout checked instead of
    /// asserted.
    pub fn try_with_rate(
        sample_rate: u32,
        streams: usize,
        coupled: usize,
        mapping: &[u8],
    ) -> Result<MultistreamDecoder> {
        if streams == 0 || streams > 255 || coupled > streams {
            return Err(Error::corrupt(format!(
                "opus multistream: {streams} streams of which {coupled} coupled"
            )));
        }
        let coded = coupled + streams;
        if mapping.is_empty() || mapping.len() > 255 {
            return Err(Error::corrupt(format!(
                "opus multistream: {} output channels",
                mapping.len()
            )));
        }
        if let Some(&bad) = mapping.iter().find(|&&m| m != 255 && m as usize >= coded) {
            return Err(Error::corrupt(format!(
                "opus multistream: mapping entry {bad} names a channel of {coded}"
            )));
        }
        let mut decoders = Vec::with_capacity(streams);
        for s in 0..streams {
            let ch = if s < coupled { 2 } else { 1 };
            decoders.push(Decoder::new(sample_rate, ch)?);
        }
        Ok(MultistreamDecoder {
            streams: decoders,
            coupled,
            mapping: mapping.to_vec(),
            channels: mapping.len(),
            sample_rate,
            buf: vec![0.0; 2 * 5760],
        })
    }

    /// Output channels.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Output sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Drops all inter-packet state in every stream; call after a seek.
    pub fn reset(&mut self) {
        for s in self.streams.iter_mut() {
            s.reset();
        }
    }

    /// The range coder states of all streams, combined the way RFC 6716
    /// Section 6 combines them for conformance checking.
    pub fn final_range(&self) -> u32 {
        self.streams.iter().fold(0, |acc, s| acc ^ s.final_range())
    }

    /// Decodes one multistream packet into a fresh interleaved buffer.
    ///
    /// This is the shape the product's decoder loop uses; [`decode_float`] is
    /// the same work without the allocation.
    ///
    /// [`decode_float`]: MultistreamDecoder::decode_float
    pub fn decode_packet(&mut self, data: &[u8]) -> Result<Vec<f32>> {
        // A packet is at most 120 ms, and the sample rate at most 48 kHz.
        let mut out = vec![0.0; 5760 * self.channels * self.sample_rate as usize / 48000];
        let n = self.decode_float(data, &mut out)?;
        out.truncate(n * self.channels);
        Ok(out)
    }

    /// Decodes one multistream packet into `out`, returning samples per
    /// channel.
    pub fn decode_float(&mut self, data: &[u8], out: &mut [f32]) -> Result<usize> {
        // Every stream but the last needs at least a TOC and a length byte.
        if data.len() < 2 * self.streams.len() - 1 {
            return Err(Error::corrupt(format!(
                "opus multistream: {} bytes for {} streams",
                data.len(),
                self.streams.len()
            )));
        }
        let mut rest = data;
        let mut frame_size = 0usize;
        let nb_streams = self.streams.len();
        for s in 0..nb_streams {
            let last = s == nb_streams - 1;
            let stream_channels = if s < self.coupled { 2 } else { 1 };
            let need = 5760 * stream_channels;
            if self.buf.len() < need {
                self.buf.resize(need, 0.0);
            }
            let (n, consumed) = if last {
                (
                    self.streams[s].decode_float(rest, &mut self.buf)?,
                    rest.len(),
                )
            } else {
                self.streams[s].decode_self_delimited(rest, &mut self.buf)?
            };
            rest = &rest[consumed.min(rest.len())..];
            if s == 0 {
                frame_size = n;
                if out.len() < n * self.channels {
                    return Err(Error::corrupt(format!(
                        "opus multistream: output buffer holds {} samples, packet needs {}",
                        out.len(),
                        n * self.channels
                    )));
                }
            } else if n != frame_size {
                return Err(Error::corrupt(format!(
                    "opus multistream: stream {s} decoded {n} samples, stream 0 gave {frame_size}"
                )));
            }
            // Route this stream's channels to the outputs that name them.
            for (c, &m) in self.mapping.iter().enumerate() {
                if m == 255 {
                    continue;
                }
                let m = m as usize;
                let src = if s < self.coupled {
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
                if let Some(off) = src {
                    for i in 0..frame_size {
                        out[self.channels * i + c] = self.buf[stream_channels * i + off];
                    }
                }
            }
        }
        // Channels the mapping leaves unassigned are silent.
        for (c, &m) in self.mapping.iter().enumerate() {
            if m == 255 {
                for i in 0..frame_size {
                    out[self.channels * i + c] = 0.0;
                }
            }
        }
        Ok(frame_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_are_checked() {
        // RFC 7845 mapping family 1, 5.1: four streams, two of them coupled.
        assert!(MultistreamDecoder::try_with_rate(48000, 4, 2, &[0, 4, 1, 2, 3, 5]).is_ok());
        // More coupled streams than streams.
        assert!(MultistreamDecoder::try_with_rate(48000, 2, 3, &[0, 1]).is_err());
        // A mapping entry naming a channel no stream codes: 4 streams with 2
        // coupled code 6 channels, so 6 is out of range.
        assert!(MultistreamDecoder::try_with_rate(48000, 4, 2, &[0, 6]).is_err());
        // 255 is the silent channel and is always allowed.
        assert!(MultistreamDecoder::try_with_rate(48000, 1, 0, &[0, 255]).is_ok());
        assert!(MultistreamDecoder::try_with_rate(44100, 1, 0, &[0]).is_err());
    }

    #[test]
    fn truncated_packets_are_errors_not_panics() {
        let mut dec = MultistreamDecoder::with_rate(48000, 4, 2, &[0, 4, 1, 2, 3, 5]);
        for n in 0..40usize {
            let data: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            let _ = dec.decode_packet(&data);
        }
    }
}
