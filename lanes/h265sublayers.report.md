# lane/h265sublayers: range checks on H.265 parameter sets

Base: `1db9803a` (main). Branch `lane/h265sublayers`. Files: `crates/ec-h265-syntax/src/ps.rs`, `crates/ec-h265-syntax/src/vui.rs`.

## Goal
`vps_max_sub_layers_minus1` and `sps_max_sub_layers_minus1` must be 0..=6 (7.4.3.1, 7.4.3.2.1). A value of 7 is refused with `Error::Corrupt` naming the field. Then check the other ranged PS fields that were parsed unchecked.

## Guards added (all return `Error::Corrupt` naming the field)
Helper: `check_range(what, field, value, lo, hi)` (pub(crate) in `ps.rs`), and `read_delta_poc` for `delta_poc_sX_minus1`.

server:
- `vps_max_sub_layers_minus1` 0..=6
- `vps_max_dec_pic_buffering_minus1` 0..=15

SPS:
- `sps_max_sub_layers_minus1` 0..=6 (the lane's required guard)
- `sps_max_dec_pic_buffering_minus1` 0..=15 (used by `dpb.rs` and ec-hw as a DPB size)
- `sps_seq_parameter_set_id` 0..=15
- Conformance window: the chroma-unit crop must leave a picture (was an unchecked u32 subtraction in `display_size`). The sum is computed in u64.
- `CtbLog2SizeY` 4..=6 (was only an upper bound, and the sum could wrap)
- `MinTbLog2SizeY` < `MinCbLog2SizeY`
- `MaxTbLog2SizeY` in MinTb..=min(CtbLog2, 5)
- `max_transform_hierarchy_depth_inter/intra` 0..=CtbLog2 - MinTbLog2

ST RPS (`ShortTermRefPicSet::parse`):
- `delta_idx_minus1 < stRpsIdx` (replaces a checked_sub on `delta_idx_minus1 + 1`, which could overflow)
- `abs_delta_rps_minus1` 0..=32767 (was `+ 1` with no bound)
- `delta_poc_sX_minus1` 0..=32767 (was `as i32 + 1` with no bound)

PPS:
- `pps_pic_parameter_set_id` 0..=63, `pps_seq_parameter_set_id` 0..=15
- `num_ref_idx_l0/l1_default_active_minus1` 0..=14
- `init_qp_minus26` -74..=25 (lower bound uses QpBdOffset 48, the maximum for bit depth 16 that the SPS allows; the PPS parser cannot see the SPS)
- `diff_cu_qp_delta_depth` 0..=3
- `pps_cb_qp_offset`, `pps_cr_qp_offset` -12..=12
- Tile count cap (20 columns, 22 rows) now applies to uniform spacing too. It was only checked for explicit spacing, and uniform spacing sizes its arrays from the count.
- `pps_beta_offset_div2`, `pps_tc_offset_div2` -6..=6
- `log2_parallel_merge_level_minus2` 0..=4 (`motion_field.rs` adds 2 to it unchecked)

VUI (`vui.rs`, inside SPS):
- `cpb_cnt_minus1` 0..=31 (was `+ 1` with no bound)

## Checked, no guard added
- `num_extra_slice_header_bits`: 3-bit field, so 0..=7 by construction.
- `log2_max_pic_order_cnt_lsb_minus4` ≤ 12, `chroma_format_idc` ≤ 3, `bit_depth_*_minus8` ≤ 8, `num_short_term_ref_pic_sets` ≤ 64, `num_long_term_ref_pics_sps` ≤ 32, short-term negative/positive picture counts ≤ 16: already guarded before this lane.
- PCM sizes: parsed, but no consumer in the decoder or ec-hw. No arithmetic on them, so no guard. Left for whoever adds a PCM consumer.
- `max_num_reorder_pics`, `max_latency_increase_plus1`: unbounded, but nothing indexes or allocates from them.
- Picture width/height: capped at 65536 (existing). Spec's "multiple of MinCbSizeY" rule is not enforced; no out-of-bounds path found.
- Scaling list and VUI HRD loops: loops are bounded by the new cpb cap or by data length.
- Parameter-set ids: ec-h265 `store()` and ec-hw table lookups already reject out-of-range ids. The new id guards are spec-exact on top of that.

## Tests added
In `ps.rs`: `seven_sub_layers_is_refused_by_name_and_six_is_not`, `sps_block_sizes_and_depths_are_range_checked`, `sps_dpb_size_and_ids_are_range_checked`, `conformance_window_must_leave_a_picture`, `vps_dpb_size_is_range_checked`, `pps_reference_counts_and_qp_are_range_checked` (includes the boundary values, which must be accepted), `pps_loop_filter_offsets_are_range_checked`, `pps_tile_count_is_capped_for_uniform_spacing_too`, `st_ref_pic_set_fields_are_range_checked`.
In `vui.rs`: `cpb_count_past_31_is_refused_and_31_is_not`.

## Verify (host a, after merging main 5923e4e2)
- `cargo clippy -p ec-h265 -p ec-h265-syntax --all-targets -- -D warnings`: clean (exit 0).
- Gate, `scripts/remote-run.sh h265sublayers --host a -- bash -c 'EC_H265_THREADS=1 EC_H265_REF_YUV_DIR=$HOME/refyuv/hevcinter cargo test -p ec-h265 -p ec-h265-syntax --release -- --nocapture 2>&1 | grep total:'`:
  `total: 9203 pictures checked, 9203 exact, 147/147 streams accepted` (exit 0).
- Same gate with `EC_H265_THREADS=6`: `total: 9203 pictures checked, 9203 exact, 147/147 streams accepted` (exit 0).

## Earlier gap (resolved)
An earlier run printed 7157 pictures. The cause was in my invocation, not the guards: `$HOME` was expanded on the
local machine, so the reference-YUV directory did not exist on the server and those comparisons were skipped.
With `$HOME` expanded on the server, the count is the required 9203. A per-stream comparison against `main` at
threads=1 also showed identical rows for all 147 streams, so no guard changed any stream's result.

## Frame-thread hang: not reproduced
An earlier run at default threads, on a tree before the merge, sat at 0% CPU in `conformance_pictures_match_the_sei_hash`.
Two default-thread gate runs on the final tree (merged with main 5923e4e2) both exit 0 with
`total: 9203 pictures checked, 9203 exact, 147/147 streams accepted` and no failures. The hang is recorded as not reproduced,
and no test was added for it. The worker path already calls `shared.fail()` on error and panic
(`crates/ec-h265/src/dec/runner.rs`), so an error mid-picture releases waiters.
