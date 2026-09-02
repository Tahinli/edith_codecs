# lane-modectx r1 — the luma mode-context readers, and the charter's premise

Base: `lane-rectsplitx` cbf8ffd (NOT rebased onto main — `git rebase main` conflicts in
`crates/ec-av1/src/decode.rs` + `refusal_inventory.rs` because lane-rectsplitx is not merged
yet; aborted, work stays on cbf8ffd as chartered. Deviation from COMMON's "rebase first").

## 1. The pinned defect does NOT reproduce on this base (charter premise stale)

Stream pinned by the charter (192x128, 8-bit, seed 48, axis Y, step 103, noise 12, cq 32,
32x32_level_1to4 recipe), regenerated twice:

```
36cd6fae68d91d4b1fdbb77d7a7854b46a535f727a8b12094411ab0185d07673  a.obu
36cd6fae68d91d4b1fdbb77d7a7854b46a535f727a8b12094411ab0185d07673  b.obu
```

There is no `mode` divergence at mi(24,8). Our `EC_TRACE_MODE_STEP` ladder is IDENTICAL to the
instrumented aomdec's (`name=dq` excluded: our `val` field carries a different quantity there,
range identical) up to the last element we read, and we then stop on a REFUSAL, not a desync:

* ours: 48 non-dq ladder lines, last `mi_row=6 mi_col=8 name=angle_uv val=0 rng=37000`
  == aomdec line 48 exactly; then
  `REFUSED: unsupported: AV1 tile (a HORZ_A/HORZ_B/VERT_A partition below 16x16 ...)`.
* aomdec `EC_TRACE=1` confirms the stream really does code that partition:
  `EC_PART mi_row=8 mi_col=0 bsize=6 ctx=4 ... EC_PART_VAL value=4` (= `PARTITION_HORZ_A` at
  16x16) — aomenc emits an AB partition at 16x16 despite `--enable-ab-partitions=0`.
  So the cq-32 arm of this fixture is blocked by a real, named capability gap (16x16 AB
  partitions), not by a mode-context defect.

At the gate's own cq for this attempt (attempt 6 uses `--cq-level=45`, seed 48 axis Y step 103
noise 12 — i.e. the charter's stream IS already in the gate window) the frame decodes and the
whole ladder matches, before and after this lane's change.

Conclusion: the mi(24,8) `kf_y_mode` divergence lane-band63 r1 pinned was fixed by
lane-rectsplitx ec0b3b5 ("a 1:4 strip read its neighbour MODE from a 16px-coarse band"), which
is in this lane's base. Item (4) of the charter (add seed 48 to the gate window) needs no new
pin: it is attempt 6 of the existing window and passes.

EVIDENCE: /tmp/.../scratchpad/{a.obu,ours.log,ref.log,ours45.log,ref45.log} | regenerate the
pinned stream twice (identical sha256), decode with EC_TRACE_MODE_STEP under ours + instrumented
aomdec, diff ladders | cq32: 48/48 ladder lines identical then a named refusal (aomdec EC_PART
value=4 = HORZ_A at 16x16); cq45: 0 ladder differences, `OK: 1 frames decoded`.

## 2. What did change: availability-correct coarse fallback (class new-map-ignores-tile-edge)

`Neighbours::modes_above_left` (decode.rs:4005) — the entry point of every remaining coarse
reader (`decode_block_rect` :5182, `decode_block_rect64` :6169, `decode_block` :7442) — took the
mi-exact override first but then fell back to the raw `above_mode`/`left_mode` band with NO
tile-relative availability. It now routes through `modes_above_left_mi`, which returns `DC_PRED`
where libaom's `up_available`/`left_available` are false. Same for the two raw-band leaf sites,
`decode_leaf_split4` (:8348) and `decode_leaf_rect8` (:8547), which read
`neighbours.above_mode[c]`/`left_mode[r]` directly as their per-leaf fallback.

Measured result of the fix: **zero behavioural change**, and that is now asserted. New counter
`MODE_TILE_EDGE_COARSE_LEAKS` counts every coarse fallback at a tile edge that held a non-DC
mode; over the 20 attempts of
`a_real_aomenc_stream_with_two_tile_columns_decodes_pixel_exact` it is 0, because `above_mode`
is cleared per tile in `start_tile` and `left_mode` per superblock row in `start_row`. The gate
now asserts `== 0` — the invariant that makes the coarse fallback safe, and the thing that
would regress if either reset were dropped (that is exactly how lane-mtfix's chroma
`above_uv_mode` twin broke).

Honest statement: this is hardening plus a regression guard, not a defect fix. No stream in the
suite changes.

## 3. Band granularity sweep (charter item 3)

| band | granularity | availability guard | can an 8/4-px strip alias it? |
|---|---|---|---|
| `above_mode`/`left_mode` | SUB (16px) | now tile-relative via `modes_above_left_mi` | yes, but only as a fallback — `sub8_mode_col/row` (mi) is primary and always written by `record_mode_mi` |
| `above_uv_mode`/`left_uv_mode` | SUB | tile-relative in `smooth_uv_neighbour` (mi primary, `uv_mode_col/row`) | same shape as luma; primary is mi-exact |
| `above_side`/`left_side` | SUB | — | WRITE-ONLY: no reader left (partition ctx uses `above_side_mi`/`left_side_mi`, mi-granular). Dead state, left alone this round |
| `above_side_mi`/`left_side_mi` | mi | reset in `start_tile`/`start_row` | no |
| `above_skip`/`skip_mode`/`inter`/`ref`/`ref1`/`comp_group_idx`/`compound_idx`/`filter`/`txfm` | mi | reset per tile/row | no |
| `skip_grid`/`tx_grid`/`tx_h_grid`/`uv_tx_grid`/`ref_grid`/`delta_lf_grid` | mi, 2-D | absolute mi coords | no |
| `above_palette_size/colors`, `left_palette_size/colors` (+ `_uv_`) | **SUB, square-only writer** | above: `r % 4 != 0`; left: tile-relative | **YES** — `record_palette_y` loops `0..side/SUB`, so a 32x8 / 8x32 palette strip writes NOTHING on its short axis and leaves an older block's palette cache in the cell; four 32x8 strips also share one left cell |

The palette row is the one live instance of the charter's "last strip wins" class that remains.
It is NOT fixed here: every 1:4 / rect-strip gate runs `--enable-palette=0`, so there is no
stream in the repo that can prove or disprove it, and a blind fix would be an unverified change
to lane-palette2's territory.
Disposition: deferred(needs a real aomenc stream with `--enable-palette=1` that codes a 32x8 or
8x32 palette strip — same fixture family, palette enabled, screen-content source).

## 4. Suite

`cargo test -p ec-av1 --lib` (systemd unit, EC_AV1_REQUIRE_AOMENC=1): SUITE_RESULT_PLACEHOLDER
