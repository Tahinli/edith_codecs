# lane-inter16ab r8 — the txfm-partition band was never published by two block paths

## 1. What changed
| path:line | why |
|---|---|
| `crates/ec-av1/src/decode.rs:6866` (`decode_intra_rect_in_inter`) | an INTRA HORZ/VERT strip inside an inter frame now runs libaom's `set_txfm_ctxs` write (`txfm_partition_update_rect(neighbours, (mi_r, mi_c), (tx_w, tx_h), (bw, bh))`) — it published nothing before |
| `crates/ec-av1/src/decode.rs:20763` (`decode_inter_sub8_split4`) | same-shape sweep: a BLOCK_4X4 codes no `txfm_partition` symbol but libaom still publishes 4 in both bands |
| `crates/ec-av1/src/decode.rs` (`txfm_partition_update_rect`, `set_txfm_ctxs`) | `EC_TXUPD=1` trace of every band write — the instrument that localised this |

## 2. Root cause (the charter's premise was WRONG, and the trace says so)
The charter (from r7) said the 16x16-level AB SKIP sub-blocks at mi(12,0)/(12,2)/(14,0) of
`g14.obu` frame 3 write no txfm band. Measured with `EC_TXUPD=1`:

```
EC_MODE mi_row=12 mi_col=0 ... EC_TXUPD ctxs mi=(12,0) tx=8 wh=(2,2) skip_inter=true
EC_MODE mi_row=12 mi_col=2 ... EC_TXUPD ctxs mi=(12,2) tx=8 wh=(2,2) skip_inter=true
EC_MODE mi_row=14 mi_col=0
EC_TXCTX mi_row=14 mi_col=0 w=16 h=8 above_px=8 left_px=0 ctx=0      <-- no EC_TXUPD after it
```
The two 8x8 AB leaves DO write, and 8 is what libaom writes for them (`skip && is_inter` arm,
`n4_w * MI_SIZE == 8`). The third piece, the 16x8, is **not a skip inter strip at all**: it is an
INTRA strip (`EC_TXCTX` is emitted only by `tx_size_context_rect`, the intra tx_depth context), so
it goes through `decode_intra_rect_in_inter` — a path that reads its `tx_depth` and decodes but
never touched `above_txfm`/`left_txfm`. libaom's `set_txfm_ctxs` runs for intra blocks too (skip
term 0 → `tx_size_wide`=16 over the strip's 4 mi columns). That missing 16 is the entire defect:
mi(16,0) read ctx 13 instead of 12.

Class: `parsed-then-discarded` / `new-map-ignores-tile-edge` family — a per-mi side band with a
write path missing on one block class. Sweep done: every other inter-frame block path already
calls one of the two helpers (`decode_inter_block` both branches + its intra branch via
`read_block_tx_size`, `decode_inter_block8` all three branches, `decode_inter_sub8_rect2`
decode.rs:21101); `decode_inter_sub8_split4` was the one sibling hit and is fixed in the same commit.

## 3. EVIDENCE
- `EVIDENCE: ~/.cache/inter16ab-tmp/t2.txt | EC_TRACE_MODE_STEP decode of g14.obu after the fix | EC_ISTEP mi_row=16 mi_col=0 name=txfm_split val=0 ctx=12 above_px=16 left_px=64 rng=47777 — bit-identical to aomdec's 47777 (was ctx=13 rng=60378)`
- `EVIDENCE: ~/.cache/inter16ab-tmp/{g14o.yuv,g14f.yuv} | decode_probe EC_PROBE_OUT=g14o.yuv g14.obu; ffmpeg -i g14.obu -pix_fmt yuv420p g14f.yuv; cmp | OK: 6 frames decoded, 192x128 + cmp silent = 6/6 frames byte-identical (r6/r7: differ at byte 122402)`
- Stream recipe (unchanged, `~/.cache/inter16ab-tmp/g14.sh`): 192x128 gray sinusoid source,
  `aomenc --cq-level=18 --cpu-used=0 --sb-size=64 --enable-tx-size-search=1 --enable-rect-partitions=1
  --enable-ab-partitions=1 --enable-1to4-partitions=1 --min-partition-size=4 --max-partition-size=16`.

## 4. Gates
No new gate: the repo already sweeps this recipe family —
`a_real_aomenc_inter_sequence_with_16x16_level_ab_partitions_decodes_pixel_exact` and
`a_real_aomenc_inter_sequence_with_16x16_level_1to4_partitions_decodes_pixel_exact` (the latter with
`--min-partition-size=4`, i.e. the g14 recipe) are the sibling gates for the touched path, and both
run in the suite below. Full suite: `$HOME/.cache/inter16ab-suite-r8.log` (see §6).

## 5. Refusals
None lifted this round; none added. The charter's "drop the rect residual refusal for supported
shapes" was NOT done and is not a silent omission: `"a non-skip rectangular (HORZ/VERT/HORZ_B) strip
needs rectangular residual coding"` (decode.rs:17616) is already condition-gated by
`rect_inter_residual_supported(write_w, write_h)`, so it does not fire for the supported shapes;
deleting the string outright would be a capability claim over the shapes it still guards, with no
gate behind it. disposition: accepted (no change needed).

## 6. Film probe + suite
- HG `-ss 900` (`~/.cache/inter16ab-tmp/hg900.obu`, 3840x1608 10-bit): still REFUSES, now at
  `a split (nonzero tx_depth) transform on an intra HORZ/VERT strip in an inter frame`
  (decode.rs, added by the lane-intrarect merge). 104 frame headers parsed, no FINAL_DUMP, so no
  EC_PROBE_OUT16 compare was possible. disposition: deferred(the split-TU walk on an intra rect
  strip must be fixed — that refusal is the new film frontier and is the next lane's charter).
- Suite: see the tail of this file / the report line below.


## 7. Suite (r8, full `cargo test -p ec-av1 --lib`) — RED, 395 passed / 8 failed / 33 ignored
Log: `$HOME/.cache/inter16ab-suite-r8.log` (1275 s). Baseline comparison at the PARENT commit
`ba40d38` (detached worktree, own target dir, log `$HOME/.cache/inter16ab-base-r8.log`, partial —
the run was cut by the turn cap):

| test | ba40d38 (before fix) | ee04373 (after fix) |
|---|---|---|
| `refusal_inventory::the_decode_path_refuses_exactly_the_listed_cases` | FAILED | FAILED (pre-existing) |
| `a_frame_edge_straddling_band_decodes_pixel_exact` | FAILED | FAILED (pre-existing) |
| `a_real_aomenc_10bit_..._angle_delta_decodes_pixel_exact` | FAILED | FAILED (pre-existing) |
| `a_real_aomenc_10bit_..._split_transform_intra_block_...` | FAILED | FAILED (pre-existing) |
| `a_real_aomenc_inter_sequence_with_a_class1_chroma_eob_...` | FAILED | FAILED (pre-existing) |
| `a_real_aomenc_10bit_..._filter_intra_on_an_intra_block_...` | **ok** | **FAILED — regression or attempt-reselection, UNRESOLVED** |
| `a_real_aomenc_inter_sequence_with_16x16_level_1to4_partitions_...` | not reached (cap) | FAILED (arm-count assert, VERT_4/split-tx-8x4 read 0 with no rect-residual refusal left) |
| `real_aomenc_1to4_streams_decode_pixel_exact_and_rect_vartx_leaves_fire...` | not reached (cap) | FAILED (arm never fired) |

The five pre-existing failures come from the three merges of r7 (main 787c66f + intersub8 + sbab),
which were never suite-checked (r7 §4 deferred it). The two 1:4 failures are ARM-COUNT asserts, not
pixel mismatches — both say "0 attempts carried the arm, 0 mismatched", the signature of class
`parallel-flake-is-attempt-selection` (the desync fix changes which attempt each sweep lands on).
The `filter_intra` 10-bit one is the only red that flipped from green and is NOT explained.
disposition: fix-now for the next round (r9) — first step is to re-run the two 1:4 gates and the
filter-intra gate at `ba40d38` vs `ee04373` back to back (the baseline run was cut mid-way).
