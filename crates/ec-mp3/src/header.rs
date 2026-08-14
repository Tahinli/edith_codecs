//! The four-byte MPEG audio frame header, its CRC, and the frame geometry
//! every other module reads off it.

use ec_core::error::{Error, Result};

/// Which MPEG audio generation a frame belongs to. The three differ in sample
/// rate range, granule count and side-info layout, not in the codec itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Version {
    /// MPEG-1 (ISO/IEC 11172-3): 32/44.1/48 kHz, two granules per frame.
    Mpeg1,
    /// MPEG-2 LSF (ISO/IEC 13818-3): 16/22.05/24 kHz, one granule.
    Mpeg2,
    /// MPEG-2.5, the de-facto extension to 8/11.025/12 kHz.
    Mpeg25,
}

/// The channel arrangement a frame declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelMode {
    /// Two independent channels coded as a stereo pair.
    Stereo,
    /// Stereo with mid/side and/or intensity coupling, per `mode_extension`.
    JointStereo,
    /// Two unrelated mono programmes in one stream.
    DualChannel,
    /// A single channel.
    Mono,
}

impl ChannelMode {
    /// Channels carried by the frame.
    pub fn channels(self) -> usize {
        match self {
            ChannelMode::Mono => 1,
            _ => 2,
        }
    }
}

const BITRATES_V1: [u32; 15] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
];
const BITRATES_V2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
const RATES_V1: [u32; 3] = [44100, 48000, 32000];

/// Everything the header declares, decoded into usable units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    /// MPEG generation.
    pub version: Version,
    /// Layer number, 1..=3. Only Layer III is decodable here.
    pub layer: u8,
    /// True when a 16-bit CRC follows the header.
    pub crc: bool,
    /// Declared bitrate in kbit/s; 0 means free format.
    pub bitrate_kbps: u32,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// True when the frame carries one extra byte.
    pub padding: bool,
    /// The private bit, carried through untouched.
    pub private: bool,
    /// Channel arrangement.
    pub mode: ChannelMode,
    /// Joint-stereo mode extension: bit 0 intensity, bit 1 mid/side.
    pub mode_ext: u8,
    /// Copyright flag.
    pub copyright: bool,
    /// Original-media flag.
    pub original: bool,
    /// De-emphasis selector, 0 = none.
    pub emphasis: u8,
}

impl FrameHeader {
    /// Parses a header from four bytes.
    ///
    /// Reserved encodings are refused rather than guessed at: a "bad" bitrate
    /// index, a reserved sample rate, layer 0 and the reserved MPEG version are
    /// all [`Error::Corrupt`], which is what lets [`crate::decode::Mp3Reader`]
    /// treat a failed parse as "this was not a frame start" and resync.
    pub fn parse(b: &[u8]) -> Result<FrameHeader> {
        if b.len() < 4 {
            return Err(Error::NeedMore);
        }
        if b[0] != 0xFF || (b[1] & 0xE0) != 0xE0 {
            return Err(Error::corrupt("mp3: no frame sync"));
        }
        let version = match (b[1] >> 3) & 3 {
            0 => Version::Mpeg25,
            2 => Version::Mpeg2,
            3 => Version::Mpeg1,
            _ => return Err(Error::corrupt("mp3: reserved MPEG version id")),
        };
        let layer = match (b[1] >> 1) & 3 {
            1 => 3,
            2 => 2,
            3 => 1,
            _ => return Err(Error::corrupt("mp3: reserved layer id")),
        };
        let crc = (b[1] & 1) == 0;
        let bitrate_index = (b[2] >> 4) as usize;
        if bitrate_index == 15 {
            return Err(Error::corrupt("mp3: bitrate index 15"));
        }
        let table = if version == Version::Mpeg1 {
            &BITRATES_V1
        } else {
            &BITRATES_V2
        };
        let bitrate_kbps = table[bitrate_index];
        let rate_index = ((b[2] >> 2) & 3) as usize;
        if rate_index == 3 {
            return Err(Error::corrupt("mp3: reserved sampling frequency"));
        }
        let sample_rate = match version {
            Version::Mpeg1 => RATES_V1[rate_index],
            Version::Mpeg2 => RATES_V1[rate_index] / 2,
            Version::Mpeg25 => RATES_V1[rate_index] / 4,
        };
        let mode = match (b[3] >> 6) & 3 {
            0 => ChannelMode::Stereo,
            1 => ChannelMode::JointStereo,
            2 => ChannelMode::DualChannel,
            _ => ChannelMode::Mono,
        };
        Ok(FrameHeader {
            version,
            layer,
            crc,
            bitrate_kbps,
            sample_rate,
            padding: (b[2] >> 1) & 1 == 1,
            private: b[2] & 1 == 1,
            mode,
            mode_ext: (b[3] >> 4) & 3,
            copyright: (b[3] >> 3) & 1 == 1,
            original: (b[3] >> 2) & 1 == 1,
            emphasis: b[3] & 3,
        })
    }

    /// The header as four bytes, for the encoder and for round-trip tests.
    pub fn to_bytes(&self) -> [u8; 4] {
        let version_id = match self.version {
            Version::Mpeg25 => 0,
            Version::Mpeg2 => 2,
            Version::Mpeg1 => 3,
        };
        let layer_id = match self.layer {
            3 => 1,
            2 => 2,
            _ => 3,
        };
        let table = if self.version == Version::Mpeg1 {
            &BITRATES_V1
        } else {
            &BITRATES_V2
        };
        let bitrate_index = table
            .iter()
            .position(|&k| k == self.bitrate_kbps)
            .unwrap_or(0) as u8;
        let base = match self.version {
            Version::Mpeg1 => self.sample_rate,
            Version::Mpeg2 => self.sample_rate * 2,
            Version::Mpeg25 => self.sample_rate * 4,
        };
        let rate_index = RATES_V1.iter().position(|&r| r == base).unwrap_or(0) as u8;
        let mode_id = match self.mode {
            ChannelMode::Stereo => 0,
            ChannelMode::JointStereo => 1,
            ChannelMode::DualChannel => 2,
            ChannelMode::Mono => 3,
        };
        [
            0xFF,
            0xE0 | (version_id << 3) | (layer_id << 1) | u8::from(!self.crc),
            (bitrate_index << 4)
                | (rate_index << 2)
                | (u8::from(self.padding) << 1)
                | u8::from(self.private),
            (mode_id << 6)
                | (self.mode_ext << 4)
                | (u8::from(self.copyright) << 3)
                | (u8::from(self.original) << 2)
                | self.emphasis,
        ]
    }

    /// Channels carried by this frame.
    pub fn channels(&self) -> usize {
        self.mode.channels()
    }

    /// Granules per frame: two for MPEG-1, one for the low sampling frequency
    /// extensions.
    pub fn granules(&self) -> usize {
        if self.version == Version::Mpeg1 { 2 } else { 1 }
    }

    /// PCM samples per channel produced by this frame.
    pub fn samples_per_frame(&self) -> usize {
        self.granules() * 576
    }

    /// Side-information bytes between the header (plus CRC) and the main data.
    pub fn side_info_len(&self) -> usize {
        match (self.version, self.channels()) {
            (Version::Mpeg1, 1) => 17,
            (Version::Mpeg1, _) => 32,
            (_, 1) => 9,
            (_, _) => 17,
        }
    }

    /// Total frame length in bytes, header included, or `None` for free format
    /// (where the length is only knowable by scanning to the next sync word).
    pub fn frame_len(&self) -> Option<usize> {
        if self.bitrate_kbps == 0 {
            return None;
        }
        let bits = self.bitrate_kbps as usize * 1000;
        let rate = self.sample_rate as usize;
        let pad = usize::from(self.padding);
        Some(match self.layer {
            1 => (12 * bits / rate + pad) * 4,
            2 => 144 * bits / rate + pad,
            _ if self.version == Version::Mpeg1 => 144 * bits / rate + pad,
            _ => 72 * bits / rate + pad,
        })
    }

    /// Bytes of main data this frame carries: everything after the header, the
    /// optional CRC and the side info.
    pub fn main_data_len(&self) -> Option<usize> {
        let len = self.frame_len()?;
        let overhead = 4 + usize::from(self.crc) * 2 + self.side_info_len();
        Some(len.saturating_sub(overhead))
    }

    /// True when two headers describe the same stream configuration — the test
    /// a resync uses to tell a real frame from a byte pattern that looked like
    /// one.
    pub fn same_stream(&self, other: &FrameHeader) -> bool {
        self.version == other.version
            && self.layer == other.layer
            && self.sample_rate == other.sample_rate
            && self.mode.channels() == other.mode.channels()
    }
}

/// CRC-16 as Layer III specifies it: polynomial 0x8005, seeded with all ones,
/// over the last two header bytes followed by the whole side info.
pub fn crc16(header: &[u8; 4], side_info: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;
    for &byte in header[2..].iter().chain(side_info) {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_rebuilds_a_128k_stereo_header() {
        let bytes = [0xFF, 0xFB, 0x90, 0x00];
        let h = FrameHeader::parse(&bytes).unwrap();
        assert_eq!(h.version, Version::Mpeg1);
        assert_eq!(h.layer, 3);
        assert_eq!(h.bitrate_kbps, 128);
        assert_eq!(h.sample_rate, 44100);
        assert_eq!(h.mode, ChannelMode::Stereo);
        assert_eq!(h.frame_len(), Some(417));
        assert_eq!(h.side_info_len(), 32);
        assert_eq!(h.to_bytes(), bytes);
    }

    #[test]
    fn low_sampling_frequency_geometry() {
        // MPEG-2, 24 kHz, 64 kbit/s, mono.
        let bytes = [0xFF, 0xF3, 0x84, 0xC0];
        let h = FrameHeader::parse(&bytes).unwrap();
        assert_eq!(h.version, Version::Mpeg2);
        assert_eq!(h.sample_rate, 24000);
        assert_eq!(h.granules(), 1);
        assert_eq!(h.samples_per_frame(), 576);
        assert_eq!(h.side_info_len(), 9);
        assert_eq!(h.frame_len(), Some(72 * 64000 / 24000));
        assert_eq!(h.to_bytes(), bytes);
    }

    #[test]
    fn reserved_fields_are_refused() {
        assert!(FrameHeader::parse(&[0xFF, 0xFB, 0xF0, 0x00]).is_err()); // bitrate 15
        assert!(FrameHeader::parse(&[0xFF, 0xFB, 0x9C, 0x00]).is_err()); // rate 3
        assert!(FrameHeader::parse(&[0xFF, 0xE9, 0x90, 0x00]).is_err()); // layer 0
        assert!(FrameHeader::parse(&[0x00, 0x00, 0x00, 0x00]).is_err()); // no sync
    }

    #[test]
    fn free_format_has_no_computable_length() {
        let h = FrameHeader::parse(&[0xFF, 0xFB, 0x00, 0x00]).unwrap();
        assert_eq!(h.bitrate_kbps, 0);
        assert_eq!(h.frame_len(), None);
    }
}
