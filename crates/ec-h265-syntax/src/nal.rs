//! NAL units: the two-byte header, RBSP escaping and Annex-B framing.

use ec_core::error::{Error, Result};

/// `nal_unit_type` values this family names (Table 7-1).
///
/// The numeric value is the wire value; unnamed types travel as
/// [`NalUnitType::Other`] rather than being refused, because a parser that
/// cannot skip a NAL it does not know is a parser that breaks on the next
/// extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NalUnitType {
    /// Coded slice of a non-TSA, non-STSA trailing picture (sub-layer non-ref).
    TrailN,
    /// Coded slice of a non-TSA, non-STSA trailing picture (reference).
    TrailR,
    /// Coded slice of an IDR picture that may have leading RADL pictures.
    IdrWRadl,
    /// Coded slice of an IDR picture with no leading pictures.
    IdrNLp,
    /// Coded slice of a CRA picture.
    CraNut,
    /// Video parameter set.
    Vps,
    /// Sequence parameter set.
    Sps,
    /// Picture parameter set.
    Pps,
    /// Access unit delimiter.
    Aud,
    /// End of sequence.
    EosNut,
    /// End of bitstream.
    EobNut,
    /// Filler data.
    FdNut,
    /// SEI that precedes the pictures it describes.
    PrefixSei,
    /// SEI that follows them — where the decoded picture hash lives.
    SuffixSei,
    /// Anything else, carried verbatim.
    Other(u8),
}

impl NalUnitType {
    /// The wire value.
    pub fn code(self) -> u8 {
        match self {
            NalUnitType::TrailN => 0,
            NalUnitType::TrailR => 1,
            NalUnitType::IdrWRadl => 19,
            NalUnitType::IdrNLp => 20,
            NalUnitType::CraNut => 21,
            NalUnitType::Vps => 32,
            NalUnitType::Sps => 33,
            NalUnitType::Pps => 34,
            NalUnitType::Aud => 35,
            NalUnitType::EosNut => 36,
            NalUnitType::EobNut => 37,
            NalUnitType::FdNut => 38,
            NalUnitType::PrefixSei => 39,
            NalUnitType::SuffixSei => 40,
            NalUnitType::Other(v) => v,
        }
    }

    /// The named type for a wire value.
    pub fn from_code(code: u8) -> NalUnitType {
        match code {
            0 => NalUnitType::TrailN,
            1 => NalUnitType::TrailR,
            19 => NalUnitType::IdrWRadl,
            20 => NalUnitType::IdrNLp,
            21 => NalUnitType::CraNut,
            32 => NalUnitType::Vps,
            33 => NalUnitType::Sps,
            34 => NalUnitType::Pps,
            35 => NalUnitType::Aud,
            36 => NalUnitType::EosNut,
            37 => NalUnitType::EobNut,
            38 => NalUnitType::FdNut,
            39 => NalUnitType::PrefixSei,
            40 => NalUnitType::SuffixSei,
            v => NalUnitType::Other(v),
        }
    }

    /// True for the video coding layer types (0..=31), the ones that carry slices.
    pub fn is_vcl(self) -> bool {
        self.code() < 32
    }

    /// True for the IRAP range (16..=23): a decoder may be started here.
    pub fn is_irap(self) -> bool {
        (16..=23).contains(&self.code())
    }

    /// True for the two IDR types, which is what "every AU is a sync point" means.
    pub fn is_idr(self) -> bool {
        matches!(self, NalUnitType::IdrWRadl | NalUnitType::IdrNLp)
    }
}

/// The two-byte NAL unit header (7.3.1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NalHeader {
    /// What the payload is.
    pub nal_type: NalUnitType,
    /// Layer id; 0 for everything this family writes.
    pub layer_id: u8,
    /// `nuh_temporal_id_plus1 - 1`.
    pub temporal_id: u8,
}

impl NalHeader {
    /// A base-layer header at temporal id 0.
    pub fn new(nal_type: NalUnitType) -> NalHeader {
        NalHeader {
            nal_type,
            layer_id: 0,
            temporal_id: 0,
        }
    }

    /// The two header bytes.
    pub fn to_bytes(self) -> [u8; 2] {
        let b0 = (self.nal_type.code() << 1) | (self.layer_id >> 5);
        let b1 = ((self.layer_id & 0x1f) << 3) | (self.temporal_id + 1);
        [b0, b1]
    }

    /// Parse the two header bytes, rejecting a set `forbidden_zero_bit`.
    pub fn parse(bytes: &[u8]) -> Result<NalHeader> {
        let (b0, b1) = match bytes {
            [b0, b1, ..] => (*b0, *b1),
            _ => return Err(Error::NeedMore),
        };
        if b0 & 0x80 != 0 {
            return Err(Error::corrupt("HEVC NAL header: forbidden_zero_bit set"));
        }
        let temporal_id_plus1 = b1 & 0x7;
        if temporal_id_plus1 == 0 {
            return Err(Error::corrupt("HEVC NAL header: nuh_temporal_id_plus1 = 0"));
        }
        Ok(NalHeader {
            nal_type: NalUnitType::from_code((b0 >> 1) & 0x3f),
            layer_id: ((b0 & 1) << 5) | (b1 >> 3),
            temporal_id: temporal_id_plus1 - 1,
        })
    }
}

/// Escape an RBSP into a NAL unit payload (7.3.1.1).
///
/// A `0x03` goes in before any byte that would otherwise complete a
/// `00 00 00`..`00 00 03` sequence, so a start code can never appear inside a
/// NAL unit. Trailing `00 00` at the very end is escaped too: the next thing in
/// the stream is a start code, and `00 00 00 01` read backwards from there would
/// swallow the last payload byte.
pub fn escape_rbsp(rbsp: &[u8], out: &mut Vec<u8>) {
    let mut zeros = 0usize;
    for &b in rbsp {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        if b == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
    }
    if zeros >= 2 {
        out.push(3);
    }
}

/// Strip emulation prevention bytes from a NAL unit payload.
pub fn unescape_rbsp(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len());
    let mut zeros = 0usize;
    for &b in payload {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        out.push(b);
        if b == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
    }
    out
}

/// Append one Annex-B NAL unit: start code, header, escaped RBSP.
///
/// The four-byte start code is used for parameter sets and the first slice of a
/// picture, three bytes elsewhere — the same shape ffmpeg writes, and what a
/// caller splitting an access unit back apart expects.
pub fn write_annex_b(out: &mut Vec<u8>, header: NalHeader, rbsp: &[u8], long_start_code: bool) {
    if long_start_code {
        out.extend_from_slice(&[0, 0, 0, 1]);
    } else {
        out.extend_from_slice(&[0, 0, 1]);
    }
    out.extend_from_slice(&header.to_bytes());
    escape_rbsp(rbsp, out);
}

/// One NAL unit found in an Annex-B stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnexBNal<'a> {
    /// Parsed two-byte header.
    pub header: NalHeader,
    /// Payload after the header, still escaped.
    pub payload: &'a [u8],
}

impl AnnexBNal<'_> {
    /// The payload with emulation prevention bytes removed.
    pub fn rbsp(&self) -> Vec<u8> {
        unescape_rbsp(self.payload)
    }
}

/// Split an Annex-B byte stream into NAL units.
///
/// Bytes before the first start code are ignored rather than refused, which is
/// what makes this usable on a stream joined mid-file. A NAL whose header is
/// malformed is skipped, not fatal.
pub fn split_annex_b(data: &[u8]) -> Vec<AnnexBNal<'_>> {
    let mut nals = Vec::new();
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    for (n, &start) in starts.iter().enumerate() {
        let mut end = starts.get(n + 1).map_or(data.len(), |&next| next - 3);
        // A four-byte start code leaves one extra zero at the end of the
        // previous unit; trailing zeros are never part of an RBSP.
        while end > start && data[end - 1] == 0 {
            end -= 1;
        }
        if end < start + 2 {
            continue;
        }
        if let Ok(header) = NalHeader::parse(&data[start..end]) {
            nals.push(AnnexBNal {
                header,
                payload: &data[start + 2..end],
            });
        }
    }
    nals
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        for code in 0u8..64 {
            let h = NalHeader {
                nal_type: NalUnitType::from_code(code),
                layer_id: 0,
                temporal_id: 0,
            };
            let bytes = h.to_bytes();
            assert_eq!(NalHeader::parse(&bytes).unwrap(), h);
        }
        // An IDR slice header is the well-known 0x26 0x01.
        assert_eq!(NalHeader::new(NalUnitType::IdrWRadl).to_bytes(), [0x26, 0x01]);
        assert_eq!(NalHeader::new(NalUnitType::Vps).to_bytes(), [0x40, 0x01]);
        assert!(NalHeader::parse(&[0x80, 0x01]).is_err());
        assert!(NalHeader::parse(&[0x26, 0x00]).is_err());
        assert!(NalHeader::parse(&[0x26]).is_err());
    }

    #[test]
    fn escaping_round_trips_and_kills_start_codes() {
        let cases: &[&[u8]] = &[
            &[],
            &[0, 0, 0],
            &[0, 0, 1],
            &[0, 0, 2],
            &[0, 0, 3],
            &[0, 0, 4],
            &[1, 0, 0, 0, 0, 1],
            &[0, 0],
            &[0xff, 0, 0, 3, 0, 0, 0, 1],
        ];
        for case in cases {
            let mut escaped = Vec::new();
            escape_rbsp(case, &mut escaped);
            assert_eq!(&unescape_rbsp(&escaped)[..], *case, "case {case:?}");
            // No start code emulation survives escaping.
            assert!(
                !escaped.windows(3).any(|w| w == [0, 0, 1]),
                "start code in {escaped:?}"
            );
        }
    }

    #[test]
    fn annex_b_split_finds_units() {
        let mut stream = Vec::new();
        write_annex_b(&mut stream, NalHeader::new(NalUnitType::Vps), &[1, 2, 3], true);
        write_annex_b(
            &mut stream,
            NalHeader::new(NalUnitType::IdrWRadl),
            &[0, 0, 0, 7],
            false,
        );
        let nals = split_annex_b(&stream);
        assert_eq!(nals.len(), 2);
        assert_eq!(nals[0].header.nal_type, NalUnitType::Vps);
        assert_eq!(nals[0].rbsp(), vec![1, 2, 3]);
        assert_eq!(nals[1].header.nal_type, NalUnitType::IdrWRadl);
        assert_eq!(nals[1].rbsp(), vec![0, 0, 0, 7]);
        assert!(nals[1].header.nal_type.is_idr());
        assert!(nals[1].header.nal_type.is_irap());
        assert!(nals[1].header.nal_type.is_vcl());
        assert!(!nals[0].header.nal_type.is_vcl());
    }
}
