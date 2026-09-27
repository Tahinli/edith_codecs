# lane-av1-llintra8: the inter frame's 8x8 INTRA leaf lossless 4:4:4 chroma walk

## Ticket

`decode_inter_block8`'s intra leaf read 4:4:4 lossless chroma as ONE TX_8X8
unit (`u_grid`/`v_grid = read_plane(..)` at `chroma_side == 8`,
decode.rs:38958/:38985 pre-change) and hit `TxParams::run`'s lossless WHT
assert `(8, 8) != (4, 4)` at decode.rs:2566 — the one arm sibling
`84acbd09`/fb4e4b0e did not cover (it routed the INTER arms only, named in
llsub8b's report item 1). `decode_inter_sub8_rect2` and
`sub8_leaf_chroma444` untouched (owned elsewhere).

## Change (decode.rs, one branch + one alphabet gate + counter)

1. `chroma_side == 8 && lossless(fctx) && !mono(fctx)` routes the leaf's
   chroma through `read_intra_chroma_lossless` — the intra twin the >=16x16
   intra-in-inter block already runs (plane-major 2x2 raster of TX_4X4
   units per plane, `av1_get_tx_size` blockd.h:1383) — with region and
   block `(8, 8)` at stride 8, the leaf's own `reach`, and the leaf's
   `palette_uv_bufs` windowed per unit by the helper. The walk stamps each
   unit's own coefficient context, so `chroma_stamped_per_unit = true` and
   the tail's whole-block `record_mi` gets its chroma half saved/restored
   around — the same rule the INTER arms' `leaf8_inter_chroma_lossless`
   obeys (the mechanism was already in place). `Grid::Zero(64)` when every
   unit hung off the frame edge, matching the inter helper. 4:2:0: zero
   change — the branch lives inside `chroma_side == 8`, and the reserved
   4:2:0 group-tail chroma arm was not touched.
2. SAME LEAF, one read earlier, second defect of the same class: the leaf
   read `uv_mode` off `uv_mode_cfl[mode]` unconditionally, but libaom picks
   the uv CDF row by `is_cfl_allowed` (decodemv.c:145, cfl.h:23) and at
   LOSSLESS that is "the chroma plane block is BLOCK_4X4" (cfl.h:29) —
   for this BLOCK_8X8 leaf only at 4:2:0. A 4:4:4 lossless leaf must read
   the 13-symbol `uv_mode_no_cfl` alphabet; the tables differ (cdf.rs
   `UV_MODE_CFL` vs `UV_MODE_NO_CFL`, row 0: 10407.. vs 22631..), so the
   unconditional CFL row consumed a different partition at the leaf's own
   uv_mode symbol and desynced every stream that reached the leaf.
   Now gated on `cfl_allowed_px(SIDE, SIDE, fctx)` — the same gate five
   other readers use; 4:2:0 lossless and every non-lossless partition
   still take the CFL row (value-identical reads).
3. Non-vacuity counter `LLINTRA8_CHROMA_WALK_HITS` +
   `llintra8_chroma_walk_hits()` + a `decode_probe` line, per the lane
   convention: CHROMA_SPLIT_TX_HITS cannot attribute its bumps to this
   leaf.

## Verification (measured, this tree @ HEAD + this change)

- Fail-before (assert): `testsrc2 128x96 yuv444p`, aomenc
  `--profile=1 --lossless=1 --enable-palette=0 --enable-intrabc=0
  --max-partition-size=64`, 6 frames (`/tmp/llintra8/t6.obu`) → panic
  `(8, 8) != (4, 4)` at decode.rs:2566, debug backtrace
  `read_plane (decode.rs:38958) → push_intra_tx → TxParams::run`. The
  release build's assert folds into an index panic at `exec_intra`; the
  debug line numbers are the evidence.
- Fail-before (alphabet): with only the uv hunk reverted, the regenerated
  `--min-partition-size=8` stream's EC_COEFF trace diverges from aomdec's
  at all_zero #2400 — `plane=0 mi(4,0)`, the reads right after that leaf's
  uv_mode symbol. With the fix: divergence gone (below).
- Entropy witness: `testsrc2 128x96 yuv444p` with
  `--min-partition-size=8 --max-partition-size=64` (min 8 excludes the
  sub-8 shapes so the leaf actually fires; key frame becomes exact too):
  our EC_COEFF `all_zero` rng sequence matches fresh
  `aomdec --rawvideo` **11292/11292 reads across all 6 frames** with the
  walk firing 34 times; frame 0 is byte-exact and ALL luma planes are
  byte-exact stream-wide. The pre-fix stream panicked at the leaf; this
  stream decodes all 6 frames.
- Non-regression: sibling's own witness bytes
  `/tmp/ll444/dodge2.obu` decode byte-exact vs fresh `aomdec`, 6 frames,
  in this tree with this change.
- Gates: `cargo test -p ec-av1 --lib a_lossless` → 6 passed, 0 failed
  (both 4:2:0 lossless libaom gates, the 444 min-partition-64 inter gate,
  the sb128 pair, the 16x4 pair).
- `cargo check -p ec-av1 --all-targets`: 0 warnings, 0 errors (target dir
  `$HOME/.cache/cargo-target-av1llintra8`). Release decode_probe output
  identical to debug on both streams.
- Fixtures are scratch in /tmp/llintra8 (regenerable recipes above);
  fixtures/ is gitignored, nothing committed from it.

## Named, not chased (spoken)

1. Chroma PREDICTION pixel diffs, entropy-exact: on the min-8 stream,
   2772 bytes differ from aomdec — all chroma, luma exact, and the EC_COEFF
   trace is bit-aligned everywhere (so symbols, uv modes and angle deltas
   all match; only reconstructed pixels differ). Frame 1's diffs cover
   three readers at once with clean upstream edges: the inter 8x16 strip's
   chroma at mi(0,16) (2x4 units, MC-side), an INTRA 8x8 leaf at mi(2,24)
   (uv V_PRED, via this lane's new walk), and the >=16x16 intra block's
   chroma at mi(8,28) (the pre-existing `read_intra_chroma_lossless`
   caller, untouched by this change). One shared per-unit chroma
   prediction-input class across readers, present before this change
   (the >=16 arm predates it) — larger than this arm.
   deferred(the lane owning per-unit 4:4:4 chroma prediction:
   `read_plane`/`PlaneBuf::reconstruct` intra chroma edges + the inter
   strip chroma MC), with the min-8 stream as a ready-made fixture.
2. The default-partition (min 4) testsrc2 key frame still entropy-diverges
   at frame-0 coefficient read #452 (pre-existing, key-frame intra lane;
   llsub8b's item 3). The min-8 key frame is exact, so this lane's witness
   does not depend on it.

## Note on the shared oracle

aomenc/aomdec are the shared `$HOME/.cache/aom-oracle/build`; the
EC_COEFF/EC_COEFF_STEP rungs of that patched tree are what the entropy
pairing above used. Today's aomenc produces different RD picks than this
morning's (llsub8b's note), so every comparison is against fresh aomdec
output of THE SAME bytes.
