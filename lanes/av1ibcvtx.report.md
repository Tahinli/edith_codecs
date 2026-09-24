# lane-av1-ibcvtx — "an intrabc block whose var-tx tree resolved to mixed leaf transform sizes"

Base 73f3ac64. Charter: bounded witness hunt (12 encodes) for the refusal at
`decode.rs::decode_block`'s intrabc luma loop; port libaom's per-leaf intrabc
reconstruct if a stream hits; otherwise pin unreachable.

## Outcome

**Hit, ported.** 5 of 12 hunt recipes refuse on the pre-fix tree. The mixed-leaf
refusal is lifted: the intrabc luma reconstruct now walks the var-tx tree's own
leaf list per leaf, and the block's chroma inherits the co-located luma unit's
coded tx type (a Rule-2 inheritance the uniform multi-TU intrabc path was
missing as well — pre-existing, and it desynced every witness inside the first
mixed tree's first chroma unit until fixed).

Full pixel-exactness of the witness frame is **deferred** to a second,
pre-existing, out-of-charter defect (pinned below). Everything this lane owns is
verified symbol-exact against the instrumented aomdec oracle.

## The 12-recipe hunt (all: cpu-used=0, keyframes, 4:2:0 8-bit, square partitions
only (`--enable-rect-partitions=0 --enable-1to4-partitions=0`),
`--tune-content=screen --enable-intrabc=1 --enable-palette=0
--enable-tx-size-search=1`, `--min-partition-size=8`)

| # | source (lavfi, tile=2x2) | canvas   | sb  | maxp | cq | limit | pre-fix result |
|---|--------------------------|----------|-----|------|----|-------|----------------|
| R1 | testsrc2=128x128 | 256x256 | 64  | 64  | 20 | 1 | **REFUSED (mixed)** — 7 trees read first |
| R2 | testsrc2=128x128 | 256x256 | 64  | 64  | 45 | 1 | OK (uniform) |
| R3 | testsrc2=128x128 | 256x256 | 128 | 128 | 20 | 1 | OK (uniform) |
| R4 | testsrc2=128x128 | 256x256 | 128 | 128 | 45 | 1 | OK (uniform) |
| R5 | testsrc2=320x180 | 640x360 | 64  | 64  | 20 | 1 | **REFUSED (mixed)** — 5 trees |
| R6 | testsrc2=320x180 | 640x360 | 64  | 64  | 45 | 1 | **REFUSED (mixed)** — 84 trees |
| R7 | testsrc2=320x180 | 640x360 | 128 | 128 | 30 | 1 | **REFUSED (mixed)** — 8 trees |
| R8 | smptebars=128x128 | 256x256 | 128 | 128 | 30 | 1 | OK (uniform) |
| R9 | smptebars=320x180 | 640x360 | 64  | 64  | 30 | 1 | OK (uniform, byte-exact vs ffmpeg) |
| R10 | testsrc2=128x128 | 256x256 | 64  | 64  | 30 | 3 | OK (uniform) |
| R11 | testsrc2=320x180 | 640x360 | 128 | 128 | 30 | 2 | **REFUSED (mixed)** — 8 trees |
| R12 | testsrc2=128x128 | 256x256 | 128 | 128 | 10 | 1 | OK (uniform) |

The t900 r33 census (smptebars 256x192, maxp32, 4 cqs) resolved 5/5 trees
uniform; testsrc2's texture is what bends split decisions. smptebars never
produced a mixed tree at any cq/sb probed.

## The port

`decode.rs::decode_block`, intrabc luma loop only:

* The tree's leaf list (`read_var_tx_size`'s `(mi_row, mi_col, w, h)` entries)
  replaces the uniform `logical_tx` grid. Leaves are bucketed per 64x64 mu
  chunk (`chunk_tus`), preserving lane-sb128b r3's chunk-plane-major order; a
  non-intrabc block generates its old uniform grid into the same buckets, so
  that path is byte-identical (verified: R2 pre/post decoder bytes equal).
* Per leaf: prediction windowed from the whole-block DV copy (the palette
  override slot's per-TU window pattern), `inter_txbset_for(tu_tx)` /
  `scan_for` at the unit's own size, per-unit `txb_skip_ctx`,
  `tu_reach`, `record_mi_luma`. All reads stay symbol-identical to libaom's
  `decode_reconstruct_tx` descent (same order: depth-first over the tree in
  raster sub-cell order — confirmed against the oracle source,
  `decodeframe.c:282-337`).
* Chroma of an intrabc block arms `INTRABC_CHROMA_TX` with the chunk's FIRST
  luma leaf's coded tx type and clears it after the chunk's chroma pass —
  `av1_get_tx_type` (blockd.h:1278) reads the tx_type_map at the co-located
  luma position for every non-Y plane of an inter-classified block, and intrabc
  is inter-classified. The uniform multi-TU path never armed it; with testsrc2
  content the first luma leaf codes a non-DCT type (V_PRED et al. from the
  inter set), and the chroma unit's eob class/scan then diverge. Measured: R5's
  first mixed tree, U unit — aom ctx `all_zero ctx=8, txtype=13` vs our
  DctDct-only read; after the arm, every read of the block aligns.
* New counter `INTRABC_VARTX_MIXED_LEAVES_HITS` (+ accessor and
  `decode_probe` print) marks the shape so gates can prove a witness actually
  carries a mixed tree; env-gated `EC_AV1_IBCVTX_DEBUG` prints each mixed
  tree's footprint and DV.

## Verification

* Non-vacuousness: the refusal fired on the PRE-fix tree for R1/R5/R6/R7/R11
  (this worktree, base+counter edits only), with `intrabc_vartx_hits` of
  7/5/84/8/8 respectively — the trees were read before refusing.
* Post-fix, per-symbol trace diff vs the instrumented oracle
  (`EC_TRACE_COEFF` aomdec, `scripts/build-aom-oracle.sh` build): every luma
  leaf read and chroma unit of every mixed tree matches entry-rng, order and
  tx size. The uniform multi-TU walk is code-identical to the old loop and R2's
  whole decoded frame is byte-identical pre/post port.
* Gate:
  `a_real_aomenc_intrabc_mixed_vartx_tree_decodes_without_the_mixed_leaf_refusal`
  (W1 recipe: testsrc2 320x180 tiled, sb64, maxp64, cq30 — 5 mixed trees on the
  pre-fix tree, refusal fires there, decodes clean post-fix).
* Census `an_intrabc_vartx_census_measures_the_mixed_leaf_refusal` survives as
  the premise witness (`reached` is 0 by construction).

## Deferred: the second defect (blocks full pixel-exactness of the witness)

**Class**: silent wrong pixels on a NON-intrabc intra block — outside the
intrabc loop this lane owns.

**Fingerprint** (R5 recipe: testsrc2 640x360, sb64, maxp64, cq20): first
diverging sample px (128,296) vs BOTH ffmpeg and aomdec. Owning block mi
(32,32): 64x64, uniform 4x TX_32X32 var-tx tree, every luma and chroma unit
`all_zero`, every txb read symbol-identical to aomdec (entry rngs
32960/59528/54292/49930/46121/36364 match exactly) — yet one luma sample is
189 where both oracles hold 170, and the wrong samples cascade over ~121k
luma+chroma samples (rows 128+). All rows above 128 are byte-exact. The same
class fires in every testsrc2-tiled stream probed (R1 px (128,40), R7/R11
px (128,193), W1 px (192,32)); smptebars streams are byte-exact throughout.

**Unblock**: a lane owning the intra mode/prediction path reproduces R5
(recipe above; streams are 1-encode deterministic), finds the mi (32,32) block's
mode-read divergence, and fixes it — then the gate above tightens to a full
pixel-exact assert. `decode_probe` with `EC_AV1_IBCVTX_DEBUG=1` and an
`EC_TRACE_COEFF=1` instrumented aomdec give the trace-diff harness.

## Suite

Scoped: the gate, the census, and all 14 `refusal_inventory::` tests pass;
`cargo check -p ec-av1 --all-targets` clean (0 warnings). Full suite: staged to
Main for the VPS run (`--test-threads=1`), result to be appended.
