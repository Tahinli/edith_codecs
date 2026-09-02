# lane-fistrip r2 — verifier fix-now items

Round 1 (57f1fcb) landed the defect fix (8x16/16x8 strips never read their
`use_filter_intra` flag). This round closes the four fix-now items the verifier
raised against it. No decode-path behaviour changed: every edit is a test, a
generator script, or a gate's seed window.

## 1. The filter-intra CDF row table is generated from the oracle, and pins every allowed shape

`scripts/extract-filter-intra-cdfs.py` (new) reads `BLOCK_SIZES_ALL`'s order from
`av1/common/enums.h` and `default_filter_intra_cdfs` from `av1/common/entropymode.c`
in the oracle tree and prints the `(width, height, default)` rows for the shapes
`av1_filter_intra_allowed_bsize` allows. r1 had guessed 16384 for the four 1:4
shapes; their real rows are **4x16=12770, 16x4=10368, 8x32=20229, 32x8=18101**
(entropymode.c:827-828, BLOCK_SIZES_ALL indices 16..19).

`decode.rs` `filter_intra_classes_carry_their_own_libaom_default_row` now lists all
14 allowed shapes with their true row plus a `refused_by` note for the six with no
class today (4x8/8x4 → "a partition below 8x8 …"; 4x16/16x4 → the 16x16-level
partition refusal; 8x32/32x8 → "a 32x32 partition type this decoder does not code"),
and asserts, per shape: the class's row equals the oracle default, the row is a real
2-symbol CDF (`{p, 32768, 0}`), no two shapes share a class, and every one of
`cdf::FILTER_INTRA`'s 8 rows is claimed by exactly one shape. So the round that
lifts one of those refusals cannot land a wrong CDF row for the shape it unlocks.

## 2. `predict_filter_intra` now has a bit-exact unit test

`intra.rs` `filter_intra_matches_the_libaom_predictor_for_every_mode_and_shape`:
`av1_filter_intra_taps` and `av1_filter_intra_predictor_c` /
`highbd_filter_intra_predictor` (reconintra.c:807-955) are transcribed into the test
module from the C, independently of `FILTER_INTRA_TAPS`, and compared against
`predict_filter_intra` for **5 modes x 14 shapes (4x4 … 32x32, both orientations of
1:2 and 1:4) x 3 seeded random edge sets x {8-bit, 10-bit} = 420 blocks**, whole
block, bit-exact. Before this round the predictor had no unit test at all.

## 3. Both firing seeds are inside the ignored gate's default window

r1 recorded only the first firing seed (49) of its 200-seed measurement. r2 re-ran
just the missing window (seeds 50..=241, `EC_FISTRIP_GATE_SEED0=50
EC_FISTRIP_GATE_ATTEMPTS=192`, 31 s): **172 pixel-exact matches, 20 named refusals,
exactly one attempt with a sub16 filter-intra delta -- seed 213**, refusing at
"a coded (non-skip) HORZ/VERT rect strip below 16x16". So over seeds 42..=241 the
firing set is exactly {49, 213}, both `skip=0`.

The 8-bit gate now builds its seed list as `(0..n_attempts).map(|a| 42 + a)` plus
`FIRING_SEEDS = [49, 213]` (appended when the window misses them), so the default
40-attempt run -- and even a 2-attempt one -- carries both hits; the doc and the
`#[ignore]` string both name them. The 10-bit arm gets seed 213 appended too, marked
in its doc as a widening from the 8-bit recipe's measurement, NOT a measured 10-bit
firing set (its encode is a different stream; the round that un-ignores it must
re-measure). The temporary `EC_FISTRIP_GATE_SEED0` knob used for the scan was removed
again -- the seeds are in the source, not in an env var.

## 4. MERGE NOTE

`a_pinned_aomenc_16x8_strip_reads_its_use_filter_intra_flag` (stream.rs) asserts
`decode_stream(...).expect_err(...)` on the refusal
"a coded (non-skip) HORZ/VERT rect strip below 16x16". **The round that lifts that
refusal MUST flip this test from `expect_err` to a full pixel compare against ffmpeg**
(the fixture's strip is `skip=0`, so it becomes decodable the moment the ceiling
moves) and un-ignore
`a_real_aomenc_stream_with_filter_intra_on_a_sub16_horz_vert_strip_decodes_pixel_exact`
plus its 10-bit twin. A green `expect_err` after that lift is a stale refusal claim,
not a pass.

## Files changed

- `scripts/extract-filter-intra-cdfs.py` (new) — generates the `default_filter_intra_cdfs`
  rows from the oracle tree; no hand transcription.
- `crates/ec-av1/src/decode.rs:16315` — the domain test now pins all 14 allowed shapes
  (true rows for the six refused ones) + row shape + class uniqueness + full table coverage.
- `crates/ec-av1/src/intra.rs` (tests) — independent C port of
  `av1_filter_intra_taps`/`av1_filter_intra_predictor_c` and the 420-block bit-exact compare.
- `crates/ec-av1/src/stream.rs` — 8-bit sub16 gate: seed window + `FIRING_SEEDS = [49, 213]`,
  doc and `#[ignore]` string record both firing seeds; 10-bit arm: seed 213 appended with
  its "widening, not measured" note.

## EVIDENCE

EVIDENCE: `scripts/extract-filter-intra-cdfs.py` output | run against `~/.cache/aom-oracle/src` |
14 rows, 1:4 shapes = 12770/10368/20229/18101 (r1 had 16384 for all four)

EVIDENCE: `cargo test -p ec-av1 --lib filter_intra` | 5 passed, 0 failed, 2 ignored (14.9 s) |
`filter_intra_matches_the_libaom_predictor_for_every_mode_and_shape` compares 420 blocks
(2 depths x 14 shapes x 5 modes x 3 seeds), all bit-exact; the test asserts that count

EVIDENCE: `$HOME/.cache/fistrip-seedscan-r2.log` | `EC_FISTRIP_GATE_SEED0=50
EC_FISTRIP_GATE_ATTEMPTS=192` on the ignored 8-bit gate | 172 matches, 20 refusals, one
sub16 delta: "seed 213 refused with sub16 filter-intra delta 1"

EVIDENCE: `EC_FISTRIP_GATE_ATTEMPTS=2 ... -- --ignored --nocapture` | default window of 2
attempts + FIRING_SEEDS | "seed 49 ... delta 1", "seed 213 ... delta 1",
"filter_intra_rect_sub16_hits over ALL attempts (refused included) = 2"; the gate still
fails its `compared_hits > 0` assert, which is exactly the ceiling it is ignored for

## Residue

- deferred: the pixel gates (8- and 10-bit) stay `#[ignore]`d — blocked on
  "a coded (non-skip) HORZ/VERT rect strip below 16x16"; un-ignore + flip the pinned
  test's `expect_err` (MERGE NOTE above) the round that lifts it.
- deferred: 10-bit firing seeds are unmeasured — the 10-bit encode is a different stream;
  measure when that gate un-ignores.
- accepted: `predict_filter_intra` is now proven on all 14 shapes including the four 1:4
  ones whose partition levels refuse — prediction ahead of reach, deliberately.

## Suite totals

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib` under `systemd-run --user`
(log `$HOME/.cache/fistrip-suite-r2.log`, 257 s): **301 passed, 1 failed, 29 ignored**
(r1: 300/1/29 — the new predictor test is the +1).

The single failure is the PRE-EXISTING one already red on main `df5d630`:
`decode::tests::nz_map_ctx_offset_tables_match_the_rect_rule` (32x64 nz_map offset at
display row 0 col 2), the rectsplit-r4 `[col][row]` convention clash. This round touched
neither `NZ_MAP_CTX_OFFSET_*` nor `base_ctx_rect`.
`refusal_inventory` (3 passed) and `gate_coverage` (9 passed) both green.
