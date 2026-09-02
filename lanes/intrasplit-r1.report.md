# lane-intrasplit r1 — RED (refusal lifted, two root causes fixed, gate never fired)

Base: 18bf7dc. Branch: lane-intrasplit.

## What changed
- `crates/ec-av1/src/decode.rs:6225` — the refusal "a split (nonzero tx_depth) transform on
  an intra HORZ/VERT strip in an inter frame" is gone; a nonzero depth now walks the key
  frame's own per-TU path (`depth_to_tx_wh` + `decode_rect_split`, already wired) and bumps
  the new per-depth counter.
- `crates/ec-av1/src/decode.rs` (`tx_size_context_txfm_rect`, new, above
  `tx_size_context_txfm`) — ROOT CAUSE 1: the arm read its `tx_depth` symbol through
  `tx_size_context_rect`, the KEY FRAME's deblock-grid approximation. libaom
  `get_tx_size_context` (pred_common.h:342, called from `read_selected_tx_size`,
  decodemv.c) reads the two `TXFM_CONTEXT` bands and, for an INTER neighbour, that
  neighbour's BLOCK width/height instead. Class [[cdf-row-held-constant]]: wrong CDF row,
  same value most of the time, different range.
- `crates/ec-av1/src/decode.rs` (tail of `decode_intra_rect_in_inter`) — ROOT CAUSE 2:
  the strip never wrote its own `TXFM_CONTEXT` bands (`set_txfm_ctxs`,
  decodeframe.c end of `decode_token_recon_block`), because the call site
  (decode.rs:18210) returns early. Every later block in the frame read a stale
  `tx_size`/`txfm_split` context across the strip's footprint.
- `crates/ec-av1/src/refusal_inventory.rs` — inventory line dropped.
- `crates/ec-av1/src/stream.rs` — new gate
  `a_real_aomenc_{,10bit_}inter_sequence_with_a_split_transform_intra_strip_decodes_pixel_exact`
  (192x128, 8 frames, real aomenc, `--enable-rect-partitions=1 --enable-tx-size-search=1`,
  `--kf-max-dist/-min-dist=9999 --lag-in-frames=0`, cpu-used and cq (incl. 63) swept,
  each flag spelled once with the rect/tx overrides LAST) hard-asserting
  `decode::intra_rect_in_inter_split_tx_hits()` per depth.

## Gate result: RED (never fired)
`cargo test -p ec-av1 --lib a_real_aomenc_inter_sequence_with_a_split_transform_intra_strip`
40/40 attempts stop at OTHER lanes' named refusals; 0 attempts decoded whole, so the lift is
UNGATED and this branch MUST NOT merge as-is.

EVIDENCE: $HOME/.cache/intrasplit-suite-r1.log | 40-attempt sweep, min/max-partition 32/32,
cpu 0..4, cq 30/20/12/63, zoom-mandelbrot source | refusal histogram:
intra 1:4 strip 9, non-skip inter rect strip 7, inter 16x16 AB/1:4 7, inter SB AB 4,
1:4 inter strip split w/ 64 axis 3, inter sub-8 3, split intra strip w/ 64x32 unit 2,
32x64 unit 2, Golomb tail 2, 8x8 non-DC chroma 1.
Six recipe families were swept (192x128 and 96x96; min/max partition 16/32, 16/64, 32/64,
32/32; hard-cut overlay and fast-zoom sources; sb-size 64): every stream that makes aomenc
pick an intra 2:1 strip with a split transform also carries one of the refusals above.
Same shape as the recorded lane-intra14 r1 and lane-r14 r3 gate hunts.

## Film probes (charter)
EVIDENCE: scratchpad/intrasplit/{hg1200,troy1800}.obu | `ffmpeg -ss 1200/-ss 1800 -t 2 -c:v copy -f obu`
then `cargo run -p ec-av1 --example decode_probe` | new stops:
- Hunger Games -ss 1200: `a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular
  residual coding` (was the split-intra-strip refusal per census4) — 0 frames in
  EC_AV1_FINAL_DUMP.
- Troy -ss 1800: `an intra-coded 1:4 (or other non-2:1) rect strip on the inter block path`
  (lane-intra14's surface) — 0 frames in EC_AV1_FINAL_DUMP. No 128-superblock refusal was
  reached on this segment.

## Suite
`cargo test -p ec-av1 --lib` (systemd unit, MemoryMax=10G) — see
`$HOME/.cache/intrasplit-suite-r1.log`; totals in the handoff line below.
The two new gate tests FAIL by construction (never fired). One further failure,
`a_real_aomenc_stream_with_film_grain_decodes_pixel_exact`, is being triaged against the
base commit (`$HOME/.cache/intrasplit-base-filmgrain.log`).

## Residue
- fix-now (next round): make the gate fire. The only untried levers are a content family
  that yields 32x16/16x32 intra strips with NO 1:4 or AB partition anywhere in the frame,
  or waiting on lane-intra14 (1:4 intra strips) and lane-r14 (inter rect leaves) — both
  would remove the two dominant blockers.
- deferred(lane-r14/lane-intra14): the two 64-axis strip refusals ("split intra strip whose
  transform unit is 64x32/32x64") are this arm's neighbours but need rectangular 64-axis
  luma coefficient tables.
