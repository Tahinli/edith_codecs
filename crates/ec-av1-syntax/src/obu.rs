//! OBU framing (spec 5.3) and the bit-reading descriptors AV1 adds on top of
//! the family [`BitReader`].

use ec_core::{BitReader, Error, Result};

/// OBU type (spec 6.2.2, `obu_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObuType {
    /// `OBU_SEQUENCE_HEADER`, 1.
    SequenceHeader,
    /// `OBU_TEMPORAL_DELIMITER`, 2 — a zero-length OBU marking a temporal unit.
    TemporalDelimiter,
    /// `OBU_FRAME_HEADER`, 3.
    FrameHeader,
    /// `OBU_TILE_GROUP`, 4.
    TileGroup,
    /// `OBU_METADATA`, 5.
    Metadata,
    /// `OBU_FRAME`, 6 — a frame header and its tile group in one OBU.
    Frame,
    /// `OBU_REDUNDANT_FRAME_HEADER`, 7 — a repeat of the frame header, for
    /// error resilience; identical syntax, and it must not change any state.
    RedundantFrameHeader,
    /// `OBU_TILE_LIST`, 8 — large scale tile mode.
    TileList,
    /// `OBU_PADDING`, 15.
    Padding,
    /// A type this specification version does not define; skip by `obu_size`.
    Reserved(u8),
}

impl ObuType {
    fn from_code(code: u8) -> ObuType {
        match code {
            1 => ObuType::SequenceHeader,
            2 => ObuType::TemporalDelimiter,
            3 => ObuType::FrameHeader,
            4 => ObuType::TileGroup,
            5 => ObuType::Metadata,
            6 => ObuType::Frame,
            7 => ObuType::RedundantFrameHeader,
            8 => ObuType::TileList,
            15 => ObuType::Padding,
            other => ObuType::Reserved(other),
        }
    }
}

/// An OBU header (spec 5.3.2), including the extension header when present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObuHeader {
    /// `obu_type`.
    pub obu_type: ObuType,
    /// `obu_extension_flag`: a temporal/spatial layer id follows.
    pub extension_flag: bool,
    /// `obu_has_size_field`: an explicit `leb128` size follows the header. The
    /// low-overhead bitstream format requires this; Annex B length-delimited
    /// streams may omit it, in which case the size comes from the container.
    pub has_size_field: bool,
    /// `temporal_id`, 0 when there is no extension header.
    pub temporal_id: u8,
    /// `spatial_id`, 0 when there is no extension header.
    pub spatial_id: u8,
}

impl ObuHeader {
    /// Parse an OBU header from `r` (spec 5.3.2).
    pub fn parse(r: &mut BitReader<'_>) -> Result<ObuHeader> {
        if r.read_bit()? {
            return Err(Error::corrupt("AV1 obu_forbidden_bit is set"));
        }
        let obu_type = ObuType::from_code(r.read_bits(4)? as u8);
        let extension_flag = r.read_bit()?;
        let has_size_field = r.read_bit()?;
        let _obu_reserved_1bit = r.read_bit()?;
        let (temporal_id, spatial_id) = if extension_flag {
            let temporal_id = r.read_bits(3)? as u8;
            let spatial_id = r.read_bits(2)? as u8;
            let _reserved = r.read_bits(3)?;
            (temporal_id, spatial_id)
        } else {
            (0, 0)
        };
        Ok(ObuHeader {
            obu_type,
            extension_flag,
            has_size_field,
            temporal_id,
            spatial_id,
        })
    }
}

/// `leb128()` (spec 4.10.5): little-endian base-128, at most 8 bytes.
///
/// The spec caps the value at 32 bits, so a longer encoding or a value that
/// does not fit is [`Error::Corrupt`] rather than a silently truncated size.
pub fn read_leb128(r: &mut BitReader<'_>) -> Result<u32> {
    let mut value = 0u64;
    for i in 0..8 {
        let byte = r.read_bits(8)? as u64;
        value |= (byte & 0x7f) << (i * 7);
        if byte & 0x80 == 0 {
            return u32::try_from(value)
                .map_err(|_| Error::corrupt(format!("AV1 leb128 value {value} exceeds 32 bits")));
        }
    }
    Err(Error::corrupt("AV1 leb128 longer than 8 bytes"))
}

/// `uvlc()` (spec 4.10.3): a variable length unsigned integer.
pub fn read_uvlc(r: &mut BitReader<'_>) -> Result<u32> {
    let mut leading_zeros = 0u32;
    while !r.read_bit()? {
        leading_zeros += 1;
        if leading_zeros >= 32 {
            // The spec's own escape: 32 leading zeros means (1 << 32) - 1.
            return Ok(u32::MAX);
        }
    }
    if leading_zeros == 0 {
        return Ok(0);
    }
    let value = r.read_bits(leading_zeros)?;
    Ok(value + (1u32 << leading_zeros) - 1)
}

/// `su(n)` (spec 4.10.6): an `n`-bit two's complement signed integer.
pub fn read_su(r: &mut BitReader<'_>, n: u32) -> Result<i32> {
    r.read_signed(n)
}

/// `ns(n)` (spec 4.10.7): a non-symmetric unsigned integer below `n`.
pub fn read_ns(r: &mut BitReader<'_>, n: u32) -> Result<u32> {
    if n <= 1 {
        return Ok(0);
    }
    let w = floor_log2(n) + 1;
    let m = (1u32 << w) - n;
    let v = r.read_bits(w - 1)?;
    if v < m {
        return Ok(v);
    }
    let extra = u32::from(r.read_bit()?);
    Ok((v << 1) - m + extra)
}

/// `le(n)` (spec 4.10.4): an `n`-byte little-endian integer, byte aligned.
pub fn read_le(r: &mut BitReader<'_>, n: u32) -> Result<u32> {
    let mut value = 0u32;
    for i in 0..n {
        value |= r.read_bits(8)? << (i * 8);
    }
    Ok(value)
}

/// `FloorLog2(x)` (spec 4.7), with `FloorLog2(0) == 0` as the spec's callers assume.
pub fn floor_log2(x: u32) -> u32 {
    31 - x.max(1).leading_zeros()
}

/// `tile_log2(blkSize, target)` (spec 5.9.15): the smallest `k` with
/// `blkSize << k >= target`.
pub fn tile_log2(blk_size: u32, target: u32) -> u32 {
    let mut k = 0;
    while (blk_size.max(1) << k) < target {
        k += 1;
        if k >= 32 {
            break;
        }
    }
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leb128_round_trips_the_spec_examples() {
        assert_eq!(read_leb128(&mut BitReader::new(&[0x00])).unwrap(), 0);
        assert_eq!(read_leb128(&mut BitReader::new(&[0x7f])).unwrap(), 127);
        assert_eq!(
            read_leb128(&mut BitReader::new(&[0x80, 0x01])).unwrap(),
            128
        );
        assert_eq!(
            read_leb128(&mut BitReader::new(&[0xe5, 0x8e, 0x26])).unwrap(),
            624_485
        );
        // Nine continuation bytes cannot encode anything legal.
        assert!(read_leb128(&mut BitReader::new(&[0x80; 9])).is_err());
        // A value past 32 bits is refused rather than wrapped.
        assert!(read_leb128(&mut BitReader::new(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x01])).is_err());
    }

    #[test]
    fn uvlc_reads_the_exp_golomb_shape() {
        assert_eq!(read_uvlc(&mut BitReader::new(&[0b1000_0000])).unwrap(), 0);
        assert_eq!(read_uvlc(&mut BitReader::new(&[0b0100_0000])).unwrap(), 1);
        assert_eq!(read_uvlc(&mut BitReader::new(&[0b0110_0000])).unwrap(), 2);
        assert_eq!(read_uvlc(&mut BitReader::new(&[0b0010_0000])).unwrap(), 3);
        // 32 leading zeros is the spec's escape to the maximum value.
        assert_eq!(read_uvlc(&mut BitReader::new(&[0; 8])).unwrap(), u32::MAX);
    }

    #[test]
    fn ns_matches_the_worked_table() {
        // n = 5: w = 3, m = 3, so 0..2 take two bits and 3..4 take three.
        let read = |bits: u8, len: usize| {
            let mut r = BitReader::new(std::slice::from_ref(&bits));
            let v = read_ns(&mut r, 5).unwrap();
            assert!(r.bit_position() as usize <= len * 8);
            v
        };
        assert_eq!(read(0b0000_0000, 1), 0);
        assert_eq!(read(0b0100_0000, 1), 1);
        assert_eq!(read(0b1000_0000, 1), 2);
        assert_eq!(read(0b1100_0000, 1), 3);
        assert_eq!(read(0b1110_0000, 1), 4);
        assert_eq!(read_ns(&mut BitReader::new(&[0xff]), 1).unwrap(), 0);
    }

    #[test]
    fn tile_log2_and_floor_log2() {
        assert_eq!(floor_log2(1), 0);
        assert_eq!(floor_log2(8), 3);
        assert_eq!(floor_log2(9), 3);
        assert_eq!(tile_log2(1, 1), 0);
        assert_eq!(tile_log2(1, 5), 3);
        assert_eq!(tile_log2(4, 16), 2);
    }

    #[test]
    fn obu_header_rejects_the_forbidden_bit() {
        assert!(ObuHeader::parse(&mut BitReader::new(&[0x80])).is_err());
        // 0x0a: type 1 (sequence header), no extension, has_size_field.
        let h = ObuHeader::parse(&mut BitReader::new(&[0x0a, 0x00])).unwrap();
        assert_eq!(h.obu_type, ObuType::SequenceHeader);
        assert!(h.has_size_field);
        assert!(!h.extension_flag);
    }
}
