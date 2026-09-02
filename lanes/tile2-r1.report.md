# lane-tile2 r1 — multi-tile inter decode

## Verdict: GREEN on the chartered defect (the reproducer is un-ignored and passes);
one NEW, pre-existing residual found and left RED behind an `#[ignore]` (var-tx + multi-tile chroma).

## Root cause (charter defect)
`decode::build_motion_field` (crates/ec-av1/src/decode.rs:2507) read the frame's mi grid
through `MiGrid::get`, which narrows to the CURRENT tile's bounds. That loop runs AFTER the
whole tile walk, so the grid still carried the LAST tile's bounds and returned `None` for every
cell of every other tile: those tiles' motion field stayed empty and the next frame's temporal
MV candidates there were all zero. Class `new-map-ignores-tile-edge`, INVERTED — a frame-wide
consumer inheriting a neighbour-scan narrowing. libaom's `av1_copy_frame_mvs` is called per
block over the whole frame and knows nothing about tiles.
Fix: `MiGrid::get_any` (crates/ec-av1/src/mvstack.rs:263) for frame-wide readers; the intrabc DV
candidate grid additionally gets this tile's bounds in both tile decoders (decode.rs:13122,
decode.rs:20799).

## Neighbour-map sweep (charter step "sweep EVERY neighbour read for the tile guard")
Two bands were missing their reset (class: reset list missing a table):
- `above_txfm` was never reset per tile. libaom `av1_zero_above_context`
  (~/.cache/aom-oracle/src/av1/common/av1_common_int.h:1622) memsets `above_contexts.txfm` to
  `tx_size_wide[TX_SIZES_LARGEST]` (= 64 = our `TXFM_CTX_INIT`) over the tile's mi column span at
  every tile start. Without it the first mi row of a SECOND TILE ROW reads the previous tile
  row's var-tx partition context. Fixed in `Neighbours::start_tile`.
- `left_txfm` was never reset per superblock row. libaom `av1_zero_left_context`
  (av1_common_int.h:1628) memsets `left_txfm_context_buffer` to `tx_size_high[TX_SIZES_LARGEST]`
  at every SB row inside a tile; a SECOND TILE COLUMN revisits the same mi rows and read the
  left tile's context. Fixed in `Neighbours::start_row` (called per SB row, decode.rs:21176).
  HONEST SCOPE: both are libaom-cited conformance fixes; they are INERT on every stream measured
  here (ablation below), so they are code-verified, not stream-proven.
Checked and found already correct: `find_samples`/`has_top_right` (out-of-tile neighbours are
`None` through `MiGrid::get`), `tx_size_context_txfm` (`mi_r > tile_row0_mi` / `mi_c > tile_col0_mi`),
palette above bands (`r*(SUB/MI) > tile_row0_mi`), `above_uv_mode` (tile_row0 guard),
all the mi-granular inter bands (reset over the tile's column span in `start_tile`).
`tile.rs`'s `r > 0`/`c > 0` availability is our single-tile WRITER, not the decoder — accepted.

## Gates
`a_real_aomenc_multi_tile_gate` (crates/ec-av1/src/stream.rs): real aomenc, 256x128, 16 frames at
25 fps, compound refs + `--enable-ref-frame-mvs=1` + OBMC, EVERY decode-order frame compared
against the instrumented oracle aomdec (hidden alt-refs included), all planes; per attempt it
HARD-ASSERTS `tile_hits >= 2 * frames` (the tile flag arrived) and `tile_reach_clips > 0` (at
least one block past mi 0 clamped its MV-candidate scan to a tile origin, i.e. read a guarded
neighbour). 3 seeds x 3 cq each.
- `a_real_aomenc_compound_mv_stack_across_two_tile_columns_decodes_pixel_exact` — un-ignored, PASS
- `..._in_two_tile_columns_...` (2 cols) — PASS
- `..._in_two_tile_rows_...` (2 rows) — PASS
- `..._in_a_two_by_two_tile_grid_...` (2x2, 8-bit) — PASS
- `a_real_aomenc_10bit_stream_in_a_two_by_two_tile_grid_...` (2x2, 10-bit) — PASS
- `..._in_a_tile_grid_with_var_tx_...` (2x2 + `--enable-tx-size-search=1`) — RED, `#[ignore]`d

EVIDENCE: $HOME/.cache/tile2-suite-r1.log | cargo test -p ec-av1 --lib (systemd unit, MemoryMax=10G) | see totals below
EVIDENCE: gate stderr | 4 tile arms x 3 seeds, every decode-order frame vs oracle aomdec | e.g. 2x2 seed 320: 16 frames exact, tile_reach_clips +360, tiles 64; 2 cols seed 300: 17 frames exact (1 hidden), clips +256; 10-bit seed 330: 17 frames exact (1 hidden), clips +384
EVIDENCE: ~/.cache/tile2-tmp/{enc.sh,enc2.sh,a.obu,e.obu} | aomenc with/without --enable-tx-size-search=1, tx_mode read back through our own parser | base tile recipe = 16/16 frames TX_MODE_LARGEST (var-tx path never entered); with lag-0/no-alt-ref/min-part-16 = 4/16 TX_MODE_SELECT

## Open residue
- deferred(next round): var-tx + multi-tile CHROMA residual — `..._tile_grid_with_var_tx_...`,
  decode-order frame 3, byte 35079 (U plane, ours 111 vs oracle 112), 16 bytes, reproduced
  byte-identically twice. ABLATION: removing this round's two context resets reproduces the
  SAME byte and count, so it is pre-existing and independent of them. Unblocked by an
  EC_TRACE_MODE range ladder on that frame's first chroma TU.
- accepted: the two txfm-context resets are libaom-cited but stream-inert here (no gate can
  currently show them; the only var-tx multi-tile arm is red for the unrelated reason above).

## r1 close-out (after merging main 1176a16)

VERDICT: **RED — do not merge as-is.** The chartered defect is fixed and gated, but the fix
REDDENS a sibling gate (class `fix-trades-sibling-gate`).

Full suite, tip ee24353: `test result: FAILED. 400 passed; 1 failed; 34 ignored`
(`$HOME/.cache/tile2-suite-r1b.log`, one systemd unit, MemoryMax=10G, 1143 s).
The single failure is `stream::tests::a_frame_edge_straddling_band_decodes_pixel_exact`:
`192x68 cq35 frames=5 10bit=false tile_cols=1 frame 1 plane Y: 164 pixels differ, first at
row 59 col 146 (ours 113 vs ffmpeg 114)`.

ABLATION (three builds of that one test, this worktree, decode.rs edited in place and restored):
- both this round's decode changes removed (`get_any` -> `get`, both txfm resets dropped): **PASS**
- `get_any` kept, both txfm resets dropped: FAIL, 3939 pixels differ, first at row 0 col 129
- tip (both): FAIL, 164 pixels differ, first at row 59 col 146

Reading: `build_motion_field`'s `get_any` is what flips this gate. Before it, a second tile
column's motion field was EMPTY, so frame 1's temporal MV candidates there were all zero — and
that emptiness happened to match the oracle on this 192x68 two-tile-column stream, while the
two-tile-column compound MV-stack reproducer needed the real data. So the real temporal-MV
projection for cells coming from another tile is still wrong somewhere downstream
(`motion_field.rs` projection / `mvstack` temporal candidate), and the old `get` narrowing was
masking it. The txfm resets are NOT inert on this stream (3939 -> 164 pixels), which also means
this gate exercises `TxMode::Select` across tile columns — the var-tx arm's chroma residual and
this luma band may share a cause.

EVIDENCE: $HOME/.cache/tile2-suite-r1b.log | one systemd unit on tip ee24353 | test result: FAILED. 400 passed; 1 failed; 34 ignored
EVIDENCE: ablation runs above | decode.rs edited in place, single test by name, file restored from ~/.cache/tile2-tmp/decode.rs.tip | PASS / 3939 px / 164 px

NEXT STEP for r2: pin the 192x68 `tile_cols=1` stream that gate builds, and range-ladder frame 1
against aomdec from the first block of tile column 1 whose temporal candidate list is non-empty
(`EC_TRACE_MODE`, compare msac RANGE) — the first diverging element decides whether the defect is
the temporal projection or the var-tx context after the reset.
