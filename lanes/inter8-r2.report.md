# lane-inter8 r2 -- mi-granular inter neighbour bands; the 8-bit 8x8-split gate is GREEN

Branch `lane-inter8`, commit `ae69b25` on top of r1's `b2231a9` (off main `3808cf8`,
not rebased -- the orchestrator merges).

## What changed (`crates/ec-av1/src/decode.rs`, `crates/ec-av1/src/stream.rs`)

1. **`Neighbours`' inter side bands are mi(4px)-granular** (`decode.rs` ~1910 `new`,
   ~1975 `start_tile`, `record_inter_rect` -> new `record_inter_rect_mi` ~2300,
   `record_compound_ctx_rect` -> `record_compound_ctx_rect_mi` ~2389; every reader
   now indexes `[cmi]`/`[rmi]`). Fields: `above/left_skip`, `_skip_mode`, `_inter`,
   `_ref`, `_ref1`, `_comp_group_idx`, `_compound_idx`, `_filter`. The SUB-unit
   wrappers stay, so every 16x16+ caller reads exactly what it read before (a
   SUB-aligned block's mi corner is libaom's `above_mbmi = mi(mi_row-1, mi_col)`).
   r1's `prev_leaves` sibling-override is deleted: each leaf stamps its own 2x2-mi
   span in the caller loop (`decode.rs` ~12664) right after it decodes, so a leaf's
   left neighbour can now be the *previous 16x16 block's* bottom leaf.
2. **The compound arm of `decode_inter_block8` returned before its own
   `record_mi`/`fill_lf_grid`** (`decode.rs` ~11353). The single-ref arm falls
   through to them at the end of the function; the compound arm did not. Every leaf
   of the gate's stream is compound (LAST+ALTREF), so a split 16x16 left
   `above_side_mi` at 16 and the *next* block's coefficient level state and lf grid
   describing whatever block was there before. Consequence: the 16x16 at mi(8,12)
   computed partition ctx **0** where aomdec had **1** and read `PARTITION_NONE`
   where the stream coded `PARTITION_SPLIT`.
3. **`get_comp_group_idx_context` / `get_comp_index_context` took the leaf's
   ENCLOSING 16x16 corner** (`outer_at`). Both now take mi coordinates
   (`decode.rs` ~8558/~8572) and the leaf passes `leaf_mi`; `decode_inter_block`
   passes its own `(rmi, cmi)`. This was the last divergence (block mi(8,14)).
4. `tpl_frame` threaded into the leaf's two mv stacks (`decode_inter_block8` new
   last param + `comp_tpl`/`tpl` builds mirroring `decode_inter_block`). Alignment
   only -- **it did not move this stream's ladder** (no `use_ref_frame_mvs` here);
   kept because passing `None` while the level above passes the field is a real
   parsed-then-discarded gap.

All three of 1-3 are class `context-read-from-one-cell`.

## Gate

    cd <worktree>; export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-inter8 EC_AV1_REQUIRE_AOMENC=1
    cargo test -p ec-av1 --lib -- an_8x8_leaf_split

`a_real_aomenc_inter_sequence_with_an_8x8_leaf_split_decodes_pixel_exact` **ok**
(un-ignored). It hard-asserts `decode::inter_sub16_split_hits` advanced (interior
16x16 `PARTITION_SPLIT` fired) and every one of the 4 frames pixel-exact vs ffmpeg;
a refusal or a mismatch panics, it never SKIPs.

EVIDENCE: /tmp/r2-fix.obu (aomenc mandelbrot 64x64 4 frames cq40 cpu0 --min-partition-size=8 --max-partition-size=16, rect/AB/1to4 off) | ours EC_TRACE_MODE vs instrumented aomdec EC_TRACE_MODE, `EC_MODE` ladders diffed line for line | inter mode-info range ladder 42/57 (r1) -> 48/57 (bands mi-granular) -> 50/57 (compound-arm record) -> **57/57**, and the gate decodes 4/4 frames pixel-exact
EVIDENCE: /tmp/orc_r2.txt (aomdec EC_TRACE=1 EC_TRACE_MODE=1 on the same stream) | `EC_PART mi_row=8 mi_col=12 bsize=6 ctx=5 rng=38329` / `EC_PART_VAL ... value=3` vs our `TRACE partition_w16 mi=(8,12) ctx=0 value=0` before fix 2 | partition context 0 vs 1 at the block below a split 16x16 -- the stale `above_side_mi`
EVIDENCE: /tmp/orc10.txt (aomdec EC_TRACE=1 on the 10-bit pinned stream) | `grep EC_PART_VAL | uniq -c` | 16 `bsize=9 value=3`, **zero** `value=9` anywhere in the stream

## Test totals

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib` -> **270 passed, 0 failed,
24 ignored** (1100 s). `refusal_inventory` and `gate_coverage` green; neither
needed an edit -- r1 already listed the narrowed string, and no gate recipe flag
changed.

## Refusals

- No new narrowing this round. r1's narrowing
  `"an inter partition below 16x16 other than SPLIT (16x8/8x16 rect inter leaves are not coded yet)"`
  is now **backed by a green gate** (it was not at r1), which is what this round owed.
- Disproved, not a refusal change: the r1 ledger dead-end
  `"aomenc emits PARTITION_VERT_4 (value=9) at 32x32 even with --enable-1to4-partitions=0"`
  is wrong. `EC_TRACE=1 aomdec` on the very stream that produced it reads no
  `value=9` at any bsize. Class `refusal-from-own-desync`.

## Open residue

- fix-now(next round): the **10-bit twin** `a_real_aomenc_10bit_inter_sequence_with_an_8x8_leaf_split_decodes_pixel_exact`
  stays `#[ignore]`d with the corrected reason. It refuses with "an INTER 32x32
  partition type this decoder does not code (value=9)", which the oracle trace above
  proves is our own desync, not the encoder. Not bisected: start by diffing the
  `EC_MODE` ladder of `/tmp/a_real_aomenc_10bit_..._-refused.obu` against
  `EC_TRACE_MODE=1 aomdec`, exactly as this round did for 8-bit.
- deferred(lane-gmaffine merge): the per-leaf stamp records `[3, 3]` ("no info") for
  the leaf's interp filter, as r1's whole-16x16 stamp did. lane-gmaffine `65ef3f5`
  makes `decode_inter_block8` read and hand back its real filter symbols; at merge,
  that value goes into the `record_inter_rect_mi(leaf_mi, 2, 2, ..., [3, 3], ...)`
  call at `decode.rs` ~12664. **Merge warning**: lane-gmaffine rewrites large parts
  of the same `decode_inter_block8` (GLOBALMV/WARPED_CAUSAL/non-LAST refs/interp
  filter); this branch's leaf edits are the neighbour/context plumbing only, but the
  two WILL conflict textually in that function.
- deferred(unstarted): 8x8 HORZ/VERT -> 8x4/4x8 leaves, rect inter leaves at
  16x8/8x16, AB/1-to-4 at every level, and the per-type-per-level aggregate
  hit-counter gate.
- accepted: `tpl_frame` in the leaf stacks is unmeasured on this stream (item 4).
