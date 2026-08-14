//! Sequence parameter set (Rec. ITU-T H.264 clause 7.3.2.1) and everything it
//! nests: scaling lists (7.3.2.1.1.1), VUI parameters (Annex E.1.1) and HRD
//! parameters (Annex E.1.2).
//!
//! Field names are the syntax element names of the specification, including the
//! `_minus1`/`_minus4` suffixes: a struct that renames them silently loses the
//! ability to be checked against the tables it came from. Derived values
//! (clause 7.4.2.1.1) are methods, never stored fields.

use ec_core::bitio::BitReader;
use ec_core::error::{Error, Result};

use crate::nal::RbspReader;

/// `scaling_list()` output (clause 7.3.2.1.1.1): the list plus the
/// `useDefaultScalingMatrixFlag` its parse derived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalingList {
    /// Coefficients in the list's own scan order, 16 or 64 of them.
    pub values: Vec<u8>,
    /// `UseDefaultScalingMatrixFlag`: the encoder asked for the default matrix.
    pub use_default: bool,
}

/// `scaling_list(scalingList, sizeOfScalingList, useDefaultScalingMatrixFlag)`.
///
/// Coefficients are coded as `se(v)` deltas modulo 256 around a running
/// `lastScale`; a `nextScale` of zero freezes the rest of the list at
/// `lastScale`, and a zero on the very first coefficient means "use the default
/// matrix instead of this list".
pub fn scaling_list(r: &mut BitReader<'_>, size: usize) -> Result<ScalingList> {
    let mut values = vec![0u8; size];
    let mut last_scale: i32 = 8;
    let mut next_scale: i32 = 8;
    let mut use_default = false;
    for (j, slot) in values.iter_mut().enumerate() {
        if next_scale != 0 {
            let delta_scale = r.read_se()?;
            next_scale = (last_scale + delta_scale + 256).rem_euclid(256);
            use_default = j == 0 && next_scale == 0;
        }
        let value = if next_scale == 0 {
            last_scale
        } else {
            next_scale
        };
        *slot = value as u8;
        last_scale = value;
    }
    Ok(ScalingList {
        values,
        use_default,
    })
}

/// `hrd_parameters()` (Annex E.1.2).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HrdParameters {
    /// `cpb_cnt_minus1`: number of alternative CPB specifications, minus 1.
    pub cpb_cnt_minus1: u32,
    /// `bit_rate_scale`.
    pub bit_rate_scale: u8,
    /// `cpb_size_scale`.
    pub cpb_size_scale: u8,
    /// `bit_rate_value_minus1[SchedSelIdx]`.
    pub bit_rate_value_minus1: Vec<u32>,
    /// `cpb_size_value_minus1[SchedSelIdx]`.
    pub cpb_size_value_minus1: Vec<u32>,
    /// `cbr_flag[SchedSelIdx]`.
    pub cbr_flag: Vec<bool>,
    /// `initial_cpb_removal_delay_length_minus1`.
    pub initial_cpb_removal_delay_length_minus1: u8,
    /// `cpb_removal_delay_length_minus1`.
    pub cpb_removal_delay_length_minus1: u8,
    /// `dpb_output_delay_length_minus1`.
    pub dpb_output_delay_length_minus1: u8,
    /// `time_offset_length`.
    pub time_offset_length: u8,
}

impl HrdParameters {
    /// Parse `hrd_parameters()`.
    pub fn parse(r: &mut BitReader<'_>) -> Result<HrdParameters> {
        let cpb_cnt_minus1 = r.read_ue()?;
        if cpb_cnt_minus1 > 31 {
            return Err(Error::corrupt(format!(
                "H.264 HRD: cpb_cnt_minus1 = {cpb_cnt_minus1} > 31"
            )));
        }
        let bit_rate_scale = r.read_bits(4)? as u8;
        let cpb_size_scale = r.read_bits(4)? as u8;
        let n = cpb_cnt_minus1 as usize + 1;
        let mut bit_rate_value_minus1 = Vec::with_capacity(n);
        let mut cpb_size_value_minus1 = Vec::with_capacity(n);
        let mut cbr_flag = Vec::with_capacity(n);
        for _ in 0..n {
            bit_rate_value_minus1.push(r.read_ue()?);
            cpb_size_value_minus1.push(r.read_ue()?);
            cbr_flag.push(r.read_bit()?);
        }
        Ok(HrdParameters {
            cpb_cnt_minus1,
            bit_rate_scale,
            cpb_size_scale,
            bit_rate_value_minus1,
            cpb_size_value_minus1,
            cbr_flag,
            initial_cpb_removal_delay_length_minus1: r.read_bits(5)? as u8,
            cpb_removal_delay_length_minus1: r.read_bits(5)? as u8,
            dpb_output_delay_length_minus1: r.read_bits(5)? as u8,
            time_offset_length: r.read_bits(5)? as u8,
        })
    }
}

/// `vui_parameters()` (Annex E.1.1).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VuiParameters {
    /// `aspect_ratio_info_present_flag`.
    pub aspect_ratio_info_present_flag: bool,
    /// `aspect_ratio_idc` (Table E-1); 255 is Extended_SAR.
    pub aspect_ratio_idc: u8,
    /// `sar_width`, present only for Extended_SAR.
    pub sar_width: u16,
    /// `sar_height`, present only for Extended_SAR.
    pub sar_height: u16,
    /// `overscan_info_present_flag`.
    pub overscan_info_present_flag: bool,
    /// `overscan_appropriate_flag`.
    pub overscan_appropriate_flag: bool,
    /// `video_signal_type_present_flag`.
    pub video_signal_type_present_flag: bool,
    /// `video_format` (Table E-2); 5 = Unspecified.
    pub video_format: u8,
    /// `video_full_range_flag`.
    pub video_full_range_flag: bool,
    /// `colour_description_present_flag`.
    pub colour_description_present_flag: bool,
    /// `colour_primaries`, an ITU-T H.273 code point.
    pub colour_primaries: u8,
    /// `transfer_characteristics`, an ITU-T H.273 code point.
    pub transfer_characteristics: u8,
    /// `matrix_coefficients`, an ITU-T H.273 code point.
    pub matrix_coefficients: u8,
    /// `chroma_loc_info_present_flag`.
    pub chroma_loc_info_present_flag: bool,
    /// `chroma_sample_loc_type_top_field`.
    pub chroma_sample_loc_type_top_field: u32,
    /// `chroma_sample_loc_type_bottom_field`.
    pub chroma_sample_loc_type_bottom_field: u32,
    /// `timing_info_present_flag`.
    pub timing_info_present_flag: bool,
    /// `num_units_in_tick`.
    pub num_units_in_tick: u32,
    /// `time_scale`.
    pub time_scale: u32,
    /// `fixed_frame_rate_flag`.
    pub fixed_frame_rate_flag: bool,
    /// `nal_hrd_parameters_present_flag` and its payload.
    pub nal_hrd_parameters: Option<HrdParameters>,
    /// `vcl_hrd_parameters_present_flag` and its payload.
    pub vcl_hrd_parameters: Option<HrdParameters>,
    /// `low_delay_hrd_flag`, present when either HRD block is.
    pub low_delay_hrd_flag: bool,
    /// `pic_struct_present_flag`.
    pub pic_struct_present_flag: bool,
    /// `bitstream_restriction_flag`.
    pub bitstream_restriction_flag: bool,
    /// `motion_vectors_over_pic_boundaries_flag`.
    pub motion_vectors_over_pic_boundaries_flag: bool,
    /// `max_bytes_per_pic_denom`.
    pub max_bytes_per_pic_denom: u32,
    /// `max_bits_per_mb_denom`.
    pub max_bits_per_mb_denom: u32,
    /// `log2_max_mv_length_horizontal`.
    pub log2_max_mv_length_horizontal: u32,
    /// `log2_max_mv_length_vertical`.
    pub log2_max_mv_length_vertical: u32,
    /// `max_num_reorder_frames`.
    pub max_num_reorder_frames: u32,
    /// `max_dec_frame_buffering`.
    pub max_dec_frame_buffering: u32,
}

impl VuiParameters {
    /// Parse `vui_parameters()`.
    pub fn parse(r: &mut BitReader<'_>) -> Result<VuiParameters> {
        let mut v = VuiParameters {
            // Defaults are the "unspecified" code points of Annex E, so a VUI
            // that omits a block still reads as the specification defines it.
            aspect_ratio_idc: 0,
            video_format: 5,
            colour_primaries: 2,
            transfer_characteristics: 2,
            matrix_coefficients: 2,
            ..VuiParameters::default()
        };
        v.aspect_ratio_info_present_flag = r.read_bit()?;
        if v.aspect_ratio_info_present_flag {
            v.aspect_ratio_idc = r.read_bits(8)? as u8;
            if v.aspect_ratio_idc == 255 {
                v.sar_width = r.read_bits(16)? as u16;
                v.sar_height = r.read_bits(16)? as u16;
            }
        }
        v.overscan_info_present_flag = r.read_bit()?;
        if v.overscan_info_present_flag {
            v.overscan_appropriate_flag = r.read_bit()?;
        }
        v.video_signal_type_present_flag = r.read_bit()?;
        if v.video_signal_type_present_flag {
            v.video_format = r.read_bits(3)? as u8;
            v.video_full_range_flag = r.read_bit()?;
            v.colour_description_present_flag = r.read_bit()?;
            if v.colour_description_present_flag {
                v.colour_primaries = r.read_bits(8)? as u8;
                v.transfer_characteristics = r.read_bits(8)? as u8;
                v.matrix_coefficients = r.read_bits(8)? as u8;
            }
        }
        v.chroma_loc_info_present_flag = r.read_bit()?;
        if v.chroma_loc_info_present_flag {
            v.chroma_sample_loc_type_top_field = r.read_ue()?;
            v.chroma_sample_loc_type_bottom_field = r.read_ue()?;
        }
        v.timing_info_present_flag = r.read_bit()?;
        if v.timing_info_present_flag {
            v.num_units_in_tick = r.read_bits(32)?;
            v.time_scale = r.read_bits(32)?;
            v.fixed_frame_rate_flag = r.read_bit()?;
        }
        if r.read_bit()? {
            v.nal_hrd_parameters = Some(HrdParameters::parse(r)?);
        }
        if r.read_bit()? {
            v.vcl_hrd_parameters = Some(HrdParameters::parse(r)?);
        }
        if v.nal_hrd_parameters.is_some() || v.vcl_hrd_parameters.is_some() {
            v.low_delay_hrd_flag = r.read_bit()?;
        }
        v.pic_struct_present_flag = r.read_bit()?;
        v.bitstream_restriction_flag = r.read_bit()?;
        if v.bitstream_restriction_flag {
            v.motion_vectors_over_pic_boundaries_flag = r.read_bit()?;
            v.max_bytes_per_pic_denom = r.read_ue()?;
            v.max_bits_per_mb_denom = r.read_ue()?;
            v.log2_max_mv_length_horizontal = r.read_ue()?;
            v.log2_max_mv_length_vertical = r.read_ue()?;
            v.max_num_reorder_frames = r.read_ue()?;
            v.max_dec_frame_buffering = r.read_ue()?;
        }
        Ok(v)
    }
}

/// `seq_parameter_set_data()` (clause 7.3.2.1.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceParameterSet {
    /// `profile_idc` (Annex A).
    pub profile_idc: u8,
    /// `constraint_set0_flag` .. `constraint_set5_flag`, bit 0 = set0.
    pub constraint_set_flags: u8,
    /// `level_idc` (Table A-1), in units of a tenth of a level.
    pub level_idc: u8,
    /// `seq_parameter_set_id`, 0..=31.
    pub seq_parameter_set_id: u32,
    /// `chroma_format_idc`; 1 (4:2:0) unless the profile codes it explicitly.
    pub chroma_format_idc: u32,
    /// `separate_colour_plane_flag`, only meaningful for 4:4:4.
    pub separate_colour_plane_flag: bool,
    /// `bit_depth_luma_minus8`.
    pub bit_depth_luma_minus8: u32,
    /// `bit_depth_chroma_minus8`.
    pub bit_depth_chroma_minus8: u32,
    /// `qpprime_y_zero_transform_bypass_flag`: lossless coding at QP'Y = 0.
    pub qpprime_y_zero_transform_bypass_flag: bool,
    /// `seq_scaling_matrix_present_flag`.
    pub seq_scaling_matrix_present_flag: bool,
    /// `ScalingList4x4[0..6]`, `None` where `seq_scaling_list_present_flag` is 0.
    pub scaling_list_4x4: [Option<ScalingList>; 6],
    /// `ScalingList8x8[0..6]`, `None` where not present.
    pub scaling_list_8x8: [Option<ScalingList>; 6],
    /// `log2_max_frame_num_minus4`.
    pub log2_max_frame_num_minus4: u32,
    /// `pic_order_cnt_type`, 0..=2.
    pub pic_order_cnt_type: u32,
    /// `log2_max_pic_order_cnt_lsb_minus4` (POC type 0).
    pub log2_max_pic_order_cnt_lsb_minus4: u32,
    /// `delta_pic_order_always_zero_flag` (POC type 1).
    pub delta_pic_order_always_zero_flag: bool,
    /// `offset_for_non_ref_pic` (POC type 1).
    pub offset_for_non_ref_pic: i32,
    /// `offset_for_top_to_bottom_field` (POC type 1).
    pub offset_for_top_to_bottom_field: i32,
    /// `offset_for_ref_frame[i]` (POC type 1).
    pub offset_for_ref_frame: Vec<i32>,
    /// `max_num_ref_frames`.
    pub max_num_ref_frames: u32,
    /// `gaps_in_frame_num_value_allowed_flag`.
    pub gaps_in_frame_num_value_allowed_flag: bool,
    /// `pic_width_in_mbs_minus1`.
    pub pic_width_in_mbs_minus1: u32,
    /// `pic_height_in_map_units_minus1`.
    pub pic_height_in_map_units_minus1: u32,
    /// `frame_mbs_only_flag`: 0 means the stream may contain fields or MBAFF.
    pub frame_mbs_only_flag: bool,
    /// `mb_adaptive_frame_field_flag`.
    pub mb_adaptive_frame_field_flag: bool,
    /// `direct_8x8_inference_flag`.
    pub direct_8x8_inference_flag: bool,
    /// `frame_cropping_flag`.
    pub frame_cropping_flag: bool,
    /// `frame_crop_left_offset`, in units of `CropUnitX`.
    pub frame_crop_left_offset: u32,
    /// `frame_crop_right_offset`, in units of `CropUnitX`.
    pub frame_crop_right_offset: u32,
    /// `frame_crop_top_offset`, in units of `CropUnitY`.
    pub frame_crop_top_offset: u32,
    /// `frame_crop_bottom_offset`, in units of `CropUnitY`.
    pub frame_crop_bottom_offset: u32,
    /// `vui_parameters()` when `vui_parameters_present_flag` is 1.
    pub vui_parameters: Option<VuiParameters>,
}

/// Profiles that code `chroma_format_idc` and the bit depths explicitly
/// (clause 7.3.2.1.1); every other profile is 4:2:0 8-bit by definition.
const PROFILES_WITH_CHROMA_SYNTAX: &[u8] =
    &[100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];

impl SequenceParameterSet {
    /// Parse `seq_parameter_set_rbsp()` from an RBSP (clause 7.3.2.1).
    pub fn parse(rbsp: &[u8]) -> Result<SequenceParameterSet> {
        let mut rr = RbspReader::new(rbsp);
        let r = rr.bits();
        let profile_idc = r.read_bits(8)? as u8;
        let constraint_set_flags = r.read_bits(6)? as u8;
        let _reserved_zero_2bits = r.read_bits(2)?;
        let level_idc = r.read_bits(8)? as u8;
        let seq_parameter_set_id = r.read_ue()?;
        if seq_parameter_set_id > 31 {
            return Err(Error::corrupt(format!(
                "H.264 SPS: seq_parameter_set_id = {seq_parameter_set_id} > 31"
            )));
        }

        let mut sps = SequenceParameterSet {
            profile_idc,
            constraint_set_flags,
            level_idc,
            seq_parameter_set_id,
            chroma_format_idc: 1,
            separate_colour_plane_flag: false,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            qpprime_y_zero_transform_bypass_flag: false,
            seq_scaling_matrix_present_flag: false,
            scaling_list_4x4: [const { None }; 6],
            scaling_list_8x8: [const { None }; 6],
            log2_max_frame_num_minus4: 0,
            pic_order_cnt_type: 0,
            log2_max_pic_order_cnt_lsb_minus4: 0,
            delta_pic_order_always_zero_flag: false,
            offset_for_non_ref_pic: 0,
            offset_for_top_to_bottom_field: 0,
            offset_for_ref_frame: Vec::new(),
            max_num_ref_frames: 0,
            gaps_in_frame_num_value_allowed_flag: false,
            pic_width_in_mbs_minus1: 0,
            pic_height_in_map_units_minus1: 0,
            frame_mbs_only_flag: true,
            mb_adaptive_frame_field_flag: false,
            direct_8x8_inference_flag: false,
            frame_cropping_flag: false,
            frame_crop_left_offset: 0,
            frame_crop_right_offset: 0,
            frame_crop_top_offset: 0,
            frame_crop_bottom_offset: 0,
            vui_parameters: None,
        };

        if PROFILES_WITH_CHROMA_SYNTAX.contains(&profile_idc) {
            sps.chroma_format_idc = r.read_ue()?;
            if sps.chroma_format_idc > 3 {
                return Err(Error::corrupt(format!(
                    "H.264 SPS: chroma_format_idc = {}",
                    sps.chroma_format_idc
                )));
            }
            if sps.chroma_format_idc == 3 {
                sps.separate_colour_plane_flag = r.read_bit()?;
            }
            sps.bit_depth_luma_minus8 = r.read_ue()?;
            sps.bit_depth_chroma_minus8 = r.read_ue()?;
            if sps.bit_depth_luma_minus8 > 6 || sps.bit_depth_chroma_minus8 > 6 {
                return Err(Error::corrupt("H.264 SPS: bit_depth_*_minus8 > 6"));
            }
            sps.qpprime_y_zero_transform_bypass_flag = r.read_bit()?;
            sps.seq_scaling_matrix_present_flag = r.read_bit()?;
            if sps.seq_scaling_matrix_present_flag {
                let count = if sps.chroma_format_idc != 3 { 8 } else { 12 };
                for i in 0..count {
                    if r.read_bit()? {
                        if i < 6 {
                            sps.scaling_list_4x4[i] = Some(scaling_list(r, 16)?);
                        } else {
                            sps.scaling_list_8x8[i - 6] = Some(scaling_list(r, 64)?);
                        }
                    }
                }
            }
        }

        sps.log2_max_frame_num_minus4 = r.read_ue()?;
        if sps.log2_max_frame_num_minus4 > 12 {
            return Err(Error::corrupt(format!(
                "H.264 SPS: log2_max_frame_num_minus4 = {}",
                sps.log2_max_frame_num_minus4
            )));
        }
        sps.pic_order_cnt_type = r.read_ue()?;
        match sps.pic_order_cnt_type {
            0 => {
                sps.log2_max_pic_order_cnt_lsb_minus4 = r.read_ue()?;
                if sps.log2_max_pic_order_cnt_lsb_minus4 > 12 {
                    return Err(Error::corrupt(
                        "H.264 SPS: log2_max_pic_order_cnt_lsb_minus4 > 12",
                    ));
                }
            }
            1 => {
                sps.delta_pic_order_always_zero_flag = r.read_bit()?;
                sps.offset_for_non_ref_pic = r.read_se()?;
                sps.offset_for_top_to_bottom_field = r.read_se()?;
                let num_ref_frames_in_pic_order_cnt_cycle = r.read_ue()?;
                if num_ref_frames_in_pic_order_cnt_cycle > 255 {
                    return Err(Error::corrupt(
                        "H.264 SPS: num_ref_frames_in_pic_order_cnt_cycle > 255",
                    ));
                }
                for _ in 0..num_ref_frames_in_pic_order_cnt_cycle {
                    sps.offset_for_ref_frame.push(r.read_se()?);
                }
            }
            2 => {}
            other => {
                return Err(Error::corrupt(format!(
                    "H.264 SPS: pic_order_cnt_type = {other}"
                )));
            }
        }
        sps.max_num_ref_frames = r.read_ue()?;
        sps.gaps_in_frame_num_value_allowed_flag = r.read_bit()?;
        sps.pic_width_in_mbs_minus1 = r.read_ue()?;
        sps.pic_height_in_map_units_minus1 = r.read_ue()?;
        // A picture larger than level 6.2 allows is a corrupt header, and the
        // allocation it would imply is the fuzzer's favourite denial of service.
        if sps.pic_width_in_mbs_minus1 >= 1024 || sps.pic_height_in_map_units_minus1 >= 1024 {
            return Err(Error::corrupt(format!(
                "H.264 SPS: picture {}x{} macroblocks is beyond any level",
                sps.pic_width_in_mbs_minus1 + 1,
                sps.pic_height_in_map_units_minus1 + 1
            )));
        }
        sps.frame_mbs_only_flag = r.read_bit()?;
        if !sps.frame_mbs_only_flag {
            sps.mb_adaptive_frame_field_flag = r.read_bit()?;
        }
        sps.direct_8x8_inference_flag = r.read_bit()?;
        sps.frame_cropping_flag = r.read_bit()?;
        if sps.frame_cropping_flag {
            sps.frame_crop_left_offset = r.read_ue()?;
            sps.frame_crop_right_offset = r.read_ue()?;
            sps.frame_crop_top_offset = r.read_ue()?;
            sps.frame_crop_bottom_offset = r.read_ue()?;
        }
        if r.read_bit()? {
            sps.vui_parameters = Some(VuiParameters::parse(r)?);
        }
        Ok(sps)
    }

    /// `ChromaArrayType` (clause 7.4.2.1.1): the chroma format the decoding
    /// process sees, which is 0 when the colour planes are coded separately.
    pub fn chroma_array_type(&self) -> u32 {
        if self.separate_colour_plane_flag {
            0
        } else {
            self.chroma_format_idc
        }
    }

    /// `(SubWidthC, SubHeightC)` (Table 6-1); `None` for monochrome and for
    /// 4:4:4 with separate colour planes, where the variables are undefined.
    pub fn sub_wh_c(&self) -> Option<(u32, u32)> {
        match self.chroma_array_type() {
            1 => Some((2, 2)),
            2 => Some((2, 1)),
            3 => Some((1, 1)),
            _ => None,
        }
    }

    /// `MbWidthC` and `MbHeightC` (clause 6.2): chroma samples per macroblock.
    pub fn mb_wh_c(&self) -> (u32, u32) {
        match self.sub_wh_c() {
            Some((sw, sh)) => (16 / sw, 16 / sh),
            None => (0, 0),
        }
    }

    /// `BitDepthY` (clause 7.4.2.1.1).
    pub fn bit_depth_y(&self) -> u32 {
        8 + self.bit_depth_luma_minus8
    }

    /// `BitDepthC` (clause 7.4.2.1.1).
    pub fn bit_depth_c(&self) -> u32 {
        8 + self.bit_depth_chroma_minus8
    }

    /// `QpBdOffsetY` (clause 7.4.2.1.1).
    pub fn qp_bd_offset_y(&self) -> i32 {
        6 * self.bit_depth_luma_minus8 as i32
    }

    /// `QpBdOffsetC` (clause 7.4.2.1.1).
    pub fn qp_bd_offset_c(&self) -> i32 {
        6 * self.bit_depth_chroma_minus8 as i32
    }

    /// `MaxFrameNum` (clause 7.4.2.1.1).
    pub fn max_frame_num(&self) -> u32 {
        1 << (self.log2_max_frame_num_minus4 + 4)
    }

    /// `MaxPicOrderCntLsb` (clause 7.4.2.1.1).
    pub fn max_pic_order_cnt_lsb(&self) -> u32 {
        1 << (self.log2_max_pic_order_cnt_lsb_minus4 + 4)
    }

    /// `PicWidthInMbs` (clause 7.4.2.1.1).
    pub fn pic_width_in_mbs(&self) -> u32 {
        self.pic_width_in_mbs_minus1 + 1
    }

    /// `PicHeightInMapUnits` (clause 7.4.2.1.1).
    pub fn pic_height_in_map_units(&self) -> u32 {
        self.pic_height_in_map_units_minus1 + 1
    }

    /// `FrameHeightInMbs` (clause 7.4.2.1.1): map units are frame macroblock
    /// rows only when `frame_mbs_only_flag` is 1, field macroblock pairs
    /// otherwise.
    pub fn frame_height_in_mbs(&self) -> u32 {
        (2 - u32::from(self.frame_mbs_only_flag)) * self.pic_height_in_map_units()
    }

    /// `PicSizeInMapUnits` (clause 7.4.2.1.1).
    pub fn pic_size_in_map_units(&self) -> u32 {
        self.pic_width_in_mbs() * self.pic_height_in_map_units()
    }

    /// `(CropUnitX, CropUnitY)` (clause 7.4.2.1.1).
    pub fn crop_units(&self) -> (u32, u32) {
        match self.sub_wh_c() {
            None => (1, 2 - u32::from(self.frame_mbs_only_flag)),
            Some((sw, sh)) => (sw, sh * (2 - u32::from(self.frame_mbs_only_flag))),
        }
    }

    /// Visible frame size in luma samples after applying the cropping rectangle
    /// (clause 7.4.2.1.1), which is what a player must display.
    pub fn cropped_size(&self) -> Result<(u32, u32)> {
        let (unit_x, unit_y) = self.crop_units();
        let full_w = self.pic_width_in_mbs() * 16;
        let full_h = self.frame_height_in_mbs() * 16;
        let cut_w = unit_x * (self.frame_crop_left_offset + self.frame_crop_right_offset);
        let cut_h = unit_y * (self.frame_crop_top_offset + self.frame_crop_bottom_offset);
        if cut_w >= full_w || cut_h >= full_h {
            return Err(Error::corrupt(format!(
                "H.264 SPS: cropping removes the whole picture ({full_w}x{full_h}, cut {cut_w}x{cut_h})"
            )));
        }
        Ok((full_w - cut_w, full_h - cut_h))
    }
}
