# lane-txselect r1 — inter var-tx (`TxMode::Select`), spec 5.11.17

Branch `lane-txselect`, commit `138c558` (on top of the orchestrator's WIP
snapshot `16d83e4`, itself off main `3808cf8`).

## What changed

- `crates/ec-av1/src/decode.rs`
  - `read_block_tx_size` / `read_var_tx_size` / `txfm_partition_ctx` /
    `txfm_partition_update` / `set_txfm_ctxs` — the recursive `txfm_split`
    tree, max square transform down to `MAX_VARTX_DEPTH = 2`, ported against
    `av1/decoder/decodeframe.c:1029-1090` and `av1/common/av1_common_int.h`
    (WIP snapshot; verified line by line against the oracle source this round).
  - `TXFM_CTX_INIT = 64` (`decode.rs`, next to `MAX_VARTX_DEPTH`) — **root
    cause of the first gate failure this round**. The WIP initialised
    `Neighbours::above_txfm`/`left_txfm` to 0 with a comment claiming that is
    "libaom's own tile-start value". It is not:
    `av1_common_int.h:1624` memsets the above txfm row to
    `tx_size_wide[TX_SIZES_LARGEST]` and `:1632` the left buffer to
    `tx_size_high[TX_SIZES_LARGEST]` — 64. An unwritten neighbour reads as the
    *widest* transform, so every frame-edge block took the wrong
    `txfm_partition` context.
  - `read_inter_luma8` + `split8` in `decode_inter_block8` — the 8x8 inter
    leaf now reads its own `txfm_split` symbol (a `BLOCK_8X8` does signal one)
    and, when it splits, decodes four 4x4 units, keeping plane-0 neighbour
    context per unit across the tail `record_mi`. All three of that function's
    branches (compound, single-ref, intra-in-inter) now call
    `read_block_tx_size`; without this an 8x8 inter block under `Select`
    desynced at its first coefficient.
  - two narrow refusals at their own site: a var-tx leaf larger than 32x32 (no
    64-point *inter* coefficient tables here) and an intra block in an inter
    frame whose `tx_depth` splits (that path predicts the whole block at once).
- `crates/ec-av1/src/cdf.rs`, `cdf_state.rs` (WIP): `TXFM_PARTITION` (21
  contexts) in the table set, adaptation and the per-frame counter reset;
  `Luma4Inter`/`Luma4InterSet1` coefficient sets.
- `crates/ec-av1/src/stream.rs`
  - the blanket refusal *"an inter frame using TxMode::Select …"* is deleted,
    and `tx_mode == TxMode::Select` is threaded into
    `decode_inter_frame_tile_with_cdfs` (new `tx_select` param → thread-local
    `set_inter_tx_mode`).
  - new gate `a_real_aomenc_inter_sequence_with_tx_select_decodes_pixel_exact`.
- `crates/ec-av1/src/refusal_inventory.rs`: −1 blanket refusal, +2 narrow ones.
  `gate_coverage.rs` needs no change: it derives only `--enable-*` tool flags,
  and `--enable-tx-size-search` is *present* (as `=0`) in the sibling gates, so
  the tool was never on its `NEVER_EXERCISED` list.

## How the existing inter gates restricted tx-mode

`--enable-tx-size-search=0` — it appears **39** times in `stream.rs`
(`grep -c '"--enable-tx-size-search=0"'`), i.e. every inter gate. That flag is
exactly what pins a stream to `TxMode::Largest`. The new gate is the same
recipe as `a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact`
with that flag DELIBERATELY ABSENT (plus `--min-partition-size=16`, because
detailed content otherwise makes aomenc pick sub-16 inter partitions — a
separate, already-named refusal, hit on 30+ of 40 seeds).

## Gate result — RED, honestly

```
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-txselect EC_AV1_REQUIRE_AOMENC=1 \
  cargo test -p ec-av1 --lib a_real_aomenc_inter_sequence_with_tx_select
```

EVIDENCE: cargo test stderr (seed 43) | aomenc default tx-mode, 4-frame 64x64
mandelbrot inter sequence, decode_stream vs ffmpeg per frame |
`txfm_split reads=22 splits taken=8`; frame 0 (key) and frame 1 (first inter
frame, var-tx active) pixel-exact on Y/U/V; frame 2 Y first differs at (59,14)
`got 143 want 144`, 1774/4096 samples; frame 3 differs from (0,0).

So: the var-tx read + per-TU residual path is real and correct for a whole key
frame and a whole inter frame, and the gate fires (8 splits actually taken).
It does not yet pass. **No refusal should be considered "proven lifted" on this
evidence** — the branch is not merge-ready.

Before the `TXFM_CTX_INIT` fix the same gate failed at frame 2 (32,0) with
3209/4096 samples differing; after it, the first divergence moved to (59,14)
with 1774 — the fix is a real advance, not a reshuffle.

## Residue

- fix-now (next round, blocks the merge): frame 2 divergence. Shape: rows 0..13
  of frame 2 match everywhere; the first differing samples are on rows 14/15
  (x 34..62) with deltas of 1..3, then the error spreads to 43% of the frame.
  That is a late-in-block divergence inside the second/third 16x16..32x32
  block of the top block row, not a whole-frame desync from symbol 0.
  Next step is the ledger's blessed rung, not more reasoning: instrumented
  `aomdec` range ladder (`EC_TRACE=1`) vs our `EC_TRACE_MODE_STEP` on frame 2
  only, comparing msac RANGE element by element (never `tell()`), starting at
  the first block of frame 2. Prime suspects, in order: (1) `txfm_partition`
  CDF *adaptation* forwarded into frame 2 (frame 1 is clean, frame 2 is the
  first frame that inherits an adapted copy — the "CDF counter not reset"
  class, though `reset_counts` does list it); (2) `tx_size_context` for an
  intra block in an inter frame, which still reads the deblock `tx_grid`
  instead of libaom's `above_txfm_context`/`left_txfm_context` with the
  `is_inter_block` → block-size override (`pred_common.h:342-371`) — a known,
  unported divergence introduced the moment inter neighbours carry split
  transforms; (3) the deblock tx grid written per var-tx leaf.
- deferred(the frame-2 fix): 10-bit gate — his films are `yuv420p10le` and
  10-bit is a separate standing refusal (`Picture` planes are `Vec<u8>`), so a
  10-bit tx-select gate would prove nothing this round.
- deferred(the frame-2 fix): film probe (`decode_probe` on 0.4 s of each film)
  — pointless until the 8-bit gate is green.
- accepted (documented corner-cut, in the code): a var-tx block's chroma
  deblock edges read `tx_h_grid / 2`, i.e. they follow the luma leaves even
  though the block's uv transform is not split.

## Suite totals

```
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-txselect cargo test -p ec-av1 --lib
test result: FAILED. 267 passed; 1 failed; 23 ignored; 0 measured; 0 filtered out;
finished in 1146.75s
```

The single failure is this lane's own new gate
(`a_real_aomenc_inter_sequence_with_tx_select_decodes_pixel_exact`), red for the
frame-2 reason above. No pre-existing test regressed: the same 267 pass with the
blanket `TxMode::Select` inter refusal removed.

Measured at `138c558`. Two commits landed after it:
- `ecc71ec` — this report only, no code.
- `3c174b6` — `tx_size_context_txfm`, reachable from exactly one call site
  (`read_block_tx_size`'s intra branch, new code this lane); `read_tx_size`'s
  new `ctx_override` is `None` at both pre-existing key-frame call sites
  (`decode.rs:4934`, `:5346`), so it cannot move any of the 267. Re-verified
  after it: the gate (still red, byte-identical failure), plus
  `refusal_inventory` + `gate_coverage` (5 passed, 0 failed). The full 1146 s
  suite was NOT re-run at `3c174b6`.
