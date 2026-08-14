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

/// One long-term reference picture named by a slice header (7.3.6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LongTermRef {
    /// `PocLsbLt`: the low bits of the reference's picture order count.
    pub poc_lsb_lt: u32,
    /// `UsedByCurrPicLt`: whether the current picture may predict from it.
    pub used_by_curr: bool,
    /// `delta_poc_msb_present_flag`.
    pub delta_poc_msb_present: bool,
    /// `DeltaPocMsbCycleLt`, accumulated as equation 7-52 requires.
    pub delta_poc_msb_cycle: u32,
}

/// One entry of a `pred_weight_table()` (7.3.6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WeightEntry {
    /// `(delta_luma_weight, luma_offset)`, absent when the flag was 0.
    pub luma: Option<(i32, i32)>,
    /// `(delta_chroma_weight, ChromaOffset)` per chroma component.
    pub chroma: Option<[(i32, i32); 2]>,
}

/// A parsed `pred_weight_table()` (7.3.6.3).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PredWeightTable {
    /// `luma_log2_weight_denom`.
    pub luma_log2_weight_denom: u32,
    /// `delta_chroma_log2_weight_denom`.
    pub delta_chroma_log2_weight_denom: i32,
    /// One entry per active list 0 reference.
    pub l0: Vec<WeightEntry>,
    /// One entry per active list 1 reference.
    pub l1: Vec<WeightEntry>,
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
    /// `colour_plane_id`, present only when the sequence codes its three
    /// planes as separate pictures.
    pub colour_plane_id: u8,
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
    /// The short-term reference picture set in force, whether it came from the
    /// SPS by index or was written out in this header.
    ///
    /// This and the three fields after it are what a *stateless hardware*
    /// decoder needs and a software one derives on the fly: the driver is given
    /// `RefPicSetStCurrBefore` / `StCurrAfter` / `LtCurr` as picture lists, so
    /// the caller has to turn these deltas into reference pictures itself.
    pub short_term_ref_pic_set: ShortTermRefPicSet,
    /// True when the set above came from the SPS rather than this header.
    pub short_term_ref_pic_set_sps_flag: bool,
    /// The long-term reference pictures this slice names, in bitstream order.
    pub long_term: Vec<LongTermRef>,
    /// `list_entry_l0` / `list_entry_l1`; empty when the list is not modified.
    pub list_entry: [Vec<u32>; 2],
    /// `pred_weight_table()`, when the PPS turns weighted prediction on.
    pub pred_weight_table: Option<PredWeightTable>,
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
            colour_plane_id: 0,
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
            short_term_ref_pic_set: ShortTermRefPicSet::default(),
            short_term_ref_pic_set_sps_flag: true,
            long_term: Vec::new(),
            list_entry: [Vec::new(), Vec::new()],
            pred_weight_table: None,
        }
    }

    /// Write the header, up to and including `byte_alignment()`.
    ///
    /// Entry point offsets are written from `self.entry_point_offsets`, so a WPP
    /// encoder codes its substreams first and builds the header afterwards —
    /// the offsets are not knowable any earlier.
    pub fn write(&self, w: &mut BitWriter, sps: &Sps, pps: &Pps, nal_type: NalUnitType) {
        self.write_without_alignment(w, sps, pps, nal_type);
        // byte_alignment(): a one bit then zeros.
        w.write_bit(true);
        w.align_to_byte();
    }

    /// The header without its closing `byte_alignment()`.
    ///
    /// A VA-API encoder wants exactly this: the driver is told how many bits of
    /// header the application wrote and appends the alignment itself, so
    /// including it here leaves the alignment bit sitting where the driver
    /// expects slice data (measured on radeonsi: every P picture then decodes
    /// as `alignment_bit_equal_to_one = 0`).
    pub fn write_without_alignment(
        &self,
        w: &mut BitWriter,
        sps: &Sps,
        pps: &Pps,
        nal_type: NalUnitType,
    ) {
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
                w.write_bit(self.short_term_ref_pic_set_sps_flag);
                if self.short_term_ref_pic_set_sps_flag {
                    if sps.num_short_term_ref_pic_sets > 1 {
                        w.write_bits(0, ceil_log2(sps.num_short_term_ref_pic_sets));
                    }
                } else {
                    // Written out here, which is what a stream whose SPS carries
                    // no sets must do — and what a hardware encoder's driver
                    // does for its own single reference.
                    self.short_term_ref_pic_set
                        .write(w, sps.num_short_term_ref_pic_sets);
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
                h.colour_plane_id = r.read_bits(2)? as u8;
            }
            let mut num_long_term = 0;
            if !nal_type.is_idr() {
                h.poc_lsb = r.read_bits(sps.log2_max_poc_lsb_minus4 + 4)?;
                let from_sps = r.read_bit()?;
                h.short_term_ref_pic_set_sps_flag = from_sps;
                let before = r.bit_position();
                if !from_sps {
                    // Written out here, possibly predicted from the SPS sets —
                    // hence `num_short_term_ref_pic_sets` as this set's index
                    // and the SPS sets as the prediction source (7.3.7).
                    h.short_term_ref_pic_set = ShortTermRefPicSet::parse(
                        &mut r,
                        sps.num_short_term_ref_pic_sets,
                        &sps.short_term_ref_pic_sets,
                    )?;
                } else {
                    let idx = if sps.num_short_term_ref_pic_sets > 1 {
                        r.read_bits(ceil_log2(sps.num_short_term_ref_pic_sets))?
                    } else {
                        0
                    };
                    h.short_term_ref_pic_set = sps
                        .short_term_ref_pic_sets
                        .get(idx as usize)
                        .copied()
                        .ok_or_else(|| {
                            Error::corrupt(format!(
                                "HEVC slice: short_term_ref_pic_set_idx {idx} is not in the SPS"
                            ))
                        })?;
                }
                // Only a set written *in the header* costs the driver bits to
                // skip; for an SPS-indexed one libva wants zero.
                pos.st_rps_bits = if from_sps {
                    0
                } else {
                    (r.bit_position() - before) as u32
                };
                if sps.long_term_ref_pics_present {
                    let num_long_term_sps = if sps.num_long_term_ref_pics_sps > 0 {
                        r.read_ue()?
                    } else {
                        0
                    };
                    let num_long_term_pics = r.read_ue()?;
                    let mut prev_delta_msb = 0u32;
                    for i in 0..num_long_term_sps + num_long_term_pics {
                        let mut entry = LongTermRef::default();
                        if i < num_long_term_sps {
                            let idx = if sps.num_long_term_ref_pics_sps > 1 {
                                r.read_bits(ceil_log2(sps.num_long_term_ref_pics_sps))?
                            } else {
                                0
                            };
                            let (poc_lsb_lt, used) = sps
                                .long_term_ref_pics_sps
                                .get(idx as usize)
                                .copied()
                                .unwrap_or((0, false));
                            entry.poc_lsb_lt = poc_lsb_lt;
                            entry.used_by_curr = used;
                        } else {
                            entry.poc_lsb_lt = r.read_bits(sps.log2_max_poc_lsb_minus4 + 4)?;
                            entry.used_by_curr = r.read_bit()?;
                        }
                        if entry.used_by_curr {
                            num_long_term += 1;
                        }
                        entry.delta_poc_msb_present = r.read_bit()?;
                        if entry.delta_poc_msb_present {
                            // 7-52: coded as a delta against the previous entry,
                            // except for the first of each kind.
                            let delta = r.read_ue()?;
                            entry.delta_poc_msb_cycle = if i == 0 || i == num_long_term_sps {
                                delta
                            } else {
                                delta + prev_delta_msb
                            };
                            prev_delta_msb = entry.delta_poc_msb_cycle;
                        }
                        h.long_term.push(entry);
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
                let num_pic_total_curr = h.short_term_ref_pic_set.num_used_by_curr + num_long_term;
                if pps.lists_modification_present && num_pic_total_curr > 1 {
                    let bits = ceil_log2(num_pic_total_curr);
                    if r.read_bit()? {
                        for _ in 0..=h.num_ref_idx_l0_active_minus1 {
                            let entry = r.read_bits(bits)?;
                            h.list_entry[0].push(entry);
                        }
                    }
                    if h.slice_type == SliceType::B && r.read_bit()? {
                        for _ in 0..=h.num_ref_idx_l1_active_minus1 {
                            let entry = r.read_bits(bits)?;
                            h.list_entry[1].push(entry);
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
                    h.pred_weight_table = Some(parse_pred_weight_table(&mut r, &h, sps)?);
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

/// How many emulation prevention bytes a NAL unit payload carries.
///
/// `VASliceParameterBufferHEVC::slice_data_num_emu_prevn_bytes` wants this for
/// the slice data a driver is handed, and a caller that has the escaped bytes
/// should not have to unescape them twice to find out.
pub fn count_emulation_prevention_bytes(escaped_payload: &[u8]) -> usize {
    let mut zeros = 0usize;
    let mut count = 0usize;
    for &b in escaped_payload {
        if zeros >= 2 && b == 3 {
            count += 1;
            zeros = 0;
            continue;
        }
        if b == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
    }
    count
}

/// Parse a `pred_weight_table()` (7.3.6.3).
///
/// The chroma *offset* is derived, not coded: equation 7-56 turns
/// `delta_chroma_offset_lX` into `ChromaOffsetLX` against the weight, and a
/// decoder that forwards the raw delta to hardware shifts every weighted
/// chroma sample.
fn parse_pred_weight_table(
    r: &mut BitReader,
    h: &SliceHeader,
    sps: &Sps,
) -> Result<PredWeightTable> {
    let mut table = PredWeightTable {
        luma_log2_weight_denom: r.read_ue()?,
        ..PredWeightTable::default()
    };
    let chroma = sps.chroma_format_idc != 0;
    if chroma {
        table.delta_chroma_log2_weight_denom = r.read_se()?;
    }
    let chroma_log2 = table.luma_log2_weight_denom as i32 + table.delta_chroma_log2_weight_denom;
    if !(0..=7).contains(&chroma_log2) {
        return Err(Error::corrupt(
            "HEVC pred_weight_table: ChromaLog2WeightDenom out of range",
        ));
    }
    // A fuzzed SPS can name any bit depth; the shift below has to survive it.
    let half_range = 1i64 << (sps.bit_depth_chroma_minus8.min(8) + 7);

    let lists: &[u32] = if h.slice_type == SliceType::B {
        &[
            h.num_ref_idx_l0_active_minus1,
            h.num_ref_idx_l1_active_minus1,
        ]
    } else {
        &[h.num_ref_idx_l0_active_minus1]
    };
    for (list, &active_minus1) in lists.iter().enumerate() {
        let count = (active_minus1 + 1) as usize;
        let mut luma_flags = Vec::with_capacity(count);
        for _ in 0..count {
            luma_flags.push(r.read_bit()?);
        }
        let mut chroma_flags = vec![false; count];
        if chroma {
            for flag in chroma_flags.iter_mut() {
                *flag = r.read_bit()?;
            }
        }
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let mut entry = WeightEntry::default();
            if luma_flags[i] {
                let delta_weight = r.read_se()?;
                let offset = r.read_se()?;
                entry.luma = Some((delta_weight, offset));
            }
            if chroma && chroma_flags[i] {
                let mut pair = [(0i32, 0i32); 2];
                for component in &mut pair {
                    let delta_weight = r.read_se()?;
                    let delta_offset = r.read_se()?;
                    let weight = (1i64 << chroma_log2) + i64::from(delta_weight);
                    // 7-56, in 64 bits because both terms come straight off the
                    // wire and a corrupt stream must not overflow the derivation.
                    let offset = (half_range + i64::from(delta_offset)
                        - ((half_range * weight) >> chroma_log2))
                        .clamp(-half_range, half_range - 1);
                    *component = (delta_weight, offset as i32);
                }
                entry.chroma = Some(pair);
            }
            entries.push(entry);
        }
        if list == 0 {
            table.l0 = entries;
        } else {
            table.l1 = entries;
        }
    }
    Ok(table)
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
            pcm: None,
            num_short_term_ref_pic_sets: 0,
            short_term_ref_pic_sets: Vec::new(),
            long_term_ref_pics_present: false,
            num_long_term_ref_pics_sps: 0,
            long_term_ref_pics_sps: Vec::new(),
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
    fn emulation_prevention_bytes_are_counted() {
        assert_eq!(count_emulation_prevention_bytes(&[0, 0, 3, 1, 0xff]), 1);
        assert_eq!(count_emulation_prevention_bytes(&[0, 0, 3, 0, 0, 3, 2]), 2);
        // A 3 that is not preceded by two zeros is payload, not an escape.
        assert_eq!(count_emulation_prevention_bytes(&[0, 3, 3, 3]), 0);
        assert_eq!(count_emulation_prevention_bytes(&[]), 0);
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
