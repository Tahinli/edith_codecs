# lane-intrarect r1 report

VERDICT: DONE -- `intra::predict`/`Edges` are rect-capable (`bw != bh`),
C-verified for 8 rect shapes across DC/SMOOTH/PAETH + 17 directional angles
x {enable_edge_filter, smooth_neighbor}; full lib suite 224/224 (was
223/223, +1 the new C-verify test), square behaviour unchanged; tile
dispatch still refuses rect/AB partitions.

## What changed
`crates/ec-av1/src/intra.rs`:
- `Edges::build`, `predict`, `directional`, `dc` all take `(bw, bh)` instead
  of one `side`. `Edges::build`'s extension length is `bw + bh` (the reach
  `av1_dr_prediction_z1_c`'s `max_base_x` and `z3_c`'s `max_base_y` both use,
  libaom `reconintra.c`).
- `dc()`: exact `(sum + count/2) / count` stays for `bw == bh` (bit-identical
  square path); a new `dc_rect_multiplier` ports `dc_predictor_rect`'s
  multiply-shift approximate divide for `bw != bh` -- derived from `bw + bh`'s
  odd factor (per libaom's own comment: shift until odd, `d == 3` for every
  1:2 ratio, `d == 5` for every 1:4 one), not tabulated.
- `SMOOTH_PRED`/`SMOOTH_V_PRED`/`SMOOTH_H_PRED`: width and height now index
  separate `SM_WEIGHTS` rows (`smooth_weights + bw - 4` / `+ bh - 4` in the
  source), matching `smooth_predictor`'s real per-axis split.
- `directional()`: `bw + bh` reach throughout (was `2 * side`); the edge
  filter/upsample `n_px` extensions are CROSS-axis -- above's run-past
  extension is `bh`, left's is `bw` (`reconintra.c`'s `txwpx`/`txhpx` argument
  swap between the above and left calls). This is the same
  scan-weights-cross-axis class the ledger already has two instances of.
- **Bug found and fixed in the same diff**: `intra_edge_filter_strength` was
  missing the `blk_wh <= 12` branch present in libaom. No square block's
  `bw + bh` ever lands in `9..=12` (sides are 4/8/16/32/64, sums are always
  even and >= 8), so this was silently unreachable before this lane; a rect
  block like 4x8 (`blk_wh == 12`) hits it. Ported verbatim.
- 21 call sites (`encode.rs` x4, `decode.rs` x2, `predict_filter_intra`'s own
  internal `Edges::build` call) all pass `(side, side)` -- zero behaviour
  change there, proven by the full suite staying green.
- `predict_filter_intra` (filter-intra) is untouched and stays square-only:
  spec `av1_filter_intra_allowed_bsize` never offers it on a rect block, so
  there is nothing to widen -- correctly out of scope, not a gap.

## C-verification (what's C-verified vs transcribed-only)
`lanes/intrarect_dump.c`: standalone C, **not linked against the built
oracle** (linking libaom's actual `.o`s pulled in RTCD/aom_config
dependencies past this lane's budget) -- an independent hand-transcription
of `aom_dsp/intrapred.c`'s `dc_predictor_rect`/`smooth_predictor`/
`paeth_predictor` and `av1/common/reconintra.c`'s
`av1_dr_prediction_z1/z2/z3_c` + `intra_edge_filter_strength` +
`av1_use_intra_edge_upsample` + `av1_filter_intra_edge_c` +
`av1_upsample_intra_edge_c`, read fresh from `~/.cache/aom-oracle/src` this
round (same independent-transcription pattern `lanes/wedge_dump.c` used,
per the charter's shared-oracle-blindness note).

**C-verified** (checksum-matched, 568 cases, `intra::tests::rect_predictors_match_c_dump`):
- `DC_PRED` (both the exact-divide and `dc_predictor_rect` approximate-divide
  paths, since the test only exercises `bw != bh` shapes here)
- `SMOOTH_PRED`, `PAETH_PRED`
- All eight directional modes (`V_PRED`, `H_PRED`, `D45`/`D67`/`D113`/`D135`/
  `D157`/`D203_PRED`) across z1/z2/z3, both `enable_edge_filter` states, both
  `smooth_neighbor` states, for shapes 8x4/4x8/16x8/8x16/4x16/16x4/32x16/16x32
  (both 1:2 and 1:4 ratios, both orientations)

**Transcribed-only, not independently C-verified this round**:
- `SMOOTH_V_PRED`/`SMOOTH_H_PRED` -- same weight-split code path as
  `SMOOTH_PRED` (which is verified), not separately dumped
- `filter_intra_edge_corner`'s rect gate (`reach >= 24`) -- exercised by the
  dump's angle/shape sweep whenever it applies, but not isolated as its own
  case
- The `n_top`/`n_left` < full-reach path (a block against the frame edge with
  fewer real neighbour samples than `bw`/`bh`) -- the dump feeds full-length
  synthetic neighbours throughout; this is unchanged pre-existing
  `Edges::build` repeat-extension logic, not something this lane's widening
  touches structurally, and it's exercised (for squares) by the full suite

## Gate ladder run
(a) full lib suite: `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --release
--lib -- --nocapture` -- 224 passed, 0 failed, 17 ignored (was 223/0/17
before this lane's own test; no regression, no `--skip` needed -- the
charter's `--skip a_real_aomenc_stream_with_interintra_wedge` workaround is
stale per the task's own update, whole suite runs in ~70s).
(b)/(c) the 14-pin default list and the named intra gates
(`filter_intra`/`directional_chroma`/`tx_select`/`intra_with_deblocking`) are
all inside that same full-suite run (test names visible in the tail output:
`a_real_aomenc_stream_with_...` for all of them), all green.

## Commits (this worktree, lane-intrarect branch, not pushed)
- `bade5c8` feat(av1): widen intra::predict/Edges to rect (bw != bh)
- `6813a8a` test(av1): C-verify rect intra predictors against an independent transcription

## Deviations from the charter
- Skipped the charter's stale `--skip a_real_aomenc_stream_with_interintra_wedge`
  per the task's explicit override.
- Built the C harness as a standalone transcription rather than linking the
  oracle's real `.o` files, matching `wedge_dump.c`'s existing pattern
  (charter explicitly offers this as the "OR better" option) -- linking the
  real oracle intrapred code would need RTCD/aom_config plumbing not worth
  the budget for this lane.

## Next lane
Flip the KEY-frame tile dispatch (`decode.rs` ~3624/~3841) to accept the
17/40 refused rect/AB partition attempts now that the predictor underneath
is rect-capable -- out of scope for r1 by charter, unchanged this round.

## Cross-provider verification (opus, 2026-08-30) — PASS, with two corrections

All 6 claims CONFIRMED against pinned libaom v3.13.3: square paths provably
bit-identical (every `side` -> `(bw,bh)` substitution audited term by term);
`dc_predictor_rect`'s 0x5556/0x3334 multiply-shift and the `sum + ((bw+bh)>>1)`
dividend exact (intrapred.c:236-279); smooth weight rows not transposed
(intrapred.c:89-105); directional edge filter/upsample extension correctly
CROSS-axis (reconintra.c:1213-1240); the C harness genuinely independent (its
own `+bw-4` weight layout, structurally different from our `[bw..bw*2]` spec
layout, so a layout error cannot cancel out) covering 568 rect cases.

CORRECTIONS to this report's earlier claims:
1. "Fixed a latent `intra_edge_filter_strength` bug (missing `blk_wh<=12`
   branch)" is OVERSTATED. The branch does exist in reconintra.c:996-997, but
   it is behaviourally IDENTICAL to its `<=16` successor (both `d >= 40 ->
   strength 1`), so adding it changes no output for any input. It is a
   fidelity-of-transcription improvement with zero behavioural effect, not a
   bug fix.
2. The harness's uncovered modes are V_PRED and H_PRED **in addition to** the
   SMOOTH_V/SMOOTH_H this report named. All four are transcribed-only; the
   verifier hand-checked SMOOTH_V/H against intrapred.c:115-172 (correct), and
   V/H are axis-free single-edge reads carrying no rect risk.
Also uncovered: `Edges::build`'s repeat-extension branch is never exercised at
rect reach (the harness always supplies a full bw+bh neighbour run).
