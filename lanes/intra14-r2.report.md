# lane-intra14 r2 -- merge main, un-ignore the gate, re-measure

Branch `lane-intra14`, base main `18bf7dc`. **RED**: the r1 blocker refusal is
gone and the gate now reaches the pixel compare, but every compared attempt
mismatches on a defect this lane does not own, so the lifted refusal is still
**ungated** and this branch MUST NOT merge as-is.

## 1. Merge (commit 4672212)

`git merge --no-commit main` -- one conflict (stream.rs counter accessors,
resolved keeping BOTH lanes' functions: `intra_rect4_in_inter_counters` and
lane-r14's `rect64_inter_tu_hits`). md5 of the merged files vs main:

| file | main | merged |
|---|---|---|
| cdf.rs (incl. NZ_MAP / FILTER_INTRA regions) | a3fb168e | a3fb168e |
| cdf_state.rs | 6faaa22c | 6faaa22c |
| encode.rs | 6c9f9f66 | 6c9f9f66 |
| mvstack.rs | 3ff196a2 | 3ff196a2 |

i.e. every file this lane does not own came through byte-identical to main;
`decode.rs` / `refusal_inventory.rs` / `decode_probe.rs` carry only this lane's
additions on top of main's text (`git diff main -- <file>` reviewed hunk by hunk).

## 2. The gate, un-ignored and re-run (8-bit, 40 attempts)

`--exact stream::tests::a_real_aomenc_inter_sequence_with_an_intra_1to4_strip_decodes_pixel_exact --nocapture`

Before (r1 tree): 40/40 attempts refused, 0 pixel-compared.
After the merge: **3 attempts decode whole**, one of them firing the feature:

* seed 52 (`--cpu-used=3 --cq-level=45`): `64x16=4 16x64=3 32x8=0 8x32=0`
* seed 46 (`--cpu-used=1 --cq-level=55`): `0 0 0 0`
* seed 54 (`--cpu-used=1 --cq-level=35`): `0 0 0 0`

All three MISMATCH ffmpeg, and **the two zero-hit attempts mismatch exactly like
the firing one**: decode-order frame 3/4 luma ~3.7-4.5k samples at max |d| 6..17
(first diff around x=128..141, y=58..59), then frames 5-7 drift to ~24k samples
at max |d| ~220 as the references carry the error. So the failure is NOT the 1:4
strip path: it is a pre-existing INTER-frame defect of this mandelbrot 192x128
source on this tree -- the same one r1 already recorded as "open, NOT mine"
(3729 samples, max |d| = 6, with rect partitions off). Control: re-encoding the
seed-52 recipe with `--enable-1to4-partitions=0` yields a byte-identical stream
(same hits, same per-frame diff counts), so aomenc ignores that flag here and
the shape is not the discriminator either.

Remaining refusals over the 40 attempts (37 of them, all other lanes' surfaces):
inter SB-level AB (12), inter 16x16-level AB or 1:4 (6), non-skip rect strip
needs rect residual coding (5), split intra strip whose TU is 32x64 / 64x32 (5),
inter partition below 8x8 (4), sub-8 angle delta / other (5).

The two gate arms are therefore **`#[ignore]`d again, with the new blocker
measured and named in the source** (stream.rs, above both arms) instead of the
stale r1 reason. The gate itself is unchanged and unweakened: counters are
per-attempt deltas on decoded+compared attempts only, every decode-order frame
compared, a decode error is never a SKIP.

### Recipe hunt for a clean-baseline source (240-recipe matrix + a 4-source probe)
* matrix 192x128/256x192/128x128 x min-part 16/32 x tx-size-search 0/1 x
  cq 63/55/45/35/25 x cpu 1..4 (98 recipes ran before the 10-min bound): exactly
  ONE fired 1:4 strips (192x128, min=32, txs=1, cq35, cpu2 -> `64x16=2 16x64=4`)
  and it mismatches (118035 samples); 7 more decoded with zero hits.
* `testsrc2=192x128` on the same recipe: 4 of 12 attempts decode **pixel-exact**
  (clean baseline) but fire zero 1:4 strips; the other 8 hit named refusals.
* `--enable-cfl-intra=1` was NOT re-measured -- the run budget went to the
  mismatch triage. deferred.

## 3. Film check (his Hunger Games extract, unchanged by the merge)

`decode_probe scratchpad/census3/kf/seg_4500.obu` (3840x1608 yuv420p10le, 213
frame headers): still stops at "a split intra strip whose transform unit is
64x32 (no luma coefficient tables for that shape here)", still decodes
`intra_rect4_in_inter: 64x16=0 16x64=4 32x8=0 8x32=0`. `EC_AV1_FINAL_DUMP` file
count is 0 (r1 measured 1 on the pre-merge tree) -- the stop is inside
decode-order frame 1 either way.

EVIDENCE: $HOME/.cache/intra14-r2-fi0.log, $HOME/.cache/intra14-r2-sweep.log, $HOME/.cache/intra14-r2-src.log | un-ignored gate + 240-recipe matrix + 4-source probe under systemd-run scopes | 3/40 attempts pixel-compared (0 before the merge), seed 52 fires 64x16=4 16x64=3, and the two zero-hit attempts mismatch identically
EVIDENCE: scratchpad/census3/kf/seg_4500.obu | decode_probe under a 6G scope, EC_AV1_FINAL_DUMP file count | stop string unchanged ("transform unit is 64x32"), 4 intra 16x64 strips decoded, 0 frames completed

## 4. Suite

`cargo test -p ec-av1 --lib` under a user systemd unit (MemoryMax=10G):
**370 passed; 0 failed; 34 ignored** (2 of the ignored are this lane's gate
arms); 889.13s.

EVIDENCE: $HOME/.cache/intra14-suite-r2.log | systemd-run --user --unit ... MemoryMax=10G, EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1, nice -n 10 -j3 | test result: ok. 370 passed; 0 failed; 34 ignored; 889.13s

## Residue

* fix-now (successor / another lane): the pre-existing inter-frame pixel defect
  on 192x128 mandelbrot at cq 35..55, cpu 1..3 -- reproducible with ZERO 1:4
  strips and zero rect partitions, first divergence decode-order frame 3/4 luma
  around (130,58) with max |d| < 20. It is unrefused (silent), so every gate on
  mandelbrot inter content is measuring it. deferred -- what unblocks this lane:
  either that fix, or a source whose inter baseline is exact AND that fires an
  intra 1:4 strip.
* deferred: `--enable-cfl-intra=1` arm -- what unblocks it: a green gate to add
  it to.
* accepted: `--enable-filter-intra=1` stays in the recipe. An earlier r2 run with
  it at 0 showed zero mismatches, but only ONE stream was compared in that run,
  so it proves nothing; the claim is withdrawn.
