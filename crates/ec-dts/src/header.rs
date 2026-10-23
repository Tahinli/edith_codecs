//! DTS core sync words, stream word order conversion and the core frame
//! header (ETSI TS 102 114 section 5.3.1).
//!
//! [`parse_frame_header`] is the stable entry point for raw `.dts` sniffing:
//! it accepts any of the four stream layouts (16-bit big or little endian,
//! 14-bit big or little endian) and states the frame size, sample rate,
//! channel count and samples per frame without decoding any audio.

use crate::bits::Bits;
use ec_core::{Error, Result};

/// Core sync word as it reads in canonical (16-bit, big-endian) order.
pub const SYNC_CORE: u32 = 0x7FFE_8001;
/// Core sync word in 16-bit little-endian word order.
pub const SYNC_CORE_LE: u32 = 0xFE7F_0180;
/// Core sync word in 14-bit-in-16, big-endian word order.
pub const SYNC_CORE_14_BE: u32 = 0x1FFF_E800;
/// Core sync word in 14-bit-in-16, little-endian word order.
pub const SYNC_CORE_14_LE: u32 = 0xFF1F_00E8;
/// DTS-HD extension substream sync word.
pub const SYNC_EXSS: u32 = 0x6458_2025;

/// How the stream stores the canonical bit string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFormat {
    /// 16-bit words, big endian (the canonical order).
    Be16,
    /// 16-bit words, little endian.
    Le16,
    /// 14 significant bits per 16-bit word, big endian.
    Be14,
    /// 14 significant bits per 16-bit word, little endian.
    Le14,
}

impl StreamFormat {
    /// Classify the four-byte sync word at the start of `data`.
    pub fn from_sync(data: &[u8]) -> Option<StreamFormat> {
        let w = u32::from_be_bytes(data.get(..4)?.try_into().ok()?);
        match w {
            SYNC_CORE => Some(StreamFormat::Be16),
            SYNC_CORE_LE => Some(StreamFormat::Le16),
            SYNC_CORE_14_BE => Some(StreamFormat::Be14),
            SYNC_CORE_14_LE => Some(StreamFormat::Le14),
            _ => None,
        }
    }
}

/// Convert stream words to the canonical big-endian packed bit string.
///
/// `data` starts at a core sync word. The output is appended to `out`.
pub fn to_canonical(format: StreamFormat, data: &[u8], out: &mut Vec<u8>) {
    match format {
        StreamFormat::Be16 => out.extend_from_slice(data),
        StreamFormat::Le16 => {
            for c in data.chunks(2) {
                if c.len() == 2 {
                    out.push(c[1]);
                    out.push(c[0]);
                } else {
                    out.push(c[0]);
                }
            }
        }
        StreamFormat::Be14 | StreamFormat::Le14 => {
            let mut acc: u32 = 0;
            let mut nbits: u32 = 0;
            for c in data.chunks(2) {
                let w = if c.len() == 2 {
                    if format == StreamFormat::Be14 {
                        u16::from_be_bytes([c[0], c[1]])
                    } else {
                        u16::from_le_bytes([c[0], c[1]])
                    }
                } else {
                    (c[0] as u16) << 8
                };
                acc = (acc << 14) | (w & 0x3FFF) as u32;
                nbits += 14;
                while nbits >= 8 {
                    out.push((acc >> (nbits - 8)) as u8);
                    nbits -= 8;
                }
                acc &= (1 << nbits) - 1;
            }
            if nbits > 0 {
                out.push((acc << (8 - nbits)) as u8);
            }
        }
    }
}

/// Sample rate by `SFREQ` (Table 5-5); 0 marks a code the core does not use.
pub const SAMPLE_RATES: [u32; 16] = [
    0, 8000, 16000, 32000, 0, 0, 11025, 22050, 44100, 0, 0, 12000, 24000, 48000, 0, 0,
];

/// Transmission bit rate by `RATE` (Table 5-7). The last three codes are
/// open (1), variable (2) and lossless (3) and carry no nominal rate.
pub const BIT_RATES: [u32; 32] = [
    32000, 56000, 64000, 96000, 112000, 128000, 192000, 224000, 256000, 320000, 384000, 448000,
    512000, 576000, 640000, 768000, 896000, 1024000, 1152000, 1280000, 1344000, 1408000, 1411200,
    1472000, 1536000, 1920000, 2048000, 3072000, 3840000, 1, 2, 3,
];

/// Primary channels by `AMODE` (Table 5-4).
pub const CHANNELS: [u8; 16] = [1, 2, 2, 2, 2, 3, 3, 4, 4, 5, 6, 6, 6, 7, 8, 8];

/// Source PCM resolution by `PCMR` (Table 5-10); 0 marks a reserved code.
pub const PCM_RESOLUTION: [u8; 8] = [16, 16, 20, 20, 0, 24, 24, 0];

/// Everything the core frame header states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameHeader {
    /// How the stream stores the bit string.
    pub format: StreamFormat,
    /// Normal frame flag (false for a termination frame).
    pub normal_frame: bool,
    /// Header and side-information CRCs are present.
    pub crc_present: bool,
    /// PCM sample blocks of 32 samples in the frame.
    pub npcmblocks: u32,
    /// Frame size in bytes of the canonical (packed) bit string.
    pub frame_size: usize,
    /// Frame size in bytes as stored in the stream, which is larger for the
    /// 14-bit formats.
    pub stream_frame_size: usize,
    /// Audio channel arrangement code (`AMODE`).
    pub amode: u8,
    /// Core sample rate in Hz.
    pub sample_rate: u32,
    /// Transmission bit rate code (`RATE`).
    pub bit_rate_code: u8,
    /// Nominal bit rate in bit/s, `None` for open, variable and lossless.
    pub bit_rate: Option<u32>,
    /// Embedded dynamic range coefficients present.
    pub drc_present: bool,
    /// Embedded time stamp present.
    pub ts_present: bool,
    /// Auxiliary data present.
    pub aux_present: bool,
    /// Extension audio descriptor (`EXT_AUDIO_ID`): 0 XCH, 2 X96, 6 XXCH.
    pub ext_audio_type: u8,
    /// An extension is present in the core frame.
    pub ext_audio_present: bool,
    /// Audio sync word inserted after every subsubframe.
    pub sync_ssf: bool,
    /// LFE flag: 0 none, 1 with 128x decimation, 2 with 64x decimation.
    pub lfe: u8,
    /// ADPCM predictor history carries over from the previous frame.
    pub predictor_history: bool,
    /// Perfect-reconstruction QMF bank.
    pub filter_perfect: bool,
    /// Encoder software revision.
    pub encoder_rev: u8,
    /// Source PCM resolution in bits (16, 20 or 24).
    pub source_pcm_res: u8,
    /// Source was ES mastered (`PCMR` bit 0).
    pub es_format: bool,
    /// Front channels are sum/difference coded.
    pub sumdiff_front: bool,
    /// Surround channels are sum/difference coded.
    pub sumdiff_surround: bool,
    /// Dialogue normalisation parameter (`DIALNORM`), surfaced, never applied.
    pub dialnorm_code: u8,
    /// Total channels the core carries: primary channels plus the LFE.
    pub channels: u8,
    /// PCM samples per channel in the frame (`npcmblocks * 32`).
    pub samples_per_frame: u32,
}

impl FrameHeader {
    /// True when the arrangement code is one the decoder handles (0..=9).
    pub fn amode_supported(&self) -> bool {
        self.amode <= 9
    }
}

/// Parse the core frame header at the start of `data`.
///
/// `data` starts at a core sync word in any of the four stream layouts. A
/// short buffer is [`Error::NeedMore`]; anything that is not a valid core
/// frame header is [`Error::Corrupt`], which makes this usable as an "is this
/// DTS?" probe. Only the header is read, so a frame does not have to be
/// complete: the frame size is a statement about the frame to come.
pub fn parse_frame_header(data: &[u8]) -> Result<FrameHeader> {
    if data.len() < 4 {
        return Err(Error::NeedMore);
    }
    let Some(format) = StreamFormat::from_sync(data) else {
        return Err(Error::corrupt("DTS: no core sync word"));
    };
    let need = match format {
        StreamFormat::Be16 | StreamFormat::Le16 => 18,
        _ => 21,
    };
    if data.len() < need {
        return Err(Error::NeedMore);
    }
    let mut canon = Vec::with_capacity(24);
    to_canonical(format, &data[..need], &mut canon);
    parse_canonical(format, &canon)
}

pub(crate) fn parse_canonical(format: StreamFormat, canon: &[u8]) -> Result<FrameHeader> {
    let mut r = Bits::new(canon);
    if r.get(32) != SYNC_CORE {
        return Err(Error::corrupt("DTS: no core sync word"));
    }
    let normal_frame = r.get1() != 0;
    let deficit = r.get(5) + 1;
    if deficit != 32 {
        return Err(Error::unsupported(
            "DTS deficit samples",
            "only frames of whole 32-sample blocks are decoded",
        ));
    }
    let crc_present = r.get1() != 0;
    let npcmblocks = r.get(7) + 1;
    if npcmblocks & 7 != 0 {
        return Err(Error::corrupt("DTS: PCM block count not a multiple of 8"));
    }
    let frame_size = r.get(14) as usize + 1;
    if frame_size < 96 {
        return Err(Error::corrupt("DTS: core frame size below 96 bytes"));
    }
    let amode = r.get(6) as u8;
    if amode > 9 {
        return Err(Error::unsupported(
            format!("DTS audio channel arrangement {amode}"),
            "only the ten core arrangements (AMODE 0-9) are decoded",
        ));
    }
    let sfreq = r.get(4) as usize;
    let sample_rate = SAMPLE_RATES[sfreq];
    if sample_rate == 0 {
        return Err(Error::corrupt("DTS: invalid core sampling frequency"));
    }
    let bit_rate_code = r.get(5) as u8;
    if r.get1() != 0 {
        return Err(Error::corrupt("DTS: reserved header bit set"));
    }
    let drc_present = r.get1() != 0;
    let ts_present = r.get1() != 0;
    let aux_present = r.get1() != 0;
    let _hdcd = r.get1();
    let ext_audio_type = r.get(3) as u8;
    let ext_audio_present = r.get1() != 0;
    let sync_ssf = r.get1() != 0;
    let lfe = r.get(2) as u8;
    if lfe == 3 {
        return Err(Error::corrupt("DTS: invalid LFE flag"));
    }
    let predictor_history = r.get1() != 0;
    if crc_present {
        r.skip(16);
    }
    let filter_perfect = r.get1() != 0;
    let encoder_rev = r.get(4) as u8;
    let _copy_hist = r.get(2);
    let pcmr = r.get(3) as usize;
    let source_pcm_res = PCM_RESOLUTION[pcmr];
    if source_pcm_res == 0 {
        return Err(Error::corrupt("DTS: invalid source PCM resolution"));
    }
    let sumdiff_front = r.get1() != 0;
    let sumdiff_surround = r.get1() != 0;
    let dialnorm_code = r.get(4) as u8;
    if r.over {
        return Err(Error::NeedMore);
    }
    let rate = BIT_RATES[bit_rate_code as usize];
    let stream_frame_size = match format {
        StreamFormat::Be16 | StreamFormat::Le16 => frame_size,
        StreamFormat::Be14 | StreamFormat::Le14 => (frame_size * 8).div_ceil(14) * 2,
    };
    Ok(FrameHeader {
        format,
        normal_frame,
        crc_present,
        npcmblocks,
        frame_size,
        stream_frame_size,
        amode,
        sample_rate,
        bit_rate_code,
        bit_rate: if rate > 3 { Some(rate) } else { None },
        drc_present,
        ts_present,
        aux_present,
        ext_audio_type,
        ext_audio_present,
        sync_ssf,
        lfe,
        predictor_history,
        filter_perfect,
        encoder_rev,
        source_pcm_res,
        es_format: pcmr & 1 != 0,
        sumdiff_front,
        sumdiff_surround,
        dialnorm_code,
        channels: CHANNELS[amode as usize] + u8::from(lfe != 0),
        samples_per_frame: npcmblocks * 32,
    })
}

/// Length in bytes of the core frame starting at `data[0]`, as stored in the
/// stream (so 14-bit streams answer with the larger stored size).
pub fn frame_size(data: &[u8]) -> Result<usize> {
    Ok(parse_frame_header(data)?.stream_frame_size)
}

/// Find the first core sync word in `data` and its stream layout.
pub fn find_sync(data: &[u8]) -> Option<(usize, StreamFormat)> {
    (0..data.len().saturating_sub(3)).find_map(|i| StreamFormat::from_sync(&data[i..]).map(|f| (i, f)))
}
