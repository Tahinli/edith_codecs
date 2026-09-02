# lane-inter16ab r4 — report (commit 36a5542)

## 1. PANIC RISK closed (the r3 handoff's item 3) — and it was already firing
`sub_tx_size_map[TX_16X4] == TX_8X4`, so after the lane-r14 merge a split-transform
16x4/4x16 strip reaches `rect_inter_residual_supported(8,4) == false` and
`rect_inter_luma_set`'s `unreachable!()`. Not hypothetical: the r3 SUITE failed
`a_real_aomenc_10bit_inter_sequence_with_a_directional_intra_block_with_angle_delta_decodes_pixel_exact`
with `panicked at decode.rs:11799: rect_inter_residual_supported gates every shape that reaches here`.

- `cdf_state.rs`: `TxbSet::LumaRect8x4Inter{,1}` — `LumaRect4x8`'s shape (`Luma8`
  coefficient tables, true 32-position `eob_pt_32_luma{,_class1}`) with the INTER
  `tx_type` table at `txsize_sqr_map[TX_8X4] = TX_4X4`; `av1_get_ext_tx_set_type`
  (blockd.h:1097) at `tx_size_sqr_up == TX_8X8` gives `EXT_TX_SET_DCT_IDTX` when
  reduced (`inter_tx_type_4`), `EXT_TX_SET_ALL16` otherwise (`inter_tx_type_4_set1`).
- `decode.rs:rect_inter_residual_supported` += `(8,4)|(4,8)`, reachable only as a
  var-tx LEAF (an 8x4 inter BLOCK still refuses with "an inter partition below 8x8").
- `decode.rs`: `rect_inter_luma_set` / `rect_inter_chroma_set` / `rect_scan` now return
  `Result<..>`; the three `unreachable!()`s on stream-derived shapes became named
  refusals (added to `refusal_inventory::REFUSALS`). `SCAN_8X4`/`SCAN_4X8` already existed.
- Counter `vartx_rect_leaf4_hits() -> [8x4, 4x8]` (decode.rs + stream.rs + decode_probe).

EVIDENCE: $HOME/.cache/inter16ab-suite-r3.log (panic) vs the post-fix run |
`cargo test -p ec-av1 --lib a_real_aomenc_10bit_inter_sequence_with_a_directional_intra_block_with_angle_delta -- --nocapture` |
`test result: ok. 1 passed` (was: panic at decode.rs:11799).

## 2. CLASS SWEEP of the r3 root cause (CDF row / table index taken from the enclosing square `side`)
Swept every `match side {` / `side ==` / `[side]` reaching a CDF row or table index on a
path that can carry a rectangular block (decode.rs 16400..19100 = `decode_inter_block`).
Three sites were wrong, all now keyed by `write_w`/`write_h`:

| site | before | after | why |
|---|---|---|---|
| `is_any_masked_compound_used_here` (decode.rs ~16089, call ~16920) | `side.min(side) >= 8` | `bw.min(bh) >= 8` | `is_comp_ref_allowed(bsize)` is min of the TRUE dims; a 16x4/4x16 strip must read NO `comp_group_idx` symbol (class equal-range-means-unread) |
| palette ctx in the intra-in-inter branch (~19000) | `palette_bsize_ctx(side)` | `palette_bsize_ctx_wh(write_w, write_h)` | `av1_get_palette_bsize_ctx` is per-bsize; a 16x4 (64-pixel) strip reads no palette symbol at all |
| filter-intra class, same branch (~19025) | `filter_intra_size_class(side)` | `filter_intra_size_class_rect(write_w, write_h)` | `cdf::FILTER_INTRA` has its own rows for every rect bsize (12/13 for 16x4/4x16) |

Verified NOT defective (already keyed on the true footprint or genuinely square):
`motion_mode`/`obmc` (`bsize_idx` from `(write_w, write_h)` with a named refusal default),
`interintra`/`interintra_mode` (`size_group_wh`), `compound_type`/`wedge_idx`
(`bsize_all_index`, r3), `y_mode` (`size_group_wh`), `comp_group_idx`/`compound_idx`
CONTEXTS (neighbour-derived; their `side` parameter is `let _ = side`), and the whole
8x8-leaf decoder (`decode_leaf8`, constant rows).

## 3/4. Gate arms — still RED, and the blocker is now identified, not guessed
`a_real_aomenc_inter_sequence_with_16x16_level_1to4_partitions_decodes_pixel_exact`
(log `$HOME/.cache/inter16ab-gate-r4.log`): 8-bit `[HORZ_4=1, VERT_4=0, chroma-pair=1,
sub8x8=1, split-tx-8x4=0]`, 10-bit all zero, `oos_mismatch = 0` at BOTH depths, no pixel
mismatch anywhere. The 5th (split-transform 8x4/4x8 leaf) arm is wired from the new counter.

Recipe widening was measured OUTSIDE the test (`$HOME/.cache/inter16ab-tmp/sweep{,2,3,4}.sh`,
histogram `hist.py`) over 100+ aomenc streams: sources (2 translating sinusoid pairs + a fine
`sin(X/3)` stripe pair), cq 8..55, sp 3/8, tx-size-search 0/1, 8- and 10-bit, 192x128 and
320x192, `--min-partition-size=4 --enable-1to4-partitions=1 --enable-rect-partitions=1`,
with and without `--enable-interintra-comp=0`.

- First instrument was WRONG and is recorded as such: `EC_PART_VAL bsize=6 value=8|9` counted
  over the whole stream finds plenty of 1:4 partitions at low cq — every one of them in the
  KEY frame. `hist.py` segments the aomdec trace per frame (a new frame starts at
  `EC_PART mi_row=0 mi_col=0 bsize=12`) and counts only frames >= 1.
- Frame-aware result: across the whole sweep exactly ONE stream carries an inter-frame 1:4 at
  BLOCK_16X16 — `s_8_18_x_3_1_16.obu` (8-bit, cq 18, X-structured source, sp 3, tx-search 1),
  `INTER {4: 2, 9: 1}` = one `PARTITION_VERT_4`. That recipe IS the gate's attempt 1, and our
  decoder refuses it: `a wedge interintra mask on a rectangular inter block (the wedge codebook
  is square-only)` (this lane's own r3 refusal).
- Disabling interintra to dodge that refusal changes aomenc's RD and the VERT_4 disappears
  (16 fresh streams, cq 16..24 x/y sp 3/8 both depths: zero inter-frame 1:4).
- A 4-recipe "fine stripe" widening was wired, run, and REVERTED in the same round once the
  frame-aware histogram showed its partitions were key-frame ones (the gate diff keeps only the
  comment recording that).

Conclusion: VERT_4 and split-tx-8x4 are blocked on RECT WEDGE MASK SUPPORT, not on recipe search.

EVIDENCE: $HOME/.cache/inter16ab-tmp/s_8_18_x_3_1_16.obu | `hist.py` on it, then
`decode_probe s_8_18_x_3_1_16.obu` | aomdec inter-frame histogram `{HORZ_A:2, VERT_4:1}`;
ours REFUSED with the rect wedge-interintra string.

## Film re-probe (10-bit 2160p HDR, -ss 900, 2 s)
`decode_probe` stops at: `a COMPOUND_WEDGE mask on a rectangular inter block (the wedge codebook
is square-only)` (was, r3: the same lane's compound path decoded wrongly instead of refusing).
104 frame headers parsed, seq 3840x1608 10-bit, 1 tile; no `EC_AV1_FINAL_DUMP` frame reached.
So the FILM's next blocker is the same rect wedge codebook.

EVIDENCE: $HOME/.cache/inter16ab-tmp/hg900.obu | `ffmpeg -ss 900 -t 2 -c:v copy -f obu` then
`EC_AV1_FINAL_DUMP=1 decode_probe` | stop string above, 0 frames decoded.

## Residue
- fix-now (next round): rectangular wedge masks — `av1_init_wedge_masks` builds BLOCK_8X16 /
  16X8 / 16X32 / 32X16 / 8X32 / 32X8 from the same master mask as the square sizes. Lifts BOTH
  new refusals, unblocks the gate's VERT_4 arm (attempt 1 carries one) AND the film at -ss 900.
- deferred(rect wedge): the gate's `arms_total` assert — VERT_4, and with it the split-tx-8x4
  arm, cannot fire on any recipe found in a 100+ stream sweep while attempt 1 refuses.
- accepted: 10-bit 1:4 in an INTER frame was not produced by any recipe swept.

## Suite
`systemd-run --user --unit=inter16ab-suite-r4 -p MemoryMax=10G ... cargo test -p ec-av1 --lib -j3`
-> `$HOME/.cache/inter16ab-suite-r4.log`: **382 passed; 1 failed; 33 ignored** (866 s).
The single failure is this lane's own 1:4 gate, on its `arms_total` assert (no pixel mismatch,
`oos_mismatch = 0`). r3 was 381 passed / 2 failed — the 10-bit angle-delta PANIC is gone.
