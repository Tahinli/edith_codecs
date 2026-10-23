//! Backward probability adaptation (spec 8.4.2-8.4.4; libvpx
//! `vp9_adapt_coef_probs`, `vp9_adapt_mode_probs`, `vp9_adapt_mv_probs`).
//!
//! A frame that is neither error-resilient nor frame-parallel counts every
//! symbol it reads ([`Counts`]). After its tiles are decoded, the adapted
//! probabilities are `merge(pre, counts)`, where `pre` is the stored frame
//! context the frame started from (not the header-delta-updated copy the
//! frame decoded with). The result becomes the frame context that is
//! stored when `refresh_frame_context` is set.

use crate::header::FrameContext;
use crate::tables::*;

/// Count saturation and update factor for the coefficient tables
/// (`COEF_COUNT_SAT`, `COEF_MAX_UPDATE_FACTOR{,_KEY,_AFTER_KEY}`).
pub(crate) const COEF_COUNT_SAT: u32 = 24;
pub(crate) const COEF_MAX_UPDATE_FACTOR: u32 = 112;
pub(crate) const COEF_MAX_UPDATE_FACTOR_AFTER_KEY: u32 = 128;
/// `MODE_MV_COUNT_SAT` / `MODE_MV_MAX_UPDATE_FACTOR`.
const MODE_MV_COUNT_SAT: u32 = 20;
const MODE_MV_MAX_UPDATE_FACTOR: u32 = 128;

/// Symbol counts for one frame (libvpx `FRAME_COUNTS`).
#[derive(Clone, Default)]
pub(crate) struct Counts {
    /// `[tx][plane][ref][band][ctx][token]`: token 0 ZERO, 1 ONE, 2 TWO-or-more,
    /// 3 EOB (the end-of-block decision that stopped the block).
    pub coef: [[[[[[u32; 4]; 6]; 6]; 2]; 2]; 4],
    /// `[tx][plane][ref][band][ctx]`: how often the EOB decision was read.
    pub eob_branch: [[[[[u32; 6]; 6]; 2]; 2]; 4],
    pub partition: [[u32; 4]; 16],
    pub y_mode: [[u32; 10]; 4],
    pub uv_mode: [[u32; 10]; 10],
    pub skip: [[u32; 2]; 3],
    pub tx8: [[u32; 2]; 2],
    pub tx16: [[u32; 3]; 2],
    pub tx32: [[u32; 4]; 2],
    /// Indexed by `INTER_OFFSET(mode)`: NEAREST 0, NEAR 1, ZERO 2, NEW 3.
    pub inter_mode: [[u32; 4]; 7],
    pub intra_inter: [[u32; 2]; 4],
    pub comp_inter: [[u32; 2]; 5],
    pub comp_ref: [[u32; 2]; 5],
    pub single_ref: [[[u32; 2]; 2]; 5],
    pub switchable_interp: [[u32; 3]; 4],
    pub mv_joints: [u32; 4],
    pub mv: [MvCounts; 2],
}

/// Motion-vector component counts (libvpx `nmv_component_counts`).
#[derive(Clone, Copy, Default)]
pub(crate) struct MvCounts {
    pub sign: [u32; 2],
    pub classes: [u32; 11],
    pub class0: [u32; 2],
    pub bits: [[u32; 2]; 10],
    pub class0_fp: [[u32; 4]; 2],
    pub fp: [u32; 4],
    pub class0_hp: [u32; 2],
    pub hp: [u32; 2],
}

/// `vp9_inc_mv` with `usehp = 1`: count one decoded motion-vector difference
/// `(row, col)` (libvpx `inc_mv_component`, vp9_entropymv.c). The hp bit of a
/// component read without high precision is 1, so it counts into `hp[1]`.
pub(crate) fn inc_mv(counts: &mut Counts, diff: (i32, i32)) {
    let vert = diff.0 != 0;
    let horz = diff.1 != 0;
    let joint = (usize::from(vert) << 1) | usize::from(horz);
    counts.mv_joints[joint] += 1;
    if vert {
        inc_mv_component(&mut counts.mv[0], diff.0);
    }
    if horz {
        inc_mv_component(&mut counts.mv[1], diff.1);
    }
}

fn inc_mv_component(c: &mut MvCounts, v: i32) {
    let s = usize::from(v < 0);
    c.sign[s] += 1;
    let z = (if v < 0 { -v } else { v }) as u32 - 1;
    // `vp9_get_mv_class`: class 0 spans [0, 16); class c >= 1 starts at
    // CLASS0_SIZE << (c + 2) = 2 << (c + 2).
    let class = if z < 16 {
        0
    } else {
        // log2 of (z >> 3), as libvpx's log_in_base_2 table does.
        let mut c = 1usize;
        while c < 10 && z >= (2u32 << (c + 3)) {
            c += 1;
        }
        c
    };
    let base = if class == 0 { 0 } else { 2u32 << (class + 2) };
    let o = z - base;
    let d = (o >> 3) as usize;
    let f = ((o >> 1) & 3) as usize;
    let e = (o & 1) as usize;
    c.classes[class] += 1;
    if class == 0 {
        c.class0[d] += 1;
        c.class0_fp[d][f] += 1;
        c.class0_hp[e] += 1;
    } else {
        for i in 0..class {
            c.bits[i][(d >> i) & 1] += 1;
        }
        c.fp[f] += 1;
        c.hp[e] += 1;
    }
}

/// `get_prob` (vp9_entropy.c): `num / den` in 1/256 units, clipped to 1..=255.
fn get_prob(num: u32, den: u32) -> u8 {
    let p = (u64::from(num) * 256 + u64::from(den >> 1)) / u64::from(den);
    p.clamp(1, 255) as u8
}

/// `weighted_prob`: blend the pre-adaptation probability with the observed one.
fn weighted_prob(pre: u8, prob: u8, factor: u32) -> u8 {
    let v = u32::from(pre) * (256 - factor) + u32::from(prob) * factor;
    ((v + 128) >> 8) as u8
}

/// `merge_probs` (coefficient tables): saturating update factor by count.
fn merge_probs(pre: u8, ct: [u32; 2], count_sat: u32, max_update_factor: u32) -> u8 {
    let den = ct[0] + ct[1];
    let prob = if den == 0 { 128 } else { get_prob(ct[0], den) };
    let count = den.min(count_sat);
    let factor = max_update_factor * count / count_sat;
    weighted_prob(pre, prob, factor)
}

/// `mode_mv_merge_probs`: the mode and motion-vector tables. The factor
/// table `count_to_update_factor` is `128 * count / 20`.
fn mode_mv_merge_probs(pre: u8, ct: [u32; 2]) -> u8 {
    let den = ct[0] + ct[1];
    if den == 0 {
        return pre;
    }
    let count = den.min(MODE_MV_COUNT_SAT);
    let factor = MODE_MV_MAX_UPDATE_FACTOR * count / MODE_MV_COUNT_SAT;
    weighted_prob(pre, get_prob(ct[0], den), factor)
}

/// `vpx_tree_merge_probs` over one tree: node `i` gets `mode_mv_merge_probs`
/// of its two subtree counts. Returns the subtree's total count.
fn tree_merge(tree: &[i8], i: usize, pre: &[u8], counts: &[u32], out: &mut [u8]) -> u32 {
    let side = |child: i8, out: &mut [u8]| -> u32 {
        if child <= 0 {
            counts[(-child) as usize]
        } else {
            tree_merge(tree, child as usize, pre, counts, out)
        }
    };
    let left = side(tree[i], out);
    let right = side(tree[i + 1], out);
    out[i >> 1] = mode_mv_merge_probs(pre[i >> 1], [left, right]);
    left + right
}

fn merge_tree(tree: &[i8], pre: &[u8], counts: &[u32], out: &mut [u8]) {
    tree_merge(tree, 0, pre, counts, out);
}

/// `vp9_adapt_coef_probs`. `update_factor` is 112 for a frame after a
/// non-key frame, 128 for the frame right after a key frame.
pub(crate) fn adapt_coef_probs(
    cur: &mut FrameContext,
    pre: &FrameContext,
    counts: &Counts,
    update_factor: u32,
) {
    for t in 0..4 {
        for i in 0..2 {
            for j in 0..2 {
                for k in 0..6 {
                    // Band 0 has three contexts; the rest are never coded.
                    let nctx = if k == 0 { 3 } else { 6 };
                    for l in 0..nctx {
                        let c = counts.coef[t][i][j][k][l];
                        let n0 = c[0];
                        let n1 = c[1];
                        let n2 = c[2];
                        let neob = c[3];
                        let branch = counts.eob_branch[t][i][j][k][l];
                        let ct = [
                            [neob, branch.saturating_sub(neob)],
                            [n0, n1 + n2],
                            [n1, n2],
                        ];
                        for m in 0..3 {
                            cur.coef[t][i][j][k][l][m] = merge_probs(
                                pre.coef[t][i][j][k][l][m],
                                ct[m],
                                COEF_COUNT_SAT,
                                update_factor,
                            );
                        }
                    }
                }
            }
        }
    }
}

/// `vp9_adapt_mode_probs` (inter frames only). `tx_select` is
/// `tx_mode == TX_MODE_SELECT`; `switchable` is `interp_filter == SWITCHABLE`.
pub(crate) fn adapt_mode_probs(
    cur: &mut FrameContext,
    pre: &FrameContext,
    counts: &Counts,
    tx_select: bool,
    switchable: bool,
) {
    for i in 0..4 {
        cur.intra_inter[i] = mode_mv_merge_probs(pre.intra_inter[i], counts.intra_inter[i]);
    }
    for i in 0..5 {
        cur.comp_inter[i] = mode_mv_merge_probs(pre.comp_inter[i], counts.comp_inter[i]);
        cur.comp_ref[i] = mode_mv_merge_probs(pre.comp_ref[i], counts.comp_ref[i]);
        for j in 0..2 {
            cur.single_ref[i][j] =
                mode_mv_merge_probs(pre.single_ref[i][j], counts.single_ref[i][j]);
        }
    }
    for i in 0..7 {
        merge_tree(
            &INTER_MODE_TREE,
            &pre.inter_mode[i],
            &counts.inter_mode[i],
            &mut cur.inter_mode[i],
        );
    }
    for i in 0..4 {
        merge_tree(
            &INTRA_MODE_TREE,
            &pre.y_mode[i],
            &counts.y_mode[i],
            &mut cur.y_mode[i],
        );
    }
    for i in 0..10 {
        merge_tree(
            &INTRA_MODE_TREE,
            &pre.uv_mode[i],
            &counts.uv_mode[i],
            &mut cur.uv_mode[i],
        );
    }
    for i in 0..16 {
        merge_tree(
            &PARTITION_TREE,
            &pre.partition[i],
            &counts.partition[i],
            &mut cur.partition[i],
        );
    }
    if switchable {
        for i in 0..4 {
            merge_tree(
                &SWITCHABLE_INTERP_TREE,
                &pre.switchable_interp[i],
                &counts.switchable_interp[i],
                &mut cur.switchable_interp[i],
            );
        }
    }
    if tx_select {
        for i in 0..2 {
            let c = counts.tx8[i];
            cur.tx_p8x8[i][0] = mode_mv_merge_probs(pre.tx_p8x8[i][0], [c[0], c[1]]);

            let c = counts.tx16[i];
            cur.tx_p16x16[i][0] = mode_mv_merge_probs(pre.tx_p16x16[i][0], [c[0], c[1] + c[2]]);
            cur.tx_p16x16[i][1] = mode_mv_merge_probs(pre.tx_p16x16[i][1], [c[1], c[2]]);

            let c = counts.tx32[i];
            cur.tx_p32x32[i][0] =
                mode_mv_merge_probs(pre.tx_p32x32[i][0], [c[0], c[1] + c[2] + c[3]]);
            cur.tx_p32x32[i][1] = mode_mv_merge_probs(pre.tx_p32x32[i][1], [c[1], c[2] + c[3]]);
            cur.tx_p32x32[i][2] = mode_mv_merge_probs(pre.tx_p32x32[i][2], [c[2], c[3]]);
        }
    }
    for i in 0..3 {
        cur.skip[i] = mode_mv_merge_probs(pre.skip[i], counts.skip[i]);
    }
}

/// `vp9_adapt_mv_probs`. The hp tables adapt only when high precision is on.
pub(crate) fn adapt_mv_probs(
    cur: &mut FrameContext,
    pre: &FrameContext,
    counts: &Counts,
    allow_hp: bool,
) {
    merge_tree(
        &MV_JOINT_TREE,
        &pre.nmv_joints,
        &counts.mv_joints,
        &mut cur.nmv_joints,
    );
    for comp in 0..2 {
        let c = &counts.mv[comp];
        cur.nmv_sign[comp] = mode_mv_merge_probs(pre.nmv_sign[comp], c.sign);
        merge_tree(
            &MV_CLASS_TREE,
            &pre.nmv_classes[comp],
            &c.classes,
            &mut cur.nmv_classes[comp],
        );
        cur.nmv_class0[comp][0] = mode_mv_merge_probs(pre.nmv_class0[comp][0], c.class0);
        for j in 0..10 {
            cur.nmv_bits[comp][j] = mode_mv_merge_probs(pre.nmv_bits[comp][j], c.bits[j]);
        }
        for j in 0..2 {
            merge_tree(
                &MV_FP_TREE,
                &pre.nmv_class0_fp[comp][j],
                &c.class0_fp[j],
                &mut cur.nmv_class0_fp[comp][j],
            );
        }
        merge_tree(&MV_FP_TREE, &pre.nmv_fp[comp], &c.fp, &mut cur.nmv_fp[comp]);
        if allow_hp {
            cur.nmv_class0_hp[comp] =
                mode_mv_merge_probs(pre.nmv_class0_hp[comp], c.class0_hp);
            cur.nmv_hp[comp] = mode_mv_merge_probs(pre.nmv_hp[comp], c.hp);
        }
    }
}

impl Counts {
    /// Add `other` into `self` (merging per-worker counts; order-free).
    pub(crate) fn add(&mut self, other: &Counts) {
        for (d, s) in self.coef.iter_mut().flatten().flatten().flatten().flatten().flatten().zip(
            other.coef.iter().flatten().flatten().flatten().flatten().flatten(),
        ) {
            *d += s;
        }
        for (d, s) in self.eob_branch.iter_mut().flatten().flatten().flatten().flatten().zip(
            other.eob_branch.iter().flatten().flatten().flatten().flatten(),
        ) {
            *d += s;
        }
        add_rows(&mut self.partition, &other.partition);
        add_rows(&mut self.y_mode, &other.y_mode);
        add_rows(&mut self.uv_mode, &other.uv_mode);
        add_rows(&mut self.skip, &other.skip);
        add_rows(&mut self.tx8, &other.tx8);
        add_rows(&mut self.tx16, &other.tx16);
        add_rows(&mut self.tx32, &other.tx32);
        add_rows(&mut self.inter_mode, &other.inter_mode);
        add_rows(&mut self.intra_inter, &other.intra_inter);
        add_rows(&mut self.comp_inter, &other.comp_inter);
        add_rows(&mut self.comp_ref, &other.comp_ref);
        for (d, s) in self.single_ref.iter_mut().flatten().flatten().zip(other.single_ref.iter().flatten().flatten()) {
            *d += s;
        }
        add_rows(&mut self.switchable_interp, &other.switchable_interp);
        for (d, s) in self.mv_joints.iter_mut().zip(&other.mv_joints) {
            *d += s;
        }
        for (d, s) in self.mv.iter_mut().zip(&other.mv) {
            d.add(s);
        }
    }
}

/// Element-wise `+=` of two arrays of count rows (`[[u32; N]; M]`).
fn add_rows<const M: usize, const N: usize>(dst: &mut [[u32; N]; M], src: &[[u32; N]; M]) {
    for (d, s) in dst.iter_mut().flatten().zip(src.iter().flatten()) {
        *d += s;
    }
}

impl MvCounts {
    fn add(&mut self, o: &MvCounts) {
        for (d, s) in self.sign.iter_mut().zip(&o.sign) {
            *d += s;
        }
        for (d, s) in self.classes.iter_mut().zip(&o.classes) {
            *d += s;
        }
        for (d, s) in self.class0.iter_mut().zip(&o.class0) {
            *d += s;
        }
        for (d, s) in self.bits.iter_mut().flatten().zip(o.bits.iter().flatten()) {
            *d += s;
        }
        for (d, s) in self.class0_fp.iter_mut().flatten().zip(o.class0_fp.iter().flatten()) {
            *d += s;
        }
        for (d, s) in self.fp.iter_mut().zip(&o.fp) {
            *d += s;
        }
        for (d, s) in self.class0_hp.iter_mut().zip(&o.class0_hp) {
            *d += s;
        }
        for (d, s) in self.hp.iter_mut().zip(&o.hp) {
            *d += s;
        }
    }
}

