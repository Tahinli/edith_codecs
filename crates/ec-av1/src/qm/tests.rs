use super::*;

/// The level-0 luma 4x4 corner, read off the C file by eye as well as by
/// the extractor: a table that shifted a chunk boundary moves this.
#[test]
fn table_anchor_level0_luma_4x4() {
    let m = iwt_matrix(0, false, 4, 4).unwrap();
    assert_eq!(
        m.data,
        &[32, 43, 73, 97, 43, 67, 94, 110, 73, 94, 137, 150, 97, 110, 150, 200]
    );
    assert_eq!(m.stride, 4);
}

/// Chroma matrices are DIFFERENT data at the same level, not a re-read
/// of the luma ones (`iwt_matrix_ref[q][c >= 1]`).
#[test]
fn chroma_matrix_at_the_same_level_differs_from_luma() {
    let luma = iwt_matrix(0, false, 4, 4).unwrap();
    let chroma = iwt_matrix(0, true, 4, 4).unwrap();
    assert_eq!(
        chroma.data,
        &[35, 46, 57, 66, 46, 60, 69, 71, 57, 69, 90, 90, 66, 71, 90, 109]
    );
    assert_ne!(luma.data, chroma.data);
}

/// Every level selects a matrix below 15 and nothing does at 15 (the
/// flat sentinel libaom stores as NULL), for every stored shape and
/// both plane kinds; the shape lengths follow the walk.
#[test]
fn level_and_shape_selection_matches_the_walk() {
    const SHAPES: [(usize, usize, usize); 14] = [
        (4, 4, 16),
        (8, 8, 64),
        (16, 16, 256),
        (32, 32, 1024),
        (4, 8, 32),
        (8, 4, 32),
        (8, 16, 128),
        (16, 8, 128),
        (16, 32, 512),
        (32, 16, 512),
        (4, 16, 64),
        (16, 4, 64),
        (8, 32, 256),
        (32, 8, 256),
    ];
    for level in 0..NUM_QM_LEVELS {
        for chroma in [false, true] {
            for &(w, h, len) in SHAPES.iter() {
                let m = iwt_matrix(level, chroma, w, h);
                if level >= NUM_QM_LEVELS - 1 {
                    assert!(m.is_none());
                } else {
                    let m = m.unwrap();
                    assert_eq!(m.data.len(), len, "{level} {chroma} {w}x{h}");
                    // the stride is the column stride of libaom's
                    // column-major position space: the active HEIGHT.
                    assert_eq!(m.stride, h);
                }
            }
        }
    }
}

/// The 64-axis clamps (`av1_get_adjusted_tx_size`): 64x64/64x32/32x64
/// read the 32x32 matrix, 64x16 the 32x16 one, 16x64 the 16x32 one --
/// the same slice as their adjusted shapes.
#[test]
fn clamped_64_shapes_reuse_their_adjusted_matrix() {
    let sq = iwt_matrix(3, false, 32, 32).unwrap();
    for &(w, h) in [(64usize, 64usize), (64, 32), (32, 64)].iter() {
        let m = iwt_matrix(3, false, w, h).unwrap();
        assert_eq!(m.data.as_ptr(), sq.data.as_ptr(), "{w}x{h}");
        assert_eq!(m.stride, 32);
    }
    let r3216 = iwt_matrix(3, true, 32, 16).unwrap();
    let m = iwt_matrix(3, true, 64, 16).unwrap();
    assert_eq!(m.data.as_ptr(), r3216.data.as_ptr());
    assert_eq!(m.stride, 16);
    let r1632 = iwt_matrix(3, true, 16, 32).unwrap();
    let m = iwt_matrix(3, true, 16, 64).unwrap();
    assert_eq!(m.data.as_ptr(), r1632.data.as_ptr());
    assert_eq!(m.stride, 32);
}

/// `dqv_at` is libaom's `get_dqv` verbatim: an entry of 32 (`1 <<
/// AOM_QM_BITS`) is the identity, rounding is half-up into the shift,
/// and a position on a later ROW of a true-w raster re-strides onto the
/// matrix stride.
#[test]
fn dqv_scales_and_rounds_like_get_dqv() {
    let m = iwt_matrix(0, false, 8, 8).unwrap();
    // DC: entry 32 is the exact identity.
    assert_eq!(dqv_at(&m, 0, 8, 100), 100);
    // pos 1 = cell (0,1) = column-major index 1*8+0 = 8, entry 32
    // (row-major would read data[1] = 32 too at this corner -- the
    // shapes coincide at the top-left corner): exact.
    assert_eq!(dqv_at(&m, 1, 8, 100), 100);
    // pos 8 = cell (1,0) = column-major index 0*8+1 = 1, entry 32:
    // (32 * 10 + 16) >> 5 = 10. Row-major indexing would have read
    // data[8] = 32 as well here -- but at pos 1 they differ (32 vs 35),
    // which is how the oracle exposed the transposition.
    assert_eq!(dqv_at(&m, 8, 8, 10), ((32 * 10) + 16) >> 5);
}
