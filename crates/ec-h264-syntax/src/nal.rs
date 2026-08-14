//! NAL unit framing: Annex B start-code scan and emulation-prevention
//! removal (spec 7.3.1, 7.4.1, B.1).

use ec_core::error::{Error, Result};

/// `nal_unit_type` code points (spec Table 7-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NalUnitType {
    /// 1 — coded slice of a non-IDR picture.
    Slice,
    /// 2 — coded slice data partition A.
    SliceDataA,
    /// 3 — coded slice data partition B.
    SliceDataB,
    /// 4 — coded slice data partition C.
    SliceDataC,
    /// 5 — coded slice of an IDR picture.
    SliceIdr,
    /// 6 — supplemental enhancement information.
    Sei,
    /// 7 — sequence parameter set.
    Sps,
    /// 8 — picture parameter set.
    Pps,
    /// 9 — access unit delimiter.
    AccessUnitDelimiter,
    /// 10 — end of sequence.
    EndOfSequence,
    /// 11 — end of stream.
    EndOfStream,
    /// 12 — filler data.
    Filler,
    /// 13 — SPS extension.
    SpsExtension,
    /// 14 — prefix NAL unit (SVC/MVC).
    Prefix,
    /// 15 — subset SPS (SVC/MVC).
    SubsetSps,
    /// 19 — auxiliary coded picture slice.
    SliceAux,
    /// 20 — slice extension (SVC/MVC).
    SliceExtension,
    /// 21 — slice extension for depth/3D.
    SliceDepth,
    /// Any reserved or unspecified code point, carried verbatim.
    Other(u8),
}

impl NalUnitType {
    /// Map the 5-bit code point.
    pub fn from_code(code: u8) -> NalUnitType {
        use NalUnitType::*;
        match code {
            1 => Slice,
            2 => SliceDataA,
            3 => SliceDataB,
            4 => SliceDataC,
            5 => SliceIdr,
            6 => Sei,
            7 => Sps,
            8 => Pps,
            9 => AccessUnitDelimiter,
            10 => EndOfSequence,
            11 => EndOfStream,
            12 => Filler,
            13 => SpsExtension,
            14 => Prefix,
            15 => SubsetSps,
            19 => SliceAux,
            20 => SliceExtension,
            21 => SliceDepth,
            n => Other(n),
        }
    }

    /// True for the VCL slice types the decoder reconstructs pictures from.
    pub fn is_slice(&self) -> bool {
        matches!(self, NalUnitType::Slice | NalUnitType::SliceIdr)
    }
}

/// First byte of a NAL unit (spec 7.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NalHeader {
    /// `nal_ref_idc`: 0 = disposable, >0 = used for reference.
    pub ref_idc: u8,
    /// `nal_unit_type`.
    pub unit_type: NalUnitType,
}

impl NalHeader {
    /// Parse the header byte; `forbidden_zero_bit` set is a corrupt stream.
    pub fn parse(byte: u8) -> Result<NalHeader> {
        if byte & 0x80 != 0 {
            return Err(Error::corrupt("NAL forbidden_zero_bit set"));
        }
        Ok(NalHeader {
            ref_idc: (byte >> 5) & 0x3,
            unit_type: NalUnitType::from_code(byte & 0x1F),
        })
    }

    /// True for an IDR slice.
    pub fn is_idr(&self) -> bool {
        self.unit_type == NalUnitType::SliceIdr
    }
}

/// Iterator over NAL unit payloads in an Annex B byte stream (spec B.1).
///
/// Yields each NAL unit as a sub-slice of the input **including** its header
/// byte, with start codes (`00 00 01` / `00 00 00 01`) stripped and trailing
/// zero padding removed. Borrowing, zero-copy: emulation prevention is left
/// in place for [`unescape_rbsp`] to strip into a reusable buffer.
#[derive(Debug, Clone)]
pub struct AnnexBIter<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> AnnexBIter<'a> {
    /// Iterate over `data`, skipping anything before the first start code.
    pub fn new(data: &'a [u8]) -> AnnexBIter<'a> {
        AnnexBIter { data, pos: 0 }
    }

    /// Position of the next `00 00 01` at or after `from`, or `len`.
    fn next_start_code(&self, from: usize) -> Option<usize> {
        let d = self.data;
        let mut i = from;
        // memchr-style scan on the middle zero of `00 00 01`.
        while i + 2 < d.len() {
            if d[i + 1] != 0 {
                i += 2;
            } else if d[i] != 0 {
                i += 1;
            } else if d[i + 2] == 1 {
                return Some(i);
            } else {
                i += 1;
            }
        }
        None
    }
}

impl<'a> Iterator for AnnexBIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        let start = self.next_start_code(self.pos)? + 3;
        if start >= self.data.len() {
            self.pos = self.data.len();
            return None;
        }
        let (end, next_pos) = match self.next_start_code(start) {
            Some(sc) => (sc, sc),
            None => (self.data.len(), self.data.len()),
        };
        self.pos = next_pos;
        // Trailing zero bytes before the next start code belong to the start
        // code prefix / trailing_zero_8bits, not to this NAL unit.
        let mut end = end;
        while end > start && self.data[end - 1] == 0 {
            end -= 1;
        }
        if end == start {
            // Empty NAL unit (all-zero span): skip to the next one.
            return self.next();
        }
        Some(&self.data[start..end])
    }
}

/// Insert emulation prevention bytes into an RBSP (spec 7.4.1.1), the inverse
/// of [`unescape_rbsp`]: a `00 00 00`, `00 00 01`, `00 00 02` or `00 00 03`
/// run gets an `emulation_prevention_three_byte` before its last byte, and a
/// payload ending in two zero bytes gets one appended.
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
/// Strip emulation-prevention bytes (spec 7.4.1.1): every `00 00 03` becomes
/// `00 00`. `nal` is the NAL payload **after** the header byte; the result is
/// appended into `out`, which is cleared first and whose capacity is reused
/// across calls — the steady-state decode loop performs no allocation once
/// `out` has grown to the largest NAL size.
pub fn unescape_rbsp(nal: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(nal.len());
    let mut zeros = 0u32;
    let mut i = 0;
    while i < nal.len() {
        let b = nal[i];
        if zeros >= 2 && b == 3 {
            // Emulation prevention byte: drop it, next byte restarts the count.
            zeros = 0;
            i += 1;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annexb_split_three_and_four_byte_codes() {
        let data = [
            0x00, 0x00, 0x00, 0x01, 0x67, 0xAA, // SPS
            0x00, 0x00, 0x01, 0x68, 0xBB, // PPS
            0x00, 0x00, 0x00, 0x01, 0x65, 0xCC, 0xDD, // IDR
        ];
        let nals: Vec<&[u8]> = AnnexBIter::new(&data).collect();
        assert_eq!(nals.len(), 3);
        assert_eq!(nals[0], &[0x67, 0xAA]);
        assert_eq!(nals[1], &[0x68, 0xBB]);
        assert_eq!(nals[2], &[0x65, 0xCC, 0xDD]);
    }

    #[test]
    fn annexb_trailing_zeros_trimmed_and_garbage_prefix_skipped() {
        let data = [
            0xDE, 0xAD, // garbage before the first start code
            0x00, 0x00, 0x01, 0x41, 0x01, 0x00, 0x00, // trailing zeros
            0x00, 0x00, 0x01, 0x41, 0x02,
        ];
        let nals: Vec<&[u8]> = AnnexBIter::new(&data).collect();
        assert_eq!(nals.len(), 2);
        assert_eq!(nals[0], &[0x41, 0x01]);
        assert_eq!(nals[1], &[0x41, 0x02]);
    }

    #[test]
    fn unescape_removes_only_emulation_bytes() {
        let mut out = Vec::new();
        unescape_rbsp(&[0x00, 0x00, 0x03, 0x00, 0x01], &mut out);
        assert_eq!(out, &[0x00, 0x00, 0x00, 0x01]);
        // 00 00 03 03 -> 00 00 03: only the first 03 is the escape.
        unescape_rbsp(&[0x00, 0x00, 0x03, 0x03, 0x01], &mut out);
        assert_eq!(out, &[0x00, 0x00, 0x03, 0x01]);
        // A 03 not preceded by two zeros stays.
        unescape_rbsp(&[0x01, 0x03, 0x00, 0x03], &mut out);
        assert_eq!(out, &[0x01, 0x03, 0x00, 0x03]);
        // Reuse does not leak previous contents.
        unescape_rbsp(&[0x7F], &mut out);
        assert_eq!(out, &[0x7F]);
    }

    #[test]
    fn header_parse_and_forbidden_bit() {
        let h = NalHeader::parse(0x67).unwrap();
        assert_eq!(h.ref_idc, 3);
        assert_eq!(h.unit_type, NalUnitType::Sps);
        assert!(!h.is_idr());
        let h = NalHeader::parse(0x65).unwrap();
        assert!(h.is_idr());
        assert!(h.unit_type.is_slice());
        assert!(NalHeader::parse(0x80).is_err());
    }
}
