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
use crate::tables::CBP_INTRA_420;

/// Residual block class (spec Table 9-42, the 4:2:0 subset this decoder codes).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BlockCat {
    /// Intra16x16DCLevel, `ctxBlockCat` 0.
    LumaDc,
    /// Intra16x16ACLevel, `ctxBlockCat` 1.
    LumaAc,
    /// LumaLevel4x4, `ctxBlockCat` 2.
    Luma4x4,
    /// ChromaDCLevel, `ctxBlockCat` 3.
    ChromaDc,
    /// ChromaACLevel, `ctxBlockCat` 4.
    ChromaAc,
}

impl BlockCat {
    /// `maxNumCoeff` (Table 9-42).
    pub(crate) const fn max_num_coeff(self) -> usize {
        match self {
            BlockCat::LumaDc | BlockCat::Luma4x4 => 16,
            BlockCat::LumaAc | BlockCat::ChromaAc => 15,
            BlockCat::ChromaDc => 4,
        }
    }
}

/// The slice-data syntax reader.
pub(crate) enum Entropy<'a> {
    Cavlc(BitCursor<'a>),
}

impl<'a> Entropy<'a> {
    /// A CAVLC reader over `rbsp`, positioned just past the slice header.
    pub(crate) fn cavlc(rbsp: &'a [u8], header_bits: u64) -> Entropy<'a> {
        Entropy::Cavlc(BitCursor::new(rbsp, header_bits))
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
        }
    }

    /// `transform_size_8x8_flag` (7.3.5).
    pub(crate) fn transform_size_8x8_flag(&mut self) -> Result<bool> {
        match self {
            Entropy::Cavlc(r) => r.read_bit(),
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
        }
    }

    /// `mb_qp_delta` (7.4.5), already range checked.
    pub(crate) fn mb_qp_delta(&mut self) -> Result<i32> {
        let delta = match self {
            Entropy::Cavlc(r) => r.read_se()?,
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
                let nc = if cat == BlockCat::ChromaDc {
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
        }
    }

    /// True while the slice has another macroblock: `more_rbsp_data()` for
    /// CAVLC, `!end_of_slice_flag` for CABAC.
    pub(crate) fn more_macroblocks(&mut self) -> Result<bool> {
        match self {
            Entropy::Cavlc(r) => Ok(r.more_rbsp_data()),
        }
    }
}
