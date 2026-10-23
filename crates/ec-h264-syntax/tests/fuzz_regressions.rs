//! Regression inputs found by the `h264_build_parameters` fuzz target
//! (`crates/ec-hw/fuzz`).
//!
//! Each input must be refused without a panic.

use ec_h264_syntax::{AnnexBIter, NalHeader, NalUnitType, Pps, unescape_rbsp};

/// A PPS whose scaling list carries a `delta_scale` far outside -128..=127. The sum
/// `last_scale + delta_scale + 256` in `parse_scaling_list` overflowed, which panics
/// in a debug or fuzzing build. Out-of-range values are corrupt (7.4.2.1.1).
#[test]
fn pps_scaling_delta_out_of_range_is_corrupt_not_a_panic() {
    let data = include_bytes!("data/fuzz/pps-scaling-delta-overflow.bin");
    let nal = AnnexBIter::new(data)
        .next()
        .expect("the fixture holds one NAL unit");
    let (&first, payload) = nal.split_first().expect("a NAL header byte");
    let header = NalHeader::parse(first).expect("a valid NAL header");
    assert_eq!(header.unit_type, NalUnitType::Pps);
    let mut rbsp = Vec::new();
    unescape_rbsp(payload, &mut rbsp);
    let err = Pps::parse(&rbsp, |_| None).expect_err("an out-of-range delta_scale is corrupt");
    assert!(err.to_string().contains("delta_scale"), "wrong refusal: {err}");
}
