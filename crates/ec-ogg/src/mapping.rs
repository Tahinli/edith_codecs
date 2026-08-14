//! What a logical stream carries, and what its granule positions mean.
//!
//! Ogg itself says nothing about time: a granule position is an opaque integer
//! whose meaning belongs to the *mapping* named by the first packet. Vorbis and
//! FLAC count samples at the stream's own rate; Opus counts samples at 48 kHz
//! whatever the input rate was, and its count includes the pre-skip a decoder
//! throws away (RFC 7845 §4). Everything timing-related in this crate goes
//! through this module so those three conventions live in one place.

use ec_core::{ChannelLayout, CodecId, TimeBase};

/// A recognised logical-stream mapping, with what its identification header
/// stated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mapping {
    /// Vorbis I: three header packets, granule counts samples at `rate`.
    Vorbis {
        /// Sample rate from the identification header.
        rate: u32,
        /// Channel count from the identification header.
        channels: u8,
    },
    /// Opus (RFC 7845): `OpusHead` then `OpusTags`, granule counts 48 kHz
    /// samples and starts `pre_skip` above the first decoded sample.
    Opus {
        /// Output channel count.
        channels: u8,
        /// Samples (at 48 kHz) a decoder discards from the head of the stream.
        pre_skip: u16,
        /// Rate the encoder was fed, informational only — granules stay 48 kHz.
        input_rate: u32,
    },
    /// FLAC-in-Ogg: `\x7fFLAC` mapping header carrying `fLaC` + STREAMINFO,
    /// granule counts samples at `rate`.
    Flac {
        /// Sample rate from STREAMINFO.
        rate: u32,
        /// Channel count from STREAMINFO.
        channels: u8,
        /// Header packets this stream declares, the identification one included.
        headers: usize,
    },
}

impl Mapping {
    /// Recognise a mapping from the first packet of a logical stream, or
    /// [`None`] when nothing in the family claims it (Theora, Speex, Skeleton,
    /// a private mapping — all valid Ogg, none of them ours).
    pub fn identify(bos: &[u8]) -> Option<Mapping> {
        if bos.len() >= 16 && bos[0] == 1 && &bos[1..7] == b"vorbis" {
            return Some(Mapping::Vorbis {
                rate: u32::from_le_bytes(bos[12..16].try_into().ok()?),
                channels: bos[11],
            });
        }
        if bos.len() >= 19 && &bos[..8] == b"OpusHead" {
            return Some(Mapping::Opus {
                channels: bos[9],
                pre_skip: u16::from_le_bytes(bos[10..12].try_into().ok()?),
                input_rate: u32::from_le_bytes(bos[12..16].try_into().ok()?),
            });
        }
        if bos.len() >= 51 && bos[0] == 0x7f && &bos[1..5] == b"FLAC" && &bos[9..13] == b"fLaC" {
            // STREAMINFO starts after the mapping header and the metadata block
            // header; its rate is 20 bits, then 3 bits of channel count - 1.
            let si = &bos[17..51];
            let rate =
                (u32::from(si[10]) << 12) | (u32::from(si[11]) << 4) | u32::from(si[12] >> 4);
            let declared = usize::from(u16::from_be_bytes([bos[7], bos[8]]));
            return Some(Mapping::Flac {
                rate,
                channels: ((si[12] >> 1) & 0x07) + 1,
                // A zero here means "not stated"; the comment header still
                // follows, so two is the floor.
                headers: (declared + 1).max(2),
            });
        }
        None
    }

    /// Which codec decodes this stream's packets.
    pub fn codec(&self) -> CodecId {
        match self {
            Mapping::Vorbis { .. } => CodecId::Vorbis,
            Mapping::Opus { .. } => CodecId::Opus,
            Mapping::Flac { .. } => CodecId::Flac,
        }
    }

    /// The clock granule positions are counted in.
    pub fn time_base(&self) -> TimeBase {
        match self {
            // Opus granules are always 48 kHz, whatever the encoder was fed.
            Mapping::Opus { .. } => TimeBase::from_rate(48_000),
            Mapping::Vorbis { rate, .. } | Mapping::Flac { rate, .. } => {
                TimeBase::try_new(1, i64::from(*rate)).unwrap_or(TimeBase::from_rate(48_000))
            }
        }
    }

    /// Decoded sample rate.
    pub fn sample_rate(&self) -> u32 {
        match self {
            Mapping::Opus { .. } => 48_000,
            Mapping::Vorbis { rate, .. } | Mapping::Flac { rate, .. } => *rate,
        }
    }

    /// Channel layout, by count — Vorbis and Opus both order channels the way
    /// [`ChannelLayout`] does for the layouts it names.
    pub fn layout(&self) -> ChannelLayout {
        let channels = match self {
            Mapping::Vorbis { channels, .. }
            | Mapping::Opus { channels, .. }
            | Mapping::Flac { channels, .. } => *channels,
        };
        ChannelLayout::from_count(usize::from(channels).max(1))
    }

    /// How many packets belong to the stream's header, the identification
    /// packet included. Audio starts after them.
    pub fn header_packets(&self) -> usize {
        match self {
            Mapping::Vorbis { .. } => 3,
            Mapping::Opus { .. } => 2,
            Mapping::Flac { headers, .. } => *headers,
        }
    }

    /// Samples this packet decodes to, in granule units, when the mapping makes
    /// that cheap to know.
    ///
    /// Opus states it in the first byte of every packet, so its timing is exact
    /// packet by packet. Vorbis does not: the block size of a packet is a mode
    /// number whose meaning lives at the end of the setup header, past the full
    /// codebook parse, which belongs to the Vorbis decoder rather than here —
    /// so this answers [`None`] and the demuxer re-synchronises on every page
    /// granule instead (see [`crate::demux`]). FLAC frame headers do state a
    /// block size, but reading it means the frame parser, same argument.
    pub fn packet_duration(&self, data: &[u8]) -> Option<i64> {
        match self {
            Mapping::Opus { .. } => opus_packet_samples(data),
            Mapping::Vorbis { .. } | Mapping::Flac { .. } => None,
        }
    }
}

/// Samples at 48 kHz one Opus packet decodes to, from its TOC byte
/// (RFC 6716 §3.1): a configuration number picking the frame duration, and a
/// code picking how many frames are packed behind it.
pub fn opus_packet_samples(data: &[u8]) -> Option<i64> {
    let toc = *data.first()?;
    let config = usize::from(toc >> 3);
    // Frame duration in 48 kHz samples, per configuration block: SILK modes
    // step 10/20/40/60 ms, hybrid 10/20 ms, CELT 2.5/5/10/20 ms.
    let samples = match config {
        0..=11 => [480, 960, 1920, 2880][config % 4],
        12..=15 => [480, 960][config % 2],
        _ => [120, 240, 480, 960][config % 4],
    };
    let frames = match toc & 0x03 {
        0 => 1,
        1 | 2 => 2,
        // Code 3 states the count in the low six bits of the next byte.
        _ => i64::from(*data.get(1)? & 0x3f),
    };
    // RFC 6716 §3.1: a packet may not exceed 120 ms of audio.
    match frames * samples {
        n if n > 0 && n <= 5760 => Some(n),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_the_three_mappings() {
        let mut vorbis = vec![1u8];
        vorbis.extend_from_slice(b"vorbis");
        vorbis.extend_from_slice(&0u32.to_le_bytes()); // version
        vorbis.push(2); // channels
        vorbis.extend_from_slice(&44_100u32.to_le_bytes());
        vorbis.resize(30, 0);
        assert_eq!(
            Mapping::identify(&vorbis),
            Some(Mapping::Vorbis {
                rate: 44_100,
                channels: 2
            })
        );

        let mut opus = Vec::from(*b"OpusHead");
        opus.push(1); // version
        opus.push(6); // channels
        opus.extend_from_slice(&312u16.to_le_bytes()); // pre-skip
        opus.extend_from_slice(&48_000u32.to_le_bytes());
        opus.resize(19, 0);
        let mapping = Mapping::identify(&opus).unwrap();
        assert_eq!(
            mapping,
            Mapping::Opus {
                channels: 6,
                pre_skip: 312,
                input_rate: 48_000
            }
        );
        // Whatever the input rate said, granules count 48 kHz samples.
        assert_eq!(mapping.time_base(), TimeBase::from_rate(48_000));
        assert_eq!(mapping.layout(), ChannelLayout::Surround5_1);
        assert_eq!(mapping.header_packets(), 2);

        assert_eq!(Mapping::identify(b"not a header at all"), None);
        assert_eq!(Mapping::identify(&[]), None);
    }

    #[test]
    fn opus_durations_follow_the_toc() {
        // config 16 (CELT 2.5 ms), one frame.
        assert_eq!(opus_packet_samples(&[16 << 3]), Some(120));
        // config 31 (CELT 20 ms), one frame; the 960 that every 48 kHz encoder
        // in this family emits by default.
        assert_eq!(opus_packet_samples(&[31 << 3]), Some(960));
        // ...same config, code 1: two frames, so twice the samples.
        assert_eq!(opus_packet_samples(&[(31 << 3) | 1]), Some(1920));
        // Code 3 takes its frame count from the next byte, and 120 ms is the
        // ceiling the RFC sets — 7 frames of 20 ms is over it.
        assert_eq!(opus_packet_samples(&[(31 << 3) | 3, 6]), Some(5760));
        assert_eq!(opus_packet_samples(&[(31 << 3) | 3, 7]), None);
        assert_eq!(opus_packet_samples(&[(31 << 3) | 3, 0]), None);
        assert_eq!(opus_packet_samples(&[(31 << 3) | 3]), None);
        // SILK 60 ms narrowband.
        assert_eq!(opus_packet_samples(&[3 << 3]), Some(2880));
        assert_eq!(opus_packet_samples(&[]), None);
    }
}
