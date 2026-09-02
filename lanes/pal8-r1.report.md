# lane-pal8 r1 -- palette on a square 8x8 leaf

Built on `lane-kf900` 5a28354 (merged first, commit `merge lane-kf900`).

## What changed

- `crates/ec-av1/src/decode.rs` `decode_leaf8` (~10680): the square 8x8 leaf now
  passes its OWN palette-Y mode ctx + colour cache
  (`neighbours.palette_ctx_and_cache_mi(leaf_mi)` / `palette_uv_cache_mi`) into
  `read_intra_mode` instead of `None`/`&[]`, and keeps the returned
  `PaletteY`/`PaletteUv` instead of dropping them. Mirrors
  `read_palette_mode_info` (decodemv.c:567; `av1_get_palette_mode_ctx`,
  `av1_get_palette_cache`, `read_palette_colors_y/uv` at decodemv.c:478) and
  `av1_decode_palette_tokens` / `av1_visit_palette` (decodeframe.c:1135), whose
  colour-index map is read after the whole mode-info read -- both already
  implemented in `read_intra_mode`; only this call site was excluded.
  `palette_bsize_ctx(8)` is `num_pels_log2 - 6 = 0`, chroma map is the 4x4
  (`ss_size_lookup`) one `read_intra_mode` already decodes at `side / 2`.
- Same fn: reconstruction wired through the existing `PALETTE_PRED` slot --
  whole-block 8x8 buffer for the single-TX8 and skip arms, a per-4x4-TU window
  slice under a `tx_depth`-split leaf (the same slicing `decode_block` does at
  16x16 and up); a palette skip leaf takes the single 8x8 reconstruct, never the
  per-TU one (`decode_block`'s own `split_tx_skip` rule).
- Same fn: the two `record_palette_y_rect(leaf_mi, 8, 8, 0, [0; 8])` tail sites
  now stamp the leaf's REAL palette size/colours into the per-mi neighbour bands
  (kf900's bands), so the next block's ctx and colour cache see it.
- New counters `PALETTE_LEAF8_HITS` (+ getter/reset) for the gate.
- `crates/ec-av1/src/stream.rs`: new gate
  `a_real_aomenc_palette_stream_with_8x8_leaves_decodes_pixel_exact`.

## Refusals

- LIFTED (for the square 8x8 leaf): `"a block that actually uses a palette (Y)
  -- reconstruction is out of scope"` and its UV twin no longer reachable from
  `decode_leaf8`. The two strings STAY in `refusal_inventory.rs`: they are still
  live on the inter-frame intra paths (decode.rs:19783/19788, 22479/22484),
  another lane's scope. No inventory line was removable this round.
- NOT lifted, MEASURED: `"a palette block with a real transform on a
  superblock-level HORZ/VERT strip"`. I removed it and re-ran the gate: the arm
  `testsrc2-10bit-cq55-txs0-tc1` decodes a whole-frame MISMATCH vs ffmpeg, so
  the refusal is real, not stale. Restored, with the measurement recorded in the
  code comment, and the gate now HARD-asserts that some arm still reaches it
  (`sb_strip_refusals > 0`) so the residue cannot silently disappear.
  Disposition: deferred(root-cause of the 64-level-strip palette mismatch --
  bisect the msac range of that stream's first SB-strip palette block, and check
  the class `refusal-hides-a-defect`: the mismatch may be an unrelated defect
  that the refusal was masking).

## Gate

`cargo test -p ec-av1 --lib a_real_aomenc_palette_stream_with_8x8_leaves -- --nocapture`
(`EC_AV1_REQUIRE_AOMENC=1`, oracle `~/.cache/aom-oracle/build/aomenc`).
48 arms: {testsrc2, smptebars} x {8, 10} bit x cq {30,40,55} x
`--enable-tx-size-search` {0,1} x `--tile-columns` {0,1}, 128x128,
`--tune-content=screen --enable-palette=1 --min-partition-size=8 --limit=1`
(key frame; the inter frames of this recipe stop on OTHER lanes' open refusals:
non-DC chroma on an 8x8 inter leaf, inter 16x16 1:4 partitions). Every decoded
frame compared against ffmpeg, `out_of_scope_mismatch == 0` asserted.

EVIDENCE: ~/.cache/pal8-tmp/gate.log (pre-lift run) + the run below | aomenc 48 arms, ec-av1 decode, ffmpeg compare per frame | 44 8x8-leaf palette block(s) across 24 arm(s), 47 arms compared (47 frames), 23 out of scope (0 mismatched), 1 refused (1 superblock-strip palette) -- test result: ok
EVIDENCE: gate rerun with the SB-strip refusal REMOVED | same 48 arms | `testsrc2-10bit-cq55-txs0-tc1 MISMATCH at frame 0`, test FAILED -- which is why that refusal stayed

Before this change the same 8-bit recipe refused with "a block that actually
uses a palette (Y)": measured with `~/.cache/pal8-tmp/enc.sh` + `decode_probe`
on 12 streams before the fix.

## Suite

`$HOME/.cache/pal8-suite-r1.log` -- see the tail of this file / the lane report line.
