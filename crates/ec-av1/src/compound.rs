//! Compound-reference blend weights (spec 7.11.3.15's "Distance weights
//! process", libaom's `av1_dist_wtd_comp_weight_assign` in
//! `reconinter.c`) -- pure, independent of the CDF/bitstream plumbing that
//! decides *whether* a block reads `compound_idx` at all (lane-av1comp,
//! decode.rs). Kept its own module so it is testable without a live
//! decoder: `dist_wtd_comp_weight_assign` takes exactly the four order
//! hints libaom's version reads off `AV1_COMMON`/`MB_MODE_INFO`.

/// `MAX_FRAME_DISTANCE` (spec/`enums.h`): `(1 << FRAME_OFFSET_BITS) - 1`,
/// `FRAME_OFFSET_BITS == 5`.
const MAX_FRAME_DISTANCE: i32 = (1 << 5) - 1;

/// `quant_dist_weight` (libaom `common_data.h`) -- the three candidate
/// weight ratios the search below picks between, before falling back to the
/// index-3 row when either distance is zero.
const QUANT_DIST_WEIGHT: [[i32; 2]; 4] = [[2, 3], [2, 5], [2, 7], [1, MAX_FRAME_DISTANCE]];

/// `quant_dist_lookup_table` (libaom `common_data.h`): the actual
/// `fwd_offset`/`bck_offset` pair each `QUANT_DIST_WEIGHT` row resolves to
/// (both always sum to 16, `1 << DIST_PRECISION_BITS`).
const QUANT_DIST_LOOKUP_TABLE: [[i32; 2]; 4] = [[9, 7], [11, 5], [12, 4], [13, 3]];

/// `av1_dist_wtd_comp_weight_assign` (spec 7.11.3.15 / libaom
/// `reconinter.c`): `(fwd_weight, bck_weight)` for a compound block, given
/// this frame's own `order_hint` and the two references' order hints --
/// `ref0_order_hint` names `mbmi->ref_frame[0]` (libaom's "`bck`", despite
/// the name -- it is nearer in decode order for a typical GOP but the
/// spec's algebra does not assume that), `ref1_order_hint` names
/// `ref_frame[1]` ("`fwd`").
///
/// `compound_idx == 1` (or `!is_compound`, never reached from here) is the
/// simple-average caller's job: it never calls this and uses `(8, 8)`
/// directly (spec 7.11.3.15's `use_dist_wtd_comp_avg == 0` branch) --
/// this function only ever implements the `use_dist_wtd_comp_avg == 1`
/// (`compound_idx == 0`) branch.
pub(crate) fn dist_wtd_comp_weight_assign(
    order_hint_bits: u32,
    cur_order_hint: u32,
    ref0_order_hint: u32,
    ref1_order_hint: u32,
) -> (i32, i32) {
    let d0 =
        crate::motion_field::get_relative_dist(order_hint_bits, ref1_order_hint, cur_order_hint)
            .abs()
            .clamp(0, MAX_FRAME_DISTANCE);
    let d1 =
        crate::motion_field::get_relative_dist(order_hint_bits, cur_order_hint, ref0_order_hint)
            .abs()
            .clamp(0, MAX_FRAME_DISTANCE);

    let order = usize::from(d0 <= d1);

    if d0 == 0 || d1 == 0 {
        return (
            QUANT_DIST_LOOKUP_TABLE[3][order],
            QUANT_DIST_LOOKUP_TABLE[3][1 - order],
        );
    }

    let mut i = 0;
    while i < 3 {
        let c0 = QUANT_DIST_WEIGHT[i][order];
        let c1 = QUANT_DIST_WEIGHT[i][1 - order];
        let d0_c0 = d0 * c0;
        let d1_c1 = d1 * c1;
        if (d0 > d1 && d0_c0 < d1_c1) || (d0 <= d1 && d0_c0 > d1_c1) {
            break;
        }
        i += 1;
    }

    (
        QUANT_DIST_LOOKUP_TABLE[i][order],
        QUANT_DIST_LOOKUP_TABLE[i][1 - order],
    )
}

#[cfg(test)]
mod tests {
    use super::dist_wtd_comp_weight_assign;

    /// Equidistant references (spec/libaom's `d0 == d1` case) land on
    /// `order == 1` (`d0 <= d1`) throughout, and the very first weight
    /// candidate (`i == 0`, ratio `2:3`) always survives the search's break
    /// condition when `d0_c0 == d1_c1`'s neither branch fires -- so a
    /// symmetric GOP (order hints `8` behind and `8` ahead of a `16`-hint
    /// current frame) resolves to the row-0 weights `(7, 9)`: `order == 1`
    /// (`d0 <= d1`) selects `QUANT_DIST_LOOKUP_TABLE[0][1] == 7` for
    /// `fwd_offset` and `QUANT_DIST_LOOKUP_TABLE[0][0] == 9` for
    /// `bck_offset`.
    #[test]
    fn equidistant_references_pick_the_first_weight_row() {
        assert_eq!(dist_wtd_comp_weight_assign(8, 16, 8, 24), (7, 9));
    }

    /// A reference at zero relative distance (this frame repeats one of its
    /// own references' order hint -- degenerate but spec-legal) always
    /// takes the index-3 lookup row (`13`/`3`) rather than entering the
    /// weight search, matching libaom's `d0 == 0 || d1 == 0` early return.
    #[test]
    fn zero_distance_reference_takes_the_index_3_row() {
        // ref1 (fwd) coincides with cur -> d0 == 0.
        assert_eq!(dist_wtd_comp_weight_assign(8, 16, 8, 16), (3, 13));
    }

    /// Weights always sum to `1 << DIST_PRECISION_BITS == 16` -- every row
    /// of `QUANT_DIST_LOOKUP_TABLE` was picked to hold that invariant, and
    /// [`combine_compound`](crate::mc::combine_compound)'s final shift
    /// (`InterPostRound + DIST_PRECISION_BITS`) assumes it.
    #[test]
    fn weights_always_sum_to_sixteen() {
        for cur in [0u32, 16, 40] {
            for r0 in [0u32, 8, 32] {
                for r1 in [4u32, 24, 48] {
                    let (fwd, bck) = dist_wtd_comp_weight_assign(8, cur, r0, r1);
                    assert_eq!(fwd + bck, 16, "cur={cur} r0={r0} r1={r1}");
                }
            }
        }
    }
}
