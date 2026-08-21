//! FLAC decoding: metadata blocks, frame headers, all four subframe kinds and
//! the three stereo decorrelations, per RFC 9639.
//!
//! Every parse routine takes a borrowed buffer and answers with
//! [`ec_core::Error`]: truncated input is `NeedMore`, a stream that breaks its
//! own rules is `Corrupt`. Nothing here panics on input — the only asserts are
//! in `ec_core::bitio`, and they fire on API misuse (`n > 64`), never on bytes.

use ec_core::bitio::BitReader;
use ec_core::error::{Error, Result};

use crate::checksum::{crc8, crc16};

/// The four bytes every FLAC stream starts with.
pub const MAGIC: [u8; 4] = *b"fLaC";

/// Largest block size RFC 9639 can express (16-bit `block_size - 1`).
pub const MAX_BLOCK_SIZE: usize = 65536;

/// The `STREAMINFO` metadata block: everything a player needs before the first
/// frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    /// Smallest block size in the stream, in inter-channel samples.
    pub min_block_size: u16,
    /// Largest block size in the stream. Equal to `min_block_size` for a
    /// fixed-block-size stream.
    pub max_block_size: u16,
    /// Smallest frame size in bytes; 0 when the encoder did not state it.
    pub min_frame_size: u32,
    /// Largest frame size in bytes; 0 when the encoder did not state it.
    pub max_frame_size: u32,
    /// Sample rate in Hz. 0 means "non-audio stream" and is refused here.
    pub sample_rate: u32,
    /// Channel count, 1..=8.
    pub channels: u8,
    /// Bits per sample, 4..=32.
    pub bits_per_sample: u8,
    /// Total inter-channel samples, or 0 when unknown.
    pub total_samples: u64,
    /// MD5 of the unencoded audio; all zero when the encoder did not compute it.
    pub md5: [u8; 16],
}

impl StreamInfo {
    /// Parse the 34-byte `STREAMINFO` payload.
    pub fn parse(data: &[u8]) -> Result<StreamInfo> {
        if data.len() < 34 {
            return Err(Error::NeedMore);
        }
        let mut r = BitReader::new(data);
        let info = StreamInfo {
            min_block_size: r.read_bits(16)? as u16,
            max_block_size: r.read_bits(16)? as u16,
            min_frame_size: r.read_bits(24)?,
            max_frame_size: r.read_bits(24)?,
            sample_rate: r.read_bits(20)?,
            channels: (r.read_bits(3)? + 1) as u8,
            bits_per_sample: (r.read_bits(5)? + 1) as u8,
            total_samples: r.read_bits64(36)?,
            md5: data[18..34].try_into().expect("18..34 is 16 bytes"),
        };
        if info.sample_rate == 0 {
            return Err(Error::corrupt("STREAMINFO: sample rate 0"));
        }
        if info.bits_per_sample < 4 {
            return Err(Error::corrupt(format!(
                "STREAMINFO: {} bits per sample",
                info.bits_per_sample
            )));
        }
        if info.min_block_size < 16 || info.max_block_size < 16 {
            return Err(Error::corrupt(format!(
                "STREAMINFO: block sizes {}..{}",
                info.min_block_size, info.max_block_size
            )));
        }
        Ok(info)
    }

    /// The 34-byte `STREAMINFO` payload.
    pub fn to_bytes(&self) -> [u8; 34] {
        let mut out = [0u8; 34];
        out[0..2].copy_from_slice(&self.min_block_size.to_be_bytes());
        out[2..4].copy_from_slice(&self.max_block_size.to_be_bytes());
        out[4..7].copy_from_slice(&self.min_frame_size.to_be_bytes()[1..]);
        out[7..10].copy_from_slice(&self.max_frame_size.to_be_bytes()[1..]);
        let packed = (u64::from(self.sample_rate) << 44)
            | (u64::from(self.channels - 1) << 41)
            | (u64::from(self.bits_per_sample - 1) << 36)
            | (self.total_samples & 0xf_ffff_ffff);
        out[10..18].copy_from_slice(&packed.to_be_bytes());
        out[18..34].copy_from_slice(&self.md5);
        out
    }
}

/// One entry of the `SEEKTABLE` block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeekPoint {
    /// First sample of the target frame.
    pub sample: u64,
    /// Byte offset of that frame from the first byte of the first frame.
    pub offset: u64,
    /// Samples in the target frame.
    pub frame_samples: u16,
}

/// How the channels of one frame were decorrelated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelAssignment {
    /// `n` channels, each coded on its own.
    Independent(u8),
    /// Channel 0 is left, channel 1 is `left - right`.
    LeftSide,
    /// Channel 0 is `left - right`, channel 1 is right.
    RightSide,
    /// Channel 0 is `(left + right) >> 1`, channel 1 is `left - right`.
    MidSide,
}

impl ChannelAssignment {
    /// Channels coded in the frame.
    pub fn channel_count(&self) -> usize {
        match self {
            ChannelAssignment::Independent(n) => *n as usize,
            _ => 2,
        }
    }
}

/// A parsed frame header, with the `STREAMINFO` fallbacks already resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Inter-channel samples in this frame.
    pub block_size: usize,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Channel coding of this frame.
    pub channels: ChannelAssignment,
    /// Bits per sample of the *decoded* audio.
    pub bits_per_sample: u32,
    /// Frame number for a fixed-block-size stream, first sample number for a
    /// variable-block-size one.
    pub number: u64,
    /// True when the stream declares variable block sizes.
    pub variable_block_size: bool,
}

impl FrameHeader {
    /// First inter-channel sample of this frame, when it can be known: a
    /// variable-block-size stream states it, a fixed one implies it from the
    /// frame number and the block size the stream was written with.
    pub fn first_sample(&self, stream_block_size: Option<usize>) -> Option<u64> {
        match self.variable_block_size {
            true => Some(self.number),
            false => stream_block_size.map(|bs| self.number * bs as u64),
        }
    }
}

/// A decoded block: one `Vec<i32>` per channel, samples at the frame's own
/// bit depth (not shifted).
#[derive(Debug, Clone, Default)]
pub struct Block {
    /// The header the samples came from.
    pub header: Option<FrameHeader>,
    /// Per-channel samples, already un-decorrelated.
    pub channels: Vec<Vec<i32>>,
}

impl Block {
    /// Samples per channel.
    pub fn len(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    /// True when no samples are held.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The block interleaved into one buffer, samples shifted left so they fill
    /// a 16- or 32-bit container the way the oracle and every mixer expect
    /// (`s16` for depths up to 16, `s32` above).
    pub fn to_interleaved(&self, shift: u32) -> Vec<i32> {
        let channels = self.channels.len();
        let len = self.len();
        let mut out = vec![0i32; len * channels];
        for (c, plane) in self.channels.iter().enumerate() {
            for (i, &s) in plane.iter().enumerate() {
                out[i * channels + c] = s << shift;
            }
        }
        out
    }
}

/// Bits the samples of a `bits_per_sample` stream are shifted left by to fill
/// their PCM container — the convention the oracle's decoder uses, so raw output
/// compares byte for byte.
pub fn container_shift(bits_per_sample: u32) -> u32 {
    match bits_per_sample <= 16 {
        true => 16 - bits_per_sample,
        false => 32 - bits_per_sample,
    }
}

/// A FLAC stream over a complete in-memory buffer.
///
/// Metadata is parsed on construction; frames are decoded one call at a time so
/// a caller can stop, seek and resume without decoding what it will discard.
#[derive(Debug)]
pub struct FlacReader<'a> {
    data: &'a [u8],
    stream_info: Option<StreamInfo>,
    seek_table: Vec<SeekPoint>,
    first_frame: usize,
    pos: usize,
    residual: Vec<i32>,
}

impl<'a> FlacReader<'a> {
    /// Open a stream: read the magic and metadata blocks, then stop at the
    /// first frame.
    ///
    /// A buffer that does not start with `fLaC` is scanned for a frame sync
    /// instead — some tools hand out FLAC that starts mid-stream, and the frame
    /// headers carry enough to decode without `STREAMINFO`.
    pub fn new(data: &'a [u8]) -> Result<FlacReader<'a>> {
        let mut reader = FlacReader {
            data,
            stream_info: None,
            seek_table: Vec::new(),
            first_frame: 0,
            pos: 0,
            residual: Vec::new(),
        };
        if data.len() >= 4 && data[..4] == MAGIC {
            reader.read_metadata(4)?;
        } else {
            reader.first_frame = find_frame_sync(data, 0, None)
                .ok_or_else(|| Error::corrupt("no fLaC magic and no frame sync"))?;
            reader.pos = reader.first_frame;
        }
        Ok(reader)
    }

    /// A reader over bare frames — what a container hands out, one packet at a
    /// time, with the `STREAMINFO` it carried in its own headers.
    pub fn frames(data: &'a [u8], info: Option<StreamInfo>) -> Result<FlacReader<'a>> {
        let first_frame = find_frame_sync(data, 0, info.as_ref())
            .ok_or_else(|| Error::corrupt("no FLAC frame sync in this packet"))?;
        Ok(FlacReader {
            data,
            stream_info: info,
            seek_table: Vec::new(),
            first_frame,
            pos: first_frame,
            residual: Vec::new(),
        })
    }

    fn read_metadata(&mut self, mut at: usize) -> Result<()> {
        let mut first = true;
        loop {
            let header = self.data.get(at..at + 4).ok_or(Error::NeedMore)?;
            let last = header[0] & 0x80 != 0;
            let kind = header[0] & 0x7f;
            let len = u32::from_be_bytes([0, header[1], header[2], header[3]]) as usize;
            let body = self.data.get(at + 4..at + 4 + len).ok_or(Error::NeedMore)?;
            if first && kind != 0 {
                return Err(Error::corrupt(format!(
                    "first metadata block is type {kind}, not STREAMINFO"
                )));
            }
            match kind {
                0 => self.stream_info = Some(StreamInfo::parse(body)?),
                3 => {
                    self.seek_table = body
                        .chunks_exact(18)
                        .map(|p| SeekPoint {
                            sample: u64::from_be_bytes(p[0..8].try_into().expect("8 bytes")),
                            offset: u64::from_be_bytes(p[8..16].try_into().expect("8 bytes")),
                            frame_samples: u16::from_be_bytes(
                                p[16..18].try_into().expect("2 bytes"),
                            ),
                        })
                        // A placeholder point (sample == u64::MAX) indexes nothing.
                        .filter(|p| p.sample != u64::MAX)
                        .collect();
                }
                127 => return Err(Error::corrupt("metadata block type 127 is invalid")),
                _ => {}
            }
            first = false;
            at += 4 + len;
            if last {
                break;
            }
        }
        self.first_frame = at;
        self.pos = at;
        Ok(())
    }

    /// The `STREAMINFO` block, when the stream had one.
    pub fn stream_info(&self) -> Option<&StreamInfo> {
        self.stream_info.as_ref()
    }

    /// The seek table, empty when the stream carries none.
    pub fn seek_table(&self) -> &[SeekPoint] {
        &self.seek_table
    }

    /// Byte offset of the next frame to be decoded.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Position the reader at the last indexed frame at or before `sample` and
    /// answer that frame's first sample; the caller decodes forward and
    /// discards the difference (`SeekMode::SyncBefore`).
    ///
    /// Without a seek table the answer is the start of the stream: FLAC frames
    /// are not self-indexing, so a blind scan would cost the same as decoding.
    pub fn seek_to_sample(&mut self, sample: u64) -> u64 {
        let point = self
            .seek_table
            .iter()
            .filter(|p| p.sample <= sample)
            .max_by_key(|p| p.sample);
        match point {
            Some(p) if self.first_frame + p.offset as usize <= self.data.len() => {
                self.pos = self.first_frame + p.offset as usize;
                p.sample
            }
            _ => {
                self.pos = self.first_frame;
                0
            }
        }
    }

    /// Decode the next frame into `block`, reusing its buffers.
    ///
    /// Answers `false` at the end of the stream. Trailing bytes that are not a
    /// frame (an ID3v1 tag, padding a container left) end the stream rather
    /// than failing it; a frame that starts well and then breaks is `Corrupt`.
    pub fn next_block(&mut self, block: &mut Block) -> Result<bool> {
        if self.pos >= self.data.len() {
            return Ok(false);
        }
        let start = match find_frame_sync(self.data, self.pos, self.stream_info.as_ref()) {
            Some(at) => at,
            None => {
                self.pos = self.data.len();
                return Ok(false);
            }
        };
        self.pos = start;
        let mut r = BitReader::new(&self.data[start..]);
        let header = parse_frame_header(&mut r, self.stream_info.as_ref())?;
        let header_bytes = (r.bit_position() / 8) as usize;
        let want = crc8(&self.data[start..start + header_bytes - 1]);
        if want != self.data[start + header_bytes - 1] {
            return Err(Error::corrupt(format!(
                "frame at {start}: header CRC-8 {:#04x}, expected {want:#04x}",
                self.data[start + header_bytes - 1]
            )));
        }

        let coded = header.channels.channel_count();
        block.channels.resize_with(coded, Vec::new);
        for (c, plane) in block.channels.iter_mut().enumerate() {
            let side = matches!(
                (header.channels, c),
                (ChannelAssignment::LeftSide, 1)
                    | (ChannelAssignment::MidSide, 1)
                    | (ChannelAssignment::RightSide, 0)
            );
            let bps = header.bits_per_sample + u32::from(side);
            decode_subframe(&mut r, bps, header.block_size, plane, &mut self.residual)?;
        }
        r.align_to_byte();
        let frame_bytes = (r.bit_position() / 8) as usize;
        let stated = r.read_bits(16)? as u16;
        let want = crc16(&self.data[start..start + frame_bytes]);
        if stated != want {
            return Err(Error::corrupt(format!(
                "frame at {start}: CRC-16 {stated:#06x}, expected {want:#06x}"
            )));
        }
        undecorrelate(header.channels, &mut block.channels)?;
        block.header = Some(header);
        self.pos = start + frame_bytes + 2;
        Ok(true)
    }

    /// Decode the whole stream into per-channel planes, verifying the
    /// `STREAMINFO` MD5 when the stream states one.
    ///
    /// Convenience over [`FlacReader::next_block`]: it holds the whole decoded
    /// stream in memory, which is what a fixture test and a waveform pass want
    /// and what a player does not.
    pub fn decode_all(&mut self) -> Result<DecodedStream> {
        let mut block = Block::default();
        let mut out: Vec<Vec<i32>> = Vec::new();
        let mut header: Option<FrameHeader> = None;
        while self.next_block(&mut block)? {
            let h = block.header.expect("a decoded block has a header");
            match header {
                None => {
                    out.resize_with(block.channels.len(), Vec::new);
                    header = Some(h);
                }
                Some(prev)
                    if prev.bits_per_sample != h.bits_per_sample
                        || prev.sample_rate != h.sample_rate
                        || prev.channels.channel_count() != h.channels.channel_count() =>
                {
                    return Err(Error::unsupported(
                        "FLAC stream changing format mid-stream",
                        format!(
                            "{}ch/{}bit/{}Hz became {}ch/{}bit/{}Hz",
                            prev.channels.channel_count(),
                            prev.bits_per_sample,
                            prev.sample_rate,
                            h.channels.channel_count(),
                            h.bits_per_sample,
                            h.sample_rate
                        ),
                    ));
                }
                Some(_) => {}
            }
            for (dst, src) in out.iter_mut().zip(&block.channels) {
                dst.extend_from_slice(src);
            }
        }
        let header = header.ok_or(Error::Eof)?;
        Ok(DecodedStream {
            bits_per_sample: header.bits_per_sample,
            sample_rate: header.sample_rate,
            channels: out,
        })
    }
}

/// The result of [`FlacReader::decode_all`].
#[derive(Debug, Clone)]
pub struct DecodedStream {
    /// Bit depth of the samples.
    pub bits_per_sample: u32,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// One plane per channel, samples at the stream's own bit depth.
    pub channels: Vec<Vec<i32>>,
}

impl DecodedStream {
    /// Samples per channel.
    pub fn len(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    /// True when nothing decoded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Interleaved samples at the stream's own bit depth.
    pub fn interleaved(&self) -> Vec<i32> {
        let channels = self.channels.len();
        let mut out = vec![0i32; self.len() * channels];
        for (c, plane) in self.channels.iter().enumerate() {
            for (i, &s) in plane.iter().enumerate() {
                out[i * channels + c] = s;
            }
        }
        out
    }

    /// Interleaved samples shifted into their PCM container, as raw bytes:
    /// `s16le` for depths up to 16 bits, `s32le` above. This is exactly what
    /// `the oracle -f s16le/-f s32le` writes for the same file.
    pub fn to_pcm_bytes(&self) -> Vec<u8> {
        let shift = container_shift(self.bits_per_sample);
        let narrow = self.bits_per_sample <= 16;
        let mut out = Vec::with_capacity(self.len() * self.channels.len() * 4);
        for i in 0..self.len() {
            for plane in &self.channels {
                let v = plane[i] << shift;
                match narrow {
                    true => out.extend_from_slice(&(v as i16).to_le_bytes()),
                    false => out.extend_from_slice(&v.to_le_bytes()),
                }
            }
        }
        out
    }
}

/// Byte offset of the next plausible frame sync at or after `from`.
///
/// "Plausible" is: the 14-bit sync code, a zero reserved bit, and a header that
/// both parses and matches its own CRC-8. That is what makes resynchronisation
/// after damage, and opening a buffer that starts mid-stream, safe.
pub fn find_frame_sync(data: &[u8], from: usize, info: Option<&StreamInfo>) -> Option<usize> {
    let mut at = from;
    while at + 2 <= data.len() {
        if data[at] == 0xff && data[at + 1] & 0xfe == 0xf8 {
            let mut r = BitReader::new(&data[at..]);
            if let Ok(_header) = parse_frame_header(&mut r, info) {
                let n = (r.bit_position() / 8) as usize;
                if at + n <= data.len() && crc8(&data[at..at + n - 1]) == data[at + n - 1] {
                    return Some(at);
                }
            }
        }
        at += 1;
    }
    None
}

/// Parse a frame header, resolving the "same as `STREAMINFO`" codes.
pub fn parse_frame_header(r: &mut BitReader<'_>, info: Option<&StreamInfo>) -> Result<FrameHeader> {
    if r.read_bits(14)? != 0x3ffe {
        return Err(Error::corrupt("frame header: bad sync code"));
    }
    if r.read_bit()? {
        return Err(Error::corrupt("frame header: reserved bit set"));
    }
    let variable_block_size = r.read_bit()?;
    let block_size_code = r.read_bits(4)?;
    let sample_rate_code = r.read_bits(4)?;
    let channel_code = r.read_bits(4)?;
    let bps_code = r.read_bits(3)?;
    if r.read_bit()? {
        return Err(Error::corrupt("frame header: second reserved bit set"));
    }

    let number = read_utf8_number(r, if variable_block_size { 36 } else { 31 })?;

    let block_size = match block_size_code {
        0 => return Err(Error::corrupt("frame header: block size code 0")),
        1 => 192,
        2..=5 => 576 << (block_size_code - 2),
        6 => r.read_bits(8)? as usize + 1,
        7 => r.read_bits(16)? as usize + 1,
        _ => 256 << (block_size_code - 8),
    };
    let sample_rate = match sample_rate_code {
        0 => info
            .map(|i| i.sample_rate)
            .ok_or_else(|| Error::corrupt("frame header: rate from a missing STREAMINFO"))?,
        1 => 88200,
        2 => 176_400,
        3 => 192_000,
        4 => 8000,
        5 => 16000,
        6 => 22050,
        7 => 24000,
        8 => 32000,
        9 => 44100,
        10 => 48000,
        11 => 96000,
        12 => r.read_bits(8)? * 1000,
        13 => r.read_bits(16)?,
        14 => r.read_bits(16)? * 10,
        _ => return Err(Error::corrupt("frame header: sample rate code 15")),
    };
    let channels = match channel_code {
        0..=7 => ChannelAssignment::Independent(channel_code as u8 + 1),
        8 => ChannelAssignment::LeftSide,
        9 => ChannelAssignment::RightSide,
        10 => ChannelAssignment::MidSide,
        _ => {
            return Err(Error::corrupt(format!(
                "frame header: channel assignment {channel_code}"
            )));
        }
    };
    let bits_per_sample = match bps_code {
        0 => info
            .map(|i| u32::from(i.bits_per_sample))
            .ok_or_else(|| Error::corrupt("frame header: depth from a missing STREAMINFO"))?,
        1 => 8,
        2 => 12,
        3 => return Err(Error::corrupt("frame header: bit depth code 3 is reserved")),
        4 => 16,
        5 => 20,
        6 => 24,
        _ => 32,
    };
    // The CRC-8 byte closes the header; callers check it against the bytes they
    // hold, so it is only skipped here.
    r.skip_bits(8)?;
    Ok(FrameHeader {
        block_size,
        sample_rate,
        channels,
        bits_per_sample,
        number,
        variable_block_size,
    })
}

/// The UTF-8-shaped coded number of a frame header (up to 36 bits, so wider
/// than real UTF-8).
pub(crate) fn read_utf8_number(r: &mut BitReader<'_>, max_bits: u32) -> Result<u64> {
    let first = r.read_bits(8)?;
    let extra = match first {
        0x00..=0x7f => 0,
        0xc0..=0xdf => 1,
        0xe0..=0xef => 2,
        0xf0..=0xf7 => 3,
        0xf8..=0xfb => 4,
        0xfc..=0xfd => 5,
        0xfe => 6,
        _ => return Err(Error::corrupt("frame header: bad coded-number prefix")),
    };
    // The lead byte carries `6 - extra` payload bits after its prefix.
    let mut value = match extra {
        0 => u64::from(first),
        _ => u64::from(first & ((1u32 << (6 - extra)) - 1)),
    };
    for _ in 0..extra {
        let byte = r.read_bits(8)?;
        if byte & 0xc0 != 0x80 {
            return Err(Error::corrupt(
                "frame header: bad coded-number continuation",
            ));
        }
        value = (value << 6) | u64::from(byte & 0x3f);
    }
    if max_bits < 64 && value >> max_bits != 0 {
        return Err(Error::corrupt(format!(
            "frame header: coded number {value} exceeds {max_bits} bits"
        )));
    }
    Ok(value)
}

fn decode_subframe(
    r: &mut BitReader<'_>,
    bps: u32,
    block_size: usize,
    out: &mut Vec<i32>,
    residual: &mut Vec<i32>,
) -> Result<()> {
    if r.read_bit()? {
        return Err(Error::corrupt("subframe: padding bit set"));
    }
    let kind = r.read_bits(6)?;
    let mut wasted = 0u32;
    if r.read_bit()? {
        wasted = 1;
        while !r.read_bit()? {
            wasted += 1;
            if wasted >= bps {
                break;
            }
        }
    }
    if wasted >= bps {
        return Err(Error::corrupt(format!(
            "subframe: {wasted} wasted bits of {bps}"
        )));
    }
    let bps = bps - wasted;
    if bps > 32 {
        // Only a 32-bit stream that also decorrelates its stereo pair gets
        // here: its side channel needs 33 bits, which does not fit the i32
        // sample planes this decoder (and the oracle's, and the reference encoder's encoder)
        // works in. Named rather than mangled.
        return Err(Error::unsupported(
            "33-bit side subframe",
            "32-bit stereo decorrelation needs 33-bit sample planes",
        ));
    }

    out.clear();
    out.reserve(block_size);
    match kind {
        0 => {
            let value = r.read_signed(bps)?;
            out.resize(block_size, value);
        }
        1 => {
            for _ in 0..block_size {
                out.push(r.read_signed(bps)?);
            }
        }
        8..=12 => decode_fixed(r, (kind - 8) as usize, bps, block_size, out, residual)?,
        32..=63 => decode_lpc(r, (kind - 31) as usize, bps, block_size, out, residual)?,
        _ => {
            return Err(Error::corrupt(format!("subframe: type {kind} is reserved")));
        }
    }
    if wasted > 0 {
        for s in out.iter_mut() {
            *s <<= wasted;
        }
    }
    Ok(())
}

fn decode_fixed(
    r: &mut BitReader<'_>,
    order: usize,
    bps: u32,
    block_size: usize,
    out: &mut Vec<i32>,
    residual: &mut Vec<i32>,
) -> Result<()> {
    if order > block_size {
        return Err(Error::corrupt(format!(
            "fixed subframe: order {order} over a {block_size}-sample block"
        )));
    }
    for _ in 0..order {
        out.push(r.read_signed(bps)?);
    }
    decode_residual(r, block_size, order, residual)?;
    for (i, &res) in residual.iter().enumerate() {
        let n = order + i;
        let p = &out[n - order..n];
        // Fixed predictors are the first four differences of the signal.
        let predicted: i64 = match order {
            0 => 0,
            1 => i64::from(p[0]),
            2 => 2 * i64::from(p[1]) - i64::from(p[0]),
            3 => 3 * i64::from(p[2]) - 3 * i64::from(p[1]) + i64::from(p[0]),
            _ => 4 * i64::from(p[3]) - 6 * i64::from(p[2]) + 4 * i64::from(p[1]) - i64::from(p[0]),
        };
        out.push((predicted as i32).wrapping_add(res));
    }
    Ok(())
}

fn decode_lpc(
    r: &mut BitReader<'_>,
    order: usize,
    bps: u32,
    block_size: usize,
    out: &mut Vec<i32>,
    residual: &mut Vec<i32>,
) -> Result<()> {
    if order > block_size {
        return Err(Error::corrupt(format!(
            "LPC subframe: order {order} over a {block_size}-sample block"
        )));
    }
    for _ in 0..order {
        out.push(r.read_signed(bps)?);
    }
    let precision = r.read_bits(4)? + 1;
    if precision == 16 {
        return Err(Error::corrupt("LPC subframe: coefficient precision 15+1"));
    }
    let shift = r.read_signed(5)?;
    if shift < 0 {
        return Err(Error::corrupt(format!(
            "LPC subframe: negative shift {shift}"
        )));
    }
    let mut coefs = [0i64; 32];
    for c in coefs.iter_mut().take(order) {
        *c = i64::from(r.read_signed(precision)?);
    }
    decode_residual(r, block_size, order, residual)?;
    let coefs = &coefs[..order];
    for (i, &res) in residual.iter().enumerate() {
        let n = order + i;
        let history = &out[n - order..n];
        // 64-bit accumulation: 32-bit samples with 15-bit coefficients at order
        // 32 need 52 bits, and the "predictor overflow" streams exist to catch
        // a decoder that accumulates narrower.
        let mut sum = 0i64;
        for (c, &s) in coefs.iter().zip(history.iter().rev()) {
            sum += c * i64::from(s);
        }
        out.push(((sum >> shift) as i32).wrapping_add(res));
    }
    Ok(())
}

fn decode_residual(
    r: &mut BitReader<'_>,
    block_size: usize,
    order: usize,
    out: &mut Vec<i32>,
) -> Result<()> {
    let method = r.read_bits(2)?;
    let param_bits = match method {
        0 => 4,
        1 => 5,
        _ => {
            return Err(Error::corrupt(format!(
                "residual: coding method {method} is reserved"
            )));
        }
    };
    let escape = (1u32 << param_bits) - 1;
    let partition_order = r.read_bits(4)?;
    let partitions = 1usize << partition_order;
    if !block_size.is_multiple_of(partitions) {
        return Err(Error::corrupt(format!(
            "residual: {partitions} partitions over a {block_size}-sample block"
        )));
    }
    let partition_len = block_size / partitions;
    if partition_len < order {
        return Err(Error::corrupt(format!(
            "residual: order {order} over {partition_len}-sample partitions"
        )));
    }
    out.clear();
    out.reserve(block_size - order);
    for p in 0..partitions {
        let count = partition_len - if p == 0 { order } else { 0 };
        let param = r.read_bits(param_bits)?;
        if param == escape {
            let raw = r.read_bits(5)?;
            for _ in 0..count {
                out.push(match raw {
                    0 => 0,
                    n => r.read_signed(n)?,
                });
            }
        } else {
            for _ in 0..count {
                out.push(read_rice(r, param)?);
            }
        }
    }
    Ok(())
}

/// One Rice-coded, zig-zag folded residual.
fn read_rice(r: &mut BitReader<'_>, param: u32) -> Result<i32> {
    let mut quotient = 0u32;
    while !r.read_bit()? {
        quotient += 1;
        // A quotient wider than the sample container is damage, not data; the
        // cap is what keeps a fuzzed stream from spinning to the end of the
        // buffer one bit at a time.
        if quotient > 1 << 20 {
            return Err(Error::corrupt("residual: Rice quotient over 2^20"));
        }
    }
    let folded = match param {
        0 => quotient,
        n => (quotient << n) | r.read_bits(n)?,
    };
    Ok(((folded >> 1) as i32) ^ -((folded & 1) as i32))
}

fn undecorrelate(assignment: ChannelAssignment, channels: &mut [Vec<i32>]) -> Result<()> {
    if matches!(assignment, ChannelAssignment::Independent(_)) {
        return Ok(());
    }
    let (a, b) = channels.split_at_mut(1);
    let (a, b) = (&mut a[0], &mut b[0]);
    if a.len() != b.len() {
        return Err(Error::corrupt("stereo decorrelation over unequal channels"));
    }
    match assignment {
        ChannelAssignment::LeftSide => {
            // a = left, b = left - right.
            for (l, s) in a.iter().zip(b.iter_mut()) {
                *s = l.wrapping_sub(*s);
            }
        }
        ChannelAssignment::RightSide => {
            // a = left - right, b = right.
            for (s, r) in a.iter_mut().zip(b.iter()) {
                *s = s.wrapping_add(*r);
            }
        }
        ChannelAssignment::MidSide => {
            for (m, s) in a.iter_mut().zip(b.iter_mut()) {
                // The low bit of the difference is carried in the mid channel.
                let mid = (*m << 1) | (*s & 1);
                let side = *s;
                *m = mid.wrapping_add(side) >> 1;
                *s = mid.wrapping_sub(side) >> 1;
            }
        }
        ChannelAssignment::Independent(_) => unreachable!("returned above"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_info_round_trips() {
        let info = StreamInfo {
            min_block_size: 4096,
            max_block_size: 4096,
            min_frame_size: 14,
            max_frame_size: 16384,
            sample_rate: 44100,
            channels: 2,
            bits_per_sample: 16,
            total_samples: 1_234_567,
            md5: [7; 16],
        };
        assert_eq!(StreamInfo::parse(&info.to_bytes()).unwrap(), info);
    }

    #[test]
    fn truncated_input_is_need_more_not_a_panic() {
        let info = StreamInfo {
            min_block_size: 16,
            max_block_size: 16,
            min_frame_size: 0,
            max_frame_size: 0,
            sample_rate: 8000,
            channels: 1,
            bits_per_sample: 8,
            total_samples: 0,
            md5: [0; 16],
        };
        let bytes = info.to_bytes();
        for n in 0..34 {
            assert!(StreamInfo::parse(&bytes[..n]).unwrap_err().is_need_more());
        }
    }

    #[test]
    fn garbage_never_panics_and_never_opens() {
        for seed in 0..64u32 {
            let junk: Vec<u8> = (0..512).map(|i| (i * 37 + seed * 11) as u8).collect();
            let _ = FlacReader::new(&junk).map(|mut r| {
                let mut b = Block::default();
                let _ = r.next_block(&mut b);
            });
        }
    }

    #[test]
    fn mid_side_inverts_the_encoder_transform() {
        let left: Vec<i32> = (-8..8).collect();
        let right: Vec<i32> = (0..16).map(|i| i * 3 - 20).collect();
        let mid: Vec<i32> = left.iter().zip(&right).map(|(l, r)| (l + r) >> 1).collect();
        let side: Vec<i32> = left.iter().zip(&right).map(|(l, r)| l - r).collect();
        let mut planes = vec![mid, side];
        undecorrelate(ChannelAssignment::MidSide, &mut planes).unwrap();
        assert_eq!(planes[0], left);
        assert_eq!(planes[1], right);
    }

    #[test]
    fn container_shift_matches_reference_layout() {
        assert_eq!(container_shift(8), 8);
        assert_eq!(container_shift(12), 4);
        assert_eq!(container_shift(16), 0);
        assert_eq!(container_shift(20), 12);
        assert_eq!(container_shift(24), 8);
        assert_eq!(container_shift(32), 0);
    }
}
