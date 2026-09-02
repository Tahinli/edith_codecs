# lane-gmaffine r3 — lane-inter8 merged, one real 8x8-leaf desync fixed, two gates still RED

Commits on `lane-gmaffine`: `0d56f26` (merge of lane-inter8 `ae69b25`) and `0b8cb54`
(the context-index fix). Not rebased onto main (charter order).

Suite: **271 passed, 2 failed, 24 ignored** (`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1
--release --lib -j3`, 571 s). r2 was 268/2/23; the deltas are lane-inter8's gates arriving.
The two failures are the same two 8x8-leaf motion gates, still red.

## STEP 1 — merge (13 conflicts, all in `decode.rs` + 1 in `refusal_inventory.rs`)
Resolution, per the charter's "keep both, unify duplicates" (full list in `0d56f26`'s message):
- OBMC chroma-above skip: kept **lane-inter8's** explicit `BLOCK_4X4/4X8/8X4` match. r2's
  `write_w >= 16 && write_h >= 16` is wrong for 8x32/32x8, where libaom's chroma plane is
  4x16/16x4 and the above pass DOES blend chroma.
- OBMC neighbour filter: read from lane-inter8's now mi-granular `Neighbours::above_filter`
  /`left_filter`. r2's `MiGrid::filters` + `set_filter`/`filter`/`grid_or_slot` recorded the
  SAME field per mi and are deleted (`mvstack.rs`, `decode.rs`).
- `decode_inter_block8`: one `global_motion` param (r1's), plus lane-inter8's
  `sign_bias_table` and `tpl_frame`; one `gm_table` build at the top; returns
  `(skip, is_inter, skip_mode, compound_ctx8, leaf_filter_syms, leaf_refs)`.
- The leaf's temporal-mv `cur_offset_0` now keys on the leaf's OWN reference — lane-inter8
  hardcoded `LAST_FRAME` there because its leaf refused every other ref, which r1 lifted.
- Per-leaf `record_inter_rect_mi` keeps r2's REAL filter symbols instead of lane-inter8's
  `[3, 3]` intra sentinel (that sentinel is what panicked `neighbour_filter`); the coarse
  whole-16x16 `record_inter` and its `last_filter`/`last_ref` bookkeeping are gone.
- Interior 16x16 SPLIT: lane-inter8's restructure (one `part16` read, one shared leaf loop);
  refusal strings follow it.

## STEP 3 — first divergent element found and fixed (class: unit mismatch after a granularity change)
`crates/ec-av1/src/decode.rs:11504,11700-11716` — `decode_inter_block8` still indexed
`neighbours.above_ref/above_ref1/above_filter` with `c` and the `left_*` twins with `r`,
i.e. `outer_at`, the enclosing 16x16's **SUB(16px)** coordinates, while every other read in
the same function uses `cmi`/`rmi` (**mi**, 4px) since lane-inter8 r2 made those bands
mi-granular. So a leaf's `read_single_ref` p1..p6 contexts and its `switchable_interp`
neighbour rows came from an unrelated 16px column/row. Now `cmi`/`rmi`.

EVIDENCE: `/tmp/.../scratchpad/{ref-mode.txt,o3.txt}` (aomdec `EC_TRACE_MODE` ladder vs ours)
| stream `none-8-32.obu` = the gate recipe with gm and warp both off, 8-bit cq32,
`--min/--max-partition-size=8`, decoded by instrumented aomdec and by
`cargo run --example decode_probe` with `EC_TRACE_MODE=1`
| first inter frame, first 32x32: before the fix leaf mi=(6,6) had identical mode/mv/stack
(NEWMV, mv (0,-7), stack 5) but post-mode-info range **41953 vs aomdec 38345**; after the
fix that leaf and the following 15 agree to the range (38345, 51367, 58585, 58652, 54339,
37583 …). The stream's refusal moved from "an INTER 32x32 partition type … (value=8)" (a
bogus symbol from the desync) to a later block.

Instrument added and kept: `EC_MODE_VAL8` under `EC_TRACE_MODE`
(`crates/ec-av1/src/decode.rs:11731`), one line per 8x8 leaf
(`newmv/globalmv/ref0/mv0/stack/rng`), diffable line-for-line against aomdec's
`EC_MODE_VAL`. Reproduce script: `scratchpad/enc.sh <cq> <depth> <gm> <out.obu>`.

## STEP 2 — gate results
EVIDENCE: `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --release --lib -j3 -- obmc_8x8 globalmv_8x8_leaf warped_causal_8x8_leaf 8x8_leaf_split --nocapture`
| the merged tree, real aomenc streams | `a_real_aomenc_stream_with_obmc_8x8_decodes_pixel_exact` **ok**
(FIRING seed 53, 8x8 obmc hits 4), `a_real_aomenc_inter_sequence_with_an_8x8_leaf_split_decodes_pixel_exact`
**ok**, both motion gates **FAILED** with named refusals (never a SKIP).

## Still RED — the next divergent element (for r4)
Same instrument, same stream: first inter frame, leaf **mi=(0,14)** — aomdec reads
`mode=16` (NEWMV) `mv0=(0,-12)` from a 3-entry stack; we read not-new, `mv0=(0,-11)`, also
3 entries. Stack SIZE agrees, so the suspect is the mode context itself
(`stack.new_mv_ctx`, i.e. `find_mv_stack_with_sign_bias`'s weight/context computation for a
2x2-mi block at the right frame edge with no above row), not the stack contents. Every leaf
before it in that frame matches to the range.
Note the gate recipe is `--max-partition-size=8`, i.e. EVERY block is an 8x8 leaf; the
lane-inter8 gate that is green uses `--max-partition-size=16`, so this is the harder
all-leaf case, and it fails identically with gm off, gm on and warp on (three encodes) —
**the remaining desync is not global-motion or warp specific**.

## Refusals / inventory
No refusal string changed this round; `refusal_inventory` and `gate_coverage` are green
(they are part of the 271). r1's GLOBALMV/WARPED_CAUSAL 8x8 lifts stay lifted, restating
r2's named DEVIATION from the charter fallback: restoring them would put a refusal BEFORE
the code it guards (class `refusal-short-circuits-its-own-code`), the two gates never reach
the motion code (they die on partition refusals from the desync above), and a GLOBALMV-at-8x8
refusal would turn the GREEN obmc_8x8 gate red. The lift is not mergeable to main until a
gate proves it; the merge + context fix in this round are.

## Residue
- fix-now (r4), deferred(nothing — it is the next step): leaf mi=(0,14) `new_mv_ctx` above.
- deferred(lane-hbdinter): `warp::warp_affine` hardcodes `const BD: i32 = 8;`.
- deferred(own lane): compound `GLOBAL_GLOBALMV` never warps, and no refusal names it.
- accepted: `EC_MODE_VAL8` prints only on the single-ref leaf path (the compound arm returns
  earlier); enough for these gates, whose leaves are single-ref.
