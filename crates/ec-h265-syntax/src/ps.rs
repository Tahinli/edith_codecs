//! Parameter sets: VPS, SPS and PPS, written and parsed.
//!
//! The field sets here are sized by what a *stateless hardware decoder* asks
//! for, not by what this family's own encoder writes: everything in
//! `VAPictureParameterBufferHEVC` is either a field on [`Sps`]/[`Pps`] or
//! derivable from one by a named method. Anything the parser walks past but does
//! not model (scaling lists, HRD, the multilayer and 3D extensions) is a thing
//! no consumer in this family reads, and each such skip says so where it happens.

use crate::vui::VuiParameters;
use ec_core::bitio::{BitReader, BitWriter};
use ec_core::error::{Error, Result};

/// `profile_tier_level()` for a single-sub-layer stream (7.3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileTierLevel {
    /// `general_profile_space`, 0 for everything in the wild.
    pub profile_space: u8,
    /// False = Main tier, true = High tier.
    pub tier_flag: bool,
    /// 1 = Main, 2 = Main 10, 3 = Main Still Picture, 4 = range extensions.
    pub profile_idc: u8,
    /// `general_profile_compatibility_flag[0..32]` as a bit set, bit `j` = flag `j`.
    pub profile_compatibility: u32,
    /// `general_progressive_source_flag`.
    pub progressive_source: bool,
    /// `general_interlaced_source_flag`.
    pub interlaced_source: bool,
    /// `general_non_packed_constraint_flag`.
    pub non_packed_constraint: bool,
    /// `general_frame_only_constraint_flag`.
    pub frame_only_constraint: bool,
    /// The 44 bits between the source flags and `general_level_idc`: the
    /// profile-dependent constraint flags, their reserved padding and
    /// `general_inbld_flag`. Kept verbatim so a re-write is byte-identical,
    /// because their *meaning* depends on `profile_idc` while their length
    /// never does.
    pub constraint_bits: u64,
    /// `general_level_idc`: 30 x the level number (level 4 = 120).
    pub level_idc: u8,
}

impl ProfileTierLevel {
    /// Main profile at `level_idc`, progressive, frame-only — what an intra
    /// 8-bit 4:2:0 encoder emits.
    pub fn main(level_idc: u8) -> ProfileTierLevel {
        ProfileTierLevel {
            profile_space: 0,
            tier_flag: false,
            profile_idc: 1,
            // Main is compatible with itself; bit 1 is what every decoder checks.
            profile_compatibility: 1 << 1,
            progressive_source: true,
            interlaced_source: false,
            non_packed_constraint: true,
            frame_only_constraint: true,
            constraint_bits: 0,
            level_idc,
        }
    }

    /// The smallest level whose limits hold for `width x height` at `fps`
    /// (Annex A, Table A.8/A.9 luma sample and sample-rate limits).
    ///
    /// Levels are a decoder's promise about buffer sizes; naming one too small
    /// is what makes a hardware decoder refuse a stream it could otherwise play,
    /// so this rounds up and stops at 6.2.
    pub fn level_for(width: u32, height: u32, fps: f64) -> u8 {
        // (level_idc, MaxLumaPs, MaxLumaSr)
        const LIMITS: &[(u8, u64, u64)] = &[
            (30, 36_864, 552_960),
            (60, 122_880, 3_686_400),
            (63, 245_760, 7_372_800),
            (90, 552_960, 16_588_800),
            (93, 983_040, 33_177_600),
            (120, 2_228_224, 66_846_720),
            (123, 2_228_224, 133_693_440),
            (126, 2_228_224, 267_386_880),
            (150, 8_912_896, 534_773_760),
            (153, 8_912_896, 1_069_547_520),
            (156, 8_912_896, 1_069_547_520),
            (180, 35_651_584, 2_139_095_040),
            (183, 35_651_584, 4_278_190_080),
            (186, 35_651_584, 4_278_190_080),
        ];
        let samples = u64::from(width) * u64::from(height);
        let rate = (samples as f64 * fps.max(1.0)) as u64;
        for &(idc, max_ps, max_sr) in LIMITS {
            if samples <= max_ps && rate <= max_sr {
                return idc;
            }
        }
        186
    }

    /// Write `profile_tier_level(1, 0)` — 96 bits, always.
    pub fn write(&self, w: &mut BitWriter) {
        w.write_bits(u32::from(self.profile_space), 2);
        w.write_bit(self.tier_flag);
        w.write_bits(u32::from(self.profile_idc), 5);
        for j in 0..32 {
            w.write_bit(self.profile_compatibility & (1 << j) != 0);
        }
        w.write_bit(self.progressive_source);
        w.write_bit(self.interlaced_source);
        w.write_bit(self.non_packed_constraint);
        w.write_bit(self.frame_only_constraint);
        w.write_bits64(self.constraint_bits, 44);
        w.write_bits(u32::from(self.level_idc), 8);
    }

    /// Parse `profile_tier_level(1, max_sub_layers_minus1)`.
    pub fn parse(r: &mut BitReader, max_sub_layers_minus1: u32) -> Result<ProfileTierLevel> {
        let profile_space = r.read_bits(2)? as u8;
        let tier_flag = r.read_bit()?;
        let profile_idc = r.read_bits(5)? as u8;
        let mut profile_compatibility = 0u32;
        for j in 0..32 {
            if r.read_bit()? {
                profile_compatibility |= 1 << j;
            }
        }
        let progressive_source = r.read_bit()?;
        let interlaced_source = r.read_bit()?;
        let non_packed_constraint = r.read_bit()?;
        let frame_only_constraint = r.read_bit()?;
        let constraint_bits = r.read_bits64(44)?;
        let level_idc = r.read_bits(8)? as u8;
        let mut sub_layer_profile = Vec::new();
        let mut sub_layer_level = Vec::new();
        for _ in 0..max_sub_layers_minus1 {
            sub_layer_profile.push(r.read_bit()?);
            sub_layer_level.push(r.read_bit()?);
        }
        if max_sub_layers_minus1 > 0 {
            for _ in max_sub_layers_minus1..8 {
                r.read_bits(2)?;
            }
        }
        for i in 0..max_sub_layers_minus1 as usize {
            if sub_layer_profile[i] {
                r.skip_bits(88)?;
            }
            if sub_layer_level[i] {
                r.read_bits(8)?;
            }
        }
        Ok(ProfileTierLevel {
            profile_space,
            tier_flag,
            profile_idc,
            profile_compatibility,
            progressive_source,
            interlaced_source,
            non_packed_constraint,
            frame_only_constraint,
            constraint_bits,
            level_idc,
        })
    }
}

/// The video parameter set (7.3.2.1).
///
/// A single-layer stream needs one and nothing in it varies, but a decoder that
/// never sees a VPS refuses the stream, so it is written all the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vps {
    /// `vps_video_parameter_set_id`.
    pub id: u8,
    /// Profile, tier and level of the base layer.
    pub ptl: ProfileTierLevel,
    /// `vps_max_dec_pic_buffering_minus1[0]`.
    pub max_dec_pic_buffering_minus1: u32,
    /// `vps_max_num_reorder_pics[0]`; 0 for an intra-only stream.
    pub max_num_reorder_pics: u32,
}

impl Vps {
    /// The RBSP, trailing bits included.
    pub fn to_rbsp(&self) -> Vec<u8> {
        let mut w = BitWriter::with_capacity(32);
        w.write_bits(u32::from(self.id), 4);
        w.write_bit(true); // vps_base_layer_internal_flag
        w.write_bit(true); // vps_base_layer_available_flag
        w.write_bits(0, 6); // vps_max_layers_minus1
        w.write_bits(0, 3); // vps_max_sub_layers_minus1
        w.write_bit(true); // vps_temporal_id_nesting_flag
        w.write_bits(0xffff, 16); // vps_reserved_0xffff_16bits
        self.ptl.write(&mut w);
        w.write_bit(true); // vps_sub_layer_ordering_info_present_flag
        w.write_ue(self.max_dec_pic_buffering_minus1);
        w.write_ue(self.max_num_reorder_pics);
        w.write_ue(0); // vps_max_latency_increase_plus1
        w.write_bits(0, 6); // vps_max_layer_id
        w.write_ue(0); // vps_num_layer_sets_minus1
        w.write_bit(false); // vps_timing_info_present_flag
        w.write_bit(false); // vps_extension_flag
        rbsp_trailing_bits(&mut w);
        w.into_bytes()
    }

    /// Parse a VPS RBSP.
    pub fn parse(rbsp: &[u8]) -> Result<Vps> {
        let mut r = BitReader::new(rbsp);
        let id = r.read_bits(4)? as u8;
        r.read_bit()?;
        r.read_bit()?;
        r.read_bits(6)?; // vps_max_layers_minus1
        let max_sub_layers_minus1 = r.read_bits(3)?;
        r.read_bit()?;
        r.read_bits(16)?;
        let ptl = ProfileTierLevel::parse(&mut r, max_sub_layers_minus1)?;
        let ordering_info = r.read_bit()?;
        let first = if ordering_info {
            0
        } else {
            max_sub_layers_minus1
        };
        let mut max_dec_pic_buffering_minus1 = 0;
        let mut max_num_reorder_pics = 0;
        for _ in first..=max_sub_layers_minus1 {
            max_dec_pic_buffering_minus1 = r.read_ue()?;
            max_num_reorder_pics = r.read_ue()?;
            r.read_ue()?;
        }
        Ok(Vps {
            id,
            ptl,
            max_dec_pic_buffering_minus1,
            max_num_reorder_pics,
        })
    }
}

/// The conformance window: how many samples to crop off a coded picture.
///
/// This is how 1920x1080 is coded with 64x64 coding tree blocks — the picture is
/// coded 1920x1088 and eight rows are cropped — and it is a first-class field
/// here rather than a caller's problem, because a caller that pads its own
/// planes has to guess what the encoder does with the padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConformanceWindow {
    /// `conf_win_left_offset`, in chroma units.
    pub left: u32,
    /// `conf_win_right_offset`, in chroma units.
    pub right: u32,
    /// `conf_win_top_offset`, in chroma units.
    pub top: u32,
    /// `conf_win_bottom_offset`, in chroma units.
    pub bottom: u32,
}

impl ConformanceWindow {
    /// True when nothing is cropped.
    pub fn is_empty(&self) -> bool {
        *self == ConformanceWindow::default()
    }
}

/// The sequence parameter set (7.3.2.2.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sps {
    /// `sps_video_parameter_set_id`.
    pub vps_id: u8,
    /// `sps_seq_parameter_set_id`.
    pub id: u32,
    /// 0 = monochrome, 1 = 4:2:0, 2 = 4:2:2, 3 = 4:4:4.
    pub chroma_format_idc: u32,
    /// `separate_colour_plane_flag`.
    pub separate_colour_plane: bool,
    /// Coded width — a multiple of the minimum coding block size.
    pub pic_width_in_luma_samples: u32,
    /// Coded height.
    pub pic_height_in_luma_samples: u32,
    /// What to crop off the coded picture for display.
    pub conf_win: ConformanceWindow,
    /// `bit_depth_luma_minus8`.
    pub bit_depth_luma_minus8: u32,
    /// `bit_depth_chroma_minus8`.
    pub bit_depth_chroma_minus8: u32,
    /// `log2_max_pic_order_cnt_lsb_minus4`.
    pub log2_max_poc_lsb_minus4: u32,
    /// `sps_max_dec_pic_buffering_minus1[HighestTid]`.
    pub max_dec_pic_buffering_minus1: u32,
    /// `sps_max_num_reorder_pics[HighestTid]`.
    pub max_num_reorder_pics: u32,
    /// `log2_min_luma_coding_block_size_minus3`.
    pub log2_min_cb_size_minus3: u32,
    /// `log2_diff_max_min_luma_coding_block_size`.
    pub log2_diff_max_min_cb_size: u32,
    /// `log2_min_luma_transform_block_size_minus2`.
    pub log2_min_tb_size_minus2: u32,
    /// `log2_diff_max_min_luma_transform_block_size`.
    pub log2_diff_max_min_tb_size: u32,
    /// `max_transform_hierarchy_depth_inter`.
    pub max_transform_hierarchy_depth_inter: u32,
    /// `max_transform_hierarchy_depth_intra`.
    pub max_transform_hierarchy_depth_intra: u32,
    /// `scaling_list_enabled_flag`; this family never writes scaling lists.
    pub scaling_list_enabled: bool,
    /// `amp_enabled_flag`.
    pub amp_enabled: bool,
    /// `sample_adaptive_offset_enabled_flag`.
    pub sao_enabled: bool,
    /// `pcm_enabled_flag`.
    pub pcm_enabled: bool,
    /// `num_short_term_ref_pic_sets`.
    pub num_short_term_ref_pic_sets: u32,
    /// `long_term_ref_pics_present_flag`.
    pub long_term_ref_pics_present: bool,
    /// `num_long_term_ref_pics_sps`.
    pub num_long_term_ref_pics_sps: u32,
    /// `sps_temporal_mvp_enabled_flag`.
    pub temporal_mvp_enabled: bool,
    /// `strong_intra_smoothing_enabled_flag`.
    pub strong_intra_smoothing: bool,
    /// Profile, tier, level.
    pub ptl: ProfileTierLevel,
    /// VUI, when present.
    pub vui: Option<VuiParameters>,
}

impl Sps {
    /// Coding tree block size in luma samples (`CtbSizeY`).
    pub fn ctb_size(&self) -> u32 {
        1 << (self.log2_min_cb_size_minus3 + 3 + self.log2_diff_max_min_cb_size)
    }

    /// `PicWidthInCtbsY`.
    pub fn pic_width_in_ctbs(&self) -> u32 {
        self.pic_width_in_luma_samples.div_ceil(self.ctb_size())
    }

    /// `PicHeightInCtbsY`.
    pub fn pic_height_in_ctbs(&self) -> u32 {
        self.pic_height_in_luma_samples.div_ceil(self.ctb_size())
    }

    /// `SubWidthC`, `SubHeightC` for the chroma format.
    pub fn chroma_subsampling(&self) -> (u32, u32) {
        match self.chroma_format_idc {
            1 => (2, 2),
            2 => (2, 1),
            _ => (1, 1),
        }
    }

    /// Displayed size after the conformance window is applied.
    pub fn display_size(&self) -> (u32, u32) {
        let (sub_w, sub_h) = self.chroma_subsampling();
        (
            self.pic_width_in_luma_samples - sub_w * (self.conf_win.left + self.conf_win.right),
            self.pic_height_in_luma_samples - sub_h * (self.conf_win.top + self.conf_win.bottom),
        )
    }

    /// Number of bits `slice_segment_address` occupies in a slice header:
    /// `Ceil( Log2( PicSizeInCtbsY ) )`.
    pub fn slice_address_bits(&self) -> u32 {
        ceil_log2(self.pic_width_in_ctbs() * self.pic_height_in_ctbs())
    }

    /// The RBSP, trailing bits included.
    pub fn to_rbsp(&self) -> Vec<u8> {
        let mut w = BitWriter::with_capacity(64);
        w.write_bits(u32::from(self.vps_id), 4);
        w.write_bits(0, 3); // sps_max_sub_layers_minus1
        w.write_bit(true); // sps_temporal_id_nesting_flag
        self.ptl.write(&mut w);
        w.write_ue(self.id);
        w.write_ue(self.chroma_format_idc);
        if self.chroma_format_idc == 3 {
            w.write_bit(self.separate_colour_plane);
        }
        w.write_ue(self.pic_width_in_luma_samples);
        w.write_ue(self.pic_height_in_luma_samples);
        if self.conf_win.is_empty() {
            w.write_bit(false);
        } else {
            w.write_bit(true);
            w.write_ue(self.conf_win.left);
            w.write_ue(self.conf_win.right);
            w.write_ue(self.conf_win.top);
            w.write_ue(self.conf_win.bottom);
        }
        w.write_ue(self.bit_depth_luma_minus8);
        w.write_ue(self.bit_depth_chroma_minus8);
        w.write_ue(self.log2_max_poc_lsb_minus4);
        w.write_bit(true); // sps_sub_layer_ordering_info_present_flag
        w.write_ue(self.max_dec_pic_buffering_minus1);
        w.write_ue(self.max_num_reorder_pics);
        w.write_ue(0); // sps_max_latency_increase_plus1
        w.write_ue(self.log2_min_cb_size_minus3);
        w.write_ue(self.log2_diff_max_min_cb_size);
        w.write_ue(self.log2_min_tb_size_minus2);
        w.write_ue(self.log2_diff_max_min_tb_size);
        w.write_ue(self.max_transform_hierarchy_depth_inter);
        w.write_ue(self.max_transform_hierarchy_depth_intra);
        w.write_bit(self.scaling_list_enabled);
        if self.scaling_list_enabled {
            w.write_bit(false); // sps_scaling_list_data_present_flag
        }
        w.write_bit(self.amp_enabled);
        w.write_bit(self.sao_enabled);
        w.write_bit(self.pcm_enabled);
        w.write_ue(self.num_short_term_ref_pic_sets);
        w.write_bit(self.long_term_ref_pics_present);
        if self.long_term_ref_pics_present {
            w.write_ue(self.num_long_term_ref_pics_sps);
        }
        w.write_bit(self.temporal_mvp_enabled);
        w.write_bit(self.strong_intra_smoothing);
        match &self.vui {
            Some(vui) => {
                w.write_bit(true);
                vui.write(&mut w);
            }
            None => w.write_bit(false),
        }
        w.write_bit(false); // sps_extension_present_flag
        rbsp_trailing_bits(&mut w);
        w.into_bytes()
    }

    /// Parse an SPS RBSP.
    ///
    /// Short-term reference picture sets are walked but not kept: a decoder
    /// resolves them from the slice header, and this family's own streams have
    /// none. A stream with scaling lists is refused rather than mis-decoded.
    pub fn parse(rbsp: &[u8]) -> Result<Sps> {
        let mut r = BitReader::new(rbsp);
        let vps_id = r.read_bits(4)? as u8;
        let max_sub_layers_minus1 = r.read_bits(3)?;
        r.read_bit()?; // sps_temporal_id_nesting_flag
        let ptl = ProfileTierLevel::parse(&mut r, max_sub_layers_minus1)?;
        let id = r.read_ue()?;
        let chroma_format_idc = r.read_ue()?;
        if chroma_format_idc > 3 {
            return Err(Error::corrupt(format!(
                "HEVC SPS: chroma_format_idc = {chroma_format_idc}"
            )));
        }
        let separate_colour_plane = if chroma_format_idc == 3 {
            r.read_bit()?
        } else {
            false
        };
        let pic_width_in_luma_samples = r.read_ue()?;
        let pic_height_in_luma_samples = r.read_ue()?;
        // A dimension is a bit width elsewhere (slice_segment_address is
        // Ceil(Log2(PicSizeInCtbsY)) bits), so an absurd one is refused here
        // rather than turned into an absurd shift there.
        if pic_width_in_luma_samples == 0
            || pic_height_in_luma_samples == 0
            || pic_width_in_luma_samples > 65_536
            || pic_height_in_luma_samples > 65_536
        {
            return Err(Error::corrupt(format!(
                "HEVC SPS: picture size {pic_width_in_luma_samples}x{pic_height_in_luma_samples}"
            )));
        }
        let mut conf_win = ConformanceWindow::default();
        if r.read_bit()? {
            conf_win = ConformanceWindow {
                left: r.read_ue()?,
                right: r.read_ue()?,
                top: r.read_ue()?,
                bottom: r.read_ue()?,
            };
        }
        let bit_depth_luma_minus8 = r.read_ue()?;
        let bit_depth_chroma_minus8 = r.read_ue()?;
        if bit_depth_luma_minus8 > 8 || bit_depth_chroma_minus8 > 8 {
            return Err(Error::corrupt("HEVC SPS: bit depth out of range"));
        }
        let log2_max_poc_lsb_minus4 = r.read_ue()?;
        if log2_max_poc_lsb_minus4 > 12 {
            return Err(Error::corrupt(format!(
                "HEVC SPS: log2_max_pic_order_cnt_lsb_minus4 = {log2_max_poc_lsb_minus4}"
            )));
        }
        let ordering_info = r.read_bit()?;
        let first = if ordering_info {
            0
        } else {
            max_sub_layers_minus1
        };
        let mut max_dec_pic_buffering_minus1 = 0;
        let mut max_num_reorder_pics = 0;
        for _ in first..=max_sub_layers_minus1 {
            max_dec_pic_buffering_minus1 = r.read_ue()?;
            max_num_reorder_pics = r.read_ue()?;
            r.read_ue()?;
        }
        let log2_min_cb_size_minus3 = r.read_ue()?;
        let log2_diff_max_min_cb_size = r.read_ue()?;
        let log2_min_tb_size_minus2 = r.read_ue()?;
        let log2_diff_max_min_tb_size = r.read_ue()?;
        if log2_min_cb_size_minus3 + log2_diff_max_min_cb_size > 3 {
            return Err(Error::corrupt("HEVC SPS: CTB larger than 64x64"));
        }
        let max_transform_hierarchy_depth_inter = r.read_ue()?;
        let max_transform_hierarchy_depth_intra = r.read_ue()?;
        let scaling_list_enabled = r.read_bit()?;
        if scaling_list_enabled && r.read_bit()? {
            return Err(Error::unsupported(
                "HEVC SPS scaling lists",
                "this family codes with the flat 16 matrix only",
            ));
        }
        let amp_enabled = r.read_bit()?;
        let sao_enabled = r.read_bit()?;
        let pcm_enabled = r.read_bit()?;
        if pcm_enabled {
            r.read_bits(4)?; // pcm_sample_bit_depth_luma_minus1
            r.read_bits(4)?; // pcm_sample_bit_depth_chroma_minus1
            r.read_ue()?; // log2_min_pcm_luma_coding_block_size_minus3
            r.read_ue()?; // log2_diff_max_min_pcm_luma_coding_block_size
            r.read_bit()?; // pcm_loop_filter_disabled_flag
        }
        let num_short_term_ref_pic_sets = r.read_ue()?;
        if num_short_term_ref_pic_sets > 64 {
            return Err(Error::corrupt("HEVC SPS: num_short_term_ref_pic_sets > 64"));
        }
        let mut sets = Vec::new();
        for i in 0..num_short_term_ref_pic_sets {
            let set = ShortTermRefPicSet::parse(&mut r, i, &sets)?;
            sets.push(set);
        }
        let long_term_ref_pics_present = r.read_bit()?;
        let mut num_long_term_ref_pics_sps = 0;
        if long_term_ref_pics_present {
            num_long_term_ref_pics_sps = r.read_ue()?;
            if num_long_term_ref_pics_sps > 32 {
                return Err(Error::corrupt(format!(
                    "HEVC SPS: num_long_term_ref_pics_sps = {num_long_term_ref_pics_sps}"
                )));
            }
            for _ in 0..num_long_term_ref_pics_sps {
                r.read_bits(log2_max_poc_lsb_minus4 + 4)?;
                r.read_bit()?;
            }
        }
        let temporal_mvp_enabled = r.read_bit()?;
        let strong_intra_smoothing = r.read_bit()?;
        let vui = if r.read_bit()? {
            Some(VuiParameters::parse(&mut r, max_sub_layers_minus1)?)
        } else {
            None
        };
        Ok(Sps {
            vps_id,
            id,
            chroma_format_idc,
            separate_colour_plane,
            pic_width_in_luma_samples,
            pic_height_in_luma_samples,
            conf_win,
            bit_depth_luma_minus8,
            bit_depth_chroma_minus8,
            log2_max_poc_lsb_minus4,
            max_dec_pic_buffering_minus1,
            max_num_reorder_pics,
            log2_min_cb_size_minus3,
            log2_diff_max_min_cb_size,
            log2_min_tb_size_minus2,
            log2_diff_max_min_tb_size,
            max_transform_hierarchy_depth_inter,
            max_transform_hierarchy_depth_intra,
            scaling_list_enabled,
            amp_enabled,
            sao_enabled,
            pcm_enabled,
            num_short_term_ref_pic_sets,
            long_term_ref_pics_present,
            num_long_term_ref_pics_sps,
            temporal_mvp_enabled,
            strong_intra_smoothing,
            ptl,
            vui,
        })
    }
}

/// One `st_ref_pic_set()` (7.3.7), kept only as far as re-parsing needs.
///
/// The delta values themselves are a decoder's reference-list problem; what a
/// *parser* has to get right is the number of pictures in each direction,
/// because the next set's syntax depends on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShortTermRefPicSet {
    /// `NumNegativePics`.
    pub num_negative: u32,
    /// `NumPositivePics`.
    pub num_positive: u32,
    /// How many of them have `used_by_curr_pic` set — the count that feeds
    /// `NumPicTotalCurr`, which sizes the reference list modification syntax.
    pub num_used_by_curr: u32,
}

impl ShortTermRefPicSet {
    /// Parse one set; `prev` holds the sets already parsed for inter-set prediction.
    pub fn parse(
        r: &mut BitReader,
        idx: u32,
        prev: &[ShortTermRefPicSet],
    ) -> Result<ShortTermRefPicSet> {
        let inter_ref_pic_set_prediction = if idx != 0 { r.read_bit()? } else { false };
        if inter_ref_pic_set_prediction {
            let delta_idx_minus1 = if idx == prev.len() as u32 {
                r.read_ue()?
            } else {
                0
            };
            let ref_idx = idx
                .checked_sub(delta_idx_minus1 + 1)
                .ok_or_else(|| Error::corrupt("HEVC st_ref_pic_set: delta_idx out of range"))?;
            let reference = *prev
                .get(ref_idx as usize)
                .ok_or_else(|| Error::corrupt("HEVC st_ref_pic_set: unknown reference set"))?;
            r.read_bit()?; // delta_rps_sign
            r.read_ue()?; // abs_delta_rps_minus1
            let num_delta_pocs = reference.num_negative + reference.num_positive;
            let mut kept = 0;
            let mut used_by_curr = 0;
            for _ in 0..=num_delta_pocs {
                let used = r.read_bit()?;
                if used {
                    kept += 1;
                    used_by_curr += 1;
                } else if r.read_bit()? {
                    kept += 1;
                }
            }
            // Without reconstructing the POCs the split between negative and
            // positive is not knowable; the total is, and that is what the
            // syntax of any following set depends on.
            Ok(ShortTermRefPicSet {
                num_negative: kept,
                num_positive: 0,
                num_used_by_curr: used_by_curr,
            })
        } else {
            let num_negative = r.read_ue()?;
            let num_positive = r.read_ue()?;
            if num_negative > 16 || num_positive > 16 {
                return Err(Error::corrupt("HEVC st_ref_pic_set: too many pictures"));
            }
            let mut num_used_by_curr = 0;
            for _ in 0..num_negative + num_positive {
                r.read_ue()?; // delta_poc_sX_minus1
                if r.read_bit()? {
                    num_used_by_curr += 1; // used_by_curr_pic_sX_flag
                }
            }
            Ok(ShortTermRefPicSet {
                num_negative,
                num_positive,
                num_used_by_curr,
            })
        }
    }
}

/// The picture parameter set (7.3.2.3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pps {
    /// `pps_pic_parameter_set_id`.
    pub id: u32,
    /// `pps_seq_parameter_set_id`.
    pub sps_id: u32,
    /// `dependent_slice_segments_enabled_flag`.
    pub dependent_slice_segments_enabled: bool,
    /// `output_flag_present_flag`.
    pub output_flag_present: bool,
    /// `num_extra_slice_header_bits`.
    pub num_extra_slice_header_bits: u32,
    /// `sign_data_hiding_enabled_flag`.
    pub sign_data_hiding_enabled: bool,
    /// `cabac_init_present_flag`.
    pub cabac_init_present: bool,
    /// `num_ref_idx_l0_default_active_minus1`.
    pub num_ref_idx_l0_default_active_minus1: u32,
    /// `num_ref_idx_l1_default_active_minus1`.
    pub num_ref_idx_l1_default_active_minus1: u32,
    /// `init_qp_minus26`.
    pub init_qp_minus26: i32,
    /// `constrained_intra_pred_flag`.
    pub constrained_intra_pred: bool,
    /// `transform_skip_enabled_flag`.
    pub transform_skip_enabled: bool,
    /// `cu_qp_delta_enabled_flag`.
    pub cu_qp_delta_enabled: bool,
    /// `diff_cu_qp_delta_depth`.
    pub diff_cu_qp_delta_depth: u32,
    /// `pps_cb_qp_offset`.
    pub cb_qp_offset: i32,
    /// `pps_cr_qp_offset`.
    pub cr_qp_offset: i32,
    /// `pps_slice_chroma_qp_offsets_present_flag`.
    pub slice_chroma_qp_offsets_present: bool,
    /// `weighted_pred_flag`.
    pub weighted_pred: bool,
    /// `weighted_bipred_flag`.
    pub weighted_bipred: bool,
    /// `transquant_bypass_enabled_flag`.
    pub transquant_bypass_enabled: bool,
    /// `tiles_enabled_flag`.
    pub tiles_enabled: bool,
    /// `entropy_coding_sync_enabled_flag` — wavefront parallel processing.
    pub entropy_coding_sync_enabled: bool,
    /// `num_tile_columns_minus1`.
    pub num_tile_columns_minus1: u32,
    /// `num_tile_rows_minus1`.
    pub num_tile_rows_minus1: u32,
    /// `uniform_spacing_flag`.
    pub uniform_spacing: bool,
    /// `loop_filter_across_tiles_enabled_flag`.
    pub loop_filter_across_tiles_enabled: bool,
    /// `pps_loop_filter_across_slices_enabled_flag`.
    pub loop_filter_across_slices_enabled: bool,
    /// `deblocking_filter_control_present_flag`.
    pub deblocking_filter_control_present: bool,
    /// `deblocking_filter_override_enabled_flag`.
    pub deblocking_filter_override_enabled: bool,
    /// `pps_deblocking_filter_disabled_flag`.
    pub deblocking_filter_disabled: bool,
    /// `pps_beta_offset_div2`.
    pub beta_offset_div2: i32,
    /// `pps_tc_offset_div2`.
    pub tc_offset_div2: i32,
    /// `lists_modification_present_flag`.
    pub lists_modification_present: bool,
    /// `log2_parallel_merge_level_minus2`.
    pub log2_parallel_merge_level_minus2: u32,
    /// `slice_segment_header_extension_present_flag`.
    pub slice_segment_header_extension_present: bool,
}

impl Default for Pps {
    fn default() -> Self {
        Pps {
            id: 0,
            sps_id: 0,
            dependent_slice_segments_enabled: false,
            output_flag_present: false,
            num_extra_slice_header_bits: 0,
            sign_data_hiding_enabled: false,
            cabac_init_present: false,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            init_qp_minus26: 0,
            constrained_intra_pred: false,
            transform_skip_enabled: false,
            cu_qp_delta_enabled: false,
            diff_cu_qp_delta_depth: 0,
            cb_qp_offset: 0,
            cr_qp_offset: 0,
            slice_chroma_qp_offsets_present: false,
            weighted_pred: false,
            weighted_bipred: false,
            transquant_bypass_enabled: false,
            tiles_enabled: false,
            entropy_coding_sync_enabled: false,
            num_tile_columns_minus1: 0,
            num_tile_rows_minus1: 0,
            uniform_spacing: true,
            loop_filter_across_tiles_enabled: true,
            loop_filter_across_slices_enabled: true,
            deblocking_filter_control_present: false,
            deblocking_filter_override_enabled: false,
            deblocking_filter_disabled: false,
            beta_offset_div2: 0,
            tc_offset_div2: 0,
            lists_modification_present: false,
            log2_parallel_merge_level_minus2: 0,
            slice_segment_header_extension_present: false,
        }
    }
}

impl Pps {
    /// The RBSP, trailing bits included.
    pub fn to_rbsp(&self) -> Vec<u8> {
        let mut w = BitWriter::with_capacity(32);
        w.write_ue(self.id);
        w.write_ue(self.sps_id);
        w.write_bit(self.dependent_slice_segments_enabled);
        w.write_bit(self.output_flag_present);
        w.write_bits(self.num_extra_slice_header_bits, 3);
        w.write_bit(self.sign_data_hiding_enabled);
        w.write_bit(self.cabac_init_present);
        w.write_ue(self.num_ref_idx_l0_default_active_minus1);
        w.write_ue(self.num_ref_idx_l1_default_active_minus1);
        w.write_se(self.init_qp_minus26);
        w.write_bit(self.constrained_intra_pred);
        w.write_bit(self.transform_skip_enabled);
        w.write_bit(self.cu_qp_delta_enabled);
        if self.cu_qp_delta_enabled {
            w.write_ue(self.diff_cu_qp_delta_depth);
        }
        w.write_se(self.cb_qp_offset);
        w.write_se(self.cr_qp_offset);
        w.write_bit(self.slice_chroma_qp_offsets_present);
        w.write_bit(self.weighted_pred);
        w.write_bit(self.weighted_bipred);
        w.write_bit(self.transquant_bypass_enabled);
        w.write_bit(self.tiles_enabled);
        w.write_bit(self.entropy_coding_sync_enabled);
        if self.tiles_enabled {
            w.write_ue(self.num_tile_columns_minus1);
            w.write_ue(self.num_tile_rows_minus1);
            w.write_bit(self.uniform_spacing);
            w.write_bit(self.loop_filter_across_tiles_enabled);
        }
        w.write_bit(self.loop_filter_across_slices_enabled);
        w.write_bit(self.deblocking_filter_control_present);
        if self.deblocking_filter_control_present {
            w.write_bit(self.deblocking_filter_override_enabled);
            w.write_bit(self.deblocking_filter_disabled);
            if !self.deblocking_filter_disabled {
                w.write_se(self.beta_offset_div2);
                w.write_se(self.tc_offset_div2);
            }
        }
        w.write_bit(false); // pps_scaling_list_data_present_flag
        w.write_bit(self.lists_modification_present);
        w.write_ue(self.log2_parallel_merge_level_minus2);
        w.write_bit(self.slice_segment_header_extension_present);
        w.write_bit(false); // pps_extension_present_flag
        rbsp_trailing_bits(&mut w);
        w.into_bytes()
    }

    /// Parse a PPS RBSP.
    pub fn parse(rbsp: &[u8]) -> Result<Pps> {
        let mut r = BitReader::new(rbsp);
        let mut pps = Pps {
            id: r.read_ue()?,
            sps_id: r.read_ue()?,
            ..Pps::default()
        };
        pps.dependent_slice_segments_enabled = r.read_bit()?;
        pps.output_flag_present = r.read_bit()?;
        pps.num_extra_slice_header_bits = r.read_bits(3)?;
        pps.sign_data_hiding_enabled = r.read_bit()?;
        pps.cabac_init_present = r.read_bit()?;
        pps.num_ref_idx_l0_default_active_minus1 = r.read_ue()?;
        pps.num_ref_idx_l1_default_active_minus1 = r.read_ue()?;
        pps.init_qp_minus26 = r.read_se()?;
        pps.constrained_intra_pred = r.read_bit()?;
        pps.transform_skip_enabled = r.read_bit()?;
        pps.cu_qp_delta_enabled = r.read_bit()?;
        if pps.cu_qp_delta_enabled {
            pps.diff_cu_qp_delta_depth = r.read_ue()?;
        }
        pps.cb_qp_offset = r.read_se()?;
        pps.cr_qp_offset = r.read_se()?;
        pps.slice_chroma_qp_offsets_present = r.read_bit()?;
        pps.weighted_pred = r.read_bit()?;
        pps.weighted_bipred = r.read_bit()?;
        pps.transquant_bypass_enabled = r.read_bit()?;
        pps.tiles_enabled = r.read_bit()?;
        pps.entropy_coding_sync_enabled = r.read_bit()?;
        if pps.tiles_enabled {
            pps.num_tile_columns_minus1 = r.read_ue()?;
            pps.num_tile_rows_minus1 = r.read_ue()?;
            pps.uniform_spacing = r.read_bit()?;
            if !pps.uniform_spacing {
                for _ in 0..pps.num_tile_columns_minus1 {
                    r.read_ue()?;
                }
                for _ in 0..pps.num_tile_rows_minus1 {
                    r.read_ue()?;
                }
            }
            pps.loop_filter_across_tiles_enabled = r.read_bit()?;
        }
        pps.loop_filter_across_slices_enabled = r.read_bit()?;
        pps.deblocking_filter_control_present = r.read_bit()?;
        if pps.deblocking_filter_control_present {
            pps.deblocking_filter_override_enabled = r.read_bit()?;
            pps.deblocking_filter_disabled = r.read_bit()?;
            if !pps.deblocking_filter_disabled {
                pps.beta_offset_div2 = r.read_se()?;
                pps.tc_offset_div2 = r.read_se()?;
            }
        }
        if r.read_bit()? {
            return Err(Error::unsupported(
                "HEVC PPS scaling lists",
                "this family codes with the flat 16 matrix only",
            ));
        }
        pps.lists_modification_present = r.read_bit()?;
        pps.log2_parallel_merge_level_minus2 = r.read_ue()?;
        pps.slice_segment_header_extension_present = r.read_bit()?;
        Ok(pps)
    }
}

/// `rbsp_trailing_bits()`: a one bit then zeros to the byte boundary.
pub fn rbsp_trailing_bits(w: &mut BitWriter) {
    w.write_bit(true);
    w.align_to_byte();
}

/// `Ceil( Log2( v ) )`, the width of a `u(v)` field counting `v` values.
pub fn ceil_log2(v: u32) -> u32 {
    let mut bits = 0;
    while (1u64 << bits) < u64::from(v) {
        bits += 1;
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vui::{ColourDescription, VideoSignalType};

    fn sample_sps() -> Sps {
        Sps {
            vps_id: 0,
            id: 0,
            chroma_format_idc: 1,
            separate_colour_plane: false,
            pic_width_in_luma_samples: 1920,
            pic_height_in_luma_samples: 1088,
            conf_win: ConformanceWindow {
                left: 0,
                right: 0,
                top: 0,
                bottom: 4,
            },
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            log2_max_poc_lsb_minus4: 4,
            max_dec_pic_buffering_minus1: 0,
            max_num_reorder_pics: 0,
            log2_min_cb_size_minus3: 0,
            log2_diff_max_min_cb_size: 3,
            log2_min_tb_size_minus2: 0,
            log2_diff_max_min_tb_size: 3,
            max_transform_hierarchy_depth_inter: 0,
            max_transform_hierarchy_depth_intra: 0,
            scaling_list_enabled: false,
            amp_enabled: false,
            sao_enabled: false,
            pcm_enabled: false,
            num_short_term_ref_pic_sets: 0,
            long_term_ref_pics_present: false,
            num_long_term_ref_pics_sps: 0,
            temporal_mvp_enabled: false,
            strong_intra_smoothing: true,
            ptl: ProfileTierLevel::main(120),
            vui: Some(VuiParameters {
                sample_aspect_ratio: Some((1, 1)),
                video_signal_type: Some(VideoSignalType {
                    video_format: 5,
                    video_full_range_flag: false,
                    colour_description: Some(ColourDescription {
                        colour_primaries: 1,
                        transfer_characteristics: 1,
                        matrix_coeffs: 1,
                    }),
                }),
                timing: Some((1, 30)),
            }),
        }
    }

    #[test]
    fn parameter_sets_round_trip() {
        let vps = Vps {
            id: 0,
            ptl: ProfileTierLevel::main(120),
            max_dec_pic_buffering_minus1: 0,
            max_num_reorder_pics: 0,
        };
        assert_eq!(Vps::parse(&vps.to_rbsp()).unwrap(), vps);

        let sps = sample_sps();
        let parsed = Sps::parse(&sps.to_rbsp()).unwrap();
        assert_eq!(parsed, sps);
        // 1080 is cropped back out of the 1088 coded rows.
        assert_eq!(parsed.display_size(), (1920, 1080));
        assert_eq!(parsed.ctb_size(), 64);
        assert_eq!(parsed.pic_width_in_ctbs(), 30);
        assert_eq!(parsed.pic_height_in_ctbs(), 17);

        let pps = Pps {
            entropy_coding_sync_enabled: true,
            init_qp_minus26: 1,
            deblocking_filter_control_present: true,
            deblocking_filter_disabled: true,
            ..Pps::default()
        };
        assert_eq!(Pps::parse(&pps.to_rbsp()).unwrap(), pps);
    }

    #[test]
    fn levels_round_up_with_size_and_rate() {
        assert_eq!(ProfileTierLevel::level_for(1920, 1080, 30.0), 120); // 4
        assert_eq!(ProfileTierLevel::level_for(1920, 1080, 60.0), 123); // 4.1
        assert_eq!(ProfileTierLevel::level_for(3840, 2160, 30.0), 150); // 5
        assert_eq!(ProfileTierLevel::level_for(640, 480, 30.0), 90); // 3
    }

    #[test]
    fn ceil_log2_counts_values() {
        assert_eq!(ceil_log2(1), 0);
        assert_eq!(ceil_log2(2), 1);
        assert_eq!(ceil_log2(510), 9);
        assert_eq!(ceil_log2(512), 9);
        assert_eq!(ceil_log2(513), 10);
    }
}
