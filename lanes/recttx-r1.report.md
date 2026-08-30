# lane-recttx r1 report

VERDICT: PASS — all 14 rectangular inverse-transform sizes pinned against real
libaom kernels; square path proven bit-identical; lib suite 226 passed / 0
failed (224 pre-lane + the 2 new tests).

## What landed
`transform.rs`'s inverse path is now `(w, h)`:
`inverse_transform_2d_typed_wh` and `dequant_and_inverse_typed_wh` do the work,
and the old `side`-taking entry points are thin wrappers, so no existing call
site moved. `quant.rs` gained `dq_denom_area` / `dequant_coeff_wh` /
`dequant_wh` the same way.

Two values carry the rectangular behaviour, both cited in the code:
- the rect scale `round2(c * 2896, 12)`, applied to each row's input **only**
  when `|log2w - log2h| == 1` (`av1_inv_txfm2d.c:272-276`);
- `row_shift_wh(w, h)`, all 19 sizes from `av1_inv_txfm_shift_ls`
  (`av1_inv_txfm2d.c:132-158`). The column shift is 4 everywhere, which is what
  the code already did.

## Per-size status — all 14 pinned, all passing
4x8, 8x4, 8x16, 16x8, 16x32, 32x16, 32x64, 64x32, 4x16, 16x4, 8x32, 32x8,
16x64, 64x16.

Checksums live in `lanes/recttx_dump.expected.txt` and are duplicated as
literal values in `transform.rs::tests::rect_sizes_pinned_against_libaom`. The
probe coefficient block is asymmetric by construction (DC plus a
`(i+1)*24 - (j+1)*17` corner), because a symmetric probe cannot see an axis
swap — the failure this lane was most likely to ship.

`square_wrapper_matches_wh_core` runs all five square sizes through both the
wrapper and the new core and requires byte-identical output, so the square
path's inertness is proven by value and not merely by a green suite.

## Dead end, recorded so nobody repeats it
The `av1_inv_txfm2d_add_WxH_c` facades segfault when called standalone from a
bare harness linked against the oracle's `libaom.a`, even with the real
`config/av1_rtcd.h` declaration — a null function-pointer call inside
`inv_txfm2d_add_c`, consistent with `inv_txfm_type_to_func`'s
`assert(0); return NULL;` being compiled out under NDEBUG. Why that branch is
reached for a trivially valid `DCT_DCT` index was not found. The harness
instead calls the real `av1_idct4/8/16/32/64` 1D kernels directly and
transcribes the 2D loop, so the part libaom actually contributes — the butterfly
math — is still linked rather than reimplemented.

## What this does NOT do
Nothing in `decode.rs` was touched. Every one of the 14 sizes is proven at the
transform-math level only. To USE this, a follow-up lane still needs:
- rectangular scan order (only square zig-zag/diagonal tables are wired today);
- rectangular eob context (the `get_eob_ctx` family is keyed on a square tx size
  class);
- `max_txsize_rect_lookup` threading through partition and tx-size selection.

Those three are what stands between this primitive and lifting
`decode.rs`'s refusal of inter partitions below 16x16, and the intra HORZ/VERT
strips' skip-only restriction.
