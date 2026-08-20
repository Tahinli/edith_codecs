//! Apple Lossless (ALAC) decoding for the edith_codecs family.
//!
//! Two ways in:
//!
//! - [`AlacDecoder::decode`], one coded frame in, interleaved samples out, in
//!   the layout order the rest of the family uses (FL, FR, FC, LFE, BL, BR for
//!   5.1 — the stream's own order is centre-first).
//! - [`AlacDecoder`] as an [`ec_core::Decoder`], for a container that hands out
//!   one ALAC frame per packet, which is how mp4 and Matroska deliver them.
//!
//! The stream describes itself in a *magic cookie* ([`MagicCookie`]) that every
//! container carries as extradata: the frame length, the bit depth and the
//! three Golomb constants the residual coder adapts from.
//!
//! - [`AlacEncoder`], the mirror image: frames in, coded packets out, for a
//!   container that wants to write ALAC instead of read it.
//!
//! **Clean room.** Written from the published ALAC format description; Apple's
//! decoder sources were not read. Samples are returned shifted into their PCM
//! container the way ffmpeg's decoder returns them (16-bit as `s16`, 24-bit as
//! `s32` with the low 8 bits zero), so raw output compares byte for byte.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod decode;
mod encode;

pub use encode::AlacEncoder;

use ec_core::error::{Error, Result};
use ec_core::frame::{AudioFrame, ChannelLayout, Frame, SampleFormat};
use ec_core::packet::{Buf, Packet};
use ec_core::registry::{AudioParameters, CodecId, CodecParameters, Decoder, MediaParameters};

/// The `ALACSpecificConfig` every ALAC stream is introduced by: 24 bytes,
/// big-endian, carried as the body of an `alac` box in mp4 and as `CodecPrivate`
/// in Matroska.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MagicCookie {
    /// Samples per channel in a full frame (4096 from Apple's encoder). A
    /// stream's last frame states its own shorter length.
    pub frame_length: u32,
    /// Bitstream version the file is compatible with; 0 is the only one.
    pub compatible_version: u8,
    /// Bits per sample: 16, 20, 24 or 32.
    pub bit_depth: u8,
    /// Golomb mean update rate.
    pub pb: u8,
    /// Initial Golomb mean.
    pub mb: u8,
    /// Ceiling on the Rice parameter.
    pub kb: u8,
    /// Channels the stream carries.
    pub channels: u8,
    /// Longest zero run the encoder emits.
    pub max_run: u16,
    /// Largest coded frame in bytes, `0` when the encoder did not say.
    pub max_frame_bytes: u32,
    /// Nominal bit rate, `0` when the encoder did not say.
    pub avg_bit_rate: u32,
    /// Sample rate in Hz.
    pub sample_rate: u32,
}

impl MagicCookie {
    /// Parse a cookie from the bytes a container carries.
    ///
    /// Accepts the three shapes those bytes arrive in: the bare 24-byte config,
    /// the config behind a four-byte version/flags word, and the whole `alac`
    /// box with its size and type in front (which is what ffmpeg and this
    /// family's own mp4 demuxer hand over).
    pub fn parse(data: &[u8]) -> Result<MagicCookie> {
        let at = match data {
            [_, _, _, _, b'a', b'l', b'a', b'c', ..] if data.len() >= 36 => 12,
            _ if data.len() >= 28 && Self::at(data, 0).is_err() => 4,
            _ => 0,
        };
        Self::at(data, at)
    }

    fn at(data: &[u8], at: usize) -> Result<MagicCookie> {
        let c = data
            .get(at..at + 24)
            .ok_or_else(|| Error::corrupt("ALAC: a magic cookie shorter than 24 bytes"))?;
        let be32 = |i: usize| u32::from_be_bytes([c[i], c[i + 1], c[i + 2], c[i + 3]]);
        let cookie = MagicCookie {
            frame_length: be32(0),
            compatible_version: c[4],
            bit_depth: c[5],
            pb: c[6],
            mb: c[7],
            kb: c[8],
            channels: c[9],
            max_run: u16::from_be_bytes([c[10], c[11]]),
            max_frame_bytes: be32(12),
            avg_bit_rate: be32(16),
            sample_rate: be32(20),
        };
        // The three fields a decoder cannot proceed on, checked here so a
        // mis-sliced cookie is caught at open rather than as garbled audio.
        if !(1..=1 << 20).contains(&cookie.frame_length) {
            return Err(Error::corrupt(format!(
                "ALAC: a {}-sample frame length",
                cookie.frame_length
            )));
        }
        if !(1..=32).contains(&cookie.bit_depth) {
            return Err(Error::corrupt(format!(
                "ALAC: {}-bit samples",
                cookie.bit_depth
            )));
        }
        if !(1..=8).contains(&cookie.channels) {
            return Err(Error::corrupt(format!(
                "ALAC: {} channels",
                cookie.channels
            )));
        }
        Ok(cookie)
    }

    /// Bits the decoded samples are shifted left by to fill their PCM
    /// container, matching [`MagicCookie::sample_format`].
    pub fn container_shift(&self) -> u32 {
        match u32::from(self.bit_depth) <= 16 {
            true => 16 - u32::from(self.bit_depth),
            false => 32 - u32::from(self.bit_depth),
        }
    }

    /// The sample format decoded audio is delivered in.
    pub fn sample_format(&self) -> SampleFormat {
        match self.bit_depth <= 16 {
            true => SampleFormat::S16,
            false => SampleFormat::S32,
        }
    }

    /// The bare 24-byte `ALACSpecificConfig`, big-endian.
    pub fn to_bytes(&self) -> [u8; 24] {
        let mut b = [0u8; 24];
        b[0..4].copy_from_slice(&self.frame_length.to_be_bytes());
        b[4] = self.compatible_version;
        b[5] = self.bit_depth;
        b[6] = self.pb;
        b[7] = self.mb;
        b[8] = self.kb;
        b[9] = self.channels;
        b[10..12].copy_from_slice(&self.max_run.to_be_bytes());
        b[12..16].copy_from_slice(&self.max_frame_bytes.to_be_bytes());
        b[16..20].copy_from_slice(&self.avg_bit_rate.to_be_bytes());
        b[20..24].copy_from_slice(&self.sample_rate.to_be_bytes());
        b
    }

    /// The whole `alac` box a container's sample entry carries: a 12-byte
    /// full-box header (size, `alac`, version/flags) in front of
    /// [`MagicCookie::to_bytes`], which is the shape `ec_mp4`'s muxer expects
    /// as a track's `extradata`.
    pub fn extradata_box(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(36);
        out.extend_from_slice(&36u32.to_be_bytes());
        out.extend_from_slice(b"alac");
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&self.to_bytes());
        out
    }
}

/// Where each output channel sits in the stream's own coded order.
///
/// ALAC codes centre first (C, L, R, Ls, Rs, LFE for 5.1) because its elements
/// are AAC's; every consumer in this family wants film order (FL, FR, FC, LFE,
/// BL, BR), so the map is applied once, on the way out.
fn film_order(channels: usize) -> &'static [usize] {
    match channels {
        3 => &[1, 2, 0],
        4 => &[1, 2, 0, 3],
        5 => &[1, 2, 0, 3, 4],
        6 => &[1, 2, 0, 5, 3, 4],
        7 => &[1, 2, 0, 6, 5, 3, 4],
        8 => &[3, 4, 0, 7, 5, 6, 1, 2],
        // Mono and stereo are already in it.
        _ => &[0, 1],
    }
}

/// An ALAC decoder for one stream.
#[derive(Debug)]
pub struct AlacDecoder {
    cookie: MagicCookie,
    params: CodecParameters,
    scratch: decode::Scratch,
    coded: Vec<i32>,
    out: Vec<i32>,
    pending: Option<AudioFrame>,
    drained: bool,
}

impl AlacDecoder {
    /// A decoder for a stream described by `cookie`.
    pub fn new(cookie: MagicCookie) -> AlacDecoder {
        AlacDecoder {
            params: codec_parameters(&cookie),
            cookie,
            scratch: decode::Scratch::default(),
            coded: Vec::new(),
            out: Vec::new(),
            pending: None,
            drained: false,
        }
    }

    /// A decoder for a stream whose `extradata` is its magic cookie, which is
    /// how every container states one.
    pub fn from_parameters(params: CodecParameters) -> Result<AlacDecoder> {
        let cookie = MagicCookie::parse(params.extradata.as_deref().ok_or_else(|| {
            Error::corrupt("ALAC: a track with no magic cookie in its extradata")
        })?)?;
        let mut decoder = AlacDecoder::new(cookie);
        decoder.params.extradata = params.extradata;
        Ok(decoder)
    }

    /// What the stream said about itself.
    pub fn cookie(&self) -> &MagicCookie {
        &self.cookie
    }

    /// One coded frame in, interleaved samples out in film order, already
    /// shifted into their PCM container.
    ///
    /// The slice is valid until the next call; a caller that keeps it copies.
    pub fn decode(&mut self, data: &[u8]) -> Result<&[i32]> {
        let channels = usize::from(self.cookie.channels.max(1));
        let samples = decode::frame(&self.cookie, &mut self.scratch, data, &mut self.coded)?;
        let shift = self.cookie.container_shift();
        let map = film_order(channels);
        self.out.clear();
        self.out.reserve(samples * channels);
        for frame in self.coded.chunks_exact(channels).take(samples) {
            for &src in &map[..channels] {
                self.out.push(frame[src] << shift);
            }
        }
        Ok(&self.out)
    }
}

impl Decoder for AlacDecoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        self.decode(&packet.data)?;
        let channels = usize::from(self.cookie.channels.max(1));
        let format = self.cookie.sample_format();
        let mut bytes = Vec::with_capacity(self.out.len() * format.bytes_per_sample());
        for &s in &self.out {
            match format {
                SampleFormat::S16 => bytes.extend_from_slice(&(s as i16).to_ne_bytes()),
                _ => bytes.extend_from_slice(&s.to_ne_bytes()),
            }
        }
        let mut frame = AudioFrame::try_new(
            format,
            false,
            ChannelLayout::from_count(channels),
            self.cookie.sample_rate,
            self.out.len() / channels,
            vec![Buf::from_vec(bytes)],
        )?;
        frame.pts = packet
            .pts
            .map(|ticks| ec_core::timebase::Timestamp::new(ticks, packet.time_base));
        self.pending = Some(frame);
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        match self.pending.take() {
            Some(frame) => Ok(Frame::Audio(frame)),
            None if self.drained => Err(Error::Eof),
            None => Err(Error::NeedMore),
        }
    }

    fn flush(&mut self) -> Result<()> {
        self.drained = true;
        Ok(())
    }

    fn reset(&mut self) {
        self.pending = None;
        self.drained = false;
    }
}

/// Codec parameters for the stream a cookie describes.
pub fn codec_parameters(cookie: &MagicCookie) -> CodecParameters {
    let mut params = CodecParameters::new(CodecId::Alac);
    params.media = MediaParameters::Audio(AudioParameters {
        sample_rate: cookie.sample_rate,
        layout: ChannelLayout::from_count(usize::from(cookie.channels.max(1))),
        format: Some(cookie.sample_format()),
        bits_per_sample: Some(u32::from(cookie.bit_depth)),
    });
    params
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ffmpeg's 36-byte `alac` box, a bare 24-byte config and the 28-byte form
    /// in between all describe the same stream.
    #[test]
    fn a_magic_cookie_is_read_in_every_shape_a_container_states_it() {
        let config: [u8; 24] = [
            0, 0, 0x10, 0, // frameLength 4096
            0, 16, // version 0, 16-bit
            40, 10, 14, // pb, mb, kb
            2,  // channels
            0, 255, // maxRun
            0, 0, 0x10, 0, // maxFrameBytes
            0, 0, 0, 0, // avgBitRate
            0, 0, 0xAC, 0x44, // 44100
        ];
        let bare = MagicCookie::parse(&config).expect("bare config");
        assert_eq!(bare.frame_length, 4096);
        assert_eq!(bare.bit_depth, 16);
        assert_eq!(bare.channels, 2);
        assert_eq!(bare.sample_rate, 44100);
        assert_eq!(bare.pb, 40);
        assert_eq!(bare.mb, 10);
        assert_eq!(bare.kb, 14);

        let mut boxed = vec![0, 0, 0, 36, b'a', b'l', b'a', b'c', 0, 0, 0, 0];
        boxed.extend_from_slice(&config);
        assert_eq!(MagicCookie::parse(&boxed).expect("boxed"), bare);

        let mut versioned = vec![0, 0, 0, 0];
        versioned.extend_from_slice(&config);
        assert_eq!(MagicCookie::parse(&versioned).expect("versioned"), bare);

        // A refusal that names what it refused, rather than decoding noise.
        assert!(MagicCookie::parse(&config[..12]).is_err());
    }

    /// The layout map is a permutation for every width, which is the property
    /// that keeps a channel from being dropped or doubled on the way out.
    #[test]
    fn film_order_is_a_permutation_at_every_width() {
        for channels in 1..=8usize {
            let map = film_order(channels);
            let mut seen: Vec<usize> = map[..channels].to_vec();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), channels, "{channels} channels");
            assert!(seen.iter().all(|&c| c < channels), "{channels} channels");
        }
        // 5.1: the stream's centre channel is coded first and lands third.
        assert_eq!(film_order(6), &[1, 2, 0, 5, 3, 4]);
    }
}
