# lane-pal8 r1 HANDOFF

Branch `lane-pal8`, tip **7d9902c** (parent `3d5a737` = merge of lane-kf900 5a28354 into main 8810bad).
Everything below is COMMITTED; nothing is in-flight.

## What the patch does
`crates/ec-av1/src/decode.rs` `decode_leaf8` (the square 8x8 intra leaf) now passes its
own per-mi palette ctx + colour cache into `read_intra_mode`
(`palette_ctx_and_cache_mi` / `palette_uv_cache_mi`, kf900's per-mi bands), keeps the
returned `PaletteY`/`PaletteUv`, reconstructs through the existing `PALETTE_PRED` slot
(whole 8x8; a per-4x4-TU window slice under a split `tx_depth`; a palette skip leaf takes
the single 8x8 reconstruct), and stamps the leaf's real palette size/colours into the
neighbour bands. New counter `PALETTE_LEAF8_HITS` + getter/reset.
References: `read_palette_mode_info` decodemv.c:567, `read_palette_colors_y/uv`
decodemv.c:478, `av1_visit_palette` / `av1_decode_palette_tokens` decodeframe.c:1135.

## Refusal targeted / lifted
"a block that actually uses a palette (Y|UV) -- reconstruction is out of scope" is no
longer reachable from `decode_leaf8`. Both strings STAY in `refusal_inventory.rs` -- they
are still live on the inter-frame intra paths (decode.rs 19783/19788, 22479/22484), which
are another lane's scope. No inventory line was removable.

## Localised, measured, NOT lifted
"a palette block with a real transform on a superblock-level HORZ/VERT strip". I removed
it and re-ran the gate: stream `testsrc2` 128x128 10-bit cq55 `--enable-tx-size-search=0
--tile-columns=1 --tune-content=screen --enable-palette=1 --min-partition-size=8
--limit=1` decodes frame 0 with a whole-frame mismatch vs ffmpeg. So the refusal is REAL,
not stale; restored with that measurement in the code comment, and the gate now asserts an
arm still reaches it. Next agent: bisect that stream's first `decode_block_rect64` palette
block with an msac range ladder (the corner-cropped luma path expands `min(bw,32)` x
`min(bh,32)` levels into a `bw`x`bh` grid and reconstructs once, so the prediction shape
already matches the palette buffer -- suspect the residual/scan or class
`refusal-hides-a-defect`).

## Gate
`a_real_aomenc_palette_stream_with_8x8_leaves_decodes_pixel_exact` (stream.rs), 48 arms,
GREEN: 44 8x8-leaf palette blocks across 24 arms, 47 arms / 47 frames pixel-exact,
0 out-of-scope mismatches, 1 named refusal (the SB-strip one).

## Untested / open
- Full suite: 401 passed / 1 failed / 33 ignored. Failure
  `a_frame_edge_straddling_band_decodes_pixel_exact` 68x192 cq35 10-bit tile_cols=1,
  frame 1, 6944 luma pixels. Attribution unverified -- run that single test at 3d5a737
  first (see lanes/pal8-r1.report.md for the exact command).
- Inter-frame palette (the 19783/22479 sites) untouched.
- The gate is key-frame only (`--limit=1`): the inter frames of this recipe stop on other
  lanes' refusals (non-DC chroma on an 8x8 inter leaf; inter 16x16 1:4 partitions).
