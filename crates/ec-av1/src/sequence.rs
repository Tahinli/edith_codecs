//! Sequence-header OBU writer (spec 5.5) — bit-exact inverse of
//! [`ec_av1_syntax::sequence::SequenceHeader::parse`], for the
//! stateless-decoder subset.

use ec_av1_syntax::obu::{ObuHeader, ObuType};
#[cfg(test)]
use ec_av1_syntax::sequence::OperatingPoint;
use ec_av1_syntax::sequence::{
    ChromaSamplePosition, ColorConfig, SELECT_INTEGER_MV, SELECT_SCREEN_CONTENT_TOOLS,
    SequenceHeader,
};
use ec_core::{BitWriter, Error, Result};

use crate::bits::write_trailing_bits;
use crate::obu::wrap_obu;

/// Map a [`ChromaSamplePosition`] to its 2-bit code (spec 6.4.2).
fn chroma_sample_position_code(p: ChromaSamplePosition) -> u32 {
    match p {
        ChromaSamplePosition::Unknown => 0,
        ChromaSamplePosition::Vertical => 1,
        ChromaSamplePosition::Colocated => 2,
        ChromaSamplePosition::Reserved => 3,
    }
}

/// Write `color_config()` (spec 5.5.2) — the inverse of
/// `ec_av1_syntax::sequence::read_color_config`.
fn write_color_config(w: &mut BitWriter, c: &ColorConfig, seq_profile: u8) -> Result<()> {
    let high_bitdepth = c.bit_depth >= 10;
    w.write_bit(high_bitdepth);
    if seq_profile == 2 && high_bitdepth {
        // The extra `twelve_bit` flag: 1 for 12-bit, 0 for 10-bit.
        w.write_bit(c.bit_depth == 12);
    }

    if seq_profile == 1 {
        // Profile 1 forces mono_chrome = false; the bit is not coded.
        if c.mono_chrome {
            return Err(Error::unsupported(
                "AV1 color_config mono_chrome",
                "profile 1 is 4:4:4 and cannot be monochrome",
            ));
        }
    } else {
        w.write_bit(c.mono_chrome);
    }

    // color_description_present_flag: absent when all three are the
    // unspecified default (H.273 code 2), which the parser then assumes.
    let present =
        c.color_primaries != 2 || c.transfer_characteristics != 2 || c.matrix_coefficients != 2;
    w.write_bit(present);
    if present {
        w.write_bits(c.color_primaries as u32, 8);
        w.write_bits(c.transfer_characteristics as u32, 8);
        w.write_bits(c.matrix_coefficients as u32, 8);
    }

    if c.mono_chrome {
        w.write_bit(c.color_range);
        // subsampling is forced 1,1 and separate_uv_delta_q is not coded.
        return Ok(());
    }

    // sRGB identity (BT.709 primaries, sRGB transfer, identity matrix) forces
    // full-range 4:4:4; none of color_range, subsampling or position is coded.
    let srgb =
        c.color_primaries == 1 && c.transfer_characteristics == 13 && c.matrix_coefficients == 0;
    if !srgb {
        w.write_bit(c.color_range);
        match seq_profile {
            0 => {} // subsampling forced 1,1 (4:2:0), not coded
            1 => {} // subsampling forced 0,0 (4:4:4), not coded
            _ => {
                if c.bit_depth == 12 {
                    w.write_bits(c.subsampling_x as u32, 1);
                    if c.subsampling_x == 1 {
                        w.write_bits(c.subsampling_y as u32, 1);
                    }
                }
                // Profile 2 below 12-bit: subsampling forced 1,0 (4:2:2).
            }
        }
        if c.subsampling_x == 1 && c.subsampling_y == 1 {
            w.write_bits(chroma_sample_position_code(c.chroma_sample_position), 2);
        }
    }
    w.write_bit(c.separate_uv_delta_q);
    Ok(())
}

/// Write a sequence-header OBU payload (spec 5.5.1) — the inverse of
/// [`SequenceHeader::parse`], ending with `trailing_bits()`.
///
/// Supported subset (anything else returns [`Error::Unsupported`]):
/// `reduced_still_picture_header == false`, no `timing_info`, no
/// `decoder_model_info`, and exactly one operating point — the normal
/// single-layer video case. Still-image, timed and buffered-decoder-model
/// streams are refused rather than mis-written.
pub fn write_sequence_header(w: &mut BitWriter, s: &SequenceHeader) -> Result<()> {
    if s.reduced_still_picture_header {
        return Err(Error::unsupported(
            "AV1 reduced_still_picture_header",
            "the cut-down still-image header is not implemented",
        ));
    }
    if s.timing_info.is_some() {
        return Err(Error::unsupported(
            "AV1 timing_info",
            "timing info is not implemented",
        ));
    }
    if s.decoder_model_info.is_some() {
        return Err(Error::unsupported(
            "AV1 decoder_model_info",
            "the decoder model is not implemented",
        ));
    }
    if s.operating_points.len() != 1 {
        return Err(Error::unsupported(
            "AV1 operating_points",
            "only one operating point is supported",
        ));
    }

    w.write_bits(s.seq_profile as u32, 3);
    w.write_bit(s.still_picture);
    w.write_bit(s.reduced_still_picture_header);
    // timing_info_present_flag = false (the subset above guarantees it).
    w.write_bit(false);
    w.write_bit(s.initial_display_delay_present_flag);
    // operating_points_cnt_minus_1 = 0.
    w.write_bits(0, 5);

    let op = &s.operating_points[0];
    w.write_bits(op.idc, 12);
    w.write_bits(op.seq_level_idx as u32, 5);
    if op.seq_level_idx > 7 {
        w.write_bit(op.seq_tier != 0);
    }
    // No decoder model: decoder_model_present_for_this_op is not coded.
    if s.initial_display_delay_present_flag {
        if op.initial_display_delay > 0 {
            w.write_bit(true);
            w.write_bits(op.initial_display_delay as u32 - 1, 4);
        } else {
            w.write_bit(false);
        }
    }

    w.write_bits(s.frame_width_bits - 1, 4);
    w.write_bits(s.frame_height_bits - 1, 4);
    w.write_bits(s.max_frame_width - 1, s.frame_width_bits);
    w.write_bits(s.max_frame_height - 1, s.frame_height_bits);

    // reduced_still_picture_header is false, so the flag is always coded.
    w.write_bit(s.frame_id_numbers_present_flag);
    if s.frame_id_numbers_present_flag {
        w.write_bits(s.delta_frame_id_length - 2, 4);
        w.write_bits(s.additional_frame_id_length - 1, 3);
    }

    w.write_bit(s.use_128x128_superblock);
    w.write_bit(s.enable_filter_intra);
    w.write_bit(s.enable_intra_edge_filter);

    // The !reduced_still_picture_header block (spec 5.5.1).
    w.write_bit(s.enable_interintra_compound);
    w.write_bit(s.enable_masked_compound);
    w.write_bit(s.enable_warped_motion);
    w.write_bit(s.enable_dual_filter);
    w.write_bit(s.enable_order_hint);
    if s.enable_order_hint {
        w.write_bit(s.enable_jnt_comp);
        w.write_bit(s.enable_ref_frame_mvs);
    }
    // seq_force_screen_content_tools: SELECT (2) is the single-bit form.
    if s.seq_force_screen_content_tools == SELECT_SCREEN_CONTENT_TOOLS {
        w.write_bit(true);
    } else if s.seq_force_screen_content_tools <= 1 {
        w.write_bit(false);
        w.write_bits(s.seq_force_screen_content_tools as u32, 1);
    } else {
        return Err(Error::unsupported(
            "AV1 seq_force_screen_content_tools",
            "value must be 0, 1 or SELECT (2)",
        ));
    }
    // seq_force_integer_mv: only coded when screen content tools are on.
    if s.seq_force_screen_content_tools > 0 {
        if s.seq_force_integer_mv == SELECT_INTEGER_MV {
            w.write_bit(true);
        } else if s.seq_force_integer_mv <= 1 {
            w.write_bit(false);
            w.write_bits(s.seq_force_integer_mv as u32, 1);
        } else {
            return Err(Error::unsupported(
                "AV1 seq_force_integer_mv",
                "value must be 0, 1 or SELECT (2)",
            ));
        }
    }
    if s.enable_order_hint {
        w.write_bits(s.order_hint_bits - 1, 3);
    }

    w.write_bit(s.enable_superres);
    w.write_bit(s.enable_cdef);
    w.write_bit(s.enable_restoration);
    write_color_config(w, &s.color_config, s.seq_profile)?;
    w.write_bit(s.film_grain_params_present);
    write_trailing_bits(w);
    Ok(())
}

/// Build a complete sequence-header OBU (spec 5.5.1): the payload from
/// [`write_sequence_header`] wrapped by [`wrap_obu`] with
/// [`ObuType::SequenceHeader`], `has_size_field` set and no extension header.
pub fn sequence_header_obu(s: &SequenceHeader) -> Result<Vec<u8>> {
    let mut w = BitWriter::new();
    write_sequence_header(&mut w, s)?;
    let payload = w.into_bytes();
    Ok(wrap_obu(
        &ObuHeader {
            obu_type: ObuType::SequenceHeader,
            extension_flag: false,
            has_size_field: true,
            temporal_id: 0,
            spatial_id: 0,
        },
        &payload,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_av1_syntax::obu::{ObuHeader, read_leb128};
    use ec_av1_syntax::sequence::SequenceHeader;
    use ec_core::{BitReader, BitWriter};

    /// A realistic 1920x1080 8-bit 4:2:0 main-profile sequence header.
    fn sample_1080p() -> SequenceHeader {
        SequenceHeader {
            seq_profile: 0,
            still_picture: false,
            reduced_still_picture_header: false,
            timing_info: None,
            decoder_model_info: None,
            initial_display_delay_present_flag: false,
            operating_points: vec![OperatingPoint {
                idc: 0,
                seq_level_idx: 8, // level 4.0; > 7 so seq_tier is coded
                seq_tier: 0,
                decoder_model_present: false,
                decoder_buffer_delay: 0,
                encoder_buffer_delay: 0,
                low_delay_mode_flag: false,
                initial_display_delay: 0,
            }],
            operating_point: 0,
            operating_point_idc: 0,
            frame_width_bits: 11,
            frame_height_bits: 11,
            max_frame_width: 1920,
            max_frame_height: 1080,
            frame_id_numbers_present_flag: false,
            delta_frame_id_length: 0,
            additional_frame_id_length: 0,
            use_128x128_superblock: true,
            enable_filter_intra: true,
            enable_intra_edge_filter: true,
            enable_interintra_compound: false,
            enable_masked_compound: false,
            enable_warped_motion: false,
            enable_dual_filter: true,
            enable_order_hint: true,
            enable_jnt_comp: false,
            enable_ref_frame_mvs: false,
            seq_force_screen_content_tools: SELECT_SCREEN_CONTENT_TOOLS,
            seq_force_integer_mv: SELECT_INTEGER_MV,
            order_hint_bits: 7,
            enable_superres: false,
            enable_cdef: true,
            enable_restoration: false,
            color_config: ColorConfig {
                bit_depth: 8,
                mono_chrome: false,
                num_planes: 3,
                color_primaries: 2,
                transfer_characteristics: 2,
                matrix_coefficients: 2,
                color_range: false,
                subsampling_x: 1,
                subsampling_y: 1,
                chroma_sample_position: ChromaSamplePosition::Unknown,
                separate_uv_delta_q: false,
            },
            film_grain_params_present: false,
        }
    }

    #[test]
    fn sequence_header_roundtrip() {
        let original = sample_1080p();
        let mut w = BitWriter::new();
        write_sequence_header(&mut w, &original).unwrap();
        let payload = w.into_bytes();

        let mut r = BitReader::new(&payload);
        let parsed = SequenceHeader::parse(&mut r).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn sequence_header_obu_size_matches_payload() {
        let s = sample_1080p();

        // The payload the OBU must carry.
        let mut w = BitWriter::new();
        write_sequence_header(&mut w, &s).unwrap();
        let payload = w.into_bytes();

        let obu = sequence_header_obu(&s).unwrap();

        // Header (no extension) → leb128 size → payload.
        let mut r = BitReader::new(&obu);
        let h = ObuHeader::parse(&mut r).unwrap();
        assert_eq!(h.obu_type, ObuType::SequenceHeader);
        assert!(h.has_size_field);
        let size = read_leb128(&mut r).unwrap();
        assert_eq!(size as usize, payload.len());

        // The trailing slice re-parses to the same header.
        let body = &obu[obu.len() - payload.len()..];
        let mut r2 = BitReader::new(body);
        let parsed = SequenceHeader::parse(&mut r2).unwrap();
        assert_eq!(parsed, s);
    }
}
