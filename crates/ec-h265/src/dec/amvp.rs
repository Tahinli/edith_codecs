//! Luma motion vector prediction, AMVP (8.5.3.2.6, 8.5.3.2.7): the spatial
//! candidates A (A0, A1) and B (B0, B1, B2) with the scaled variants and the
//! `isScaledFlagLX` rule, the temporal candidate, duplicate removal, zero filling to
//! two candidates, and `mvLX = mvpLX + MvdLX` with the 16-bit wrap (8-57 to 8-60).

use super::cu_syntax::{InterPredIdc, PredictionUnit};
use super::motion_field::{PbMotion, SliceMotion, scale_mv, to32};
use super::rps::RefEntry;

/// The AMVP motion of one prediction unit: each list the unit uses gets its
/// predictor (selected by `mvp_lX_flag`) plus the decoded difference.
pub fn derive_amvp(src: &SliceMotion<'_>, pu: &PredictionUnit) -> PbMotion {
    let mut pred = [false; 2];
    let mut ref_idx = [-1i8; 2];
    let mut mv = [[0i16; 2]; 2];
    for lx in 0..2 {
        let used = match pu.inter_pred_idc {
            InterPredIdc::PredL0 => lx == 0,
            InterPredIdc::PredL1 => lx == 1,
            InterPredIdc::PredBi => true,
        };
        if !used {
            continue;
        }
        let r = pu.ref_idx[lx] as i8;
        let mvp = predictor(src, pu, lx, r, usize::from(pu.mvp_flag[lx]));
        pred[lx] = true;
        ref_idx[lx] = r;
        mv[lx] = [
            wrap16(mvp[0] + pu.mvd[lx][0]),
            wrap16(mvp[1] + pu.mvd[lx][1]),
        ];
    }
    PbMotion::new(pred, ref_idx, mv)
}

/// `mvpLX` (8.5.3.2.6): the candidate `flag` of the list of up to two predictors.
fn predictor(
    src: &SliceMotion<'_>,
    pu: &PredictionUnit,
    lx: usize,
    ref_idx: i8,
    flag: usize,
) -> [i32; 2] {
    let (x, y) = (pu.x as isize, pu.y as isize);
    let (w, h) = (pu.width as isize, pu.height as isize);
    let target = src.ref_entry(lx, ref_idx);
    let target_poc = target.map(|e| e.poc);

    // A0 then A1 (8.5.3.2.7 steps 1 to 6).
    let a = [src.neighbour(x - 1, y + h), src.neighbour(x - 1, y + h - 1)];
    // B0, B1, B2.
    let b = [
        src.neighbour(x + w, y - 1),
        src.neighbour(x + w - 1, y - 1),
        src.neighbour(x - 1, y - 1),
    ];
    // isScaledFlagLX: some A neighbour is available.
    let is_scaled = a.iter().any(Option::is_some);

    let mv_a = if is_scaled {
        first_pass(src, &a, lx, target_poc).or_else(|| second_pass(src, &a, lx, target))
    } else {
        // Without an A neighbour, the unscaled B candidate takes the A slot.
        first_pass(src, &b, lx, target_poc)
    };
    let mv_b = if is_scaled {
        first_pass(src, &b, lx, target_poc)
    } else {
        second_pass(src, &b, lx, target)
    };

    let mut list: Vec<[i32; 2]> = Vec::with_capacity(3);
    if let Some(mv) = mv_a {
        list.push(mv);
    }
    if let Some(mv) = mv_b {
        // Equal A and B candidates collapse into one (8.5.3.2.6 step 3).
        if mv_a != Some(mv) {
            list.push(mv);
        }
    }
    // The temporal candidate, when the spatial ones do not already fill the list.
    if list.len() < 2 {
        let (xp, yp) = (pu.x, pu.y);
        if let Some(mv) = src.temporal_mv(xp, yp, pu.width, pu.height, lx, ref_idx) {
            list.push(mv);
        }
    }
    list.resize(2, [0, 0]);
    list.get(flag).copied().unwrap_or([0, 0])
}

/// The first neighbour, in order, that refers to the target picture itself, in list
/// `lx` or the other list. No scaling.
fn first_pass(
    src: &SliceMotion<'_>,
    neighbours: &[Option<PbMotion>],
    lx: usize,
    target_poc: Option<i32>,
) -> Option<[i32; 2]> {
    let target_poc = target_poc?;
    for m in neighbours.iter().flatten() {
        for list in [lx, 1 - lx] {
            if m.pred[list] && src.ref_poc(list, m.ref_idx[list]) == Some(target_poc) {
                return Some(to32(m.mv[list]));
            }
        }
    }
    None
}

/// The first neighbour, in order, whose reference has the same long-term marking as
/// the target. Its vector is scaled by the POC distances when both are short-term.
fn second_pass(
    src: &SliceMotion<'_>,
    neighbours: &[Option<PbMotion>],
    lx: usize,
    target: Option<RefEntry>,
) -> Option<[i32; 2]> {
    let target = target?;
    for m in neighbours.iter().flatten() {
        for list in [lx, 1 - lx] {
            if !m.pred[list] {
                continue;
            }
            let Some(r) = src.ref_entry(list, m.ref_idx[list]) else {
                continue;
            };
            if r.long_term != target.long_term {
                continue;
            }
            let mv = to32(m.mv[list]);
            if r.long_term || target.long_term {
                return Some(mv);
            }
            return Some(scale_mv(mv, src.cur_poc - r.poc, src.cur_poc - target.poc));
        }
    }
    None
}

/// `uLX = (mvpLX + mvdLX + 2^16) % 2^16`, read as a signed 16-bit value (8-57 to 8-60).
fn wrap16(v: i32) -> i16 {
    let u = v.rem_euclid(1 << 16);
    let signed = if u >= 1 << 15 { u - (1 << 16) } else { u };
    signed as i16
}

#[cfg(test)]
mod tests {
    use super::super::cu_syntax::InterPredIdc;
    use super::super::motion_field::{MotionField, MotionParams, SliceRefs, test_shared};
    use super::super::rps::{RefEntry, RefPicLists};
    use super::super::shared::SharedPic;
    use super::*;
    use std::sync::Arc;

    fn entry(id: u32, poc: i32, long_term: bool) -> RefEntry {
        RefEntry { id, poc, long_term }
    }

    fn params(temporal: bool) -> MotionParams {
        MotionParams {
            is_b: false,
            max_merge_cand: 5,
            log2_par_mrg_level: 2,
            ctb_log2: 6,
            collocated_from_l0: true,
            temporal_mvp: temporal,
            no_backward_pred: true,
        }
    }

    fn l0(ref_idx: i8, mv: [i16; 2]) -> PbMotion {
        PbMotion::new([true, false], [ref_idx, -1], [mv, [0, 0]])
    }

    /// An AMVP PU at (16,16) of size 16x16 with list 0, reference `ref_idx` and
    /// difference `mvd`, using predictor `flag`.
    fn pu(mvd: [i32; 2], flag: u8) -> PredictionUnit {
        PredictionUnit {
            x: 16,
            y: 16,
            width: 16,
            height: 16,
            inter_pred_idc: InterPredIdc::PredL0,
            mvp_flag: [flag, 0],
            mvd: [mvd, [0, 0]],
            ..PredictionUnit::default()
        }
    }

    /// Run the AMVP derivation for `pu` with `field` as the current motion field.
    fn run(
        field: &mut MotionField,
        lists: &RefPicLists,
        cur_poc: i32,
        col: Option<Arc<SharedPic>>,
        temporal: bool,
        pu: &PredictionUnit,
    ) -> PbMotion {
        let src = SliceMotion {
            params: params(temporal),
            lists,
            cur_poc,
            field,
            col,
            publish: None,
            slice: 0,
        };
        derive_amvp(&src, pu)
    }

    fn lists(poc: i32, long_term: bool) -> RefPicLists {
        RefPicLists {
            l0: vec![entry(0, poc, long_term)],
            l1: Vec::new(),
        }
    }

    #[test]
    fn spatial_a_with_the_same_reference_is_used_unscaled() {
        // A1 = (15,31) refers to POC 4, the target. Its vector is the predictor.
        let mut field = MotionField::new(64, 64);
        field.store(0, 16, 16, 16, 0, l0(0, [40, -12]));
        let got = run(&mut field, &lists(4, false), 8, None, false, &pu([1, 1], 0));
        assert_eq!(got, l0(0, [41, -11]));
    }

    #[test]
    fn spatial_a_scales_by_distance() {
        // Current POC 8, target POC 4 (tb = 4). A1 refers to POC 0 (td = 8), mv 64:
        // tx = (16384 + 4) / 8 = 2048, dsf = (4 * 2048 + 32) >> 6 = 128, mv' = 32.
        // L0[0] is the target (POC 4); the neighbour uses L0[1] (POC 0).
        let lists = RefPicLists {
            l0: vec![entry(0, 4, false), entry(1, 0, false)],
            l1: Vec::new(),
        };
        let mut field = MotionField::new(64, 64);
        field.store(0, 16, 16, 16, 0, l0(1, [64, 0]));
        let got = run(&mut field, &lists, 8, None, false, &pu([0, 0], 0));
        assert_eq!(got, l0(0, [32, 0]));
    }

    #[test]
    fn spatial_a_scales_with_negative_distance() {
        // Current POC 8, target POC 4 (tb = 4). A1 refers to POC 12 (td = -4), mv 64:
        // tx = (16384 + 2) / -4 = -4096, dsf = (4 * -4096 + 32) >> 6 = -256,
        // mv' = -64.
        let lists = RefPicLists {
            l0: vec![entry(0, 4, false), entry(1, 12, false)],
            l1: Vec::new(),
        };
        let mut field = MotionField::new(64, 64);
        field.store(0, 16, 16, 16, 0, l0(1, [64, 0]));
        let got = run(&mut field, &lists, 8, None, false, &pu([0, 0], 0));
        assert_eq!(got, l0(0, [-64, 0]));
    }

    #[test]
    fn long_term_neighbour_is_not_scaled() {
        // The target is long-term (POC 4) and so is the neighbour's reference (POC 0):
        // the vector is used as it is.
        let lists = RefPicLists {
            l0: vec![entry(0, 4, true), entry(1, 0, true)],
            l1: Vec::new(),
        };
        let mut field = MotionField::new(64, 64);
        field.store(0, 16, 16, 16, 0, l0(1, [64, 0]));
        let got = run(&mut field, &lists, 8, None, false, &pu([0, 0], 0));
        assert_eq!(got, l0(0, [64, 0]));
    }

    #[test]
    fn long_term_mismatch_skips_the_neighbour() {
        // The target is long-term but the neighbour's reference is short-term: the
        // neighbour gives no predictor, and the list is zero-filled.
        let lists = RefPicLists {
            l0: vec![entry(0, 4, true), entry(1, 0, false)],
            l1: Vec::new(),
        };
        let mut field = MotionField::new(64, 64);
        field.store(0, 16, 16, 16, 0, l0(1, [64, 0]));
        let got = run(&mut field, &lists, 8, None, false, &pu([0, 0], 0));
        assert_eq!(got, l0(0, [0, 0]));
    }

    #[test]
    fn b_candidate_fills_the_a_slot_when_no_a_neighbour() {
        // No A neighbour. B1 = (31,15) refers to the target POC 4: it fills the A slot,
        // and the second B pass scales it back to itself (td = tb), so the list is
        // [B, 0] and flag 0 picks B.
        let mut field = MotionField::new(64, 64);
        field.store(16, 0, 16, 16, 0, l0(0, [20, 6]));
        let got = run(&mut field, &lists(4, false), 8, None, false, &pu([0, 0], 0));
        assert_eq!(got, l0(0, [20, 6]));
        let other = run(&mut field, &lists(4, false), 8, None, false, &pu([0, 0], 1));
        assert_eq!(other, l0(0, [0, 0]));
    }

    #[test]
    fn equal_a_and_b_collapse_to_one_predictor() {
        // A1 and B1 carry the same vector toward the target: the list is [A, 0].
        let mut field = MotionField::new(64, 64);
        field.store(0, 16, 16, 16, 0, l0(0, [7, 7]));
        field.store(16, 0, 16, 16, 0, l0(0, [7, 7]));
        let got = run(&mut field, &lists(4, false), 8, None, false, &pu([0, 0], 1));
        assert_eq!(got, l0(0, [0, 0]));
        let first = run(&mut field, &lists(4, false), 8, None, false, &pu([0, 0], 0));
        assert_eq!(first, l0(0, [7, 7]));
    }

    #[test]
    fn temporal_fills_the_second_slot() {
        // A1 gives one predictor, so the collocated vector is the second. The collocated
        // picture is POC 8 with a vector toward POC 4 (td = 4), and the target is POC 4
        // (tb = 4): no scaling.
        let mut col_field = MotionField::new(64, 64);
        col_field.slices.push(SliceRefs {
            poc: [vec![4], vec![]],
            long_term: [vec![false], vec![]],
        });
        col_field.store(32, 32, 16, 16, 0, l0(0, [-9, 5]));
        let col = test_shared(&col_field, 8);
        let mut field = MotionField::new(64, 64);
        field.store(0, 16, 16, 16, 0, l0(0, [1, 1]));
        let got = run(
            &mut field,
            &lists(4, false),
            8,
            Some(col),
            true,
            &pu([0, 0], 1),
        );
        assert_eq!(got, l0(0, [-9, 5]));
    }

    #[test]
    fn mv_wraps_at_sixteen_bits() {
        // mvp 32000 plus mvd 1000 is 33000, which wraps to 33000 - 65536 = -32536.
        // mvp -32000 plus mvd -1000 is -33000, which wraps to 32536.
        assert_eq!(wrap16(33000), -32536);
        assert_eq!(wrap16(-33000), 32536);
        // The boundaries: 32767 stays, 32768 becomes -32768.
        assert_eq!(wrap16(32767), 32767);
        assert_eq!(wrap16(32768), -32768);
        assert_eq!(wrap16(-32768), -32768);
        assert_eq!(wrap16(-32769), 32767);
    }

    #[test]
    fn mvd_wrap_through_the_full_derivation() {
        let mut field = MotionField::new(64, 64);
        field.store(0, 16, 16, 16, 0, l0(0, [32000, -32000]));
        let got = run(
            &mut field,
            &lists(4, false),
            8,
            None,
            false,
            &pu([1000, -1000], 0),
        );
        assert_eq!(got, l0(0, [-32536, 32536]));
    }

    #[test]
    fn scale_mv_with_negative_and_positive_distances() {
        // tb / td = 1/2 with td > 0: half the vector.
        assert_eq!(scale_mv([64, 0], 4, 2), [32, 0]);
        // td < 0, tb > 0: the vector flips.
        assert_eq!(scale_mv([64, 0], -4, 4), [-64, 0]);
        // Both negative, ratio 1: the vector is unchanged.
        assert_eq!(scale_mv([-30, 7], -4, -4), [-30, 7]);
        // The result is clipped to 16 bits.
        assert_eq!(scale_mv([32767, 0], 1, 127), [32767, 0]);
    }
}
