//! Regression inputs found by the `parse_params_slice` fuzz target (`crates/ec-h265/fuzz`).
//!
//! The input is parsed the way the target parses it: every VPS, SPS and PPS, and each
//! slice header against the latest SPS and PPS. Each must come back without a panic
//! or an allocation the input did not justify.

use ec_h265_syntax::slice::SliceHeader;
use ec_h265_syntax::{NalUnitType, Pps, Sps, Vps, split_annex_b};

fn parse_params_slice(data: &[u8]) {
    let mut sps: Option<Sps> = None;
    let mut pps: Option<Pps> = None;
    for nal in split_annex_b(data) {
        let rbsp = nal.rbsp();
        match nal.header.nal_type {
            NalUnitType::Vps => {
                let _ = Vps::parse(&rbsp);
            }
            NalUnitType::Sps => {
                if let Ok(parsed) = Sps::parse(&rbsp) {
                    sps = Some(parsed);
                }
            }
            NalUnitType::Pps => {
                if let Ok(parsed) = Pps::parse(&rbsp) {
                    pps = Some(parsed);
                }
            }
            t if t.is_vcl() => {
                if let (Some(s), Some(p)) = (&sps, &pps) {
                    let _ = SliceHeader::parse(&rbsp, s, p, t);
                }
            }
            _ => {}
        }
    }
}

/// A slice header whose `num_ref_idx_l0_active_minus1` comes from the PPS default
/// and is out of range: `pred_weight_table` sized a Vec from it (a 2.6 GB malloc).
#[test]
fn pps_default_num_ref_idx_out_of_range_does_not_allocate() {
    parse_params_slice(include_bytes!("data/fuzz/parse_params_slice-pred-weight-oom.bin"));
}

/// Long-term reference pictures whose `delta_poc_msb_cycle_lt` deltas accumulate past
/// `u32::MAX` (7-52): the running sum overflowed in `SliceHeader::parse`.
#[test]
fn delta_poc_msb_cycle_accumulation_does_not_overflow() {
    parse_params_slice(include_bytes!("data/fuzz/parse_params_slice-delta-msb-overflow.bin"));
}

/// A short-term reference picture set whose `delta_poc_s0_minus1` is far past its
/// 2^15 - 1 range: the running POC sum overflowed (`attempt to subtract with overflow`).
#[test]
fn short_term_rps_delta_poc_out_of_range_does_not_overflow() {
    parse_params_slice(include_bytes!("data/fuzz/parse_params_slice-rps-delta-overflow.bin"));
}

/// An inter-predicted short-term RPS whose `abs_delta_rps_minus1` is out of range:
/// `delta_rps` and the POC sums built from it overflowed `i32`.
#[test]
fn inter_rps_abs_delta_out_of_range_does_not_overflow() {
    parse_params_slice(include_bytes!("data/fuzz/parse_params_slice-inter-rps-overflow.bin"));
}

/// A pred_weight_table whose chroma log2 weight denominator is the luma one plus a
/// near-`i32::MAX` delta: the sum overflowed.
#[test]
fn pred_weight_chroma_denom_sum_does_not_overflow() {
    parse_params_slice(include_bytes!("data/fuzz/parse_params_slice-weight-denom-overflow.bin"));
}

/// A slice header whose long-term picture counts sum past `u32::MAX`: the loop bound
/// `num_long_term_sps + num_long_term_pics` overflowed.
#[test]
fn long_term_picture_count_sum_does_not_overflow() {
    parse_params_slice(include_bytes!("data/fuzz/parse_params_slice-long-term-count.bin"));
}

/// A slice header whose entry point offset is read as the all-ones 32-bit value: the
/// `+ 1` that makes it a length overflowed.
#[test]
fn entry_point_offset_all_ones_does_not_overflow() {
    parse_params_slice(include_bytes!("data/fuzz/parse_params_slice-entry-point-overflow.bin"));
}
