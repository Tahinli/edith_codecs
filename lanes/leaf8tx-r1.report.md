# lane-leaf8tx r1 — 8x8 intra leaf inside an inter frame

Branch `lane-leaf8tx`, tip `63ea2a9` (on top of `aacfbdb`, the interrupted
round's partial work committed verbatim first).

## Verdict: GREEN on the angle-delta half, RED (refusal kept) on the tx_depth half

### Lifted: `angle_delta_y != 0` on an 8x8 intra leaf in an inter frame
`decode.rs:21831` — `av1_use_angle_delta(BLOCK_8X8)` is true (`bsize >=
BLOCK_8X8`), so the leaf codes `angle_delta_y` off `angle_delta_cdf[mode -
V_PRED]` exactly as a key-frame block does (libaom `read_intra_angle_info`
inside `read_intra_block_mode_info`, `decodemv.c`). The delta already reached
the directional predictor + edge filter/upsample through the `reconstruct`
call below the refusal; only the refusal stood in front of it. Refusal string
dropped from `refusal_inventory.rs`.

Gate: `stream.rs` `a_real_aomenc_inter_sequence_with_an_angle_delta_8x8_intra_leaf_decodes_pixel_exact`
and its `..._10bit_...` twin (real aomenc, 192x128, 6 frames, mandelbrot zoom
with a hard cut at n>=3 + seeded noise, cq 12..40, `--enable-angle-delta=1
--enable-directional-intra=1 --min-partition-size=8 --max-partition-size=32
--kf-max-dist=9999`, `--enable-tx-size-search=0` so the still-refused sibling
tool is not in the recipe). Per-attempt counter delta
`decode::intra_in_inter8_angle_delta_y_hits()` (new, 8x8-only so a bigger
block cannot satisfy it); a decode error or any pixel mismatch is a failure,
never a SKIP.

EVIDENCE: /home/tahinli/.cache/leaf8tx-final-r1.log | `cargo test -p ec-av1 --lib -- angle_delta_8x8_intra_leaf refusal_inventory gate_coverage` | 14 passed / 0 failed; 8-bit seed 42 cq 12 = 109 nonzero-delta 8x8 intra leaves, 10-bit seed 42 = 75, all 6 decode-order frames Y/U/V exact vs ffmpeg
EVIDENCE: /home/tahinli/.cache/leaf8tx-arm-angle.log | `EC_LEAF8TX_CONTROL=angle` (angle on, tx-size-search off), 30 seeds x 2 bit depths | 1737 nonzero-delta leaves, ZERO pixel mismatches in 60 attempts

### Kept: the 8x8 leaf's `tx_depth` split into four TX_4X4 units
The per-TU implementation is in the tree (`decode.rs`, the `if let Some(leaves)
= intra_leaves` loop: `Reach::of_tu` per unit, raster order, per-TU
`txbset_for(4, reduced_tx_set)` intra 7-type set, per-TU
`neighbours.luma_skip_ctx` txb_skip ctx, `record_mi_luma` per unit, chroma one
4x4, `saved_luma_ctx` restore + `fill_lf_grid` tx 4 in the shared tail), but it
sits behind a re-instated refusal (`decode.rs`, right after
`read_block_tx_size`) because it is NOT green:

- 8-bit, both tools on: 150 split leaves, luma bit-exact on every frame, chroma
  off by +-1 on 8..46 samples (`/home/tahinli/.cache/leaf8tx-gate-r1.log`).
- `EC_LEAF8TX_CONTROL=tx` (tx-size-search on, angle off) mismatches chroma even
  on an attempt where ZERO 8x8 splits fired — i.e. `--enable-tx-size-search=1`
  also lets 16/32 intra splits and inter var-tx through, so the full-recipe
  mismatch is confounded (class [[gate-recipe-confound]]).
- `EC_LEAF8TX_CONTROL=tx8` (adds `--max-partition-size=8`, which removes the
  16/32 confound): the previously-mismatching 10-bit seed 64 becomes exact, and
  10-bit seed 68 cq 19 with 5 split leaves mismatches hard — luma first diff
  frame 5 (160, 96) got 664 want 670, 1140 luma / 281 U / 285 V samples. That
  is OUR split path, and it is the r2 starting point.
- Control (`EC_LEAF8TX_CONTROL=1`, both tools off): 60 attempts, 0 mismatches —
  the recipe itself is clean.

`EC_LEAF8TX_CONTROL` (`stream.rs`, inside the gate's aomenc args) is the
diagnostic knob for these arms; per-arm overrides are appended LAST because
aomenc keeps the last occurrence of a repeated `--enable-*` flag.

## Disposition
- deferred: the 8x8 `tx_depth` split lift — 10-bit `tx8` seed 68 mismatches
  from the split leaf onwards — unblocked by bisecting that one stream
  (`EC_LEAF8TX_CONTROL=tx8`, 10-bit, seed 68, cq 19, frame 5, mi around
  (24, 40)) with the aomdec range ladder; the implementation is already in the
  tree behind the refusal.

## Suite (RED, one failure, caused by this lift)
EVIDENCE: /home/tahinli/.cache/leaf8tx-suite-r1.log | `cargo test -p ec-av1 --lib -j3` under a systemd unit | 393 passed / 1 failed / 33 ignored in 1609 s

Failure: `stream::tests::a_real_aomenc_stream_with_cdef_and_sub16_inter_leaves_decodes_pixel_exact`,
`stream.rs:16955` — "no sub-16x16 inter leaf wrote the CDEF skip band (depth=8 cq=12)".
Not a pixel mismatch: the gate's FIRST attempt (depth 8, cq 12) used to hit the
now-lifted angle-delta refusal and be pushed onto its refusal list, so the sweep
moved to a later attempt that did fire `inter8_skip_band_hits`. With the refusal
gone that first attempt decodes, and its hard per-attempt counter assert fires
before any comparison (class [[counter-from-refused-stream]] /
[[parallel-flake-is-attempt-selection]] — a lifted refusal reshuffles which
attempt a sibling gate lands on).

- fix-now for r2 (owner: whoever merges this lane): that gate must keep sweeping
  when a decoded attempt carries no sub-16 leaf (continue + a single
  end-of-sweep assert that at least one compared attempt fired), instead of
  asserting on the first decoded attempt. Whether that attempt is pixel-exact is
  UNKNOWN — the assert precedes the ffmpeg compare.
