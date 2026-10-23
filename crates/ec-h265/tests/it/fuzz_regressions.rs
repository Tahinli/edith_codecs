//! Regression inputs found by the `decode_annexb` fuzz target (`crates/ec-h265/fuzz`).
//!
//! Each input is fed to the decoder as the fuzzer found it, through the public
//! Annex B entry point with no harness caps. Each must come back without a panic.
//! Errors are fine: a corrupt input is an error, not a crash.

use ec_h265::Decoder;

fn replay(data: &[u8]) {
    let mut decoder = Decoder::new();
    decoder.set_threads(1);
    let _ = decoder.decode_annex_b(data);
    let _ = decoder.flush();
    while let Some(frame) = decoder.next_frame() {
        std::hint::black_box(frame);
    }
}

/// Each regression input with the old test name it was filed under. The name is the case's
/// description: the comment above each input in git history says what the input broke.
///
/// - `escape_prefix_shift_overflow`: a slice whose `coeff_abs_level_remaining` escape prefix runs
///   past 16 bits; the EGk suffix shift overflowed (`1 << k` with k >= 32 in a debug build).
/// - `sps_size_not_multiple_of_min_cb`: an SPS whose width is not a multiple of the minimum coding
///   block; the right-edge coding blocks ran past the picture planes in `recon::predict_block`.
/// - `slice_qp_below_qp_bd_offset`: SliceQpY below -QpBdOffsetY; the dequantiser indexed
///   `LEVEL_SCALE` with a negative `qP % 6` in `transform::dequant_weighted`.
/// - `sps_transform_size_above_32`: a maximum transform block above 32 (or the CTB); the split-flag
///   context index `5 - log2` underflowed in `cu_syntax`.
/// - `cu_qp_delta_depth_deeper_than_ctb`: `diff_cu_qp_delta_depth` deeper than the CTB; the
///   quantization group size `CtbLog2SizeY - diff` underflowed in `cu_syntax`.
/// - `pcm_bit_depth_above_coded_depth`: PCM sample bit depth above the coded bit depth; the PCM
///   path computed `BitDepth - PcmBitDepth` in unsigned arithmetic and underflowed.
/// - `parallel_merge_level_above_ctb`: a parallel merge level above the CTB; the merge candidates
///   shifted the block position by it and the shift overflowed.
const CASES: &[(&str, &[u8])] = &[
    (
        "escape_prefix_shift_overflow_does_not_panic",
        include_bytes!("../data/fuzz/decode_annexb-escape-shift.bin"),
    ),
    (
        "sps_size_not_multiple_of_min_cb_does_not_panic",
        include_bytes!("../data/fuzz/decode_annexb-sps-not-mincb.bin"),
    ),
    (
        "slice_qp_below_qp_bd_offset_does_not_panic",
        include_bytes!("../data/fuzz/decode_annexb-slice-qp-range.bin"),
    ),
    (
        "sps_transform_size_above_32_does_not_panic",
        include_bytes!("../data/fuzz/decode_annexb-tb-size-overflow.bin"),
    ),
    (
        "cu_qp_delta_depth_deeper_than_ctb_does_not_panic",
        include_bytes!("../data/fuzz/decode_annexb-cu-qp-depth.bin"),
    ),
    (
        "pcm_bit_depth_above_coded_depth_does_not_panic",
        include_bytes!("../data/fuzz/decode_annexb-pcm-depth.bin"),
    ),
    (
        "parallel_merge_level_above_ctb_does_not_panic",
        include_bytes!("../data/fuzz/decode_annexb-par-mrg-level.bin"),
    ),
];

#[test]
fn fuzz_regressions_do_not_panic() {
    let mut failures = Vec::new();
    for &(name, data) in CASES {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| replay(data)));
        if outcome.is_err() {
            failures.push(name);
        }
    }
    assert!(failures.is_empty(), "failed cases: {failures:?}");
}
