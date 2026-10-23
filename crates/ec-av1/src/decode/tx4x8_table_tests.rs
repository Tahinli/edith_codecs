use super::*;

/// Asymmetric gate on the two new scan tables (class
/// `reference-layout-not-spec`): a transposed pair of tables passes any
/// "is a permutation" check, so this pins the pair AGAINST each other --
/// `SCAN_8X4[k]` must be `SCAN_4X8[k]` reflected across the diagonal, and
/// both must be bijections of `0..32` in our own row-major indexing.
#[test]
fn the_rect_4x8_scans_are_each_other_transposed() {
    let mut seen_a = [false; 32];
    let mut seen_b = [false; 32];
    for k in 0..32usize {
        let a = SCAN_4X8[k] as usize;
        let b = SCAN_8X4[k] as usize;
        assert!(!seen_a[a] && !seen_b[b], "scan position {k} repeats");
        seen_a[a] = true;
        seen_b[b] = true;
        let (row, col) = (a / 4, a % 4);
        assert_eq!(
            b,
            col * 8 + row,
            "SCAN_8X4[{k}] is not SCAN_4X8[{k}] transposed"
        );
    }
    assert!(seen_a.iter().all(|&s| s) && seen_b.iter().all(|&s| s));
}

/// `mrow_scan_4x8`/`mcol_scan_4x8`/`mrow_scan_8x4`/`mcol_scan_8x4`
/// (libaom `scan.c`), converted out of their column-major encoding by
/// `(p % h) * w + p / h` -- the head of each, pinned so the rect
/// `V_DCT`/`H_DCT` walk cannot silently transpose.
#[test]
fn the_rect_class1_scans_match_libaom() {
    assert_eq!(
        class_scan_table_wh(4, 8, TxClass::Vert),
        (0..32u16).collect::<Vec<_>>()
    );
    assert_eq!(
        &class_scan_table_wh(4, 8, TxClass::Horiz)[..8],
        &[0u16, 4, 8, 12, 16, 20, 24, 28]
    );
    assert_eq!(
        class_scan_table_wh(8, 4, TxClass::Vert),
        (0..32u16).collect::<Vec<_>>()
    );
    assert_eq!(
        &class_scan_table_wh(8, 4, TxClass::Horiz)[..8],
        &[0u16, 8, 16, 24, 1, 9, 17, 25]
    );
}

/// The 32-position `eob_pt` alphabet: six symbols (one more than the
/// 16-position table, one fewer than the 64-position one), strictly
/// increasing, terminated at 32768 with the adaptation counter after it.
#[test]
fn the_32_position_eob_tables_are_well_formed() {
    for table in [
        &cdf::EOB_PT_32_LUMA_Q0,
        &cdf::EOB_PT_32_LUMA_Q1,
        &cdf::EOB_PT_32_LUMA,
        &cdf::EOB_PT_32_LUMA_Q3,
        &cdf::EOB_PT_32_LUMA_CLASS1_Q0,
        &cdf::EOB_PT_32_LUMA_CLASS1_Q1,
        &cdf::EOB_PT_32_LUMA_CLASS1,
        &cdf::EOB_PT_32_LUMA_CLASS1_Q3,
    ] {
        assert_eq!(table.len(), 7);
        assert_eq!(table[5], 32768);
        assert_eq!(table[6], 0);
        for w in table[..6].windows(2) {
            assert!(w[0] < w[1], "{table:?} is not increasing");
        }
    }
}

/// `Reach::of_rect` must read libaom's own `has_tr_4x8`/`has_tr_8x4`
/// rows, not the 32x16 pair the two existing rect strips use: the two
/// shapes disagree at a position inside the superblock, which is what
/// makes this an asymmetric check rather than a tautology.
#[test]
fn the_sub8_rect_reach_tables_are_shape_specific() {
    let fctx = &crate::decode::FrameCtx::new();
    let a = Reach::of_rect(4, 8, 4, 8, 64, 64, fctx);
    let b = Reach::of_rect(8, 4, 8, 4, 64, 64, fctx);
    assert!(
        a.above_right != b.above_right || a.below_left != b.below_left,
        "4x8 and 8x4 reach identically at mirrored positions -- one table is unused"
    );
}
