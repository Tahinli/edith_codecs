# lane-tx4x8 round 2 — the RED gate's root cause: a tx_depth symbol never read

Branch `lane-tx4x8`, commit `3677783` off r1's `efed7ee` (not rebased onto main, charter order).

## Root cause

`decode_leaf_rect8` (decode.rs:6081) read `skip`/`y_mode`/`uv_mode` and went straight to the
leaf's coefficients. libaom's `av1_read_tx_size` writes a `tx_depth` symbol for every intra
block with `block_signals_txsize(bsize)` — true for `BLOCK_4X8`/`BLOCK_8X4` — whenever the
frame's tx mode is `TX_MODE_SELECT`, which aomenc's **default** `--enable-tx-size-search=1`
selects and no gate recipe turns off. We consumed one symbol fewer than the encoder wrote, so
the tile desynced at the very first rect leaf and every later symbol read garbage that
decoded as near-zero residual (hence "near-flat 126..142 from sample 0, no decode error").
Class `symbol-consumption-gap`; the 16x16-level strip path (decode.rs:3473) already carries a
comment about being bitten by exactly this.

Decisive measurement (range ladder, not tell()): our post-`y_mode` range equalled the
oracle's (38104), but the oracle then entered its first coefficient read at rng=46600 — one
symbol's narrowing + renorm that we had not performed.

## What changed (`crates/ec-av1/src/`)

* `decode.rs:6081` `decode_leaf_rect8` — new `tx_select` parameter (threaded at both call
  sites, decode.rs:8019 / 8146). Per sub-block, after the mode read and the filter-intra
  refusal: `tx_size_cat0[tx_size_context_rect(lmi, bw, bh)]`.
  `bsize_to_tx_size_cat(BLOCK_4X8) == 0` because `sub_tx_size_map[TX_4X8] == TX_4X4` is a
  single step — the same 2-symbol category an 8x8 uses, **not** the 3-symbol `tx_size_cat2`
  the 32x16 strips read (class `table-indexed-by-raw-size` avoided).
* `decode.rs` same function — depth 1 ported, not refused: two 4x4 transform units along the
  leaf's long axis (`decode_leaf8`'s own depth-1 loop with two units instead of four), each
  predicted from the previous unit's reconstruction, each with its own
  `luma_skip_ctx`/`around_mi`/`TxbSet::Luma4(Set1)`. **Skipped** leaves take the per-unit path
  too: a skipped 4x8 predicted in one shot is a different picture from two chained 4x4
  predictions. `fill_lf_grid_rect` records tx 4x4 in that case so the next block's
  `tx_size_context` reads the coded size.
* `decode.rs:944` — `RECT8_SPLIT_TX_HITS` + `rect8_split_tx_hits()`; separate from
  `TX_DEPTH_HITS`, which every square path also bumps.
* `decode.rs` `read_coeffs_rect` — `read_coeffs`'s `EC_TRACE_COEFF` range ladder
  (`all_zero`/`tx_type`/`eob`/`base_eob`/`base`/`br`) in the oracle's own tag format. This was
  the missing instrument: r1 could not compare rect coefficients element by element at all.
* `stream.rs:7429` gate — per-arm hard assert `split_tx > 0` on top of the existing
  4x8/8x4 coded-leaf asserts.

No refusal string added or removed this round (`refusal_inventory` / `gate_coverage`
unchanged; r1 already removed the three this lane owns). The 16x16-level
"a HORZ/VERT intra strip with a split transform" refusal stays — it guards the 32x16 strips,
a different path.

## Gate: GREEN (3 arms)

`EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-tx4x8 cargo test -p ec-av1 --lib sub8_rect_leaf`
-> `test result: ok. 1 passed; 0 failed` (32.5s), arms 16x16 8-bit / 128x16 8-bit
`--tile-columns=1` / 16x16 10-bit, each with >=4 firing pixel-exact runs and >=1 split-tx leaf.

EVIDENCE: /tmp/.../scratchpad/s303a.obu (sha256 272ff85db3409432619b5e7ca8fefdde50616c08b4576b1e37823e2cd7912e95, regenerated twice, identical) | aomenc seed=303 cq=12 16x16 testsrc2+noise with the gate's own flags; `decode_probe s303a.obu ours.yuv` vs `ffmpeg -i s303a.obu -f rawvideo ref.yuv` | `cmp` byte-identical 384/384 bytes (was: mismatch at sample 0, ours 126 vs ref 76)

EVIDENCE: EC_TRACE_COEFF ladder, ours vs instrumented aomdec, first rect leaf of s303a.obu | `EC_TRACE_COEFF=1 decode_probe` vs `EC_TRACE_COEFF=1 EC_TRACE_MODE=1 aomdec` | before: first `all_zero` rng 36990 vs oracle 45231 (diverged at the first coefficient symbol); after: 45231 / 54240 / eob=30 / 62552 / 63488 / 59248 / 46438 / 45648 / 49904 / 35324 / 32812 — every element equal. Positions differ only by the known layout convention (oracle prints `scan[c]` in an 8-wide grid, ours in a 4-wide one: oracle 30/15/22/29 = ours 27/29/26/23 transposed — the same coefficients).

EVIDENCE: $HOME/.cache/tx4x8-suite.log | `EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-tx4x8 cargo test -p ec-av1 --lib -j3` (full lib suite, aomenc required so an absent oracle fails) | `test result: ok. 274 passed; 0 failed; 22 ignored; 0 measured; 0 filtered out; finished in 768.55s` -- every sibling gate (sub8, tiny sweep, coded_rect_strip_below_16x16, refusal_inventory, gate_coverage) is in that run and green

## Film probe (charter recipe, `ffmpeg -t 0.4 -c:v copy -f obu`)

* Hunger Games (2160p 10-bit): `REFUSED: filter intra on a HORZ/VERT strip (this decoder
  predicts square-only)`.
* Troy (1080p 10-bit): `REFUSED: a 32x32 partition type this decoder does not code (value=4)`
  (HORZ_A/HORZ_B/VERT_A).

Measured only AFTER the fix — r1 recorded no before-value for these extracts, so no
"advanced from X to Y" claim is made.

## Residue

* deferred: rect filter-intra prediction (`predict_filter_intra` is square-only) — blocks the
  Hunger Games head — unblocked by a rect filter-intra predictor, which the 16x16-level strips
  need identically; one lane should own both.
* deferred: 32-level `PARTITION_HORZ_A/B/VERT_A` (Troy's stop) — other lane's cluster.
* accepted: `read_coeffs_rect`'s trace prints `pos` in our own row-major-by-`w` layout, which
  is the transpose of the oracle's for rect sizes. Documented here rather than "fixed", because
  changing it would make our trace disagree with our own `read_coeffs`.
