//! What a stream type means, and everything derivable from the bytes of the
//! elementary stream itself: NAL scanning, avcC/hvcC records, AudioSpecificConfig,
//! audio frame headers.

use ec_core::CodecId;

use crate::psi::EsEntry;

/// What an elementary stream carries, as far as this crate can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StreamKind {
    /// H.264 / AVC (stream type 0x1B), Annex B.
    H264,
    /// H.265 / HEVC (0x24), Annex B.
    Hevc,
    /// MPEG-1 video (0x01), exposed as `CodecId::Mpeg2Video`.
    Mpeg1Video,
    /// MPEG-2 video (0x02), exposed as `CodecId::Mpeg2Video`.
    Mpeg2Video,
    /// AAC in ADTS (0x0F).
    AacAdts,
    /// AAC in LATM/LOAS (0x11). Always refused.
    AacLatm,
    /// AC-3 (0x81, or private data with the AC-3 registration / DVB descriptor 0x6A).
    Ac3,
    /// E-AC-3 (0x87, 0x84, 0xA1, or DVB descriptor 0x7A).
    Eac3,
    /// Dolby TrueHD (0x83, the Blu-ray stream type). The AC-3 core that Blu-ray
    /// interleaves in the same PID is dropped; see `track.rs`.
    TrueHd,
    /// DTS core (0x82, DTS-HD 0x85/0x86/0xA2, or private data with a DTS registration / descriptor 0x7B).
    Dts,
    /// MPEG-1/2 audio, layer I/II/III (0x03, 0x04). The layer, read from the first
    /// frame, picks `Mp1`, `Mp2` or `Mp3`.
    MpegAudio,
    /// HDMV LPCM (0x80).
    Lpcm,
    /// DVD-Video LPCM (private_stream_1 substream 0xA0-0xA7 in a program stream).
    DvdLpcm,
    /// HDMV presentation graphics (0x90).
    Pgs,
    /// DVD-Video subpictures (private_stream_1 substream 0x20-0x3F in a program stream).
    DvdSub,
    /// DVB subtitles (private data with the subtitling descriptor 0x59).
    DvbSub,
    /// Anything else, by a descriptive name.
    Other(String),
}

impl StreamKind {
    /// Classifies a PMT entry.
    pub fn classify(es: &EsEntry) -> StreamKind {
        match es.stream_type {
            0x01 => StreamKind::Mpeg1Video,
            0x02 => StreamKind::Mpeg2Video,
            0x03 | 0x04 => StreamKind::MpegAudio,
            0x0F => StreamKind::AacAdts,
            0x11 => StreamKind::AacLatm,
            0x1B => StreamKind::H264,
            0x24 => StreamKind::Hevc,
            0x06 | 0x05 => {
                let reg = es.registration();
                if reg == Some(*b"AC-3") || es.descriptor(0x6A).is_some() {
                    StreamKind::Ac3
                } else if reg == Some(*b"EAC3") || es.descriptor(0x7A).is_some() {
                    StreamKind::Eac3
                } else if reg == Some(*b"HEVC") {
                    StreamKind::Hevc
                } else if reg == Some(*b"DTS1")
                    || reg == Some(*b"DTS2")
                    || reg == Some(*b"DTS3")
                    || es.descriptor(0x7B).is_some()
                {
                    StreamKind::Dts
                } else if es.descriptor(0x56).is_some() {
                    StreamKind::Other("dvb_teletext".into())
                } else if es.descriptor(0x59).is_some() {
                    StreamKind::DvbSub
                } else if reg == Some(*b"Opus") {
                    StreamKind::Other("opus".into())
                } else {
                    StreamKind::Other("private_data".into())
                }
            }
            0x80 => StreamKind::Lpcm,
            0x81 => StreamKind::Ac3,
            0x82 => StreamKind::Dts,
            0x83 => StreamKind::TrueHd,
            0x84 | 0x87 | 0xA1 => StreamKind::Eac3,
            0x85 | 0x86 | 0xA2 => StreamKind::Dts,
            0x90 => StreamKind::Pgs,
            0x91 => StreamKind::Other("hdmv_igs".into()),
            0x92 => StreamKind::Other("hdmv_textst".into()),
            0xEA => StreamKind::Other("vc1".into()),
            0x10 => StreamKind::Other("mpeg4_visual".into()),
            0x1C => StreamKind::Other("aac_raw".into()),
            0x42 => StreamKind::Other("avs".into()),
            t => StreamKind::Other(format!("stream_type_0x{t:02x}")),
        }
    }

    /// Short lowercase name, usable in a refusal message.
    pub fn name(&self) -> String {
        match self {
            StreamKind::H264 => "h264".into(),
            StreamKind::Hevc => "hevc".into(),
            StreamKind::Mpeg1Video => "mpeg1video".into(),
            StreamKind::Mpeg2Video => "mpeg2video".into(),
            StreamKind::AacAdts => "aac".into(),
            StreamKind::AacLatm => "aac_latm".into(),
            StreamKind::Ac3 => "ac3".into(),
            StreamKind::Eac3 => "eac3".into(),
            StreamKind::TrueHd => "truehd".into(),
            StreamKind::Dts => "dts".into(),
            StreamKind::MpegAudio => "mpeg_audio".into(),
            StreamKind::Lpcm => "pcm_bluray".into(),
            StreamKind::DvdLpcm => "pcm_dvd".into(),
            StreamKind::Pgs => "hdmv_pgs_subtitle".into(),
            StreamKind::DvdSub => "dvd_subtitle".into(),
            StreamKind::DvbSub => "dvb_subtitle".into(),
            StreamKind::Other(n) => n.clone(),
        }
    }

    /// The family codec this maps to without looking at the stream. MPEG audio and
    /// LPCM depend on their first frame and answer `None` here.
    pub fn codec_id(&self) -> Option<CodecId> {
        match self {
            StreamKind::H264 => Some(CodecId::H264),
            StreamKind::Hevc => Some(CodecId::H265),
            StreamKind::Mpeg1Video | StreamKind::Mpeg2Video => Some(CodecId::Mpeg2Video),
            StreamKind::AacAdts => Some(CodecId::Aac),
            StreamKind::Ac3 => Some(CodecId::Ac3),
            StreamKind::Eac3 => Some(CodecId::EAc3),
            StreamKind::TrueHd => Some(CodecId::TrueHd),
            StreamKind::Dts => Some(CodecId::Dts),
            StreamKind::Pgs => Some(CodecId::Pgs),
            StreamKind::DvdSub => Some(CodecId::DvdSub),
            StreamKind::DvbSub => Some(CodecId::DvbSub),
            _ => None,
        }
    }

    /// True for the sparse bitmap subtitle streams (PGS, DVD, DVB): few packets, no timing
    /// facts to wait for.
    pub fn is_bitmap_sub(&self) -> bool {
        matches!(self, StreamKind::Pgs | StreamKind::DvdSub | StreamKind::DvbSub)
    }

    /// True for the two codecs delivered as Annex-B access units.
    pub fn is_annexb_video(&self) -> bool {
        matches!(self, StreamKind::H264 | StreamKind::Hevc)
    }

    /// True for MPEG-1 and MPEG-2 video.
    pub fn is_mpeg_video(&self) -> bool {
        matches!(self, StreamKind::Mpeg1Video | StreamKind::Mpeg2Video)
    }

    /// True for a picture stream of any codec.
    pub fn is_video(&self) -> bool {
        matches!(
            self,
            StreamKind::H264 | StreamKind::Hevc | StreamKind::Mpeg1Video | StreamKind::Mpeg2Video
        )
    }
}

// ---------------------------------------------------------------------------
// Annex B
// ---------------------------------------------------------------------------

/// Iterator over the NAL units of an Annex-B buffer: each item is the NAL with its
/// header byte(s), without start code or trailing zero bytes.
pub struct NalIter<'a> {
    data: &'a [u8],
    pos: usize,
}

/// NAL units of an Annex-B byte stream.
pub fn nal_units(data: &[u8]) -> NalIter<'_> {
    NalIter {
        data,
        pos: find_start(data, 0).map_or(data.len(), |p| p + 3),
    }
}

/// Offset of the next `00 00 01` at or after `from`.
fn find_start(data: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 3 <= data.len() {
        match data[i + 2] {
            1 if data[i] == 0 && data[i + 1] == 0 => return Some(i),
            0 | 1 => i += 1,
            _ => i += 3,
        }
    }
    None
}

impl<'a> Iterator for NalIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        if self.pos >= self.data.len() {
            return None;
        }
        let start = self.pos;
        let (end, next) = match find_start(self.data, start) {
            Some(p) => (p, p + 3),
            None => (self.data.len(), self.data.len()),
        };
        self.pos = next;
        let mut e = end;
        while e > start && self.data[e - 1] == 0 {
            e -= 1;
        }
        if e == start {
            return self.next();
        }
        Some(&self.data[start..e])
    }
}

/// H.264 `nal_unit_type`.
pub(crate) fn h264_type(nal: &[u8]) -> u8 {
    nal.first().map_or(0, |b| b & 0x1f)
}

/// HEVC `nal_unit_type`.
pub(crate) fn hevc_type(nal: &[u8]) -> u8 {
    nal.first().map_or(0, |b| (b >> 1) & 0x3f)
}

/// Facts an access-unit scan collects.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct AuScan {
    /// IDR (H.264) or IRAP (HEVC) slice present, or an H.264 recovery-point SEI.
    pub key: bool,
    /// Number of access unit delimiters.
    pub aud: u32,
    /// Offsets (into the scanned buffer) of each AUD NAL's start code.
    pub aud_at: Vec<usize>,
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
    pub vps: Vec<Vec<u8>>,
}

const MAX_SETS: usize = 8;
const MAX_SET_LEN: usize = 1024;

fn add_set(list: &mut Vec<Vec<u8>>, nal: &[u8]) {
    if nal.len() <= MAX_SET_LEN && list.len() < MAX_SETS && !list.iter().any(|s| s == nal) {
        list.push(nal.to_vec());
    }
}

/// Scans an Annex-B access unit.
pub(crate) fn scan_au(data: &[u8], hevc: bool) -> AuScan {
    let mut s = AuScan::default();
    for nal in nal_units(data) {
        let off = nal.as_ptr() as usize - data.as_ptr() as usize;
        if hevc {
            match hevc_type(nal) {
                16..=23 => s.key = true,
                35 => {
                    s.aud += 1;
                    s.aud_at.push(off.saturating_sub(3));
                }
                32 => add_set(&mut s.vps, nal),
                33 => add_set(&mut s.sps, nal),
                34 => add_set(&mut s.pps, nal),
                _ => {}
            }
        } else {
            match h264_type(nal) {
                5 => s.key = true,
                6 => {
                    if sei_has_recovery_point(nal) {
                        s.key = true;
                    }
                }
                9 => {
                    s.aud += 1;
                    s.aud_at.push(off.saturating_sub(3));
                }
                7 => add_set(&mut s.sps, nal),
                8 => add_set(&mut s.pps, nal),
                _ => {}
            }
        }
    }
    s
}

/// True when an H.264 SEI NAL carries a recovery point message (payload type 6).
fn sei_has_recovery_point(nal: &[u8]) -> bool {
    let mut i = 1;
    while i < nal.len() {
        let mut ty = 0usize;
        while nal.get(i) == Some(&0xFF) {
            ty += 255;
            i += 1;
        }
        let Some(&b) = nal.get(i) else {
            return false;
        };
        ty += usize::from(b);
        i += 1;
        let mut size = 0usize;
        while nal.get(i) == Some(&0xFF) {
            size += 255;
            i += 1;
        }
        let Some(&b) = nal.get(i) else {
            return false;
        };
        size += usize::from(b);
        i += 1;
        if ty == 6 {
            return true;
        }
        i = i.saturating_add(size);
        if nal.get(i).is_none_or(|&b| b == 0x80) {
            return false;
        }
    }
    false
}

/// Converts an Annex-B buffer to NAL units each preceded by a big-endian length of
/// `length_size` bytes (1, 2 or 4).
pub fn annexb_to_length_prefixed(data: &[u8], length_size: usize) -> Vec<u8> {
    let n = length_size.clamp(1, 4);
    let mut out = Vec::with_capacity(data.len() + 16);
    for nal in nal_units(data) {
        let len = nal.len() as u64;
        if n < 4 && len >= 1u64 << (8 * n) {
            continue;
        }
        out.extend_from_slice(&(len as u32).to_be_bytes()[4 - n..]);
        out.extend_from_slice(nal);
    }
    out
}

/// Removes emulation-prevention bytes.
fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// Parsed H.264 facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct H264Facts {
    pub width: u32,
    pub height: u32,
    pub chroma: u8,
    pub depth_luma: u8,
    pub depth_chroma: u8,
    pub frame_mbs_only: bool,
    pub profile: u8,
}

pub(crate) fn h264_facts(sps_nal: &[u8]) -> Option<H264Facts> {
    let rbsp = unescape(sps_nal.get(1..)?);
    let s = ec_h264_syntax::Sps::parse(&rbsp).ok()?;
    Some(H264Facts {
        width: s.width,
        height: s.height,
        chroma: s.chroma_format_idc,
        depth_luma: s.bit_depth_luma,
        depth_chroma: s.bit_depth_chroma,
        frame_mbs_only: s.frame_mbs_only,
        profile: s.profile_idc,
    })
}

/// An `AVCDecoderConfigurationRecord` (ISO 14496-15) from in-band SPS and PPS NAL
/// units (no start codes, header byte included). `None` without both.
pub fn build_avcc(sps: &[Vec<u8>], pps: &[Vec<u8>]) -> Option<Vec<u8>> {
    let first = sps.first()?;
    if first.len() < 4 || pps.is_empty() {
        return None;
    }
    let mut out = vec![1, first[1], first[2], first[3], 0xFF];
    out.push(0xE0 | sps.len().min(31) as u8);
    for s in sps.iter().take(31) {
        out.extend_from_slice(&(s.len() as u16).to_be_bytes());
        out.extend_from_slice(s);
    }
    out.push(pps.len().min(255) as u8);
    for p in pps.iter().take(255) {
        out.extend_from_slice(&(p.len() as u16).to_be_bytes());
        out.extend_from_slice(p);
    }
    if matches!(first[1], 100 | 110 | 122 | 144) {
        let f = h264_facts(first)?;
        out.push(0xFC | f.chroma);
        out.push(0xF8 | (f.depth_luma - 8));
        out.push(0xF8 | (f.depth_chroma - 8));
        out.push(0);
    }
    Some(out)
}

/// Parsed HEVC facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HevcFacts {
    pub width: u32,
    pub height: u32,
    pub chroma: u8,
    pub depth_luma: u8,
    pub depth_chroma: u8,
    pub interlaced_source: bool,
}

fn hevc_sps(sps_nal: &[u8]) -> Option<ec_h265_syntax::ps::Sps> {
    let rbsp = unescape(sps_nal.get(2..)?);
    ec_h265_syntax::ps::Sps::parse(&rbsp).ok()
}

pub(crate) fn hevc_facts(sps_nal: &[u8]) -> Option<HevcFacts> {
    let s = hevc_sps(sps_nal)?;
    let (width, height) = s.display_size();
    Some(HevcFacts {
        width,
        height,
        chroma: s.chroma_format_idc as u8,
        depth_luma: (s.bit_depth_luma_minus8 + 8) as u8,
        depth_chroma: (s.bit_depth_chroma_minus8 + 8) as u8,
        interlaced_source: s.ptl.interlaced_source,
    })
}

/// An `HEVCDecoderConfigurationRecord` from in-band VPS, SPS and PPS NAL units.
/// `None` without an SPS that parses.
pub fn build_hvcc(vps: &[Vec<u8>], sps: &[Vec<u8>], pps: &[Vec<u8>]) -> Option<Vec<u8>> {
    let first = sps.first()?;
    let s = hevc_sps(first)?;
    let p = &s.ptl;
    let mut out = vec![1];
    out.push((p.profile_space << 6) | (u8::from(p.tier_flag) << 5) | (p.profile_idc & 0x1f));
    out.extend_from_slice(&p.profile_compatibility.to_be_bytes());
    let flags = (u64::from(p.progressive_source) << 47)
        | (u64::from(p.interlaced_source) << 46)
        | (u64::from(p.non_packed_constraint) << 45)
        | (u64::from(p.frame_only_constraint) << 44)
        | (p.constraint_bits & ((1 << 44) - 1));
    out.extend_from_slice(&flags.to_be_bytes()[2..]);
    out.push(p.level_idc);
    out.extend_from_slice(&[0xF0, 0x00]); // min_spatial_segmentation_idc
    out.push(0xFC); // parallelismType
    out.push(0xFC | (s.chroma_format_idc as u8 & 3));
    out.push(0xF8 | (s.bit_depth_luma_minus8 as u8 & 7));
    out.push(0xF8 | (s.bit_depth_chroma_minus8 as u8 & 7));
    out.extend_from_slice(&[0, 0]); // avgFrameRate
    out.push((1 << 3) | (1 << 2) | 3); // 1 temporal layer, nested, 4-byte lengths
    let arrays: Vec<(u8, &[Vec<u8>])> = [(32u8, vps), (33, sps), (34, pps)]
        .into_iter()
        .filter(|(_, l)| !l.is_empty())
        .collect();
    out.push(arrays.len() as u8);
    for (ty, list) in arrays {
        out.push(0x80 | ty);
        out.extend_from_slice(&(list.len().min(MAX_SETS) as u16).to_be_bytes());
        for n in list.iter().take(MAX_SETS) {
            out.extend_from_slice(&(n.len() as u16).to_be_bytes());
            out.extend_from_slice(n);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Audio frame headers
// ---------------------------------------------------------------------------

/// What a frame header states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameHdr {
    /// Whole frame in bytes, header included.
    pub len: usize,
    /// PCM samples per channel the frame decodes to.
    pub samples: u32,
    pub rate: u32,
    /// Channel count including LFE.
    pub channels: u8,
    /// Dependent E-AC-3 substream: belongs to the previous frame.
    pub join: bool,
    /// ADTS object type minus one / MPEG audio layer (1..3), for building configs.
    pub aux: u8,
    /// ADTS sampling frequency index / MPEG audio unused.
    pub sr_idx: u8,
    /// ADTS channel configuration.
    pub ch_cfg: u8,
}

pub(crate) const AAC_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

/// ADTS header at the start of `b`.
pub(crate) fn adts_header(b: &[u8]) -> Option<FrameHdr> {
    let h = b.get(..7)?;
    if h[0] != 0xFF || h[1] & 0xF6 != 0xF0 {
        return None;
    }
    let sr_idx = (h[2] >> 2) & 15;
    let rate = *AAC_RATES.get(usize::from(sr_idx))?;
    let ch_cfg = ((h[2] & 1) << 2) | (h[3] >> 6);
    let len = (usize::from(h[3] & 3) << 11) | (usize::from(h[4]) << 3) | usize::from(h[5] >> 5);
    let hdr = if h[1] & 1 == 0 { 9 } else { 7 };
    if len < hdr {
        return None;
    }
    let blocks = u32::from(h[6] & 3) + 1;
    let channels = match ch_cfg {
        0 => 0,
        7 => 8,
        n => n,
    };
    Some(FrameHdr {
        len,
        samples: 1024 * blocks,
        rate,
        channels,
        join: false,
        aux: h[2] >> 6,
        sr_idx,
        ch_cfg,
    })
}

/// A two-byte AudioSpecificConfig for AAC with object type `aot` (1 = Main, 2 = LC,
/// 3 = SSR, 4 = LTP), sampling-frequency index and channel configuration.
pub fn audio_specific_config(aot: u8, sr_idx: u8, ch_cfg: u8) -> Vec<u8> {
    let v =
        (u16::from(aot & 31) << 11) | (u16::from(sr_idx & 15) << 7) | (u16::from(ch_cfg & 15) << 3);
    v.to_be_bytes().to_vec()
}

const AC3_SIZES: [[u16; 38]; 3] = [
    [
        64, 64, 80, 80, 96, 96, 112, 112, 128, 128, 160, 160, 192, 192, 224, 224, 256, 256, 320,
        320, 384, 384, 448, 448, 512, 512, 640, 640, 768, 768, 896, 896, 1024, 1024, 1152, 1152,
        1280, 1280,
    ],
    [
        69, 70, 87, 88, 104, 105, 121, 122, 139, 140, 174, 175, 208, 209, 243, 244, 278, 279, 348,
        349, 417, 418, 487, 488, 557, 558, 696, 697, 835, 836, 975, 976, 1114, 1115, 1253, 1254,
        1393, 1394,
    ],
    [
        96, 96, 120, 120, 144, 144, 168, 168, 192, 192, 240, 240, 288, 288, 336, 336, 384, 384,
        480, 480, 576, 576, 672, 672, 768, 768, 960, 960, 1152, 1152, 1344, 1344, 1536, 1536, 1728,
        1728, 1920, 1920,
    ],
];

fn ac3_channels(acmod: u8, lfe: bool) -> u8 {
    let base = [2, 1, 2, 3, 3, 4, 4, 5][usize::from(acmod & 7)];
    base + u8::from(lfe)
}

/// AC-3 or E-AC-3 syncframe header at the start of `b`. Returns the header and
/// whether it is E-AC-3.
pub(crate) fn ac3_header(b: &[u8]) -> Option<(FrameHdr, bool)> {
    let h = b.get(..8)?;
    if h[0] != 0x0B || h[1] != 0x77 {
        return None;
    }
    let bsid = h[5] >> 3;
    if bsid <= 10 {
        let fscod = h[4] >> 6;
        let frmsizecod = usize::from(h[4] & 0x3f);
        if fscod == 3 || frmsizecod >= 38 {
            return None;
        }
        let words = AC3_SIZES[usize::from(fscod)][frmsizecod];
        let rate = [48000, 44100, 32000][usize::from(fscod)];
        let acmod = h[6] >> 5;
        let mut bit = 3;
        if acmod & 1 != 0 && acmod != 1 {
            bit += 2;
        }
        if acmod & 4 != 0 {
            bit += 2;
        }
        if acmod == 2 {
            bit += 2;
        }
        let lfe = (u16::from_be_bytes([h[6], h[7]]) >> (15 - bit)) & 1 != 0;
        Some((
            FrameHdr {
                len: usize::from(words) * 2,
                samples: 1536,
                rate,
                channels: ac3_channels(acmod, lfe),
                join: false,
                aux: 0,
                sr_idx: 0,
                ch_cfg: 0,
            },
            false,
        ))
    } else if (11..=16).contains(&bsid) {
        let strmtyp = h[2] >> 6;
        let substream = (h[2] >> 3) & 7;
        let frmsiz = (usize::from(h[2] & 7) << 8) | usize::from(h[3]);
        let fscod = h[4] >> 6;
        let (rate, blocks) = if fscod == 3 {
            let fscod2 = (h[4] >> 4) & 3;
            if fscod2 == 3 {
                return None;
            }
            ([24000, 22050, 16000][usize::from(fscod2)], 6)
        } else {
            (
                [48000, 44100, 32000][usize::from(fscod)],
                [1u32, 2, 3, 6][usize::from((h[4] >> 4) & 3)],
            )
        };
        let acmod = (h[4] >> 1) & 7;
        let lfe = h[4] & 1 != 0;
        if strmtyp == 3 {
            return None;
        }
        Some((
            FrameHdr {
                len: (frmsiz + 1) * 2,
                samples: blocks * 256,
                rate,
                channels: ac3_channels(acmod, lfe),
                join: strmtyp == 1 || substream != 0,
                aux: 0,
                sr_idx: 0,
                ch_cfg: 0,
            },
            true,
        ))
    } else {
        None
    }
}

const MPA_BITRATES: [[[u16; 15]; 3]; 2] = [
    // MPEG-1: layer I, II, III
    [
        [
            0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
        ],
        [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
        ],
        [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ],
    ],
    // MPEG-2 / 2.5
    [
        [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
        ],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
    ],
];

/// MPEG audio frame header at the start of `b`; `aux` is the layer (1..=3).
pub(crate) fn mpa_header(b: &[u8]) -> Option<FrameHdr> {
    let h = b.get(..4)?;
    if h[0] != 0xFF || h[1] & 0xE0 != 0xE0 {
        return None;
    }
    let version = (h[1] >> 3) & 3; // 3 = MPEG-1, 2 = MPEG-2, 0 = 2.5
    let layer = match (h[1] >> 1) & 3 {
        3 => 1u8,
        2 => 2,
        1 => 3,
        _ => return None,
    };
    if version == 1 {
        return None;
    }
    let br_idx = usize::from(h[2] >> 4);
    let sr_idx = usize::from((h[2] >> 2) & 3);
    if br_idx == 0 || br_idx == 15 || sr_idx == 3 {
        return None;
    }
    let mpeg1 = version == 3;
    let rate = [44100u32, 48000, 32000][sr_idx]
        >> (if mpeg1 {
            0
        } else if version == 2 {
            1
        } else {
            2
        });
    let kbps = u32::from(MPA_BITRATES[usize::from(!mpeg1)][usize::from(layer - 1)][br_idx]);
    let pad = u32::from((h[2] >> 1) & 1);
    let (len, samples) = match layer {
        1 => ((12 * kbps * 1000 / rate + pad) * 4, 384),
        2 => (144 * kbps * 1000 / rate + pad, 1152),
        _ if mpeg1 => (144 * kbps * 1000 / rate + pad, 1152),
        _ => (72 * kbps * 1000 / rate + pad, 576),
    };
    Some(FrameHdr {
        len: len as usize,
        samples,
        rate,
        channels: if (h[3] >> 6) == 3 { 1 } else { 2 },
        join: false,
        aux: layer,
        sr_idx: 0,
        ch_cfg: 0,
    })
}

/// Which framing an audio stream uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Framing {
    Adts,
    Ac3,
    Mpa,
    Dts,
}

/// Header of the frame starting at `b[0]`, whichever the framing.
pub(crate) fn frame_header(f: Framing, b: &[u8]) -> Option<FrameHdr> {
    match f {
        Framing::Adts => adts_header(b),
        Framing::Ac3 => ac3_header(b).map(|(h, _)| h),
        Framing::Mpa => mpa_header(b),
        Framing::Dts => crate::dts::access_unit(b).map(|(c, len)| FrameHdr {
            len,
            samples: c.samples,
            rate: c.rate,
            channels: c.channels,
            join: false,
            aux: 0,
            sr_idx: 0,
            ch_cfg: 0,
        }),
    }
}

/// HDMV LPCM header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LpcmHeader {
    /// `audio_data_payload_size`.
    pub payload_size: usize,
    /// Channels of the layout (without the padding channel of odd layouts).
    pub channels: u8,
    /// Sample rate in Hz.
    pub rate: u32,
    /// 16, 20 or 24.
    pub bits: u8,
    /// True when the layout has an odd number of channels; BD LPCM then stores one
    /// extra zero channel at the end of every frame, which the demuxer drops.
    pub padded: bool,
}

/// Parses the 4-byte header of an HDMV LPCM PES payload.
pub fn parse_lpcm_header(b: &[u8]) -> Option<LpcmHeader> {
    let h = b.get(..4)?;
    let payload_size = usize::from(u16::from_be_bytes([h[0], h[1]]));
    let assignment = h[2] >> 4;
    let channels = match assignment {
        1 => 1,
        3 => 2,
        4 | 5 => 3,
        6 | 7 => 4,
        8 => 5,
        9 => 6,
        10 => 7,
        11 => 8,
        _ => return None,
    };
    let rate = match h[2] & 15 {
        1 => 48000,
        4 => 96000,
        5 => 192000,
        _ => return None,
    };
    let bits = match h[3] >> 6 {
        1 => 16,
        2 => 20,
        3 => 24,
        _ => return None,
    };
    Some(LpcmHeader {
        payload_size,
        channels,
        rate,
        bits,
        padded: channels % 2 == 1,
    })
}

/// Parses the 3-byte audio header that follows the substream header of DVD-Video LPCM.
pub(crate) fn parse_dvd_lpcm_header(b: &[u8]) -> Option<LpcmHeader> {
    let h = b.get(..3)?;
    let bits = match h[1] >> 6 {
        0 => 16,
        1 => 20,
        2 => 24,
        _ => return None,
    };
    let rate = match (h[1] >> 4) & 3 {
        0 => 48000,
        1 => 96000,
        _ => return None,
    };
    Some(LpcmHeader {
        payload_size: 0,
        channels: (h[1] & 7) + 1,
        rate,
        bits,
        padded: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nal_iteration() {
        let d = [0, 0, 0, 1, 9, 0xF0, 0, 0, 1, 0x65, 1, 2, 0, 0, 0, 1, 6, 5];
        let n: Vec<&[u8]> = nal_units(&d).collect();
        assert_eq!(n, vec![&[9u8, 0xF0][..], &[0x65, 1, 2][..], &[6, 5][..]]);
        assert!(scan_au(&d, false).key);
        let lp = annexb_to_length_prefixed(&d, 4);
        assert_eq!(&lp[..6], &[0, 0, 0, 2, 9, 0xF0]);
    }

    #[test]
    fn adts_and_asc() {
        // AAC-LC, 48 kHz, stereo, frame length 100.
        let mut h = [0xFF, 0xF1, 0x4C, 0x80, 0, 0, 0xFC];
        let len = 100usize;
        h[3] |= (len >> 11) as u8 & 3;
        h[4] = (len >> 3) as u8;
        h[5] = ((len & 7) << 5) as u8 | 0x1F;
        let hdr = adts_header(&h).unwrap();
        assert_eq!(
            (hdr.len, hdr.rate, hdr.channels, hdr.samples),
            (100, 48000, 2, 1024)
        );
        assert_eq!(
            audio_specific_config(hdr.aux + 1, hdr.sr_idx, hdr.ch_cfg),
            vec![0x11, 0x90]
        );
    }

    #[test]
    fn lpcm_header() {
        let h = parse_lpcm_header(&[0x07, 0x80, 0x31, 0x40]).unwrap();
        assert_eq!(
            (h.channels, h.rate, h.bits, h.payload_size),
            (2, 48000, 16, 0x780)
        );
    }
}

// ---------------------------------------------------------------------------
// MPEG-1/2 video (ISO 11172-2 / 13818-2)
// ---------------------------------------------------------------------------

/// What a sequence header (+ sequence_extension) says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mpeg2Facts {
    pub width: u32,
    pub height: u32,
    /// Frame rate as `(num, den)`.
    pub rate: (u32, u32),
    /// Sample aspect ratio when derivable and not square.
    pub sar: Option<(u32, u32)>,
    /// `chroma_format` of the sequence_extension (1 = 4:2:0, 2 = 4:2:2, 3 = 4:4:4).
    pub chroma: u8,
    /// True when a sequence_extension follows the header (MPEG-2 proper).
    pub has_ext: bool,
    /// Sequence header (and extension) bytes, start codes included.
    pub extradata: Vec<u8>,
}

/// What one scan over an MPEG-1/2 video access unit found.
#[derive(Debug, Default)]
pub struct Mpeg2Scan {
    /// Offsets of every `00 00 01 00` picture start code.
    pub pictures: Vec<usize>,
    /// Offsets of the start of each picture's header run (preceding sequence, GOP,
    /// extension and user-data headers included); parallel to `pictures`.
    pub starts: Vec<usize>,
    /// A sequence header or an I picture is present.
    pub key: bool,
    pub facts: Option<Mpeg2Facts>,
}

/// Start codes of the headers that may precede a picture and belong to it.
fn mpeg2_lead(code: u8) -> bool {
    matches!(code, 0xB2 | 0xB3 | 0xB5 | 0xB8)
}

/// Scans an access unit for pictures, the keyframe property and the sequence header.
pub fn mpeg2_scan(data: &[u8]) -> Mpeg2Scan {
    let mut sc = Mpeg2Scan::default();
    // (offset, code) of every start code.
    let mut codes: Vec<(usize, u8)> = Vec::new();
    let mut i = 0usize;
    while i + 4 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            codes.push((i, data[i + 3]));
            i += 3;
        } else {
            i += 1;
        }
    }
    for (k, &(off, code)) in codes.iter().enumerate() {
        match code {
            0x00 => {
                if data.get(off + 5).is_some_and(|b| (b >> 3) & 7 == 1) {
                    sc.key = true;
                }
                let mut j = k;
                while j > 0 && mpeg2_lead(codes[j - 1].1) {
                    j -= 1;
                }
                sc.pictures.push(off);
                sc.starts.push(codes[j].0);
            }
            0xB3 => {
                sc.key = true;
                if sc.facts.is_none() {
                    sc.facts = parse_mpeg2_seq(&data[off..], &codes[k + 1..], off);
                }
            }
            _ => {}
        }
    }
    sc
}

/// Parses the sequence header at `d[0..]` and the extension if it directly follows.
fn parse_mpeg2_seq(d: &[u8], later: &[(usize, u8)], base: usize) -> Option<Mpeg2Facts> {
    if d.len() < 12 {
        return None;
    }
    let h = &d[4..];
    let mut width = (u32::from(h[0]) << 4) | u32::from(h[1] >> 4);
    let mut height = (u32::from(h[1] & 15) << 8) | u32::from(h[2]);
    let aspect = h[3] >> 4;
    let rate_code = usize::from(h[3] & 15);
    // 12 bytes up to and including the load_intra flag byte (h[7] bit 1).
    let mut len = 12usize;
    if h[7] & 2 != 0 {
        len += 64;
    }
    let nonintra_byte = len - 1;
    if d.len() <= nonintra_byte {
        return None;
    }
    if d[nonintra_byte] & 1 != 0 {
        len += 64;
    }
    if d.len() < len || width == 0 || height == 0 {
        return None;
    }
    const RATES: [(u32, u32); 9] = [
        (0, 0),
        (24000, 1001),
        (24, 1),
        (25, 1),
        (30000, 1001),
        (30, 1),
        (50, 1),
        (60000, 1001),
        (60, 1),
    ];
    let (mut n, mut dn) = *RATES.get(rate_code)?;
    if n == 0 {
        return None;
    }
    let mut extradata = d[..len].to_vec();
    let mut chroma = 1u8;
    let mut has_ext = false;
    if let Some(&(eoff, 0xB5)) = later.first()
        && eoff - base == len
        && d.len() >= len + 10
        && d[len + 4] >> 4 == 1
    {
        let e = &d[len + 4..len + 10];
        has_ext = true;
        chroma = (e[1] >> 1) & 3;
        width |= u32::from(((e[1] & 1) << 1) | (e[2] >> 7)) << 12;
        height |= u32::from((e[2] >> 5) & 3) << 12;
        n *= u32::from((e[4] >> 5) & 3) + 1;
        dn *= u32::from(e[5] & 31) + 1;
        extradata.extend_from_slice(&d[len..len + 10]);
    }
    let g = gcd(n, dn);
    let (w, hh) = (u64::from(width), u64::from(height));
    // MPEG-1 codes are sample aspect ratios (only square is exact enough to use);
    // MPEG-2 codes are display aspect ratios, converted with the coded size.
    let sar = if has_ext {
        let dar = match aspect {
            2 => Some((4u64, 3u64)),
            3 => Some((16, 9)),
            4 => Some((221, 100)),
            _ => None,
        };
        dar.and_then(|(dn_, dd)| {
            let (a, b) = (dn_ * hh, dd * w);
            let g = gcd64(a, b);
            (a != b).then_some(((a / g) as u32, (b / g) as u32))
        })
    } else {
        None
    };
    Some(Mpeg2Facts {
        width,
        height,
        rate: (n / g, dn / g),
        sar,
        chroma,
        has_ext,
        extradata,
    })
}

fn gcd(a: u32, b: u32) -> u32 {
    gcd64(u64::from(a), u64::from(b)) as u32
}

fn gcd64(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

/// Bytes a frame header needs before [`frame_header`] can judge it.
pub(crate) fn header_lookahead(f: Framing) -> usize {
    if f == Framing::Dts { 16 } else { 8 }
}
