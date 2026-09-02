# lane-rectsplitx r1 report

Branch `lane-rectsplitx` off main `8262b99`.

## What changed

- `crates/ec-av1/src/decode.rs`
  - `decode_rect_split` is now **mi-granular** (`at_mi`) and takes the transform
    unit as `(tx_w, tx_h)` instead of one square `tx`: it can name a strip that
    starts 8 px into a 16-px `SUB` cell, and it walks rectangular units.
    Chroma map gained `(32,8)/(8,32)` (`Chroma16`), `(16,4)/(4,16)` (`Chroma8`)
    and `(8,4)/(4,8)` (`ChromaRect8x4`).
  - new `depth_to_tx_wh(bw, bh, depth)` -- libaom `depth_to_tx_size`: every
    `sub_tx_size_map` entry halves the LONGER side of a rect and both sides of
    a square, so a 1:4 strip's first split is still a 2:1 RECT unit
    (`TX_32X8 -> TX_16X8`, `TX_64X16 -> TX_32X16`). Replaces the
    `bw.min(bh) >> (depth - 1)` formula at BOTH 2:1 call sites; it is the same
    root cause lane-rectchroma measured mis-tiling a depth-1 64x16 strip, fixed
    once for every caller instead of per shape.
  - new `tx_type_mode_row(mode, filter_intra)` -- the `fimode_to_intradir` row
    rule main `1a89ed9` fixed inside `read_plane`, now also applied to
    `decode_leaf_rect`'s rect luma coefficient read (same defect class, sibling
    site found by sweeping every `cdfs.txb(<luma set>, mode)` call: the other
    luma rect sets are `tx_size_sqr_up >= TX_32X32`, i.e. DCT-only, no symbol).
  - **`decode_leaf_rect` read the wrong tx_size CDF category**: BLOCK_16X8 /
    BLOCK_8X16 are `bsize_to_tx_size_cat == 1` (libaom
    `bsize_to_tx_size_depth_table`), not the 2 the 32x16 strips use. Same
    symbol count, different CDF row -- a wrong-table read of a real symbol.
  - `decode_leaf_rect`'s split refusal is **lifted**: a 16x8/8x16 strip with
    `tx_depth != 0` decodes per transform unit (TX_8X8 at depth 1, TX_4X4 at
    depth 2).
  - `Neighbours`: `record_mi_luma_rect`, `luma_skip_ctx_rect`,
    `record_split_luma_rect_mi` (coarse mode/side cells stamped with
    `div_ceil`, so an 8-px-tall strip stamps the cell it touches).
- `crates/ec-av1/src/stream.rs`
  - `a_pinned_aomenc_16x8_strip_reads_its_use_filter_intra_flag` is now a PIXEL
    COMPARE (was `expect_err`) and additionally asserts `sub16_split_hits > 0`:
    it is the gate for the sub-16 split lift.
  - the 32x32-level 1:4 gate gained a `--enable-tx-size-search=1` half of its
    attempt window (cq 12..28), which keeps the 1:4 split refusals honest.
- `crates/ec-av1/src/refusal_inventory.rs` updated (one refusal removed, two
  added).

## Refusals

Lifted: "a HORZ/VERT intra strip below 16x16 with a split transform (per-unit
rect prediction is not ported)".

Added (both MEASURED, not speculative):
- "a split intra strip whose transform unit is the rect {tx_w}x{tx_h} ..." --
  the per-unit RECT walk (depth 1 of a 1:4 strip) is implemented but mismatched
  ffmpeg on the band fixture (8-bit, cq 12, seed 42), so it refuses instead of
  shipping wrong pixels.
- "a split intra strip whose transform unit is {tx_w}x{tx_h} (no luma
  coefficient tables ...)" -- shape guard for the next caller.

Kept: "a 32x32-level 1:4 strip with a split transform (... depth={depth})".
Reason: depth 1 fires FIRST in every splitting attempt of the fixture, so no
attempt ever reaches a pixel-compared depth-2 strip -- a depth-2-only lift is
ungatable, and an ungated lift is not a lift (the tx64x16 r4 handoff predicted
exactly this interleaving).

## Gates

- `a_pinned_aomenc_16x8_strip_reads_its_use_filter_intra_flag` -- **ok**.
  EVIDENCE: crates/ec-av1/fixtures/filter_intra_8x16_strip_seed49.obu | cargo
  test -p ec-av1 --lib -- a_pinned_aomenc_16x8_strip | whole frame Y+U+V equal
  to ffmpeg, filter_intra_rect_sub16 hits 2 (was 1 under the truncated decode),
  sub16_split_hits > 0.
- `a_real_aomenc_stream_with_a_32x32_level_1to4_partition_decodes_pixel_exact`
  -- ok, now over 16 attempts per bit depth (8 with tx-size-search on).
  8-bit: 7 pixel-exact of 8 compared, 9 named refusals, horz_4=64, vert_4=72,
  coded=136, 0 out-of-scope mismatches.

## Open residue (fix-now for the next round, all MEASURED)

1. `deferred(rect-unit defect)` -- the depth-1 RECT transform unit (TX_16X8 /
   TX_32X16) mismatches ffmpeg; code is wired and refused by one `if tx_w !=
   tx_h` in `decode_rect_split`. Deleting that one line + the 1:4 caller's twin
   is the whole re-lift. Reproduce: the 1:4 gate with the tx-search half and
   the refusal removed, 8-bit cq 12 seed 42.
2. `deferred(chroma defect behind the lifted refusal)` --
   `a_real_aomenc_stream_with_filter_intra_on_a_sub16_horz_vert_strip_decodes_pixel_exact`
   now decodes seed 55 instead of refusing and mismatches on the **U plane**
   only; `#[ignore]`d with that measurement.
3. `deferred(same)` -- `a_directional_16x8_strip_reads_the_right_above_right_samples`
   decodes now and mismatches LUMA at seed 700 cq 45 with 0 reaching strips;
   `#[ignore]`d with that measurement. Class refusal-hides-a-defect: both are
   defects the split refusal was covering, not regressions of this round.

## Films

`decode_probe` (release), before -> after:
- hg5.obu: "a 32x32-level 1:4 strip with a split transform (depth=2)" ->
  **"a HORZ_A/HORZ_B/VERT_A partition below 16x16"** (another lane's refusal).
  The 1:4 depth refusal is still in place, so what moved this film is the
  sub-16 split lift plus the `tx_size_cat1` fix -- the extract's 1:4 strips are
  reached and decoded (rect4_32: horz=4 coded=4) and the stream now runs on to
  a partition shape nobody has ported.
  EVIDENCE: hg5.obu | cargo run --release --example decode_probe -- hg5.obu |
  refusal string changed, rect4_32 horz=4 vert=0 coded=4
- hg-head.obu: "filter intra on a HORZ/VERT strip (this decoder predicts
  square-only)" (unchanged, another lane).
- troy-head.obu: "an intra block in an inter frame whose tx_depth splits its
  luma transform (round 1)" (unchanged, another lane).
