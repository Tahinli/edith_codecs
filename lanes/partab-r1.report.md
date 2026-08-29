# lane-partab r1 report (orchestrator-written; flash builder capped before reporting)

VERDICT: PASS pending cross-provider source verification (opus verifier running) — AB inter arms decode and fire live; all gates green.

## What landed
- 46235d1: PARTITION_HORZ_A(4)/VERT_A(6)/VERT_B(7) arms in the INTER 32x32
  dispatch (decode.rs `match part32`), composing the existing 16x16-leaf and
  32x16/16x32-strip decode_inter_block calls; PARTAB_HITS atomic.
- d6453b5: dedicated gate a_real_aomenc_stream_with_ab_partitions_decodes_
  pixel_exact — rect-r2 free recipe + --enable-ab-partitions=1
  --enable-1to4-partitions=0; INTER AB refusals (values 4/6/7) FORBIDDEN,
  warp/obmc/interintra refusals forbidden, other named refusals tolerated
  and printed.

## Evidence (orchestrator re-ran everything)
- 14-pin default list green (3 warp + 8 ii + 3 rect fixtures).
- AB gate 4 runs: matches 5-8/40 per run, partab_hits 1-4 per run (arms FIRE
  live), zero forbidden refusals. Refusal breakdown (40-attempt run):
  14x allow_screen_content_tools (pre-existing, content-triggered),
  17x INTRA-site partition refusals (values 1/2/4/5/6/7 on key frames) —
  intra rect/AB partitions are out of this lane's scope and now the widest
  measured remaining gap in THIS recipe.
- Free-partition gate 6/6 with self-pin armed.
- Full ec-av1 lib: 222 passed / 0 failed.

## Residue
- INTRA-frame rect/AB partition decode (17/40 refusals above) — queue.
- allow_screen_content_tools streams (14/40) — pre-existing named refusal.
- HORZ_4/VERT_4 (needs 32x8/8x32 tx/CDF threading) — queue.
- No AB-specific pinned fixture yet: no AB-gate mismatch has occurred to pin
  (self-pin stays armed in the gate).
