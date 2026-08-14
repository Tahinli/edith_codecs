//! NAL units and the RBSP layer (Rec. ITU-T H.264 clauses 7.3.1, 7.4.1, B.1).
//!
//! Three things live here, in the order a decoder needs them:
//!
//! 1. [`annex_b_units`] splits a byte stream (Annex B) into NAL units.
//! 2. [`rbsp_from_ebsp`] undoes the `emulation_prevention_three_byte` escaping
//!    of clause 7.4.1.1, turning an EBSP into an RBSP.
//! 3. [`RbspReader`] reads that RBSP, adding `more_rbsp_data()` (clause 7.2)
//!    which every `..._rbsp()` syntax structure needs to know when to stop.

use ec_core::bitio::BitReader;
use ec_core::error::{Error, Result};

/// `nal_unit_type` (Table 7-1).
///
/// Only the values the family acts on are named; the rest travel as
/// [`NalUnitType::Reserved`] / [`NalUnitType::Unspecified`] so a parser never
/// loses a code point it cannot interpret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NalUnitType {
    /// 1: coded slice of a non-IDR picture.
    NonIdrSlice,
    /// 2: coded slice data partition A.
    SlicePartitionA,
    /// 3: coded slice data partition B.
    SlicePartitionB,
    /// 4: coded slice data partition C.
    SlicePartitionC,
    /// 5: coded slice of an IDR picture.
    IdrSlice,
    /// 6: supplemental enhancement information.
    Sei,
    /// 7: sequence parameter set.
    Sps,
    /// 8: picture parameter set.
    Pps,
    /// 9: access unit delimiter.
    AccessUnitDelimiter,
    /// 10: end of sequence.
    EndOfSequence,
    /// 11: end of stream.
    EndOfStream,
    /// 12: filler data.
    FillerData,
    /// 13: sequence parameter set extension.
    SpsExtension,
    /// 14: prefix NAL unit.
    Prefix,
    /// 15: subset sequence parameter set.
    SubsetSps,
    /// 19: coded slice of an auxiliary coded picture without partitioning.
    AuxiliarySlice,
    /// 20: coded slice extension (SVC/MVC).
    SliceExtension,
    /// 0 and 24..=31: unspecified.
    Unspecified(u8),
    /// Everything reserved by the specification.
    Reserved(u8),
}

impl NalUnitType {
    /// The `nal_unit_type` code point this value stands for.
    pub fn code(self) -> u8 {
        match self {
            NalUnitType::NonIdrSlice => 1,
            NalUnitType::SlicePartitionA => 2,
            NalUnitType::SlicePartitionB => 3,
            NalUnitType::SlicePartitionC => 4,
            NalUnitType::IdrSlice => 5,
            NalUnitType::Sei => 6,
            NalUnitType::Sps => 7,
            NalUnitType::Pps => 8,
            NalUnitType::AccessUnitDelimiter => 9,
            NalUnitType::EndOfSequence => 10,
            NalUnitType::EndOfStream => 11,
            NalUnitType::FillerData => 12,
            NalUnitType::SpsExtension => 13,
            NalUnitType::Prefix => 14,
            NalUnitType::SubsetSps => 15,
            NalUnitType::AuxiliarySlice => 19,
            NalUnitType::SliceExtension => 20,
            NalUnitType::Unspecified(v) | NalUnitType::Reserved(v) => v,
        }
    }

    /// Map a 5-bit code point onto this enum.
    pub fn from_code(code: u8) -> NalUnitType {
        match code {
            1 => NalUnitType::NonIdrSlice,
            2 => NalUnitType::SlicePartitionA,
            3 => NalUnitType::SlicePartitionB,
            4 => NalUnitType::SlicePartitionC,
            5 => NalUnitType::IdrSlice,
            6 => NalUnitType::Sei,
            7 => NalUnitType::Sps,
            8 => NalUnitType::Pps,
            9 => NalUnitType::AccessUnitDelimiter,
            10 => NalUnitType::EndOfSequence,
            11 => NalUnitType::EndOfStream,
            12 => NalUnitType::FillerData,
            13 => NalUnitType::SpsExtension,
            14 => NalUnitType::Prefix,
            15 => NalUnitType::SubsetSps,
            19 => NalUnitType::AuxiliarySlice,
            20 => NalUnitType::SliceExtension,
            0 | 24..=31 => NalUnitType::Unspecified(code),
            other => NalUnitType::Reserved(other),
        }
    }

    /// True for the VCL types (1..=5), the ones carrying slice data.
    pub fn is_vcl(self) -> bool {
        matches!(self.code(), 1..=5)
    }
}

/// One NAL unit: its header fields plus the de-escaped payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NalUnit {
    /// `nal_ref_idc` (clause 7.4.1): 0 means the unit is not used for reference.
    pub nal_ref_idc: u8,
    /// `nal_unit_type` (Table 7-1).
    pub nal_unit_type: NalUnitType,
    /// The RBSP: payload with `emulation_prevention_three_byte`s removed.
    pub rbsp: Vec<u8>,
}

impl NalUnit {
    /// Parse one NAL unit (clause 7.3.1) from its bytes, header byte included.
    ///
    /// `forbidden_zero_bit` set is a corrupt unit — that bit exists precisely so
    /// a start-code emulation in a transport layer is detectable.
    pub fn parse(bytes: &[u8]) -> Result<NalUnit> {
        let &first = bytes
            .first()
            .ok_or_else(|| Error::corrupt("H.264 NAL unit: empty"))?;
        if first & 0x80 != 0 {
            return Err(Error::corrupt("H.264 NAL unit: forbidden_zero_bit is 1"));
        }
        let nal_unit_type = NalUnitType::from_code(first & 0x1F);
        if matches!(
            nal_unit_type,
            NalUnitType::Prefix | NalUnitType::SliceExtension | NalUnitType::Reserved(21)
        ) {
            return Err(Error::unsupported(
                format!("H.264 nal_unit_type {}", nal_unit_type.code()),
                "SVC/MVC/3D header extensions are not parsed",
            ));
        }
        Ok(NalUnit {
            nal_ref_idc: (first >> 5) & 0x03,
            nal_unit_type,
            rbsp: rbsp_from_ebsp(&bytes[1..]),
        })
    }
}

/// Remove `emulation_prevention_three_byte`s (clause 7.4.1.1).
///
/// Inside a NAL unit payload the encoder inserts a `0x03` after any `0x00 0x00`
/// that would otherwise be followed by a byte `<= 0x03`, so that no start code
/// prefix can appear by accident. The decoder drops exactly those bytes.
pub fn rbsp_from_ebsp(ebsp: &[u8]) -> Vec<u8> {
    let mut rbsp = Vec::with_capacity(ebsp.len());
    let mut zeros = 0usize;
    for &b in ebsp {
        if zeros >= 2 && b == 0x03 {
            zeros = 0;
            continue;
        }
        if b == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
        rbsp.push(b);
    }
    rbsp
}

/// Insert `emulation_prevention_three_byte`s, the inverse of [`rbsp_from_ebsp`].
///
/// Present so the round trip is testable and so an encoder or a bitstream
/// rewriter has one implementation to share.
pub fn ebsp_from_rbsp(rbsp: &[u8]) -> Vec<u8> {
    let mut ebsp = Vec::with_capacity(rbsp.len());
    let mut zeros = 0usize;
    for &b in rbsp {
        if zeros >= 2 && b <= 0x03 {
            ebsp.push(0x03);
            zeros = 0;
        }
        if b == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
        ebsp.push(b);
    }
    ebsp
}

/// Split an Annex B byte stream (clause B.1) into NAL unit byte ranges.
///
/// A unit runs from just after a `0x000001` start code prefix to just before
/// the next one (or the end), with trailing zero bytes trimmed: those belong to
/// `trailing_zero_8bits`, or to the next unit's `zero_byte`, never to the
/// payload.
pub fn annex_b_units(stream: &[u8]) -> Vec<&[u8]> {
    let mut units = Vec::new();
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 2 < stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    for (n, &start) in starts.iter().enumerate() {
        let end = starts
            .get(n + 1)
            .map(|&next| next - 3)
            .unwrap_or(stream.len());
        let mut end = end;
        // The zero_byte of the following start code, and trailing_zero_8bits,
        // are not payload.
        while end > start && stream[end - 1] == 0 {
            end -= 1;
        }
        if end > start {
            units.push(&stream[start..end]);
        }
    }
    units
}

/// A [`BitReader`] over an RBSP, with the clause 7.2 `more_rbsp_data()` test.
#[derive(Debug, Clone)]
pub struct RbspReader<'a> {
    reader: BitReader<'a>,
    /// Bit position of the `rbsp_stop_one_bit`: the last bit set in the RBSP.
    stop_bit: Option<u64>,
}

impl<'a> RbspReader<'a> {
    /// A reader over `rbsp`, positioned at its first bit.
    pub fn new(rbsp: &'a [u8]) -> RbspReader<'a> {
        // MSB-first bit numbering: the least significant set bit of the last
        // non-zero byte sits at offset `7 - trailing_zeros` inside that byte.
        let stop_bit = rbsp
            .iter()
            .rposition(|&b| b != 0)
            .map(|byte| byte as u64 * 8 + (7 - rbsp[byte].trailing_zeros()) as u64);
        RbspReader {
            reader: BitReader::new(rbsp),
            stop_bit,
        }
    }

    /// Mutable access to the underlying bit reader.
    pub fn bits(&mut self) -> &mut BitReader<'a> {
        &mut self.reader
    }

    /// `more_rbsp_data()` (clause 7.2): true while data other than the
    /// `rbsp_stop_one_bit` and its zero padding remains.
    pub fn more_rbsp_data(&self) -> bool {
        match self.stop_bit {
            Some(stop) => self.reader.bit_position() < stop,
            None => false,
        }
    }
}
