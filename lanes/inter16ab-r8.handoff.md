# lane-inter16ab r8 — HANDOFF (turn cap; root cause FOUND and fixed, suite RED with mostly pre-existing reds)

Tip: `ee04373` on `lane-inter16ab`.

## 1. g14.obu — GREEN
`~/.cache/inter16ab-tmp/g14.sh` (192x128 8-bit cq18 tx-size-search=1, ab + 1:4, min-part 4).
`decode_probe EC_PROBE_OUT=g14o.yuv g14.obu` → `OK: 6 frames decoded, 192x128`;
`cmp g14o.yuv g14f.yuv` (ffmpeg) silent → **6/6 frames byte-exact** (r6/r7: differed at byte 122402).
The former first divergence, frame 3 / block mi(16,0) / symbol `txfm_split`, now reads
`ctx=12 above_px=16 left_px=64 rng=47777` = aomdec's range exactly (was ctx=13, rng=60378).

## 2. What changed (and the charter's premise was WRONG)
The charter said the 16x16-level AB **SKIP sub-blocks** at mi(12,0)/(12,2)/(14,0) write no txfm band.
Measured with a new `EC_TXUPD=1` trace on both band writers: the two 8x8 AB leaves DO write, and 8 is
what libaom writes for them (`set_txfm_ctxs`' `skip && is_inter` arm, `n4_w*MI_SIZE == 8`). **Nothing
was wrong with the AB skip sub-blocks and nothing was changed there.** The third piece, the 16x8, is
an **INTRA** strip (it emits `EC_TXCTX`, which only `tx_size_context_rect` prints), decoded by
`decode_intra_rect_in_inter` (decode.rs:6726) — a path that published NO txfm band at all, while
libaom runs `set_txfm_ctxs` for intra blocks too (skip term 0 → `tx_size_wide`=16 over 4 mi cols).

Fix (commit `ee04373`), 2 lines + comments:
- `decode.rs:6866` `decode_intra_rect_in_inter`: `txfm_partition_update_rect(neighbours, (mi_r, mi_c), (tx_w, tx_h), (bw, bh))` right after `depth_to_tx_wh`.
- `decode.rs:20763` `decode_inter_sub8_split4` (same-shape sweep): `set_txfm_ctxs(neighbours, lmi, B4, 1, 1, false)` per 4x4 sub-block (libaom publishes 4 there; a BLOCK_4X4 codes no symbol but still writes).
- `txfm_partition_update_rect` / `set_txfm_ctxs`: `EC_TXUPD=1` trace of every band write (the instrument that localised this; keep it).
Sweep done: every other inter-frame block path already writes — `decode_inter_block` (both inter
branches at 18473/19561 via `read_block_tx_size_rect`, intra branch at 20092 via `read_block_tx_size`),
`decode_inter_block8` (all three branches), `decode_inter_sub8_rect2` (decode.rs:21101).

## 3. Gates run, by name
Full suite `cargo test -p ec-av1 --lib` → `$HOME/.cache/inter16ab-suite-r8.log`:
**395 passed, 8 failed, 33 ignored** (1275 s). Baseline of the same 8 at the parent `ba40d38`
(`$HOME/.cache/inter16ab-base-r8.log`, run CUT by the cap): 5 already FAILED before the fix
(`refusal_inventory`, `a_frame_edge_straddling_band`, 10-bit `angle_delta`, 10-bit
`split_transform_intra_block`, `class1_chroma_eob`) — they come from r7's three unverified merges.
`a_real_aomenc_10bit_..._filter_intra_on_an_intra_block_...` was **ok at baseline and FAILED after**:
the one unexplained red, fix-now for r9. The two 1:4 gates
(`a_real_aomenc_inter_sequence_with_16x16_level_1to4_partitions_...`,
`real_aomenc_1to4_streams_...`) fail on ARM-COUNT asserts (0 attempts carried the arm, 0 mismatched)
= class `parallel-flake-is-attempt-selection`; their baseline was not reached before the cap.
The 16-level AB gate (`a_real_aomenc_inter_sequence_with_16x16_level_ab_partitions_decodes_pixel_exact`)
and the 32x8 gate PASSED.

## 4. Refusals
Nothing lifted, nothing added. `"a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular
residual coding"` (decode.rs:17616) is **still in place** — deliberately: it is already gated by
`rect_inter_residual_supported(write_w, write_h)` so it never fires for the supported shapes, and
deleting the string would be an ungated capability claim over the shapes it still guards.
NOTE for r9: the 1:4 gate's own assert message says its documented blocker (that refusal) is gone for
its VERT_4 arm — reconcile the gate's expectation with the refusal, that is what makes it red.

## 5. Next steps for r9 (in order)
1. Re-run at `ba40d38` vs `ee04373`: `a_real_aomenc_10bit_inter_sequence_with_filter_intra_on_an_intra_block_decodes_pixel_exact` (the only flipped red) and the two 1:4 gates.
2. Own the 5 pre-existing merge reds (they are r7 merge debt, not this fix).
3. Film frontier moved: HG `-ss 900` (`~/.cache/inter16ab-tmp/hg900.obu`, 104 frame headers) now stops at `"a split (nonzero tx_depth) transform on an intra HORZ/VERT strip in an inter frame"` — same family as this round's fix, added by the lane-intrarect merge.
