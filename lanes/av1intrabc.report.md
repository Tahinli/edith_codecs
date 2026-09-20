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
