# lane-av1-llsub8b: the inter sub8 rect route's 4:4:4 lossless chroma raster

## Ticket

`decode_inter_sub8_rect2` hit `TxParams::run`'s lossless WHT assert
`(8, 4) != (4, 4)` at decode.rs:2566 (pre-existing at the parent, named by
`lane-av1-ll64`'s same-class sweep). Port sibling `84acbd09` if it covers
this route; else fix only the unit size that trips the assert.
`decode_rect4_16_strip` untouched (sibling-owned). The 12-frame stream's
Golomb-tail refusal was expected to disappear once the (8,4) read was
fixed.

## Sibling verdict: not covered — own fix, same shape

`84acbd09` (already ported here as fb4e4b0e) routed `decode_inter_block8`'s
INTER arms' 8x8 chroma through `leaf8_inter_chroma_lossless`. The rect2
route was one arm over: `decode_inter_sub8_rect2`'s `chroma_444` coded arm
read each piece's (8,4)/(4,8) U/V plane block as ONE `TxbSet::ChromaRect8x4`
rect unit — at lossless that unit hits the WHT assert, exactly the ll64
report's prediction.

## Change (decode.rs, one branch)

New arm in `decode_inter_sub8_rect2`'s `chroma_444` coded path:
`lossless(fctx) && !mono(fctx)` routes the piece's chroma through the
sibling's `read_inter_chroma_lossless` walk with region and block
`(bw, bh)` at stride `SIDE` — lossless codes TX_4X4 on EVERY plane
(`av1_get_tx_size`, blockd.h:1383), so the plane block is a 2x1/1x2 raster
of `TxbSet::Chroma4` units. The walk stamps each unit's own coefficient
context, so — unlike the lossy arm — no whole-block `record_mi_chroma`
runs after it (the leaf8 rule). `piece_tx_type` inheritance stays lossy-
only (lossless is DCT_DCT on every plane). 4:2:0: zero change — the branch
lives inside `chroma_444`, and the reserved 4:2:0 group-tail chroma arm was
not touched. Luma was already correct (`lane-lossless2`'s TX_4X4 leaf walk).

## Verification (measured, this tree @ HEAD + this change)

- Fail-before: `testsrc2 128x96 yuv444p`, aomenc `--profile=1 --lossless=1
  --enable-palette=0 --enable-intrabc=0 --max-partition-size=64`, 6 frames
  → panic `(8, 4) != (4, 4)` at decode.rs:2566, backtrace
  `read_inter_plane_rect → push_mc_rect_tx`. Fail-after: gone; the decoder
  reads through the rect2 pieces and arrives coherently at the next
  (pre-existing) defect site — see deferred (1).
- Witness with the route actually firing: mandelbrot 128x96 6 frames, same
  recipe, `--cpu-used` 0 and 3 both decode ALL 6 frames;
  `sub8_inter_rect: horz8x4=22 vert4x8=15` (cpu0) / `10/12` (cpu3) — 37
  and 22 lossless 444 rect2 chroma pieces through the new walk, stream
  completes.
- Sibling non-regression: the ORIGINAL `/tmp/ll444/dodge2.obu` (sibling's
  own witness bytes) decodes byte-exact vs fresh `aomdec --rawvideo`, all 6
  frames, in this tree with this change.
- Entropy alignment on the repro stream: frames 0-2 of the 6-frame
  testsrc2 stream byte-exact vs aomdec; `sub8_inter_rect` counters 0 there
  (its first rect2 piece sits behind deferred (1)'s site), and its frame 3
  reads through the new arm up to the (8,8) site without a desync-form
  failure.
- Gates: `cargo test -p ec-av1 --lib a_lossless` → 6 passed, 0 failed
  (both 4:2:0 lossless libaom gates, the 444 min-partition-64 inter gate,
  the sb128 pair).
- `cargo check -p ec-av1 --all-targets` (target dir
  `$HOME/.cache/cargo-target-av1llsub8b`): 0 warnings, 0 errors.
- Fixtures are scratch in /tmp/llsub8b (regenerable recipes above);
  fixtures/ is gitignored, nothing committed from it.

## Named, not chased (spoken)

1. `decode_inter_block8`'s INTRA-leaf chroma arm (decode.rs:38958/:38985,
   `u_grid`/`v_grid = read_plane(..)` at `chroma_side == 8`) reads 4:4:4
   lossless chroma as ONE TX_8X8 unit → the same WHT assert, `(8, 8) !=
   (4, 4)`. Same class, the one arm the sibling's fix did not cover (it
   routed the INTER arms only). decode_inter_block8 is the llinter lane's
   function — deferred(fix-now by the lane owning block8's intra arm; the
   fix shape is the same per-unit lossless walk with intra-plane read
   semantics, not a port of the inter helper).
2. The 12-frame stream's `REFUSED: AV1 tile (a Golomb tail …)` from the
   ll64 report: GONE after this fix — that stream now stops at deferred
   (1)'s (8,8) site instead, exactly as ll64 predicted.
3. Fresh observation on today's regenerated fixtures: a mandelbrot
   default-partition lossless 444 KEY frame pixel-differs from aomdec
   (key-frame-only stream, zero sub8 counters, no refusal, first diff at
   sample 40). Key frames never enter `decode_inter_sub8_rect2`, so this
   is pre-existing and intra-side — the earlier lanes' key-frame proofs
   ran min-partition-64 keys, which stay exact. deferred(the intra lane).

## Note on the shared oracle

aomenc/aomdec binaries are the shared `$HOME/.cache/aom-oracle/build`.
Today's aomenc produces different RD picks than this morning's (the
regenerated min-partition-64 testsrc2 stream's inter frames differ from
the sibling's original bytes) — streams were therefore compared only
against aomdec output of THE SAME bytes, and the sibling's original
dodge2.obu was used verbatim for the non-regression check.
