//! AV1 OBU framing writer — bit-exact inverse of the readers in
//! [`ec_av1_syntax::obu`], built on the family [`ec_core::BitWriter`].

use ec_av1_syntax::obu::{ObuHeader, ObuType};
use ec_core::BitWriter;

use crate::bits::write_leb128;

/// Map an [`ObuType`] to its 4-bit `obu_type` code (spec 6.2.2); the inverse of
/// `ObuType::from_code`.
pub fn obu_type_code(t: ObuType) -> u8 {
    match t {
        ObuType::SequenceHeader => 1,
        ObuType::TemporalDelimiter => 2,
        ObuType::FrameHeader => 3,
        ObuType::TileGroup => 4,
        ObuType::Metadata => 5,
        ObuType::Frame => 6,
        ObuType::RedundantFrameHeader => 7,
        ObuType::TileList => 8,
        ObuType::Padding => 15,
        ObuType::Reserved(code) => code,
    }
}

/// Write an OBU header (spec 5.3.2) — the inverse of [`ObuHeader::parse`].
///
/// The forbidden bit and every reserved bit are written `0`. The header is
/// always a whole number of bytes: one without, two with the extension header.
pub fn write_obu_header(w: &mut BitWriter, h: &ObuHeader) {
    w.write_bit(false); // obu_forbidden_bit
    w.write_bits(obu_type_code(h.obu_type) as u32, 4);
    w.write_bit(h.extension_flag);
    w.write_bit(h.has_size_field);
    w.write_bit(false); // obu_reserved_1bit
    if h.extension_flag {
        w.write_bits(h.temporal_id as u32, 3);
        w.write_bits(h.spatial_id as u32, 2);
        w.write_bits(0, 3); // extension reserved bits
    }
}

/// Frame `payload` as a complete low-overhead OBU (spec 5.3): the header
/// bytes, then `leb128(obu_size)` when `h.has_size_field`, then the payload.
///
/// The header is byte-aligned, so the size field and payload follow on byte
/// boundaries and the result is a flat byte vector.
pub fn wrap_obu(h: &ObuHeader, payload: &[u8]) -> Vec<u8> {
    let mut w = BitWriter::new();
    write_obu_header(&mut w, h);
    if h.has_size_field {
        write_leb128(&mut w, payload.len() as u32);
    }
    let mut out = w.into_bytes();
    out.extend_from_slice(payload);
    out
}

/// A temporal-delimiter OBU (spec 6.2.2): type 2, no extension, size field
/// present, empty payload — the two bytes `12 00` that mark a new temporal
/// unit in the low-overhead format.
pub fn temporal_delimiter() -> Vec<u8> {
    wrap_obu(
        &ObuHeader {
            obu_type: ObuType::TemporalDelimiter,
            extension_flag: false,
            has_size_field: true,
            temporal_id: 0,
            spatial_id: 0,
        },
        &[],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_av1_syntax::obu::read_leb128;
    use ec_core::BitReader;

    #[test]
    fn obu_header_plain_roundtrip() {
        let h = ObuHeader {
            obu_type: ObuType::SequenceHeader,
            extension_flag: false,
            has_size_field: true,
            temporal_id: 0,
            spatial_id: 0,
        };
        let mut w = BitWriter::new();
        write_obu_header(&mut w, &h);
        let bytes = w.into_bytes();
        assert_eq!(bytes.len(), 1);
        let mut r = BitReader::new(&bytes);
        assert_eq!(ObuHeader::parse(&mut r).unwrap(), h);
    }

    #[test]
    fn obu_header_extension_roundtrip() {
        let h = ObuHeader {
            obu_type: ObuType::Frame,
            extension_flag: true,
            has_size_field: false,
            temporal_id: 3,
            spatial_id: 2,
        };
        let mut w = BitWriter::new();
        write_obu_header(&mut w, &h);
        let bytes = w.into_bytes();
        assert_eq!(bytes.len(), 2);
        let mut r = BitReader::new(&bytes);
        assert_eq!(ObuHeader::parse(&mut r).unwrap(), h);
    }

    #[test]
    fn wrap_obu_size_field_matches_payload() {
        let h = ObuHeader {
            obu_type: ObuType::SequenceHeader,
            extension_flag: false,
            has_size_field: true,
            temporal_id: 0,
            spatial_id: 0,
        };
        let payload = [0u8; 19]; // arbitrary
        let obu = wrap_obu(&h, &payload);
        // Header is one byte; the leb128 size follows; then the payload.
        let mut r = BitReader::new(&obu);
        let parsed_h = ObuHeader::parse(&mut r).unwrap();
        assert_eq!(parsed_h, h);
        let size = read_leb128(&mut r).unwrap();
        assert_eq!(size as usize, payload.len());
        assert_eq!(&obu[obu.len() - payload.len()..], &payload);
    }

    #[test]
    fn temporal_delimiter_is_two_bytes() {
        assert_eq!(temporal_delimiter(), [0x12, 0x00]);
    }
}
