# lane-av1444chr — 4:4:4 chroma per-quadrant tx_type (H2 fixed) + lossless key-frame chroma localization (H3)

Branch `lane-av1444chr` (worktree `~/.cache/wt/av1444chr`), base `4155c7c7`.
Oracle `~/.cache/aom-oracle/build/{aomdec,aomenc}` (instrumented).
`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1444chr`.

## H2 — FIXED. Per-quadrant `tx_type` inheritance for 4:4:4 inter chroma units

### Witness (pinned)

`crates/ec-av1/fixtures/444_quad_leaf_tx_type.obu`, 27933 bytes,
sha256 `a06c9f7a0862252f6ab7ebdcec340d0602e730e38089365f14747db37f8e78d5`,
fnv1a64 `0x85fa830b880090df`.

Recipe (reproduces the sha bit-for-bit on this box):

```
ffmpeg -f lavfi -i "testsrc2=size=128x128:rate=25" -frames:v 4 -pix_fmt yuv444p -f yuv4mpegpipe - src.y4m
aomenc --codec=av1 --profile=1 --cq-level=20 --cpu-used=2 --sb-size=64 \
  --min-partition-size=64 --max-partition-size=64 --passes=1 --end-usage=q \
  --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 --limit=4 --obu -o w.obu src.y4m
```

### The site identification Main asked for (three block-level-type sites instrumented)

`decode_inter_block` contains TWO structurally duplicated chroma arm chains — the
compound branch and the single-reference branch. The charter's line number
(`~39591` pre-revert) is the COMPOUND copy. Instrumenting all six sites
(env-gated, since removed) with `mi= side= cside= ctx= blk_type= leaves=`:

| site | hits on the witness |
|---|---|
| compound `rect` / `quad4` / `whole` | 0 / **1** / 0 |
| single-ref `srect` / `squad4` / `swhole` | 0 / **12** / 0 |

So `quad4` (the 4:4:4 four-unit arm) IS the live site — but only in its
**single-reference** copy, 12 times (4 blocks x 3 inter frames). The compound
copy fires exactly once, on one block whose four leaves all agree: that is why
the earlier port looked like a no-op. Each dump also showed
`side=64 cside=64 ctx=32`, i.e. the 64x64 `chroma_side > chroma_tx` arm with
`chroma_tx=32` — the four-TX_32X32-per-plane shape.

### Root (measured, not assumed)

1. Every inter block here is a whole 64x64 (min=max partition 64) whose luma
   var-tx tree is four TX_32X32 leaves in raster order. `leaf_tx_types` was
   collected ONLY under `if side > 64`, so at 64 it was empty and the arm
   handed all four chroma units the block-level `luma_tx_type` = the top-left
   leaf's type.
2. Per-unit type census, ours (`EC_TRACE_UNIT`, non-empty units in decode
   order) vs the oracle's `EC_DQCOEFF`, paired 1:1 after skipping frame 0's
   45 intra-frame units (136 inter units on each side, counts equal):
   **pre-fix 4 disagreements, all V plane, all where the covering luma leaf
   coded nothing** — V(0,32) of block mi(0,0) in frame 1, V(64,96) of block
   mi(16,16) in each of the three inter frames. Post-fix **136/136 agree**.
3. Our own `EC_TRACE_TXTYPE` shows the covering leaf of exactly those quadrants
   returning `DctDct` (the `all_zero` stamp, mirroring `decodetxb.c:199-203`),
   while the block-level type is `Idtx`. That is the whole defect: a chroma
   unit over a SKIPPED luma quadrant inherited the block's `IDTX` instead of
   the map cell's `DCT_DCT`.

### The fix

- `covering_leaf_tx_type(leaves, rel_mi)` — one function, extracted from the
  three inline copies that already existed (the 128 mu-chunk tail in both
  copies and the intrabc-128rect path), so there is no second convention.
- `leaf_tx_types.push(...)` is now unconditional in both copies (the `side > 64`
  gate is gone).
- Both four-unit arms resolve per quadrant:
  `covering_leaf_tx_type(&leaf_tx_types, (cr * cu_tx/MI, cc * cu_tx/MI))`
  with `.unwrap_or(luma_tx_type)` as the fallback for a block that coded a
  single luma unit (no vartx tree, so no leaves collected).

### Bar

- **Red-before by mutation**: reverting only the live arm's resolve
  (`let cu_tx_type = luma_tx_type;`) turns the new gate red with
  `decode-order frame 1 of 4 (4 shown, 0 hidden) differs from the oracle at
  byte 36635 (ours 127 vs 126), 777 bytes differ` — the report's 777 wrong V
  samples. Restored: green.
- **Identity green** (each run as a single named test, `--nocapture`, no SKIP
  lines): `a_lossless_444_min_partition64_inter_stream_decodes_pixel_exact`,
  `a_lossless_444_defaultp_inter_strip_stream_decodes_byte_exact`,
  `a_444_sb128_root_rect_stream_with_restoration_decodes_pixel_exact`,
  `a_real_aomenc_stream_with_a_16_level_1to4_partition_decodes_pixel_exact`,
  `a_real_aomenc_rect_strip_palette_decodes_pixel_exact` — 5/5 ok.
  NOTE: the three sweep gate names in the charter
  (`a_real_aomenc_odd_coded_dimension_streams_decode_pixel_exact`,
  `a_lossless_444_10bit_inter_stream_decodes_pixel_exact`,
  `a_444_12bit_inter_sequence_decodes_pixel_exact`) DO NOT EXIST in this tree;
  the closest existing gates from each family were run instead. The 32-attempt
  `a_real_aomenc_inter_sequence_with_a_16_level_rect_leaf_decodes_pixel_exact`
  was NOT run locally (multi-encode sweep — VPS/Main territory).
- **New gate**: `stream::tests::a_pinned_444_inter_stream_chroma_units_inherit_their_own_quadrants_tx_type`
  (fixtures/444_quad_leaf_tx_type.obu, length + fnv pinned, oracle compare via
  the length-asserting `decode_all_frames_vs_oracle`, 4 frames, 0 hidden).
  Measured on the witness: `quad-resolved chroma units 96, of which 8 differed`
  — 96 = 4 blocks x 4 units x 2 planes x 3 inter frames; the 8 = the four
  skipped-leaf quadrants (two of them in frame 1) resolved once per plane.
  The gate asserts `diff >= 2` and, per Main's non-vacuity correction, uses the
  DIFFERS number rather than the route counter: the route counter fires on all
  96 reads and stayed green through the earlier no-op port. A 4:2:0 control
  (`fixtures/av1_192x128_8bit_intra64_in_inter.obu`, sha256
  `e4cff46cb1700527c68efc866db27a2835705e949d646f73f0c9df577d594bd6`) asserts
  both counters stay at exactly 0.

### Class sweep (same rule, other sites)

- The two 128x128 mu-chunk tails and the intrabc-128rect path now call the
  shared helper instead of three inline copies of the same search (pure code
  move, no behaviour change).
- `read_inter_rect_chroma` (the `rect_tu || non-square chroma plane` sibling)
  still passes the block-level type to its units. Measured unreachable on five
  4:4:4 rect-enabled recipes (testsrc2 128x128/192x128, cq 20/24/28/30/40,
  rect + 1to4 + tx-size-search + sb 64/128): the arm fires 13-43 times per
  stream but `multi` (nx*ny > 1, the only case where units could sit over
  different leaves) is **0 on every one**, and all five streams are
  pixel-exact against aomdec. Left unchanged deliberately rather than
  speculatively re-signaturing a shared helper; if a witness ever shows
  `multi` with mixed leaf types, that is where the same fix goes.

## H3 — LOCALIZED, not fixed. Lossless 4:4:4 key-frame chroma

### Witness used

The charter's sha256 (`87b0d1a5…`, `--lossless=1 --tile-columns=1 --cpu-used=2`,
256x128 testsrc2 yuv444p) did not reproduce from the flags as given: the encode
is not bit-reproducible across the flag variants tried (68142 B
`7b8d4d44…`, 67332 B `04de36a6…`, 75378 B `fe9eab1a…` for
`--passes=1`/`--threads=1 --row-mt=0 --lag-in-frames=0`). I used the plain
`--profile=1 --lossless=1 --tile-columns=1 --cpu-used=2 --limit=6` encode
(68142 B, sha256 `7b8d4d442c4a847314b7bc8c26af79bae1b3e32b22b41eebef4c2dcacf895a57`),
which reproduces the SAME SIGNATURE: luma exact in all six frames, 59-73 wrong U
and 103-139 wrong V samples per frame, deltas ±1, confined to two small bands —
chroma (108..123, 24..31) and (236..255, 56..67) in frame 0.

### What is measured

1. **Not coefficients.** Every affected unit's residual is ZERO on our side
   (`OUR_LLCHROMA`, all 16-value grids zero for the units at x=112..124,
   y=24/28) and the oracle agrees: for the block covering the first band
   (mi(6,28), a 16x8 block at (24,112)) `EC_COEFF_STEP tag=all_zero ... =1` on
   all eight chroma units with empty `EC_DQCOEFF`. So the WHT is not involved
   and neither side codes a residual there.
2. **It is the intra chroma PREDICTION.** Scoped to frame 0 only (the stream
   cut to its first shown frame; 98304 bytes both sides, lengths asserted),
   `EC_PRED`/`EC_PREDOUT8` vs our `OUR_PRED` compared per unit:
   - U px(108,24) oracle sum 267 vs ours 245
   - U px(112,24) 243 vs 236, px(112,28) 1731 vs 1707
   - U px(116,24) 240 vs 248, px(120,24) 248 vs 251
   - V px(116,28), px(120,28), px(124,28)
   Every other unit in the window matches by sum, row0 and col0, and the LUMA
   of the same blocks matches exactly.
3. **Shape of the divergence.** In every divergent unit the TOP row agrees
   (`row0=[17,17,16,16]` vs oracle `17,17,16,16`) and the difference is in the
   left column's lower entries — e.g. px(108,24) col0 oracle `17,11,3,32` vs
   ours `17,11,3,29`; px(112,24) oracle `16,14,7,32` vs ours `16,14,9,24`. The
   first three left samples agree, the last one(s) do not.
4. The affected units are TU rows 2 and 3 of 4x16/16x8 chroma blocks in
   `decode_rect_split`'s lossless per-4x4 walk
   (`RECT_SPLIT_LOSSLESS_CHROMA444_HITS`): row_off 0/1 match, row_off 2/3 do
   not, in every block of the band.

### Where that points, and what is still open

The first divergent unit is the third TU row of the leftmost block of the band
(chroma x=108..111, y=24..31), and its only disagreeing input is the BOTTOM
left-edge sample — i.e. a reconstructed pixel from the block to its left at
(x=107, y=27). Everything above and to the left of that agrees, the residual
is zero, and the mode is per-block (one symbol, shared by all four TU rows).
So the open question is narrow and answerable with the next probe: **which
source our lossless 4x4 chroma walk feeds the TU rows below the first as their
`above`/`left` edge** — the reconstructed rows inside the block, or a stale /
wrongly-anchored neighbour column. Two concrete next steps, both cheap:
(a) dump the reconstructed left column and above row the walk hands to
`read_plane` for px(108,24) on both sides (`push_intra`'s edge gather is where
the divergence must show up), and (b) re-check the same comparison with the
walk's `span_x/span_y` reach for rows > 0 — the `lane-av1-llband` note in that
walk already records one span/step bug at 4:4:4 (the 8-px 4:2:0 step), so a
second, row-dependent one in the same walk is the most likely home.

Not fixed, deliberately: the root is not bounded to a named line yet, and
guessing would have burned the lane's remaining budget without a measurement.

### Rung gaps found (worth a follow-up lane, not fixed here)

- Our `OUR_PRED` rung (decode.rs ~19390 / ~19530) carries no PLANE field —
  `PlaneBuf::reconstruct*` does not know it — so a per-unit prediction diff
  has to disambiguate U from V by value matching. The oracle's `EC_PREDOUT8`
  has the same gap the other way: it prints `sum/row0/col0` but NOT the mode for
  non-directional 8-bit units (`EC_PREDND` prints the mode but is HBD-only), so
  a directional/non-directional disagreement is currently only inferable.
- A temporary plane-tagged `OUR_MODE` rung in `read_plane` was used for this
  analysis and removed before the commit; it is a 6-line addition worth landing
  in a follow-up.

## Not run here (Main's validation)

Full suite, the 32-attempt `…16_level_rect_leaf…` sweep, the 4:2:0/10-bit/12-bit
sweep families, and the VPS stages. Local runs were single named tests only, per
the lane's local-scope rule.
