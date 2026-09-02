# lane-cdef r2 report -- merge, the 68x192 chroma residue CLOSED, gate un-ignored

Branch `lane-cdef`, base main `727f29a`. Commits this round:
* `32a467a` Merge main 727f29a (lane-r14, lane-kf1200, lane-troykf) -- no conflicts.
* `0e6c5c9` Merge lane-golomb 173b793 -- 2 conflicts in `decode.rs`, both resolved (below).
* `26b971a` straddling-band gate un-ignored + `CDEF_STRADDLE_UNITS` counter + multi-tile arms.

`cdf.rs` is byte-identical across `main`, `lane-golomb` and this tree
(`sha256 655f9a45ccedf7e37adab8b46703cf7ec4c9cc395978a1e6179157741def5844`, all four).

## r1's leftover suite unit
`cdef-suite-1788330834.service` was still RUNNING at the start of this round; it had reached
**184 tests ok, 0 FAILED / 0 panics** with no `test result` line yet. Stopped (its target dir is
this lane's) and superseded by the r2 run below.

## Merge conflicts (decode.rs), resolved
1. `dump_stage`'s per-decode-order-frame index: both lanes added one. Kept this lane's shared
   `dump_stage_idx()` helper (`dump_stage16` uses it too), dropped lane-golomb's duplicate
   `thread_local` counter.
2. The var-tx leaf loop (`decode.rs:19323`): took **main's** general `(row, col, tw, th)` rect
   leaves and **lane-golomb's** `fill_lf_grid_leaf_luma` writer. Routing a rect leaf through
   `fill_lf_grid_rect` would recompute the CHROMA tx dims from the leaf span; libaom's
   `get_transform_size` (`av1/common/av1_loopfilter.c:207`) applies the split override only for
   `plane == AOM_PLANE_Y`. Class `table-and-reader-move-together`: table (leaf tuple) from main,
   writer from golomb, both re-gated below.

## Charter item (2) -- the 68x192 CHROMA residue: CLOSED, and it was NOT a chroma-CDEF defect
The charter's hypotheses (chroma `hsize >> xdec` fill, `damping - 1`, `uv_pri/uv_sec`, shared luma
`dir`, `coeff_shift`, the 8x8 skip band) were each checked against the oracle source and all six
were **already correct** in `apply_cdef`:
* right/bottom pad bound: `sample_uv` bounds at `plane.true_width/true_height`, which is
  `mi_cols * 4 / 2` (`decode.rs:13388`) = 36 for a 68-px axis -- exactly libaom's
  `hsize = nhb << mi_wide_l2` with `mi_wide_l2 = MI_SIZE_LOG2 - subsampling_x`
  (`cdef.c:164-167`), the fill starting there being `cdef.c:249-256`.
* `damping += coeff_shift - (pli != AOM_PLANE_Y)` (`cdef_block.c:337`) == our `damping_uv`.
* chroma `t = pri_strength`, NOT `adjust_strength` (`cdef_block.c:387`) == ours.
* `pri_strength ? dir[by][bx] : 0` per plane (`cdef_block.c:392`) == ours; the `conv422/conv440`
  direction remap is `if (pli == 1 && xdec != ydec)` (`cdef_block.c:361-369`), i.e. never in 4:2:0.
* `level`/`sec_strength` from `cdef_uv_strengths[mbmi_cdef_strength]` with the `== 3` bump
  (`cdef.c:325-333`) == ours; block size `8 >> xdec` by `8 >> ydec` == our 4x4.

The residue was the **loop restoration plane size**, fixed on lane-golomb r8 and merged this
round: LR is the one post-decode filter sized by the CROP (`av1_get_upsampled_plane_size`,
`restoration.c:47`, asserted against the crop at `restoration.c:1106`), while deblock and CDEF walk
the mi grid. With both branches' fixes in one tree all four straddling arms are exact on Y, U and V.

EVIDENCE: /home/tahinli/.cache/cdef-strad-r2.log | merged tree, `cargo test -p ec-av1 --lib -- --ignored --nocapture a_frame_edge_straddling_band` (192x68 + 68x192, 5 frames, 8+10-bit, cq 35..61) | 14 pixel-exact attempts (Y, U and V, every shown frame), 0 differing pixels, 18 named refusals, test result ok 1 passed / 0 failed

## Charter item (3) -- `--tile-columns=1` arms: added, and VACUOUS today
Each straddling arm gained a `--tile-columns=1` twin (8 arms x 8 cq = 64 attempts). All 32
multi-tile attempts refuse **by name**; census (in the test's doc comment):
10x "an inter SB-level AB partition", 6x "a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs
rectangular residual coding", 6x "an 8x8 intra leaf in an inter frame whose tx_depth splits it into
4x4 transform units", 5x "an inter partition below 8x8", 3x "an inter 16x16-level AB or 1:4
partition", 1x "a split intra strip whose transform unit is 32x64", 1x "an intra-coded 1:4 rect
strip on the inter block path". Class `counter-from-refused-stream`: that half of the gate compares
nothing yet and every assert is carried by the single-tile arms. Arms kept so the coverage appears
the day those refusals lift.

## Gate
`stream.rs::a_frame_edge_straddling_band_decodes_pixel_exact` -- **un-ignored** (was
`#[ignore] "open r9 defect"`). Hard-asserts the new `decode::cdef_straddle_units()` grew: a FILTERED
8x8 CDEF unit whose extent crosses the cropped frame edge is the only shape for which "clamp at the
crop" and "pad with CDEF_VERY_LARGE past the mi-rounded extent" differ, so a zero delta would make
these arms vacuous for this defect.

EVIDENCE: /home/tahinli/.cache/cdef-strad2-r2.log | `cargo test -p ec-av1 --lib -- --nocapture a_frame_edge_straddling_band` on the final tree | 14 pixel-exact attempts, **337 straddling CDEF units**, 50 named refusals, ok 1 passed / 0 failed in 33.99s

Sibling gate (same tables/paths -- `apply_cdef`, the skip band, `fill_skip_grid`):
EVIDENCE: /home/tahinli/.cache/cdef-sib-r2.log | `cargo test -p ec-av1 --lib -- --nocapture cdef_and_sub16` | a_real_aomenc_stream_with_cdef_and_sub16_inter_leaves_decodes_pixel_exact ok, 1 passed / 0 failed

## Suite
EVIDENCE: /home/tahinli/.cache/cdef-suite-r2.log | `systemd-run --user --unit=cdef-suite-r2-... -p MemoryMax=10G ... cargo test -p ec-av1 --lib -j3` on `646f2d3`'s tree | **386 passed, 0 failed, 34 ignored** in 1009.29s (r1's tree had 385/0/35 -- one test moved out of `ignored` and into `passed`: this round's un-ignored straddling gate)

## Refusals
None lifted this round, so `refusal_inventory.rs` is unchanged. `gate_coverage.rs` needs no entry
either: its derivation deliberately excludes `--tile-columns` (see its header comment) and this
round adds no `--enable-*` flag.

## Residue
* deferred(the 7 refusals in the census above) -- the multi-tile half of the straddling gate.
* accepted -- the gate compares SHOWN frames (`ffmpeg_decode_sequence`); no-show alt-ref frames are
  not compared here, same as every sibling gate (class `gate-blind-to-hidden-frames`).
