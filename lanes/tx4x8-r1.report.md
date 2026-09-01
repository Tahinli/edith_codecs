# lane-tx4x8 round 1 — TX_4X8/TX_8X4 end to end (gate RED, first divergence not yet isolated)

Branch `lane-tx4x8` off `lane-sub8` b500160. NOT rebased onto main (charter order).

## What changed

* `crates/ec-av1/src/cdf.rs` — `EOB_PT_32_LUMA{,_Q0,_Q1,_Q3}` + `_CLASS1` siblings,
  transcribed from `av1_default_eob_multi32_cdfs` (`token_cdfs.h:792`), luma plane, both
  tx-class rows. Format cross-checked against the already-pinned `EOB_PT_16_LUMA_CLASS1_Q0`.
* `crates/ec-av1/src/cdf_state.rs` — `eob_pt_32_luma{,_class1}` state + q-ctx `pick` + counter
  reset; `TxbSet::LumaRect4x8`/`LumaRect4x8Set1`. Derivation: `get_txsize_entropy_ctx(TX_4X8)`
  = `(txsize_sqr_map=TX_4X4 + txsize_sqr_up_map=TX_8X8 + 1) >> 1` = **TX_8X8**, so every
  coefficient table is `Luma8`'s; `read_tx_type`'s CDF row is `txsize_sqr_map[TX_4X8]` =
  **TX_4X4**, so the `tx_type` table is `Luma4`/`Luma4Set1`'s; `eob_pt` is the true
  32-position table (`txsize_log2_minus4[TX_4X8] == 1` -> `eob_flag_cdf32`).
  (class `table-indexed-by-raw-size`: three different size keys in one variant.)
* `crates/ec-av1/src/decode.rs` —
  - `SCAN_4X8`/`SCAN_8X4` (libaom `default_scan_4x8`/`8x4`), `class_scan_table_wh`
    (`mrow`/`mcol` at rect sizes). Conversion `(p % h) * w + p / h` **re-proved first** by
    regenerating the committed `SCAN_16X8`/`SCAN_8X16` from the same source (heads matched).
  - `base_ctx_rect`/`br_ctx_rect` gained a `TxClass` (the 1D-class arms are shape-independent
    in libaom's `get_nz_map_ctx_from_stats`; no new `av1_nz_map_ctx_offset_*` table is needed —
    the `w<h -> 11+ctx` / `w>h -> 16+ctx` rule already in `base_ctx_rect` IS that table).
  - `read_coeffs_rect` now reads a real `tx_type` symbol and honours `V_DCT`/`H_DCT`
    (rect class scan + class contexts + `eob_pt_class1`). Two capability-claim refusals
    deleted.
  - `decode_leaf_rect8` — the `PARTITION_HORZ`/`VERT` leaf of an 8x8: two `BLOCK_8X4`/`4X8`
    sub-blocks, mode reads via `read_intra_mode_sub8`, chroma once on the last sub-block at the
    group's 4x4 unit, `dequant_and_inverse_typed_wh` (the rect transform primitive, its
    `abs(rect_type)==1` `NewInvSqrt2` scale and `av1_inv_txfm_shift_ls[TX_4X8]` row, already
    existed and needed no change), `Reach::of_rect`, `fill_lf_grid_rect(bw, bh)` for the
    deblock grid, `partition_context_lookup[BLOCK_4X8] = {31,30}` / `[BLOCK_8X4] = {30,31}`.
    Wired at BOTH sub-8x8 partition sites; the leftover `else` is now `unreachable!`
    (`partition_w8` is a 4-symbol CDF).
  - counters `tx4x8_coded_hits`/`tx8x4_coded_hits` (leaves with REAL coefficients only).
  - 4 new table tests (transposed-pair scan gate, class-1 scan pins, eob table shape,
    shape-specific reach).
* `crates/ec-av1/src/encode.rs` — `has_tr_4x8`/`has_tr_8x4`/`has_bl_4x8`/`has_bl_8x4`
  transcribed verbatim; `rect_reach_tables(bw, bh)` replaces the `bw == 16` two-way pick.
* `crates/ec-av1/src/refusal_inventory.rs` — three refusals removed: the 4x8/8x4 partition
  refusal and the two rect `tx_type`/`tx class` capability claims.
* `crates/ec-av1/src/stream.rs` — gate
  `a_real_aomenc_stream_with_a_sub8_rect_leaf_decodes_pixel_exact`, three arms
  (8-bit 16x16, 8-bit 128x16 `--tile-columns=1`, 10-bit 16x16), `--enable-rect-partitions=1
  --min-partition-size=4 --max-partition-size=8`, per-attempt hard assert that BOTH
  `tx4x8_coded_hits` and `tx8x4_coded_hits` moved, named refusals only, no SKIP on error.

## Gate: RED

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib sub8_rect_leaf`
-> `luma vs ffmpeg (seed=303 cq=12 depth=8 tiles=[])`, mismatch from sample 0 of a 16x16
frame; ours is near-flat (126..142), ffmpeg's is full-range (0..229). The decode does not
error, so this is wrong coefficients/reconstruction (or an entropy divergence inside the
first rect leaf), not a refusal.

EVIDENCE: cargo test output above | 40 attempts x 3 arms, aomenc --enable-rect-partitions=1
--min-partition-size=4 --max-partition-size=8 --enable-filter-intra=0 on testsrc2+noise 16x16 |
first firing attempt (seed 303) mismatches luma at index 0; 291 other lib tests unaffected at
the time of the run.

EVIDENCE: probe sweep with examples/decode_probe + EC_AV1_TRACE=1 | 12 seeds x 16x16, 12 x
32x32, --enable-filter-intra=0 | 16x16: 12/12 decode without any refusal, 3/12 code BOTH
orientations, 9/12 at least one; 32x32: 1/12 (11 die on 16x16-LEVEL rect refusals owned by
other lanes) — this is why the gate uses 16x16/128x16 fixtures.

## Recipe findings (cite before reusing)

* `--max-partition-size=8` does NOT stop aomenc coding a 16x16 block as two 16x8 strips: at
  64x64, 39/40 attempts refuse on 16x16-level rect gaps ("a HORZ/VERT intra strip with a split
  transform", "a coded (non-skip) HORZ/VERT rect strip below 16x16", "a HORZ_A/HORZ_B/VERT_A
  partition below 16x16"). Only a 16-pixel frame dimension isolates the 8x8 level.
* With `--enable-filter-intra=1`, most attempts die on the pre-existing square-only filter-intra
  refusal, which `decode_leaf_rect8` re-raises by name for a 4x8 leaf.
* A probe sweep whose binary path was wrong reported "OK, 0 hits" for every cell
  (class `stale-output-faked-measurement`): always `ls -l` the probe binary in the lane's own
  `CARGO_TARGET_DIR` first. The first sweep in this round was invalid for exactly that reason.

## Residue

* fix-now (next round): the pixel mismatch. Nothing in the ladder below was disproved this
  round, ranked by suspicion:
  1. an entropy divergence at the FIRST rect leaf (range ladder vs instrumented aomdec,
     `EC_TRACE_COEFF`, on the pinned seed-303 stream) — ours decoding a near-flat frame is
     consistent with an `eob`/`all_zero` read off the wrong table;
  2. `TxbSet::LumaRect4x8`'s three-way table split (coeff tables TX_8X8 / tx_type row TX_4X4 /
     eob 32) — verify each against `read_coeffs_txb` live, not against my derivation;
  3. `dequant_wh` / the rect transform at (4,8) — never exercised below 16x8 before this lane;
  4. `Reach::of_rect` at 4x8/8x4 (localized errors only — would not explain sample 0).
* deferred: `--tile-columns=1` arm result — not separable while arm 1 is red — unblocked by
  the fix above.
* deferred: filter intra on a 4x8/8x4 leaf — `predict_filter_intra` is square-only — unblocked
  by a rect filter-intra predictor (same refusal the 16x16-level strips carry).
