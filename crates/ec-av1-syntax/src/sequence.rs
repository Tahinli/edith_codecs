//! The sequence header OBU (spec 5.5).

use ec_core::{BitReader, Error, Result};

use crate::obu::{read_leb128, read_uvlc};

/// `SELECT_SCREEN_CONTENT_TOOLS` and `SELECT_INTEGER_MV` (spec 3): "the frame
/// header decides", rather than a value forced by the sequence.
pub const SELECT_SCREEN_CONTENT_TOOLS: u8 = 2;
/// See [`SELECT_SCREEN_CONTENT_TOOLS`].
pub const SELECT_INTEGER_MV: u8 = 2;

/// `timing_info()` (spec 5.5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimingInfo {
    /// `num_units_in_display_tick`.
    pub num_units_in_display_tick: u32,
    /// `time_scale`.
    pub time_scale: u32,
    /// `equal_picture_interval`: every frame is displayed for the same time,
    /// which is what makes [`TimingInfo::num_ticks_per_picture`] meaningful.
    pub equal_picture_interval: bool,
    /// `num_ticks_per_picture_minus_1 + 1`, or 0 when not coded.
    pub num_ticks_per_picture: u32,
}

/// `decoder_model_info()` (spec 5.5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoderModelInfo {
    /// `buffer_delay_length_minus_1 + 1`.
    pub buffer_delay_length: u32,
    /// `num_units_in_decoding_tick`.
    pub num_units_in_decoding_tick: u32,
    /// `buffer_removal_time_length_minus_1 + 1`.
    pub buffer_removal_time_length: u32,
    /// `frame_presentation_time_length_minus_1 + 1`.
    pub frame_presentation_time_length: u32,
}

/// One entry of the operating point list (spec 5.5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OperatingPoint {
    /// `operating_point_idc`: which spatial and temporal layers this point has.
    pub idc: u32,
    /// `seq_level_idx`.
    pub seq_level_idx: u8,
    /// `seq_tier`.
    pub seq_tier: u8,
    /// `decoder_model_present_for_this_op`.
    pub decoder_model_present: bool,
    /// `decoder_buffer_delay`, when the decoder model is present.
    pub decoder_buffer_delay: u32,
    /// `encoder_buffer_delay`, when the decoder model is present.
    pub encoder_buffer_delay: u32,
    /// `low_delay_mode_flag`, when the decoder model is present.
    pub low_delay_mode_flag: bool,
    /// `initial_display_delay_minus_1 + 1`, or 0 when not coded.
    pub initial_display_delay: u8,
}

/// Chroma sample position (spec 6.4.2, `chroma_sample_position`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ChromaSamplePosition {
    /// `CSP_UNKNOWN`, 0.
    Unknown = 0,
    /// `CSP_VERTICAL`, 1 — co-sited horizontally, between rows vertically (MPEG-2).
    Vertical = 1,
    /// `CSP_COLOCATED`, 2 — co-sited both ways.
    Colocated = 2,
    /// `CSP_RESERVED`, 3.
    Reserved = 3,
}

/// `color_config()` (spec 5.5.2).
///
/// The three code points are H.273 (CICP) values, the same vocabulary
/// [`ec_core::color`] speaks, so they can be handed to it unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorConfig {
    /// `BitDepth`: 8, 10 or 12.
    pub bit_depth: u8,
    /// `mono_chrome`: no chroma planes are coded.
    pub mono_chrome: bool,
    /// `NumPlanes`: 1 when monochrome, else 3.
    pub num_planes: u8,
    /// `color_primaries`, H.273; 2 (unspecified) when not coded.
    pub color_primaries: u8,
    /// `transfer_characteristics`, H.273; 2 when not coded.
    pub transfer_characteristics: u8,
    /// `matrix_coefficients`, H.273; 2 when not coded.
    pub matrix_coefficients: u8,
    /// `color_range`: true for full range.
    pub color_range: bool,
    /// `subsampling_x`.
    pub subsampling_x: u8,
    /// `subsampling_y`.
    pub subsampling_y: u8,
    /// `chroma_sample_position`.
    pub chroma_sample_position: ChromaSamplePosition,
    /// `separate_uv_delta_q`: U and V may carry different quantizer deltas.
    pub separate_uv_delta_q: bool,
}

impl Default for ColorConfig {
    fn default() -> ColorConfig {
        ColorConfig {
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
        }
    }
}

/// A parsed sequence header OBU (spec 5.5.1).
///
/// Everything a frame header needs in order to parse at all lives here: the
/// frame size field widths, whether order hints exist and how wide they are,
/// which coding tools the sequence enables. A frame header without its sequence
/// header is unparseable, which is why [`crate::Av1Parser`] refuses one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceHeader {
    /// `seq_profile`, 0-2. 0 is 4:2:0 8/10-bit, 1 adds 4:4:4, 2 adds 4:2:2 and 12-bit.
    pub seq_profile: u8,
    /// `still_picture`: the sequence is a single frame.
    pub still_picture: bool,
    /// `reduced_still_picture_header`: the cut-down header a still image uses.
    pub reduced_still_picture_header: bool,
    /// `timing_info()`, when present.
    pub timing_info: Option<TimingInfo>,
    /// `decoder_model_info()`, when present.
    pub decoder_model_info: Option<DecoderModelInfo>,
    /// `initial_display_delay_present_flag`.
    pub initial_display_delay_present_flag: bool,
    /// The operating points, at least one.
    pub operating_points: Vec<OperatingPoint>,
    /// The operating point this parse selected — always 0, the highest quality
    /// one, since a stateless decoder here decodes everything it is given.
    pub operating_point: usize,
    /// `OperatingPointIdc` of the selected operating point.
    pub operating_point_idc: u32,
    /// `frame_width_bits_minus_1 + 1`.
    pub frame_width_bits: u32,
    /// `frame_height_bits_minus_1 + 1`.
    pub frame_height_bits: u32,
    /// `max_frame_width_minus_1 + 1`.
    pub max_frame_width: u32,
    /// `max_frame_height_minus_1 + 1`.
    pub max_frame_height: u32,
    /// `frame_id_numbers_present_flag`.
    pub frame_id_numbers_present_flag: bool,
    /// `delta_frame_id_length_minus_2 + 2`.
    pub delta_frame_id_length: u32,
    /// `additional_frame_id_length_minus_1 + 1`.
    pub additional_frame_id_length: u32,
    /// `use_128x128_superblock`.
    pub use_128x128_superblock: bool,
    /// `enable_filter_intra`.
    pub enable_filter_intra: bool,
    /// `enable_intra_edge_filter`.
    pub enable_intra_edge_filter: bool,
    /// `enable_interintra_compound`.
    pub enable_interintra_compound: bool,
    /// `enable_masked_compound`.
    pub enable_masked_compound: bool,
    /// `enable_warped_motion`.
    pub enable_warped_motion: bool,
    /// `enable_dual_filter`.
    pub enable_dual_filter: bool,
    /// `enable_order_hint`: without it there is no reference ordering, and half
    /// the inter tools below switch off.
    pub enable_order_hint: bool,
    /// `enable_jnt_comp`.
    pub enable_jnt_comp: bool,
    /// `enable_ref_frame_mvs`.
    pub enable_ref_frame_mvs: bool,
    /// `seq_force_screen_content_tools`, or [`SELECT_SCREEN_CONTENT_TOOLS`].
    pub seq_force_screen_content_tools: u8,
    /// `seq_force_integer_mv`, or [`SELECT_INTEGER_MV`].
    pub seq_force_integer_mv: u8,
    /// `OrderHintBits`: 0 when order hints are disabled.
    pub order_hint_bits: u32,
    /// `enable_superres`.
    pub enable_superres: bool,
    /// `enable_cdef`.
    pub enable_cdef: bool,
    /// `enable_restoration`.
    pub enable_restoration: bool,
    /// `color_config()`.
    pub color_config: ColorConfig,
    /// `film_grain_params_present`.
    pub film_grain_params_present: bool,
}

impl SequenceHeader {
    /// Parse a sequence header OBU payload (spec 5.5.1).
    pub fn parse(r: &mut BitReader<'_>) -> Result<SequenceHeader> {
        let seq_profile = r.read_bits(3)? as u8;
        if seq_profile > 2 {
            return Err(Error::corrupt(format!(
                "AV1 seq_profile {seq_profile} is reserved"
            )));
        }
        let still_picture = r.read_bit()?;
        let reduced_still_picture_header = r.read_bit()?;

        let mut timing_info = None;
        let mut decoder_model_info = None;
        let mut initial_display_delay_present_flag = false;
        let mut operating_points = Vec::new();

        if reduced_still_picture_header {
            operating_points.push(OperatingPoint {
                seq_level_idx: r.read_bits(5)? as u8,
                ..OperatingPoint::default()
            });
        } else {
            if r.read_bit()? {
                let info = TimingInfo {
                    num_units_in_display_tick: r.read_bits(32)?,
                    time_scale: r.read_bits(32)?,
                    equal_picture_interval: r.read_bit()?,
                    num_ticks_per_picture: 0,
                };
                let info = if info.equal_picture_interval {
                    TimingInfo {
                        num_ticks_per_picture: read_uvlc(r)?.saturating_add(1),
                        ..info
                    }
                } else {
                    info
                };
                timing_info = Some(info);
                if r.read_bit()? {
                    decoder_model_info = Some(DecoderModelInfo {
                        buffer_delay_length: r.read_bits(5)? + 1,
                        num_units_in_decoding_tick: r.read_bits(32)?,
                        buffer_removal_time_length: r.read_bits(5)? + 1,
                        frame_presentation_time_length: r.read_bits(5)? + 1,
                    });
                }
            }
            initial_display_delay_present_flag = r.read_bit()?;
            let count = r.read_bits(5)? as usize + 1;
            for _ in 0..count {
                let idc = r.read_bits(12)?;
                let seq_level_idx = r.read_bits(5)? as u8;
                let seq_tier = if seq_level_idx > 7 {
                    r.read_bits(1)? as u8
                } else {
                    0
                };
                let mut op = OperatingPoint {
                    idc,
                    seq_level_idx,
                    seq_tier,
                    ..OperatingPoint::default()
                };
                if let Some(model) = decoder_model_info {
                    op.decoder_model_present = r.read_bit()?;
                    if op.decoder_model_present {
                        let n = model.buffer_delay_length;
                        op.decoder_buffer_delay = r.read_bits(n)?;
                        op.encoder_buffer_delay = r.read_bits(n)?;
                        op.low_delay_mode_flag = r.read_bit()?;
                    }
                }
                if initial_display_delay_present_flag && r.read_bit()? {
                    op.initial_display_delay = r.read_bits(4)? as u8 + 1;
                }
                operating_points.push(op);
            }
        }

        // choose_operating_point() (spec 6.4.1): a decoder that outputs every
        // layer picks 0, the operating point that includes all of them.
        let operating_point = 0;
        let operating_point_idc = operating_points[operating_point].idc;

        let frame_width_bits = r.read_bits(4)? + 1;
        let frame_height_bits = r.read_bits(4)? + 1;
        let max_frame_width = r.read_bits(frame_width_bits)? + 1;
        let max_frame_height = r.read_bits(frame_height_bits)? + 1;

        let frame_id_numbers_present_flag = !reduced_still_picture_header && r.read_bit()?;
        let (delta_frame_id_length, additional_frame_id_length) = if frame_id_numbers_present_flag {
            (r.read_bits(4)? + 2, r.read_bits(3)? + 1)
        } else {
            (0, 0)
        };

        let use_128x128_superblock = r.read_bit()?;
        let enable_filter_intra = r.read_bit()?;
        let enable_intra_edge_filter = r.read_bit()?;

        let mut seq = SequenceHeader {
            seq_profile,
            still_picture,
            reduced_still_picture_header,
            timing_info,
            decoder_model_info,
            initial_display_delay_present_flag,
            operating_points,
            operating_point,
            operating_point_idc,
            frame_width_bits,
            frame_height_bits,
            max_frame_width,
            max_frame_height,
            frame_id_numbers_present_flag,
            delta_frame_id_length,
            additional_frame_id_length,
            use_128x128_superblock,
            enable_filter_intra,
            enable_intra_edge_filter,
            enable_interintra_compound: false,
            enable_masked_compound: false,
            enable_warped_motion: false,
            enable_dual_filter: false,
            enable_order_hint: false,
            enable_jnt_comp: false,
            enable_ref_frame_mvs: false,
            seq_force_screen_content_tools: SELECT_SCREEN_CONTENT_TOOLS,
            seq_force_integer_mv: SELECT_INTEGER_MV,
            order_hint_bits: 0,
            enable_superres: false,
            enable_cdef: false,
            enable_restoration: false,
            color_config: ColorConfig::default(),
            film_grain_params_present: false,
        };

        if !reduced_still_picture_header {
            seq.enable_interintra_compound = r.read_bit()?;
            seq.enable_masked_compound = r.read_bit()?;
            seq.enable_warped_motion = r.read_bit()?;
            seq.enable_dual_filter = r.read_bit()?;
            seq.enable_order_hint = r.read_bit()?;
            if seq.enable_order_hint {
                seq.enable_jnt_comp = r.read_bit()?;
                seq.enable_ref_frame_mvs = r.read_bit()?;
            }
            seq.seq_force_screen_content_tools = if r.read_bit()? {
                SELECT_SCREEN_CONTENT_TOOLS
            } else {
                r.read_bits(1)? as u8
            };
            seq.seq_force_integer_mv = if seq.seq_force_screen_content_tools > 0 {
                if r.read_bit()? {
                    SELECT_INTEGER_MV
                } else {
                    r.read_bits(1)? as u8
                }
            } else {
                SELECT_INTEGER_MV
            };
            if seq.enable_order_hint {
                seq.order_hint_bits = r.read_bits(3)? + 1;
            }
        }

        seq.enable_superres = r.read_bit()?;
        seq.enable_cdef = r.read_bit()?;
        seq.enable_restoration = r.read_bit()?;
        seq.color_config = read_color_config(r, seq_profile)?;
        seq.film_grain_params_present = r.read_bit()?;
        if std::env::var_os("EC_AV1_SEQDUMP").is_some() {
            eprintln!(
                "SEQDUMP profile={seq_profile} still={still_picture} reduced={reduced_still_picture_header} \
                 timing_present={} op_cnt={} idc0={} level0={} frame_width_bits={frame_width_bits} \
                 frame_height_bits={frame_height_bits} max_w={max_frame_width} max_h={max_frame_height} \
                 frame_id_present={frame_id_numbers_present_flag} use_128={} filter_intra={} edge_filter={} \
                 interintra={} masked={} warped={} dual_filter={} order_hint={} jnt_comp={} ref_frame_mvs={}",
                timing_info.is_some(),
                seq.operating_points.len(),
                seq.operating_points.first().map_or(0, |o| o.idc),
                seq.operating_points.first().map_or(0, |o| o.seq_level_idx),
                use_128x128_superblock, enable_filter_intra, enable_intra_edge_filter,
                seq.enable_interintra_compound, seq.enable_masked_compound,
                seq.enable_warped_motion, seq.enable_dual_filter, seq.enable_order_hint,
                seq.enable_jnt_comp, seq.enable_ref_frame_mvs
            );
        }
        Ok(seq)
    }

    /// `sbShift` (spec 5.9.15): 5 for 128x128 superblocks, 4 for 64x64. Pass it
    /// to [`crate::TileInfo::width_in_sbs_minus_1`] and its row counterpart.
    pub fn sb_shift(&self) -> u32 {
        if self.use_128x128_superblock { 5 } else { 4 }
    }

    /// `bit_depth_idx` as `VADecPictureParameterBufferAV1` indexes it: 0 for
    /// 8-bit, 1 for 10-bit, 2 for 12-bit.
    pub fn bit_depth_idx(&self) -> u8 {
        (self.color_config.bit_depth - 8) >> 1
    }

    /// `MaxTileWidthSb`-style geometry: how many superblocks a frame of
    /// `mi_cols` 4x4 units is wide.
    pub(crate) fn sb_cols(&self, mi_cols: u32) -> u32 {
        if self.use_128x128_superblock {
            mi_cols.div_ceil(32)
        } else {
            mi_cols.div_ceil(16)
        }
    }

    /// As [`SequenceHeader::sb_cols`], vertically.
    pub(crate) fn sb_rows(&self, mi_rows: u32) -> u32 {
        if self.use_128x128_superblock {
            mi_rows.div_ceil(32)
        } else {
            mi_rows.div_ceil(16)
        }
    }
}

/// `color_config()` (spec 5.5.2).
fn read_color_config(r: &mut BitReader<'_>, seq_profile: u8) -> Result<ColorConfig> {
    let high_bitdepth = r.read_bit()?;
    let bit_depth = if seq_profile == 2 && high_bitdepth {
        if r.read_bit()? { 12 } else { 10 }
    } else if high_bitdepth {
        10
    } else {
        8
    };
    let mono_chrome = if seq_profile == 1 {
        false
    } else {
        r.read_bit()?
    };
    let mut c = ColorConfig {
        bit_depth,
        mono_chrome,
        num_planes: if mono_chrome { 1 } else { 3 },
        ..ColorConfig::default()
    };
    if r.read_bit()? {
        c.color_primaries = r.read_bits(8)? as u8;
        c.transfer_characteristics = r.read_bits(8)? as u8;
        c.matrix_coefficients = r.read_bits(8)? as u8;
    }

    if mono_chrome {
        c.color_range = r.read_bit()?;
        c.subsampling_x = 1;
        c.subsampling_y = 1;
        c.separate_uv_delta_q = false;
        return Ok(c);
    }
    // sRGB: BT.709 primaries, sRGB transfer, identity matrix. Full range 4:4:4
    // by definition, and none of it is coded.
    if c.color_primaries == 1 && c.transfer_characteristics == 13 && c.matrix_coefficients == 0 {
        c.color_range = true;
        c.subsampling_x = 0;
        c.subsampling_y = 0;
    } else {
        c.color_range = r.read_bit()?;
        match seq_profile {
            0 => {
                c.subsampling_x = 1;
                c.subsampling_y = 1;
            }
            1 => {
                c.subsampling_x = 0;
                c.subsampling_y = 0;
            }
            _ => {
                if bit_depth == 12 {
                    c.subsampling_x = r.read_bits(1)? as u8;
                    c.subsampling_y = if c.subsampling_x == 1 {
                        r.read_bits(1)? as u8
                    } else {
                        0
                    };
                } else {
                    // Profile 2 below 12-bit is 4:2:2 and nothing else.
                    c.subsampling_x = 1;
                    c.subsampling_y = 0;
                }
            }
        }
        if c.subsampling_x == 1 && c.subsampling_y == 1 {
            c.chroma_sample_position = match r.read_bits(2)? {
                0 => ChromaSamplePosition::Unknown,
                1 => ChromaSamplePosition::Vertical,
                2 => ChromaSamplePosition::Colocated,
                _ => ChromaSamplePosition::Reserved,
            };
        }
    }
    c.separate_uv_delta_q = r.read_bit()?;
    Ok(c)
}

/// `metadata_obu()` (spec 5.8.1): the type, and nothing else parsed.
///
/// Metadata carries HDR mastering display and content light level among other
/// things, but those belong to [`ec_core::color`]'s vocabulary rather than to a
/// hardware decode submission, so this returns the type and leaves the payload
/// to a caller that wants it.
pub fn metadata_type(payload: &[u8]) -> Result<u32> {
    read_leb128(&mut BitReader::new(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Profile 2 at 12-bit is the only place subsampling is coded outright.
    #[test]
    fn profile_shapes_the_subsampling() {
        // profile 0: high_bitdepth 0, mono_chrome 0, no color description,
        // range 0, chroma position 00, separate_uv_delta_q 0.
        let bits = [0b0000_0000u8];
        let c = read_color_config(&mut BitReader::new(&bits), 0).unwrap();
        assert_eq!((c.bit_depth, c.subsampling_x, c.subsampling_y), (8, 1, 1));
        assert_eq!(c.num_planes, 3);

        // profile 1: high_bitdepth 1, no color description, range 0, sep 0;
        // mono_chrome is not coded and 4:4:4 is implied.
        let bits = [0b1000_0000u8];
        let c = read_color_config(&mut BitReader::new(&bits), 1).unwrap();
        assert_eq!((c.bit_depth, c.subsampling_x, c.subsampling_y), (10, 0, 0));

        // profile 2, 10-bit: 4:2:2, and no chroma_sample_position is coded.
        let bits = [0b1000_0000u8];
        let c = read_color_config(&mut BitReader::new(&bits), 2).unwrap();
        assert_eq!((c.bit_depth, c.subsampling_x, c.subsampling_y), (10, 1, 0));
    }

    #[test]
    fn monochrome_stops_before_the_chroma_fields() {
        // high_bitdepth 0, mono_chrome 1, no color description, range 1, sep 0.
        let bits = [0b0101_0000u8];
        let c = read_color_config(&mut BitReader::new(&bits), 0).unwrap();
        assert!(c.mono_chrome);
        assert_eq!(c.num_planes, 1);
        assert!(c.color_range);
        assert!(!c.separate_uv_delta_q);
    }

    #[test]
    fn reserved_profile_is_corrupt() {
        assert!(SequenceHeader::parse(&mut BitReader::new(&[0xff; 16])).is_err());
    }
}
