//! One writing surface over both entropy coders, mirroring
//! [`crate::entropy::Entropy`] on the decoding side.
//!
//! The macroblock coder writes syntax elements, not bits: which coder is in
//! force changes the bins, the contexts and — for skipped macroblocks — the
//! shape of the slice data itself (`mb_skip_run` against `mb_skip_flag`), and
//! none of that belongs in the mode decision.

use ec_core::BitWriter;

use crate::entropy::{BlockCat, MbCtx};

use super::cabac_enc::CabacEnc;
use super::vlc::{write_cbp, write_residual_block};

pub(crate) enum EncEntropy {
    Cavlc {
        w: BitWriter,
        /// Macroblocks skipped since the last coded one (7.3.4).
        skip_run: u32,
    },
    Cabac(Box<CabacEnc>),
}

impl EncEntropy {
    /// A CAVLC writer continuing after the slice header.
    pub(crate) fn cavlc(w: BitWriter) -> EncEntropy {
        EncEntropy::Cavlc { w, skip_run: 0 }
    }

    /// A CABAC writer: `cabac_alignment_one_bit`s to the byte boundary, then
    /// the context and engine initialisation of 9.3.1.
    pub(crate) fn cabac(mut w: BitWriter, slice_qp: i32, init_column: usize) -> EncEntropy {
        while !w.is_byte_aligned() {
            w.write_bit(true);
        }
        EncEntropy::Cabac(Box::new(CabacEnc::new(w, slice_qp, init_column)))
    }

    /// Bits written so far, for the row-level rate control.
    pub(crate) fn bit_len(&self) -> u64 {
        match self {
            EncEntropy::Cavlc { w, .. } => w.bit_len(),
            EncEntropy::Cabac(c) => c.bit_len(),
        }
    }

    /// Publish the neighbourhood of the macroblock about to be written.
    pub(crate) fn begin_mb(&mut self, ctx: &MbCtx) {
        if let EncEntropy::Cabac(c) = self {
            c.begin_mb(ctx);
        }
    }

    /// Declare whether this macroblock is intra coded (9.3.3.1.1.9).
    pub(crate) fn set_intra(&mut self, intra: bool) {
        if let EncEntropy::Cabac(c) = self {
            c.set_intra(intra);
        }
    }

    /// This macroblock is a skipped one.
    pub(crate) fn skipped_mb(&mut self, inc: usize) {
        match self {
            EncEntropy::Cavlc { skip_run, .. } => *skip_run += 1,
            EncEntropy::Cabac(c) => c.mb_skip_flag(true, inc),
        }
    }

    /// This macroblock is coded; in a P slice that closes any run of skips.
    pub(crate) fn coded_mb(&mut self, p_slice: bool, inc: usize) {
        if !p_slice {
            return;
        }
        match self {
            EncEntropy::Cavlc { w, skip_run } => {
                w.write_ue(*skip_run);
                *skip_run = 0;
            }
            EncEntropy::Cabac(c) => c.mb_skip_flag(false, inc),
        }
    }

    /// `mb_type`, in the slice's own numbering.
    pub(crate) fn mb_type(&mut self, p_slice: bool, value: u32) {
        match self {
            EncEntropy::Cavlc { w, .. } => w.write_ue(value),
            EncEntropy::Cabac(c) => {
                if p_slice {
                    c.mb_type_p(value);
                } else {
                    c.mb_type_i(value);
                }
            }
        }
    }

    /// One 4x4 intra prediction mode: `None` selects the predicted mode.
    pub(crate) fn intra4x4_pred_mode(&mut self, rem: Option<u8>) {
        match self {
            EncEntropy::Cavlc { w, .. } => match rem {
                None => w.write_bit(true),
                Some(r) => {
                    w.write_bit(false);
                    w.write_bits(u32::from(r), 3);
                }
            },
            EncEntropy::Cabac(c) => c.intra4x4_pred_mode(rem),
        }
    }

    pub(crate) fn intra_chroma_pred_mode(&mut self, mode: u8) {
        match self {
            EncEntropy::Cavlc { w, .. } => w.write_ue(u32::from(mode)),
            EncEntropy::Cabac(c) => c.intra_chroma_pred_mode(mode),
        }
    }

    pub(crate) fn coded_block_pattern(&mut self, luma: u8, chroma: u8, intra: bool) {
        match self {
            EncEntropy::Cavlc { w, .. } => write_cbp(w, luma, chroma, intra),
            EncEntropy::Cabac(c) => c.coded_block_pattern(luma, chroma),
        }
    }

    pub(crate) fn mb_qp_delta(&mut self, delta: i32) {
        match self {
            EncEntropy::Cavlc { w, .. } => w.write_se(delta),
            EncEntropy::Cabac(c) => c.mb_qp_delta(delta),
        }
    }

    /// One `mvd_lX` component; `inc` is the 9.3.3.1.1.7 increment, unused by
    /// CAVLC.
    pub(crate) fn mvd(&mut self, comp: usize, inc: usize, value: i32) {
        match self {
            EncEntropy::Cavlc { w, .. } => w.write_se(value),
            EncEntropy::Cabac(c) => c.mvd(comp, inc, value),
        }
    }

    /// One residual block in scan order; returns its non-zero count, which the
    /// caller records for the neighbours.
    pub(crate) fn residual_block(
        &mut self,
        coeff: &[i32],
        cat: BlockCat,
        na: Option<u8>,
        nb: Option<u8>,
    ) -> u8 {
        match self {
            EncEntropy::Cavlc { w, .. } => {
                let nc = if matches!(cat, BlockCat::ChromaDc(_)) {
                    -1
                } else {
                    match (na, nb) {
                        (Some(a), Some(b)) => (i32::from(a) + i32::from(b) + 1) >> 1,
                        (Some(a), None) => i32::from(a),
                        (None, Some(b)) => i32::from(b),
                        (None, None) => 0,
                    }
                };
                write_residual_block(w, coeff, cat.max_num_coeff(), nc)
            }
            EncEntropy::Cabac(c) => c.residual_block(coeff, cat, na, nb),
        }
    }

    /// `end_of_slice_flag` after a macroblock (CABAC only).
    pub(crate) fn end_of_slice(&mut self, last: bool) {
        if let (EncEntropy::Cabac(c), false) = (self, last) {
            c.not_end_of_slice();
        }
    }

    /// Close the slice and hand back its RBSP.
    pub(crate) fn finish(self, p_slice: bool) -> Vec<u8> {
        match self {
            EncEntropy::Cavlc { mut w, skip_run } => {
                if p_slice && skip_run > 0 {
                    // A slice ending in skipped macroblocks says so with a
                    // final mb_skip_run; writing one when nothing was skipped
                    // would instead promise another macroblock (7.3.4).
                    w.write_ue(skip_run);
                }
                w.write_bit(true); // rbsp_stop_one_bit
                w.align_to_byte();
                w.into_bytes()
            }
            // EncodeFlush already wrote the stop bit (9.3.4.6).
            EncEntropy::Cabac(c) => c.finish().into_bytes(),
        }
    }
}
