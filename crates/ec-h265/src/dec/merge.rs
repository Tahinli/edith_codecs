//! Merge mode motion (8.5.3.2.2 to 8.5.3.2.5): the spatial candidates A1, B1, B0,
//! A0 and B2 with the parallel merge level and the second-partition exclusions, the
//! temporal candidate, combined bi-predictive candidates for B slices, zero
//! candidates, and the 8x4 / 4x8 restriction on bi-prediction.

use super::cu_syntax::{CuGeometry, PartMode, PredictionUnit};
use super::motion_field::{PbMotion, SliceMotion, to16};

/// Table 8-7: `l0CandIdx` for `combIdx` 0 to 11.
const COMB_L0: [usize; 12] = [0, 1, 0, 2, 1, 2, 0, 3, 1, 3, 2, 3];
/// Table 8-7: `l1CandIdx` for `combIdx` 0 to 11.
const COMB_L1: [usize; 12] = [1, 0, 2, 0, 2, 1, 3, 0, 3, 1, 3, 2];

/// The merge motion of one prediction unit: the candidate `merge_idx` selects from
/// the merge candidate list (8.5.3.2.2).
pub fn derive_merge(
    src: &SliceMotion<'_>,
    cu: &CuGeometry,
    pu_index: usize,
    pu: &PredictionUnit,
) -> PbMotion {
    let params = src.params;
    // 8.5.3.2.2: with a parallel merge level above 4x4, every 8x8 coding unit shares
    // one merge list, derived as if it were a single prediction unit.
    let single_list = params.log2_par_mrg_level > 2 && cu.size == 8;
    let (x, y, w, h, part_idx) = if single_list {
        (cu.x, cu.y, cu.size, cu.size, 0)
    } else {
        (pu.x, pu.y, pu.width, pu.height, pu_index)
    };

    let mut cands = spatial_candidates(src, cu.part_mode, x, y, w, h, part_idx);
    if let Some(col) = temporal_candidate(src, x, y, w, h) {
        cands.push(col);
    }
    let num_orig = cands.len();
    let max = params.max_merge_cand;
    if params.is_b && num_orig > 1 && num_orig < max {
        combined_candidates(src, &mut cands, num_orig, max);
    }
    // 8.5.3.2.5: zero candidates fill the list up to MaxNumMergeCand.
    let num_ref_idx = if params.is_b {
        src.lists.l0.len().min(src.lists.l1.len())
    } else {
        src.lists.l0.len()
    };
    let mut zero_idx = 0;
    while cands.len() < max {
        let ref_idx = if zero_idx < num_ref_idx { zero_idx } else { 0 };
        cands.push(zero_candidate(params.is_b, ref_idx));
        zero_idx += 1;
    }
    let mut chosen = cands
        .get(usize::from(pu.merge_idx))
        .copied()
        .unwrap_or_else(|| zero_candidate(params.is_b, 0));

    // 8.5.3.2.2: an 8x4 or 4x8 prediction unit is not bi-predicted (nOrigPbW + nOrigPbH == 12).
    if chosen.pred[0] && chosen.pred[1] && pu.width + pu.height == 12 {
        chosen = PbMotion::new(
            [true, false],
            [chosen.ref_idx[0], -1],
            [chosen.mv[0], [0, 0]],
        );
    }
    chosen
}

/// The spatial merge candidates (8.5.3.2.3), in list order. A candidate is dropped
/// when it is unavailable, when it lies in the same parallel merge region as the
/// block, or when it repeats an earlier candidate that the spec compares it with.
fn spatial_candidates(
    src: &SliceMotion<'_>,
    part: PartMode,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    part_idx: usize,
) -> Vec<PbMotion> {
    let (xp, yp) = (x as isize, y as isize);
    let (wi, hi) = (w as isize, h as isize);
    let level = src.params.log2_par_mrg_level;
    // Neighbours in the same merge estimation region are not used.
    let fetch = |xn: isize, yn: isize| -> Option<PbMotion> {
        if xp >> level == xn >> level && yp >> level == yn >> level {
            return None;
        }
        src.neighbour(xn, yn)
    };
    // The second prediction unit of a vertical split must not take the first one's
    // motion as its A1 candidate; the horizontal split does the same for B1.
    let second_vertical = part_idx == 1
        && matches!(
            part,
            PartMode::PartNx2N | PartMode::PartnLx2N | PartMode::PartnRx2N
        );
    let second_horizontal = part_idx == 1
        && matches!(
            part,
            PartMode::Part2NxN | PartMode::Part2NxnU | PartMode::Part2NxnD
        );

    let a1 = if second_vertical {
        None
    } else {
        fetch(xp - 1, yp + hi - 1)
    };
    let b1 = if second_horizontal {
        None
    } else {
        fetch(xp + wi - 1, yp - 1)
    };
    let b0 = fetch(xp + wi, yp - 1);
    let a0 = fetch(xp - 1, yp + hi);
    let b2 = fetch(xp - 1, yp - 1);

    let mut out = Vec::with_capacity(5);
    out.extend(a1);
    out.extend(b1.filter(|m| a1 != Some(*m)));
    out.extend(b0.filter(|m| b1 != Some(*m)));
    out.extend(a0.filter(|m| a1 != Some(*m)));
    // B2 is used only when the four candidates above are not all there.
    if out.len() != 4 {
        out.extend(b2.filter(|m| a1 != Some(*m) && b1 != Some(*m)));
    }
    out
}

/// The temporal merge candidate (8.5.3.2.2 step 4): reference index 0 in each list
/// the slice uses. `None` when neither list has a collocated vector.
fn temporal_candidate(
    src: &SliceMotion<'_>,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
) -> Option<PbMotion> {
    let mv0 = src.temporal_mv(x, y, w, h, 0, 0);
    let mv1 = if src.params.is_b {
        src.temporal_mv(x, y, w, h, 1, 0)
    } else {
        None
    };
    if mv0.is_none() && mv1.is_none() {
        return None;
    }
    let conv = |mv: Option<[i32; 2]>| mv.map_or([0i16, 0], to16);
    Some(PbMotion::new(
        [mv0.is_some(), mv1.is_some()],
        [0, 0],
        [conv(mv0), conv(mv1)],
    ))
}

/// Combined bi-predictive candidates (8.5.3.2.4): pairs an L0 motion of one
/// candidate with an L1 motion of another, in the order of Table 8-7, when the two
/// refer to different pictures or carry different vectors.
fn combined_candidates(
    src: &SliceMotion<'_>,
    cands: &mut Vec<PbMotion>,
    num_orig: usize,
    max: usize,
) {
    let total = num_orig * (num_orig - 1);
    let mut comb = 0;
    while comb < total && comb < COMB_L0.len() && cands.len() < max {
        let (Some(&l0c), Some(&l1c)) = (cands.get(COMB_L0[comb]), cands.get(COMB_L1[comb])) else {
            break;
        };
        if l0c.pred[0]
            && l1c.pred[1]
            && (src.ref_poc(0, l0c.ref_idx[0]) != src.ref_poc(1, l1c.ref_idx[1])
                || l0c.mv[0] != l1c.mv[1])
        {
            cands.push(PbMotion::new(
                [true, true],
                [l0c.ref_idx[0], l1c.ref_idx[1]],
                [l0c.mv[0], l1c.mv[1]],
            ));
        }
        comb += 1;
    }
}

/// A zero-vector candidate with reference index `ref_idx` in list 0 (and list 1 for
/// a B slice).
fn zero_candidate(is_b: bool, ref_idx: usize) -> PbMotion {
    let r = i8::try_from(ref_idx).unwrap_or(0);
    if is_b {
        PbMotion::new([true, true], [r, r], [[0, 0], [0, 0]])
    } else {
        PbMotion::new([true, false], [r, -1], [[0, 0], [0, 0]])
    }
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

    fn params(is_b: bool, max: usize, par: u32) -> MotionParams {
        MotionParams {
            is_b,
            max_merge_cand: max,
            log2_par_mrg_level: par,
            ctb_log2: 6,
            collocated_from_l0: true,
            temporal_mvp: false,
            no_backward_pred: false,
        }
    }

    fn uni(list: usize, ref_idx: i8, mv: [i16; 2]) -> PbMotion {
        let mut pred = [false, false];
        pred[list] = true;
        let mut r = [-1, -1];
        r[list] = ref_idx;
        let mut m = [[0i16; 2]; 2];
        m[list] = mv;
        PbMotion::new(pred, r, m)
    }

    fn pu_at(x: usize, y: usize, w: usize, h: usize, merge_idx: u8) -> PredictionUnit {
        PredictionUnit {
            x,
            y,
            width: w,
            height: h,
            merge_flag: true,
            merge_idx,
            inter_pred_idc: InterPredIdc::PredL0,
            ..PredictionUnit::default()
        }
    }

    fn cu(x: usize, y: usize, size: usize, part: PartMode) -> CuGeometry {
        CuGeometry {
            x,
            y,
            size,
            part_mode: part,
            skip: false,
        }
    }

    fn p_lists() -> RefPicLists {
        RefPicLists {
            l0: vec![entry(0, 0, false)],
            l1: Vec::new(),
        }
    }

    /// The merge result for one PU, in a 64x64 picture with `field` as the current
    /// motion field and `col` as the collocated picture.
    fn merge_with(
        field: &mut MotionField,
        lists: &RefPicLists,
        p: MotionParams,
        col: Option<Arc<SharedPic>>,
        geom: CuGeometry,
        pu: PredictionUnit,
        pu_index: usize,
    ) -> PbMotion {
        let src = SliceMotion {
            params: p,
            lists,
            cur_poc: 4,
            field,
            col,
            publish: None,
            slice: 0,
        };
        derive_merge(&src, &geom, pu_index, &pu)
    }

    #[test]
    fn no_neighbours_gives_zero_candidate() {
        let mut field = MotionField::new(64, 64);
        let got = merge_with(
            &mut field,
            &p_lists(),
            params(false, 5, 2),
            None,
            cu(16, 16, 16, PartMode::Part2Nx2N),
            pu_at(16, 16, 16, 16, 0),
            0,
        );
        assert_eq!(got, uni(0, 0, [0, 0]));
    }

    #[test]
    fn a1_is_first_and_intra_or_missing_neighbour_is_skipped() {
        let mut field = MotionField::new(64, 64);
        // A1 of the 16x16 block at (16,16) is (15,31): store motion there.
        let m = uni(0, 0, [12, -7]);
        field.store(0, 16, 16, 16, 0, m);
        // B1 (31,15) stays missing (intra or not decoded).
        let got = merge_with(
            &mut field,
            &p_lists(),
            params(false, 5, 2),
            None,
            cu(16, 16, 16, PartMode::Part2Nx2N),
            pu_at(16, 16, 16, 16, 0),
            0,
        );
        assert_eq!(got, m);
    }

    #[test]
    fn neighbour_in_other_slice_is_unavailable() {
        let mut field = MotionField::new(64, 64);
        field.store(0, 16, 16, 16, 1, uni(0, 0, [12, -7]));
        let got = merge_with(
            &mut field,
            &p_lists(),
            params(false, 5, 2),
            None,
            cu(16, 16, 16, PartMode::Part2Nx2N),
            pu_at(16, 16, 16, 16, 0),
            0,
        );
        assert_eq!(got, uni(0, 0, [0, 0]));
    }

    #[test]
    fn b1_identical_to_a1_is_pruned() {
        let mut field = MotionField::new(64, 64);
        let a = uni(0, 0, [1, 2]);
        let n = uni(0, 0, [5, 6]);
        field.store(0, 16, 16, 16, 0, a); // A1
        field.store(16, 0, 16, 16, 0, a); // B1 (31,15): same as A1, pruned
        field.store(32, 0, 16, 16, 0, n); // B0 (32,15)
        // merge_idx 1 is B0, since B1 is pruned.
        let got = merge_with(
            &mut field,
            &p_lists(),
            params(false, 5, 2),
            None,
            cu(16, 16, 16, PartMode::Part2Nx2N),
            pu_at(16, 16, 16, 16, 1),
            0,
        );
        assert_eq!(got, n);
    }

    #[test]
    fn parallel_merge_level_hides_neighbours_in_the_same_region() {
        // Log2ParMrgLevel 4 (16x16 regions). The 8x8 CU at (8,8) shares one list
        // (singleMCLFlag), so A1 (7,15) and B1 (15,7) are in its own region and gone.
        let mut field = MotionField::new(64, 64);
        field.store(0, 8, 8, 8, 0, uni(0, 0, [9, 9])); // A1 position, same region
        field.store(8, 0, 8, 8, 0, uni(0, 0, [8, 8])); // B1 position, same region
        let far = uni(0, 0, [-3, 4]);
        field.store(16, 0, 8, 8, 0, far); // B0 (16,7), another region
        let got = merge_with(
            &mut field,
            &p_lists(),
            params(false, 5, 4),
            None,
            cu(8, 8, 8, PartMode::Part2Nx2N),
            pu_at(8, 8, 8, 8, 0),
            0,
        );
        assert_eq!(got, far);
    }

    #[test]
    fn parallel_merge_level_two_uses_the_neighbour() {
        // The same layout with Log2ParMrgLevel 2: nothing is in the same region, so A1 is used.
        let mut field = MotionField::new(64, 64);
        let a = uni(0, 0, [9, 9]);
        field.store(0, 8, 8, 8, 0, a);
        field.store(8, 0, 8, 8, 0, uni(0, 0, [8, 8]));
        let got = merge_with(
            &mut field,
            &p_lists(),
            params(false, 5, 2),
            None,
            cu(8, 8, 8, PartMode::Part2Nx2N),
            pu_at(8, 8, 8, 8, 0),
            0,
        );
        assert_eq!(got, a);
    }

    #[test]
    fn second_vertical_partition_skips_a1() {
        // Nx2N, partIdx 1 at (x+8,y): A1 is the first partition, so it is not a candidate.
        let mut field = MotionField::new(64, 64);
        field.store(16, 16, 8, 16, 0, uni(0, 0, [1, 1])); // the first partition
        let got = merge_with(
            &mut field,
            &p_lists(),
            params(false, 5, 2),
            None,
            cu(16, 16, 16, PartMode::PartNx2N),
            pu_at(24, 16, 8, 16, 0),
            1,
        );
        assert_eq!(got, uni(0, 0, [0, 0]));
    }

    #[test]
    fn temporal_candidate_scales_with_negative_distance() {
        // Current POC 2; the collocated picture is POC 8 and its vector points to POC 4.
        // The current L0[0] is POC 6, after the current picture: tb = -4, td = 4.
        // mv 64: tx = (16384 + 2) / 4 = 4096, dsf = (-4 * 4096 + 32) >> 6 = -256,
        // mv' = Sign(-256 * 64) * ((16384 + 127) >> 8) = -64.
        let mut col_field = MotionField::new(64, 64);
        col_field.slices.push(SliceRefs {
            poc: [vec![4], vec![]],
            long_term: [vec![false], vec![]],
        });
        col_field.store(32, 32, 16, 16, 0, uni(0, 0, [64, 0]));
        let col = test_shared(&col_field, 8);
        let lists = RefPicLists {
            l0: vec![entry(0, 6, false)],
            l1: Vec::new(),
        };
        let mut field = MotionField::new(64, 64);
        let mut p = params(false, 5, 2);
        p.temporal_mvp = true;
        let src = SliceMotion {
            params: p,
            lists: &lists,
            cur_poc: 2,
            field: &mut field,
            col: Some(col),
            publish: None,
            slice: 0,
        };
        // A 16x16 block at (16,16): bottom-right (32,32) is inside the picture and CTB row.
        let got = derive_merge(
            &src,
            &cu(16, 16, 16, PartMode::Part2Nx2N),
            0,
            &pu_at(16, 16, 16, 16, 0),
        );
        assert_eq!(got, uni(0, 0, [-64, 0]));
    }

    #[test]
    fn temporal_candidate_with_long_term_mismatch_is_unavailable() {
        // The current L0[0] is long-term, the collocated reference is short-term:
        // the candidate is unavailable and the zero candidate is used.
        let mut col_field = MotionField::new(64, 64);
        col_field.slices.push(SliceRefs {
            poc: [vec![4], vec![]],
            long_term: [vec![false], vec![]],
        });
        col_field.store(32, 32, 16, 16, 0, uni(0, 0, [64, 0]));
        let col = test_shared(&col_field, 8);
        let lists = RefPicLists {
            l0: vec![entry(0, 0, true)],
            l1: Vec::new(),
        };
        let mut field = MotionField::new(64, 64);
        let mut p = params(false, 5, 2);
        p.temporal_mvp = true;
        let src = SliceMotion {
            params: p,
            lists: &lists,
            cur_poc: 4,
            field: &mut field,
            col: Some(col),
            publish: None,
            slice: 0,
        };
        let got = derive_merge(
            &src,
            &cu(16, 16, 16, PartMode::Part2Nx2N),
            0,
            &pu_at(16, 16, 16, 16, 0),
        );
        assert_eq!(got, uni(0, 0, [0, 0]));
    }

    #[test]
    fn combined_bi_candidate_from_l0_and_l1_neighbours() {
        // A1 is L0-only (POC 0), B1 is L1-only (POC 6): they pair into a bi candidate.
        let lists = RefPicLists {
            l0: vec![entry(0, 0, false)],
            l1: vec![entry(1, 6, false)],
        };
        let mut field = MotionField::new(64, 64);
        let m0 = uni(0, 0, [2, 2]);
        let m1 = uni(1, 0, [-5, 9]);
        field.store(0, 16, 16, 16, 0, m0); // A1
        field.store(16, 0, 16, 16, 0, m1); // B1 (31,15)
        let got = merge_with(
            &mut field,
            &lists,
            params(true, 5, 2),
            None,
            cu(16, 16, 16, PartMode::Part2Nx2N),
            pu_at(16, 16, 16, 16, 2),
            0,
        );
        let expect = PbMotion::new([true, true], [0, 0], [[2, 2], [-5, 9]]);
        assert_eq!(got, expect);
    }

    #[test]
    fn bi_candidate_becomes_uni_for_an_8x4_block() {
        // A B slice whose A1 is bi-predicted: an 8x4 block keeps only L0.
        let lists = RefPicLists {
            l0: vec![entry(0, 0, false)],
            l1: vec![entry(1, 8, false)],
        };
        let mut field = MotionField::new(64, 64);
        let bi = PbMotion::new([true, true], [0, 0], [[3, -3], [4, 4]]);
        field.store(0, 16, 16, 16, 0, bi); // A1 of an 8x4 PU at (16,16)
        let got = merge_with(
            &mut field,
            &lists,
            params(true, 5, 2),
            None,
            cu(16, 16, 8, PartMode::Part2Nx2N),
            pu_at(16, 16, 8, 4, 0),
            0,
        );
        assert_eq!(
            got,
            PbMotion::new([true, false], [0, -1], [[3, -3], [0, 0]])
        );
    }

    #[test]
    fn zero_candidates_use_increasing_reference_indices() {
        // Two L0 references and merge_idx 1 with no spatial candidates: zero ref 1.
        let lists = RefPicLists {
            l0: vec![entry(0, 0, false), entry(1, 2, false)],
            l1: Vec::new(),
        };
        let mut field = MotionField::new(64, 64);
        let got = merge_with(
            &mut field,
            &lists,
            params(false, 5, 2),
            None,
            cu(16, 16, 16, PartMode::Part2Nx2N),
            pu_at(16, 16, 16, 16, 1),
            0,
        );
        assert_eq!(got, uni(0, 1, [0, 0]));
    }
}
