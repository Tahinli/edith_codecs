//! Parameter sets: VPS, SPS and PPS, written and parsed.
//!
//! The field sets here are sized by what a *stateless hardware decoder* asks
//! for, not by what this family's own encoder writes: everything in
//! `VAPictureParameterBufferHEVC` is either a field on [`Sps`]/[`Pps`] or
//! derivable from one by a named method. Anything the parser walks past but does
//! not model (scaling lists, HRD, the multilayer and 3D extensions) is a thing
//! no consumer in this family reads, and each such skip says so where it happens.

use crate::scaling::ScalingList;
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

    /// Main 10 profile at `level_idc`: 10-bit 4:2:0 with the same flags as
    /// [`ProfileTierLevel::main`]. Main 10 decoders also decode Main, so both
    /// compatibility bits are set.
    pub fn main10(level_idc: u8) -> ProfileTierLevel {
        ProfileTierLevel {
            profile_idc: 2,
            profile_compatibility: (1 << 1) | (1 << 2),
            ..ProfileTierLevel::main(level_idc)
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
        check_range(
            "HEVC VPS",
            "vps_max_sub_layers_minus1",
            i64::from(max_sub_layers_minus1),
            0,
            MAX_SUB_LAYERS_MINUS1,
        )?;
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
            check_range(
                "HEVC VPS",
                "vps_max_dec_pic_buffering_minus1",
                i64::from(max_dec_pic_buffering_minus1),
                0,
                MAX_DPB_PICTURES_MINUS1,
            )?;
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
    /// `scaling_list_enabled_flag`.
    pub scaling_list_enabled: bool,
    /// The lists `sps_scaling_list_data_present_flag` sends. `None` with scaling
    /// lists enabled means the defaults of Tables 7-5 and 7-6. Meaningful only
    /// when `scaling_list_enabled`.
    pub scaling_list: Option<ScalingList>,
    /// `amp_enabled_flag`.
    pub amp_enabled: bool,
    /// `sample_adaptive_offset_enabled_flag`.
    pub sao_enabled: bool,
    /// `pcm_enabled_flag`.
    pub pcm_enabled: bool,
    /// PCM sample depths and block sizes, present when `pcm_enabled`:
    /// `(pcm_sample_bit_depth_luma_minus1, pcm_sample_bit_depth_chroma_minus1,
    /// log2_min_pcm_luma_coding_block_size_minus3,
    /// log2_diff_max_min_pcm_luma_coding_block_size, pcm_loop_filter_disabled_flag)`.
    pub pcm: Option<(u32, u32, u32, u32, bool)>,
    /// `num_short_term_ref_pic_sets`.
    pub num_short_term_ref_pic_sets: u32,
    /// The sets themselves, in bitstream order: a slice header may name one by
    /// index instead of carrying it, and a decoder then needs it from here.
    pub short_term_ref_pic_sets: Vec<ShortTermRefPicSet>,
    /// `long_term_ref_pics_present_flag`.
    pub long_term_ref_pics_present: bool,
    /// `num_long_term_ref_pics_sps`.
    pub num_long_term_ref_pics_sps: u32,
    /// `(lt_ref_pic_poc_lsb_sps, used_by_curr_pic_lt_sps_flag)` per SPS long
    /// term picture, which a slice header names by index.
    pub long_term_ref_pics_sps: Vec<(u32, bool)>,
    /// `sps_temporal_mvp_enabled_flag`.
    pub temporal_mvp_enabled: bool,
    /// `strong_intra_smoothing_enabled_flag`.
    pub strong_intra_smoothing: bool,
    /// Profile, tier, level.
    pub ptl: ProfileTierLevel,
    /// VUI, when present.
    pub vui: Option<VuiParameters>,
    /// `sps_max_latency_increase_plus1[HighestTid]`; 0 when there is no latency limit.
    pub max_latency_increase_plus1: u32,
    /// The extension flags of 7.3.2.2.1, one byte: bit 7 range, 6 multilayer, 5 3D, 4 SCC,
    /// bits 0..3 `sps_extension_4bits`. Zero when the SPS carries no extension.
    pub extension_flags: u8,
    /// The `sps_range_extension()` flags (7.3.2.2.2); all false when absent.
    pub range: SpsRangeExt,
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
            match &self.scaling_list {
                Some(list) => {
                    w.write_bit(true); // sps_scaling_list_data_present_flag
                    list.write(&mut w);
                }
                None => w.write_bit(false),
            }
        }
        w.write_bit(self.amp_enabled);
        w.write_bit(self.sao_enabled);
        w.write_bit(self.pcm_enabled);
        if let Some((luma, chroma, log2_min, log2_diff, filter_off)) = self.pcm {
            w.write_bits(luma, 4);
            w.write_bits(chroma, 4);
            w.write_ue(log2_min);
            w.write_ue(log2_diff);
            w.write_bit(filter_off);
        }
        w.write_ue(self.num_short_term_ref_pic_sets);
        for (i, set) in self.short_term_ref_pic_sets.iter().enumerate() {
            set.write(&mut w, i as u32);
        }
        w.write_bit(self.long_term_ref_pics_present);
        if self.long_term_ref_pics_present {
            w.write_ue(self.num_long_term_ref_pics_sps);
            for &(poc_lsb_lt, used_by_curr) in &self.long_term_ref_pics_sps {
                w.write_bits(poc_lsb_lt, self.log2_max_poc_lsb_minus4 + 4);
                w.write_bit(used_by_curr);
            }
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
        write_extension_flags(&mut w, self.extension_flags);
        if self.extension_flags & 0x80 != 0 {
            let r = &self.range;
            for flag in [
                r.transform_skip_rotation_enabled,
                r.transform_skip_context_enabled,
                r.implicit_rdpcm_enabled,
                r.explicit_rdpcm_enabled,
                r.extended_precision_processing,
                r.intra_smoothing_disabled,
                r.high_precision_offsets_enabled,
                r.persistent_rice_adaptation_enabled,
                r.cabac_bypass_alignment_enabled,
            ] {
                w.write_bit(flag);
            }
        }
        rbsp_trailing_bits(&mut w);
        w.into_bytes()
    }

    /// Parse an SPS RBSP.
    ///
    /// Short-term reference picture sets are walked but not kept: a decoder
    /// resolves them from the slice header, and this family's own streams have
    /// none.
    pub fn parse(rbsp: &[u8]) -> Result<Sps> {
        let mut r = BitReader::new(rbsp);
        let vps_id = r.read_bits(4)? as u8;
        let max_sub_layers_minus1 = r.read_bits(3)?;
        check_range(
            "HEVC SPS",
            "sps_max_sub_layers_minus1",
            i64::from(max_sub_layers_minus1),
            0,
            MAX_SUB_LAYERS_MINUS1,
        )?;
        r.read_bit()?; // sps_temporal_id_nesting_flag
        let ptl = ProfileTierLevel::parse(&mut r, max_sub_layers_minus1)?;
        let id = r.read_ue()?;
        check_range("HEVC SPS", "sps_seq_parameter_set_id", i64::from(id), 0, 15)?;
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
        // The window is in chroma units (7-18 to 7-21) and must leave a picture.
        let (sub_w, sub_h) = match chroma_format_idc {
            1 => (2u64, 2u64),
            2 => (2, 1),
            _ => (1, 1),
        };
        let crop_w = sub_w * (u64::from(conf_win.left) + u64::from(conf_win.right));
        let crop_h = sub_h * (u64::from(conf_win.top) + u64::from(conf_win.bottom));
        if crop_w >= u64::from(pic_width_in_luma_samples)
            || crop_h >= u64::from(pic_height_in_luma_samples)
        {
            return Err(Error::corrupt(format!(
                "HEVC SPS: conformance window {conf_win:?} leaves no picture"
            )));
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
        let mut max_latency_increase_plus1 = 0;
        for _ in first..=max_sub_layers_minus1 {
            max_dec_pic_buffering_minus1 = r.read_ue()?;
            check_range(
                "HEVC SPS",
                "sps_max_dec_pic_buffering_minus1",
                i64::from(max_dec_pic_buffering_minus1),
                0,
                MAX_DPB_PICTURES_MINUS1,
            )?;
            max_num_reorder_pics = r.read_ue()?;
            max_latency_increase_plus1 = r.read_ue()?;
        }
        let log2_min_cb_size_minus3 = r.read_ue()?;
        let log2_diff_max_min_cb_size = r.read_ue()?;
        let log2_min_tb_size_minus2 = r.read_ue()?;
        let log2_diff_max_min_tb_size = r.read_ue()?;
        // Block sizes as log2 values, widened so that no sum below can wrap. The
        // CTB is 16 to 64 samples (7-10), the minimum TB is below the minimum CB,
        // and the largest TB is at most 32 samples and at most the CTB (7-11).
        let min_cb_log2 = i64::from(log2_min_cb_size_minus3) + 3;
        let ctb_log2 = min_cb_log2 + i64::from(log2_diff_max_min_cb_size);
        check_range("HEVC SPS", "CtbLog2SizeY", ctb_log2, 4, 6)?;
        let min_tb_log2 = i64::from(log2_min_tb_size_minus2) + 2;
        if min_tb_log2 >= min_cb_log2 {
            return Err(Error::corrupt(format!(
                "HEVC SPS: MinTbLog2SizeY = {min_tb_log2} not below MinCbLog2SizeY = {min_cb_log2}"
            )));
        }
        let max_tb_log2 = min_tb_log2 + i64::from(log2_diff_max_min_tb_size);
        check_range(
            "HEVC SPS",
            "MaxTbLog2SizeY",
            max_tb_log2,
            min_tb_log2,
            ctb_log2.min(5),
        )?;
        // 7.4.3.2.1: the coded size is a whole number of minimum coding blocks. A
        // coding block at the edge would otherwise run past the picture buffer.
        let min_cb = 1u32 << min_cb_log2;
        if pic_width_in_luma_samples % min_cb != 0 || pic_height_in_luma_samples % min_cb != 0 {
            return Err(Error::corrupt(format!(
                "HEVC SPS: picture size {pic_width_in_luma_samples}x{pic_height_in_luma_samples} is not a multiple of the minimum coding block {min_cb}"
            )));
        }
        let max_transform_hierarchy_depth_inter = r.read_ue()?;
        let max_transform_hierarchy_depth_intra = r.read_ue()?;
        // 7-12: at most CtbLog2SizeY - MinTbLog2SizeY, which is at most 4.
        let max_depth = ctb_log2 - min_tb_log2;
        check_range(
            "HEVC SPS",
            "max_transform_hierarchy_depth_inter",
            i64::from(max_transform_hierarchy_depth_inter),
            0,
            max_depth,
        )?;
        check_range(
            "HEVC SPS",
            "max_transform_hierarchy_depth_intra",
            i64::from(max_transform_hierarchy_depth_intra),
            0,
            max_depth,
        )?;
        let scaling_list_enabled = r.read_bit()?;
        let mut scaling_list = None;
        if scaling_list_enabled && r.read_bit()? {
            scaling_list = Some(ScalingList::parse(&mut r)?);
        }
        let amp_enabled = r.read_bit()?;
        let sao_enabled = r.read_bit()?;
        let pcm_enabled = r.read_bit()?;
        let pcm = if pcm_enabled {
            let luma = r.read_bits(4)?;
            let chroma = r.read_bits(4)?;
            // 7.4.3.2.1: PcmBitDepthY <= BitDepthY, and likewise for chroma.
            if luma >= bit_depth_luma_minus8 + 8 || chroma >= bit_depth_chroma_minus8 + 8 {
                return Err(Error::corrupt("HEVC SPS: PCM bit depth above the coded bit depth"));
            }
            Some((
                luma,
                chroma,
                r.read_ue()?,
                r.read_ue()?,
                r.read_bit()?,
            ))
        } else {
            None
        };
        let num_short_term_ref_pic_sets = r.read_ue()?;
        if num_short_term_ref_pic_sets > 64 {
            return Err(Error::corrupt("HEVC SPS: num_short_term_ref_pic_sets > 64"));
        }
        let mut sets = Vec::new();
        for i in 0..num_short_term_ref_pic_sets {
            let set = ShortTermRefPicSet::parse(&mut r, i, num_short_term_ref_pic_sets, &sets)?;
            sets.push(set);
        }
        let long_term_ref_pics_present = r.read_bit()?;
        let mut num_long_term_ref_pics_sps = 0;
        let mut long_term_ref_pics_sps = Vec::new();
        if long_term_ref_pics_present {
            num_long_term_ref_pics_sps = r.read_ue()?;
            if num_long_term_ref_pics_sps > 32 {
                return Err(Error::corrupt(format!(
                    "HEVC SPS: num_long_term_ref_pics_sps = {num_long_term_ref_pics_sps}"
                )));
            }
            for _ in 0..num_long_term_ref_pics_sps {
                let poc_lsb_lt = r.read_bits(log2_max_poc_lsb_minus4 + 4)?;
                let used_by_curr = r.read_bit()?;
                long_term_ref_pics_sps.push((poc_lsb_lt, used_by_curr));
            }
        }
        let temporal_mvp_enabled = r.read_bit()?;
        let strong_intra_smoothing = r.read_bit()?;
        let vui = if r.read_bit()? {
            Some(VuiParameters::parse(&mut r, max_sub_layers_minus1)?)
        } else {
            None
        };
        let extension_flags = parse_extension_flags(&mut r);
        let range = if extension_flags & 0x80 != 0 {
            SpsRangeExt::parse(&mut r)?
        } else {
            SpsRangeExt::default()
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
            scaling_list,
            amp_enabled,
            sao_enabled,
            pcm_enabled,
            pcm,
            num_short_term_ref_pic_sets,
            short_term_ref_pic_sets: sets,
            long_term_ref_pics_present,
            num_long_term_ref_pics_sps,
            long_term_ref_pics_sps,
            temporal_mvp_enabled,
            strong_intra_smoothing,
            ptl,
            vui,
            max_latency_increase_plus1,
            extension_flags,
            range,
        })
    }
}

/// Largest `st_ref_pic_set()` this crate carries in either direction; the spec
/// caps `NumNegativePics` and `NumPositivePics` at `sps_max_dec_pic_buffering`,
/// which itself cannot exceed 16.
pub const MAX_ST_REF_PICS: usize = 16;

/// One `st_ref_pic_set()` (7.3.7), with the POC deltas a decoder needs.
///
/// A *stateless hardware* decoder is the reason the deltas are here: the driver
/// is handed `RefPicSetStCurrBefore` / `StCurrAfter` as picture lists, so
/// something has to turn `delta_poc_sX_minus1` into actual reference pictures,
/// and that something is the caller of this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortTermRefPicSet {
    /// `NumNegativePics`.
    pub num_negative: u32,
    /// `NumPositivePics`.
    pub num_positive: u32,
    /// How many of them have `used_by_curr_pic` set — the count that feeds
    /// `NumPicTotalCurr`, which sizes the reference list modification syntax.
    pub num_used_by_curr: u32,
    /// `DeltaPocS0[i]`, negative, in decreasing order (7-59).
    pub delta_poc_s0: [i32; MAX_ST_REF_PICS],
    /// `UsedByCurrPicS0[i]`.
    pub used_s0: [bool; MAX_ST_REF_PICS],
    /// `DeltaPocS1[i]`, positive, in increasing order (7-60).
    pub delta_poc_s1: [i32; MAX_ST_REF_PICS],
    /// `UsedByCurrPicS1[i]`.
    pub used_s1: [bool; MAX_ST_REF_PICS],
}

impl Default for ShortTermRefPicSet {
    fn default() -> ShortTermRefPicSet {
        ShortTermRefPicSet {
            num_negative: 0,
            num_positive: 0,
            num_used_by_curr: 0,
            delta_poc_s0: [0; MAX_ST_REF_PICS],
            used_s0: [false; MAX_ST_REF_PICS],
            delta_poc_s1: [0; MAX_ST_REF_PICS],
            used_s1: [false; MAX_ST_REF_PICS],
        }
    }
}

impl ShortTermRefPicSet {
    /// Parse one set. `idx` is `stRpsIdx`; `num_sets` is `num_short_term_ref_pic_sets`
    /// (the slice header's set sits at `idx == num_sets`, and only there does 7.3.7
    /// carry `delta_idx_minus1`). `prev` holds the SPS sets already parsed for
    /// inter-set prediction.
    pub fn parse(
        r: &mut BitReader,
        idx: u32,
        num_sets: u32,
        prev: &[ShortTermRefPicSet],
    ) -> Result<ShortTermRefPicSet> {
        let inter_ref_pic_set_prediction = if idx != 0 { r.read_bit()? } else { false };
        if inter_ref_pic_set_prediction {
            let delta_idx_minus1 = if idx == num_sets {
                r.read_ue()?
            } else {
                0
            };
            // 7-61: RefRpsIdx = stRpsIdx - (delta_idx_minus1 + 1), so delta_idx_minus1 < stRpsIdx.
            if delta_idx_minus1 >= idx {
                return Err(Error::corrupt("HEVC st_ref_pic_set: delta_idx out of range"));
            }
            let ref_idx = idx - (delta_idx_minus1 + 1);
            let reference = *prev
                .get(ref_idx as usize)
                .ok_or_else(|| Error::corrupt("HEVC st_ref_pic_set: unknown reference set"))?;
            let delta_rps_sign = r.read_bit()?;
            let abs_delta_rps_minus1 = r.read_ue()?;
            check_range(
                "HEVC st_ref_pic_set",
                "abs_delta_rps_minus1",
                i64::from(abs_delta_rps_minus1),
                0,
                MAX_DELTA_POC_MINUS1,
            )?;
            let delta_rps = if delta_rps_sign {
                -((abs_delta_rps_minus1 + 1) as i32)
            } else {
                (abs_delta_rps_minus1 + 1) as i32
            };
            let num_delta_pocs = (reference.num_negative + reference.num_positive) as usize;
            if num_delta_pocs >= MAX_ST_REF_PICS * 2 {
                return Err(Error::corrupt(
                    "HEVC st_ref_pic_set: reference set too large",
                ));
            }
            let mut used_by_curr = [false; MAX_ST_REF_PICS * 2 + 1];
            let mut use_delta = [true; MAX_ST_REF_PICS * 2 + 1];
            for j in 0..=num_delta_pocs {
                used_by_curr[j] = r.read_bit()?;
                if !used_by_curr[j] {
                    use_delta[j] = r.read_bit()?;
                }
            }
            Ok(reference.predict(delta_rps, &used_by_curr, &use_delta))
        } else {
            let num_negative = r.read_ue()?;
            let num_positive = r.read_ue()?;
            if num_negative as usize > MAX_ST_REF_PICS || num_positive as usize > MAX_ST_REF_PICS {
                return Err(Error::corrupt("HEVC st_ref_pic_set: too many pictures"));
            }
            let mut set = ShortTermRefPicSet {
                num_negative,
                num_positive,
                ..ShortTermRefPicSet::default()
            };
            let mut poc = 0i32;
            for i in 0..num_negative as usize {
                let delta = read_delta_poc(r)?;
                poc -= delta;
                set.delta_poc_s0[i] = poc;
                set.used_s0[i] = r.read_bit()?;
                if set.used_s0[i] {
                    set.num_used_by_curr += 1;
                }
            }
            let mut poc = 0i32;
            for i in 0..num_positive as usize {
                let delta = read_delta_poc(r)?;
                poc += delta;
                set.delta_poc_s1[i] = poc;
                set.used_s1[i] = r.read_bit()?;
                if set.used_s1[i] {
                    set.num_used_by_curr += 1;
                }
            }
            Ok(set)
        }
    }

    /// Derive a set predicted from this one (7-61, 7-62).
    ///
    /// The two loops per direction are the spec's, and their order is what puts
    /// `DeltaPocS0` in decreasing and `DeltaPocS1` in increasing order without a
    /// sort: entries derived from the *other* direction of the reference set
    /// come first, then the reference picture itself, then the same direction.
    fn predict(
        &self,
        delta_rps: i32,
        used_by_curr: &[bool],
        use_delta: &[bool],
    ) -> ShortTermRefPicSet {
        let mut out = ShortTermRefPicSet::default();
        let neg = self.num_negative as usize;
        let pos = self.num_positive as usize;
        let num_delta_pocs = neg + pos;

        let mut i = 0usize;
        for j in (0..pos).rev() {
            let d = self.delta_poc_s1[j] + delta_rps;
            if d < 0 && use_delta[neg + j] && i < MAX_ST_REF_PICS {
                out.delta_poc_s0[i] = d;
                out.used_s0[i] = used_by_curr[neg + j];
                i += 1;
            }
        }
        if delta_rps < 0 && use_delta[num_delta_pocs] && i < MAX_ST_REF_PICS {
            out.delta_poc_s0[i] = delta_rps;
            out.used_s0[i] = used_by_curr[num_delta_pocs];
            i += 1;
        }
        for j in 0..neg {
            let d = self.delta_poc_s0[j] + delta_rps;
            if d < 0 && use_delta[j] && i < MAX_ST_REF_PICS {
                out.delta_poc_s0[i] = d;
                out.used_s0[i] = used_by_curr[j];
                i += 1;
            }
        }
        out.num_negative = i as u32;

        let mut i = 0usize;
        for j in (0..neg).rev() {
            let d = self.delta_poc_s0[j] + delta_rps;
            if d > 0 && use_delta[j] && i < MAX_ST_REF_PICS {
                out.delta_poc_s1[i] = d;
                out.used_s1[i] = used_by_curr[j];
                i += 1;
            }
        }
        if delta_rps > 0 && use_delta[num_delta_pocs] && i < MAX_ST_REF_PICS {
            out.delta_poc_s1[i] = delta_rps;
            out.used_s1[i] = used_by_curr[num_delta_pocs];
            i += 1;
        }
        for j in 0..pos {
            let d = self.delta_poc_s1[j] + delta_rps;
            if d > 0 && use_delta[neg + j] && i < MAX_ST_REF_PICS {
                out.delta_poc_s1[i] = d;
                out.used_s1[i] = used_by_curr[neg + j];
                i += 1;
            }
        }
        out.num_positive = i as u32;
        out.num_used_by_curr = out.used_s0[..out.num_negative as usize]
            .iter()
            .chain(out.used_s1[..out.num_positive as usize].iter())
            .filter(|&&u| u)
            .count() as u32;
        out
    }

    /// Write the set in its non-predicted form (7.3.7), which is always legal.
    pub fn write(&self, w: &mut ec_core::BitWriter, idx: u32) {
        if idx != 0 {
            w.write_bit(false); // inter_ref_pic_set_prediction_flag
        }
        w.write_ue(self.num_negative);
        w.write_ue(self.num_positive);
        let mut prev = 0i32;
        for i in 0..self.num_negative as usize {
            w.write_ue((prev - self.delta_poc_s0[i] - 1).max(0) as u32);
            prev = self.delta_poc_s0[i];
            w.write_bit(self.used_s0[i]);
        }
        let mut prev = 0i32;
        for i in 0..self.num_positive as usize {
            w.write_ue((self.delta_poc_s1[i] - prev - 1).max(0) as u32);
            prev = self.delta_poc_s1[i];
            w.write_bit(self.used_s1[i]);
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
    /// `column_width_minus1[]`, empty when the spacing is uniform (a decoder
    /// derives the widths then, and so should a caller).
    pub column_width_minus1: Vec<u32>,
    /// `row_height_minus1[]`, empty when the spacing is uniform.
    pub row_height_minus1: Vec<u32>,
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
    /// The lists `pps_scaling_list_data_present_flag` sends, which override the SPS
    /// lists. `None` when the PPS sends none.
    pub scaling_list: Option<ScalingList>,
    /// `lists_modification_present_flag`.
    pub lists_modification_present: bool,
    /// `log2_parallel_merge_level_minus2`.
    pub log2_parallel_merge_level_minus2: u32,
    /// `slice_segment_header_extension_present_flag`.
    pub slice_segment_header_extension_present: bool,
    /// The PPS extension flags, same layout as [`Sps::extension_flags`].
    pub extension_flags: u8,
    /// The `pps_range_extension()` fields (7.3.2.3.2); defaults when absent.
    pub range: PpsRangeExt,
}

impl Default for Pps {
    fn default() -> Self {
        Pps {
            scaling_list: None,
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
            column_width_minus1: Vec::new(),
            row_height_minus1: Vec::new(),
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
            extension_flags: 0,
            range: Default::default(),
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
            if !self.uniform_spacing {
                for &width in &self.column_width_minus1 {
                    w.write_ue(width);
                }
                for &height in &self.row_height_minus1 {
                    w.write_ue(height);
                }
            }
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
        match &self.scaling_list {
            Some(list) => {
                w.write_bit(true); // pps_scaling_list_data_present_flag
                list.write(&mut w);
            }
            None => w.write_bit(false),
        }
        w.write_bit(self.lists_modification_present);
        w.write_ue(self.log2_parallel_merge_level_minus2);
        w.write_bit(self.slice_segment_header_extension_present);
        write_extension_flags(&mut w, self.extension_flags);
        if self.extension_flags & 0x80 != 0 {
            let r = &self.range;
            if self.transform_skip_enabled {
                w.write_ue(r.log2_max_transform_skip_block_size_minus2);
            }
            w.write_bit(r.cross_component_prediction_enabled);
            w.write_bit(r.chroma_qp_offset_list_enabled);
            if r.chroma_qp_offset_list_enabled {
                w.write_ue(r.diff_cu_chroma_qp_offset_depth);
                w.write_ue(r.cb_qp_offset_list.len() as u32 - 1);
                for (cb, cr) in r.cb_qp_offset_list.iter().zip(&r.cr_qp_offset_list) {
                    w.write_se(*cb);
                    w.write_se(*cr);
                }
            }
            w.write_ue(r.log2_sao_offset_scale_luma);
            w.write_ue(r.log2_sao_offset_scale_chroma);
        }
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
        check_range("HEVC PPS", "pps_pic_parameter_set_id", i64::from(pps.id), 0, 63)?;
        check_range("HEVC PPS", "pps_seq_parameter_set_id", i64::from(pps.sps_id), 0, 15)?;
        pps.dependent_slice_segments_enabled = r.read_bit()?;
        pps.output_flag_present = r.read_bit()?;
        pps.num_extra_slice_header_bits = r.read_bits(3)?;
        pps.sign_data_hiding_enabled = r.read_bit()?;
        pps.cabac_init_present = r.read_bit()?;
        pps.num_ref_idx_l0_default_active_minus1 = r.read_ue()?;
        check_range(
            "HEVC PPS",
            "num_ref_idx_l0_default_active_minus1",
            i64::from(pps.num_ref_idx_l0_default_active_minus1),
            0,
            14,
        )?;
        pps.num_ref_idx_l1_default_active_minus1 = r.read_ue()?;
        check_range(
            "HEVC PPS",
            "num_ref_idx_l1_default_active_minus1",
            i64::from(pps.num_ref_idx_l1_default_active_minus1),
            0,
            14,
        )?;
        pps.init_qp_minus26 = r.read_se()?;
        // 7-6: -(26 + QpBdOffsetY) to +25. The offset is at most 48 (8 bits of depth
        // above 8 is refused by the SPS), and this PPS parser cannot see the SPS.
        check_range(
            "HEVC PPS",
            "init_qp_minus26",
            i64::from(pps.init_qp_minus26),
            -74,
            25,
        )?;
        pps.constrained_intra_pred = r.read_bit()?;
        pps.transform_skip_enabled = r.read_bit()?;
        pps.cu_qp_delta_enabled = r.read_bit()?;
        if pps.cu_qp_delta_enabled {
            pps.diff_cu_qp_delta_depth = r.read_ue()?;
            // 7-7: at most log2_diff_max_min_luma_coding_block_size, which is at most 3.
            check_range(
                "HEVC PPS",
                "diff_cu_qp_delta_depth",
                i64::from(pps.diff_cu_qp_delta_depth),
                0,
                3,
            )?;
        }
        pps.cb_qp_offset = r.read_se()?;
        check_range("HEVC PPS", "pps_cb_qp_offset", i64::from(pps.cb_qp_offset), -12, 12)?;
        pps.cr_qp_offset = r.read_se()?;
        check_range("HEVC PPS", "pps_cr_qp_offset", i64::from(pps.cr_qp_offset), -12, 12)?;
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
            // The tile count is capped for both spacings: a uniform split would
            // otherwise size its tile arrays from any count the stream names.
            if pps.num_tile_columns_minus1 > 20 || pps.num_tile_rows_minus1 > 22 {
                return Err(Error::corrupt("HEVC PPS: tile count beyond any level"));
            }
            if !pps.uniform_spacing {
                for _ in 0..pps.num_tile_columns_minus1 {
                    let value = r.read_ue()?;
                    pps.column_width_minus1.push(value);
                }
                for _ in 0..pps.num_tile_rows_minus1 {
                    let value = r.read_ue()?;
                    pps.row_height_minus1.push(value);
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
                check_range(
                    "HEVC PPS",
                    "pps_beta_offset_div2",
                    i64::from(pps.beta_offset_div2),
                    -6,
                    6,
                )?;
                pps.tc_offset_div2 = r.read_se()?;
                check_range(
                    "HEVC PPS",
                    "pps_tc_offset_div2",
                    i64::from(pps.tc_offset_div2),
                    -6,
                    6,
                )?;
            }
        }
        if r.read_bit()? {
            pps.scaling_list = Some(ScalingList::parse(&mut r)?);
        }
        pps.lists_modification_present = r.read_bit()?;
        pps.log2_parallel_merge_level_minus2 = r.read_ue()?;
        // 7-37: Log2ParMrgLevelY is at most CtbLog2SizeY, which is at most 6.
        check_range(
            "HEVC PPS",
            "log2_parallel_merge_level_minus2",
            i64::from(pps.log2_parallel_merge_level_minus2),
            0,
            4,
        )?;
        pps.slice_segment_header_extension_present = r.read_bit()?;
        pps.extension_flags = parse_extension_flags(&mut r);
        if pps.extension_flags & 0x80 != 0 {
            pps.range = PpsRangeExt::parse(&mut r, pps.transform_skip_enabled)?;
        }
        Ok(pps)
    }
}

/// Writes the `*_extension_present_flag` block packed as [`Sps::extension_flags`]. Only the
/// flags are written, not the extension bodies that follow them: the decoder never reads an
/// extension body, and the refusal fixtures need only the flags.
fn write_extension_flags(w: &mut BitWriter, flags: u8) {
    let present = flags != 0;
    w.write_bit(present);
    if present {
        for bit in 0..4 {
            w.write_bit(flags & (0x80 >> bit) != 0);
        }
        w.write_bits(u32::from(flags & 0x0f), 4);
    }
}

/// The `*_extension_present_flag` block at the end of an SPS or PPS (7.3.2.2.1, 7.3.2.3.1),
/// packed as [`Sps::extension_flags`]. A truncated tail reads as no extension.
fn parse_extension_flags(r: &mut BitReader) -> u8 {
    if !matches!(r.read_bit(), Ok(true)) {
        return 0;
    }
    let mut flags = 0u8;
    for bit in 0..4 {
        if matches!(r.read_bit(), Ok(true)) {
            flags |= 0x80 >> bit;
        }
    }
    flags | r.read_bits(4).unwrap_or(0) as u8
}

/// `rbsp_trailing_bits()`: a one bit then zeros to the byte boundary.
pub fn rbsp_trailing_bits(w: &mut BitWriter) {
    w.write_bit(true);
    w.align_to_byte();
}

/// `vps_max_sub_layers_minus1` and `sps_max_sub_layers_minus1` (7.4.3.1, 7.4.3.2.1): 0 to 6.
const MAX_SUB_LAYERS_MINUS1: i64 = 6;

/// `sps_max_dec_pic_buffering_minus1` (7.4.3.2.1): MaxDpbSize - 1, at most 15.
const MAX_DPB_PICTURES_MINUS1: i64 = 15;

/// `delta_poc_sX_minus1` and `abs_delta_rps_minus1` (7-53, 7-54, 7-61): 0 to 2^15 - 1.
const MAX_DELTA_POC_MINUS1: i64 = 32_767;

/// Refuse `value` unless `lo <= value <= hi`. The message names the syntax element.
pub(crate) fn check_range(what: &str, field: &str, value: i64, lo: i64, hi: i64) -> Result<()> {
    if (lo..=hi).contains(&value) {
        Ok(())
    } else {
        Err(Error::corrupt(format!(
            "{what}: {field} = {value}, outside {lo}..={hi}"
        )))
    }
}

/// Read a `delta_poc_sX_minus1` and return the delta it stands for, `minus1 + 1`.
fn read_delta_poc(r: &mut BitReader) -> Result<i32> {
    let minus1 = r.read_ue()?;
    check_range(
        "HEVC st_ref_pic_set",
        "delta_poc_sX_minus1",
        i64::from(minus1),
        0,
        MAX_DELTA_POC_MINUS1,
    )?;
    Ok(minus1 as i32 + 1)
}

/// `Ceil( Log2( v ) )`, the width of a `u(v)` field counting `v` values.
pub fn ceil_log2(v: u32) -> u32 {
    let mut bits = 0;
    while (1u64 << bits) < u64::from(v) {
        bits += 1;
    }
    bits
}

/// `delta_poc_s0_minus1` / `delta_poc_s1_minus1` (7-53, 7-54): in range 0..=2^15 - 1.
/// Checked here so the running POC sums in `ShortTermRefPicSet` cannot overflow.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::vui::{ColourDescription, VideoSignalType};

    fn sample_sps() -> Sps {
        Sps {
            max_latency_increase_plus1: 0,
            extension_flags: 0,
            range: Default::default(),
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
            scaling_list: None,
            amp_enabled: false,
            sao_enabled: false,
            pcm_enabled: false,
            pcm: None,
            num_short_term_ref_pic_sets: 0,
            short_term_ref_pic_sets: Vec::new(),
            long_term_ref_pics_present: false,
            num_long_term_ref_pics_sps: 0,
            long_term_ref_pics_sps: Vec::new(),
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
                chroma_sample_loc: None,
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

        // The fields a hardware decoder needs but this family's own encoder
        // never writes: explicit tile geometry and PCM.
        let tiled = Pps {
            tiles_enabled: true,
            num_tile_columns_minus1: 2,
            num_tile_rows_minus1: 1,
            uniform_spacing: false,
            column_width_minus1: vec![9, 19],
            row_height_minus1: vec![7],
            loop_filter_across_tiles_enabled: false,
            cu_qp_delta_enabled: true,
            diff_cu_qp_delta_depth: 2,
            cb_qp_offset: -3,
            cr_qp_offset: 4,
            ..Pps::default()
        };
        assert_eq!(Pps::parse(&tiled.to_rbsp()).unwrap(), tiled);

        let pcm = Sps {
            pcm_enabled: true,
            pcm: Some((7, 7, 0, 2, true)),
            sao_enabled: true,
            ..sample_sps()
        };
        assert_eq!(Sps::parse(&pcm.to_rbsp()).unwrap(), pcm);
    }

    /// Assert `result` is refused with `Error::Corrupt` and that the message names `field`.
    fn refused_naming<T: std::fmt::Debug>(result: Result<T>, field: &str) {
        match result {
            Ok(v) => panic!("accepted, expected {field} to be refused: {v:?}"),
            Err(e) => {
                assert!(matches!(e, Error::Corrupt { .. }), "not Corrupt: {e:?}");
                assert!(e.to_string().contains(field), "message does not name {field}: {e}");
            }
        }
    }

    /// Bits written by `build`, padded to a byte.
    fn bits(build: impl FnOnce(&mut BitWriter)) -> Vec<u8> {
        let mut w = BitWriter::new();
        build(&mut w);
        w.align_to_byte();
        w.into_bytes()
    }

    fn sps_with(edit: impl FnOnce(&mut Sps)) -> Vec<u8> {
        let mut sps = sample_sps();
        edit(&mut sps);
        sps.to_rbsp()
    }

    #[test]
    fn seven_sub_layers_is_refused_by_name_and_six_is_not() {
        // vps_max_sub_layers_minus1 is bits 12..15 of the VPS RBSP; sps_ is bits 4..7 of the SPS.
        let mut vps = Vps {
            id: 0,
            ptl: ProfileTierLevel::main(120),
            max_dec_pic_buffering_minus1: 0,
            max_num_reorder_pics: 0,
        }
        .to_rbsp();
        vps[1] |= 0x0e;
        refused_naming(Vps::parse(&vps), "vps_max_sub_layers_minus1");

        let mut sps = sample_sps().to_rbsp();
        sps[0] |= 0x0e;
        refused_naming(Sps::parse(&sps), "sps_max_sub_layers_minus1");

        // Six is legal, so the guard must let it through. The rest of this RBSP
        // is then read at the wrong offsets, so the parse may fail later, but not
        // on the sub-layer count.
        let mut sps = sample_sps().to_rbsp();
        sps[0] |= 0x0c;
        if let Err(e) = Sps::parse(&sps) {
            assert!(!e.to_string().contains("max_sub_layers"), "{e}");
        }
    }

    #[test]
    fn sps_block_sizes_and_depths_are_range_checked() {
        // CtbLog2SizeY outside 4..=6: a 128 CTB, and an 8 CTB.
        refused_naming(
            Sps::parse(&sps_with(|s| s.log2_diff_max_min_cb_size = 4)),
            "CtbLog2SizeY",
        );
        refused_naming(
            Sps::parse(&sps_with(|s| s.log2_diff_max_min_cb_size = 0)),
            "CtbLog2SizeY",
        );
        // An absurd minimum CB is refused by name, not by a wrapped sum.
        refused_naming(
            Sps::parse(&sps_with(|s| s.log2_min_cb_size_minus3 = 40)),
            "CtbLog2SizeY",
        );
        // MinTbLog2SizeY must stay below MinCbLog2SizeY (8): a 8x8 minimum TB is not.
        refused_naming(
            Sps::parse(&sps_with(|s| s.log2_min_tb_size_minus2 = 1)),
            "MinTbLog2SizeY",
        );
        // MaxTbLog2SizeY is at most 5 (32 samples): a 64 largest TB is not.
        refused_naming(
            Sps::parse(&sps_with(|s| s.log2_diff_max_min_tb_size = 4)),
            "MaxTbLog2SizeY",
        );
        // max_transform_hierarchy_depth is at most CtbLog2SizeY - MinTbLog2SizeY = 4.
        refused_naming(
            Sps::parse(&sps_with(|s| s.max_transform_hierarchy_depth_inter = 5)),
            "max_transform_hierarchy_depth_inter",
        );
        refused_naming(
            Sps::parse(&sps_with(|s| s.max_transform_hierarchy_depth_intra = 5)),
            "max_transform_hierarchy_depth_intra",
        );
        // The depth at its bound is still legal.
        assert!(Sps::parse(&sps_with(|s| s.max_transform_hierarchy_depth_intra = 4)).is_ok());
    }

    #[test]
    fn sps_dpb_size_and_ids_are_range_checked() {
        refused_naming(
            Sps::parse(&sps_with(|s| s.max_dec_pic_buffering_minus1 = 16)),
            "sps_max_dec_pic_buffering_minus1",
        );
        assert!(Sps::parse(&sps_with(|s| s.max_dec_pic_buffering_minus1 = 15)).is_ok());
        refused_naming(
            Sps::parse(&sps_with(|s| s.id = 16)),
            "sps_seq_parameter_set_id",
        );
    }

    #[test]
    fn conformance_window_must_leave_a_picture() {
        // 4:2:0 crops two luma rows per chroma-unit offset: 2 * 544 = 1088 leaves nothing.
        refused_naming(
            Sps::parse(&sps_with(|s| {
                s.conf_win = ConformanceWindow {
                    left: 0,
                    right: 0,
                    top: 0,
                    bottom: 544,
                }
            })),
            "conformance window",
        );
        // A huge offset must not wrap the sum either.
        refused_naming(
            Sps::parse(&sps_with(|s| {
                s.conf_win = ConformanceWindow {
                    left: 0x7fff_ffff,
                    right: 0x7fff_ffff,
                    top: 0,
                    bottom: 0,
                }
            })),
            "conformance window",
        );
    }

    #[test]
    fn vps_dpb_size_is_range_checked() {
        let vps = Vps {
            id: 0,
            ptl: ProfileTierLevel::main(120),
            max_dec_pic_buffering_minus1: 16,
            max_num_reorder_pics: 0,
        };
        refused_naming(Vps::parse(&vps.to_rbsp()), "vps_max_dec_pic_buffering_minus1");
    }

    #[test]
    fn pps_reference_counts_and_qp_are_range_checked() {
        let pps = |edit: fn(&mut Pps)| {
            let mut p = Pps::default();
            edit(&mut p);
            p.to_rbsp()
        };
        refused_naming(
            Pps::parse(&pps(|p| p.num_ref_idx_l0_default_active_minus1 = 15)),
            "num_ref_idx_l0_default_active_minus1",
        );
        refused_naming(
            Pps::parse(&pps(|p| p.num_ref_idx_l1_default_active_minus1 = 15)),
            "num_ref_idx_l1_default_active_minus1",
        );
        refused_naming(
            Pps::parse(&pps(|p| p.init_qp_minus26 = 26)),
            "init_qp_minus26",
        );
        refused_naming(
            Pps::parse(&pps(|p| p.init_qp_minus26 = -75)),
            "init_qp_minus26",
        );
        refused_naming(
            Pps::parse(&pps(|p| p.cb_qp_offset = 13)),
            "pps_cb_qp_offset",
        );
        refused_naming(
            Pps::parse(&pps(|p| p.cr_qp_offset = -13)),
            "pps_cr_qp_offset",
        );
        refused_naming(
            Pps::parse(&pps(|p| {
                p.cu_qp_delta_enabled = true;
                p.diff_cu_qp_delta_depth = 4;
            })),
            "diff_cu_qp_delta_depth",
        );
        refused_naming(
            Pps::parse(&pps(|p| p.log2_parallel_merge_level_minus2 = 5)),
            "log2_parallel_merge_level_minus2",
        );
        refused_naming(Pps::parse(&pps(|p| p.id = 64)), "pps_pic_parameter_set_id");
        refused_naming(
            Pps::parse(&pps(|p| p.sps_id = 16)),
            "pps_seq_parameter_set_id",
        );

        // The bounds themselves are legal.
        let edge = Pps {
            num_ref_idx_l0_default_active_minus1: 14,
            num_ref_idx_l1_default_active_minus1: 14,
            init_qp_minus26: -74,
            cb_qp_offset: -12,
            cr_qp_offset: 12,
            cu_qp_delta_enabled: true,
            diff_cu_qp_delta_depth: 3,
            log2_parallel_merge_level_minus2: 4,
            ..Pps::default()
        };
        assert_eq!(Pps::parse(&edge.to_rbsp()).unwrap(), edge);
    }

    #[test]
    fn pps_loop_filter_offsets_are_range_checked() {
        let deblock = |beta: i32, tc: i32| {
            Pps {
                deblocking_filter_control_present: true,
                deblocking_filter_disabled: false,
                beta_offset_div2: beta,
                tc_offset_div2: tc,
                ..Pps::default()
            }
            .to_rbsp()
        };
        refused_naming(Pps::parse(&deblock(7, 0)), "pps_beta_offset_div2");
        refused_naming(Pps::parse(&deblock(0, -7)), "pps_tc_offset_div2");
        assert!(Pps::parse(&deblock(-6, 6)).is_ok());
    }

    #[test]
    fn pps_tile_count_is_capped_for_uniform_spacing_too() {
        let tiled = Pps {
            tiles_enabled: true,
            uniform_spacing: true,
            num_tile_columns_minus1: 21,
            num_tile_rows_minus1: 0,
            ..Pps::default()
        };
        refused_naming(Pps::parse(&tiled.to_rbsp()), "tile count beyond any level");
    }

    #[test]
    fn st_ref_pic_set_fields_are_range_checked() {
        // Set 0 explicit, one negative picture with delta_poc_s0_minus1 = 32768.
        let too_far = bits(|w| {
            w.write_ue(1); // num_negative_pics
            w.write_ue(0); // num_positive_pics
            w.write_ue(32_768);
            w.write_bit(true);
        });
        refused_naming(
            ShortTermRefPicSet::parse(&mut BitReader::new(&too_far), 0, 1, &[]),
            "delta_poc_sX_minus1",
        );
        // The largest legal delta is accepted.
        let largest = bits(|w| {
            w.write_ue(1);
            w.write_ue(0);
            w.write_ue(32_767);
            w.write_bit(true);
        });
        assert_eq!(
            ShortTermRefPicSet::parse(&mut BitReader::new(&largest), 0, 1, &[])
                .unwrap()
                .delta_poc_s0[0],
            -32_768
        );

        // Set 1 predicted from set 0 with delta_idx_minus1 = 1 names a set before set 0.
        // delta_idx_minus1 is only read in the slice header (idx == num_sets), so
        // here num_sets is 1.
        let set0 = ShortTermRefPicSet::default();
        let bad_idx = bits(|w| {
            w.write_bit(true); // inter_ref_pic_set_prediction_flag
            w.write_ue(1); // delta_idx_minus1
        });
        refused_naming(
            ShortTermRefPicSet::parse(&mut BitReader::new(&bad_idx), 1, 1, &[set0]),
            "delta_idx out of range",
        );

        // abs_delta_rps_minus1 = 32768 is past 2^15 - 1.
        let bad_rps = bits(|w| {
            w.write_bit(true); // inter_ref_pic_set_prediction_flag
            w.write_ue(0); // delta_idx_minus1
            w.write_bit(false); // delta_rps_sign
            w.write_ue(32_768); // abs_delta_rps_minus1
        });
        refused_naming(
            ShortTermRefPicSet::parse(&mut BitReader::new(&bad_rps), 1, 2, &[set0]),
            "abs_delta_rps_minus1",
        );
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

/// The `sps_range_extension()` flags of 7.3.2.2.2, in bitstream order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpsRangeExt {
    /// `transform_skip_rotation_enabled`.
    pub transform_skip_rotation_enabled: bool,
    /// `transform_skip_context_enabled`.
    pub transform_skip_context_enabled: bool,
    /// `implicit_rdpcm_enabled`.
    pub implicit_rdpcm_enabled: bool,
    /// `explicit_rdpcm_enabled`.
    pub explicit_rdpcm_enabled: bool,
    /// `extended_precision_processing`.
    pub extended_precision_processing: bool,
    /// `intra_smoothing_disabled`.
    pub intra_smoothing_disabled: bool,
    /// `high_precision_offsets_enabled`.
    pub high_precision_offsets_enabled: bool,
    /// `persistent_rice_adaptation_enabled`.
    pub persistent_rice_adaptation_enabled: bool,
    /// `cabac_bypass_alignment_enabled`.
    pub cabac_bypass_alignment_enabled: bool,
}

impl SpsRangeExt {
    fn parse(r: &mut BitReader) -> Result<Self> {
        Ok(SpsRangeExt {
            transform_skip_rotation_enabled: r.read_bit()?,
            transform_skip_context_enabled: r.read_bit()?,
            implicit_rdpcm_enabled: r.read_bit()?,
            explicit_rdpcm_enabled: r.read_bit()?,
            extended_precision_processing: r.read_bit()?,
            intra_smoothing_disabled: r.read_bit()?,
            high_precision_offsets_enabled: r.read_bit()?,
            persistent_rice_adaptation_enabled: r.read_bit()?,
            cabac_bypass_alignment_enabled: r.read_bit()?,
        })
    }
}

/// The `pps_range_extension()` fields of 7.3.2.3.2. The defaults are the inferred values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PpsRangeExt {
    /// `log2_max_transform_skip_block_size_minus2` (0 when transform skip is off).
    pub log2_max_transform_skip_block_size_minus2: u32,
    /// `cross_component_prediction_enabled`.
    pub cross_component_prediction_enabled: bool,
    /// `chroma_qp_offset_list_enabled`.
    pub chroma_qp_offset_list_enabled: bool,
    /// `diff_cu_chroma_qp_offset_depth`.
    pub diff_cu_chroma_qp_offset_depth: u32,
    /// `cb_qp_offset_list[i]` for `i` in `0..=chroma_qp_offset_list_len_minus1`.
    pub cb_qp_offset_list: Vec<i32>,
    /// `cr_qp_offset_list[i]`, same length as `cb_qp_offset_list`.
    pub cr_qp_offset_list: Vec<i32>,
    /// `log2_sao_offset_scale_luma`.
    pub log2_sao_offset_scale_luma: u32,
    /// `log2_sao_offset_scale_chroma`.
    pub log2_sao_offset_scale_chroma: u32,
}

impl PpsRangeExt {
    fn parse(r: &mut BitReader, transform_skip_enabled: bool) -> Result<Self> {
        let mut e = PpsRangeExt::default();
        if transform_skip_enabled {
            e.log2_max_transform_skip_block_size_minus2 = r.read_ue()?;
        }
        e.cross_component_prediction_enabled = r.read_bit()?;
        e.chroma_qp_offset_list_enabled = r.read_bit()?;
        if e.chroma_qp_offset_list_enabled {
            e.diff_cu_chroma_qp_offset_depth = r.read_ue()?;
            let len = r.read_ue()? + 1;
            if len > 6 {
                return Err(Error::corrupt("HEVC PPS: chroma_qp_offset_list_len_minus1"));
            }
            for _ in 0..len {
                e.cb_qp_offset_list.push(r.read_se()?);
                e.cr_qp_offset_list.push(r.read_se()?);
            }
        }
        e.log2_sao_offset_scale_luma = r.read_ue()?;
        e.log2_sao_offset_scale_chroma = r.read_ue()?;
        Ok(e)
    }
}
