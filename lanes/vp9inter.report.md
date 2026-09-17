# lane-vp9inter report — inter-frame SYNTAX, slice 1 (`ec-vp9`)

Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-inter` (detached `43c9c913`).
No commit, no push, no fmt. Reconstruction / motion compensation: **NOT implemented,
`inter_is_named_unsupported` still refuses by name.**

## Status: SYNTAX LAYER BIT-EXACT on two inter frames (2026-09-17, slice 5) —
## frame 1 and frame 2 each match libvpx block-for-block on every comparable
## field (1213/1213 and 1851/1851), which required three fixes in the mode-info
## layer (below) on top of the earlier entropy-layer ones. Pixels are still
## refused: reconstruction/motion compensation remains out of scope.

## History: THREE defects found, fixed and verified: the compressed header
## is bit-identical to libvpx (1200/1200 bools counting the reader marker and the
## tx-mode literals) and the tile walk now matches the oracle for its first 2628
## bools. Block (0,0) of frame 1 is equivalent to the oracle on every comparable
## field (row/col/bsize/tx/mode/skip/seg/ref0/ref1/mv/interp; `uv` is libvpx-stale
## by design). The walk still stops early: 1002 of 1213 blocks, desync after tile
## bool #2628 (cause 4 below, not yet diagnosed).

## What is implemented (all in `crates/ec-vp9`)

- `src/inter.rs` (new): `read_inter_frame_mode_info` — inter segment id (incl.
  temporal-update prediction), skip, `read_is_inter_block`, tx size with the
  `allow_select = !skip || !inter` rule, reference frames (single + compound
  paths, `single_ref_p1/p2`, `comp_ref_p`, `reference_mode_context` contexts
  transcribed from `vp9_pred_common.c`), inter mode, switchable interp filter,
  the sub-8x8 `bmi` path, `read_mv`/`read_mv_component` with the
  `use_hp = allow_hp && use_mv_hp(predictor)` gate, and the MV predictor
  (`dec_find_mv_refs`, `mv_ref_blocks`, `append_sub8x8_mvs_for_idx`,
  `lower_mv_precision`, `clamp_mv_ref`, `mode_2_counter`/`counter_to_context`).
  MVs are recorded both as the refreshed MV and as the raw codeword diff.
- `src/header.rs`: `FrameContext` gained the inter tables (`inter_mode`,
  `switchable_interp`, `intra_inter`, `comp_inter`, `single_ref`, `comp_ref`,
  `y_mode`, `uv_mode`, the MV prob components) with libvpx defaults, and
  `read_compressed_header` gained the inter branch in libvpx order:
  inter-mode probs, switchable-interp probs, `intra_inter`, `read_frame_reference_mode`
  (compound-allowed gate), reference-mode probs, y-mode probs, partition probs,
  `read_mv_probs` (joints → per-component sign/classes/class0/bits → class0_fp/fp
  → hp when `allow_high_precision_mv`). It now returns
  `CompressedHeader { tx_mode, reference_mode }`.
- `src/decode.rs`: `Decoder::decode_syntax` (keyframes decode through to pixels,
  inter frames are parsed with no picture), the inter tile walk (same tile
  geometry/BE size prefixes as the keyframe walk), the frame-context rules
  (`reset_frame_context` 2/3, `refresh_frame_context` save, `use_prev_frame_mvs`
  gate), the per-8x8 `MV_REF` field and segment map for the next frame, and a
  token-only block walk (`decode_inter_block_tokens`) that keeps the entropy
  stream aligned without any prediction or residual add.
- `src/tokens.rs`: `decode_coefs` gained the `is_inter` ref-axis selector
  (`coef_probs[tx][plane][ref]`), keyframe call sites pass `false`.
- `tests/scratch_interdump.rs` (new): parses every frame of `INTER_IVF` and, with
  `EC_VP9_INTERDUMP=1`, prints one oracle-shaped `MODE` line per block.

Gates:
- `cargo test -p ec-vp9 --test keyframe_exact` → **4/4 PASS**
  (`keyframes_match_ffmpeg`, `lossless_64_matches_ffmpeg`,
  `profile1_444_is_named_unsupported`, `inter_is_named_unsupported`).
- `cargo check -p ec-vp9` → **exactly the 5 baseline warnings**
  (`TM_PRED`, `read_partition`, `partition_probs`, `x_mis`, `y_mis`).
- `cargo test -p ec-vp9` → **green end to end** (14 test binaries, 0 failures).
- `cargo test -p ec-vp9 --test scratch_interdump` → **a real gate**: any frame
  error fails it (keyframe errors now propagate) and every inter frame's
  mode-info block count must equal `INTER_EXPECT` (default `1213,1851`, the
  pinned counts of the fixture; `INTER_EXPECT=none` disables).
  `INTER_IVF` defaults to `/tmp/inter1.ivf` (the clean single-tile stream the
  counts refer to); when that file is absent the test **skips loudly**
  (`SKIP: fixture ... not found`, the `scratch_synth` convention) so a machine
  without /tmp fixtures stays green. Current output: `FRAME 1 parsed blocks=1213`,
  `FRAME 2 parsed blocks=1851`, `block-count gate passed for [1213, 1851]`.
  The failing-keyframe path stays reachable by pointing it at the two-tile-column
  stream: `INTER_IVF=/tmp/inter.ivf INTER_EXPECT=none cargo test -p ec-vp9
  --test scratch_interdump` → expected failure at `FRAME 0 failed: Corrupt {
  context: "vp9: tile bool decoder desync" }` (instrumentation for the
  multi-tile keyframe defect).
- Overread gate: the crate already asserts `overreads() == 0` per tile reader at
  the end of each tile (`decode.rs:416` keyframe walk, `decode.rs:809` inter
  walk, message "tile bool decoder desync"), which fires if a walk reads past a
  tile's last bool - that is exactly the error `/tmp/inter.ivf` (2 tile columns)
  now reports for its keyframe. An earlier statement in this report that the
  crate's only overread assertion was the `bool.rs` unit test was wrong.
- Knob-free silence: with no `EC_VP9_*` set, the same harness run emits **0**
  trace lines (grep over the twelve trace prefixes) and still passes the gate.

## Landing fixes (post-verdict)

1. **`clamp_mv_ref` had row and column swapped** (blocking-queue item 1). The
   four edges were computed correctly but applied to the wrong components:
   `mv.0` is the ROW (filled from the vertical joint component in `read_mv`)
   and was clamped with the left/right edges. libvpx's
   `clamp_mv(mv, min_col, max_col, min_row, max_row)` (`vp9_mv.h:47`) takes the
   COLUMN bounds first, and `clamp_mv_ref` passes
   `(mb_to_left_edge - MV_BORDER, mb_to_right_edge + MV_BORDER,
   mb_to_top_edge - MV_BORDER, mb_to_bottom_edge + MV_BORDER)`
   (`vp9_mvref_common.h:215`). Fixed by clamping `mv.1` with left/right and
   `mv.0` with top/bottom. Latent on this fixture (max |mv| 334) - the two
   frames still match 100% afterwards - but on edge-heavy content the wrong
   bound would move a predictor, flip `use_mv_hp` and desync the stream.
2. **`decode_syntax` no longer swallows keyframe failures.** It propagated the
   error only as a `KFAIL` print under `EC_VP9_INTERDUMP`, so a stream whose
   keyframe died mid-walk returned success with a half-updated frame context,
   prev-frame MV field and reference slots - and every later inter-frame dump was
   meaningless evidence. Now the error propagates (`decode_key_frame(..)?`), so
   `/tmp/inter.ivf` fails fast with
   `FRAME 0 failed: Corrupt { context: "vp9: tile bool decoder desync" }`.
3. **The harness is a gate, not a printer** (see the Gates list). Its block
   counts come from `Decoder::last_frame_blocks()`, captured in
   `decode_inter_syntax_frame` right before the walk state is consumed.
4. **All `EC_VP9_*` dumps now go through cached `OnceLock`/atomic gates**
   (`lib.rs`: `cached_gate!` for `EC_VP9_INTERDUMP`, `EC_VP9_MVDGB`,
   `EC_VP9_MVDGB2`, `EC_VP9_MVDUMP`), so the per-block, per-MV-component and
   per-switchable-interp checks are a relaxed atomic load instead of a
   `std::env::var_os` (stdlib ENV lock) on the decode path.

## Streams

- `/tmp/inter.ivf` — a real 1920x1080 clip, `-frames:v 3 -c:v libvpx-vp9 -crf 32`:
  keyframe + 2 inter frames, **2 tile columns**, `allow_hp=1`, `interp=SWITCHABLE`,
  `tx_mode=SELECT`, frame-level `reference_mode=SINGLE`, `reset_frame_context=1`,
  `refresh_frame_context=0` for all three frames (so frames 1/2 start from the
  default frame context — verified against the oracle's `CHSUM`).
- `/tmp/inter1.ivf` — **the stream every match count in this report refers to**
  (the single-tile control, and the one driving the frame-1/2 MODE diffs, the
  bool traces and the candidate probes). Generated with
  `ffmpeg -v error -y -i <a real 1920x1080 clip> -frames:v 3 -c:v libvpx-vp9
  -crf 32 -pix_fmt yuv420p -tile-columns 0 -f ivf /tmp/inter1.ivf`;
  the exact file used here is `sha256
  376532734662ba5faba4c4e97a5aac915ce429c11701bcf66439b61a77388fa8`, 161579 bytes,
  VP9 1920x1080, 3 frames (keyframe + 2 inter), which is what the counts are tied
  to - a different clip or encoder setting gives different block counts, so a
  third party reproducing the numbers should use this file (or regenerate from
  the same source) and check the hash.
  Consumers:
  `BTRACE=1 MODEDUMP=1 MVDUMP=1 /tmp/vp9loss/drv /tmp/inter1.ivf` (oracle) and
  `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-inter INTER_IVF=/tmp/inter1.ivf
  EC_VP9_INTERDUMP=1 EC_VP9_TRACE=1 cargo test -p ec-vp9 --test scratch_interdump
  -- --nocapture` (ours).

## Match counts (oracle `MODE` vs our `MODE`, by index, fields
## row/col/bsize/tx/mode/skip/seg/ref0/ref1/mvrow/mvcol/interp)

| stage | frame 1 ours/oracle | first divergence |
|---|---|---|---|
| before any fix (single tile) | 551 / 1213 (desync) | block #0, `bsize` (ours 10 vs 12) |
| after cause 1 (`update_mv_probs`) | 794 / 1213 (desync) | block #0, `bsize` |
| after cause 2 (kf partition leak) | **917 / 1213 (desync)** | block #0 compares EQUAL on row/col/bsize/tx/mode/skip/seg/ref0/ref1/mv (`uv` is libvpx-stale, `if` is cause 3) |
| frame 2 | never reached (frame 1 aborts) | – |

First divergent decision, single-tile stream, block #0 == (0,0) 64x64:

```
ours   = row 0 col 0 bsize 11 (64x32) tx 3 mode 10 (NEARESTMV) skip 0 ref0 2 mv (0,0)
oracle = row 0 col 0 bsize 12 (64x64) tx 3 mode 13 (NEWMV)     skip 1 ref0 1 mv (3,0)
```

## Bool index of the divergence (the decisive evidence)

Instrumentation used: oracle `MODEDUMP` now also prints `ref0/ref1/mvrow/mvcol/if`,
`MVRAW` prints each `read_mv` diff, and `CHSUM` prints
`frame tx_mode ref_mode allow_hp interp fcx refresh_ctx reset_ctx`; our side prints
`CHSTART frame=N` and `TILE <len> bytes frame=N` markers so `EC_VP9_TRACE` bools can
be attributed to a frame and a section.

- **Compressed header, frame 1 (RESOLVED):** before the fixes our reads diverged at
  bool **#1116**: `ours (prob,bit)=(252,0)` vs `oracle (128,0)`, totals **1184 vs 1200**.
  Section tables (markers `SECT <name>` on ours, `CHSEC <name>` in the oracle,
  bool index counted between markers; ours' window is 4 bools ahead of the oracle's
  because the oracle's starts at its first marker):
  `txmode 0/22 | txprobs 22/856 | coef 878/18 | skip 896/32 | inter_mode 896/32 |
  switchable 928/24 | intra_inter 952/4 | ref_mode_bits 956/0 | ref_mode_probs 956/15 |
  y_mode 971/36 | partition 1007/99 | mv_probs 1106/74(ours) vs 1106/90(oracle)`.
  Every section entry and count matched except the last: the divergence is inside
  `read_mv_probs` (span 74 vs 90). After the two fixes below the CH is
  **bit-identical: 1196 == 1196 bools, no divergence**.
- **First tile symbol, frame 1:** after the tile reader's marker bit (both sides
  `(128,0)`), the partition read differs: ours `(191,1)` → PARTITION_HORZ,
  oracle `(239,0)` → PARTITION_NONE. Different *prob and* different bit ⇒ the
  frame probability state is already wrong at the tile start, which is exactly
  what the CH divergence predicts. Our walk then stays self-consistent for 551
  blocks and finally over-reads the 1717-byte tile ("tile bool decoder desync").

So the first wrong decision is `compressed_header`, bool #1116 — the mode-info,
MV and token layers below it are unaudited until that is fixed.

## Named causes (evidence, not speculation)

1. **FIXED — `update_mv_probs` is NOT `vp9_diff_update_prob`** (`vp9_decodeframe.c:136`):
   `if (vpx_read(r, MV_UPDATE_PROB)) p[i] = (vpx_read_literal(r, 7) << 1) | 1;`
   — a 252-prob flag, then seven literal bits, and the new prob is `(literal << 1) | 1`;
   no `decode_term_subexp`, no `inv_remap_prob`. Our mv probs used `diff_update_prob`
   for all 69 entries, hence the 16-bool deficit and the (252,0)-vs-(128,0) divergence.
   Fix: `fn update_mv_probs(r, &mut [u8])` + all mv loops switched. Evidence of the
   fix: the oracle's mv span 90 == ours 90, CH totals equal, PUPD-equivalent traces equal.

2. **FIXED — keyframe partition probs leaked into the stored frame context.**
   libvpx's keyframe partition table is the CONST `vp9_kf_partition_probs`, selected
   by `get_partition_probs()` for intra-only frames — it is NOT a `FRAME_CONTEXT`
   field. Our `FrameContext::new(true)` baked it into the context and we stored that
   context in `frame_ctxs` at every keyframe, so frame 1 started with the keyframe
   table (`ours ctx0=[158,97,94]` vs `oracle [199,122,141]`; 44/48 entries wrong —
   same bits read, different values, which is why the section tables looked clean).
   Fix: stored contexts are always `FrameContext::new(false)`; the active keyframe
   copy keeps `new(true)`; `FrameContext::reset_inter_partition()` runs before a
   keyframe context save. Evidence: `PPFLAT_PRE` on both sides now equal, and the
   48-entry `PUPD j i before after` traces are IDENTICAL.

3. **FIXED — `SWITCHABLE_INTERP_TREE` was built from the SPEC's naming order, not
   libvpx's.** `vp9/common/vp9_filter.h:23-28` are `#define`s, not an enum:
   `EIGHTTAP 0`, `EIGHTTAP_SMOOTH 1`, `EIGHTTAP_SHARP 2` (the spec's `literal_to_filter`
   names the *same coded values* in a different order). The tree
   `{ -EIGHTTAP, 2, -EIGHTTAP_SMOOTH, -EIGHTTAP_SHARP }` is therefore
   `{0, 2, -1, -2}`, not `{-1, 2, 0, -2}`; with the wrong table our interp leaf came
   out 0 where libvpx gives 1, which poisoned the *next* block's
   `get_pred_context_switchable_interp` (left neighbour's filter is the context).
   Evidence after the fix: both sides print `SWI ctx=3 p0=87 p1=252` / `SWItype 1`
   for block (0,0) and `SWI ctx=1 p0=19 p1=255` / `SWItype 1` for (0,8); block (0,0)
   now matches the oracle on `interp` too.
   **Reader-counter evidence** (oracle `g_bool_count` reset at the compressed header,
   printed as `bools=` in its `SWI`/`SWItype` lines; ours = CH total + tile bool index
   at the same print, both counting the tile reader marker and the tx-mode literals):
   `ours 1208 / 1210 / 1221 / 1223 / 1229 / 1231` vs
   `oracle 1208 / 1210 / 1225 / 1227 / 1233 / 1235` — the first two agree exactly, so
   the two walks are bool-aligned through block (0,0)'s interp read; the later pairs
   differ because the walks diverge at #2628 (below).

4. **FIXED — the tile desync was a transposed `mv_ref_blocks` lookup (POSITION is
   `{ row, col }`, our accessors added `p.0` to the column), plus two smaller
   defects found in its wake.** All three are in `crates/ec-vp9/src/inter.rs`.

   - **The transposition (the root cause).** `vp9_mvref_common.h`'s
     `mv_ref_blocks` is a table of `POSITION struct { int row, col }`, so the
     64x64 row is `{ -1, 3 }, { 3, -1 }, { -1, 4 }, { 4, -1 }, ...` meaning
     `(row - 1, col + 3)` first. Our accessors (`is_inside`, `find_mv_refs`'s
     cell closure, `get_mode_context`, `get_sub_block_mv`'s `search_col`) added
     `p.0` to the **column** and `p.1` to the **row**, so every asymmetric entry
     picked a different neighbour. Evidence: for the block at `r=8 c=32`
     bsize 12 mode 10 the first candidate came from `p=(-1,3)` mapped to
     `(11,31)` (the 64x64 at `(8,24)`, MV `(3,0)`) where libvpx's `(7,35)` (the
     64x64 at `(0,32)`) holds `(-1,0)` - the MV the oracle used. Fixed by
     swapping the two components at every accessor (array text kept verbatim).
   - **`clamp_mv_ref`'s unsigned underflow.** `mi_rows - bh - row` is negative
     for a tall block on the frame's partial last row (1080p: `135 - 8 - 128`),
     and libvpx computes it in `int`; ours subtracted in `usize` and panicked
     ("attempt to subtract with overflow", inter.rs:497) - which is why the walk
     stopped after 1183 of 1213 blocks. Fixed by computing `mb_to_*` in `i32`
     (libvpx's formulas: `-(row*64)`, `(mi_rows - bh - row)*64`).
   - **The candidate count semantics.** First attempt returned the *real* count
     and broke every NEARMV block (frame 1 fell from 100% on the prefix to a
     first divergence at block #15, `mode=11`, ours `(1,2)` vs oracle `(0,0)`).
     `dec_find_mv_refs`' tail (vp9_decodemv.c:626) is subtler: an early exit
     (`goto Done`, i.e. `ADD_MV_REF_LIST_EB` filling the second slot or
     `early_break` stopping at the first) **jumps over** the mode-derived
     override and returns the count as accumulated; falling out of the loops
     applies `count = (mode == NEARMV) ? 2 : 1`, so a NEARMV block whose search
     found only one candidate still reports two and the caller reads the zeroed
     second slot `(0, 0)`. Reproduced exactly (`if done { count } else if mode ==
     NEARMV { 2 } else { 1 }`, clamp that many).

   **Result (frame-1/2 by-index MODE diff, oracle vs ours):**
   - frame 1: ours 1213, oracle 1213, **identity 1213/1213 (100.0%)**
   - frame 2: ours 1851, oracle 1851, **identity 1851/1851 (100.0%)**
   compared fields: row/col/bsize/tx/mode/skip/seg/ref0/ref1/interp and the MV -
   with `uv` dropped for inter blocks and `mvrow/mvcol` dropped for intra blocks
   inside inter frames (both are libvpx-stale, see the non-comparison list below).
   Frame 2 exercises `use_prev_frame_mvs` (the from-previous-frame MV_REF field)
   and also matches byte-for-byte, so the prev-frame MV_REF store is correct.

   **Defect (b) - frame 2's compressed header - is refuted.** Its earlier
   "pristine defaults" `MVPROB` reading was an artifact of the run that aborted
   in frame 1; with the walk fixed, frame 2 reproduces 1851/1851 blocks
   bit-exactly, which requires frame 2's CH MV tables to be correct. (Our
   `MVPROB` dump still prints 4 times per stream where the oracle prints 2, so
   the line-by-line pairing is ambiguous - a harness artifact, not a decode
   difference; frame 1's dump is identical to the oracle's apart from the
   never-updated 11th `classes` slot.)

   **stale-field non-comparisons (extend the list):** `uv_mode` is stale for
   *inter* blocks; `mi->mv[]` is stale for *intra* blocks inside an inter frame
   (libvpx never writes it from `read_intra_frame_mode_info`), so a MODE diff
   must drop `mvrow/mvcol` whenever `ref0 == INTRA_FRAME`. Dropping them is what
   took frame 2 from 97.8% to 100%.

Superseded earlier hypothesis (kept for the record): "a section count/order difference
in the 1116..1200 span" — correct span, wrong mechanism: the counts differed because
the mv-prob payloads are 7-bit literals, not term-subexp payloads.

## Cause-3 lesson worth carrying forward
Every VP9 tree constant must be rebuilt from libvpx's **macro expressions**, not from
the spec's (or one's memory of the) naming order: `vp9_filter.h` uses `#define EIGHTTAP 0`
while the spec's `literal_to_filter` lists EIGHTTAP_SMOOTH first. Trees whose leaves are
plain integers (partition, mv class/fp, segment) were safe; trees whose leaves are *named
constants* (switchable interp, inter mode via `INTER_OFFSET`) must be evaluated in libvpx's
own numbering. Our inter-mode tree was right only because `INTER_OFFSET` is a subtraction.

## Earlier ranked causes (now resolved or refuted)

1. **A compressed-header section count/order difference in the 1116..1200 span.**
   The span is ~200 bools, which matches the *inter* half of the header
   (inter-mode 21 + switchable 8 + intra/inter 4 + ref-mode 1-2 + single_ref 10 +
   y_mode 36 + partition 48 + MV 69 = 197-198 flags before inner literal reads),
   so the prime suspect is one of those sections, not the coef/skip part. The
   missing-16-bools net says at least one section is read with the wrong count.
   Note `CHSUM` (tx_mode=4, ref_mode=0, allow_hp=1, interp=4) already matches the
   oracle exactly, so the header's *front* and the uncompressed header size
   (`uhs`) are right, and the tile offset itself is not suspect.
   Fastest discriminator (next slice, ~20 min): print one marker per CH section
   (`inter_mode`, `switchable`, `intra_inter`, `ref_mode_probs`, `y_mode`,
   `partition`, `mv_probs`) and diff the markers' bool indices against the
   oracle's `BOR` positions; the first section whose start index is off is the bug.
2. **Not the tables.** The default partition probs were diffed entry-by-entry
   against `vp9/common/vp9_entropymode.c::default_partition_probs` (48 values,
   0 mismatches), and the inter defaults were extracted mechanically; so a
   *rejected* cause unless the marker diff says otherwise.
3. **Pre-existing keyframe limitation on multi-tile-column streams (found here).**
   Frame 0 of `/tmp/inter.ivf` (2 tile columns) dies in the *keyframe* path with
   "tile bool decoder desync" while the single-tile control (`/tmp/inter1.ivf`)
   decodes the same keyframe fine: the kf entropy walk is not multi-tile-safe on
   this encode, even though the earlier lane's all-intra sweep was. This is
   independent of the inter work (the inter walk needs no keyframe pixels) but it
   is a real defect to file, and it means frame 0 in `decode_syntax` must be
   tolerated (`KFAIL` line) rather than fatal.
4. Unverified by this slice: the MV predictor's `scale_mv` sign-flip variant and
   the sub-8x8 `bmi` path (never reached, since block #0 already diverges), the
   compound-reference path (this stream is `SINGLE_REFERENCE` throughout), and
   the `prev_frame_mvs` path of frame 2.

## Exact next slice

1. CH section markers → first section with a wrong bool index → fix the count/order
   in `header.rs`; re-run the by-index `MODE` diff until frames 1 and 2 walk to the
   tile's last bool with `overreads() == 0`.
2. Then re-diff `MODE` fields (skip/tx/ref0/ref1/inter_mode/mv) plus the oracle's
   `MVRAW` (raw diffs) to audit the MV reader/predictor — the predictor decides the
   high-precision bit, so it is parse-load-bearing.
3. Then file/fix the keyframe multi-tile-column desync (ranked cause 3) — it is the
   same class as the tile-walk bug fixed in the keyframe lane and is now the only
   thing blocking `/tmp/inter.ivf` frame 0.
4. Only after the syntax layer is exact: reconstruction/motion compensation
   (out of scope today), which is also what makes the pixels — no pixel-level
   inter result is claimed anywhere in this lane.

## Repro commands

```
# oracle dump (ref0/ref1/mv/interp + MVRAW + CHSUM)
MODEDUMP=1 MVDUMP=1 /tmp/vp9loss/drv /tmp/inter1.ivf 2>&1 | grep -E '^MODE|^CHSUM'
# ours
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-inter INTER_IVF=/tmp/inter1.ivf \
  EC_VP9_INTERDUMP=1 cargo test -p ec-vp9 --test scratch_interdump -- --nocapture
# bool-level CH diff: EC_VP9_TRACE=1 on ours vs BTRACE=1 on the oracle,
# split ours on CHSTART/TILE markers and the oracle on the CHSUM/MODE markers
```
