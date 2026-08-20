//! CABAC encoding (spec clause 9.3): the arithmetic *encoding* engine of
//! 9.3.4 and the write side of every binarisation the decoder in
//! [`crate::cabac`] reads.
//!
//! Context variables, initialisation tables, ctxIdx offsets and the ctxIdxInc
//! derivations are the decoder's own — this module writes the bins that module
//! reads, in the same order, against the same contexts. The two are held
//! together by the round-trip test of the encoder suite: a single wrong
//! ctxIdx desynchronises the arithmetic coder within a macroblock and the
//! reconstruction stops matching, loudly.
//!
//! Why it exists: CAVLC costs roughly 12% more bits than CABAC at the same
//! quality, and the software encoder edith replaces emits Main-profile CABAC.
//! Matching it on quality at a given bitrate is not possible without this.

// The scanning position of a residual block is a context key as well as an
// index, exactly as in the decoder; iterating the coefficients would hide it.
#![allow(clippy::needless_range_loop)]

use ec_core::BitWriter;

use crate::cabac::{
    ABS_CAT_OFF, CBF_CAT_OFF, I_MB_TYPE_CTX, I_SUFFIX_CTX_P, NUM_CTX, OFF_ABS, OFF_ABS_8X8,
    OFF_CBF, OFF_CBP_CHROMA, OFF_CBP_LUMA, OFF_CHROMA_PRED, OFF_LAST, OFF_LAST_8X8, OFF_MB_TYPE,
    OFF_MB_TYPE_P, OFF_MVD, OFF_PREV_I4, OFF_QP_DELTA, OFF_REM_I4, OFF_SIG, OFF_SIG_8X8,
    OFF_SKIP_P, OFF_TRANSFORM_8X8, SIG_CAT_OFF, init_contexts, qp_delta_inc, sig_inc,
};
use crate::cabac_tables::{LAST_8X8, SIG_8X8_FRAME};
use crate::entropy::{
    BlockCat, FLAG_CHROMA_PRED, FLAG_I16, FLAG_INTER, FLAG_PCM, FLAG_TRANS8X8, MbCtx, MbInfo,
};

/// The arithmetic encoder of 9.3.4 plus the context state.
pub(crate) struct CabacEnc {
    w: BitWriter,
    /// codILow (9.3.4.1).
    low: u32,
    /// codIRange (9.3.4.1).
    range: u32,
    /// bitsOutstanding (9.3.4.2).
    outstanding: u32,
    /// firstBitFlag (9.3.4.2): the leading bit of the arithmetic code word is
    /// not written.
    first: bool,
    state: [u8; NUM_CTX],
    ctx: MbCtx,
    /// CodedBlockPatternLuma bins written so far in this macroblock.
    cbp_luma_bins: u8,
    cur_intra: bool,
}

impl CabacEnc {
    /// Start CABAC at the current (byte-aligned) position of `w`; the caller
    /// has already written the slice header and the `cabac_alignment_one_bit`s.
    pub(crate) fn new(w: BitWriter, slice_qp: i32, init_column: usize) -> CabacEnc {
        debug_assert!(w.is_byte_aligned(), "CABAC starts on a byte boundary");
        CabacEnc {
            w,
            low: 0,
            range: 510,
            outstanding: 0,
            first: true,
            state: init_contexts(slice_qp, init_column.min(3)),
            ctx: MbCtx::default(),
            cbp_luma_bins: 0,
            cur_intra: true,
        }
    }

    // ---- 9.3.4: the encoding engine ----

    /// PutBit (9.3.4.2), with the outstanding-bit counter of the same clause.
    #[inline]
    fn put_bit(&mut self, b: u32) {
        if self.first {
            self.first = false;
        } else {
            self.w.write_bit(b != 0);
        }
        while self.outstanding > 0 {
            self.w.write_bit(b == 0);
            self.outstanding -= 1;
        }
    }

    /// RenormE (9.3.4.2).
    #[inline]
    fn renorm(&mut self) {
        while self.range < 256 {
            if self.low < 256 {
                self.put_bit(0);
            } else if self.low >= 512 {
                self.low -= 512;
                self.put_bit(1);
            } else {
                self.low -= 256;
                self.outstanding += 1;
            }
            self.range <<= 1;
            self.low <<= 1;
        }
    }

    /// EncodeDecision (9.3.4.1), state transition included.
    #[inline]
    fn decision(&mut self, ctx_idx: usize, bin: bool) {
        let s = self.state[ctx_idx];
        let p = usize::from(s >> 1);
        let mps = s & 1;
        let lps = u32::from(crate::cabac_tables::RANGE_LPS[p][((self.range >> 6) & 3) as usize]);
        self.range -= lps;
        if bin != (mps != 0) {
            self.low += self.range;
            self.range = lps;
            let mps = if p == 0 { mps ^ 1 } else { mps };
            self.state[ctx_idx] = (crate::cabac_tables::TRANS_LPS[p] << 1) | mps;
        } else {
            self.state[ctx_idx] = (crate::cabac_tables::TRANS_MPS[p] << 1) | mps;
        }
        self.renorm();
    }

    /// EncodeBypass (9.3.4.3).
    #[inline]
    fn bypass(&mut self, bin: bool) {
        self.low <<= 1;
        if bin {
            self.low += self.range;
        }
        if self.low >= 1024 {
            self.put_bit(1);
            self.low -= 1024;
        } else if self.low < 512 {
            self.put_bit(0);
        } else {
            self.low -= 512;
            self.outstanding += 1;
        }
    }

    /// EncodeTerminate (9.3.4.5); a set bin ends the slice and flushes.
    fn terminate(&mut self, bin: bool) {
        self.range -= 2;
        if bin {
            self.low += self.range;
            self.flush();
        } else {
            self.renorm();
        }
    }

    /// EncodeFlush (9.3.4.6). The last of the two bits written here is the
    /// `rbsp_stop_one_bit`, so the caller only has to byte-align afterwards.
    fn flush(&mut self) {
        self.range = 2;
        self.renorm();
        self.put_bit((self.low >> 9) & 1);
        let v = ((self.low >> 7) & 3) | 1;
        self.w.write_bit(v & 2 != 0);
        self.w.write_bit(v & 1 != 0);
    }

    /// End the slice (`end_of_slice_flag` = 1) and hand back the bitstream,
    /// byte aligned.
    pub(crate) fn finish(mut self) -> BitWriter {
        self.terminate(true);
        self.w.align_to_byte();
        self.w
    }

    /// Bits written so far, including what the engine still holds.
    pub(crate) fn bit_len(&self) -> u64 {
        self.w.bit_len() + u64::from(self.outstanding)
    }

    /// `end_of_slice_flag` = 0: another macroblock follows.
    pub(crate) fn not_end_of_slice(&mut self) {
        self.terminate(false);
    }

    // ---- neighbourhood ----

    pub(crate) fn begin_mb(&mut self, ctx: &MbCtx) {
        self.ctx = *ctx;
        self.cbp_luma_bins = 0;
        self.cur_intra = true;
    }

    pub(crate) fn set_intra(&mut self, intra: bool) {
        self.cur_intra = intra;
    }

    /// 9.3.3.1.1.3 for ctxIdxOffset 3 (I-slice mb_type).
    fn mb_type_inc(&self) -> usize {
        let cond = |n: Option<MbInfo>| match n {
            None => 0,
            Some(i) => usize::from(i.flags & (FLAG_I16 | FLAG_PCM) != 0),
        };
        cond(self.ctx.a) + cond(self.ctx.b)
    }

    // ---- syntax elements ----

    /// `mb_skip_flag` of a P slice; `inc` is condTermFlagA + condTermFlagB.
    pub(crate) fn mb_skip_flag(&mut self, skipped: bool, inc: usize) {
        self.decision(OFF_SKIP_P + inc, skipped);
    }

    /// `mb_type` of an I slice (Table 9-36).
    pub(crate) fn mb_type_i(&mut self, value: u32) {
        let mut ctx = I_MB_TYPE_CTX;
        ctx[0] = OFF_MB_TYPE + self.mb_type_inc();
        self.mb_type_intra(&ctx, value);
    }

    /// `mb_type` of a P slice: 0..3 inter, or 5 + the I-slice type.
    pub(crate) fn mb_type_p(&mut self, value: u32) {
        if value >= 5 {
            self.decision(OFF_MB_TYPE_P, true);
            self.mb_type_intra(&I_SUFFIX_CTX_P, value - 5);
            return;
        }
        self.decision(OFF_MB_TYPE_P, false);
        // Table 9-37 / 9-41: b1 then b2, whose context depends on b1.
        let (b1, b2) = match value {
            0 => (false, false), // P_L0_16x16
            1 => (true, true),   // P_L0_L0_16x8
            2 => (true, false),  // P_L0_L0_8x16
            _ => (false, true),  // P_8x8
        };
        self.decision(OFF_MB_TYPE_P + 1, b1);
        self.decision(OFF_MB_TYPE_P + if b1 { 3 } else { 2 }, b2);
    }

    /// The Intra part of an mb_type bin string; `value` is the I-slice type.
    fn mb_type_intra(&mut self, ctx: &[usize; 6], value: u32) {
        if value == 0 {
            self.decision(ctx[0], false); // I_NxN
            return;
        }
        self.decision(ctx[0], true);
        // I_PCM would set this terminate bin; this encoder never emits I_PCM.
        self.terminate(false);
        let m = value - 1;
        let cbp_luma = m >= 12;
        let chroma = (m / 4) % 3;
        let mode = m % 4;
        self.decision(ctx[1], cbp_luma);
        if chroma == 0 {
            self.decision(ctx[2], false);
        } else {
            self.decision(ctx[2], true);
            self.decision(ctx[3], chroma == 2);
        }
        self.decision(ctx[4], mode & 2 != 0);
        self.decision(ctx[5], mode & 1 != 0);
    }

    /// `prev_intra4x4_pred_mode_flag` and `rem_intra4x4_pred_mode`.
    pub(crate) fn intra4x4_pred_mode(&mut self, rem: Option<u8>) {
        match rem {
            None => self.decision(OFF_PREV_I4, true),
            Some(r) => {
                self.decision(OFF_PREV_I4, false);
                for bit in 0..3 {
                    self.decision(OFF_REM_I4, r >> bit & 1 != 0);
                }
            }
        }
    }

    /// `intra_chroma_pred_mode`: TU cMax=3 with the 9.3.3.1.1.8 increment.
    pub(crate) fn intra_chroma_pred_mode(&mut self, mode: u8) {
        let cond = |n: Option<MbInfo>| match n {
            None => 0,
            Some(i) => usize::from(
                i.flags & (FLAG_PCM | FLAG_INTER) == 0 && i.flags & FLAG_CHROMA_PRED != 0,
            ),
        };
        let inc = cond(self.ctx.a) + cond(self.ctx.b);
        if mode == 0 {
            self.decision(OFF_CHROMA_PRED + inc, false);
            return;
        }
        self.decision(OFF_CHROMA_PRED + inc, true);
        self.decision(OFF_CHROMA_PRED + 3, mode >= 2);
        if mode >= 2 {
            self.decision(OFF_CHROMA_PRED + 3, mode == 3);
        }
    }

    /// `coded_block_pattern` (9.3.2.6).
    pub(crate) fn coded_block_pattern(&mut self, luma: u8, chroma: u8) {
        for bin in 0..4u8 {
            let (a, b) = match bin {
                0 => (
                    self.cbp_luma_neighbour(self.ctx.a, 1),
                    self.cbp_luma_neighbour(self.ctx.b, 2),
                ),
                1 => (self.cbp_luma_own(0), self.cbp_luma_neighbour(self.ctx.b, 3)),
                2 => (self.cbp_luma_neighbour(self.ctx.a, 3), self.cbp_luma_own(0)),
                _ => (self.cbp_luma_own(2), self.cbp_luma_own(1)),
            };
            let set = luma & (1 << bin) != 0;
            self.decision(OFF_CBP_LUMA + a + 2 * b, set);
            if set {
                self.cbp_luma_bins |= 1 << bin;
            }
        }
        let chroma_cond = |n: Option<MbInfo>, bin: u8| match n {
            None => 0,
            Some(i) if i.flags & FLAG_PCM != 0 => 1,
            Some(i) => {
                let c = i.cbp >> 4;
                usize::from(if bin == 0 { c != 0 } else { c == 2 })
            }
        };
        let inc0 = chroma_cond(self.ctx.a, 0) + 2 * chroma_cond(self.ctx.b, 0);
        self.decision(OFF_CBP_CHROMA + inc0, chroma != 0);
        if chroma != 0 {
            let inc1 = chroma_cond(self.ctx.a, 1) + 2 * chroma_cond(self.ctx.b, 1) + 4;
            self.decision(OFF_CBP_CHROMA + inc1, chroma == 2);
        }
    }

    fn cbp_luma_neighbour(&self, n: Option<MbInfo>, blk: u8) -> usize {
        match n {
            None => 0,
            Some(i) if i.flags & FLAG_PCM != 0 => 0,
            Some(i) => usize::from(i.cbp & (1 << blk) == 0),
        }
    }

    fn cbp_luma_own(&self, blk: u8) -> usize {
        usize::from(self.cbp_luma_bins & (1 << blk) == 0)
    }

    /// `mb_qp_delta`: unary over the Table 9-3 mapping.
    pub(crate) fn mb_qp_delta(&mut self, delta: i32) {
        let k = if delta > 0 { 2 * delta - 1 } else { -2 * delta };
        let first = usize::from(self.ctx.qp_delta_inc);
        for i in 0..k {
            self.decision(OFF_QP_DELTA + qp_delta_inc(i, first), true);
        }
        self.decision(OFF_QP_DELTA + qp_delta_inc(k, first), false);
    }

    /// One `mvd_lX` component: UEG3, signed, uCoff 9 (9.3.2.3).
    pub(crate) fn mvd(&mut self, comp: usize, inc: usize, value: i32) {
        let off = OFF_MVD[comp];
        let abs = value.unsigned_abs();
        if abs == 0 {
            self.decision(off + inc, false);
            return;
        }
        self.decision(off + inc, true);
        // The decoder's loop counts prefix from 1 and stops on a zero bin.
        let mut prefix = 1u32;
        while prefix < 9 {
            let ctx = off + (2 + prefix as usize).min(6);
            if prefix < abs {
                self.decision(ctx, true);
                prefix += 1;
            } else {
                self.decision(ctx, false);
                break;
            }
        }
        if abs >= 9 {
            self.ueg_suffix(abs - 9, 3);
        }
        self.bypass(value < 0);
    }

    /// The bypass-coded k-th order Exp-Golomb suffix of a UEGk bin string.
    fn ueg_suffix(&mut self, value: u32, k0: u32) {
        let mut k = k0;
        let mut v = value;
        while v >= 1 << k {
            self.bypass(true);
            v -= 1 << k;
            k += 1;
        }
        self.bypass(false);
        while k > 0 {
            k -= 1;
            self.bypass(v >> k & 1 != 0);
        }
    }

    /// transform_size_8x8_flag (9.3.3.1.1.10), mirroring the decoder's
    /// neighbour condition: available and carrying the flag itself.
    pub(crate) fn transform_size_8x8_flag(&mut self, flag: bool) {
        let cond = |n: Option<MbInfo>| match n {
            None => 0,
            Some(i) => usize::from(i.flags & FLAG_TRANS8X8 != 0),
        };
        let inc = cond(self.ctx.a) + cond(self.ctx.b);
        self.decision(OFF_TRANSFORM_8X8 + inc, flag);
    }

    /// One luma 8x8 residual block in 8x8 zigzag order (ctxBlockCat 5). No
    /// coded_block_flag: for 4:2:0 the coded_block_pattern bit is the whole
    /// signal (7.3.5.3.3), so an all-zero block must not reach here.
    pub(crate) fn residual_block_8x8(&mut self, coeff: &[i32; 64]) -> u8 {
        let last = (0..64)
            .rev()
            .find(|&i| coeff[i] != 0)
            .expect("an 8x8 block with cbp set has a coefficient");
        for i in 0..63 {
            let significant = coeff[i] != 0;
            self.decision(OFF_SIG_8X8 + usize::from(SIG_8X8_FRAME[i]), significant);
            if significant {
                self.decision(OFF_LAST_8X8 + usize::from(LAST_8X8[i]), i == last);
                if i == last {
                    break;
                }
            }
        }
        self.encode_levels(coeff, last, OFF_ABS_8X8, false)
    }

    /// One residual block in scan order (7.3.5.3.3): coded_block_flag, the
    /// significance map, then the levels highest scanning position first.
    ///
    /// `na`/`nb` are the neighbouring blocks' non-zero counts, `None` when that
    /// neighbour is unavailable, exactly as the decoder receives them.
    pub(crate) fn residual_block(
        &mut self,
        coeff: &[i32],
        cat: BlockCat,
        na: Option<u8>,
        nb: Option<u8>,
    ) -> u8 {
        debug_assert!(cat != BlockCat::Luma8x8, "cat 5 goes through residual_block_8x8");
        let cat_i = cat.ctx_block_cat();
        let max = cat.max_num_coeff();
        let last = (0..max).rev().find(|&i| coeff[i] != 0);

        let intra = self.cur_intra;
        let cond = |n: Option<u8>| match n {
            None => usize::from(intra),
            Some(v) => usize::from(v != 0),
        };
        let (a, b) = match cat {
            BlockCat::LumaDc => (self.dc_cbf(self.ctx.a, 0), self.dc_cbf(self.ctx.b, 0)),
            BlockCat::ChromaDc(c) => (
                self.dc_cbf(self.ctx.a, 1 + c),
                self.dc_cbf(self.ctx.b, 1 + c),
            ),
            _ => (cond(na), cond(nb)),
        };
        let Some(last) = last else {
            self.decision(OFF_CBF + CBF_CAT_OFF[cat_i] + a + 2 * b, false);
            return 0;
        };
        self.decision(OFF_CBF + CBF_CAT_OFF[cat_i] + a + 2 * b, true);

        // Significance map. The decoder stops at the last significant position
        // and infers the final coefficient, so nothing is written past `last`.
        let sig_base = OFF_SIG + SIG_CAT_OFF[cat_i];
        let last_base = OFF_LAST + SIG_CAT_OFF[cat_i];
        for i in 0..max - 1 {
            let inc = sig_inc(cat, i);
            let significant = coeff[i] != 0;
            self.decision(sig_base + inc, significant);
            if significant {
                self.decision(last_base + inc, i == last);
                if i == last {
                    break;
                }
            }
        }

        self.encode_levels(coeff, last, OFF_ABS + ABS_CAT_OFF[cat_i], cat_i == 3)
    }

    /// coeff_abs_level_minus1 and coeff_sign_flag of `coeff[..=last]`, highest
    /// scanning position first (9.3.3.1.3 counters). Returns the number of
    /// levels written.
    fn encode_levels(&mut self, coeff: &[i32], last: usize, abs_base: usize, chroma_dc: bool) -> u8 {
        let mut num_eq1 = 0u32;
        let mut num_gt1 = 0u32;
        let mut total = 0u8;
        for pos in (0..=last).rev() {
            let level = coeff[pos];
            if level == 0 {
                continue;
            }
            let inc0 = if num_gt1 != 0 {
                0
            } else {
                4.min(1 + num_eq1) as usize
            };
            let inc1 = 5 + (4 - u32::from(chroma_dc)).min(num_gt1) as usize;
            let abs_minus1 = level.unsigned_abs() - 1;
            let mut prefix = 0u32;
            while prefix < 14 {
                let ctx_idx = abs_base + if prefix == 0 { inc0 } else { inc1 };
                if prefix < abs_minus1 {
                    self.decision(ctx_idx, true);
                    prefix += 1;
                } else {
                    self.decision(ctx_idx, false);
                    break;
                }
            }
            if abs_minus1 >= 14 {
                self.ueg_suffix(abs_minus1 - 14, 0);
            }
            self.bypass(level < 0);
            if abs_minus1 == 0 {
                num_eq1 += 1;
            } else {
                num_gt1 += 1;
            }
            total += 1;
        }
        total
    }

    fn dc_cbf(&self, n: Option<MbInfo>, which: u8) -> usize {
        match n {
            None => usize::from(self.cur_intra),
            Some(i) if i.flags & FLAG_PCM != 0 => 1,
            Some(i) => usize::from(i.dc_cbf & (1 << which) != 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cabac::Cabac;

    /// The engine round-trips: a pseudo-random bin string written against a
    /// spread of contexts, with bypass and terminate bins mixed in, decodes
    /// back identically. This is the property every syntax element rests on.
    #[test]
    fn engine_round_trips() {
        let mut state = 0x2545_F491u32;
        let mut rand = move |m: u32| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state % m
        };
        let mut plan: Vec<(usize, bool, u8)> = Vec::new(); // (ctx, bin, kind)
        for _ in 0..4000 {
            let kind = if rand(10) == 0 { 1 } else { 0 };
            plan.push((rand(NUM_CTX as u32) as usize, rand(2) == 1, kind));
        }
        let mut enc = CabacEnc::new(BitWriter::new(), 26, 0);
        for &(ctx, bin, kind) in &plan {
            match kind {
                0 => enc.decision(ctx, bin),
                _ => enc.bypass(bin),
            }
            enc.not_end_of_slice();
        }
        let bytes = enc.finish().into_bytes();

        let mut dec = Cabac::new(&bytes, 0, 26, 0).expect("decoder starts");
        for (i, &(ctx, bin, kind)) in plan.iter().enumerate() {
            let got = match kind {
                0 => dec.decision_for_test(ctx),
                _ => dec.bypass_for_test(),
            };
            assert_eq!(got, bin, "bin {i}");
            assert!(dec.more_macroblocks().expect("terminate"), "bin {i} end");
        }
    }

    /// transform_size_8x8_flag under every neighbour combination and ctxBlockCat
    /// 5 blocks over a spread of densities and magnitudes all decode back
    /// through the decoder's own syntax readers.
    #[test]
    fn cat5_blocks_and_transform_flag_round_trip() {
        let mut state = 0x9E37_79B9u32;
        let mut rand = move |m: u32| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state % m
        };
        let nbr = |t: bool| MbInfo {
            flags: if t { FLAG_TRANS8X8 } else { 0 },
            cbp: 0,
            dc_cbf: 0,
        };
        let mut plan: Vec<(MbCtx, bool, [i32; 64])> = Vec::new();
        for n in 0..300 {
            let ctx = MbCtx {
                a: [None, Some(nbr(false)), Some(nbr(true))][rand(3) as usize],
                b: [None, Some(nbr(false)), Some(nbr(true))][rand(3) as usize],
                qp_delta_inc: 0,
            };
            let mut c = [0i32; 64];
            let density = [1, 3, 8, 20, 64][rand(5) as usize];
            let mag = [1, 2, 5, 40, 3000][rand(5) as usize];
            for _ in 0..density {
                let v = rand(mag) as i32 + 1;
                c[rand(64) as usize] = if rand(2) == 0 { v } else { -v };
            }
            c[if n % 7 == 0 { 63 } else { 0 }] = 1; // non-empty; exercise the last position
            plan.push((ctx, rand(2) == 1, c));
        }
        let mut enc = CabacEnc::new(BitWriter::new(), 30, 0);
        for (ctx, flag, c) in &plan {
            enc.begin_mb(ctx);
            enc.transform_size_8x8_flag(*flag);
            let tc = enc.residual_block_8x8(c);
            assert_eq!(usize::from(tc), c.iter().filter(|&&v| v != 0).count());
            enc.not_end_of_slice();
        }
        let bytes = enc.finish().into_bytes();

        let mut dec = Cabac::new(&bytes, 0, 30, 0).expect("decoder starts");
        for (i, (ctx, flag, c)) in plan.iter().enumerate() {
            dec.begin_mb(ctx);
            assert_eq!(dec.transform_size_8x8_flag().expect("flag"), *flag, "flag {i}");
            let mut got = [0i32; 64];
            dec.residual_block_8x8(&mut got).expect("block");
            assert_eq!(&got[..], &c[..], "block {i}");
            assert!(dec.more_macroblocks().expect("terminate"), "block {i} end");
        }
    }
}
