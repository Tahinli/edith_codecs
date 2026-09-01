# lane-sqchroma r1 — the chartered square-path chroma defect does not reproduce

## Verdict

The charter's premise is FALSE at HEAD (f31a2c5). A plain SQUARE-partition intra key
frame with real chroma and aomenc's tx-size search left ON decodes **pixel-exact**, in
every recipe I could build around the chartered one, 8-bit and 10-bit. Nothing was
fixed because nothing was broken; what shipped is the missing gate that pins the
capability, and the removal of the stale `hue=s=0` guard the (wrong) premise put into
two live rect gates.

## Reproduction attempt (the chartered recipe, exactly)

`gradients_source(seed, 192x128, duration=0.04:rate=25)` -> `yuv420p` (NO `hue=s=0`) ->
aomenc `--cpu-used=0 --threads=1 --row-mt=0 --sb-size=64 --enable-rect-partitions=0
--enable-ab-partitions=0 --enable-1to4-partitions=0 --min-partition-size=16
--max-partition-size=32 --enable-restoration=0 --enable-palette=0 --deltaq-mode=0
--enable-filter-intra=0 --enable-cfl-intra=0 --enable-intrabc=0` (no
`--enable-tx-size-search=0`), decoded by `examples/decode_probe` and diffed plane by
plane against `ffmpeg -i s.obu -pix_fmt yuv420p -f rawvideo`.

- seeds 42..61 at cq 45: 20/20 exact (Y=0 U=0 V=0 mismatching bytes).
- seeds 42..61 at cq 20: 20/20 exact.
- seed 42 (the charter's named seed) traced: `tx_depth != 0` fires 6 times, `uv_mode`
  takes values 0/1/2/5/11 -- the exact "luma tx_depth>0 with a non-DC uv_mode" pairing
  the defect was attributed to. Seed 55: 10 depth hits, uv_modes 0/2/7/12. Still exact.
- harder content, same square recipe, cq 12 and 32: `testsrc2`, `mandelbrot`,
  `smptebars`, `rgbtestsrc` -- 8/8 exact (streams up to 4 KB, i.e. real chroma residual).
- with `--enable-cfl-intra=1 --enable-filter-intra=1` (the CFL-under-split-luma
  candidate the charter names): testsrc2 + mandelbrot at cq 12/32, 4/4 exact.
- free partition tree (no min/max pin) + CFL: 5/6 exact, 1 named refusal
  (`HORZ_A/HORZ_B/VERT_A below 16x16` -- an unrelated, already-named refusal).

So no divergence to localise: no EC_AV1_PREFILT_DUMP split, no EC_TRACE_COEFF ladder,
no lane-sub8 cherry-pick needed (`decode_block` already computes `smooth_neighbor_uv`
from `above_uv_mode`/`left_uv_mode` at decode.rs:5696 -- the square >=16 path never
passed `false`; only the leaf8/split4 paths did, and lane-sub8 r5 fixed those).

Most likely origin of the premise: it was measured on the pre-merge lane-rectsplit
branch, not on main-with-everything-merged (main f31a2c5 carries lane-tiny, lane-seg,
lane-intrabc and the rectsplit merge itself), and it was recorded as fact in a comment
rather than re-checked. Class: my-charters-shipped-stale-premises.

## What changed

- `crates/ec-av1/src/decode.rs:735` — new `SQ_CHROMA_TX_HITS` counter + `sq_chroma_tx_hits()`;
  bumped at `decode.rs:5638` in `decode_block` when `tx_select && logical_tx < side &&
  uv_mode != DC_PRED` (square block whose luma transform split under a non-DC chroma mode).
- `crates/ec-av1/src/stream.rs:9924` — new gate
  `a_real_aomenc_intra_stream_with_tx_size_search_and_chroma_decodes_pixel_exact`.
- `crates/ec-av1/src/stream.rs` (3 sites) — `-vf hue=s=0` dropped from the two live rect
  gates and the ignored SB-strip gate; the stale "PRE-EXISTING square-path chroma defect"
  comments replaced with what was actually measured. Both live gates stay green with real
  chroma, so those gates now cover chroma too.

## Gate

`a_real_aomenc_intra_stream_with_tx_size_search_and_chroma_decodes_pixel_exact`:
square partitions only, tx-size search at aomenc's default (ON), real chroma, 10 8-bit
attempts (seeds 42..51, cq alternating 20/40) plus one 10-bit stream (`--bit-depth=10`,
sequence-header `bit_depth == 10` asserted before any pixel compare). Hits counted only
over attempts that decoded AND pixel-compared; `firing > 0` hard-asserted; a non-refusal
decode error or any pixel mismatch FAILS, only a missing tool SKIPs.

```
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sqchroma EC_AV1_REQUIRE_AOMENC=1 \
  cargo test -p ec-av1 --lib a_real_aomenc_intra_stream_with_tx_size_search_and_chroma -- --nocapture
```

EVIDENCE: gate stderr | 10 8-bit aomenc streams + 1 10-bit, decoded and diffed vs ffmpeg | `11 pixel-exact decodes (0 named refusals out of 10 8-bit attempts), split-tx-with-chroma blocks: 23 (10-bit stream: 2)` — test ok
EVIDENCE: scratchpad sweep.sh/src2.sh output | 40 gradient streams (seeds 42..61 at cq 45 and cq 20) + 12 testsrc2/mandelbrot/smptebars/rgbtestsrc streams, square-only recipe, decode_probe vs ffmpeg rawvideo | every plane Y=0 U=0 V=0 mismatching bytes
EVIDENCE: rectsplit gates with the hue filter removed | `cargo test -p ec-av1 --lib split_transform_horz_vert filter_intra_on_a_horz_vert_strip` | both ok (16 decoded seeds, split-tx delta 3; 30 matches, filter_intra_rect delta 9)

## Refusals

None lifted (none were in the way -- the path already decodes). `refusal_inventory` and
`gate_coverage` untouched and green.

## Residue

- accepted: the charter's localisation plan (PREFILT dump, chroma tx_type derivation,
  CFL-under-split-luma, chroma edge-filter type) was not executed -- there is no
  divergence to localise. The tx_type/CFL rules were therefore NOT re-audited against
  libaom this round; if a real square chroma defect turns up later it is unexamined ground.
- deferred(rectsplit r2): `a_real_aomenc_stream_with_a_split_transform_superblock_strip_decodes_pixel_exact`
  stays `#[ignore]`d (measured RED on luma, seed 50) -- untouched by this lane beyond its comment.
