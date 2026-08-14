//! The slice segment header (7.3.6.1).

use crate::nal::NalUnitType;
use crate::ps::{Pps, ShortTermRefPicSet, Sps, ceil_log2};
use ec_core::bitio::{BitReader, BitWriter};
use ec_core::error::{Error, Result};

/// `slice_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceType {
    /// Bi-predictive.
    B,
    /// Predictive.
    P,
    /// Intra — the only kind this family's HEVC encoder writes.
    I,
}

impl SliceType {
    /// The wire value (`B` = 0, `P` = 1, `I` = 2).
    pub fn code(self) -> u32 {
        match self {
            SliceType::B => 0,
            SliceType::P => 1,
            SliceType::I => 2,
        }
    }

    /// The type for a wire value.
    pub fn from_code(code: u32) -> Result<SliceType> {
        match code {
            0 => Ok(SliceType::B),
            1 => Ok(SliceType::P),
            2 => Ok(SliceType::I),
            v => Err(Error::corrupt(format!("HEVC slice: slice_type = {v}"))),
        }
    }
}

/// A slice segment header, as far as an intra encoder writes one and a
/// stateless hardware decoder reads one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceHeader {
    /// `first_slice_segment_in_pic_flag`.
    pub first_slice_segment_in_pic: bool,
    /// `no_output_of_prior_pics_flag`, present on IRAP pictures only.
    pub no_output_of_prior_pics: bool,
    /// `slice_pic_parameter_set_id`.
    pub pps_id: u32,
    /// `dependent_slice_segment_flag`.
    pub dependent_slice_segment: bool,
    /// `slice_segment_address` in CTB raster order.
    pub segment_address: u32,
    /// `slice_type`.
    pub slice_type: SliceType,
    /// `pic_output_flag`, when the PPS says it is present.
    pub pic_output_flag: bool,
    /// `slice_pic_order_cnt_lsb`; absent (0) on IDR pictures.
    pub poc_lsb: u32,
    /// `slice_sao_luma_flag`.
    pub sao_luma: bool,
    /// `slice_sao_chroma_flag`.
    pub sao_chroma: bool,
    /// `num_ref_idx_l0_active_minus1` in force for this slice.
    pub num_ref_idx_l0_active_minus1: u32,
    /// `num_ref_idx_l1_active_minus1` in force for this slice.
    pub num_ref_idx_l1_active_minus1: u32,
    /// `mvd_l1_zero_flag`.
    pub mvd_l1_zero: bool,
    /// `cabac_init_flag`.
    pub cabac_init: bool,
    /// `slice_temporal_mvp_enabled_flag`.
    pub temporal_mvp_enabled: bool,
    /// `collocated_from_l0_flag`.
    pub collocated_from_l0: bool,
    /// `collocated_ref_idx`.
    pub collocated_ref_idx: u32,
    /// `five_minus_max_num_merge_cand`.
    pub five_minus_max_num_merge_cand: u32,
    /// `slice_qp_delta`: the slice QP is `26 + init_qp_minus26 + this`.
    pub qp_delta: i32,
    /// `slice_cb_qp_offset`.
    pub cb_qp_offset: i32,
    /// `slice_cr_qp_offset`.
    pub cr_qp_offset: i32,
    /// `slice_deblocking_filter_disabled_flag`, after any override.
    pub deblocking_filter_disabled: bool,
    /// `slice_beta_offset_div2`.
    pub beta_offset_div2: i32,
    /// `slice_tc_offset_div2`.
    pub tc_offset_div2: i32,
    /// `slice_loop_filter_across_slices_enabled_flag`.
    pub loop_filter_across_slices_enabled: bool,
    /// Substream entry points: one per WPP CTB row (or tile) after the first.
    pub entry_point_offsets: Vec<u32>,
}

impl SliceHeader {
    /// An I slice covering a whole picture at `qp_delta`, filters off.
    pub fn intra(pps: &Pps, qp_delta: i32) -> SliceHeader {
        SliceHeader {
            first_slice_segment_in_pic: true,
            no_output_of_prior_pics: false,
            pps_id: pps.id,
            dependent_slice_segment: false,
            segment_address: 0,
            slice_type: SliceType::I,
            pic_output_flag: true,
            poc_lsb: 0,
            sao_luma: false,
            sao_chroma: false,
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            mvd_l1_zero: false,
            cabac_init: false,
            temporal_mvp_enabled: false,
            collocated_from_l0: true,
            collocated_ref_idx: 0,
            five_minus_max_num_merge_cand: 0,
            qp_delta,
            cb_qp_offset: 0,
            cr_qp_offset: 0,
            deblocking_filter_disabled: pps.deblocking_filter_disabled,
            beta_offset_div2: 0,
            tc_offset_div2: 0,
            loop_filter_across_slices_enabled: pps.loop_filter_across_slices_enabled,
            entry_point_offsets: Vec::new(),
        }
    }

    /// Write the header, up to and including `byte_alignment()`.
    ///
    /// Entry point offsets are written from `self.entry_point_offsets`, so a WPP
    /// encoder codes its substreams first and builds the header afterwards —
    /// the offsets are not knowable any earlier.
    pub fn write(&self, w: &mut BitWriter, sps: &Sps, pps: &Pps, nal_type: NalUnitType) {
        w.write_bit(self.first_slice_segment_in_pic);
        if nal_type.is_irap() {
            w.write_bit(self.no_output_of_prior_pics);
        }
        w.write_ue(self.pps_id);
        if !self.first_slice_segment_in_pic {
            if pps.dependent_slice_segments_enabled {
                w.write_bit(self.dependent_slice_segment);
            }
            w.write_bits(self.segment_address, sps.slice_address_bits());
        }
        if !self.dependent_slice_segment {
            for _ in 0..pps.num_extra_slice_header_bits {
                w.write_bit(false);
            }
            w.write_ue(self.slice_type.code());
            if pps.output_flag_present {
                w.write_bit(self.pic_output_flag);
            }
            if !nal_type.is_idr() {
                w.write_bits(self.poc_lsb, sps.log2_max_poc_lsb_minus4 + 4);
                // An intra encoder writes no reference picture sets at all; a
                // non-IDR picture from this family does not exist yet.
                w.write_bit(true); // short_term_ref_pic_set_sps_flag
                if sps.num_short_term_ref_pic_sets > 1 {
                    w.write_bits(0, ceil_log2(sps.num_short_term_ref_pic_sets));
                }
                if sps.temporal_mvp_enabled {
                    w.write_bit(self.temporal_mvp_enabled);
                }
            }
            if sps.sao_enabled {
                w.write_bit(self.sao_luma);
                if sps.chroma_format_idc != 0 {
                    w.write_bit(self.sao_chroma);
                }
            }
            if self.slice_type != SliceType::I {
                w.write_bit(false); // num_ref_idx_active_override_flag
                if self.slice_type == SliceType::B {
                    w.write_bit(self.mvd_l1_zero);
                }
                if pps.cabac_init_present {
                    w.write_bit(self.cabac_init);
                }
                w.write_ue(self.five_minus_max_num_merge_cand);
            }
            w.write_se(self.qp_delta);
            if pps.slice_chroma_qp_offsets_present {
                w.write_se(self.cb_qp_offset);
                w.write_se(self.cr_qp_offset);
            }
            if pps.deblocking_filter_override_enabled {
                w.write_bit(false); // deblocking_filter_override_flag
            }
            if pps.loop_filter_across_slices_enabled
                && (self.sao_luma || self.sao_chroma || !self.deblocking_filter_disabled)
            {
                w.write_bit(self.loop_filter_across_slices_enabled);
            }
        }
        if pps.tiles_enabled || pps.entropy_coding_sync_enabled {
            w.write_ue(self.entry_point_offsets.len() as u32);
            if !self.entry_point_offsets.is_empty() {
                let max = self.entry_point_offsets.iter().copied().max().unwrap_or(1);
                let offset_len = ceil_log2(max + 1).max(1);
                w.write_ue(offset_len - 1);
                for &offset in &self.entry_point_offsets {
                    w.write_bits(offset - 1, offset_len);
                }
            }
        }
        // byte_alignment(): a one bit then zeros.
        w.write_bit(true);
        w.align_to_byte();
    }

    /// Parse a slice segment header out of an unescaped slice NAL payload.
    ///
    /// Returns the header and the bit position where slice data begins — what
    /// `VASliceParameterBufferHEVC::slice_data_byte_offset` is derived from.
    pub fn parse(
        rbsp: &[u8],
        sps: &Sps,
        pps: &Pps,
        nal_type: NalUnitType,
    ) -> Result<(SliceHeader, ParsePositions)> {
        let mut r = BitReader::new(rbsp);
        let mut h = SliceHeader::intra(pps, 0);
        let mut pos = ParsePositions::default();
        h.first_slice_segment_in_pic = r.read_bit()?;
        if nal_type.is_irap() {
            h.no_output_of_prior_pics = r.read_bit()?;
        }
        h.pps_id = r.read_ue()?;
        if !h.first_slice_segment_in_pic {
            if pps.dependent_slice_segments_enabled {
                h.dependent_slice_segment = r.read_bit()?;
            }
            h.segment_address = r.read_bits(sps.slice_address_bits())?;
        }
        if !h.dependent_slice_segment {
            for _ in 0..pps.num_extra_slice_header_bits {
                r.read_bit()?;
            }
            h.slice_type = SliceType::from_code(r.read_ue()?)?;
            if pps.output_flag_present {
                h.pic_output_flag = r.read_bit()?;
            }
            if sps.separate_colour_plane {
                r.read_bits(2)?; // colour_plane_id
            }
            let mut st_rps = ShortTermRefPicSet::default();
            let mut num_long_term = 0;
            if !nal_type.is_idr() {
                h.poc_lsb = r.read_bits(sps.log2_max_poc_lsb_minus4 + 4)?;
                let from_sps = r.read_bit()?;
                let before = r.bit_position();
                if !from_sps {
                    // The sets in the SPS are not kept, so an inter-set
                    // predicted slice set cannot be resolved here.
                    st_rps = ShortTermRefPicSet::parse(&mut r, 0, &[])?;
                } else if sps.num_short_term_ref_pic_sets > 1 {
                    r.read_bits(ceil_log2(sps.num_short_term_ref_pic_sets))?;
                }
                pos.st_rps_bits = (r.bit_position() - before) as u32;
                if sps.long_term_ref_pics_present {
                    let num_long_term_sps = if sps.num_long_term_ref_pics_sps > 0 {
                        r.read_ue()?
                    } else {
                        0
                    };
                    let num_long_term_pics = r.read_ue()?;
                    for i in 0..num_long_term_sps + num_long_term_pics {
                        if i < num_long_term_sps {
                            if sps.num_long_term_ref_pics_sps > 1 {
                                r.read_bits(ceil_log2(sps.num_long_term_ref_pics_sps))?;
                            }
                            num_long_term += 1;
                        } else {
                            r.read_bits(sps.log2_max_poc_lsb_minus4 + 4)?; // poc_lsb_lt
                            if r.read_bit()? {
                                num_long_term += 1; // used_by_curr_pic_lt_flag
                            }
                        }
                        if r.read_bit()? {
                            r.read_ue()?; // delta_poc_msb_cycle_lt
                        }
                    }
                }
                if sps.temporal_mvp_enabled {
                    h.temporal_mvp_enabled = r.read_bit()?;
                }
            }
            if sps.sao_enabled {
                h.sao_luma = r.read_bit()?;
                if sps.chroma_format_idc != 0 {
                    h.sao_chroma = r.read_bit()?;
                }
            }
            if h.slice_type != SliceType::I {
                h.num_ref_idx_l0_active_minus1 = pps.num_ref_idx_l0_default_active_minus1;
                h.num_ref_idx_l1_active_minus1 = pps.num_ref_idx_l1_default_active_minus1;
                if r.read_bit()? {
                    h.num_ref_idx_l0_active_minus1 = r.read_ue()?;
                    if h.slice_type == SliceType::B {
                        h.num_ref_idx_l1_active_minus1 = r.read_ue()?;
                    }
                }
                let num_pic_total_curr = st_rps.num_used_by_curr + num_long_term;
                if pps.lists_modification_present && num_pic_total_curr > 1 {
                    let bits = ceil_log2(num_pic_total_curr);
                    if r.read_bit()? {
                        for _ in 0..=h.num_ref_idx_l0_active_minus1 {
                            r.read_bits(bits)?;
                        }
                    }
                    if h.slice_type == SliceType::B && r.read_bit()? {
                        for _ in 0..=h.num_ref_idx_l1_active_minus1 {
                            r.read_bits(bits)?;
                        }
                    }
                }
                if h.slice_type == SliceType::B {
                    h.mvd_l1_zero = r.read_bit()?;
                }
                if pps.cabac_init_present {
                    h.cabac_init = r.read_bit()?;
                }
                if h.temporal_mvp_enabled {
                    if h.slice_type == SliceType::B {
                        h.collocated_from_l0 = r.read_bit()?;
                    }
                    if (h.collocated_from_l0 && h.num_ref_idx_l0_active_minus1 > 0)
                        || (!h.collocated_from_l0 && h.num_ref_idx_l1_active_minus1 > 0)
                    {
                        h.collocated_ref_idx = r.read_ue()?;
                    }
                }
                if (pps.weighted_pred && h.slice_type == SliceType::P)
                    || (pps.weighted_bipred && h.slice_type == SliceType::B)
                {
                    skip_pred_weight_table(&mut r, &h, sps)?;
                }
                h.five_minus_max_num_merge_cand = r.read_ue()?;
            }
            h.qp_delta = r.read_se()?;
            if pps.slice_chroma_qp_offsets_present {
                h.cb_qp_offset = r.read_se()?;
                h.cr_qp_offset = r.read_se()?;
            }
            h.deblocking_filter_disabled = pps.deblocking_filter_disabled;
            h.beta_offset_div2 = pps.beta_offset_div2;
            h.tc_offset_div2 = pps.tc_offset_div2;
            if pps.deblocking_filter_override_enabled && r.read_bit()? {
                h.deblocking_filter_disabled = r.read_bit()?;
                if !h.deblocking_filter_disabled {
                    h.beta_offset_div2 = r.read_se()?;
                    h.tc_offset_div2 = r.read_se()?;
                }
            }
            h.loop_filter_across_slices_enabled = pps.loop_filter_across_slices_enabled;
            if pps.loop_filter_across_slices_enabled
                && (h.sao_luma || h.sao_chroma || !h.deblocking_filter_disabled)
            {
                h.loop_filter_across_slices_enabled = r.read_bit()?;
            }
        }
        if pps.tiles_enabled || pps.entropy_coding_sync_enabled {
            let count = r.read_ue()?;
            if count > 0 {
                if count as u64 > sps.pic_height_in_ctbs() as u64 * sps.pic_width_in_ctbs() as u64 {
                    return Err(Error::corrupt("HEVC slice: num_entry_point_offsets absurd"));
                }
                let offset_len = r.read_ue()? + 1;
                if offset_len > 32 {
                    return Err(Error::corrupt("HEVC slice: offset_len_minus1 > 31"));
                }
                for _ in 0..count {
                    h.entry_point_offsets.push(r.read_bits(offset_len)? + 1);
                }
            }
        }
        if pps.slice_segment_header_extension_present {
            let len = r.read_ue()?;
            for _ in 0..len {
                r.read_bits(8)?;
            }
        }
        // byte_alignment()
        if !r.read_bit()? {
            return Err(Error::corrupt("HEVC slice: alignment_bit_equal_to_one = 0"));
        }
        r.align_to_byte();
        pos.header_bits = r.bit_position();
        Ok((h, pos))
    }
}

/// Where the slice header ended, in the units a hardware decoder wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ParsePositions {
    /// Bit position in the *unescaped* RBSP where slice data starts; always a
    /// multiple of 8.
    pub header_bits: u64,
    /// `st_rps_bits`: the size of the `st_ref_pic_set()` in the slice header,
    /// which `VAPictureParameterBufferHEVC` carries so the driver can skip it.
    pub st_rps_bits: u32,
}

impl ParsePositions {
    /// `slice_data_byte_offset`: the offset from the start of the NAL unit
    /// (header bytes included) to the first byte of slice data, counted in the
    /// *escaped* bytes a driver is handed.
    ///
    /// The emulation prevention bytes inside the header count, which is why this
    /// takes the escaped payload rather than working from `header_bits` alone.
    pub fn slice_data_byte_offset(&self, escaped_payload: &[u8]) -> usize {
        let rbsp_bytes = (self.header_bits / 8) as usize;
        let mut zeros = 0usize;
        let mut rbsp_seen = 0usize;
        let mut escaped_seen = 0usize;
        for &b in escaped_payload {
            escaped_seen += 1;
            if zeros >= 2 && b == 3 {
                zeros = 0;
                continue;
            }
            rbsp_seen += 1;
            if b == 0 {
                zeros += 1;
            } else {
                zeros = 0;
            }
            if rbsp_seen == rbsp_bytes {
                break;
            }
        }
        // 2 = the NAL unit header, which the offset is measured from.
        2 + escaped_seen
    }
}

/// Walk past a `pred_weight_table()` (7.3.6.3).
fn skip_pred_weight_table(r: &mut BitReader, h: &SliceHeader, sps: &Sps) -> Result<()> {
    r.read_ue()?; // luma_log2_weight_denom
    let chroma = sps.chroma_format_idc != 0;
    if chroma {
        r.read_se()?; // delta_chroma_log2_weight_denom
    }
    let lists: &[u32] = if h.slice_type == SliceType::B {
        &[h.num_ref_idx_l0_active_minus1, h.num_ref_idx_l1_active_minus1]
    } else {
        &[h.num_ref_idx_l0_active_minus1]
    };
    for &active_minus1 in lists {
        let count = active_minus1 + 1;
        let mut luma_flags = Vec::with_capacity(count as usize);
        for _ in 0..count {
            luma_flags.push(r.read_bit()?);
        }
        let mut chroma_flags = Vec::with_capacity(count as usize);
        if chroma {
            for _ in 0..count {
                chroma_flags.push(r.read_bit()?);
            }
        }
        for i in 0..count as usize {
            if luma_flags[i] {
                r.read_se()?; // delta_luma_weight
                r.read_se()?; // luma_offset
            }
            if chroma && chroma_flags[i] {
                for _ in 0..2 {
                    r.read_se()?; // delta_chroma_weight
                    r.read_se()?; // delta_chroma_offset
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ps::{ConformanceWindow, ProfileTierLevel};

    fn sps() -> Sps {
        Sps {
            vps_id: 0,
            id: 0,
            chroma_format_idc: 1,
            separate_colour_plane: false,
            pic_width_in_luma_samples: 1920,
            pic_height_in_luma_samples: 1088,
            conf_win: ConformanceWindow::default(),
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
            vui: None,
        }
    }

    #[test]
    fn intra_slice_header_round_trips_with_entry_points() {
        let pps = Pps {
            entropy_coding_sync_enabled: true,
            deblocking_filter_control_present: true,
            deblocking_filter_disabled: true,
            ..Pps::default()
        };
        let sps = sps();
        let mut header = SliceHeader::intra(&pps, -3);
        header.entry_point_offsets = vec![1234, 9, 65_540];
        let mut w = BitWriter::new();
        header.write(&mut w, &sps, &pps, NalUnitType::IdrWRadl);
        let rbsp = w.into_bytes();
        let (parsed, pos) = SliceHeader::parse(&rbsp, &sps, &pps, NalUnitType::IdrWRadl).unwrap();
        assert_eq!(parsed, header);
        assert_eq!(pos.header_bits % 8, 0);
        assert_eq!(pos.header_bits as usize / 8, rbsp.len());
        // No emulation prevention in this header, so the byte offset is the
        // NAL header plus the RBSP header bytes.
        assert_eq!(pos.slice_data_byte_offset(&rbsp), 2 + rbsp.len());
    }

    #[test]
    fn byte_offset_counts_escape_bytes() {
        // RBSP 00 00 01 ... escapes to 00 00 03 01 ...; a header of four RBSP
        // bytes therefore ends five escaped bytes in.
        let pos = ParsePositions {
            header_bits: 4 * 8,
            st_rps_bits: 0,
        };
        let escaped = [0u8, 0, 3, 1, 0xff];
        assert_eq!(pos.slice_data_byte_offset(&escaped), 2 + 5);
    }
}
