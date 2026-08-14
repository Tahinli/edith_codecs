//! Slice data decoding (Rec. ITU-T H.264 clauses 7.3.4, 7.3.5 and 8.3 to 8.5):
//! the macroblock layer, and the reconstruction of the samples it codes.
//!
//! This release decodes I and IDR slices coded with CAVLC. Anything else —
//! CABAC, inter prediction, 8x8 transforms, interlace — is refused by name
//! through [`ec_core::Error::Unsupported`] rather than approximated.

// Clippy's needless_range_loop asks for iterators where this file
// transcribes the specification's own `for i` / `for j` formulas; the
// index is the point.
#![allow(clippy::needless_range_loop)]

use ec_core::bitio::BitReader;
use ec_core::error::{Error, Result};
use ec_h264_syntax::nal::RbspReader;
use ec_h264_syntax::pps::PicParameterSet;
use ec_h264_syntax::slice::{SliceHeader, SliceType};
use ec_h264_syntax::sps::SequenceParameterSet;

use crate::cavlc::residual_block_cavlc;
use crate::intra::{
    Intra4x4PredMode, Intra16x16PredMode, IntraChromaPredMode, Neighbours4x4, predict_4x4,
    predict_16x16, predict_chroma_8x8,
};
use crate::picture::{MbInfo, MbKind, Picture};
use crate::tables::{CODED_BLOCK_PATTERN_CHROMA, LUMA_4X4_BLOCK_XY, qpc_from_qpi};
use crate::transform::{
    clip1, inverse_chroma_dc, inverse_luma_dc, inverse_scan_4x4, inverse_transform_4x4, scale_4x4,
};

/// Flat weight matrix: every entry of `weightScale4x4` is 16 when no scaling
/// list is in force (clause 8.5.9).
const FLAT_WEIGHT_SCALE: i32 = 16;

/// `mb_type` 25, `I_PCM` (Table 7-11).
const MB_TYPE_I_PCM: u32 = 25;

/// The transform coefficient levels of one macroblock, in the arrays clause
/// 7.3.5.3 names them by.
#[derive(Debug, Default)]
struct MacroblockResidual {
    /// `i16x16DClevel[0..15]`.
    i16x16_dc_level: [i32; 16],
    /// `level4x4[blk][0..15]`, or `i16x16AClevel[blk]` stored at positions
    /// 1..15 so that both kinds of block share one inverse scan.
    luma_level: [[i32; 16]; 16],
    /// `ChromaDCLevel[iCbCr][0..3]`.
    chroma_dc_level: [[i32; 4]; 2],
    /// `ChromaACLevel[iCbCr][blk]`, again stored at positions 1..15.
    chroma_ac_level: [[[i32; 16]; 4]; 2],
}

/// Decoding state for one slice.
pub struct SliceDecoder<'a> {
    pps: &'a PicParameterSet,
    header: &'a SliceHeader,
    picture: &'a mut Picture,
    /// Index of this slice within its picture; the availability rules of
    /// clause 6.4.8 compare against it.
    slice_id: i32,
    /// `QPY,PREV`: the quantisation parameter of the previous macroblock.
    qpy: i32,
}

impl<'a> SliceDecoder<'a> {
    /// Prepare to decode `header`'s slice into `picture`.
    ///
    /// Refuses, by name, every coding tool outside this release's scope, so
    /// that a stream is either decoded exactly or not at all.
    pub fn new(
        sps: &'a SequenceParameterSet,
        pps: &'a PicParameterSet,
        header: &'a SliceHeader,
        picture: &'a mut Picture,
        slice_id: i32,
    ) -> Result<SliceDecoder<'a>> {
        if !header.slice_type.is_intra() {
            return Err(Error::unsupported(
                format!("H.264 {:?} slice", header.slice_type),
                "inter prediction and the decoded picture buffer arrive in a later release",
            ));
        }
        if header.slice_type == SliceType::Si {
            return Err(Error::unsupported(
                "H.264 SI slice",
                "switching slices are Extended profile only",
            ));
        }
        if pps.entropy_coding_mode_flag {
            return Err(Error::unsupported(
                "H.264 CABAC entropy coding",
                "this release decodes CAVLC (entropy_coding_mode_flag 0) only",
            ));
        }
        if sps.chroma_array_type() != 1 {
            return Err(Error::unsupported(
                format!("H.264 chroma_format_idc {}", sps.chroma_format_idc),
                "only 4:2:0 (ChromaArrayType 1) is decoded",
            ));
        }
        if sps.bit_depth_luma_minus8 != 0 || sps.bit_depth_chroma_minus8 != 0 {
            return Err(Error::unsupported(
                format!("H.264 {}-bit samples", sps.bit_depth_y()),
                "only 8-bit sample depth is decoded",
            ));
        }
        if !sps.frame_mbs_only_flag || header.field_pic_flag {
            return Err(Error::unsupported(
                "H.264 field or MBAFF coding",
                "only frame_mbs_only_flag 1 progressive pictures are decoded",
            ));
        }
        if sps.seq_scaling_matrix_present_flag || pps.pic_scaling_matrix_present_flag {
            return Err(Error::unsupported(
                "H.264 scaling matrices",
                "only the flat weightScale4x4 of 16 is applied",
            ));
        }
        if sps.qpprime_y_zero_transform_bypass_flag {
            return Err(Error::unsupported(
                "H.264 lossless transform bypass",
                "qpprime_y_zero_transform_bypass_flag 1 is not implemented",
            ));
        }
        if pps.num_slice_groups_minus1 > 0 {
            return Err(Error::unsupported(
                "H.264 slice groups (FMO)",
                "only one slice group per picture is decoded",
            ));
        }
        if pps.transform_8x8_mode_flag {
            return Err(Error::unsupported(
                "H.264 8x8 transform",
                "transform_8x8_mode_flag 1 needs the 8x8 intra prediction and transform",
            ));
        }
        let qpy = header.slice_qp_y(pps);
        Ok(SliceDecoder {
            pps,
            header,
            picture,
            slice_id,
            qpy,
        })
    }

    /// `slice_data()` (clause 7.3.4): every macroblock of the slice.
    pub fn decode_slice_data(&mut self, rr: &mut RbspReader<'_>) -> Result<()> {
        let total_mbs = self.picture.width_mbs * self.picture.height_mbs;
        let mut curr_mb_addr = self.header.first_mb_in_slice as usize;
        if curr_mb_addr >= total_mbs {
            return Err(Error::corrupt(format!(
                "H.264 slice: first_mb_in_slice {curr_mb_addr} is past the picture"
            )));
        }
        loop {
            self.decode_macroblock(rr, curr_mb_addr)?;
            curr_mb_addr += 1;
            if !rr.more_rbsp_data() {
                return Ok(());
            }
            if curr_mb_addr >= total_mbs {
                return Err(Error::corrupt(
                    "H.264 slice: more macroblocks than the picture holds",
                ));
            }
        }
    }

    /// `macroblock_layer()` (clause 7.3.5), followed by the reconstruction of
    /// clauses 8.3 to 8.5 for that macroblock.
    fn decode_macroblock(&mut self, rr: &mut RbspReader<'_>, mb_addr: usize) -> Result<()> {
        let mb_x = mb_addr % self.picture.width_mbs;
        let mb_y = mb_addr / self.picture.width_mbs;
        // The macroblock joins the slice before it is decoded: clause 6.4.8
        // availability of the current macroblock to itself is what makes the
        // neighbouring 4x4 blocks inside it visible to intra prediction.
        self.picture.mb[mb_addr] = MbInfo {
            slice_id: self.slice_id,
            kind: MbKind::Intra4x4,
            qpy: self.qpy,
            disable_deblocking_filter_idc: self.header.disable_deblocking_filter_idc,
            filter_offset_a: self.header.slice_alpha_c0_offset_div2 << 1,
            filter_offset_b: self.header.slice_beta_offset_div2 << 1,
        };

        let mb_type = rr.bits().read_ue()?;
        if mb_type == MB_TYPE_I_PCM {
            return self.decode_i_pcm(rr, mb_addr, mb_x, mb_y);
        }
        if mb_type > MB_TYPE_I_PCM {
            return Err(Error::corrupt(format!(
                "H.264 I slice: mb_type = {mb_type} (Table 7-11 stops at 25)"
            )));
        }

        // Table 7-11: mb_type 0 is I_NxN, 1..=24 are the I_16x16 variants and
        // carry their coded block pattern and prediction mode in the type.
        let intra_16x16 = mb_type > 0;
        let (mut cbp_luma, mut cbp_chroma, intra16x16_pred_mode) = if intra_16x16 {
            let t = mb_type - 1;
            let (t, cbp_luma) = if t < 12 { (t, 0) } else { (t - 12, 15) };
            (
                cbp_luma,
                t / 4,
                Some(Intra16x16PredMode::from_value(t % 4)?),
            )
        } else {
            (0, 0, None)
        };

        // mb_pred(), clause 7.3.5.1.
        let mut intra4x4_pred_mode = [0i8; 16];
        if !intra_16x16 {
            for blk_idx in 0..16 {
                let (bx, by) = LUMA_4X4_BLOCK_XY[blk_idx];
                let (x, y) = (mb_x * 16 + bx, mb_y * 16 + by);
                let predicted = self.predicted_intra4x4_pred_mode(x, y);
                let mode = if rr.bits().read_bit()? {
                    // prev_intra4x4_pred_mode_flag
                    predicted
                } else {
                    let rem = rr.bits().read_bits(3)? as i32;
                    if rem < predicted { rem } else { rem + 1 }
                };
                intra4x4_pred_mode[blk_idx] = mode as i8;
                let index = self.picture.luma_blk_index(x, y);
                self.picture.intra4x4_pred_mode[index] = mode as i8;
            }
        }
        let intra_chroma_pred_mode = IntraChromaPredMode::from_value(rr.bits().read_ue()?)?;

        if !intra_16x16 {
            // coded_block_pattern is me(v): Table 9-4's intra column.
            let code_num = rr.bits().read_ue()? as usize;
            let cbp = *CODED_BLOCK_PATTERN_CHROMA.get(code_num).ok_or_else(|| {
                Error::corrupt(format!(
                    "H.264: coded_block_pattern codeNum {code_num} is outside Table 9-4"
                ))
            })?;
            cbp_luma = (cbp.0 % 16) as u32;
            cbp_chroma = (cbp.0 / 16) as u32;
        }

        let mut residual = MacroblockResidual::default();
        if cbp_luma > 0 || cbp_chroma > 0 || intra_16x16 {
            let mb_qp_delta = rr.bits().read_se()?;
            if !(-26..=25).contains(&mb_qp_delta) {
                return Err(Error::corrupt(format!(
                    "H.264: mb_qp_delta = {mb_qp_delta} outside -26..=25"
                )));
            }
            // Clause 7.4.5: QPY wraps within 0..51 for 8-bit sample depth.
            self.qpy = (self.qpy + mb_qp_delta + 52).rem_euclid(52);
            self.picture.mb[mb_addr].qpy = self.qpy;
            self.parse_residual(
                rr.bits(),
                &mut residual,
                mb_x,
                mb_y,
                intra_16x16,
                cbp_luma,
                cbp_chroma,
            )?;
        }

        self.picture.mb[mb_addr].kind = if intra_16x16 {
            MbKind::Intra16x16
        } else {
            MbKind::Intra4x4
        };

        // Reconstruction, clauses 8.3 to 8.5.
        match intra16x16_pred_mode {
            Some(mode) => self.reconstruct_intra_16x16(mb_x, mb_y, mode, &residual, cbp_luma)?,
            None => {
                self.reconstruct_intra_4x4(mb_x, mb_y, &intra4x4_pred_mode, &residual, cbp_luma)?
            }
        }
        self.reconstruct_chroma(mb_x, mb_y, intra_chroma_pred_mode, &residual, cbp_chroma)?;
        Ok(())
    }

    /// The `I_PCM` branch of clause 7.3.5: raw samples, byte aligned.
    fn decode_i_pcm(
        &mut self,
        rr: &mut RbspReader<'_>,
        mb_addr: usize,
        mb_x: usize,
        mb_y: usize,
    ) -> Result<()> {
        let r = rr.bits();
        while !r.is_byte_aligned() {
            // pcm_alignment_zero_bit
            if r.read_bit()? {
                return Err(Error::corrupt(
                    "H.264 I_PCM: pcm_alignment_zero_bit is not zero",
                ));
            }
        }
        for i in 0..256usize {
            let sample = r.read_bits(8)? as u8;
            self.picture
                .set_luma(mb_x * 16 + i % 16, mb_y * 16 + i / 16, sample);
        }
        for i_cb_cr in 0..2usize {
            for i in 0..64usize {
                let sample = r.read_bits(8)? as u8;
                self.picture
                    .set_chroma(i_cb_cr, mb_x * 8 + i % 8, mb_y * 8 + i / 8, sample);
            }
        }
        // Clause 9.2.1: an I_PCM neighbour predicts nC = 16, and clause 8.7.2
        // filters it as if its QPY were 0.
        for blk_idx in 0..16 {
            let (bx, by) = LUMA_4X4_BLOCK_XY[blk_idx];
            let index = self.picture.luma_blk_index(mb_x * 16 + bx, mb_y * 16 + by);
            self.picture.total_coeff_luma[index] = 16;
            self.picture.constructed[index] = true;
            self.picture.intra4x4_pred_mode[index] = -1;
        }
        for blk in 0..4usize {
            let index = self
                .picture
                .chroma_blk_index(mb_x * 8 + (blk % 2) * 4, mb_y * 8 + (blk / 2) * 4);
            self.picture.total_coeff_cb[index] = 16;
            self.picture.total_coeff_cr[index] = 16;
        }
        self.picture.mb[mb_addr].kind = MbKind::IPcm;
        self.picture.mb[mb_addr].qpy = 0;
        Ok(())
    }

    /// `residual(0, 15)` (clause 7.3.5.3) for an intra macroblock.
    #[allow(clippy::too_many_arguments)]
    fn parse_residual(
        &mut self,
        r: &mut BitReader<'_>,
        residual: &mut MacroblockResidual,
        mb_x: usize,
        mb_y: usize,
        intra_16x16: bool,
        cbp_luma: u32,
        cbp_chroma: u32,
    ) -> Result<()> {
        // residual_luma(): the DC block of an Intra_16x16 macroblock first.
        if intra_16x16 {
            let nc = self.nc_luma(mb_x * 16, mb_y * 16);
            residual_block_cavlc(r, &mut residual.i16x16_dc_level, 0, 15, 16, nc)?;
        }
        for i8x8 in 0..4usize {
            for i4x4 in 0..4usize {
                let blk_idx = i8x8 * 4 + i4x4;
                let (bx, by) = LUMA_4X4_BLOCK_XY[blk_idx];
                let (x, y) = (mb_x * 16 + bx, mb_y * 16 + by);
                let total_coeff = if cbp_luma & (1 << i8x8) != 0 {
                    let nc = self.nc_luma(x, y);
                    let level = &mut residual.luma_level[blk_idx];
                    if intra_16x16 {
                        residual_block_cavlc(r, level, 1, 15, 15, nc)?
                    } else {
                        residual_block_cavlc(r, level, 0, 15, 16, nc)?
                    }
                } else {
                    0
                };
                let index = self.picture.luma_blk_index(x, y);
                self.picture.total_coeff_luma[index] = total_coeff;
            }
        }

        // The chroma DC blocks, then the chroma AC blocks (clause 7.3.5.3 with
        // NumC8x8 = 1 for 4:2:0).
        for i_cb_cr in 0..2usize {
            if cbp_chroma & 3 != 0 {
                residual_block_cavlc(r, &mut residual.chroma_dc_level[i_cb_cr], 0, 3, 4, -1)?;
            }
        }
        for i_cb_cr in 0..2usize {
            for blk in 0..4usize {
                let (x, y) = (mb_x * 8 + (blk % 2) * 4, mb_y * 8 + (blk / 2) * 4);
                let total_coeff = if cbp_chroma & 2 != 0 {
                    let nc = self.nc_chroma(i_cb_cr, x, y);
                    residual_block_cavlc(
                        r,
                        &mut residual.chroma_ac_level[i_cb_cr][blk],
                        1,
                        15,
                        15,
                        nc,
                    )?
                } else {
                    0
                };
                let index = self.picture.chroma_blk_index(x, y);
                if i_cb_cr == 0 {
                    self.picture.total_coeff_cb[index] = total_coeff;
                } else {
                    self.picture.total_coeff_cr[index] = total_coeff;
                }
            }
        }
        Ok(())
    }

    /// `nC` for a luma block at luma location `(x, y)` (clause 9.2.1).
    fn nc_luma(&self, x: usize, y: usize) -> i32 {
        let (x, y) = (x as isize, y as isize);
        let a = self.neighbour_total_coeff_luma(x - 1, y);
        let b = self.neighbour_total_coeff_luma(x, y - 1);
        combine_nc(a, b)
    }

    /// `nC` for a chroma AC block at chroma location `(x, y)`.
    fn nc_chroma(&self, i_cb_cr: usize, x: usize, y: usize) -> i32 {
        let (x, y) = (x as isize, y as isize);
        let a = self.neighbour_total_coeff_chroma(i_cb_cr, x - 1, y);
        let b = self.neighbour_total_coeff_chroma(i_cb_cr, x, y - 1);
        combine_nc(a, b)
    }

    fn neighbour_total_coeff_luma(&self, x: isize, y: isize) -> Option<i32> {
        if !self.picture.mb_available_at(x, y, self.slice_id) {
            return None;
        }
        let (x, y) = (x as usize, y as usize);
        if self.picture.mb_at(x / 16, y / 16).kind == MbKind::IPcm {
            return Some(16);
        }
        Some(self.picture.total_coeff_luma[self.picture.luma_blk_index(x, y)] as i32)
    }

    fn neighbour_total_coeff_chroma(&self, i_cb_cr: usize, x: isize, y: isize) -> Option<i32> {
        // Chroma locations map to luma locations for the availability test.
        if !self.picture.mb_available_at(x * 2, y * 2, self.slice_id) {
            return None;
        }
        let (x, y) = (x as usize, y as usize);
        if self.picture.mb_at(x / 8, y / 8).kind == MbKind::IPcm {
            return Some(16);
        }
        let index = self.picture.chroma_blk_index(x, y);
        Some(if i_cb_cr == 0 {
            self.picture.total_coeff_cb[index] as i32
        } else {
            self.picture.total_coeff_cr[index] as i32
        })
    }

    /// `predIntra4x4PredMode` (clause 8.3.1.1) for the block at luma `(x, y)`.
    fn predicted_intra4x4_pred_mode(&self, x: usize, y: usize) -> i32 {
        let (xi, yi) = (x as isize, y as isize);
        let a_available = self.picture.mb_available_at(xi - 1, yi, self.slice_id);
        let b_available = self.picture.mb_available_at(xi, yi - 1, self.slice_id);
        // dcPredModePredictedFlag: one missing neighbour forces DC for both.
        if !a_available || !b_available {
            return 2;
        }
        let mode_of = |x: usize, y: usize| -> i32 {
            let mode = self.picture.intra4x4_pred_mode[self.picture.luma_blk_index(x, y)];
            // A neighbour that is not Intra_4x4 (or Intra_8x8) predicts DC.
            if mode < 0 { 2 } else { mode as i32 }
        };
        mode_of(x - 1, y).min(mode_of(x, y - 1))
    }

    /// Clause 8.3.1: `Intra_4x4` prediction and reconstruction, block by block
    /// in the decoding order of clause 6.4.3.
    fn reconstruct_intra_4x4(
        &mut self,
        mb_x: usize,
        mb_y: usize,
        modes: &[i8; 16],
        residual: &MacroblockResidual,
        cbp_luma: u32,
    ) -> Result<()> {
        for blk_idx in 0..16usize {
            let (bx, by) = LUMA_4X4_BLOCK_XY[blk_idx];
            let (x, y) = (mb_x * 16 + bx, mb_y * 16 + by);
            let neighbours = self.luma_neighbours_4x4(x, y);
            let mode = Intra4x4PredMode::from_value(modes[blk_idx] as i32)?;
            let pred = predict_4x4(mode, &neighbours)?;
            let r = if cbp_luma & (1 << (blk_idx / 4)) != 0 {
                let c = inverse_scan_4x4(&residual.luma_level[blk_idx]);
                let d = scale_4x4(&c, self.qpy, FLAT_WEIGHT_SCALE, None);
                inverse_transform_4x4(&d)
            } else {
                [[0; 4]; 4]
            };
            for i in 0..4 {
                for j in 0..4 {
                    self.picture
                        .set_luma(x + j, y + i, clip1(pred[i][j] as i32 + r[i][j]));
                }
            }
            self.picture.mark_constructed(x, y);
        }
        Ok(())
    }

    /// Clause 8.3.3 and 8.5.1: `Intra_16x16` prediction and reconstruction.
    fn reconstruct_intra_16x16(
        &mut self,
        mb_x: usize,
        mb_y: usize,
        mode: Intra16x16PredMode,
        residual: &MacroblockResidual,
        cbp_luma: u32,
    ) -> Result<()> {
        let (x0, y0) = (mb_x * 16, mb_y * 16);
        let top = self.luma_row_above(x0, y0, 16).map(|row| {
            let mut a = [0u8; 16];
            a.copy_from_slice(&row);
            a
        });
        let left = self.luma_column_left(x0, y0, 16).map(|col| {
            let mut a = [0u8; 16];
            a.copy_from_slice(&col);
            a
        });
        let corner = self.luma_corner(x0, y0);
        let pred = predict_16x16(mode, top.as_ref(), left.as_ref(), corner)?;

        // 8.5.10: the DC coefficients of the sixteen blocks, transformed and
        // scaled together.
        let dc = inverse_luma_dc(
            &inverse_scan_4x4(&residual.i16x16_dc_level),
            self.qpy,
            FLAT_WEIGHT_SCALE,
        );
        for blk_idx in 0..16usize {
            let (bx, by) = LUMA_4X4_BLOCK_XY[blk_idx];
            let (x, y) = (x0 + bx, y0 + by);
            let c = if cbp_luma & (1 << (blk_idx / 4)) != 0 {
                inverse_scan_4x4(&residual.luma_level[blk_idx])
            } else {
                [[0; 4]; 4]
            };
            let d = scale_4x4(&c, self.qpy, FLAT_WEIGHT_SCALE, Some(dc[by / 4][bx / 4]));
            let r = inverse_transform_4x4(&d);
            for i in 0..4 {
                for j in 0..4 {
                    self.picture.set_luma(
                        x + j,
                        y + i,
                        clip1(pred[by + i][bx + j] as i32 + r[i][j]),
                    );
                }
            }
            self.picture.mark_constructed(x, y);
        }
        Ok(())
    }

    /// Clause 8.3.4 and 8.5.4: chroma prediction and reconstruction for both
    /// components of a 4:2:0 macroblock.
    fn reconstruct_chroma(
        &mut self,
        mb_x: usize,
        mb_y: usize,
        mode: IntraChromaPredMode,
        residual: &MacroblockResidual,
        cbp_chroma: u32,
    ) -> Result<()> {
        let (x0, y0) = (mb_x * 8, mb_y * 8);
        for i_cb_cr in 0..2usize {
            let top = self.chroma_row_above(i_cb_cr, x0, y0);
            let left = self.chroma_column_left(i_cb_cr, x0, y0);
            let corner = self.chroma_corner(i_cb_cr, x0, y0);
            let pred = predict_chroma_8x8(mode, top.as_ref(), left.as_ref(), corner)?;

            let qp = self.chroma_qp(i_cb_cr);
            // 8.5.11: the four DC coefficients as a 2x2 block, in raster order.
            let level = residual.chroma_dc_level[i_cb_cr];
            let dc = if cbp_chroma & 3 != 0 {
                inverse_chroma_dc(
                    &[[level[0], level[1]], [level[2], level[3]]],
                    qp,
                    FLAT_WEIGHT_SCALE,
                )
            } else {
                [[0; 2]; 2]
            };
            for blk in 0..4usize {
                let (bx, by) = ((blk % 2) * 4, (blk / 2) * 4);
                let c = if cbp_chroma & 2 != 0 {
                    inverse_scan_4x4(&residual.chroma_ac_level[i_cb_cr][blk])
                } else {
                    [[0; 4]; 4]
                };
                let d = scale_4x4(&c, qp, FLAT_WEIGHT_SCALE, Some(dc[by / 4][bx / 4]));
                let r = inverse_transform_4x4(&d);
                for i in 0..4 {
                    for j in 0..4 {
                        self.picture.set_chroma(
                            i_cb_cr,
                            x0 + bx + j,
                            y0 + by + i,
                            clip1(pred[by + i][bx + j] as i32 + r[i][j]),
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// `QP'C` (clause 8.5.8) for component `i_cb_cr`, from the macroblock's
    /// `QPY` and the picture's chroma offsets.
    fn chroma_qp(&self, i_cb_cr: usize) -> i32 {
        let offset = if i_cb_cr == 0 {
            self.pps.chroma_qp_index_offset
        } else {
            self.pps.second_chroma_qp_index_offset
        };
        qpc_from_qpi((self.qpy + offset).clamp(0, 51))
    }

    /// The neighbouring samples of the 4x4 luma block at `(x, y)`, with the
    /// availability rules of clause 8.3.1.2 applied.
    fn luma_neighbours_4x4(&self, x: usize, y: usize) -> Neighbours4x4 {
        Neighbours4x4 {
            left: self
                .luma_column_left(x, y, 4)
                .map(|c| [c[0], c[1], c[2], c[3]]),
            top: self
                .luma_row_above(x, y, 4)
                .map(|c| [c[0], c[1], c[2], c[3]]),
            top_right: self
                .luma_row_above(x + 4, y, 4)
                .map(|c| [c[0], c[1], c[2], c[3]]),
            corner: self.luma_corner(x, y),
        }
    }

    fn luma_row_above(&self, x: usize, y: usize, len: usize) -> Option<Vec<u8>> {
        let yi = y as isize - 1;
        // A run of samples above a block never crosses a macroblock boundary
        // horizontally within the block itself, so one availability test at the
        // first sample answers for all of them.
        for i in 0..len {
            if !self
                .picture
                .luma_sample_available((x + i) as isize, yi, self.slice_id)
            {
                return None;
            }
        }
        Some(
            (0..len)
                .map(|i| self.picture.luma_at(x + i, y - 1))
                .collect(),
        )
    }

    fn luma_column_left(&self, x: usize, y: usize, len: usize) -> Option<Vec<u8>> {
        let xi = x as isize - 1;
        for i in 0..len {
            if !self
                .picture
                .luma_sample_available(xi, (y + i) as isize, self.slice_id)
            {
                return None;
            }
        }
        Some(
            (0..len)
                .map(|i| self.picture.luma_at(x - 1, y + i))
                .collect(),
        )
    }

    fn luma_corner(&self, x: usize, y: usize) -> Option<u8> {
        let (xi, yi) = (x as isize - 1, y as isize - 1);
        self.picture
            .luma_sample_available(xi, yi, self.slice_id)
            .then(|| self.picture.luma_at(x - 1, y - 1))
    }

    fn chroma_row_above(&self, i_cb_cr: usize, x: usize, y: usize) -> Option<[u8; 8]> {
        let yi = y as isize * 2 - 1;
        if !self
            .picture
            .mb_available_at(x as isize * 2, yi, self.slice_id)
        {
            return None;
        }
        Some(std::array::from_fn(|i| {
            self.picture.chroma_at(i_cb_cr, x + i, y - 1)
        }))
    }

    fn chroma_column_left(&self, i_cb_cr: usize, x: usize, y: usize) -> Option<[u8; 8]> {
        let xi = x as isize * 2 - 1;
        if !self
            .picture
            .mb_available_at(xi, y as isize * 2, self.slice_id)
        {
            return None;
        }
        Some(std::array::from_fn(|i| {
            self.picture.chroma_at(i_cb_cr, x - 1, y + i)
        }))
    }

    fn chroma_corner(&self, i_cb_cr: usize, x: usize, y: usize) -> Option<u8> {
        let (xi, yi) = (x as isize * 2 - 1, y as isize * 2 - 1);
        self.picture
            .mb_available_at(xi, yi, self.slice_id)
            .then(|| self.picture.chroma_at(i_cb_cr, x - 1, y - 1))
    }
}

/// Clause 9.2.1: combine the neighbouring coefficient counts into `nC`.
fn combine_nc(a: Option<i32>, b: Option<i32>) -> i32 {
    match (a, b) {
        (Some(a), Some(b)) => (a + b + 1) >> 1,
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nc_averages_available_neighbours_only() {
        assert_eq!(combine_nc(Some(3), Some(4)), 4, "(3 + 4 + 1) >> 1");
        assert_eq!(combine_nc(Some(3), None), 3);
        assert_eq!(combine_nc(None, Some(7)), 7);
        assert_eq!(combine_nc(None, None), 0);
    }
}
