//! CABAC slice-data parsing (spec clause 9.3): arithmetic decoding engine,
//! context variables, binarizations and context selection.
//!
//! Scope is what a frame-coded 4:2:0 8-bit stream needs, I, P and B; every
//! other tool the decoder refuses by name before reaching this module, so the
//! contexts, block categories and initialisation columns that only field or
//! 8x8-transform coding reads are absent rather than transcribed untested.
//!
//! The engine keeps codIRange and codIOffset in `u32` with the spec's exact
//! integer steps — no probability floats, no shortcuts — and reads its bits
//! from the same [`BitCursor`] the CAVLC path uses. Past the end of the RBSP the
//! cursor is treated as zero-filled: a conformant stream never depends on those
//! bits (the last one read is the rbsp_stop_one_bit), and the macroblock loop,
//! not the bit supply, is what bounds a truncated one.

use ec_core::error::{Error, Result};

use crate::bits::BitCursor;
use crate::cabac_tables::{
    INIT_11_59, INIT_60_69, INIT_70_275, INIT_399_401, INIT_MB_TYPE_I, RANGE_LPS, TRANS_LPS,
    TRANS_MPS,
};
use crate::entropy::{
    BlockCat, FLAG_CHROMA_PRED, FLAG_DIRECT, FLAG_I16, FLAG_INTER, FLAG_PCM, MbCtx, MbInfo,
};

/// Highest ctxIdx this decoder can address, plus one (399..401 is the
/// transform_size_8x8_flag block, the last range a frame-coded slice reaches).
const NUM_CTX: usize = 402;

/// ctxIdx 276 (end_of_slice_flag and the I_PCM bin of mb_type) is decoded by
/// DecodeTerminate and has no adapting state.
const CTX_TERMINATE: usize = 276;

/// Table 9-40, `ctxIdxBlockCatOffset` for the five 4:2:0 block categories.
const CBF_CAT_OFF: [usize; 5] = [0, 4, 8, 12, 16];
const SIG_CAT_OFF: [usize; 5] = [0, 15, 29, 44, 47];
const ABS_CAT_OFF: [usize; 5] = [0, 10, 20, 30, 39];

/// ctxIdxOffset values of Table 9-34 used here.
const OFF_MB_TYPE: usize = 3;
const OFF_SKIP_P: usize = 11;
const OFF_MB_TYPE_P: usize = 14;
const OFF_SUB_TYPE_P: usize = 21;
const OFF_SKIP_B: usize = 24;
const OFF_MB_TYPE_B: usize = 27;
const OFF_SUB_TYPE_B: usize = 36;
const OFF_MVD: [usize; 2] = [40, 47];
const OFF_REF_IDX: usize = 54;
const OFF_QP_DELTA: usize = 60;
const OFF_CHROMA_PRED: usize = 64;
const OFF_PREV_I4: usize = 68;
const OFF_REM_I4: usize = 69;
const OFF_CBP_LUMA: usize = 73;
const OFF_CBP_CHROMA: usize = 77;
const OFF_CBF: usize = 85;
const OFF_SIG: usize = 105;
const OFF_LAST: usize = 166;
const OFF_ABS: usize = 227;
const OFF_TRANSFORM_8X8: usize = 399;

/// The ctxIdx values the Intra_16x16 part of an mb_type bin string uses, in
/// bin order: `[b0, CodedBlockPatternLuma, chroma bin 0, chroma bin 1,
/// prediction mode bin 0, prediction mode bin 1]`.
///
/// Table 9-39 lists these per binIdx, but the chroma element is one or two bins
/// long, which shifts the prediction-mode bins; resolving the table to fixed
/// ctxIdx values here is what makes the two spellings (I slice at ctxIdxOffset
/// 3, intra suffix at 17 or 32) one piece of code.
const I_MB_TYPE_CTX: [usize; 6] = [3, 6, 7, 8, 9, 10];
const I_SUFFIX_CTX_P: [usize; 6] = [17, 18, 19, 19, 20, 20];
const I_SUFFIX_CTX_B: [usize; 6] = [32, 33, 34, 34, 35, 35];

pub(crate) struct Cabac<'a> {
    r: BitCursor<'a>,
    /// codIRange (9.3.3.2).
    range: u32,
    /// codIOffset (9.3.3.2).
    offset: u32,
    /// Context variables: `pStateIdx << 1 | valMPS`, indexed by ctxIdx.
    state: [u8; NUM_CTX],
    /// Neighbourhood of the macroblock being parsed.
    ctx: MbCtx,
    /// CodedBlockPatternLuma bins decoded so far in this macroblock, for the
    /// within-macroblock neighbours of clause 9.3.3.1.1.4.
    cbp_luma_bins: u8,
    /// The macroblock being parsed is intra coded, which flips the
    /// coded_block_flag context of an unavailable neighbour (9.3.3.1.1.9).
    cur_intra: bool,
}

impl<'a> Cabac<'a> {
    /// Start CABAC parsing at `header_bits`: `cabac_alignment_one_bit`s, then
    /// the context and engine initialisation of clause 9.3.1.
    ///
    /// `init_column` selects the initialisation column: 0 for I and SI slices,
    /// `cabac_init_idc + 1` otherwise.
    pub(crate) fn new(
        rbsp: &'a [u8],
        header_bits: u64,
        slice_qp: i32,
        init_column: usize,
    ) -> Result<Cabac<'a>> {
        let mut r = BitCursor::new(rbsp, header_bits);
        // cabac_alignment_one_bit (7.3.4): the stuffing bits are all 1, which
        // a corrupt-stream check would only duplicate; skipping to the byte
        // boundary is the whole effect.
        r.align_to_byte();
        let mut c = Cabac {
            r,
            range: 510,
            offset: 0,
            state: [63 << 1; NUM_CTX],
            ctx: MbCtx::default(),
            cbp_luma_bins: 0,
            cur_intra: true,
        };
        c.init_contexts(slice_qp, init_column.min(3));
        c.init_engine()?;
        Ok(c)
    }

    /// 9.3.1.1: pStateIdx and valMPS from the (m, n) table entries and SliceQPY.
    fn init_contexts(&mut self, slice_qp: i32, column: usize) {
        let qp = slice_qp.clamp(0, 51);
        let set = |state: &mut [u8; NUM_CTX], idx: usize, (m, n): (i8, i8)| {
            let pre = (((i32::from(m) * qp) >> 4) + i32::from(n)).clamp(1, 126);
            state[idx] = if pre <= 63 {
                ((63 - pre) as u8) << 1
            } else {
                (((pre - 64) as u8) << 1) | 1
            };
        };
        for (i, &mn) in INIT_MB_TYPE_I.iter().enumerate() {
            set(&mut self.state, i, mn);
        }
        for (i, &mn) in INIT_11_59.iter().enumerate() {
            set(&mut self.state, 11 + i, mn[column]);
        }
        for (i, &mn) in INIT_60_69.iter().enumerate() {
            set(&mut self.state, 60 + i, mn);
        }
        for (i, &mn) in INIT_70_275.iter().enumerate() {
            set(&mut self.state, 70 + i, mn[column]);
        }
        for (i, &mn) in INIT_399_401.iter().enumerate() {
            set(&mut self.state, OFF_TRANSFORM_8X8 + i, mn[column]);
        }
    }

    /// 9.3.1.2: codIRange = 510, codIOffset = read_bits(9).
    fn init_engine(&mut self) -> Result<()> {
        self.range = 510;
        self.offset = self.r.read_bits(9)?;
        if self.offset >= 510 {
            return Err(Error::corrupt("CABAC codIOffset initialised to 510 or 511"));
        }
        Ok(())
    }

    /// One bit of slice data; zero past the end (see the module note).
    #[inline]
    fn bit(&mut self) -> u32 {
        u32::from(self.r.read_bit().unwrap_or(false))
    }

    /// 9.3.3.2.2 RenormD.
    #[inline]
    fn renorm(&mut self) {
        while self.range < 256 {
            self.range <<= 1;
            self.offset = (self.offset << 1) | self.bit();
        }
    }

    /// 9.3.3.2.1 DecodeDecision, with the state transition of 9.3.3.2.1.1.
    #[inline]
    fn decision(&mut self, ctx_idx: usize) -> bool {
        let s = self.state[ctx_idx];
        let p = usize::from(s >> 1);
        let mps = s & 1;
        let lps = u32::from(RANGE_LPS[p][((self.range >> 6) & 3) as usize]);
        self.range -= lps;
        let bin;
        if self.offset >= self.range {
            bin = mps ^ 1;
            self.offset -= self.range;
            self.range = lps;
            // Crossing state 0 swaps the most probable symbol.
            let mps = if p == 0 { mps ^ 1 } else { mps };
            self.state[ctx_idx] = (TRANS_LPS[p] << 1) | mps;
        } else {
            bin = mps;
            self.state[ctx_idx] = (TRANS_MPS[p] << 1) | mps;
        }
        self.renorm();
        bin != 0
    }

    /// 9.3.3.2.3 DecodeBypass.
    #[inline]
    fn bypass(&mut self) -> bool {
        self.offset = (self.offset << 1) | self.bit();
        if self.offset >= self.range {
            self.offset -= self.range;
            true
        } else {
            false
        }
    }

    /// 9.3.3.2.4 DecodeTerminate.
    #[inline]
    fn terminate(&mut self) -> bool {
        self.range -= 2;
        if self.offset >= self.range {
            true
        } else {
            self.renorm();
            false
        }
    }

    pub(crate) fn begin_mb(&mut self, ctx: &MbCtx) {
        self.ctx = *ctx;
        self.cbp_luma_bins = 0;
        self.cur_intra = true;
    }

    /// Declare whether the macroblock being parsed is intra coded
    /// (9.3.3.1.1.9 reads it for every coded_block_flag).
    pub(crate) fn set_intra(&mut self, intra: bool) {
        self.cur_intra = intra;
    }

    /// 9.3.3.1.1.3: condTermFlagA + condTermFlagB for mb_type. At ctxIdxOffset
    /// 3 a neighbour counts unless it is I_NxN; at 27 (B slices) unless it is
    /// B_Skip or B_Direct_16x16.
    fn mb_type_inc(&self, offset: usize) -> usize {
        let cond = |n: Option<MbInfo>| match n {
            None => 0,
            Some(i) if offset == OFF_MB_TYPE => usize::from(i.flags & (FLAG_I16 | FLAG_PCM) != 0),
            Some(i) => usize::from(i.flags & FLAG_DIRECT == 0),
        };
        cond(self.ctx.a) + cond(self.ctx.b)
    }

    /// mb_type of an I slice (Table 9-36).
    pub(crate) fn mb_type_i(&mut self) -> Result<u32> {
        let mut ctx = I_MB_TYPE_CTX;
        ctx[0] = OFF_MB_TYPE + self.mb_type_inc(OFF_MB_TYPE);
        self.mb_type_intra(&ctx)
    }

    /// The Intra part of an mb_type bin string (Table 9-36), whose `ctx` is
    /// resolved by [`I_MB_TYPE_CTX`]. `ctx[0]` has already had its neighbour
    /// increment applied where the offset uses one.
    fn mb_type_intra(&mut self, ctx: &[usize; 6]) -> Result<u32> {
        if !self.decision(ctx[0]) {
            return Ok(0); // I_NxN
        }
        if self.terminate() {
            return Ok(25); // I_PCM
        }
        let cbp_luma = u32::from(self.decision(ctx[1]));
        let chroma = if !self.decision(ctx[2]) {
            0
        } else if self.decision(ctx[3]) {
            2
        } else {
            1
        };
        let mode = u32::from(self.decision(ctx[4])) * 2 + u32::from(self.decision(ctx[5]));
        Ok(1 + mode + chroma * 4 + cbp_luma * 12)
    }

    /// mb_skip_flag (9.3.3.1.1.1); `inc` is condTermFlagA + condTermFlagB.
    pub(crate) fn mb_skip_flag(&mut self, b_slice: bool, inc: usize) -> bool {
        let off = if b_slice { OFF_SKIP_B } else { OFF_SKIP_P };
        self.decision(off + inc)
    }

    /// mb_type of a P or SP slice (Table 9-37): values 0..3, or 5 + the I-slice
    /// mb_type for an intra macroblock.
    pub(crate) fn mb_type_p(&mut self) -> Result<u32> {
        if self.decision(OFF_MB_TYPE_P) {
            return Ok(5 + self.mb_type_intra(&I_SUFFIX_CTX_P)?);
        }
        let b1 = self.decision(OFF_MB_TYPE_P + 1);
        // Table 9-41: binIdx 2 takes ctxIdxInc 2 when b1 is not 1, else 3.
        let b2 = self.decision(OFF_MB_TYPE_P + if b1 { 3 } else { 2 });
        Ok(match (b1, b2) {
            (false, false) => 0, // P_L0_16x16
            (false, true) => 3,  // P_8x8
            (true, true) => 1,   // P_L0_L0_16x8
            (true, false) => 2,  // P_L0_L0_8x16
        })
    }

    /// mb_type of a B slice (Table 9-37): values 0..22, or 23 + the I-slice
    /// mb_type for an intra macroblock.
    pub(crate) fn mb_type_b(&mut self) -> Result<u32> {
        if !self.decision(OFF_MB_TYPE_B + self.mb_type_inc(OFF_MB_TYPE_B)) {
            return Ok(0); // B_Direct_16x16
        }
        if !self.decision(OFF_MB_TYPE_B + 3) {
            // "10x": B_L0_16x16 or B_L1_16x16. binIdx 2 with b1 == 0 takes 5.
            return Ok(1 + u32::from(self.decision(OFF_MB_TYPE_B + 5)));
        }
        let tail = OFF_MB_TYPE_B + 5;
        if !self.decision(OFF_MB_TYPE_B + 4) {
            // "110" + three bins: 3..10.
            let mut v = 0u32;
            for _ in 0..3 {
                v = v * 2 + u32::from(self.decision(tail));
            }
            return Ok(3 + v);
        }
        if !self.decision(tail) {
            // "1110" + three bins: 12..19.
            let mut v = 0u32;
            for _ in 0..3 {
                v = v * 2 + u32::from(self.decision(tail));
            }
            return Ok(12 + v);
        }
        if self.decision(tail) {
            // "11111": B_L1_L0_8x16 or B_8x8.
            return Ok(if self.decision(tail) { 22 } else { 11 });
        }
        if self.decision(tail) {
            // "111101": the intra prefix.
            return Ok(23 + self.mb_type_intra(&I_SUFFIX_CTX_B)?);
        }
        // "111100" + one bin: B_Bi_Bi_16x8 or B_Bi_Bi_8x16.
        Ok(20 + u32::from(self.decision(tail)))
    }

    /// sub_mb_type of a P macroblock (Table 9-38).
    pub(crate) fn sub_mb_type_p(&mut self) -> u32 {
        if self.decision(OFF_SUB_TYPE_P) {
            return 0;
        }
        if !self.decision(OFF_SUB_TYPE_P + 1) {
            return 1;
        }
        if self.decision(OFF_SUB_TYPE_P + 2) {
            2
        } else {
            3
        }
    }

    /// sub_mb_type of a B macroblock (Table 9-38).
    pub(crate) fn sub_mb_type_b(&mut self) -> u32 {
        if !self.decision(OFF_SUB_TYPE_B) {
            return 0; // B_Direct_8x8
        }
        let tail = OFF_SUB_TYPE_B + 3;
        if !self.decision(OFF_SUB_TYPE_B + 1) {
            return 1 + u32::from(self.decision(tail));
        }
        if !self.decision(OFF_SUB_TYPE_B + 2) {
            let v = u32::from(self.decision(tail)) * 2 + u32::from(self.decision(tail));
            return 3 + v;
        }
        if !self.decision(tail) {
            let v = u32::from(self.decision(tail)) * 2 + u32::from(self.decision(tail));
            return 7 + v;
        }
        11 + u32::from(self.decision(tail))
    }

    /// ref_idx_lX: unary, with the neighbour increment of 9.3.3.1.1.6 on the
    /// first bin (Table 9-39 row 54).
    pub(crate) fn ref_idx(&mut self, inc: usize) -> Result<u32> {
        if !self.decision(OFF_REF_IDX + inc) {
            return Ok(0);
        }
        if !self.decision(OFF_REF_IDX + 4) {
            return Ok(1);
        }
        let mut v = 2u32;
        while self.decision(OFF_REF_IDX + 5) {
            v += 1;
            if v > 32 {
                return Err(Error::corrupt("ref_idx bin string too long"));
            }
        }
        Ok(v)
    }

    /// mvd_lX component `comp` (0 horizontal, 1 vertical): UEG3 with
    /// signedValFlag 1 and uCoff 9 (clause 9.3.2.3), `inc` from 9.3.3.1.1.7.
    pub(crate) fn mvd(&mut self, comp: usize, inc: usize) -> Result<i32> {
        let off = OFF_MVD[comp];
        if !self.decision(off + inc) {
            return Ok(0);
        }
        let mut prefix = 1u32;
        // Table 9-39: binIdx 1..4 take 3..6, binIdx 5 and beyond take 6.
        while prefix < 9 {
            let ctx = off + (2 + prefix as usize).min(6);
            if !self.decision(ctx) {
                break;
            }
            prefix += 1;
        }
        let abs = if prefix < 9 {
            prefix
        } else {
            9 + self.ueg_suffix(3)?
        };
        let value = i32::try_from(abs).map_err(|_| Error::corrupt("mvd out of range"))?;
        Ok(if self.bypass() { -value } else { value })
    }

    /// transform_size_8x8_flag (9.3.3.1.1.10).
    pub(crate) fn transform_size_8x8_flag(&mut self) -> Result<bool> {
        // No neighbour can carry the flag: a macroblock that set it is refused
        // before its state is ever stored, so ctxIdxInc is always 0 here.
        Ok(self.decision(OFF_TRANSFORM_8X8))
    }

    /// prev_intra4x4_pred_mode_flag, then rem_intra4x4_pred_mode as FL cMax=7
    /// with binIdx 0 the least significant bit.
    pub(crate) fn intra4x4_pred_mode(&mut self) -> Result<Option<u8>> {
        if self.decision(OFF_PREV_I4) {
            return Ok(None);
        }
        let mut rem = 0u8;
        for bit in 0..3 {
            rem |= u8::from(self.decision(OFF_REM_I4)) << bit;
        }
        Ok(Some(rem))
    }

    /// intra_chroma_pred_mode: TU cMax=3, ctxIdxInc from 9.3.3.1.1.8.
    pub(crate) fn intra_chroma_pred_mode(&mut self) -> Result<u8> {
        let cond = |n: Option<MbInfo>| match n {
            None => 0,
            Some(i) => usize::from(
                i.flags & (FLAG_PCM | FLAG_INTER) == 0 && i.flags & FLAG_CHROMA_PRED != 0,
            ),
        };
        let inc = cond(self.ctx.a) + cond(self.ctx.b);
        if !self.decision(OFF_CHROMA_PRED + inc) {
            return Ok(0);
        }
        if !self.decision(OFF_CHROMA_PRED + 3) {
            return Ok(1);
        }
        Ok(if self.decision(OFF_CHROMA_PRED + 3) {
            3
        } else {
            2
        })
    }

    /// coded_block_pattern (9.3.2.6): FL cMax=15 luma prefix, TU cMax=2 chroma
    /// suffix, both with the neighbour contexts of 9.3.3.1.1.4.
    pub(crate) fn coded_block_pattern(&mut self) -> Result<(u8, u8)> {
        for bin in 0..4u8 {
            // 6.4.11.2: the 8x8 block left of / above block `bin`, which for
            // blocks 1, 2 and 3 can be inside this macroblock.
            let (a, b) = match bin {
                0 => (
                    self.cbp_luma_neighbour(self.ctx.a, 1),
                    self.cbp_luma_neighbour(self.ctx.b, 2),
                ),
                1 => (self.cbp_luma_own(0), self.cbp_luma_neighbour(self.ctx.b, 3)),
                2 => (self.cbp_luma_neighbour(self.ctx.a, 3), self.cbp_luma_own(0)),
                _ => (self.cbp_luma_own(2), self.cbp_luma_own(1)),
            };
            if self.decision(OFF_CBP_LUMA + a + 2 * b) {
                self.cbp_luma_bins |= 1 << bin;
            }
        }
        let luma = self.cbp_luma_bins;
        let chroma_cond = |n: Option<MbInfo>, bin: u8| match n {
            None => 0,
            Some(i) if i.flags & FLAG_PCM != 0 => 1,
            Some(i) => {
                let c = i.cbp >> 4;
                usize::from(if bin == 0 { c != 0 } else { c == 2 })
            }
        };
        let inc0 = chroma_cond(self.ctx.a, 0) + 2 * chroma_cond(self.ctx.b, 0);
        let chroma = if !self.decision(OFF_CBP_CHROMA + inc0) {
            0
        } else {
            let inc1 = chroma_cond(self.ctx.a, 1) + 2 * chroma_cond(self.ctx.b, 1) + 4;
            if self.decision(OFF_CBP_CHROMA + inc1) {
                2
            } else {
                1
            }
        };
        Ok((luma, chroma))
    }

    /// condTermFlagN for a coded_block_pattern luma bin whose neighbouring 8x8
    /// block lies in another macroblock: set when that block has no coefficients.
    fn cbp_luma_neighbour(&self, n: Option<MbInfo>, blk: u8) -> usize {
        match n {
            None => 0,
            Some(i) if i.flags & FLAG_PCM != 0 => 0,
            Some(i) => usize::from(i.cbp & (1 << blk) == 0),
        }
    }

    /// The same for a neighbouring 8x8 block inside this macroblock, read from
    /// the bins already decoded.
    fn cbp_luma_own(&self, blk: u8) -> usize {
        usize::from(self.cbp_luma_bins & (1 << blk) == 0)
    }

    /// mb_qp_delta: unary bin string of the Table 9-3 mapped value.
    pub(crate) fn mb_qp_delta(&mut self) -> Result<i32> {
        let mut k = 0i32;
        while self.decision(OFF_QP_DELTA + qp_delta_inc(k, usize::from(self.ctx.qp_delta_inc))) {
            k += 1;
            if k > 52 {
                return Err(Error::corrupt("mb_qp_delta bin string too long"));
            }
        }
        Ok(if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) })
    }

    /// residual_block_cabac (7.3.5.3.3) for one block, in scan order.
    pub(crate) fn residual_block(
        &mut self,
        coeff: &mut [i32; 16],
        cat: BlockCat,
        na: Option<u8>,
        nb: Option<u8>,
    ) -> Result<u8> {
        *coeff = [0; 16];
        let cat_i = cat.ctx_block_cat();
        let max = cat.max_num_coeff();

        // coded_block_flag (9.3.3.1.1.9). An unavailable neighbour counts as
        // set for an intra macroblock and clear for an inter one; the caller
        // has already folded I_PCM (all coefficients present) and P_Skip /
        // B_Skip (none) into the neighbouring counts.
        let intra = self.cur_intra;
        let cond = |n: Option<u8>| match n {
            None => usize::from(intra),
            Some(v) => usize::from(v != 0),
        };
        let (a, b) = match cat {
            // The DC blocks' neighbours are per macroblock, not per 4x4 block.
            BlockCat::LumaDc => (self.dc_cbf(self.ctx.a, 0), self.dc_cbf(self.ctx.b, 0)),
            BlockCat::ChromaDc(c) => (
                self.dc_cbf(self.ctx.a, 1 + c),
                self.dc_cbf(self.ctx.b, 1 + c),
            ),
            _ => (cond(na), cond(nb)),
        };
        if !self.decision(OFF_CBF + CBF_CAT_OFF[cat_i] + a + 2 * b) {
            return Ok(0);
        }

        // Significance map: significant_coeff_flag / last_significant_coeff_flag
        // over scanning positions 0..maxNumCoeff-1.
        let sig_base = OFF_SIG + SIG_CAT_OFF[cat_i];
        let last_base = OFF_LAST + SIG_CAT_OFF[cat_i];
        let mut sig = [false; 16];
        let mut num = max;
        let mut i = 0;
        while i + 1 < num {
            let inc = sig_inc(cat, i);
            if self.decision(sig_base + inc) {
                sig[i] = true;
                if self.decision(last_base + inc) {
                    num = i + 1;
                }
            }
            i += 1;
        }
        sig[num - 1] = true;

        // Levels, highest scanning position first (9.3.3.1.3 counters).
        let abs_base = OFF_ABS + ABS_CAT_OFF[cat_i];
        let mut num_eq1 = 0u32;
        let mut num_gt1 = 0u32;
        let mut total = 0u8;
        for pos in (0..num).rev() {
            if !sig[pos] {
                continue;
            }
            let inc0 = if num_gt1 != 0 {
                0
            } else {
                4.min(1 + num_eq1) as usize
            };
            let inc1 = 5 + (4 - u32::from(cat_i == 3)).min(num_gt1) as usize;
            // Prefix: TU with cMax = uCoff = 14.
            let mut prefix = 0u32;
            while prefix < 14 {
                let ctx_idx = abs_base + if prefix == 0 { inc0 } else { inc1 };
                if !self.decision(ctx_idx) {
                    break;
                }
                prefix += 1;
            }
            let abs_minus1 = if prefix < 14 {
                prefix
            } else {
                14 + self.ueg_suffix(0)?
            };
            let level = i64::from(abs_minus1) + 1;
            let level = if self.bypass() { -level } else { level };
            coeff[pos] = i32::try_from(level)
                .map_err(|_| Error::corrupt("coefficient level out of range"))?;
            if abs_minus1 == 0 {
                num_eq1 += 1;
            } else {
                num_gt1 += 1;
            }
            total += 1;
        }
        Ok(total)
    }

    /// The bypass-coded k-th order Exp-Golomb suffix of a UEGk bin string
    /// (clause 9.3.2.3): k = 0 for coeff_abs_level_minus1, 3 for mvd.
    fn ueg_suffix(&mut self, k0: u32) -> Result<u32> {
        let mut k = k0;
        let mut value = 0u32;
        while self.bypass() {
            value += 1 << k;
            k += 1;
            if k > 30 {
                return Err(Error::corrupt("UEG0 suffix prefix too long"));
            }
        }
        while k > 0 {
            k -= 1;
            value += u32::from(self.bypass()) << k;
        }
        Ok(value)
    }

    /// condTermFlagN for a DC block: bit `which` of the neighbour's DC
    /// coded_block_flags, with I_PCM counting as coded. An unavailable
    /// neighbour counts as set only for an intra macroblock (9.3.3.1.1.9) —
    /// the chroma DC block of an inter macroblock at the picture edge reads
    /// the opposite value.
    fn dc_cbf(&self, n: Option<MbInfo>, which: u8) -> usize {
        match n {
            None => usize::from(self.cur_intra),
            Some(i) if i.flags & FLAG_PCM != 0 => 1,
            Some(i) => usize::from(i.dc_cbf & (1 << which) != 0),
        }
    }

    /// The 384 raw bytes of an I_PCM macroblock. The arithmetic engine restarts
    /// afterwards (9.3.1.2); the context variables survive.
    pub(crate) fn pcm_block(&mut self) -> Result<&'a [u8]> {
        self.r.align_to_byte();
        let bytes = self.r.read_bytes(384)?;
        self.init_engine()?;
        Ok(bytes)
    }

    /// end_of_slice_flag (ctxIdx 276, DecodeTerminate).
    pub(crate) fn more_macroblocks(&mut self) -> Result<bool> {
        debug_assert_eq!(CTX_TERMINATE, 276);
        Ok(!self.terminate())
    }
}

/// ctxIdxInc for mb_qp_delta bin `k` (Table 9-39 row 60).
#[inline]
fn qp_delta_inc(k: i32, first: usize) -> usize {
    match k {
        0 => first,
        1 => 2,
        _ => 3,
    }
}

/// ctxIdxInc for significant_coeff_flag / last_significant_coeff_flag at
/// scanning position `i` (9.3.3.1.3, frame coded, no 8x8 categories).
#[inline]
fn sig_inc(cat: BlockCat, i: usize) -> usize {
    match cat {
        // ChromaDC with ChromaArrayType 1: Min( levelListIdx / NumC8x8, 2 ).
        BlockCat::ChromaDc(_) => i.min(2),
        _ => i,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine tables are transcribed by hand-checked script; these are the
    /// structural invariants clause 9.3 states about them, so a shifted row or
    /// a dropped column cannot pass silently.
    #[test]
    fn engine_tables_have_their_stated_shape() {
        // Table 9-44 first and last rows (9.3.3.2.1): state 0 is p(LPS) = 0.5,
        // state 63 is the near-deterministic one.
        assert_eq!(RANGE_LPS[0], [128, 176, 208, 240]);
        assert_eq!(RANGE_LPS[63], [2, 2, 2, 2]);
        for (p, row) in RANGE_LPS.iter().enumerate() {
            // Each row rises with qCodIRangeIdx and falls with the state index.
            assert!(
                row.windows(2).all(|w| w[0] <= w[1]),
                "row {p} not monotonic"
            );
            if p > 0 {
                assert!(
                    row[3] <= RANGE_LPS[p - 1][3],
                    "row {p} above its predecessor"
                );
            }
        }
        // Table 9-45: MPS transitions step up one state until the two sticky
        // states at the top, LPS transitions never move up.
        for (p, &next) in TRANS_MPS.iter().enumerate().take(62) {
            assert_eq!(next, p as u8 + 1);
        }
        assert_eq!((TRANS_MPS[62], TRANS_MPS[63]), (62, 63));
        for (p, &next) in TRANS_LPS.iter().enumerate() {
            assert!(usize::from(next) <= p, "LPS transition {p} moves up");
        }
        assert_eq!(TRANS_LPS[63], 63);
        assert_eq!(TRANS_LPS[0], 0);
    }

    /// 9.3.1.1 worked through by hand for two entries.
    #[test]
    fn context_initialisation_matches_equation_9_5() {
        let c = Cabac::new(&[0x55, 0, 0, 0, 0], 0, 26, 0).unwrap();
        // ctxIdx 0, (m, n) = (20, -15): preCtxState = ((20*26) >> 4) - 15 = 17,
        // so pStateIdx = 63 - 17 = 46 and valMPS = 0.
        assert_eq!(c.state[0], 46 << 1);
        // ctxIdx 60, (m, n) = (0, 41): preCtxState = 41 -> pStateIdx 22, MPS 0.
        assert_eq!(c.state[60], 22 << 1);
        // Clip3(1, 126, ...) at a low QP: ctxIdx 6, (m, n) = (-28, 127) at
        // QP 0 gives 127, clipped to 126 -> pStateIdx 62, valMPS 1.
        let c = Cabac::new(&[0x55, 0, 0, 0, 0], 0, 0, 0).unwrap();
        assert_eq!(c.state[6], (62 << 1) | 1);
    }

    /// The initialisation of 9.3.1.2 reads exactly nine bits after the
    /// alignment bits, and every context lands in range.
    #[test]
    fn engine_initialisation_reads_nine_bits() {
        // Header ends mid byte: the alignment bits carry to the boundary, then
        // 0b1010_1010_1 is codIOffset.
        let data = [0xFF, 0b1010_1010, 0b1000_0000, 0, 0];
        let c = Cabac::new(&data, 3, 30, 0).unwrap();
        assert_eq!(c.range, 510);
        assert_eq!(c.offset, 0b1_0101_0101);
        assert!(c.state.iter().all(|&s| s >> 1 < 64));
    }
}
