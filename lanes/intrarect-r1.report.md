# lane-intrarect r1 — intra rect strips on the inter block path

Branch `lane-intrarect` off `lane-rectsplitx` cbf8ffd. **Result: code landed, gate RED
(one real residual defect, evidence below). The lane does not merge in this state.**

## What changed

- `crates/ec-av1/src/decode.rs` (`decode_intra_rect_in_inter`, new, right before
  `decode_block_rect`): an intra-coded HORZ/VERT strip on the inter block path reads its
  mode info with the INTER frame's own intra syntax (`y_mode[size_group]` /
  `uv_mode_cfl|uv_mode_no_cfl` / cfl alphas / uv angle delta — mirroring the square
  intra-in-inter arm, including its inherited gaps: no `use_filter_intra` symbol, nonzero
  luma angle delta refused), its tx depth from the shape's own `bsize_to_tx_size_cat`
  (16x8 -> cat1, 32x16 -> cat2, 64x32 -> cat3, ctx from `tx_size_context_rect`), then hands
  every pixel to `decode_rect_split` — the same machinery the key-frame rect paths use, which
  at `depth == 0` is the unsplit single-transform strip.
- `crates/ec-av1/src/decode.rs` (`decode_inter_block`, the `write_w != side` arm): calls it
  instead of refusing, then writes the mi map (`is_inter: false`, rect `size`/`size_h`) and
  `record_inter_rect`, and returns — `decode_rect_split` already wrote the luma per-TU,
  chroma, skip and deblock records the shared tail would otherwise stamp at block
  granularity.
- `crates/ec-av1/src/decode.rs`: the `reject_residual && !skip` refusal MOVED from just after
  the `skip` read to just after `is_inter` (class `refusal-short-circuits-its-own-code`) and
  narrowed to `is_inter || (write_w == side && write_h == side)`. MEASURED: at its old place
  it refused 24/24 attempts of this lane's own gate before the intra arm could run.
- `crates/ec-av1/src/refusal_inventory.rs`: lifted "an intra-coded HORZ/VERT strip needs
  rectangular intra prediction this decoder does not code yet"; added the residue
  "an intra-coded 1:4 (or other non-2:1) rect strip on the inter block path" (a 1:4 strip
  breaks the size-group/tx-cat diagonal the 2:1 shapes share, so it is refused rather than
  read from the wrong CDF row).
- `crates/ec-av1/src/stream.rs`: gate
  `a_real_aomenc_inter_sequence_with_an_intra_rect_strip_decodes_pixel_exact` (+ `_10bit`),
  45 attempts sweeping seed x `--cpu-used` 0..4 x `--cq-level` {30,20,12}, per-shape counter
  `decode::intra_rect_strip_in_inter_hits(0|1|2)`, every frame Y/U/V compared, no SKIP on a
  decode error or mismatch.

## Recipe deviations from the charter (each measured, not chosen)

- `--min-partition-size=8` -> `16`: at 8, 24/24 attempts stop at sub-16 AB refusals
  ("a HORZ_A/HORZ_B/VERT_A partition below 16x16", "a coded (non-skip) HORZ_B/VERT_B rect
  strip below 16x16") — other lanes' surfaces.
- `--enable-tx-size-search=1` -> `0`: with it on, 24/24 attempts stop at the SQUARE
  intra-in-inter arm's own refusal ("an intra block in an inter frame whose tx_depth splits
  its luma transform (round 1)", `read_block_tx_size`). Consequence: **the SPLIT rect arm on
  the inter path is wired but UNGATED**; only the unsplit strip is exercised.
- Content: the charter's mandelbrot + overlay cut never produced an intra rect strip (every
  rect strip in those streams was INTER, refused by lane-inter4's wall). The fixture that
  works is a FAST-ZOOM mandelbrot (`end_scale=0.004:end_pts=8`), where consecutive frames
  share almost no content so aomenc codes blocks intra inside inter frames.
- `--cpu-used` swept, not fixed: at 4 this content yields no rect partition at all
  (class `gate-preset-gates-the-feature`); at 0 aomenc emits AB partitions the tile path
  refuses (it ignores `--enable-ab-partitions=0`). Only cpu-used 1..3 both decode and produce
  rect strips.

## Gate result — RED

Command:
`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-intrarect EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j3`

EVIDENCE: $HOME/.cache/intrarect-suite.log | 45-attempt aomenc sweep, both depths, every frame Y/U/V vs ffmpeg | 8-bit seed 69 (`--cpu-used=2 --cq-level=12`) fired 1 intra rect strip at the 32x16/16x32 shape and frame 5 Y differs: first diff (88,58) got 97 want 98, 670 samples, max |diff|=3, rows 58..95. 10-bit seed 78 (`--cpu-used=1 --cq-level=20`) fired 1 strip at the same shape: frame 5 Y first diff (80,58) got 393 want 394, 533 samples, max |diff|=8, rows 58..95.

Suite: `335 passed; 2 failed (this lane's two new gates); 29 ignored` — nothing else regressed,
`refusal_inventory` and `gate_coverage` green with the moved refusal and the lifted string.

Shape coverage over the sweep: 32x16/16x32 fired (1 stream per depth); **16x8/8x16 never fired**
(sub-16 partitions on the inter path refuse earlier) and **64x32/32x64 is unreachable on this
branch at all** — the inter tile path refuses any SB-level partition other than NONE/SPLIT, so
a superblock-level intra strip cannot be produced. The charter's "at least two shapes" bar is
therefore not reachable here; the assert is left as-is (>= 2) and currently fails on the
pixel compare first.

## Residue

- fix-now (r2): the pixel defect above. Frames 0..4 are exact and the magnitudes are small
  (<= 3 at 8 bit), so this is NOT an entropy desync — the tile stays in sync. The diff band
  starts at row 58 and runs to the frame bottom, i.e. the strip's own bottom deblock band
  (edge at y=64 filters rows 57..70) plus everything predicted below it. Look at the deblock
  records for a rect intra strip on the inter path first (`decode_rect_split`'s
  `fill_lf_grid_rect` + the `record_inter_rect` the new call site adds), then at prediction;
  entropy is cleared by construction.
- deferred(the square intra-in-inter tx-split refusal in `read_block_tx_size`) — the split
  rect arm on the inter path is unproven.
- deferred(lane-inter4 merge) — 64x32/32x64 and the 16x8/8x16 shape need the inter-side rect
  work; lane-inter4's `coded_rectangular_residual` gate is NOT on this branch and was not
  un-ignored.
- accepted — 1:4 strips refuse by name on this path.
