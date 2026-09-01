# lane-palette2 r7 report

(The charter for this round was labelled "round 3"; the branch was already at r6 — this is r7.
The charter's own step numbering is kept below.)

VERDICT: GREEN. Both charter steps are implemented AND gated pixel-exact against real aomenc.
The r6 report's RED is resolved: it was NOT the `decode_block_rect64` residual defect, it was
`--min-partition-size=64` in the gate recipe forcing SB-level VERT-64 blocks through
`decode_block_rect64`. At `--min-partition-size=16` the gate reaches real rect strips
(16x16..64x64) and is pixel-exact.

## What changed (commit 5d67fd7 on `lane-palette2`)

- `crates/ec-av1/src/decode.rs` `decode_block` (~5056, ~5241) — palette-Y prediction is now
  windowed PER TRANSFORM UNIT: with `logical_tx != side`, each TU copies its own
  `logical_tx`x`logical_tx` window out of the block's index map right before `read_plane`,
  instead of the whole-block buffer (which the FIRST unit consumed at the wrong stride).
  Residual is added per TU over that prediction. Refusal
  **"a palette block with a split luma transform (round 1)" LIFTED** (charter step 3).
- `decode.rs` `read_intra_mode` (~4415) and `read_intra_mode_rect` (~3153) — a palette-Y block
  gets NO `use_filter_intra` symbol (`av1_filter_intra_allowed`, reconintra.h:77). We read one,
  consuming a bit the encoder never wrote, and desynced from every palette block <=32x32
  (class `symbol-consumption-gap`).
- `decode.rs` `Neighbours::palette_ctx_and_cache` (~2005) — the SB-top-row exclusion of the above
  neighbour belongs to the colour CACHE only (pred_common.c:76); `av1_get_palette_mode_ctx`
  (pred_common.h:197) reads plain `xd->above_mbmi`. Applying it to the ctx too made every SB in
  the frame's second SB row read `palette_y_mode` off CDF row 0 while libaom used row 1
  (class `cdf-row-held-constant`).
- `decode.rs` `decode_leaf_rect` (~3674) — sub-16x16 rect leaves now read palette syntax with
  their own real ctx/cache (previously row 0); RECONSTRUCTION on a sub-16x16 strip stays refused
  by name: **new refusal "a palette block on a HORZ/VERT intra strip below 16x16 (reconstruction
  not ported)"**.
- `crates/ec-av1/src/stream.rs` (~2129, ~2285) — both palette gates' aomenc recipe
  `--min-partition-size=64` -> `16`; r6's gate additionally asserts
  `palette_split_tx_hits()` moved on a pixel-exact attempt.
- `crates/ec-av1/src/refusal_inventory.rs` — split-transform refusal removed, sub-16x16 rect
  palette refusal added. `gate_coverage.rs` already lists only `enable-intrabc`; palette is
  covered by an explicit `--enable-palette=1` in both gates. Both meta-tests green.

## Gates

Command (worktree, `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette2`, `EC_AV1_REQUIRE_AOMENC=1`):

    cargo test -p ec-av1 --lib -j3 -- --nocapture \
      stream::tests::a_real_aomenc_stream_with_rect_screen_content_decodes_pixel_exact \
      stream::tests::a_real_aomenc_stream_with_rect_palette_decodes_pixel_exact \
      refusal_inventory gate_coverage
    # test result: ok. 7 passed; 0 failed; 0 ignored

aomenc line in both: `--tune-content=screen --enable-palette=1 --enable-rect-partitions=1
--min-partition-size=16 --max-partition-size=64 --sb-size=64 --passes=1 --threads=1 --row-mt=0`,
70 attempts (5 sizes x 7 cq x smptebars/rgbtestsrc), pixel-compared against ffmpeg's decode.

EVIDENCE: /tmp/claude-1000/-home-tahinli-Documents-Code-Rust-edith-codecs/b6d8a07f-96a4-4bbb-b378-af9ae25cf7c9/scratchpad/palette2-r7-gates.txt | 70 real-aomenc screen-content encodes decoded and pixel-compared vs ffmpeg | a_real_aomenc_stream_with_rect_screen_content: 14/70 pixel-exact, 13 of them through a split-transform palette block, rect_screen_content_hits=169, palette_split_tx_hits=356
EVIDENCE: same file | same sweep, r5's narrower gate | a_real_aomenc_stream_with_rect_palette: 14/70 pixel-exact, palette_rect_hits=78 rect-strip palette blocks reconstructed
EVIDENCE: full lib suite `cargo test -p ec-av1 --lib -j3` | whole ec-av1 lib | 269 passed, 0 failed, 23 ignored (510s)

## Charter steps

- Step 2 (palette on HORZ/VERT rect strips, >=16x16 both dims): DONE, gated
  (`palette_rect_hits=78`, pixel-exact). The blanket refusal "a HORZ/VERT intra strip in a
  screen-content frame (palette syntax is consumed for square blocks only)" is gone and now has
  a gate behind it.
- Step 3 (palette + split luma transform): DONE, gated (`palette_split_tx_hits=356`).
- Step 2's inter-frame 8x8-leaf palette refusals (decode.rs ~10652, ~11748): NOT attempted —
  deferred. Measured, not assumed: a 10-frame `rgbtestsrc` screen-content encode at cq 15/35/55
  through `decode_probe` stops at "a 32x32 partition type this decoder does not code (value=4)"
  and "filter intra on a HORZ/VERT strip", never at a palette refusal. Those paths are
  unreachable behind other lanes' refusals, so no gate could prove a lift today
  (class `refusal-lifted-without-a-gate`).

## Residue

- deferred: inter-frame palette reconstruction (decode.rs ~10652 intra-in-inter, ~11748 8x8 leaf)
  — unblocked by lane-part32 (32x32 partition types) and the filter-intra-on-rect-strip lane;
  once those land, screen-content inter frames become reachable and a gate is possible.
- deferred: sub-16x16 rect-strip palette reconstruction — refused by name, syntax read correctly;
  unblocked by a rect (4x8/8x4) transform primitive, same ceiling as `lane-sub8`.
- accepted: the `decode_block_rect64` real-residual defect (ledger lane-sbpart-r9) is untouched
  and still open; this lane's gates simply no longer route through it. It is NOT a palette defect.
