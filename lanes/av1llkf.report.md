# lane-av1-llkf: the default-partition lossless 4:4:4 key frame's read #452

## Ticket

Default-partition lossless 4:4:4 key frames entropy-diverge at frame-0
coefficient read #452 (mandelbrot/testsrc2, `llsub8b` item 3 / `llintra8`
item 2). Min-partition-64 and min-8 keys stay exact, so the defect lives in
the shapes only a min-4 stream produces: 4x4 leaves and their 4x16/16x4
neighbours.

## Named symbol and reader (measured, t1 = testsrc2 128x96, 1 key frame,
`--profile=1 --lossless=1 --enable-palette=0 --enable-intrabc=0`, default
partition)

- Reads 1..452 of the frame's `all_zero` (TXB_SKIP) rng sequence match the
  oracle exactly; read #452 (0-indexed) diverges:
  - aom: `EC_COEFF plane=2 row=0 col=0 mi=(8,5)` — the V plane's FIRST
    TX_4X4 unit of the 4x16 intra block at mi=(8,5), read off
    `txb_skip_cdf[0][12]` (`ctx=12` = base 2 + offset 10; above=`23`,
    left=`21`, both nonzero).
  - ours: `side=4 ctx=4` — our offset-10 row for base 1 (one band cell
    reads zero), a different CDF row, so the rng diverges on the same
    symbol. Reader: `read_plane`'s `all_zero` symbol read
    (decode.rs:6819, `dec.symbol(&mut coding.txb_skip[skip_ctx])`), fed by
    `decode_rect4_16_strip`'s lossless chroma walk.
- Whose band cell: block (8,4) (the 4x16 strip to the LEFT) recorded each
  of its TX_4X4 units with `record_mi_chroma(cu_mi, 8, 8, plane, ...)`
  anchored at `oy / 2, ox / 2` — the 4:2:0 subsampled form. At 4:4:4 a unit
  is ONE mi cell, so the `(8, 8)` span smeared every unit's state over a
  2x2 mi block: the strip's later V units overwrote `above[5]` — a column
  a 4-px-wide block never touches in aom — with their own (zero) state,
  where aom keeps the 4x4 leaf (7,5)'s nonzero write
  (`av1_set_entropy_contexts`, blockd.c:29, stamps exactly
  `tx_size_wide_unit` x `tx_size_high_unit` cells = one above + one left
  for TX_4X4). The skipped-unit twin arm has the same defect (aom's skip
  path zeroes the plane block's own cells, `av1_reset_entropy_context`,
  blockd.c:58 — the 8x8 smear also stamped past the block).

## Change (decode.rs, two record sites, one branch)

`decode_rect4_16_strip`'s `lossless_pair` chroma walk (skip and non-skip
arms): the per-unit record is now ss-aware —

- anchor `cu_mi = (pair_mi.0 + (oy << ss_y) / MI, pair_mi.1 + (ox << ss_x) / MI)`
- span `record_mi_chroma(cu_mi, 4 << ss_x, 4 << ss_y, plane, ...)`

4:2:0 is arithmetically identical (`(oy << 1) / 4 == oy / 2` for the
multiple-of-4 unit origins, `4 << 1 == 8`); non-lossless arms untouched;
the intrabc arms untouched. `RECT4_16_LOSSLESS_CHROMA_HITS` remains the
route's non-vacuity counter.

## Verification (measured, this tree @ HEAD + this change)

- Entropy: the `all_zero` rng ladders now match the oracle on ALL reads of
  the whole frame — t1 (testsrc2) 2304/2304, m1 (mandelbrot) 2304/2304.
  Fail-before on the same bytes: t1 diverges at read #452, m1 at read #208
  (base-tree control, `$HOME/.cache/cargo-target-av1llkf-base`).
- Pixel witness: t1 decodes byte-exact vs fresh `aomdec --rawvideo`
  (36864 bytes). The 6-frame default-partition testsrc2 stream (`t6.obu`)
  reads frame 0 fully in sync (reads 1..2304 of 12582 consumed before the
  next, DIFFERENT divergence — see deferred (1)) and completes all 6
  frames with no refusal.
- Sibling non-regression: `/tmp/ll444/dodge2.obu` (llsub8b/llintra8's
  witness bytes) decodes byte-exact vs fresh aomdec in this tree.
- Gates: `cargo test -p ec-av1 --lib a_lossless` → 7 passed, 0 failed
  (both 4:2:0 lossless libaom gates, the 444 min-partition-64 inter gate,
  the sb128 pair, the 16x4 pair).
- `cargo check -p ec-av1 --all-targets` (target dir
  `$HOME/.cache/cargo-target-av1llkf`): 0 warnings, 0 errors.
- Fixtures are scratch in /tmp/llkf (regenerable recipes above);
  fixtures/ is gitignored, nothing committed from it.

## Named, not chased (spoken)

1. t6's NEXT divergence is a different site: frame 1 (INTER), luma
   `all_zero` at our read #2317 — same ctx row (1) and same decoded value
   as the oracle but different CDF contents (ours `cdf0=5892` vs aom
   `27244`), i.e. an adaptation-history/row-selection desync on the inter
   frame, not a band smear. deferred(the inter-frame lossless-444 lane),
   with t6.obu's recipe as a ready-made fixture.
2. m1 (mandelbrot) pixels: entropy exact frame-wide, chroma-only pixel
   diffs (U 4436 / V 9070 samples, luma 0) — the pre-existing per-unit
   4:4:4 chroma PREDICTION class `llintra8` named. One concrete suspect
   site of that class observed while here:
   `decode_rect4_16_strip`'s lossless walk calls
   `tu_reach(pw, ph, ox * 2, oy * 2, 8, ...)` — the 4:2:0 luma-space
   offsets — where `read_intra_chroma_lossless` uses `ox << ss_x, oy <<
   ss_y` with unit size `4 << ss_x`; at 4:4:4 the doubled origin moves the
   unit's prediction edge window. Prediction-domain, entropy unaffected
   (proven by the exact coefficient reads above); owned by the same
   chroma-prediction lane. deferred(that lane).

## Note on the shared oracle

aomenc/aomdec are the shared `$HOME/.cache/aom-oracle/build`; the
EC_TRACE_COEFF / EC_ECDUMP rungs of the patched tree carried the pairing.
Every comparison is against fresh aomdec output of THE SAME bytes.
