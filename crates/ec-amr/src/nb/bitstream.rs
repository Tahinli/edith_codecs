//! AMR-NB bitstream: RFC 4867 / 3GPP TS 26.101 IF1 (octet-aligned, "MIME/IETF storage") frame
//! parsing into decoder parameters (wmf_to_ets + bits2prm of opencore-amr).
#![allow(dead_code)]

use super::tables::*;

/// Receive frame type (C `enum RXFrameType`, same numeric order).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RxFrameType {
    SpeechGood = 0,
    SpeechDegraded = 1,
    Onset = 2,
    SpeechBad = 3,
    SidFirst = 4,
    SidUpdate = 5,
    SidBad = 6,
    NoData = 7,
}

impl Mode {
    /// Mode from index 0..=8 (8 = MrDtx).
    #[inline]
    pub fn from_index(i: usize) -> Option<Mode> {
        Some(match i {
            0 => Mode::Mr475,
            1 => Mode::Mr515,
            2 => Mode::Mr59,
            3 => Mode::Mr67,
            4 => Mode::Mr74,
            5 => Mode::Mr795,
            6 => Mode::Mr102,
            7 => Mode::Mr122,
            8 => Mode::MrDtx,
            _ => return None,
        })
    }
    /// Index 0..=8 (C enum value).
    #[inline]
    pub fn index(self) -> usize {
        self as usize
    }
}

/// Errors from frame parsing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BitstreamError {
    /// Fewer bytes than the ToC byte + payload for this frame type.
    Truncated,
    /// Frame type 9..=14 (unsupported SID variants / reserved): the C decoder rejects these.
    InvalidFrameType,
}

/// One parsed frame.
#[derive(Clone, Debug)]
pub struct ParsedFrame {
    /// RX type to pass to Decoder_amr (SpeechGood for FT 0-7, SidFirst/SidUpdate for FT 8, NoData for FT 15).
    pub frame_type: RxFrameType,
    /// Codec mode for Decoder_amr/post-filter. FT 0-7: that mode; SID: mode indication carried in the
    /// SID frame (0..7); NoData: `prev_mode` as passed in.
    pub mode: Mode,
    /// Raw frame type field (0..=15) from the ToC byte.
    pub ft: u8,
    /// Q (frame quality) bit from the ToC byte (opencore ignores it; informational).
    pub q: bool,
    /// Decoded parameters: for SID frames the 5 MRDTX parameters, else the mode's parameters.
    /// Entries past the used count are 0. Size MAX_PRM_SIZE + 1 (as C `parm[MAX_PRM_SIZE+1]`).
    pub prm: [i16; MAX_PRM_SIZE + 1],
    /// Total bytes consumed from the input (1 ToC byte + payload bytes).
    pub consumed: usize,
}

/// Payload bytes following the ToC byte for frame type `ft` (WmfDecBytesPerFrame:
/// 12,13,15,17,19,20,26,31 for FT 0-7, 5 for SID FT 8, 0 for FT 15). `None` for FT 9..=14 or ft > 15.
#[inline]
pub fn frame_bytes(ft: u8) -> Option<usize> {
    match ft {
        0..=8 | 15 => Some(WMFDECBYTESPERFRAME[ft as usize] as usize),
        _ => None,
    }
}

/// C wmf_to_ets: unpack IF1 payload bits into ETS order, one bit per i16 (`ets` is zero-initialised
/// by the caller; at most MAX_SERIAL_SIZE entries are written). `ft` in 0..=8 (8 = SID, 39 bits).
pub fn wmf_to_ets(ft: u8, payload: &[u8], ets: &mut [i16; MAX_SERIAL_SIZE]) {
    let n = NUMOFBITS[ft as usize] as usize;
    let bit = |i: usize| -> i16 { ((payload[i >> 3] >> ((!i) & 7)) & 1) as i16 };
    if ft < 8 {
        let tab: &[i16] = match ft {
            0 => &REORDERBITS_MR475,
            1 => &REORDERBITS_MR515,
            2 => &REORDERBITS_MR59,
            3 => &REORDERBITS_MR67,
            4 => &REORDERBITS_MR74,
            5 => &REORDERBITS_MR795,
            6 => &REORDERBITS_MR102,
            _ => &REORDERBITS_MR122,
        };
        for i in (0..n).rev() {
            ets[tab[i] as usize] = bit(i);
        }
    } else {
        for i in (0..n).rev() {
            ets[i] = bit(i);
        }
    }
}

/// C Bits2prm: ETS serial bits -> parameters (prm must hold MAX_PRM_SIZE entries).
pub fn bits2prm(mode: Mode, bits: &[i16], prm: &mut [i16]) {
    let (prmno, bitno): (usize, &[i16]) = match mode {
        Mode::Mr475 => (PRMNO[0] as usize, &BITNO_MR475),
        Mode::Mr515 => (PRMNO[1] as usize, &BITNO_MR515),
        Mode::Mr59 => (PRMNO[2] as usize, &BITNO_MR59),
        Mode::Mr67 => (PRMNO[3] as usize, &BITNO_MR67),
        Mode::Mr74 => (PRMNO[4] as usize, &BITNO_MR74),
        Mode::Mr795 => (PRMNO[5] as usize, &BITNO_MR795),
        Mode::Mr102 => (PRMNO[6] as usize, &BITNO_MR102),
        Mode::Mr122 => (PRMNO[7] as usize, &BITNO_MR122),
        Mode::MrDtx => (PRMNO[8] as usize, &BITNO_MRDTX),
    };
    let mut pos = 0usize;
    for i in 0..prmno {
        let nb = bitno[i] as usize;
        let mut v: i16 = 0;
        for k in 0..nb {
            v = (v << 1) | bits[pos + k];
        }
        prm[i] = v;
        pos += nb;
    }
}

/// Parse one IF1 frame at the start of `data`: ToC byte (F, FT(4), Q, 2 pad bits) followed by the
/// payload. Follows opencore AMRDecode(MIME_IETF): the F bit and Q bit do not alter decoding;
/// FT 9..=14 is an error; FT 15 yields NoData with mode `prev_mode`; FT 8 yields SidFirst
/// (STI bit 35 == 0) or SidUpdate (STI == 1) with the mode taken from bits 36..38 (LSB first).
/// For decoding feed `frame_type`, `mode` and `prm` to Decoder_amr (post filter uses `mode`).
/// The glue should remember `mode` as the next `prev_mode`.
pub fn parse_frame(data: &[u8], prev_mode: Mode) -> Result<ParsedFrame, BitstreamError> {
    let hdr = *data.first().ok_or(BitstreamError::Truncated)?;
    let ft = (hdr >> 3) & 0x0f;
    let q = (hdr & 0x04) != 0;
    let nbytes = frame_bytes(ft).ok_or(BitstreamError::InvalidFrameType)?;
    if data.len() < 1 + nbytes {
        return Err(BitstreamError::Truncated);
    }
    let payload = &data[1..1 + nbytes];
    let mut prm = [0i16; MAX_PRM_SIZE + 1];
    let mut ets = [0i16; MAX_SERIAL_SIZE];
    let (frame_type, mode);
    if ft <= 7 {
        mode = Mode::from_index(ft as usize).unwrap();
        frame_type = RxFrameType::SpeechGood;
        wmf_to_ets(ft, payload, &mut ets);
        bits2prm(mode, &ets, &mut prm);
    } else if ft == 8 {
        wmf_to_ets(ft, payload, &mut ets);
        let m = ets[36] as usize | ((ets[37] as usize) << 1) | ((ets[38] as usize) << 2);
        mode = Mode::from_index(m).unwrap();
        frame_type = if ets[35] == 0 { RxFrameType::SidFirst } else { RxFrameType::SidUpdate };
        bits2prm(Mode::MrDtx, &ets, &mut prm);
    } else {
        mode = prev_mode;
        frame_type = RxFrameType::NoData;
    }
    Ok(ParsedFrame { frame_type, mode, ft, q, prm, consumed: 1 + nbytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(frame_bytes(7), Some(31));
        assert_eq!(frame_bytes(8), Some(5));
        assert_eq!(frame_bytes(15), Some(0));
        assert_eq!(frame_bytes(10), None);
        // (bits + 7) / 8 for each mode
        for ft in 0..8usize {
            assert_eq!((NUMOFBITS[ft] as usize + 7) / 8, WMFDECBYTESPERFRAME[ft] as usize);
        }
    }

    #[test]
    fn parse_mr122_zero_and_nodata() {
        let f = [7u8 << 3 | 4; 32];
        let p = parse_frame(&f, Mode::Mr475).unwrap();
        assert_eq!(p.frame_type, RxFrameType::SpeechGood);
        assert_eq!(p.mode, Mode::Mr122);
        assert_eq!(p.consumed, 32);
        let p = parse_frame(&[15 << 3], Mode::Mr67).unwrap();
        assert_eq!(p.frame_type, RxFrameType::NoData);
        assert_eq!(p.mode, Mode::Mr67);
        assert_eq!(parse_frame(&[9 << 3], Mode::Mr67).unwrap_err(), BitstreamError::InvalidFrameType);
        assert_eq!(parse_frame(&[7 << 3, 0], Mode::Mr67).unwrap_err(), BitstreamError::Truncated);
    }
}
