# lane-corner — the whole-64 root at a superblock the frame edge cuts

## Verdict on the chartered defect: STALE PREMISE, not reproducible

The debt says a whole-64 root on a doubly-cut superblock desyncs chroma three
ways at 248x152. At `main` f6c67770 it does not, for two separate reasons:

1. **248x152 cannot carry the block at all.** Its bottom-right superblock has
   24 px of 64 inside vertically, so spec `decode_partition`'s `has_rows`
   (`MiRow + halfBlock4x4 < MiRows`: 32+8=40 vs MiRows 38) is FALSE and
   `PARTITION_NONE` is not even a legal symbol there — the writer's
   `Superblock::Whole` arm refuses it (`tile.rs:2919`). The chartered repro
   (`a_sweep_of_doubly_straddling_sizes_round_trips_through_ffmpeg` with the
   `aligned` guard removed) is GREEN because no edge root ever fires in it:
   its sizes top out at height 152, and the key root's own
   `x64 + SUPERBLOCK <= true_width` bound kept it to wholly-inside
   superblocks anyway (class `gate-blind-to-feature`).
2. **Where the block IS legal, all three sides agree.** Sizes whose last
   superblock keeps more than half of each axis inside (248x168, 232x168,
   248x184 — cut on BOTH axes — and 200x120) round-trip byte-exact in Y, U
   and V across the encoder's own reconstruction, our decoder and ffmpeg,
   over a 4-frame GOP.

## What shipped

* `encode.rs` `encode_key_frame_inner`: both guards lifted. The root's
  legality is now the spec's — `whole_inside || edge_inside`, where
  `edge_inside` is `has_half` on both axes — the same shape the inter root
  has had since lane-b64b. The `EC_AV1_B64EDGE` switch is now readable
  without the inter root's preset flag (`edge_root_enabled`), so `=0`
  restores the old behaviour on both roots.
* `I64_HITS` grew a third slot: whole roots that sat on a cut superblock.
  `take_i64_root_hits` returns `[whole, offered, edge]`, and the native gate
  prints the edge share (class `gate-blind-to-feature`).
* `stream::tests::a_doubly_cut_superblock_root_round_trips_in_every_plane`:
  the three-way, three-PLANE pin at four straddling sizes, asserting the edge
  fire count first. Its sibling
  `encoder.rs::a_non_superblock_aligned_clip_codes_edge_64x64_roots_and_decodes_sample_exact`
  compares LUMA only (class `metric-blind-to-a-plane`), which is why the
  chroma claim in the debt was never actually gated either way.

## Sweep

`aligned`-style whole-frame refusals: `grep -rn "SB_MI == 0" crates/ec-av1/src`
returns no other site — this was the only frame-alignment guard. The encoder
writes 64x64 superblocks only, so there is no whole-128 chroma path to sweep.

## Measurements

(filled in below as the arms land)
