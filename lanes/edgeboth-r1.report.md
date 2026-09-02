# lane-edgeboth r1 -- both-axes-cut partition node

## What changed
- `crates/ec-av1/src/decode.rs:1160` new `EDGE_BOTH_CUT_HITS` counter (`[16,32,64,128]` levels)
  + `bump_edge_both_cut`, `edge_both_cut_hits`, `reset_edge_both_cut_hits`.
- `crates/ec-av1/src/decode.rs` intra path: 128 level (`~13100`), 64 level (`~13435`),
  32 level (`~13511`) already inferred the forced SPLIT -- they now only bump the counter.
  16 level (`~13990`): the refusal `a 16x16 block whose true edge cuts through both axes ...`
  is replaced by the libaom rule `else if (!has_rows && !has_cols) partition = PARTITION_SPLIT;`
  (decodeframe.c `decode_partition`), falling into the existing SPLIT leaf loop whose
  `mr < mi_rows && mc < mi_cols` filter IS libaom's `if (mi_row >= mi_rows || mi_col >= mi_cols) return;`
  -- so the three out-of-frame quadrants get no neighbour/tx-grid write and no symbol.
- `crates/ec-av1/src/decode.rs` inter path: 64 level (`~21190`), 32 level (`~21375`) bump only;
  16 level (`~21440`): the refusal `a 16x16 inter block whose true edge cuts ...` is gone,
  `part16` is `PARTITION_SPLIT` with NO symbol read in the `(false,false)` case.
- `crates/ec-av1/src/refusal_inventory.rs`: both refusal strings dropped (47 -> 45).
- `crates/ec-av1/src/stream.rs`: gate `a_real_aomenc_both_axes_cut_corner_block_decodes_pixel_exact`
  (+ its `#[ignore]`d inter twin) and the shared `both_axes_cut_sweep` helper.

## Gate
```
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-edgeboth EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 \
PATH="$HOME/.cache/aom-oracle/build:$PATH" cargo test -p ec-av1 --lib both_axes_cut -- --nocapture
```
Recipe: real aomenc, sizes 200x72 / 136x104 / 264x136 / 72x200 / 104x136 (w mod 16 == 8 AND
h mod 16 == 8), 8- and 10-bit, cq 32 and 45, `--lag-in-frames=0` (decode order == display order,
every frame compared vs ffmpeg), rect/AB/1:4 + palette/intrabc + the inter compound set + the
intra tool set + `--enable-tx-size-search=0` pinned off.

EVIDENCE: `$HOME/.cache/edgeboth-suite-r1.log` (gate line) | 5 sizes x 2 depths x 2 cq key-frame
attempts, aomenc -> ec-av1 -> ffmpeg full-frame compare | `20 attempts pixel-exact over 5 sizes
(key arm), out_of_scope_mismatch=0, both_cut hits [16,32,64,128]=[20,20,12,0], 0 named refusals`

EVIDENCE: same run, control | sizes list swapped to 192x72 / 200x80 (no both-cut node anywhere)
| `192x72 key 8-bit cq 32 decoded pixel-exact but no 16-level both-axes-cut block fired
(hits=[0,0,0,0])` -- the counter is specific to the case, not to frame size.

EVIDENCE: same run, inter arm | 200x72 inter cq 32 6 frames | `frame 2 luma diff bbox rows
64..=71 cols 15..=186` (corner cols 192..=199 CLEAN) and control `192x72 inter 8-bit cq 32
frame 2 luma diff bbox rows 64..=71 cols 8..=188` with hits `[0,0,0,0]` -- the inter mismatch is
the one-axis frame-edge half-strip BAND, reproducible with zero both-cut nodes.

Recipe choices, measured r1 (`~/.cache/edgeboth-tmp/probe.sh`, `probe2.sh`): without the intra
tool set + `--enable-tx-size-search=0`, 30/30 inter attempts refused on other lanes' strings
(`a nonzero angle delta on an 8x8 intra leaf in an inter frame`, `a non-DC chroma mode on an 8x8
inter-frame leaf`, `an 8x8 intra leaf in an inter frame whose tx_depth splits it into 4x4
transform units`, `an inter partition below 8x8`). cq 58 and the testsrc2 source are excluded:
they reach `a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding`
and `an inter 16x16-level AB or 1:4 partition` first.

## Suite
`cargo test -p ec-av1 --lib` (unit `edgeboth-suite-1788331316`, log `$HOME/.cache/edgeboth-suite-r1.log`):
**383 passed, 0 failed, 34 ignored**, 961s.

## Residue
- deferred(lane-oddh's inter frame-edge half-strip band defect): the INTER half of the lift is
  code-verified and its corner pixels are exact, but its gate is `#[ignore]`d -- DEVIATION from
  COMMON's "a refusal is lifted only together with a gate": the inter refusal string was dropped
  anyway because it is factually wrong (no rectangular transform is needed; the node is a forced
  split), and keeping it would hide the band defect that the control 192x72 proves is independent.
- deferred(a 128-superblock recipe at a both-cut size): `hits[3] == 0`; the 128 level's forced
  split is code-only, unchanged behaviour, counter added.
- accepted: the WRITER-side twin in `crates/ec-av1/src/tile.rs:1155` (`... this writer does not
  code yet`) is untouched -- encoder scope, not in `refusal_inventory.rs`, and a lift there needs
  its own roundtrip gate.
