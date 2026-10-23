use super::obmc_skip_chroma_above;

/// lane-av1-llpred2: the above-chroma skip keys off the PLANE block
/// under the frame's own ss pair. At 4:2:0 a 16x8/8x16 rect strip's
/// plane block is 8x4/4x8 and MUST still skip (the old `chroma_side`
/// ratio read ss=(1,0) off a 16x8 strip and made it blend); at 4:4:4
/// the plane block is the luma block, so only true 4-wide/high shapes
/// skip and the shift never leaves `usize` range (the old ratio's
/// `trailing_zeros(0)` = 64 panicked a debug build).
#[test]
fn rect_strips_skip_above_chroma_at_420_and_shift_never_overflows() {
    // 4:2:0 (ss 1,1): 8x8 -> 4x4, 16x8 -> 8x4, 8x16 -> 4x8 all skip.
    assert!(obmc_skip_chroma_above(8, 8, 1, 1));
    assert!(obmc_skip_chroma_above(16, 8, 1, 1));
    assert!(obmc_skip_chroma_above(8, 16, 1, 1));
    // ... larger plane blocks blend, exactly libaom's own list.
    assert!(!obmc_skip_chroma_above(16, 16, 1, 1));
    assert!(!obmc_skip_chroma_above(32, 16, 1, 1));
    assert!(!obmc_skip_chroma_above(16, 32, 1, 1));
    // 4:4:4 (ss 0,0): the plane block is the luma block itself.
    assert!(obmc_skip_chroma_above(4, 4, 0, 0));
    assert!(obmc_skip_chroma_above(8, 4, 0, 0));
    assert!(obmc_skip_chroma_above(4, 8, 0, 0));
    assert!(!obmc_skip_chroma_above(8, 8, 0, 0));
    assert!(!obmc_skip_chroma_above(16, 8, 0, 0));
    assert!(!obmc_skip_chroma_above(8, 16, 0, 0));
}
