//! Sequence parameter set (spec 7.3.2.1, 7.4.2.1) incl. VUI (Annex E).

use ec_core::BitReader;
use ec_core::error::{Error, Result};

/// The 4x4 and 8x8 quantisation scaling lists (spec 7.3.2.1.1.1).
///
/// Stored fully resolved: fall-back rules (flat default / previous list) are
/// applied at parse time so a decoder indexes these directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalingLists {
    /// Six 4x4 lists in raster order: Intra Y/Cb/Cr, Inter Y/Cb/Cr.
    pub list_4x4: [[u8; 16]; 6],
    /// 8x8 lists: Intra Y, Inter Y (plus 4 chroma lists for 4:4:4).
    pub list_8x8: [[u8; 64]; 6],
}

/// Default (flat) scaling: weightScale 16 everywhere = no weighting.
pub const FLAT_4X4: [u8; 16] = [16; 16];
/// Flat 8x8 list.
pub const FLAT_8X8: [u8; 64] = [16; 64];

/// Default Intra 4x4 scaling list (spec Table 7-3), in zig-zag scan order.
pub const DEFAULT_4X4_INTRA: [u8; 16] = [
    6, 13, 13, 20, 20, 20, 28, 28, 28, 28, 32, 32, 32, 37, 37, 42,
];
/// Default Inter 4x4 scaling list (spec Table 7-3), in zig-zag scan order.
pub const DEFAULT_4X4_INTER: [u8; 16] = [
    10, 14, 14, 20, 20, 20, 24, 24, 24, 24, 27, 27, 27, 30, 30, 34,
];
/// Default Intra 8x8 scaling list (spec Table 7-4), in zig-zag scan order.
pub const DEFAULT_8X8_INTRA: [u8; 64] = [
    6, 10, 10, 13, 11, 13, 16, 16, 16, 16, 18, 18, 18, 18, 18, 23, 23, 23, 23, 23, 23, 25, 25, 25,
    25, 25, 25, 25, 27, 27, 27, 27, 27, 27, 27, 27, 29, 29, 29, 29, 29, 29, 29, 31, 31, 31, 31, 31,
    31, 33, 33, 33, 33, 33, 36, 36, 36, 36, 38, 38, 38, 40, 40, 42,
];
/// Default Inter 8x8 scaling list (spec Table 7-4), in zig-zag scan order.
pub const DEFAULT_8X8_INTER: [u8; 64] = [
    9, 13, 13, 15, 13, 15, 17, 17, 17, 17, 19, 19, 19, 19, 19, 21, 21, 21, 21, 21, 21, 22, 22, 22,
    22, 22, 22, 22, 24, 24, 24, 24, 24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 27, 27, 27, 27, 27,
    27, 28, 28, 28, 28, 28, 30, 30, 30, 30, 32, 32, 32, 33, 33, 35,
];

/// Place a zig-zag-scan-order list into raster order.
fn scan_to_raster<const N: usize>(scan_order: &[u8; N], scan: &[u8; N]) -> [u8; N] {
    let mut out = [0u8; N];
    for (j, &v) in scan_order.iter().enumerate() {
        out[scan[j] as usize] = v;
    }
    out
}

impl Default for ScalingLists {
    /// Flat lists — the state when no scaling matrix is signalled.
    fn default() -> ScalingLists {
        ScalingLists {
            list_4x4: [FLAT_4X4; 6],
            list_8x8: [FLAT_8X8; 6],
        }
    }
}

/// Zig-zag scan for 4x4 blocks (spec Table 8-13, frame scan), used to place
/// scaling-list entries and residual levels into raster order.
pub const ZIGZAG_4X4: [u8; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];

/// Zig-zag (frame) scan for 8x8 blocks (spec Table 8-14).
pub const ZIGZAG_8X8: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Parse one `scaling_list` (spec 7.3.2.1.1.1) of `N` entries into raster
/// order. Returns `None` when `use_default_scaling_matrix_flag` fired.
fn parse_scaling_list<const N: usize>(
    r: &mut BitReader<'_>,
    scan: &[u8; N],
) -> Result<Option<[u8; N]>> {
    let mut list = [0u8; N];
    let mut last_scale = 8i32;
    let mut next_scale = 8i32;
    for j in 0..N {
        if next_scale != 0 {
            let delta = r.read_se()?;
            next_scale = (last_scale + delta + 256) % 256;
            if j == 0 && next_scale == 0 {
                return Ok(None); // use_default_scaling_matrix_flag
            }
        }
        let v = if next_scale == 0 {
            last_scale
        } else {
            next_scale
        };
        list[scan[j] as usize] = v as u8;
        last_scale = v;
    }
    Ok(Some(list))
}

/// Parse the `seq_scaling_list` / `pic_scaling_list` block shared by SPS and
/// PPS (spec 7.3.2.1, fall-back rules of Table 7-2). `count` is 8
/// (4:2:0/4:2:2) or 12 (4:4:4). `sps_lists` is `None` in an SPS (fall-back
/// rule A: absent list 0/3/6/7 takes the *default* list) and `Some` in a PPS
/// whose SPS carried a matrix (rule B: those indices take the SPS list).
/// Lists are returned in raster order, fully resolved.
pub(crate) fn parse_scaling_matrix(
    r: &mut BitReader<'_>,
    count: usize,
    sps_lists: Option<&ScalingLists>,
) -> Result<ScalingLists> {
    let mut out = ScalingLists::default();
    let d4_intra = scan_to_raster(&DEFAULT_4X4_INTRA, &ZIGZAG_4X4);
    let d4_inter = scan_to_raster(&DEFAULT_4X4_INTER, &ZIGZAG_4X4);
    let d8_intra = scan_to_raster(&DEFAULT_8X8_INTRA, &ZIGZAG_8X8);
    let d8_inter = scan_to_raster(&DEFAULT_8X8_INTER, &ZIGZAG_8X8);
    for idx in 0..count {
        let present = r.read_bit()?;
        if idx < 6 {
            let default = if idx < 3 { d4_intra } else { d4_inter };
            let fallback = match idx {
                0 | 3 => match sps_lists {
                    Some(s) => s.list_4x4[idx],
                    None => default,
                },
                _ => out.list_4x4[idx - 1],
            };
            out.list_4x4[idx] = if present {
                parse_scaling_list::<16>(r, &ZIGZAG_4X4)?.unwrap_or(default)
            } else {
                fallback
            };
        } else {
            let i = idx - 6;
            let default = if i % 2 == 0 { d8_intra } else { d8_inter };
            let fallback = if i < 2 {
                match sps_lists {
                    Some(s) => s.list_8x8[i],
                    None => default,
                }
            } else {
                out.list_8x8[i - 2]
            };
            out.list_8x8[i] = if present {
                parse_scaling_list::<64>(r, &ZIGZAG_8X8)?.unwrap_or(default)
            } else {
                fallback
            };
        }
    }
    Ok(out)
}

/// HRD parameters (spec E.1.2), carried for completeness.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hrd {
    /// Per-CPB `(bit_rate_value_minus1, cpb_size_value_minus1, cbr_flag)`.
    pub cpb: Vec<(u32, u32, bool)>,
    /// `bit_rate_scale`.
    pub bit_rate_scale: u8,
    /// `cpb_size_scale`.
    pub cpb_size_scale: u8,
    /// `initial_cpb_removal_delay_length_minus1`.
    pub initial_cpb_removal_delay_length_minus1: u8,
    /// `cpb_removal_delay_length_minus1`.
    pub cpb_removal_delay_length_minus1: u8,
    /// `dpb_output_delay_length_minus1`.
    pub dpb_output_delay_length_minus1: u8,
    /// `time_offset_length`.
    pub time_offset_length: u8,
}

impl Hrd {
    fn parse(r: &mut BitReader<'_>) -> Result<Hrd> {
        let cpb_cnt_minus1 = r.read_ue()?;
        if cpb_cnt_minus1 > 31 {
            return Err(Error::corrupt("cpb_cnt_minus1 > 31"));
        }
        let bit_rate_scale = r.read_bits(4)? as u8;
        let cpb_size_scale = r.read_bits(4)? as u8;
        let mut cpb = Vec::with_capacity(cpb_cnt_minus1 as usize + 1);
        for _ in 0..=cpb_cnt_minus1 {
            let bit_rate = r.read_ue()?;
            let cpb_size = r.read_ue()?;
            let cbr = r.read_bit()?;
            cpb.push((bit_rate, cpb_size, cbr));
        }
        Ok(Hrd {
            cpb,
            bit_rate_scale,
            cpb_size_scale,
            initial_cpb_removal_delay_length_minus1: r.read_bits(5)? as u8,
            cpb_removal_delay_length_minus1: r.read_bits(5)? as u8,
            dpb_output_delay_length_minus1: r.read_bits(5)? as u8,
            time_offset_length: r.read_bits(5)? as u8,
        })
    }
}

/// VUI parameters (spec E.1.1).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Vui {
    /// Sample aspect ratio `(width, height)` when signalled; `aspect_ratio_idc`
    /// table values are resolved here, Extended_SAR reads the pair verbatim.
    pub sample_aspect_ratio: Option<(u16, u16)>,
    /// `overscan_appropriate_flag` when `overscan_info_present_flag`.
    pub overscan_appropriate: Option<bool>,
    /// `video_format` (E-2) when signalled.
    pub video_format: Option<u8>,
    /// `video_full_range_flag`.
    pub video_full_range: bool,
    /// H.273 `(colour_primaries, transfer_characteristics, matrix_coefficients)`.
    pub colour_description: Option<(u8, u8, u8)>,
    /// `(chroma_sample_loc_type_top_field, chroma_sample_loc_type_bottom_field)`.
    pub chroma_sample_loc: Option<(u32, u32)>,
    /// `(num_units_in_tick, time_scale, fixed_frame_rate_flag)`.
    pub timing_info: Option<(u32, u32, bool)>,
    /// NAL HRD parameters.
    pub nal_hrd: Option<Hrd>,
    /// VCL HRD parameters.
    pub vcl_hrd: Option<Hrd>,
    /// `low_delay_hrd_flag` (present when either HRD is).
    pub low_delay_hrd: bool,
    /// `pic_struct_present_flag`.
    pub pic_struct_present: bool,
    /// Bitstream restriction fields, in spec order.
    pub bitstream_restriction: Option<BitstreamRestriction>,
}

/// `bitstream_restriction` fields (spec E.1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BitstreamRestriction {
    /// `motion_vectors_over_pic_boundaries_flag`.
    pub motion_vectors_over_pic_boundaries: bool,
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

/// Table E-1 aspect ratios for `aspect_ratio_idc` 1..=16.
const SAR_TABLE: [(u16, u16); 17] = [
    (0, 0), // 0 = unspecified
    (1, 1),
    (12, 11),
    (10, 11),
    (16, 11),
    (40, 33),
    (24, 11),
    (20, 11),
    (32, 11),
    (80, 33),
    (18, 11),
    (15, 11),
    (64, 33),
    (160, 99),
    (4, 3),
    (3, 2),
    (2, 1),
];

impl Vui {
    fn parse(r: &mut BitReader<'_>) -> Result<Vui> {
        let mut vui = Vui::default();
        if r.read_bit()? {
            // aspect_ratio_info_present_flag
            let idc = r.read_bits(8)? as u8;
            vui.sample_aspect_ratio = match idc {
                0 => None,
                255 => Some((r.read_bits(16)? as u16, r.read_bits(16)? as u16)),
                1..=16 => Some(SAR_TABLE[idc as usize]),
                _ => None, // reserved: ignore per spec
            };
        }
        if r.read_bit()? {
            vui.overscan_appropriate = Some(r.read_bit()?);
        }
        if r.read_bit()? {
            // video_signal_type_present_flag
            vui.video_format = Some(r.read_bits(3)? as u8);
            vui.video_full_range = r.read_bit()?;
            if r.read_bit()? {
                vui.colour_description = Some((
                    r.read_bits(8)? as u8,
                    r.read_bits(8)? as u8,
                    r.read_bits(8)? as u8,
                ));
            }
        }
        if r.read_bit()? {
            vui.chroma_sample_loc = Some((r.read_ue()?, r.read_ue()?));
        }
        if r.read_bit()? {
            vui.timing_info = Some((r.read_bits(32)?, r.read_bits(32)?, r.read_bit()?));
        }
        let nal_hrd_present = r.read_bit()?;
        if nal_hrd_present {
            vui.nal_hrd = Some(Hrd::parse(r)?);
        }
        let vcl_hrd_present = r.read_bit()?;
        if vcl_hrd_present {
            vui.vcl_hrd = Some(Hrd::parse(r)?);
        }
        if nal_hrd_present || vcl_hrd_present {
            vui.low_delay_hrd = r.read_bit()?;
        }
        vui.pic_struct_present = r.read_bit()?;
        if r.read_bit()? {
            vui.bitstream_restriction = Some(BitstreamRestriction {
                motion_vectors_over_pic_boundaries: r.read_bit()?,
                max_bytes_per_pic_denom: r.read_ue()?,
                max_bits_per_mb_denom: r.read_ue()?,
                log2_max_mv_length_horizontal: r.read_ue()?,
                log2_max_mv_length_vertical: r.read_ue()?,
                max_num_reorder_frames: r.read_ue()?,
                max_dec_frame_buffering: r.read_ue()?,
            });
        }
        Ok(vui)
    }
}

/// Sequence parameter set (spec 7.4.2.1) with decode-ready derived geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sps {
    /// `profile_idc`.
    pub profile_idc: u8,
    /// `constraint_setX_flag` bits (bit 7 = set0) + reserved bits, verbatim.
    pub constraint_flags: u8,
    /// `level_idc`.
    pub level_idc: u8,
    /// `seq_parameter_set_id` (0..=31).
    pub id: u8,
    /// `chroma_format_idc`: 0 mono, 1 = 4:2:0, 2 = 4:2:2, 3 = 4:4:4.
    pub chroma_format_idc: u8,
    /// `separate_colour_plane_flag` (4:4:4 only).
    pub separate_colour_plane: bool,
    /// Luma bit depth (8..=14).
    pub bit_depth_luma: u8,
    /// Chroma bit depth (8..=14).
    pub bit_depth_chroma: u8,
    /// `qpprime_y_zero_transform_bypass_flag`.
    pub transform_bypass: bool,
    /// Scaling lists when `seq_scaling_matrix_present_flag`, resolved.
    pub scaling_lists: Option<ScalingLists>,
    /// `log2_max_frame_num` (4..=16, offset applied).
    pub log2_max_frame_num: u8,
    /// `pic_order_cnt_type` (0..=2).
    pub pic_order_cnt_type: u8,
    /// `log2_max_pic_order_cnt_lsb` (POC type 0, offset applied).
    pub log2_max_pic_order_cnt_lsb: u8,
    /// `delta_pic_order_always_zero_flag` (POC type 1).
    pub delta_pic_order_always_zero: bool,
    /// `offset_for_non_ref_pic` (POC type 1).
    pub offset_for_non_ref_pic: i32,
    /// `offset_for_top_to_bottom_field` (POC type 1).
    pub offset_for_top_to_bottom_field: i32,
    /// `offset_for_ref_frame` list (POC type 1).
    pub offsets_for_ref_frame: Vec<i32>,
    /// `max_num_ref_frames`.
    pub max_num_ref_frames: u32,
    /// `gaps_in_frame_num_value_allowed_flag`.
    pub gaps_in_frame_num_allowed: bool,
    /// `frame_mbs_only_flag`.
    pub frame_mbs_only: bool,
    /// `mb_adaptive_frame_field_flag` (when `!frame_mbs_only`).
    pub mb_adaptive_frame_field: bool,
    /// `direct_8x8_inference_flag`.
    pub direct_8x8_inference: bool,
    /// Crop offsets in luma samples `(left, right, top, bottom)`, already
    /// scaled by the chroma-format crop units (spec 7.4.2.1.1).
    pub crop: (u32, u32, u32, u32),
    /// VUI parameters when present.
    pub vui: Option<Vui>,

    // ---- derived, computed once at parse time ----
    /// `PicWidthInMbs`.
    pub mb_width: u32,
    /// `FrameHeightInMbs` (frame coding).
    pub mb_height: u32,
    /// Coded luma width in samples (mb_width * 16).
    pub coded_width: u32,
    /// Coded luma height in samples.
    pub coded_height: u32,
    /// Visible width after cropping.
    pub width: u32,
    /// Visible height after cropping.
    pub height: u32,
}

impl Sps {
    /// Parse an SPS RBSP (header byte already consumed, emulation stripped).
    pub fn parse(rbsp: &[u8]) -> Result<Sps> {
        let mut r = BitReader::new(rbsp);
        let r = &mut r;
        let profile_idc = r.read_bits(8)? as u8;
        let constraint_flags = r.read_bits(8)? as u8;
        let level_idc = r.read_bits(8)? as u8;
        let id = r.read_ue()?;
        if id > 31 {
            return Err(Error::corrupt("seq_parameter_set_id > 31"));
        }

        let mut chroma_format_idc = 1u8;
        let mut separate_colour_plane = false;
        let mut bit_depth_luma = 8u8;
        let mut bit_depth_chroma = 8u8;
        let mut transform_bypass = false;
        let mut scaling_lists = None;
        if matches!(
            profile_idc,
            100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
        ) {
            let cf = r.read_ue()?;
            if cf > 3 {
                return Err(Error::corrupt("chroma_format_idc > 3"));
            }
            chroma_format_idc = cf as u8;
            if chroma_format_idc == 3 {
                separate_colour_plane = r.read_bit()?;
            }
            let bdl = r.read_ue()?;
            let bdc = r.read_ue()?;
            if bdl > 6 || bdc > 6 {
                return Err(Error::corrupt("bit_depth_minus8 > 6"));
            }
            bit_depth_luma = 8 + bdl as u8;
            bit_depth_chroma = 8 + bdc as u8;
            transform_bypass = r.read_bit()?;
            if r.read_bit()? {
                let count = if chroma_format_idc == 3 { 12 } else { 8 };
                scaling_lists = Some(parse_scaling_matrix(r, count, None)?);
            }
        }

        let log2_mfn = r.read_ue()?;
        if log2_mfn > 12 {
            return Err(Error::corrupt("log2_max_frame_num_minus4 > 12"));
        }
        let log2_max_frame_num = 4 + log2_mfn as u8;
        let pic_order_cnt_type = r.read_ue()?;
        if pic_order_cnt_type > 2 {
            return Err(Error::corrupt("pic_order_cnt_type > 2"));
        }
        let pic_order_cnt_type = pic_order_cnt_type as u8;
        let mut log2_max_pic_order_cnt_lsb = 0u8;
        let mut delta_pic_order_always_zero = false;
        let mut offset_for_non_ref_pic = 0i32;
        let mut offset_for_top_to_bottom_field = 0i32;
        let mut offsets_for_ref_frame = Vec::new();
        match pic_order_cnt_type {
            0 => {
                let v = r.read_ue()?;
                if v > 12 {
                    return Err(Error::corrupt("log2_max_pic_order_cnt_lsb_minus4 > 12"));
                }
                log2_max_pic_order_cnt_lsb = 4 + v as u8;
            }
            1 => {
                delta_pic_order_always_zero = r.read_bit()?;
                offset_for_non_ref_pic = r.read_se()?;
                offset_for_top_to_bottom_field = r.read_se()?;
                let n = r.read_ue()?;
                if n > 255 {
                    return Err(Error::corrupt(
                        "num_ref_frames_in_pic_order_cnt_cycle > 255",
                    ));
                }
                offsets_for_ref_frame.reserve(n as usize);
                for _ in 0..n {
                    offsets_for_ref_frame.push(r.read_se()?);
                }
            }
            _ => {}
        }
        let max_num_ref_frames = r.read_ue()?;
        let gaps_in_frame_num_allowed = r.read_bit()?;
        let pic_width_in_mbs_minus1 = r.read_ue()?;
        let pic_height_in_map_units_minus1 = r.read_ue()?;
        let frame_mbs_only = r.read_bit()?;
        let mb_adaptive_frame_field = if !frame_mbs_only {
            r.read_bit()?
        } else {
            false
        };
        let direct_8x8_inference = r.read_bit()?;

        let mb_width = pic_width_in_mbs_minus1 + 1;
        let map_height = pic_height_in_map_units_minus1 + 1;
        let mb_height = map_height * if frame_mbs_only { 1 } else { 2 };
        if mb_width > 1024 || mb_height > 1024 {
            return Err(Error::corrupt("picture larger than 16384x16384"));
        }
        let coded_width = mb_width * 16;
        let coded_height = mb_height * 16;

        // Crop units (spec 7.4.2.1.1): chroma-format dependent.
        let (crop_x, crop_y_frame) = match chroma_format_idc {
            0 => (1, 1),
            1 => (2, 2),
            2 => (2, 1),
            _ => (1, 1),
        };
        let crop_y = crop_y_frame * if frame_mbs_only { 1 } else { 2 };
        let crop = if r.read_bit()? {
            let l = r.read_ue()?;
            let rr = r.read_ue()?;
            let t = r.read_ue()?;
            let b = r.read_ue()?;
            (crop_x * l, crop_x * rr, crop_y * t, crop_y * b)
        } else {
            (0, 0, 0, 0)
        };
        let width = coded_width
            .checked_sub(crop.0 + crop.1)
            .filter(|&w| w > 0)
            .ok_or_else(|| Error::corrupt("frame cropping wider than the picture"))?;
        let height = coded_height
            .checked_sub(crop.2 + crop.3)
            .filter(|&h| h > 0)
            .ok_or_else(|| Error::corrupt("frame cropping taller than the picture"))?;

        let vui = if r.read_bit()? {
            Some(Vui::parse(r)?)
        } else {
            None
        };

        Ok(Sps {
            profile_idc,
            constraint_flags,
            level_idc,
            id: id as u8,
            chroma_format_idc,
            separate_colour_plane,
            bit_depth_luma,
            bit_depth_chroma,
            transform_bypass,
            scaling_lists,
            log2_max_frame_num,
            pic_order_cnt_type,
            log2_max_pic_order_cnt_lsb,
            delta_pic_order_always_zero,
            offset_for_non_ref_pic,
            offset_for_top_to_bottom_field,
            offsets_for_ref_frame,
            max_num_ref_frames,
            gaps_in_frame_num_allowed,
            frame_mbs_only,
            mb_adaptive_frame_field,
            direct_8x8_inference,
            crop,
            vui,
            mb_width,
            mb_height,
            coded_width,
            coded_height,
            width,
            height,
        })
    }

    /// Total macroblocks per picture.
    pub fn mbs_per_picture(&self) -> u32 {
        self.mb_width * self.mb_height
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_core::BitWriter;

    /// Build a minimal Baseline QCIF SPS and check the derived geometry.
    #[test]
    fn baseline_qcif_geometry() {
        let mut w = BitWriter::new();
        w.write_bits(66, 8); // profile_idc Baseline
        w.write_bits(0xC0, 8); // constraint flags
        w.write_bits(10, 8); // level_idc
        w.write_ue(0); // sps id
        w.write_ue(0); // log2_max_frame_num_minus4
        w.write_ue(0); // pic_order_cnt_type
        w.write_ue(0); // log2_max_pic_order_cnt_lsb_minus4
        w.write_ue(1); // max_num_ref_frames
        w.write_bit(false); // gaps allowed
        w.write_ue(10); // width 11 MBs = 176
        w.write_ue(8); // height 9 MBs = 144
        w.write_bit(true); // frame_mbs_only
        w.write_bit(true); // direct_8x8_inference
        w.write_bit(false); // no cropping
        w.write_bit(false); // no VUI
        w.write_bit(true); // rbsp_stop_one_bit
        w.align_to_byte();
        let sps = Sps::parse(w.as_bytes()).unwrap();
        assert_eq!(sps.profile_idc, 66);
        assert_eq!((sps.mb_width, sps.mb_height), (11, 9));
        assert_eq!((sps.width, sps.height), (176, 144));
        assert_eq!(sps.chroma_format_idc, 1);
        assert_eq!(sps.bit_depth_luma, 8);
        assert!(sps.frame_mbs_only);
    }

    /// Cropping in 4:2:0 is in 2-sample units both directions.
    #[test]
    fn crop_units_420() {
        let mut w = BitWriter::new();
        w.write_bits(66, 8);
        w.write_bits(0, 8);
        w.write_bits(40, 8);
        w.write_ue(0);
        w.write_ue(0);
        w.write_ue(2); // poc type 2
        w.write_ue(1);
        w.write_bit(false);
        w.write_ue(119); // 1920
        w.write_ue(67); // 68 MBs = 1088
        w.write_bit(true);
        w.write_bit(true);
        w.write_bit(true); // cropping
        w.write_ue(0);
        w.write_ue(0);
        w.write_ue(0);
        w.write_ue(4); // bottom crop 4 units = 8 luma rows
        w.write_bit(false); // no VUI
        w.write_bit(true);
        w.align_to_byte();
        let sps = Sps::parse(w.as_bytes()).unwrap();
        assert_eq!((sps.width, sps.height), (1920, 1080));
        assert_eq!(sps.crop, (0, 0, 0, 8));
    }

    /// Truncated SPS is NeedMore, never a panic.
    #[test]
    fn truncated_is_need_more() {
        assert!(Sps::parse(&[0x42]).unwrap_err().is_need_more());
        assert!(Sps::parse(&[]).unwrap_err().is_need_more());
    }
}
