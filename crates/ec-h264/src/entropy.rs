//! Entropy seat: the slice-data syntax reader the macroblock loop talks to.
//!
//! H.264 codes the same slice-data syntax two ways — CAVLC (clause 9.2) and
//! CABAC (clause 9.3) — and the difference reaches every syntax element, not
//! just the residual blocks. This enum is the single seam between them, so
//! [`crate::decoder::decode_macroblock`] stays one fused parse-and-reconstruct
//! loop instead of growing a second copy per entropy coder.
//!
//! Enum dispatch rather than a generic parameter: the variant is fixed for a
//! whole slice, so every branch here is perfectly predicted, and one
//! monomorphisation of the macroblock loop keeps the instruction cache (and the
//! source) half the size. Measured: no change on the CAVLC ns/MB test.
//!
//! The seat deliberately takes *neighbour values*, never the picture: context
//! derivation needs the neighbouring macroblocks' coefficient counts and coded
//! block patterns, and passing those in as small copied values keeps the
//! flat-array picture state private to the decoder module.

use ec_core::error::{Error, Result};

use crate::bits::BitCursor;
use crate::cabac::Cabac;
use crate::tables::{CBP_INTER_420, CBP_INTRA_420};

/// Macroblock state bits (`Picture::mb_flags`, and `MbInfo::flags` for a
/// neighbour). CABAC context selection reads all of them.
/// The macroblock has been decoded in this picture.
pub(crate) const FLAG_DECODED: u8 = 1;
/// mb_type is I_PCM.
pub(crate) const FLAG_PCM: u8 = 2;
/// mb_type is one of the Intra_16x16 types.
pub(crate) const FLAG_I16: u8 = 4;
/// intra_chroma_pred_mode is not 0 (DC).
pub(crate) const FLAG_CHROMA_PRED: u8 = 8;
/// The macroblock is inter coded.
pub(crate) const FLAG_INTER: u8 = 16;
/// mb_type is B_Skip or B_Direct_16x16 (9.3.3.1.1.3 at ctxIdxOffset 27).
pub(crate) const FLAG_DIRECT: u8 = 32;
/// mb_type is P_Skip or B_Skip (9.3.3.1.1.1).
pub(crate) const FLAG_SKIP: u8 = 64;

/// Residual block class (spec Table 9-42, the 4:2:0 subset this decoder codes).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BlockCat {
    /// Intra16x16DCLevel, `ctxBlockCat` 0.
    LumaDc,
    /// Intra16x16ACLevel, `ctxBlockCat` 1.
    LumaAc,
    /// LumaLevel4x4, `ctxBlockCat` 2.
    Luma4x4,
    /// ChromaDCLevel of component `iCbCr`, `ctxBlockCat` 3.
    ChromaDc(u8),
    /// ChromaACLevel, `ctxBlockCat` 4.
    ChromaAc,
}

impl BlockCat {
    /// `maxNumCoeff` (Table 9-42).
    pub(crate) const fn max_num_coeff(self) -> usize {
        match self {
            BlockCat::LumaDc | BlockCat::Luma4x4 => 16,
            BlockCat::LumaAc | BlockCat::ChromaAc => 15,
            BlockCat::ChromaDc(_) => 4,
        }
    }

    /// `ctxBlockCat` (Table 9-42).
    pub(crate) const fn ctx_block_cat(self) -> usize {
        match self {
            BlockCat::LumaDc => 0,
            BlockCat::LumaAc => 1,
            BlockCat::Luma4x4 => 2,
            BlockCat::ChromaDc(_) => 3,
            BlockCat::ChromaAc => 4,
        }
    }
}

/// What a context-adaptive reader needs to know about one neighbouring
/// macroblock.
#[derive(Clone, Copy)]
pub(crate) struct MbInfo {
    /// The `FLAG_*` bits above.
    pub flags: u8,
    /// `CodedBlockPatternLuma | CodedBlockPatternChroma << 4`.
    pub cbp: u8,
    /// coded_block_flag of the DC blocks: bit 0 luma, bit 1 Cb, bit 2 Cr.
    pub dc_cbf: u8,
}

/// Neighbourhood of the macroblock about to be parsed: left (A) and above (B)
/// per clause 6.4.11.1, `None` when outside the picture or in another slice.
#[derive(Clone, Copy, Default)]
pub(crate) struct MbCtx {
    pub a: Option<MbInfo>,
    pub b: Option<MbInfo>,
    /// ctxIdxInc for the first mb_qp_delta bin (clause 9.3.3.1.1.5), derived by
    /// the caller from the previous macroblock in decoding order.
    pub qp_delta_inc: u8,
}

/// The slice-data syntax reader.
///
/// The CABAC variant is much the larger of the two (it carries 402 context
/// variables), and stays inline on purpose: one of these lives per slice on the
/// stack, so boxing it would trade a compile-time size warning for a heap
/// allocation in a decode loop that is otherwise allocation free.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Entropy<'a> {
    Cavlc(BitCursor<'a>),
    Cabac(Cabac<'a>),
}

impl<'a> Entropy<'a> {
    /// A CAVLC reader over `rbsp`, positioned just past the slice header.
    pub(crate) fn cavlc(rbsp: &'a [u8], header_bits: u64) -> Entropy<'a> {
        Entropy::Cavlc(BitCursor::new(rbsp, header_bits))
    }

    /// A CABAC reader: consumes the `cabac_alignment_one_bit`s and initialises
    /// the context variables and the arithmetic decoding engine (clause 9.3.1).
    /// `init_column` is 0 for I and SI slices, `cabac_init_idc + 1` otherwise.
    pub(crate) fn cabac(
        rbsp: &'a [u8],
        header_bits: u64,
        slice_qp: i32,
        init_column: usize,
    ) -> Result<Entropy<'a>> {
        Ok(Entropy::Cabac(Cabac::new(
            rbsp,
            header_bits,
            slice_qp,
            init_column,
        )?))
    }

    /// True for the CABAC variant.
    #[inline]
    pub(crate) fn is_cabac(&self) -> bool {
        matches!(self, Entropy::Cabac(_))
    }

    /// Declare whether the macroblock being parsed is intra coded, which
    /// CABAC needs for the coded_block_flag contexts of 9.3.3.1.1.9.
    #[inline]
    pub(crate) fn set_intra(&mut self, intra: bool) {
        if let Entropy::Cabac(c) = self {
            c.set_intra(intra);
        }
    }

    /// `mb_skip_run` (7.3.4), CAVLC only.
    pub(crate) fn mb_skip_run(&mut self) -> Result<u32> {
        match self {
            Entropy::Cavlc(r) => r.read_ue(),
            Entropy::Cabac(_) => Err(Error::corrupt("mb_skip_run in a CABAC slice")),
        }
    }

    /// `mb_skip_flag` (7.3.4), CABAC only; `inc` is condTermFlagA +
    /// condTermFlagB per 9.3.3.1.1.1.
    pub(crate) fn mb_skip_flag(&mut self, b_slice: bool, inc: usize) -> Result<bool> {
        match self {
            Entropy::Cavlc(_) => Err(Error::corrupt("mb_skip_flag in a CAVLC slice")),
            Entropy::Cabac(c) => Ok(c.mb_skip_flag(b_slice, inc)),
        }
    }

    /// `mb_type` of a P or SP slice: 0..4, or 5 + the I-slice mb_type.
    pub(crate) fn mb_type_p(&mut self) -> Result<u32> {
        match self {
            Entropy::Cavlc(r) => {
                let t = r.read_ue()?;
                if t > 30 {
                    return Err(Error::corrupt("P-slice mb_type > 30"));
                }
                Ok(t)
            }
            Entropy::Cabac(c) => c.mb_type_p(),
        }
    }

    /// `mb_type` of a B slice: 0..22, or 23 + the I-slice mb_type.
    pub(crate) fn mb_type_b(&mut self) -> Result<u32> {
        match self {
            Entropy::Cavlc(r) => {
                let t = r.read_ue()?;
                if t > 48 {
                    return Err(Error::corrupt("B-slice mb_type > 48"));
                }
                Ok(t)
            }
            Entropy::Cabac(c) => c.mb_type_b(),
        }
    }

    /// `sub_mb_type[ ]` of a P (`b_slice` false) or B macroblock.
    pub(crate) fn sub_mb_type(&mut self, b_slice: bool) -> Result<u32> {
        let max = if b_slice { 12 } else { 3 };
        match self {
            Entropy::Cavlc(r) => {
                let t = r.read_ue()?;
                if t > max {
                    return Err(Error::corrupt("sub_mb_type out of range"));
                }
                Ok(t)
            }
            Entropy::Cabac(c) => Ok(if b_slice {
                c.sub_mb_type_b()
            } else {
                c.sub_mb_type_p()
            }),
        }
    }

    /// `ref_idx_lX` (7.3.5.1): te(v) over `0..=cmax` for CAVLC, unary with the
    /// neighbour context of 9.3.3.1.1.6 for CABAC.
    pub(crate) fn ref_idx(&mut self, cmax: u32, inc: usize) -> Result<u32> {
        let v = match self {
            // te(v), clause 9.1.1: a one-valued range is a single inverted bit.
            Entropy::Cavlc(r) if cmax == 1 => u32::from(!r.read_bit()?),
            Entropy::Cavlc(r) => r.read_ue()?,
            Entropy::Cabac(c) => c.ref_idx(inc)?,
        };
        if v > cmax {
            return Err(Error::corrupt("ref_idx beyond num_ref_idx_active"));
        }
        Ok(v)
    }

    /// One `mvd_lX` component (7.3.5.1), in quarter luma samples.
    pub(crate) fn mvd(&mut self, comp: usize, inc: usize) -> Result<i32> {
        match self {
            Entropy::Cavlc(r) => r.read_se(),
            Entropy::Cabac(c) => c.mvd(comp, inc),
        }
    }

    /// `coded_block_pattern` of an inter macroblock, as
    /// `(CodedBlockPatternLuma, CodedBlockPatternChroma)`.
    pub(crate) fn coded_block_pattern_inter(&mut self) -> Result<(u8, u8)> {
        match self {
            Entropy::Cavlc(r) => {
                let code = r.read_ue()? as usize;
                let cbp = *CBP_INTER_420
                    .get(code)
                    .ok_or_else(|| Error::corrupt("coded_block_pattern codeNum > 47"))?;
                Ok((cbp & 15, cbp >> 4))
            }
            Entropy::Cabac(c) => c.coded_block_pattern(),
        }
    }

    /// Publish the neighbourhood of the macroblock about to be parsed.
    #[inline]
    pub(crate) fn begin_mb(&mut self, ctx: &MbCtx) {
        if let Entropy::Cabac(c) = self {
            c.begin_mb(ctx);
        }
    }

    /// `mb_type` of an I slice: 0 (I_NxN), 1..24 (Intra_16x16), 25 (I_PCM).
    pub(crate) fn mb_type_i(&mut self) -> Result<u32> {
        match self {
            Entropy::Cavlc(r) => {
                let t = r.read_ue()?;
                if t > 25 {
                    return Err(Error::corrupt("I-slice mb_type > 25"));
                }
                Ok(t)
            }
            Entropy::Cabac(c) => c.mb_type_i(),
        }
    }

    /// `transform_size_8x8_flag` (7.3.5).
    pub(crate) fn transform_size_8x8_flag(&mut self) -> Result<bool> {
        match self {
            Entropy::Cavlc(r) => r.read_bit(),
            Entropy::Cabac(c) => c.transform_size_8x8_flag(),
        }
    }

    /// One 4x4 intra prediction mode: `None` selects the predicted mode,
    /// `Some(rem)` the `rem_intra4x4_pred_mode` value (7.3.5.1).
    #[inline]
    pub(crate) fn intra4x4_pred_mode(&mut self) -> Result<Option<u8>> {
        match self {
            Entropy::Cavlc(r) => {
                if r.read_bit()? {
                    Ok(None)
                } else {
                    Ok(Some(r.read_bits(3)? as u8))
                }
            }
            Entropy::Cabac(c) => c.intra4x4_pred_mode(),
        }
    }

    /// `intra_chroma_pred_mode` (7.3.5.1), 0..3.
    pub(crate) fn intra_chroma_pred_mode(&mut self) -> Result<u8> {
        match self {
            Entropy::Cavlc(r) => {
                let m = r.read_ue()?;
                if m > 3 {
                    return Err(Error::corrupt("intra_chroma_pred_mode > 3"));
                }
                Ok(m as u8)
            }
            Entropy::Cabac(c) => c.intra_chroma_pred_mode(),
        }
    }

    /// `coded_block_pattern` of an Intra_4x4 macroblock, as
    /// `(CodedBlockPatternLuma, CodedBlockPatternChroma)`.
    pub(crate) fn coded_block_pattern_intra(&mut self) -> Result<(u8, u8)> {
        match self {
            Entropy::Cavlc(r) => {
                // me(v), clause 9.1.2 Table 9-4.
                let code = r.read_ue()? as usize;
                let cbp = *CBP_INTRA_420
                    .get(code)
                    .ok_or_else(|| Error::corrupt("coded_block_pattern codeNum > 47"))?;
                Ok((cbp & 15, cbp >> 4))
            }
            Entropy::Cabac(c) => c.coded_block_pattern(),
        }
    }

    /// `mb_qp_delta` (7.4.5), already range checked.
    pub(crate) fn mb_qp_delta(&mut self) -> Result<i32> {
        let delta = match self {
            Entropy::Cavlc(r) => r.read_se()?,
            Entropy::Cabac(c) => c.mb_qp_delta()?,
        };
        if !(-26..=25).contains(&delta) {
            return Err(Error::corrupt("mb_qp_delta outside [-26, 25]"));
        }
        Ok(delta)
    }

    /// Decode one residual block into `coeff[0..cat.max_num_coeff()]` in scan
    /// order, returning the number of non-zero coefficients.
    ///
    /// `na`/`nb` are the same counts for the left and above neighbouring blocks
    /// of the same class, `None` when that neighbour is unavailable. CAVLC
    /// averages them into nC (9.2.1); CABAC turns them into the
    /// coded_block_flag context increment (9.3.3.1.1.9).
    #[inline]
    pub(crate) fn residual_block(
        &mut self,
        coeff: &mut [i32; 16],
        cat: BlockCat,
        na: Option<u8>,
        nb: Option<u8>,
    ) -> Result<u8> {
        match self {
            Entropy::Cavlc(r) => {
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
                crate::cavlc::residual_block(r, coeff, cat.max_num_coeff(), nc)
            }
            Entropy::Cabac(c) => c.residual_block(coeff, cat, na, nb),
        }
    }

    /// The 384 raw bytes of an I_PCM macroblock (256 luma, 2x64 chroma), after
    /// alignment. A CABAC reader restarts its arithmetic engine afterwards
    /// (clause 9.3.1.2).
    pub(crate) fn pcm_block(&mut self) -> Result<&'a [u8]> {
        match self {
            Entropy::Cavlc(r) => {
                r.align_to_byte();
                r.read_bytes(384)
            }
            Entropy::Cabac(c) => c.pcm_block(),
        }
    }

    /// True while the slice has another macroblock: `more_rbsp_data()` for
    /// CAVLC, `!end_of_slice_flag` for CABAC.
    pub(crate) fn more_macroblocks(&mut self) -> Result<bool> {
        match self {
            Entropy::Cavlc(r) => Ok(r.more_rbsp_data()),
            Entropy::Cabac(c) => c.more_macroblocks(),
        }
    }
}
