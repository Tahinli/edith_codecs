# lane-av1sub8rect444: the lossless sub-8 rect leaf's 4:4:4 chroma per-unit walk

## Ticket

`lanes/av1llintercdf.report.md` "Named, not chased" item 1: at the shared state
rng=51429 after the 8x4 leaf's luma, the oracle reads the leaf's chroma as
FOUR TX_4X4 units (plane 1 bc=0,1 ctx=11 then plane 2 bc=0,1) while
`decode_intra_sub8_leaf`'s `chroma_444 && bw != bh` arm read the chroma plane
as ONE `TX_8X4`/`TX_4X8` rect unit per plane (`read_coeffs_rect`, skip ctx
fixed at above+left where the oracle derives 11≡4). Wrong unit count AND wrong
skip ctx. Template: b3f815c8's per-unit 8x8-leaf walk.

## Change (decode.rs, one arm + one tail guard + counter)

1. `decode_intra_sub8_leaf`'s rect arm now branches on `lossless(fctx)`: the
   walk reads the leaf's 8x4/4x8 chroma plane block as a 1x2/2x1 raster of
   `TX_4X4` units per plane, PLANE-MAJOR (`av1_get_tx_size`, blockd.h:1383),
   via `read_plane` with `TxbSet::Chroma4`, per-unit `around_mi` (per-unit
   `get_txb_ctx`), the plane-block-bigger-than-transform `+3` (libaom's +10
   rows, so above+left=1 reads row 4 ↔ libaom ctx 11 — the named 11≡4), and
   the lossless tx_type drop (`read_plane`'s own `coding.tx_type = None` for
   lossless + DCT_DCT force). Units anchor at the leaf's own (px, py) at
   4:4:4, clip past the frame edge like `read_intra_chroma_lossless`, stamp
   each unit's own coefficient context with per-unit `record_mi_chroma`
   (`av1_set_contexts` per unit), and assemble the leaf grid at stride bw.
   CFL is impossible here (the uv read upstream already takes the
   13-symbol `uv_mode_no_cfl` row for this shape at lossless), so no alpha
   rides along — the same `None` the template walks pass.
2. Tail guard: the whole-leaf `record_mi_chroma(lmi, bw, bh, ...)` is skipped
   when the walk stamped per unit — NESTED inside the `chroma_444` branch.
   The first cut wrote `if chroma_444 && !stamped { record } else { <4:2:0
   group tail> }`, which let a walked leaf fall into the 4:2:0 group tail:
   it spans the GROUP's two mi rows/cols with one composed state and
   clobbered the walk's own stamps (measured: `above[3][1]` flipped pos→neg
   between the walk's stamp and the next leaf's read — the oracle wants the
   per-unit value). The nested form skips BOTH branches for the walk.
3. Non-vacuity counter `LLSUB8RECT444_CHROMA_WALK_HITS` +
   `llsub8rect444_chroma_walk_hits()` + a `decode_probe` line, per the lane
   convention (`CHROMA_SPLIT_TX_HITS` cannot attribute its bumps to this
   leaf). 4:2:0 is arithmetically identical: the branch lives inside
   `chroma_444`, and the non-lossless rect path and the 4:2:0 group tail are
   byte-preserved.

## Verification (measured, this tree @ HEAD + this change)

- Entropy witness, `/tmp/llkf/t6.obu` vs fresh `aomdec`
  (`$HOME/.cache/aom-oracle/build`), `EC_ECDUMP_IN` (value,rng) pairing:
  pre-change the first mismatching read is #2326 — inside the named leaf's
  chroma at mi(0,2). Post-change #2316–#2488 pair 1:1: the named leaf's FOUR
  chroma units read exactly the oracle's symbols
  ((30281,51429)/(20612,34312)/(33129,58376)/(30377,36104) against oracle
  ctx 11≡4), the sibling INTER leaf mi(1,2) and everything after pairs, and
  the walk counter fires (15 walks stream-wide, `decode_probe`).
  Fail-before: with the rect read in place the leaf desynced at its own
  chroma (t6 frame-1 first differing pixel byte 65, 16032 differing bytes).
- The stream still diverges — later and at a DIFFERENT block: first
  mismatching read #2489, the U unit (2,8) of the 16x4 INTER strip at
  mi(2,8): ours reads `txb_skip` ctx 3 (above[8][1]=0, left[2][1]=0) where
  the oracle reads ctx 11 with raw cells `above=[0,] left=[23,]`
  (`EC_ECDUMP`, the patched oracle's cell dump) — libaom's left[2][1] holds
  level 7 / dc 0 from an earlier row-2 writer, ours holds 0. That cell is
  written by the 16x16 intra block (0,4)'s chroma band stamping and the
  (1,8) 16x4 strip's own tail — `decode_rect4_16_strip` is llcpred's lane.
  With the desync now downstream, our decoder refuses honestly at the strip
  (`a Golomb tail longer than this decoder reads`) instead of decoding
  garbage, so per-frame byte counts are not claimable this round: **t6
  frames 1..5 are NOT exact — no exactness claimed.** Frame 0 is untouched
  by construction (both callers, `decode_inter_sub8_split4` and
  `decode_inter_sub8_rect2`, are inter-frame paths; the key frame never
  reaches this arm) and was byte-exact before.
- Non-regression: `cargo test -p ec-av1 --lib a_lossless` → 7 passed, 0
  failed (both 4:2:0 lossless libaom gates, the 444 min-partition-64 and
  min-partition-8 inter gates, the sb128 pair, the 16x4 pair).
- `cargo check -p ec-av1 --all-targets` (target dir
  `$HOME/.cache/cargo-target-av1sub8rect444`): 0 warnings, 0 errors.
  `rustfmt --check` reports no hunks inside the edited ranges (the crate's
  pre-existing drift is untouched).
- Fixtures are scratch in /tmp/llkf (t6.obu, regenerable per
  av1llkf.report.md's recipe) and /tmp/llsub8rect444; nothing committed
  from them.

## Named, not chased (spoken)

1. The t6 next-site: the 16x4 INTER strip's chroma coefficient-context
   stamping at 4:4:4 lossless. Measured above (ctx 3 vs 11 on U (2,8);
   oracle cells `above=[0] left=[23]`). The row-2 U band state our tree
   drops is written by the 16x16-intra/(0,4) stamping and consumed one
   superblock later; `decode_rect4_16_strip` is llcpred's lane — per the
   ticket this lane stops at the boundary.
   deferred(the 16x4 strip's per-unit chroma context stamping at 4:4:4
   lossless; unblock: llcpred's decode_rect4_16_strip lane, t6.obu as the
   ready-made fixture, first divergent read #2489 = U unit (2,8)).
2. Instrument parity: `read_coeffs_rect` still has no `EC_ECDUMP_IN` rung
   (av1llintercdf item 2) — this lane's walk bypasses the rect reader
   entirely, so the rung was not needed here.
