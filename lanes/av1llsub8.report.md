# lane-av1-llsub8: 4:4:4 lossless sub8 chroma coefficient miss (r1)

## Charter

Localize the 4:4:4 lossless sub8 chroma coefficient miss. Work in
`edith_codecs-av1llsub8` @ 38393554; own `sub8_leaf_chroma444` and the
coefficient read it calls. 4:2:0 lossless stays exact.

## Repro

Fixture regenerated per the llintra report's recipe (raw `--i444` input, same
encoder): `testsrc2 128x96` yuv444p, 6 frames, `aomenc --profile=1
--lossless=1 --enable-palette=0 --enable-intrabc=0` (`~/ll444.obu`, 26511 B;
key-frame cut `~/ll444kf.obu`). Pre-fix key frame: 15001/36864 bytes differ
from `aomdec --rawvideo` (first diff byte 4181). Probe exits 101 at frame 1
(`reconstruct_mc_rect`, decode.rs:2589 path) — the charter's named inter side,
untouched.

## Ladder (EC_TRACE_COEFF, symbol-anchored, key frame only)

12136 aligned `(tag,rng)` steps, then:

1. **uv_mode alphabet (FIXED).** First divergence is *not* a coefficient: at
   the first 8x8 `PARTITION_HORZ` leaf (mi (8,16)) the leaf's own `uv_mode`
   symbol desyncs. `read_intra_mode_sub8` read `cdfs.uv_mode_cfl[mode]`
   (14-symbol) unconditionally; a 4:4:4 lossless `BLOCK_8X4`/`BLOCK_4X8`
   leaf's plane block is the rect itself, so `is_cfl_allowed` is FALSE and the
   stream carries the 13-symbol `uv_mode_no_cfl` row (class
   `wrong-alphabet-same-value`; oracle post-read rng 33082 vs ours 38358 off
   the same entry 56906). Fix: gate on `cfl_allowed_px(seg_w_mi*MI,
   seg_h_mi*MI)` — true for the 4:2:0 group tail (plane block 4x4) and 4:4:4
   split leaves, false for 4:4:4 rect leaves; 4:2:0 unchanged.
2. **Rect-leaf lossless chroma unit structure (FIXED).** Next divergence
   (step 12138): the rect leaf's chroma. `read_tx_size` answers TX_4X4 for
   EVERY plane of a lossless block, so the oracle codes the 8x4 leaf's chroma
   as TWO plane-major TX_4X4 units (U bc 0, U bc 1, then V) with
   `get_txb_ctx`'s **+10** rows (`plane pels 32 > tx pels 16`,
   `txb_skip_cdf[0][10..12]`), while ours read ONE TX_8X4 rect unit off
   `ChromaRect8x4` (rows `[1][0..2]`, only reached correctly non-losslessly).
   Fix: lossless rect arm reads per-4x4 units via `read_plane(TxbSet::Chroma4,
   luma_skip_ctx=Some(3))` (Chroma4's `big` rows = `[0][10..12]`), plane-major
   per `decode_token_recon_block`, per-unit ctx from the unit's own
   `around_mi_rect`, immediate per-unit `av1_set_entropy_contexts` stamps.
   The non-lossless arm (single rect transform, rows `[1][7..9]`) is kept.
   Tail now stamps per-unit (`grid_unit_state`) instead of one whole-leaf
   smear; 4x4 leaves and the single-unit rect arm are bit-identical to before.

After both fixes: divergence moves to step 19228, key frame 178/36864 bytes
off (first diff byte 8320).

## STOP — named, not fixed (budget)

Step 19228: inside a **16x8 leaf** (`PARTITION_HORZ` of a 16x16; U units
bc 0..3, br 0..1) the U unit at **bc=2, br=0** reads `txb_skip` base **1**
(ours, row `[0][11]` — the same `+3` fold, cdf `[8570,...]`) where the oracle
reads base **0** (row `[0][10]`, cdf0=32705): one of ours' above/left entropy
cells for that unit is stamped nonzero where the oracle's is clean. bc=0/1 of
the same leaf aligned (rng 40879/40628), and ours' `dcctx=2` on the diverging
read (vs 0 on bc=1) says the polluted cell carries a DC-sign vote — i.e. an
above-band cell written by the block above with a state the oracle doesn't
hold. Next step: dump the above-band cell contents (both sides,
`EC_DCDUMP`/oracle equivalent) for the units directly above this leaf's
bc 2/3 columns and diff the writer — suspect the 16x8-path's per-unit stamp
map (which unit owns which above/left cell when a leaf spans >1 mi row) in
`decode_leaf_rect`'s chroma loop, NOT `sub8_leaf_chroma444` (16x8 leaves are
read by `decode_leaf_rect`, which already had per-unit +3 reads before this
lane).

## Verification (measured)

- `a_lossless_libaom_key_frame_decodes_sample_exact` + `a_lossless_libaom_
  inter_frame_decodes_sample_exact` (4:2:0): green.
- `cargo check -p ec-av1 --all-targets`: 0 warnings, 0 errors.
- Key frame: 15001 → 178 differing bytes; first diff byte 4181 → 8320.
- Consumed-count ladder: 12136 → 12138 → 19228 aligned steps.

## Deferred (spoken, not hidden)

- 16x8 above-band stamp diff above (fix-now; named cell + writer).
- Intrabc rect leaves at lossless 444 still route the rect arm's
  `read_coeffs_rect` path with the wrong unit structure — unreachable with
  `--enable-intrabc=0` fixtures; same fix shape as the non-intrabc arm
  (deferred(fix-now after the inter side is green, with an intrabc 4:4:4
  fixture), matching the llintra report's deferral).
