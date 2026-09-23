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
Gate totals (re-measured at r4, on the committed tree): **5** intrabc blocks
over **5** arms -- 3 arms decode ZERO intrabc blocks (out of scope), 187 rect
`use_intrabc` reads, 8 frames compared, 0 mismatches, 0 named refusals, 1
intrabc block on a HORZ/VERT rect strip. The r4 gate
`a_coded_rect_intrabc_block_reconstructs_in_both_orientations` adds 3 more
byte-exact frames (HORZ coded, VERT coded, TX_MODE_SELECT).

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

## 4d. Round 4 (2026-09-20) -- the DV predictor's root cause is BLOCK COVERAGE, not candidate weighting; stream B is byte-exact

The remaining defect was localized to a single wrong DV at `mi(84,104)` on the
512x384 stream, with identical coded diff and identical post-read range. The
task named candidate ORDER/WEIGHT; the order was indeed wrong, but the cause is
one level down: **our intrabc MV grid only contained the intrabc blocks.**

### Mechanism (measured, not inferred)

libaom's `setup_ref_mv_list` row/col scans walk `xd->mi[]`, which holds an
`MB_MODE_INFO` for EVERY coded block, and step by
`mi_size_wide/height[candidate->bsize]`. Our `MiGrid` was filled only by
`record_intrabc_mi_rect`, so every intra non-intrabc neighbour read as
`None` and the scan advanced that cell by ONE instead of by the neighbour's
real width. At `mi(84,104)` (16x16, `bsize=6`) the row scan's extended offset
`-3` therefore over-reached from libaom's last probe `(81,106)` (the 8x16
intrabc voting `-520`) to `(81,108)` -- a 16x16 intrabc voting `-512` with a
16-wide (`len 4 * weight 4`) contribution libaom never makes.

Instrumented oracle (`~/.cache/aom-oracle`, new `EC_TRACE_STACK` /
`EC_TRACE_STACK2` prints in `setup_ref_mv_list` + `scan_row_mbmi` /
`scan_col_mbmi` / `scan_blk_mbmi`):

```
O  EC_STACK mi_row=84 mi_col=104 ref=0 w=4 h=4 cnt=2 near_match=1 ref_match=1 \
     [0 w=656 mv=(-520,0)] [1 w=648 mv=(-512,0)]
U  EC_STACK_OURS mi_row=84 mi_col=104 ... n=2 near_match=1 ref_match=1 prows=6 pcols=0
     [0 w=648 mv=(-520,0)] [1 w=664 mv=(-512,0)]
```

libaom's `-520` votes: `4` (row `-1`) + `4` (row `-3`) + `8` (row `-5`) = `16`
(`640 + 16 = 656`); `-512`'s: `4` (top-right `(83,108)`) + `4` (corner
`(83,103)`) = `8` (`648`). Ours lost `-520`'s `8` (the row `-5` contribution)
and gained `-512` `+16` from the over-reach, so the weight sort flipped:
`nearestmv` became `-512` where libaom's is `-520`, and
`dv = ref + diff = -512 + 8 = -504` instead of `-520 + 8 = -512`. One whole-pel
wrong `skip == 1` copy; the 528 wrong luma samples are that copy plus the
region that inherits its 1-px shift.

### Fix

`MiGrid` gained a block-COVERAGE map (`cover: Vec<[u8; 2]>`, the covering
coded block's own `(width, height)` in mi units) with `MiGrid::cover_rect`;
`MiGrid::get` returns the stored `MiInfo` when there is one and otherwise
synthesizes a non-inter entry (`is_inter == false`, zero MV) from the cover, so
a candidate that casts no vote still advances the scan by its real width --
exactly libaom's `xd->mi[]` semantics. `single_ref_match` rejects a non-inter
unit, so a synthetic entry can never vote.

The map is published for EVERY coded block from `Neighbours::fill_lf_grid_rect`
(the one place every block's mi span already passes through, paired 1:1 with
`fill_skip_grid_rect`), and only when the frame has an MV grid at all
(`intrabc_mi_grid` is `Some` exactly when `allow_intrabc`), so an inter frame
and the tile writer's own grid are untouched: an unfilled cover reads as
uncovered and reproduces the old `None`-per-cell behaviour exactly.

### Byte-exactness after the fix

| stream | before | now |
|---|---|---|
| B 512x384 testsrc2 cq45 `--enable-palette=1 --enable-tx-size-search=0` | 528/294912 luma | **BYTE-EXACT** (`cmp` vs ffmpeg `-pix_fmt yuv420p`) |
| A 256x192 testsrc2 cq45 var-tx (`--enable-tx-size-search=1`) | BYTE-EXACT | **BYTE-EXACT** |

Standalone arms re-run (all `cmp`-exact against ffmpeg): `cq50p0-txs0`,
`cq45p0-txs0`, `cq45p1-txs0`, `cq50p0-txs0`, `cq50p0-1to4-off`, `minp4`
(`--min-partition-size=4 --max-partition-size=16`), `cq50-txs1-1to4-off`. The
`(84,104)` DV trace now reads `dv_row=-512 rng=33207`, the oracle's own value.

### New reviewer-unblock gate

`stream::tests::a_coded_rect_intrabc_block_reconstructs_in_both_orientations`
-- three arms, ALL frames byte-exact against ffmpeg, each asserting the arm
reconstructed a CODED (`skip == 0`) rect intrabc block in the orientation it
names:

- `horz-coded-256-cq45-pal0-txs0` -- 16x8 coded strip, HORZ.
- `vert-coded-512-cq45-pal1-txs0` -- 8x16 coded strip, VERT, **and the
  DV-predictor regression stream** (this is stream B).
- `txmode-select-256-cq45-pal0-txs1` -- stream A's shape; the arm re-derives
  `tx_mode == Select && allow_intrabc` out of the frame header rather than
  trusting the encoder flags.

New instrument `decode::intrabc_rect_coded_hits() -> [horz, vert]` (bumped in
`decode_intrabc_rect` when `!skip`, indexed by `bh > bw`): the old
`intrabc_rect_hits` counts neither the skip flag nor the orientation, so an arm
that quietly became all-skip or single-orientation would keep it green
(`gate-blind-to-feature`).

**Non-vacuity (fail-pre-fix, measured):** with only the `MiGrid::get` cover
fallback neutralized (the fix's load-bearing line) and nothing else touched,
the gate fails exactly where it must --

```
horz-coded-256-cq45-pal0-txs0 ... exact, coded rect intrabc horz=1 vert=0
vert-coded-512-cq45-pal1-txs0 frame 0 does not match ffmpeg
test ... FAILED
```

and passes again with the line restored.

### Gates run this round

- `cargo check -p ec-av1 --all-targets` -> 0 warnings.
- New gate (3 arms, 3 frames, byte-exact; fail-pre-fix proven above).
- `a_real_aomenc_screen_key_frame_reads_use_intrabc_on_rect_strips` -> ok
  (187 rect reads, 5 intrabc blocks over 5 arms, 8 frames compared, 0
  refused, 0 mismatched, 1 rect-strip block).
- `an_intrabc_block_under_tx_mode_select_decodes_pixel_exact` -> ok
  (cq30 16 blocks / 1 var-tx, cq45 12 blocks / 3 var-tx).
- Refusal suite `-- refusal` -> **21 passed**. `hunger_games` -> 3 passed.
  `monochrome` -> 1 passed. `cdf` -> 13 passed. `non_420` -> 1 passed.
- Full `--lib` suite, re-measured 2026-09-23 on commit `db15bb5a` (see below):
  `test result: ok. 599 passed; 0 failed; 60 ignored; 0 measured; 0 filtered out; finished in 10087.77s`

### Deferred / open

- 1:4 pair-strip intrabc -- unchanged, named in §3.
- The cover records the mi span its `fill_lf_grid_rect` caller passes; a
  sub-8x8 leaf publishes a 4x4 (1x1 mi) or an 8x8-square span exactly as its
  own `fill_skip_grid`/`fill_lf_grid` call already does, so the coverage map
  inherits that convention. No gate or arm reaches a sub-8x8 neighbour of an
  intrabc block today; if one ever does, the leaf's true `bsize` dims are what
  libaom's `mi_size_wide/height[candidate->bsize]` would report.


### Verified 2026-09-23 (r4 commit `db15bb5a` was UNVERIFIED)

Re-measured on that committed tree. Probe rebuilt from it
(`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1ibc`). 8-bit comparison is
`decode_probe <obu> <raw>` vs `ffmpeg -pix_fmt yuv420p -f rawvideo`. `git
status --porcelain` was empty before the suite and empty after it: the suite
ran these bytes.

| stream | verdict |
|---|---|
| A 256x192 testsrc2 cq45 `--enable-palette=0 --enable-tx-size-search=1` | **BYTE-EXACT** (73728 B) |
| B 512x384 testsrc2 cq45 `--enable-palette=1 --enable-tx-size-search=0` | **BYTE-EXACT** (294912 B; was 528/294912 luma) |

Previously-green arms, all `cmp`-exact: `cq50p0-txs0`, `cq45p0-txs0`,
`cq45p1-txs0`, `minp4` (`--min-partition-size=4 --max-partition-size=16`),
`smptebars` cq40 and cq45.

Unblock gate
`a_coded_rect_intrabc_block_reconstructs_in_both_orientations` (with the
in-gate and the tx-mode-select witness, one `cargo test` invocation):

```
horz-coded-256-cq45-pal0-txs0 1 frame(s) exact, coded rect intrabc horz=1 vert=0
vert-coded-512-cq45-pal1-txs0 1 frame(s) exact, coded rect intrabc horz=0 vert=1
txmode-select-256-cq45-pal0-txs1 1 frame(s) exact, coded rect intrabc horz=1 vert=0
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 656 filtered out; finished in 3.45s
```

In-gate `a_real_aomenc_screen_key_frame_reads_use_intrabc_on_rect_strips`: 187
rect-strip `use_intrabc` reads, 5 intrabc blocks over 5 arms, 8 frames
compared, 0 refused, 0 mismatched, 1 rect-strip block.
`an_intrabc_block_under_tx_mode_select_decodes_pixel_exact`: cq=30 16 blocks /
1 var-tx, cq=45 12 blocks / 3 var-tx.

Regression filters (same tree, `--nocapture --test-threads=1`):

- `-- refusal` -> **21 passed** (111.09s).
- `monochrome` -> 1 passed: 4:2:0 twin 60 frames, monochrome 60 frames
  byte-exact vs ffmpeg.
- `non_420` -> 1 passed.
- `quantisation` -> 1 passed: qm-off control 3 frames, qm-on refused by name
  (`a frame using quantisation matrices`).
- `cdf_update` -> 1 passed: 3 `disable_cdf_update` frame headers, all frames
  sample-exact.
- `hunger_games` -> 3 passed.
- `a_10bit_film` -> 5 passed (126.80s), including
  `hg_rect64` 33 frames, `hg_arf` 37 frames, `hg_head_mvclamp` 52 frames.

Wave-1 probes (release `decode_probe`, not the debug suite):

- gray 60-frame key frame (`testsrc2` 320x240 2s `-pix_fmt gray -c:v
  libaom-av1`): `OK: 60 frames`, low byte of `EC_PROBE_OUT16` EQUAL to
  `ffmpeg -pix_fmt gray` (4,608,000 B).
- `fixtures/bitstreams/av1-monochrome.ivf` remuxed to OBU: `OK: 60 frames`,
  EQUAL the same way.
- `av1-profile1-444.ivf` remuxed to OBU: REFUSED by name (`a chroma format
  other than 4:2:0`).
- every `crates/ec-av1/fixtures/hg_*.obu` (8 files) EQUAL to
  `ffmpeg -pix_fmt yuv420p10le` (`EC_PROBE_OUT16`).

`cargo check -p ec-av1 --all-targets` after `touch` of `lib.rs`: 0 warnings
(`Finished dev profile in 1.15s`, `CHECK_RC=0`, no `warning:` lines).

Full suite, hub process `av1ibc-suite`, `cargo test -p ec-av1 --lib --
--test-threads=1`, `EC_AV1_REQUIRE_AOMENC=1`, `TMPDIR` on `$HOME`. Literal
line:

```
test result: ok. 599 passed; 0 failed; 60 ignored; 0 measured; 0 filtered out; finished in 10087.77s
```

599 = 598 (this branch's base, the txrouting merge at `3ecd4a10`, before
txbands) + this lane's one new gate. Main's `600/0/60` includes txbands' two
gates, which are not on this branch. Not a missing test. Hub log grep for
`FAILED` was empty. Exit 0.

The first suite attempt (default threads) did not finish. After 3h20m it was
deadlocked in `run_8x8_leaf_motion_gate`
(`a_real_warped_causal_8x8_leaf_stream_decodes_pixel_exact`): `write_all` of
the y4m runs before `wait_with_output`, and that aomenc's stdout pipe had been
shrunk to 8192 bytes (a fresh pipe on this box is 65536). Both sides sat in
`anon_pipe_write`. Pre-existing spawn pattern, not this lane's diff. Stopped
(exit 143) and re-run single-threaded; that run is the line above.
`deferred(concurrent stdin/stdout drain for aomenc spawns that write_all
before wait_with_output -- unblocked by a charter that may edit the spawn
helper)`.

## 4e. Round 5 (2026-09-23) -- the skip arm's residual slice was sized `bw * bh`, not `side * side`

### Root cause (the reviewer's one P1)

`decode_intrabc_rect`'s skip arm (pre-fix `decode.rs:11047-11049`) passed
`push_mc_rect(..., stride = side = bw.max(bh), w = bw, h = bh, ...,
&ZERO_RESIDUAL[..bw * bh])` (chroma: `cw * ch`). `PlaneBuf::reconstruct_mc_rect`
(`decode.rs:25726-25731`) indexes `residual[row * side .. row*side + w]` for
`row in 0..h`, needing up to `(bh-1)*side + bw` elements. For a VERT strip
(`bh > bw`) that EXCEEDS the slice: a skipped 8x32 strip reads a 256-element
slice at stride 32 and panics at row 8 -- `range end index 264 out of range
for slice of length 256`. The panic is a bounds check, never silent
corruption; a HORZ strip (`bw > bh`) never overruns (`(bh-1)*side + bw <=
bw*bh` when `bw == side`).

### Why every gate missed it

Two stacked blind spots. (1) Only the default single-threaded path crashes:
at `EC_AV1_RECON_THREADS == 1` the rect is reconstructed inline through
`ReconPlanes` (the `dense_residual` read above); on the wavefront path the
`Residual::done` ZERO_RESIDUAL pointer-compare maps the slice to the empty
marker and never indexes it. (2) Every existing VERT intrabc witness was
CODED (`skip == 0`), so no gate ever walked the skip arm on a VERT strip
(`gate-blind-to-feature`: the counters counted neither the skip flag nor the
orientation for skipped blocks).

### Fix (bug-for-bug with the proven inter path)

Three lines, `decode.rs` skip arm: `&ZERO_RESIDUAL[..side * side]` and
`&ZERO_RESIDUAL[..cside * cside]` (x2), exactly what the inter skip arm at
`decode.rs:28906-28916` passes. `(h-1)*side + w <= side*side` for every rect
shape.

### Repro (before) / verdict (after)

Reviewer's corpus at `~/.cache/tmp-av1ibcreview/repro`. Pre-fix, stock
release `decode_probe sweep_testsrc2_cq30_p0.obu` panics with the exact
message above (`decode.rs:25731:32`); post-fix it decodes clean, rc=0. The
new gate arm was also proven fail-pre-fix: with only the three fix lines
reverted, `a_coded_rect_intrabc_block_reconstructs_in_both_orientations`
panics at the same index (`264 out of range for slice of length 256`,
`decode.rs:25755:32` after the counter edit shifted lines).

### New gate arm (non-vacuous)

`stream::tests::a_coded_rect_intrabc_block_reconstructs_in_both_orientations`
gains two SKIPPED-strip arms, `vert-skip-512-cq30-pal0-txs0` and
`vert-skip-512-cq30-pal1-txs0` (testsrc2 512x384, `--cq-level=30`, palette
off AND on, txs off -- the reviewer's panic recipe family). Each arm decodes
the stream at 1 AND 4 recon threads and asserts byte-exact vs ffmpeg on
BOTH paths, and a new counter `decode::intrabc_rect_skipped_hits()` (bump in
`decode_intrabc_rect`, `[horz, vert]` split, reset with the coded one)
HARD-asserts a SKIPPED VERT 2:1 strip was actually reconstructed -- an arm
that stops producing one fails instead of passing vacuously. Measured this
round:

```
vert-skip-512-cq30-pal0-txs0 1 frame(s) exact at 1 recon thread(s), skipped rect intrabc horz=11 vert=2
vert-skip-512-cq30-pal0-txs0 1 frame(s) exact at 4 recon thread(s), skipped rect intrabc horz=11 vert=2
vert-skip-512-cq30-pal1-txs0 1 frame(s) exact at 1 recon thread(s), skipped rect intrabc horz=1 vert=2
vert-skip-512-cq30-pal1-txs0 1 frame(s) exact at 4 recon thread(s), skipped rect intrabc horz=1 vert=2
```

### Corpus sweep (post-fix, release `decode_probe` on the committed tree)

All 39 obus in `~/.cache/tmp-av1ibcreview/repro` decode without panic
(rc=0, no `panicked` in output). Byte-exactness where the reviewer compared:

| group | result |
|---|---|
| sweep_* 16 streams with a non-zero ffmpeg ref | all 16 `cmp`-EXACT (294912 B each) |
| sweep_* 14 streams whose ref is 0 B (the reviewer's panicking cells) | all decode clean now, no ref to compare |
| A.obu / B.obu / C_gradients / C_mandelbrot / C_smptebars | all `cmp`-EXACT vs `*_ref.raw` |
| D.obu | probe dispatches 0 frames (unchanged: reviewer's own `D_ours.raw`/`D_ours2.raw` are 0 B too) |
| gray60.obu, av1-monochrome.obu | `cmp`-EXACT vs `gray_ref.raw` / `mono_ref.raw` |
| hg_* 10-bit (8 fixtures, `EC_PROBE_OUT16`) | all 8 `cmp`-EXACT vs `ref_hg_*.raw` |
| av1-profile1-444.obu | still REFUSED by name (`a chroma format other than 4:2:0`) |

### Full suite

`test result: ok. 599 passed; 0 failed; 60 ignored; 0 measured; 0 filtered out; finished in 9555.99s` — hub-supervised `--test-threads=1` on the committed tree (`86ba99f6`), exit 0.

## 6. Merge-side follow-ups (merged into `main` as daf83f3a)

Merged as **daf83f3a** (`Merge lane-av1-intrabc: rect-strip intrabc reconstruction
(2:1 + skipped strips)`), parents ed12fe52 (main) + c07da1c0 (lane head).
Create-list audit vs first parent: exactly ONE new path, `lanes/av1intrabc.report.md`
(5 files changed, +1429/33). Conflict resolution followed av1rect14 §7's riders:
at the three 2:1 call sites this lane's `decode_intrabc_rect` won with the r5
skip-stride fix already in (rider 1 satisfied), and `decode_intrabc_owned_rect`
keeps only the fourth site it uniquely covers (`decode_block_rect64`, rider 2).
Fixture copy: not needed — `diff` of gitignored `fixtures/bitstreams/` between
main and the lane worktree is empty.

### Merged-tree gates (private CARGO_TARGET_DIR / TMPDIR for this close, on `$HOME`)

- `cargo check -p ec-av1 --all-targets`: exit 0, **0 warnings**.
- `a_coded_rect_intrabc_block_reconstructs_in_both_orientations` (5 arms, 2 of
  them the r5 skipped-strip arms), `EC_AV1_RECON_THREADS=1` AND `=4`, both:
  `1 passed; 0 failed; 663 filtered out` with all seven arm lines — horz-coded
  `horz=1 vert=0`, vert-coded `horz=0 vert=1`, txmode-select `horz=1 vert=0`,
  vert-skip pal0/pal1 `skipped rect intrabc horz=11 vert=2` / `horz=1 vert=2`
  at 1 AND 4 recon threads.
- rect14's five gates on the merged tree: `5 passed; 0 failed; 659 filtered
  out`; the in-gate `a_real_aomenc_screen_key_frame_reads_use_intrabc_on_rect_strips`
  totals: 187 rect-strip use_intrabc reads, 5 intrabc blocks over 5 arms, 0
  refused, 8 frames compared, 3 out of scope (0 mismatched), 1 rect-strip block.
- Refusal suite `-- refusal`: `21 passed; 0 failed` (110.21s).

### Merged-tree probes (release `decode_probe` from daf83f3a, private target)

All `cmp` vs fresh ffmpeg oracles; scratch under `$HOME/.cache`:

| probe | verdict |
|---|---|
| A 256x192 cq45 pal0 txs1 (var-tx arm, §4b/§4c recipe) | EXACT 73728 |
| B 512x384 cq45 pal1 txs0 (coded 8x16, DV-regression stream) | EXACT 294912 |
| ibc640 (av1rect14 §2 recipe) | EXACT 460800, `rect4_16_pair: intrabc=1` |
| cq50 arm (256x192 cq50 pal0 txs0) | EXACT 73728 |
| loss320 (lossless sb128 1to4 min4) | luma rows 0-127 EXACT, first diff byte 41121 (the §2 report's 41120, 0-indexed), `rect4_16_pair: lossless_chroma=6` |
| mono 60f (`av1-monochrome.ivf` remux, OUT16 low byte) | EXACT vs ffmpeg gray |
| hg_* 8 committed 10-bit fixtures | 8/8 EXACT vs yuv420p10le |
| av1-profile1-444 | REFUSED by name (`a chroma format other than 4:2:0`), 0 bytes, 0 frames dispatched |
| aomenc `--enable-qm=1` / `=0` | qm=1 REFUSED by name (`a frame using quantisation matrices (using_qmatrix=1)`), 0 bytes; qm=0 control EXACT |
| `--cdf-update-mode=0` 3-keyframe stream | 3/3 headers `disable_cdf=true`, byte-exact |

### Full suite on the merged tree

- Attempt 1 (hub-supervised `--test-threads=1`, `EC_AV1_REQUIRE_AOMENC=1`):
  deadlocked after 2h47m in
  `a_real_aomenc_inter_sequence_with_intra_16x4_strips_in_1to4_partitions_decodes_pixel_exact`
  — the KNOWN pipe-drain infra deadlock (`write_all` of the y4m before
  `wait_with_output` while aomenc's stdout pipe fills; aomenc sat 29 min in
  `anon_pipe_write` with the test blocked in `write_all`; same class as the
  feeder-thread fix already at one stream.rs site). Killed ONLY the stuck
  aomenc child; the test failed on its own EPIPE and the suite drained:
  `test result: FAILED. 603 passed; 1 failed; 60 ignored; 0 measured; 0 filtered out; finished in 11764.21s`
  — the 1 failed is that killed-child artifact, nothing else.
- The same test standalone on the same bytes, TWICE, both green:
  `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 663 filtered out; finished in 84.51s`
  and `... finished in 84.46s`. The gate's own in-suite summary had already
  printed its decode verdict before the broken pipe: 3 pixel-exact attempts
  carrying an intra 1:4 strip, per-arm attempts 16x4/4x16/chroma-reference
  [3, 3, 3], 0 mismatched, 0 named refusals — the decode under test was exact;
  only the spawn helper's pipe drained (panic is the test's own
  `expect("writing y4m to aomenc")` at stream.rs:15865).
- Batch verdict: **604/0/60 effective** on the committed bytes — 603 in-suite +
  the killed-child test green twice standalone; 603 + 1 + 60 = 664, matching
  the filtered-count arithmetic (664 total = main-after-rect14's 603-test total
  + this lane's one new gate function,
  `a_coded_rect_intrabc_block_reconstructs_in_both_orientations`; the r5 arms
  extended that same function).

### Carried deferrals (unchanged by the merge)

- **128-level rect intrabc refusal** — `decode_block_128rect`'s named refusal
  stays; `deferred(a reaching sb128 screen stream that codes a 128-level
  HORZ/VERT/1:4 intrabc strip)` (also carried in av1rect14 §9).
- **D.obu dispatches 0 frames** — reviewer-corpus stream decodes nothing on
  both sides (the reviewer's own reference dumps are 0 B too); unchanged, not
  this lane's regression.
- **aomenc pipe-drain test-infra deadlock** — SEEN LIVE during this close
  (attempt 1, above); ~150 `write_all`-then-`wait_with_output` spawn sites
  remain, one site already has the feeder-thread fix.
  `deferred(concurrent stdin/stdout drain for aomenc spawns -- unblocked by a
  charter that may edit the spawn helper)`.
- **Sibling lanes in flight, merge later**: lane-av1-loss64 (in review) and
  lane-av1-444 — not gated here; they merge on their own closes.

### Cleanup (post-push, same batch)

`git worktree remove ../edith_codecs-av1ibc`, `git branch -d lane-av1-intrabc`,
sweep `$HOME/.cache/cargo-target-av1ibc*`, `$HOME/.cache/tmp-av1ibc*`,
`$HOME/tmp-av1ibc*` (lane, r5 and this close's private dirs; peer-owned
`av1l64*`/`av1444*` untouched).
