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
`decode_block_rect4` (32x8/8x32 + 16x8/8x16), `decode_leaf_rect`,
`decode_block_rect64`, `decode_block_128rect`; engagement counters
`intrabc_rect_hits` / `intrabc_rect_vartx_hits` exposed on `decode_probe`.

## 2. Byte-exactness witnesses

In-gate (flipped pin -> witness): `stream::tests::
a_real_aomenc_screen_key_frame_reads_use_intrabc_on_rect_strips`. Its
`testsrc2-cq50-txs0` arm used to stop at the rect-strip refusal; it now decodes
1 frame, **1 intrabc block on a HORZ/VERT rect strip**, pixel-exact vs ffmpeg.
Gate totals: 187 rect `use_intrabc` reads, 5 intrabc blocks over 5 arms,
8 frames compared, 0 mismatches, 0 named refusals.

Standalone recipes (aomenc oracle at `~/.cache/aom-oracle/build/aomenc`, probe
`cargo run --release -p ec-av1 --example decode_probe -- <obu>`):

| recipe (base: `--cpu-used=0 --tune-content=screen --enable-intrabc=1
--min-partition-size=8 --max-partition-size=32 --sb-size=64 --enable-palette=1
--enable-tx-size-search=0 --enable-rect-partitions=1 --enable-1to4-partitions=1`) | intrabc rect blocks | shape (mi) | skip | result |
|---|---|---|---|---|
|`testsrc2=s=256x192:r=25 --cq-level=50 --enable-palette=0`, 1:4 off|0|--|--|REFUSED (16x8 CPR block, `--enable-1to4-partitions=0`)|
|`testsrc2=s=256x192:r=25 --cq-level=50 --enable-palette=0 --min-partition-size=4 --max-partition-size=16`|1|16x8 @(38,48)|0|**EXACT**|
|`testsrc2=s=256x192:r=25 --cq-level=45 --enable-palette=0`|1|16x8 @(38,48)|0|**EXACT**|
|`smptebars=size=512x384:rate=25 --cq-level=50`|2|16x8 @(76,92) skip=0, 8x32 @(80,104) skip=1|mixed|**EXACT**|
|`smptebars=size=1024x768:rate=25 --cq-level=50`|1|8x32 @(64,74)|1|**EXACT**|
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
strip is coded as a PAIR (both children share one chroma block), so the chroma
prediction comes from the pair's top-left mi and the chroma coefficient state
is written over the pair span -- a different neighbour/bookkeeping model from
the 2:1 helper above. Reachable witnesses for whoever takes it next:
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

## 5. Deferred

1. **1:4 pair-strip intrabc** -- named above, with the model difference and the
   witness recipes. `deferred(1:4 pair-chroma model)`
2. **Full-suite gate** -- `deferred(batch-level run after lane-av1-txbands
   merges; same file, mid-flight run would measure a half-built tree)`
