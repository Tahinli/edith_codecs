# lane-sub8 report (round 5)

## Root cause of the 4-pixel chroma residue

Recon vs filtering was separated FIRST (charter step 1): the gate's own aomenc
recipe carries `--enable-cdef=0 --loopfilter-control=0`, and our
`EC_AV1_PREFILT_DUMP` / `EC_AV1_POSTDEBLOCK_DUMP` dumps are byte-identical to
the final planes -- so every filter stage is inert on this stream and the
residue was pure reconstruction. (r4's `edge_params` experiment could not have
moved it: there is no deblocking in this stream at all.)

The mismatching block is NOT a sub-8x8 group: `EC_AV1_TRACE` shows
`partition_w8 mi=(10,8) value=0` -- a plain `PARTITION_NONE` 8x8 leaf with
`tx_depth=1` (four 4x4 luma TUs), luma mode H_PRED, `uv_mode=6` (D157_PRED),
chroma 4x4 at (16,20). Subtracting the (bit-exact) residual from both recons
isolates it to the *prediction*: ours and libaom's differ only at row 0,
columns 2-3.

**`get_intra_edge_filter_type` (reconintra.c:974) reads the CHROMA neighbours'
`uv_mode` for planes 1/2**, never the luma mode. `decode_leaf8` and
`decode_leaf_split4` passed a hardcoded `false` on every chroma
`reconstruct`/`read_plane`, so a directional chroma block whose chroma
neighbour is smooth filtered its above edge at `intra_edge_filter_strength`
type 0 (kernel `{0,4,8,4,0}`, strength 1 at d=67) instead of type 1
(`{0,5,6,5,0}`, strength 2) -- deltas of 1..3 on the two pixels the kernel
difference reaches.

Second half of the cause: the coarse `SUB`-grid `above_uv_mode`/`left_uv_mode`
slots cannot name that neighbour. Adding the leaf's own write to them makes
the top-left 8x8 leaf of a 16x16 clobber the row slot that the block to the
LEFT owns, and the block at mi(10,8) then still read `false` (measured: the
frame stayed 4 pixels off). So this round adds the `uv_mode` twin of the
existing mi-granular `sub8_mode_col`/`sub8_mode_row` machinery.

## What changed
- `crates/ec-av1/src/decode.rs:1784` -- new `uv_mode_col`/`uv_mode_row` mi-granular
  maps on `Neighbours` (+ init).
- `crates/ec-av1/src/decode.rs:2548` `record_uv_mode_mi`, `:2573`
  `smooth_uv_neighbour` (mi-exact above/left `uv_mode`, coarse `SUB` slot as
  fallback), written from `record_rect` (:2606), `record_split_luma` (:2718),
  `decode_leaf8` (:5362) and `decode_leaf_split4` (:5845).
- `crates/ec-av1/src/decode.rs:5331,5758` -- `smooth_neighbor_uv` replaces the
  hardcoded `false` at all 10 chroma reconstruct/read_plane arguments in those
  two functions.
- `crates/ec-av1/src/refusal_inventory.rs:31` -- deleted the stale capability
  claim `"a partition below 16x16 other than a clean split ..."`; r4's fixes
  removed that string from the decode path, which is exactly what made
  `the_decode_path_refuses_exactly_the_listed_cases` RED (charter step 2).
  It was the only entry the test named.

## Gate
`a_real_aomenc_stream_with_a_sub8_split_decodes_pixel_exact` -- **GREEN**
(4 firing + pixel-exact runs; `sub8_split_hits` hard-asserted, no SKIP).

```
EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib sub8
  test stream::tests::a_real_aomenc_stream_with_a_sub8_split_decodes_pixel_exact ... ok
```

EVIDENCE: <scratchpad>/sub8r5/{mine.yuv,ref.yuv} | seed=100 cq=6 gate recipe re-run standalone (sha256 64f6f74e... 3994 B), decoded by `examples/decode_probe` and by ffmpeg | 6144/6144 bytes equal (was 4 chroma bytes off, |delta| <= 3)
EVIDENCE: <scratchpad>/sub8r5/prefilt+postdeblock dumps | EC_AV1_PREFILT_DUMP / EC_AV1_POSTDEBLOCK_DUMP on the same stream | both byte-identical to the final planes -> filters inert, residue is reconstruction
EVIDENCE: <scratchpad>/sub8r5/hg.obu | `ffmpeg -t 0.4 -c:v copy -f obu` from the Hunger Games film, `decode_probe` after the fix | REFUSED at "a HORZ/VERT partition below 8x8" (was: "a partition below 8x8")

## Refusals
Nothing lifted, nothing to lift: the PARTITION_SPLIT-to-4x4 shape the charter
names has had **no refusal string** since `decode_leaf_split4` landed (r2) --
the gate is what was missing, and it is now green, so the capability is
proven rather than assumed. The two below-8x8 strings that remain are both
out of this lane by the standing constraint:
- `"a HORZ/VERT partition below 8x8 (... 4x8/8x4 need a real rectangular
  transform)"` -- deferred(a real TX_4X8/TX_8X4 primitive). This is now the
  Hunger Games blocker.
- `"an inter partition below 8x8 ..."` -- deferred(an inter lane; lane-sub8 is
  intra-scoped).

## Suite
`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib`: **269 passed, 0 failed, 22
ignored**, 476 s (r4 was 267 passed, 2 failed -- both now green).

## Open / disposition
- deferred(an inter-frame gate whose chroma neighbour codes a smooth `uv_mode`)
  -- same-shape sweep: `decode.rs:12084,12096` (`decode_inter_block8`'s
  intra-in-inter chroma reconstruct) still pass a hardcoded `false` for the
  chroma edge-filter type. Every other chroma site now takes a `uv_mode`-derived
  flag; `decode_block`/`decode_inter_block` (:3251,:3765,:4893,:10929) take it
  from the coarse `SUB` slots, which is exact for >=16x16 neighbours and
  approximate when the neighbour is an 8x8 leaf -- upgrade path is to route
  those four through `smooth_uv_neighbour` too.
- accepted: `smooth_uv_neighbour` falls back to the coarse slot when no block
  recorded the exact neighbouring mi (documented at the field).
- Film: Hunger Games now stops at the HORZ/VERT-below-8x8 refusal above; the
  film is also yuv420p10le, so a 10-bit refusal waits behind it.
