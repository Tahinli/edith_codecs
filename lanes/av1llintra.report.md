# lane-av1-llintra: 4:4:4 lossless intra side (class 444-lossless-intra-side)

## Charter

Fix the 4:4:4 lossless intra-side panic (decode.rs:3766, `index 16 len 16`, via
`decode_leaf8 -> read_plane -> exec_intra`) and the same-class 64x64 lossless
intra luma mis-decode, or stop at the first site that cannot be finished.

## Repro (fail-before)

`aomenc profile 1 --lossless=1`, testsrc2 128x96 yuv444p, 6 frames, palette and
intrabc off, cdef/restoration off (`ll444.obu` = default partitions,
`dodge2.obu` = `--sb-size=64 --max/min-partition-size=64`):

- `ll444.obu`: PANIC decode.rs:3766 (`reconstruct`, index 16 len 16) on the KEY
  frame, `decode_leaf8 -> read_plane -> exec_intra` (measured, RUST_BACKTRACE).
- `dodge2.obu`: key frame decoded but luma pixel-diffed the oracle from byte 65
  (31729 of 36864).

## Root causes and fixes (all in decode.rs)

1. **Leaf8 chroma is one TX_8X8 unit at 4:4:4 lossless** (the panic). Oracle
   `av1_get_tx_size`: `if (xd->lossless[..]) return TX_4X4;` for EVERY plane, so
   the leaf's 8x8 chroma plane block is a plane-major 2x2 raster of TX_4X4
   units. Our `TxbSet::Chroma8` single unit hit `TxParams::run`'s lossless WHT,
   which is 4x4-only (`debug_assert_eq!((w, h), (4, 4))`), silently producing a
   16-coeff residual for a side-8 reconstruct. Fixed: in `decode_leaf8`'s TX4
   arm, `chroma_444 && lossless` routes through `read_intra_chroma_lossless`
   (per-unit reads + per-unit `record_mi_chroma`); the tail's whole-leaf chroma
   band stamp is skipped for that shape (raster-order last writer per band cell
   is exactly libaom's per-entry last update). In the skip arm, the 4:4:4
   lossless leaf predicts four TX_4X4 chroma units (per-unit edges), like the
   luma `SKIP_SPLIT_TX` loop; intrabc/palette keep the whole-plane-block
   prediction.
2. **`read_intra_chroma_lossless` carried 4:2:0 math**: `cu_mi = at_mi + oy/2`,
   `tu_reach(.., ox * 2, oy * 2, 8, ..)`, `record_mi_chroma(.., 8, 8, ..)`.
   Made subsampling-aware (`MI >> ss` divisors, `<< ss` scaling, `4 << ss`
   spans) -- byte-identical at 4:2:0, correct at 4:4:4 (one mi cell per unit).
3. **Whole-block chroma unit clip was a 4:2:0-sized raster** (`plane_px =
   span_mi * 2`, overhang `2 * over_mi`): at 4:4:4 it clipped three quarters of
   each 64x64 block's coded units and desynced the tile -- this was the dodge2
   key frame's luma diff from byte 65 (the desynced chroma garbage-fed the
   following superblocks' luma). Fixed to `span_mi * MI >> ss` and `over_mi *
   (MI >> ss)`, libaom `max_block_wide/high`.
4. **Lossless CFL alphabet gate**: oracle `is_cfl_allowed` at lossless requires
   `plane_bsize == BLOCK_4X4` (TRUE for an 8x8 partition at 4:2:0, FALSE at
   4:4:4). Our `cfl_allowed_px` lossless arm accepted any <=8x8, so a 4:4:4
   lossless leaf read the 14-symbol `uv_mode_cfl` CDF where the stream carries
   the 13-symbol one (class `wrong-alphabet-same-value`). Fixed to
   `(bw >> ss_x).max(4) == 4 && (bh >> ss_y).max(4) == 4`; 4:2:0 identities
   unchanged.

## Verification (measured)

- `ll444.obu` no longer panics on the key frame; the key frame decodes.
- `dodge2.obu` KEY FRAME: byte-exact vs `aomdec --rawvideo` (first diff at byte
  37493, i.e. inside inter frame 1).
- 4:2:0 regression gate `a_lossless_libaom_inter_frame_decodes_sample_exact`:
  green.
- `cargo check -p ec-av1 --all-targets`: 0 warnings, 0 errors.

## STOP -- named, with the measured site

`ll444.obu`'s key frame is NOT yet pixel-exact: 36254 of 36864 bytes differ
from byte 19. Localized with the trace pair (ours `EC_AV1_TRACE`, oracle
`EC_TRACE_MODE_STEP` + `EC_DQCOEFF`): the structural read order matches the
oracle exactly (per-4x4-leaf plane-interleaved luma,U,V; luma unit 0 of the
first sub8 group is byte-exact), but the FIRST chroma unit (`plane 1 @(0,0)`,
`TxbSet::Chroma4`, TX_4X4) decodes near-miss coefficients from scan position 4
(e.g. ours `2:-8` vs oracle `2:-4`, ours `6:4` vs oracle `9:4`) with every
classical context input equal (bands all zero at frame start, `txb_skip_ctx`
base 7 both sides, DCT_DCT forced, same scan). Site: `sub8_leaf_chroma444`'s
square branch (decode.rs read_plane calls around 18935) -- the divergence is a
symbol-level CDF-row input unique to lossless chroma at 4x4 that the current
ctx arguments do not capture. Next step: oracle-paired token trace
(`EC_TRACE_MODE_STEP`'s `EC_COEFF` rng stream vs an rng/tell dump inside
`read_coeffs`) to name the first divergent symbol, then diff its CDF row
selection.

Per charter, stopped there. Frames 1+ of BOTH streams panic in
`reconstruct_mc_rect` (decode.rs:28328 post-insert, was 28233) via the
`decode_inter_block8` inter path -- a different function from anything this
lane changed; that is the charter's named stop for the inter side, and
`read_inter_chroma_lossless` remains untouched (another report owns its /2
design).

## Deferred (spoken, not hidden)

- `decode_leaf8`'s intrabc chroma prediction buffers are hardcoded 4x4
  (18074-18085), so an intrabc 8x8 leaf at 4:4:4 predicts 16 samples for a
  64-sample reconstruct. Unreachable with `--enable-intrabc=0` fixtures;
  deferred(fix-now after the inter side is green, with an intrabc 4:4:4
  fixture).
