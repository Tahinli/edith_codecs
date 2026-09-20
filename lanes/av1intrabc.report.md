# lane-av1-intrabc - rect-strip intrabc reconstruction (HORZ/VERT 2:1 shapes)

Base `main` **3ecd4a10**. Worktree `../edith_codecs-av1ibc`, branch
`lane-av1-intrabc`, `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1ibc`.

## 1. What landed

`decode_intrabc_rect` (`crates/ec-av1/src/decode.rs`, ~10910): the missing
reconstruction arm for a `use_intrabc` block on a **2:1 rect strip**, ported
from libaom's `av1_predict_intra_block`/`predict_inter_block` intrabc path:

- whole-block prediction once (`predict_inter_block`), for luma at the block
  footprint and for both chroma planes at `av1_get_max_uv_txsize(bw x bh)`;
- skip case: prediction IS the block (`push_mc_rect`, zero residual);
- coded case: residual per transform unit -- one whole-block rect TU
  (`read_inter_plane_rect`, `TxbSet::LumaRect32x16`/`ChromaRect16x8` and the
  real rect scan/context tables) or one residual per var-tx leaf;
- bookkeeping identical in shape to the square intrabc path: mode
  `DC_PRED` into the mode maps, coefficient contexts, zeroed palette state,
  skip/LF grids, and the DV into the intrabc mi grid.

Wired at all 2:1 rect intrabc slots: `decode_block_rect` (16x32/32x16),
`decode_block_rect4` (32x8/8x32 + 16x8/8x16), `decode_leaf_rect`; in
`decode_block_rect64`/`decode_block_128rect` the wiring REFUSES the shape (a
named refusal slot, not a reconstruction); engagement counters
`intrabc_rect_hits` / `intrabc_rect_vartx_hits` exposed on `decode_probe`.

## 2. Byte-exactness witnesses

In-gate (flipped pin -> witness): `stream::tests::
a_real_aomenc_screen_key_frame_reads_use_intrabc_on_rect_strips`. Its
`testsrc2-cq50-txs0` arm used to stop at the rect-strip refusal; it now decodes
1 frame, **1 intrabc block on a HORZ/VERT rect strip**, pixel-exact vs ffmpeg.
Gate totals (re-measured, reviewer-corrected): **2** intrabc blocks over **2**
arms -- 4 arms decode ZERO intrabc blocks (out of scope), 187 rect
`use_intrabc` reads, 8 frames compared, 0 mismatches, 0 named refusals.

Standalone recipes (aomenc oracle at `~/.cache/aom-oracle/build/aomenc`, probe
`cargo run --release -p ec-av1 --example decode_probe -- <obu>`):

| recipe (base: `--cpu-used=0 --tune-content=screen --enable-intrabc=1
--min-partition-size=8 --max-partition-size=32 --sb-size=64 --enable-palette=1
--enable-tx-size-search=0 --enable-rect-partitions=1 --enable-1to4-partitions=1`) | intrabc rect blocks | shape (mi) | skip | result |
|---|---|---|---|---|
|`testsrc2=s=256x192:r=25 --cq-level=50 --enable-palette=0`, 1:4 off|0|--|--|REFUSED (16x8 CPR block, `--enable-1to4-partitions=0`)|
|`testsrc2=s=256x192:r=25 --cq-level=50 --enable-palette=0 --min-partition-size=4 --max-partition-size=16`|1|16x8 @(38,48)|0|**EXACT**|
|`testsrc2=s=256x192:r=25 --cq-level=45 --enable-palette=0`|1|16x8 @(38,48)|0|**EXACT**|
|`testsrc2=s=256x192:r=25 --cq-level=45 --enable-palette=1`|1|16x8 @(38,48)|0|**EXACT**|
|`testsrc2=s=256x192:r=25 --cq-level=50`, 1:4 off, `--enable-tx-size-search=1`|0|--|--|REFUSED|

Every `EXACT` row is `cmp` of the probe's raw output against ffmpeg
`-pix_fmt yuv420p` decoded from the same `.obu`, i.e. byte-identical planes.
The 16x8 witnesses exercise the residual reader (`skip=0`); the 8x32
witnesses exercise `decode_block_rect4`'s wiring through the same helper.

## 3. Still refused by name

**1:4 pair strips** (`PARTITION_HORZ_4`/`VERT_4`, 16x4/4x16):
`intra block copy on a HORZ/VERT/1:4 rect intra strip (reconstruction is not
ported at this shape)` at `decode_rect4_16_strip`, `decode_block_rect4`'s 1:4
arm and `decode_block_128rect`. Reason this is NOT this lane's helper: a 1:4
strip is coded as a PAIR (both children share one chroma block). The chroma is
sourced at the pair's **odd-mi member** -- the BOTTOM strip of a `HORZ_4` pair,
the RIGHT strip of a `VERT_4` one (`is_chroma_reference`, `av1_common_int.h`
-- the reviewer corrected an earlier, inverted note in this report) -- and the
pair's chroma transform is read once for the pair, so its neighbour and
coefficient bookkeeping is a different model from the 2:1 helper above. Reachable witnesses for whoever takes it next:
`aomenc --allintra` (already stops there today), `--allintra` cpu-used 0
(lands there after the tx-depth/tx-band work, see lane-av1-txbands), and this
hand-made recipe: `testsrc2=s=640x480:r=25 --cq-level=60 --enable-palette=1
--enable-tx-size-search=0 --enable-rect-partitions=1 --enable-1to4-partitions=1
--sb-size=64` (16x4 @ mi(76,108), skip=0).

**Mixed-leaf var-tx intrabc** (`decode.rs:14816`): unchanged. The census gate
`an_intrabc_vartx_census_measures_the_mixed_leaf_refusal` (green) shows the
four-quantiser census still produces UNIFORM var-tx trees only, so no reachable
stream exercises it -- census-pinned, as the wave plan expected. The 2:1 helper
itself already handles a var-tx tree's leaves (`INTRABC_RECT_VARTX_HITS`), so
the refusal that remains is only the square/leaf path's.

## 4. Gates run

- `cargo check -p ec-av1 --all-targets` -> 0 warnings.
- `cargo test -p ec-av1 --lib -- --exact stream::tests::
  a_real_aomenc_screen_key_frame_reads_use_intrabc_on_rect_strips` -> ok.
- `cargo test -p ec-av1 --lib -- refusal` -> 21 passed.
- Full `--lib` suite: NOT run to completion by this lane (budget); sibling lane
  lane-av1-txbands is editing the same file, so the workspace-wide run belongs
  to the batch-level gate.

## 4b. Review round 2 (2026-09-20) -- what the fixes did and what is STILL open

Fixed on this branch (reviewer findings P1-2 / P2 and part of P1-1):

- **P1-2 (panic)**: every rect residual/prediction in `decode_intrabc_rect` now
  uses the proven INTER path's square stride (`side = bw.max(bh)`,
  `cside = cw.max(ch)`), and the prediction window is re-laid at that stride.
  Before: `TxParams::run` panicked (`range end index 8 out of range`) on every
  coded VERT strip.
- **P2 (LF grid)**: the var-tx arm now applies
  `Neighbours::fill_lf_grid_leaf_luma` per leaf (LUMA only), the step the
  proven inter var-tx path applies.
- **P1-1 (tx-mode)**: the intrabc tx-size read no longer keys on
  `fctx.tx_select_inter` (forced false by the key-frame tile decoder) but on a
  new frame-level `fctx.tx_select_frame`, set for EVERY frame from the frame
  header's own `tx_mode == TxMode::Select`. Measured effect: the var-tx arm now
  fires (`intrabc_rect_vartx_hits == 1`) on the encode below.

**STILL NOT BYTE-EXACT** -- two repros, both with the mismatch confined to the
frame's BOTTOM 64-row band (luma rows h-64..h-1, chroma rows h/2-32..h/2-1):

```
# var-tx leaf arm (intrabc_rect=1, var-tx tree=1): 4520/73728 bytes differ
ffmpeg -v error -f lavfi -i testsrc2=s=256x192:r=25 -t 0.2 -pix_fmt yuv420p   -strict -1 -f yuv4mpegpipe - | ~/.cache/aom-oracle/build/aomenc --codec=av1   --bit-depth=8 --input-bit-depth=8 --passes=1 --end-usage=q --cpu-used=0   --lag-in-frames=0 --kf-max-dist=1 --limit=1 --threads=1 --tile-columns=0   --min-partition-size=8 --max-partition-size=32 --sb-size=64   --tune-content=screen --enable-intrabc=1 --cq-level=45 --enable-palette=0   --enable-tx-size-search=1 --enable-rect-partitions=1   --enable-1to4-partitions=1 --obu -o /tmp/a31.obu -
# coded rect arm (intrabc_rect=2): no panic now, 7900/294912 bytes differ
#   ... same recipe, testsrc2=s=512x384:r=25 --cq-level=45 --enable-palette=1
#   --enable-tx-size-search=0
```

First divergence of the 256x192 arm: luma row 128, column 224 (the bottom
superblock row); of the 512x384 arm: luma row 320, column 424. So a coded rect
intrabc block in the bottom band still reconstructs wrong -- the shapes the
in-gate witness covers (`testsrc2-cq50-txs0`, `--enable-tx-size-search=0`,
skip and coded 16x8) stay pixel-exact, and every `smptebars`/`testsrc2` recipe
above is EXACT, so the defect is specific to these two arms and is the first
thing the next round must localize (per-block DV/prediction trace against
`EC_VARTX`/`EC_ISTEP`).

**Verdict: NOT merge-ready.** The two P1s are addressed structurally (no panic,
correct tx-mode keying) and the P2 step is in, but the reviewer's merge
unblock condition -- a coded rect intrabc witness in BOTH orientations plus a
TX_MODE_SELECT one, all byte-exact -- is NOT met: the TX_MODE_SELECT arm is
exactly the 256x192 repro above and it is still 4520 bytes off.

## 5. Deferred

1. **1:4 pair-strip intrabc** -- named above, with the model difference and the
   witness recipes. `deferred(1:4 pair-chroma model)`
2. **Full-suite gate** -- `deferred(batch-level run after lane-av1-txbands
   merges; same file, mid-flight run would measure a half-built tree)`

## 4c. Round 3 (2026-09-20) -- two frame-level root causes found, one stream now exact

Both repros were localized with a per-symbol range ladder
(`EC_TRACE_MODE_STEP` on our decoder vs the instrumented aomdec), then the
first untraced region with `EC_TRACE_COEFF`/`EC_ISTEP`.

### Root cause 1 (stream A, and latent for every rect strip): the key-frame RECT path never published the `TXFM_CONTEXT` bands

`read_intra_frame_mode_info` (libaom `decodemv.c:909`) returns EARLY for an
intrabc block, so the block's transform size comes from
`parse_decode_block`'s INTER var-tx branch -- `read_tx_size_vartx`, whose
depth-0 context is `txfm_partition_context(above_txfm[mi_col],
left_txfm[mi_row], bsize, tx_size)`. Those bands are written by libaom's
`set_txfm_ctxs` at the end of EVERY `decode_block`/`decode_token_recon_block`,
key frames included.

Only `decode_block` (lane-t900 r32, gated on the frame's `allow_intrabc`) and
`decode_block_rect4`/`decode_block_rect64`/`decode_rect4_16_strip`/
`decode_leaf_rect8` (gated on `INTRA_IN_INTER_MODE`) published. The 2:1 strip
readers reached on a key frame -- `decode_block_rect` (16x32/32x16),
`decode_leaf_rect` (8x16/16x8 leaves of a 16x16 HORZ/VERT split) and
`decode_block_128rect` -- published nothing.

Measured (256x192 testsrc2 cq45 var-tx arm): the coded intrabc 16x8 strip at
mi(38,48) read `txfm_split_rect ctx=14` where aomdec reads `ctx=13`; the left
band held 4 (from the 8x8 at mi(38,42)) where libaom had 8 (published by the
non-intrabc 16x8 intra strip at mi(38,44), `tx_depth=0` -> `TX_16X8`,
`tx_size_high=8`), i.e. `left = 4 < 8` (true) instead of `8 < 8` (false). From
that wrong CDF row the tile desynced at the very next symbol.

Fix: `publish_txfm_bands_if_in_inter`'s guard now also fires when
`allow_intrabc_frame(fctx)`, and the three missing readers above call it with
`depth_to_tx_wh(bw, bh, depth, fctx)` (one call each, placed before their
`depth != 0` split branch so both arms publish once).

### Root cause 2 (stream B): a key frame never published the frame's `reduced_tx_set`

`decode_key_frame_tile_with_cdfs` set `tx_select_inter=false` and the (r2)
`tx_select_frame`, but not `reduced_tx_set_inter`, which therefore kept its
`true` DEFAULT (or an inter frame's stale value). An intrabc block is
`is_inter_block`, so `read_tx_type` resolves its `tx_type` through the INTER
sets -- `av1_get_ext_tx_set_type` off `cm->features.reduced_tx_set_used`.

Measured (512x384 testsrc2 cq45 `--enable-tx-size-search=0`): the first rect
(8x16) intrabc block at mi(80,106) read `tx_type` from a 2-symbol CDF
(`inter_tx_type_8`, `head=[4167,32768,0]`) where aomdec read the 16-symbol
`EXT_TX_SET_ALL16` row (`inter_ext_tx_cdf[1][TX_8X8]`,
`eset=1 set=5 sqr=1 nsym=16`, `head=[31123,30195,27990,27057]`); post-read
range 37912 vs 62176. Every earlier intrabc block in that frame was 16x16
(square), which never consults this cell, so the defect surfaced exactly at the
first rect strip.

Fix: `decode_key_frame_tile_with_cdfs` now publishes the header's
`reduced_tx_set` into `fctx.reduced_tx_set_inter`, mirroring `tx_select_frame`.

### State after the two fixes (no push; committed on the lane)

| stream | bytes differ before | now |
|---|---|---|
| A 256x192 var-tx 16x8 | 4520/73728 | **BYTE-EXACT** (Y/U/V, `cmp` vs ffmpeg) |
| B 512x384 coded 8x16 | 7900/294912 | 528 luma samples |

### What is STILL open on B (precise)

The mode-info ladder is now fully IN SYNC (identical symbol sequence AND
identical post-read ranges through the end of the frame), and the residual
reads agree. The single remaining difference is a **DV VALUE**:

```
O  EC_DV mi_row=84 mi_col=104 dv_col=0 dv_row=-512 rng=33207
U  EC_DV mi_row=84 mi_col=104 dv_col=0 dv_row=-504 rng=33207
```

Same pre-range (33207 after the read on both sides) means the coded
`diff` is identical; the reconstructed DV differs because the **DV predictor**
differs. aomdec's own `EC_IBC` line for that block reads
`bsize=6 skip=1 nstack=2 nearest=(-520,0) near=(-512,0) dv_ref=(-520,0)
dv=(-512,0)`, i.e. `ref = nearest = -520`; our result `-504` is consistent with
`ref = -512` (the `nearmv`), so our intrabc `INTRA_FRAME` MV-stack ORDERING at
that block differs from libaom's (`av1_find_mv_refs` -> `setup_ref_mv_list` ->
`av1_find_best_ref_mvs`, then `dv_ref = nearestmv != 0 ? nearestmv : nearmv`).

The 528 mismatched luma samples (`rows 351..383`, `cols 416..431`) are all
inside this one wrong copy: the block is `skip=1`, so the wrong DV is a wrong
pixel copy, and the region below it inherits the shift.

Next step for whoever takes it: instrument `read_intrabc_dv`'s candidate
gathering (`record_intrabc_mi_rect` / the mv-stack build) with the candidate
list for mi(84,104) and diff it against aomdec's `EC_IBC`
`nstack/nearest/near/dv_ref` (already printed) -- the defect is candidate
ORDER/WEIGHT, not the DV syntax.

### Gates run this round

- `cargo check -p ec-av1 --all-targets` -> 0 warnings.
- Standalone: A byte-exact; B 528 luma samples (from 7900).
- **Not run**: the in-gate `--lib` arms, the refusal suite, and the full suite.
  The lane is NOT merge-ready (B is not byte-exact and the reviewer's
  TX_MODE_SELECT gate is still red); the batch-level suite run stays deferred.

`deferred(DV-predictor candidate ordering at mi(84,104))`
