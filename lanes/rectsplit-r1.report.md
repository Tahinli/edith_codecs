# lane-rectsplit r1 — HORZ/VERT strips with a split transform, and filter intra on a strip

Branch `lane-rectsplit` off main `3808cf8`; continues the orchestrator's WIP snapshot
`c5de1b6` (previous builder's uncommitted state), commit `16baa0f` is this round.

## What changed

- `crates/ec-av1/src/decode.rs:3159` `decode_rect_split` (from the WIP snapshot, kept):
  a HORZ/VERT strip with `tx_depth != 0` predicts and reconstructs PER TRANSFORM UNIT in
  raster order (spec 5.11.36), each unit taking its edges from the units already
  reconstructed inside the same strip, with per-unit `has_top_right`/`has_bottom_left`
  (libaom `reconintra.c`) instead of the standalone-block table; chroma stays one un-split
  rect transform. `sub_tx_size_map[TX_32X16] == TX_16X16`, so every unit is square and reads
  through the ordinary square `read_plane`.
- `crates/ec-av1/src/decode.rs:3453` — refusal "filter intra on a HORZ/VERT strip (this
  decoder predicts square-only)" REMOVED from `decode_block_rect`, replaced by the
  `FILTER_INTRA_RECT_HITS` counter.
- `crates/ec-av1/src/intra.rs:733` `predict_filter_intra` now takes `bw`x`bh` instead of one
  square `side` (`av1_filter_intra_predictor_c` walks its 4x2 patches over a rectangle just
  as happily; `av1_filter_intra_allowed_bsize` genuinely allows every strip with both sides
  <= 32). Two call sites updated (`reconstruct_rect` passes `bw, bh`, `reconstruct` passes
  `side, side`).
- `crates/ec-av1/src/decode.rs:3833` — the below-16x16 leaf arm (`decode_leaf_rect`) keeps
  BOTH refusals; its split-transform one is renamed to name the size ("a HORZ/VERT intra
  strip below 16x16 with a split transform ...") because the 32x32-level string it shared is
  now lifted, and it is added to `refusal_inventory.rs:41`.
- `crates/ec-av1/src/stream.rs:9337` gate (a), `:9503` gate (c), `:9700` gate (b, `#[ignore]`d
  RED, from the WIP snapshot).

## Refusals lifted (each with its gate)

| refusal | gate | result |
| --- | --- | --- |
| "a HORZ/VERT intra strip with a split transform (per-unit rect prediction is not ported)" | `a_real_aomenc_stream_with_a_split_transform_horz_vert_strip_decodes_pixel_exact` | GREEN, 20/20 pixel-exact, `rect_split_tx_hits` > 0 |
| "filter intra on a HORZ/VERT strip (this decoder predicts square-only)" (at the 32x32 level only) | `a_real_aomenc_stream_with_filter_intra_on_a_horz_vert_strip_decodes_pixel_exact` | GREEN, 40/40 pixel-exact, `filter_intra_rect_hits`=3 |

Not lifted:
- "a superblock-level HORZ/VERT strip with a split transform (per-unit rect prediction is not
  ported)" — the port itself is wired at that level too, but gate (b)
  `a_real_aomenc_stream_with_a_split_transform_superblock_strip_decodes_pixel_exact` is
  MEASURED RED: seed 50 decodes one sample off, luma (171, 56) ours 147 vs ffmpeg 148, inside
  the 64x32 strip at mi=(8,32) whose transform resolved to depth 2 (TX_16X16). Seeds 42-49
  (depth 1 / TX_32X32 units) are pixel-exact. `#[ignore]`d with that number.
  disposition: deferred(a range-ladder/prefilt-dump bisection of that one TU's top-right edge
  availability) — the refusal stays, so no wrong pixels ship.
- "filter intra on a HORZ/VERT strip" / "a HORZ/VERT intra strip below 16x16 with a split
  transform" at `decode_leaf_rect` (below 16x16): that arm only ports the skip case and has no
  gate of its own. disposition: deferred(a below-16x16 strip gate, lane-sub8's territory).

## Gate-recipe search (the part that cost the round)

`--enable-filter-intra=1` alone never fires filter intra on a strip. Measured, 192x128
`gradients` + `hue=s=0`, `--min/max-partition-size=16/32`, `--threads=1 --row-mt=0`:

| recipe | rect strips | filter-intra blocks (any shape) | filter intra ON a strip |
| --- | --- | --- | --- |
| cq 45, smooth/paeth/directional/angle-delta all OFF (the square filter-intra gate's recipe), 20 seeds | 0 | 23 | 0 |
| cq 45, every intra mode ON, 100 seeds | 18 | 136 | 0 |
| cq 25, every intra mode ON, 100 seeds | 130 | 61 | 5 |
| cq 25, every intra mode ON, 40 seeds (PINNED) | 56 | 16 | 3 |

Disabling the directional/smooth/paeth competitors — the trick the square filter-intra gate
uses — makes aomenc stop choosing rect partitions altogether (class `gate-recipe-confound`).
The knob that actually works is the quantiser: at cq 25 strips are ~7x more common. `smptebars`
and `testsrc2` sources were tried and are unusable here (every attempt refused, 0 decodes).
`EC_RECTSPLIT_CQ` / `EC_RECTSPLIT_OFF` / `EC_RECTSPLIT_GATE_ATTEMPTS` stay as diagnosis knobs
with the pinned recipe as default.

## Evidence

EVIDENCE: `cargo test -p ec-av1 --lib -- a_real_aomenc_stream_with_a_split_transform_horz_vert_strip_decodes_pixel_exact` | aomenc `--enable-rect-partitions=1`, tx-size search ON, 20 seeds, each decoded by ours and by ffmpeg | 20 pixel-exact matches, 0 refusals, `rect_split_tx_hits` delta > 0, test ok
EVIDENCE: `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -- a_real_aomenc_stream_with_filter_intra_on_a_horz_vert_strip_decodes_pixel_exact` | aomenc `--enable-filter-intra=1 --cq-level=25`, 40 seeds | 40 pixel-exact matches, 0 refusals, `filter_intra_rect_hits` delta=3, test ok

## Film check (Hunger Games)

```
ffmpeg -v error -t 0.4 -i ".../The.Hunger.Games...UH.mkv" -c:v copy -f obu hg.obu   # 1817 bytes
cargo run -p ec-av1 --example decode_probe -- hg.obu
REFUSED: unsupported: AV1 tile (a partition below 8x8 (this decoder codes no leaf smaller than 8x8))
```

EVIDENCE: /tmp/.../scratchpad/hg.obu | `ffmpeg -t 0.4 -c:v copy -f obu` then `decode_probe` on this branch | stops at "a partition below 8x8", NOT at any refusal this lane owns

The charter's premise that this extract stops at a rect-split refusal is stale for the film's
first 0.4s: the blocker there is the below-8x8 partition refusal (lane-sub8's, still open per
`memory/`). "Before" was not re-measured on main — the refusal reached is upstream of every
line this lane touched, so the branch cannot have moved it.
disposition: deferred(lane-sub8's below-8x8 leaf) — nothing this lane can do about it.
