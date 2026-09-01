# lane-part32 r5 — SB-level AB partitions at 64x64, proven arm by arm

Branch `lane-part32`, commit `fd1a2eb` on top of r4's WIP snapshot `25cac7d`
(itself rebased on main `3808cf8`).

## What r4 actually left (verified, not assumed)

r4's last words claimed SB-level HORZ/VERT 64 was green. It was not: with
`EC_AV1_REQUIRE_AOMENC=1` both
`a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact`
and its new `..._ab_partition_...` sibling FAILED on seed 63 (luma mismatch,
`test result: FAILED. 0 passed; 2 failed`). r4's two real fixes —
`EOB_PT_512_CHROMA` q-context split (`cdf.rs:269`, `cdf_state.rs:1013`) and
the four AB arms in `decode.rs` — were correct but incomplete.

## Root cause found and fixed this round

`PARTITION_VERT_A`/`_B` visit their square sub-blocks **TL, BL, TR, BR**, not
in raster order, so the bottom-left square's top-right neighbour is the
partition's own right-hand rectangle — not yet decoded. libaom switches
`has_top_right`/`has_bottom_left` onto `has_tr_vert_*`/`has_bl_vert_*` for
exactly these two partition types (`reconintra.c` `get_has_tr_table`,
`get_has_bl_table`). This crate's `Reach` carried only the raster tables:
for the BL 32x32 (blk_row_in_sb=1, blk_col_in_sb=0, bit index 4)
`has_tr_32x32[0]=95` bit 4 = **1** while `has_tr_vert_32x32[0]=15` bit 4 = **0**.
One availability bit → 4683 wrong luma pixels.

Failure chain on the pinned stream: BL square predicts (V_PRED, angle_delta=2)
off never-written above-right samples → its right columns decay
(150,150,150 → 130,93,39 at x=157..159) → the 32x64 strip to its right
DC-predicts off that corrupted left column and comes out flat **107** instead
of ~149 → the next superblock row's V_PRED copies 107 down the whole frame.

Decisive evidence that this was reconstruction, not entropy: instrumented
`aomdec` (`EC_TRACE=1 EC_TRACE_MODE=1`) agrees with us on every symbol —
`EC_PART_VAL mi_row=0 mi_col=32 value=6` (VERT_A), every `EC_IMODE_VAL`
mode/uv_mode, and the msac **range** at `mi_row=0 mi_col=40` is `53240` in
both decoders (ours: `TRACE_RECT_PREFI ... rng=53240`).

## Files changed

- `crates/ec-av1/src/encode.rs:1049` — `HAS_TOP_RIGHT_VERT` /
  `HAS_BOTTOM_LEFT_VERT` transcribed verbatim from libaom
  (`has_tr_vert_8x8/16x16/32x32`, `has_bl_vert_*`), a `VERT_AB_PARTITION`
  thread-local + `VertAbGuard`, `Reach::vert_ab_partition()`, and the table
  pick inside `top_right`/`bottom_left`. Rect sub-blocks deliberately keep
  the non-vert tables (libaom's own comment) — `Reach::of_rect` untouched.
- `crates/ec-av1/src/decode.rs:7535,7610` — the 32x32-level intra VERT_A/VERT_B
  arms take the guard (same defect one level down, swept in the same round).
- `crates/ec-av1/src/decode.rs:7766` — the SB-level AB arm takes it only for
  the two vertical arms.
- `crates/ec-av1/src/decode.rs:13432,13595` — the two inter AB VERT arms take
  it too (class sweep; inter intra-mode blocks read the same `Reach`).
- `crates/ec-av1/src/decode.rs:767,772` — `SB_AB_HITS` is now `[usize; 4]`,
  incremented by arm, with `sb_ab_hits_by_arm()`.
- `crates/ec-av1/src/stream.rs:9665` — the AB gate hard-asserts **each** of
  HORZ_A/HORZ_B/VERT_A/VERT_B fired, not just the sum.
- `crates/ec-av1/src/stream.rs:9467,9628` — both gates' stale r2 comment
  ("AB-at-64 is a different, unlanded capability") replaced with the r5 truth.
- `crates/ec-av1/src/refusal_inventory.rs:50` — (r4) the refusal is now
  narrowed to the 1:4 pair only. `gate_coverage.rs` needed no edit:
  `--enable-ab-partitions=1` is now positively exercised and the flag was
  never on `NEVER_EXERCISED`; both its tests are green in the run below.

## Gates

```
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-part32 EC_AV1_REQUIRE_AOMENC=1 \
  cargo test -p ec-av1 --lib -- --test-threads=1 --nocapture superblock_level
```
`test result: ok. 2 passed; 0 failed; 1 ignored` (was 0 passed; 2 failed).

EVIDENCE: /tmp/.../scratchpad/r5gate.txt, r5ab.txt | 40 real-aomenc streams per gate, `--sb-size=64 --enable-rect-partitions=1 --enable-ab-partitions=1`, seeds 42..81, decoded and compared to ffmpeg | ab gate 37 pixel-exact / 3 named refusals, `sb_ab_hits=13 arms(HORZ_A,HORZ_B,VERT_A,VERT_B)=[2, 5, 3, 3]`; horz/vert gate 37 pixel-exact, `sb_rect_hits=61`

EVIDENCE: /tmp/.../scratchpad/r5full.txt | `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib` | `test result: ok. 269 passed; 0 failed; 24 ignored; 0 measured` in 1094s

EVIDENCE: /tmp/.../scratchpad/pre63o.f0 vs our63_nf.y | oracle `EC_AV1_PREFILT_DUMP` vs ours with deblock+CDEF disabled, on the pinned seed-63 stream (pre-fix) | 4683 luma samples differ, bbox x=[151,191] y=[0,127]; post-fix the gate is pixel-exact

## Refusal lifted

`"a superblock-level partition type other than NONE or SPLIT (this decoder's
intra tile path codes only those two at 64x64)"` → replaced by
`"a superblock-level 1:4 partition (PARTITION_HORZ_4/VERT_4 at 64x64, four
64x16/16x64 strips)"`. HORZ/VERT/HORZ_A/HORZ_B/VERT_A/VERT_B at 64x64 are all
now real-stream-proven.

## Film check

`ffmpeg -t 0.4 -i Troy...mkv -c:v copy -f obu troy_r5.obu` (418 B), then
`cargo run -p ec-av1 --example decode_probe -- troy_r5.obu`:

```
REFUSED: unsupported: AV1 tile (filter intra on a HORZ/VERT strip (this decoder predicts square-only))
```

Troy reaches an SB-level HORZ/VERT strip and now stops on filter-intra-on-a-strip.
deferred: the matching "before" probe on main `3808cf8` — not re-measured this
round (would need a full rebuild of a second target dir at the turn ceiling);
unblocked by any lane that already has a main-based build of `decode_probe`.

## Coordinator's two facts, answered

1. **rectsplit seed 50 / `decode_block_rect64` split-transform residual** — NOT
   fixed here; my change never runs for a HORZ/VERT strip (the guard is set
   only under VERT_A/_B, and rect blocks go through `Reach::of_rect`, which is
   untouched). But the hypothesis is now concrete and shares this round's
   class: `decode.rs:5099` and `decode.rs:5425` compute per-TU availability as
   `Reach::of(logical_tx, tu_px, tu_py, ...)` — i.e. they treat the transform
   unit as if it were a *block at frame coordinates* and consult the
   superblock-position table. libaom does not: in `has_top_right`, when
   `row_off > 0` it never reaches a table at all, it returns
   `col_off + tx_size_wide_unit < plane_bw_unit` — purely block-relative. For
   a depth-2 TX_16X16 inside a 64x32 strip that is exactly the sub-TU at
   `row_off > 0` case. That is where I would look first for the (171,56)
   147-vs-148 sample. `rectsplit` keeps ownership.
2. **scaledref's masked Golomb defect at seed 67** — appears RESOLVED on this
   branch. Both r5 gate runs cover seeds 42..81; the only refusals were seeds
   45/46/59 (screen-content palette strip), and seed 67 decoded **pixel-exact
   against ffmpeg**, which is stronger than "no Golomb refusal". Most likely
   killed by r4's `EOB_PT_512_CHROMA` q-context fix, with this round's
   availability fix removing the seed-63 sibling. scaledref should re-run with
   the raised cap after merge; if it still reproduces, the cause is elsewhere.

## Residue

- fix-now(next lane): `PARTITION_HORZ_4`/`VERT_4` at 64x64 — still refused by
  name. Not attempted: `decode_block_rect64` is hardwired to 64x32/32x64
  (`TxbSet::Luma64` + `TX32` scan for luma, `ChromaRect32x16` + `SCAN_32X16/
  16X32` for chroma, `decode.rs:3846-3990`); a 64x16 strip needs a real
  TX_64X16 luma transform plus 32x8 chroma scans/EOB tables, which is a
  transform-primitive lane, not a partition-wiring one.
- accepted: `Reach::table(64)` maps to the 16x16 table row (pre-existing);
  harmless because a 64x64 block in a 64 superblock always takes the
  `row == 0` early return before any table is read.
- accepted: the per-TU `Reach::of` shape described above is left to rectsplit
  rather than fixed blind here, since every gate on this branch is green and a
  speculative change to it would risk them.
