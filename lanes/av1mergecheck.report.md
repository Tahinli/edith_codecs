# lane-av1mergecheck — do the queued lanes' new gates run from a CLEAN CHECKOUT?

Base for every diff: `main` = `a21f3680` (merge-base per branch). Nothing in the primary
checkout was edited; every run happened in a `git archive` materialisation under
`~/.cache/wt/av1mc/<branch>/`.

## Verdict

**All 13 queued lanes' new gates run and pass from a clean checkout of their own branch.**
No untracked fixture, no absolute-path dependency, no environment escape hid a gate.
Two gates are `#[ignore]`d and therefore do NOT run in a normal suite invocation
(lane-av1pins) — that is a claim/behaviour mismatch in that lane's report, not a broken
gate: both pass under `--ignored`. One lane ref name in the ticket does not exist
(`lane-av1444edge`); its content is parked at `lane-park-av1444edge` / `a02a2511`.

39 gates were run. 37 passed in a plain run, 2 more passed under `--ignored`,
0 reported `0 passed; N filtered out`.

## Method (identical for every branch)

1. `git archive <tip> | tar -x -C ~/.cache/wt/av1mc/<branch>` — tracked files only.
2. `git init` in that dir (empty; the `.cargo/config.toml` test runner resolves
   `scripts/memguard-runner.sh` through `git rev-parse --show-toplevel`, which fails
   outside a repo) and `ln -s <primary>/fixtures <dir>/fixtures` (root `fixtures/` is
   gitignored, so its absence from a clean checkout is expected, not the finding).
3. Every fixture referenced by the branch's new source lines checked with
   `git ls-tree -r <tip> -- crates/ec-av1/fixtures/`.
4. `cargo test --release -p ec-av1 --lib --no-run`, then the produced test binary invoked
   ONCE PER GATE with `--exact <module::tests::name> --nocapture --test-threads=1`, under
   `EC_AV1_REQUIRE_FFMPEG=1 EC_AV1_REQUIRE_AOMENC=1 EC_REQUIRE_FIXTURES=1`.
   One filter per invocation; no repeated `--exact`.
5. Every gate then re-run a second time with the root `fixtures/` symlink DELETED, to
   separate "the pin is committed in the crate" from "the gate found the gitignored root".

Environment notes, stated so the numbers are reproducible:
- `EC_NOMEMGUARD=1` (see finding F3 — the memguard scope collided, not the gates).
- Release profile, per-branch `CARGO_TARGET_DIR=$HOME/.cache/cargo-target/av1mc/<branch>`.
- Oracles: `/usr/bin/ffmpeg` and `~/.cache/aom-oracle/build/aomenc` (the latter is NOT on
  `PATH` on this box; `aomenc_path()` falls back to it).

## Per-gate table

`clean` = run from the `git archive` checkout. `no-root-fixtures` = same run with the
symlinked gitignored root `fixtures/` removed.

| branch (tip) | gate (`module::tests::name`) | clean checkout | what it printed | pin tracked | no-root-fixtures |
|---|---|---|---|---|---|
| lane-av1chrtx (ff7cf47c) | `stream::tests::a_pinned_444_rect_inter_stream_resolves_each_chroma_unit_from_its_own_luma_leaf` | ok, 1 passed (0.06s) | 4 decode-order frames byte-exact vs aomdec; per-leaf tx_type arm counts | yes (`444_rect_strip_leaf_tx_type.obu`) | ok |
| lane-av1chrtx | `decode::tests::covering_leaf_tx_type_picks_the_leaf_under_the_unit_or_reports_none` | ok, 1 passed | unit tests only, no fixture | n/a | ok |
| lane-av1gates444 (555c83f3) | `stream::tests::a_444_lossy_odd_66x66_stream_decodes_pixel_exact` | ok, 1 passed (0.30s) | live aomenc encode, byte-exact vs ffmpeg | n/a (live encode) | ok |
| lane-av1gates444 | `stream::tests::a_444_lossy_odd_130x122_stream_decodes_pixel_exact` | ok, 1 passed (0.58s) | same shape, odd 130x122 | n/a | ok |
| lane-av1gates444 | `stream::tests::a_444_lossy_256x128_stream_decodes_pixel_exact` | ok, 1 passed (0.98s) | same shape, 256x128 | n/a | ok |
| lane-av1skipfix (b696d485) | `stream::tests::a_real_aomenc_filter_intra_stream_decodes_pixel_exact` | ok, 1 passed (0.54s) | attempt-loop gate buckets, no SKIP | n/a | ok |
| lane-av1skipfix | `stream::tests::a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact` | ok, 1 passed | buckets; deblock edges counted | n/a | ok |
| lane-av1skipfix | `stream::tests::a_real_aomenc_inter_sequence_with_deblocking_decodes_pixel_exact` | ok, 1 passed | buckets; deblock edges counted | n/a | ok |
| lane-av1skipfix | `stream::tests::a_real_libaom_gradients_stream_with_cdef_decodes_pixel_exact` | ok, 1 passed | buckets; cdef_idx read | n/a | ok |
| lane-av1skipfix | `stream::tests::a_real_aomenc_lossless_444_key_frame_decodes_sample_exact` | ok, 1 passed (0.15s) | reads the committed pin via `CARGO_MANIFEST_DIR`, size+fnv asserted | yes (`ll444_lossless_key.obu`) | ok |
| lane-av1skipfix | `refusal_inventory::tests::gates_that_swallow_a_decode_error_are_declared` | ok, 1 passed | empty inventory holds | n/a | ok |
| lane-av1superpin (9646f74d) | `stream::tests::a_444_lossy_rect4_inter_stream_decodes_pixel_exact` | ok, 1 passed (0.26s) | frames byte-exact vs aomdec | yes (`444_lossy_rect4_inter_witness.obu`) | ok |
| lane-av1superpin | `stream::tests::a_444_lossy_rect4_strip_stream_decodes_pixel_exact_at_odd_and_wide_geometries` | ok, 1 passed | 1:4 chroma gather + inter strip counts for both geometries | yes (`…_odd_130x122`, `…_wide_256x128`) | ok |
| lane-av1superpin | `stream::tests::a_444_lossy_superres_stream_decodes_pixel_exact` | ok, 1 passed | d12 pin, upscale counts | yes (`444_lossy_superres_256x128_d12.obu`) | ok |
| lane-av1superpin | `stream::tests::a_444_lossy_superres_mode1_den9_stream_decodes_pixel_exact` | ok, 1 passed | d9 pin | yes (`…_d9.obu`) | ok |
| lane-av1superpin | `stream::tests::a_444_lossy_superres_mode2_random_denom_stream_decodes_pixel_exact` | ok, 1 passed (4.3s) | per-frame (denom, coded) table | yes (`…_mode2_256x128.obu`) | ok |
| lane-av1superhbd (a2da03e4) | `stream::tests::a_444_lossy_superres_10bit_d12_stream_decodes_pixel_exact` | ok, 1 passed | 10-bit superres | yes (`…_d12_10bit.obu`) | ok |
| lane-av1superhbd | `stream::tests::a_444_lossy_superres_12bit_d12_stream_decodes_pixel_exact` | ok, 1 passed | 12-bit superres | yes (`…_d12_12bit.obu`) | ok |
| lane-av1superhbd | `stream::tests::a_444_lossy_superres_12bit_d9_stream_decodes_pixel_exact` | ok, 1 passed | 12-bit d9 | yes (`…_d9_12bit.obu`) | ok |
| lane-av1superhbd | `stream::tests::a_444_lossy_superres_10bit_mode2_random_denom_stream_decodes_pixel_exact` | ok, 1 passed | 10-bit mode 2 | yes (`…_mode2_…_10bit.obu`) | ok |
| lane-av1superhbd | `stream::tests::a_444_lossy_superres_12bit_mode2_random_denom_stream_decodes_pixel_exact` | ok, 1 passed | 12-bit mode 2 | yes (`…_mode2_…_12bit.obu`) | ok |
| lane-av1pins (44a05b40) | `stream::tests::pinned_golden3_stream_decodes_pixel_exact` | **ignored** (`0 passed; 1 ignored`) — see F1; **1 passed** with `--ignored` | `4 frame(s) byte-exact vs ffmpeg from …/crates/ec-av1/fixtures/golden3-pin.obu` | yes (`golden3-pin.obu`) | ok (with `--ignored`) |
| lane-av1pins | `stream::tests::pinned_sbpart_stream_decodes_pixel_exact` | **ignored** — see F1; **1 passed** with `--ignored` | quiet pass; pin read, per-SB mismatch walk | yes (`sbpart-pin.obu`) | ok (with `--ignored`) |
| lane-av1pins | `stream::tests::a_real_aomenc_lossless_444_key_frame_decodes_sample_exact` | ok, 1 passed (0.39s) | committed pin, no SKIP | yes (`ll444-lossless-key.obu`) | ok |
| lane-av1distwtd (1aaf7e0b) | `gate_coverage::tests::the_detector_sees_the_gates_the_spelling_filter_missed` | ok, 1 passed | detector finds the renamed gates | n/a | ok |
| lane-av1distwtd | `stream::tests::a_distance_weighted_compound_stream_decodes_pixel_exact` | ok, 1 passed (1.6s) | byte-exact vs oracle | n/a (live encode) | ok |
| lane-av1lm444loss (9249442f) | `stream::tests::a_lossless_block_clips_its_transform_grid_at_the_frame_edge` | ok, 1 passed (0.07s) | edge-clip counters | yes (`ll444_128root_lossless.obu`, `ll444_minp64_128root_control.obu`) | ok |
| lane-whtshape (f541d062) | `stream::tests::a_444_lossless_sb64_intrabc_rect_chroma_walks_4x4_units` | ok, 1 passed | 4x4 chroma unit walk counters | yes (`ll444_sb64_1to4_lossless.obu`) | ok |
| lane-whtshape | `transform::lossless_tx_tests::a_non_4x4_lossless_unit_is_the_raster_of_4x4_wht_units` | ok, 1 passed | unit test, no fixture | n/a | ok |
| lane-whtshape | `transform::lossless_tx_tests::a_lossless_split_whose_other_tiles_are_all_zero_still_reconstructs` | ok, 1 passed | unit test | n/a | ok |
| lane-whtshape | `transform::lossless_tx_tests::the_4x4_lossless_fast_path_is_unchanged` | ok, 1 passed | unit test | n/a | ok |
| lane-whtshape | `transform::lossless_tx_tests::a_non_4x4_all_zero_lossless_unit_keeps_the_empty_marker` | ok, 1 passed | unit test | n/a | ok |
| lane-av1tilemeasure (df1370dd) | `stream::tests::a_real_aomenc_12bit_stream_with_two_tile_rows_and_a_two_by_two_tile_grid_decodes_pixel_exact` | ok, 1 passed (3.1s) | frame-facts table, 2 tile rows + 2x2 grid | n/a (live encode) | ok |
| lane-av1tilemeasure | `stream::tests::a_real_aomenc_444_superres_stream_at_8_10_and_12_bit_decodes_pixel_exact` | ok, 1 passed (3.4s) | three bit depths, frame-facts table | n/a | ok |
| lane-av1tilemeasure | `stream::tests::a_real_aomenc_444_partial_and_odd_coded_dimension_streams_decode_pixel_exact` | ok, 1 passed (4.8s) | partial/odd coded dims | n/a | ok |
| lane-av1tilemeasure | `stream::tests::a_lossless_444_8bit_untiled_control_and_its_two_tile_column_sibling_decode_pixel_exact` | ok, 1 passed (1.9s) | control + 2 tile columns | n/a | ok |
| lane-av1dualfilter10 (c2b969c8) | `gate_coverage::tests::the_detector_sees_the_gates_the_spelling_filter_missed` | ok, 1 passed | detector | n/a | ok |
| lane-av1dualfilter10 | `stream::tests::a_real_aomenc_10bit_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact` | ok, 1 passed (2.9s) | 10-bit dual filter + OBMC 8x8 | n/a | ok |
| lane-av1dualfilter10 | `stream::tests::a_distance_weighted_compound_stream_decodes_pixel_exact` | ok, 1 passed (1.8s) | (carried from lane-av1distwtd) | n/a | ok |
| lane-av1mutproof (ffd8d138) | `stream::tests::a_lossless_444_10bit_inter_stream_decodes_pixel_exact` | ok, 1 passed (0.47s) | report-only lane; the gate itself is on main | n/a | ok |
| lane-av1mutproof | `stream::tests::a_444_12bit_inter_sequence_decodes_pixel_exact` | ok, 1 passed (1.3s) | report-only lane | n/a | ok |
| lane-park-av1444edge = `a02a2511` | `stream::tests::a_444_lossy_rect4_strip_stream_decodes_pixel_exact_at_odd_and_wide_geometries` | ok, 1 passed (0.15s) | `444_lossy_rect4_odd_130x122.obu` / `…_wide_256x128.obu` byte-exact vs aomdec | yes | ok |
| lane-park-av1444edge | `stream::tests::a_444_lossy_superres_random_mode_ignores_the_denominator_flag` | ok, 1 passed (5.5s) | per-frame `(denom, coded) [(11,186),(14,146),(15,137),(9,228)]`, 2265 scaled MC blocks | yes (`444_lossy_superres_random_256x128.obu`) | ok |
| lane-park-av1444edge | `stream::tests::a_444_lossy_superres_stream_decodes_pixel_exact` | ok, 1 passed (0.17s) | d12 pin | yes (`…_256x128_d12.obu`) | ok |

## Ranked findings

### F1 — lane-av1pins: two gates are `#[ignore]`d, so they do not run in a suite; the report's table says they do

`pinned_golden3_stream_decodes_pixel_exact` (stream.rs:22048) and
`pinned_sbpart_stream_decodes_pixel_exact` (stream.rs:37804) both carry
`#[ignore = "bisect aid, not a suite gate; run it with --ignored"]`. A plain
`cargo test -p ec-av1` run reports `0 passed; 1 ignored` for each. The lane report's
before/after table (`lanes/av1pins.report.md` §2) records their after-state as
**`1 passed`, 0.15s / 0.15s**, which is only true under `--ignored`; the same report's
prose says they "stay ignored". Both readings cannot be true, and the table is the one a
merge reader copies.

Both gates DO pass from a clean checkout with `--ignored`, and both pins are committed
(`golden3-pin.obu` 159 B, `sbpart-pin.obu` 238 B) and are read through the crate's own
`fixtures/` dir — the pin work landed correctly; only the run shape is off.

**Fix for the merge owner (pick one, do not do both):**
- *Preferred:* drop the two `#[ignore]` attributes in `crates/ec-av1/src/stream.rs`.
  The whole point of the lane was that a pin living only in the gitignored root made
  these gates silently green; now that the pins are committed and proven to pass in
  0.15–0.18s, un-ignoring them is what turns the commit into a gate. Then correct the
  report's table header to say they run in the suite.
- *Minimal:* keep `#[ignore]` and correct the `lanes/av1pins.report.md` §2 table to read
  `1 ignored (1 passed with --ignored)` for both rows, so the table stops over-claiming.

### F2 — the ticket's `lane-av1444edge` ref does not exist; its content is parked, and its gate names differ from lane-av1superpin's

There is no branch named `lane-av1444edge`. `a02a2511` is contained in exactly one
branch, `lane-park-av1444edge`. Its superres gate is
`a_444_lossy_superres_random_mode_ignores_the_denominator_flag`, and its pin is
`444_lossy_superres_random_256x128.obu` — **not** the
`a_444_lossy_superres_mode1_den9_…` / `…_mode2_random_denom_…` pair that
`lane-av1superpin` carries with `…_d9.obu` / `…_mode2_256x128.obu`. Any merge plan that
treats these as the same branch will either find no ref or expect gate names that do not
exist at `a02a2511` (running them there yields `0 passed; 707 filtered out`).

**Fix for the merge owner:** sequence `lane-park-av1444edge` (`a02a2511`) and
`lane-av1superpin` (`9646f74d`) as two lanes with a known overlap — both touch
`stream.rs` superres gates and both add a `444_lossy_superres_*` fixture family. If only
one is to land, say which; if both, expect a conflict in the superres gate block.

### F3 — infrastructure (not a lane): the `.cargo/config.toml` memguard runner breaks under parallel test invocations

The first wave of 13 concurrent branch runs produced spurious
`rc=1` with `Failed to start transient scope unit: Unit run-pNNNN.scope was already
loaded or has a fragment file` on 8 gates — a harness failure, not a gate failure
(the same gates pass on re-run). It is exactly the collision mode
`scripts/memguard-runner.sh` documents for its `--unit` argument, still reachable via
the `systemd-run` scope-name exhaustion when many runners start at once. This lane
worked around it with `EC_NOMEMGUARD=1`. Worth a note to whoever runs the post-merge
suite: a red `rc=1` with that message is a runner collision, and re-running is the
correct response.

### Non-findings, recorded so they are not re-litigated

- **`0 passed; N filtered out` is the fake-green signature and it appeared ZERO times
  after names were corrected.** Two naming traps bit this lane's first pass and both
  produced exactly that line with `rc=0`: (a) unit-test names carry **no crate prefix** —
  the filter is `stream::tests::name`, not `ec_av1::stream::tests::name`; (b)
  `lane-whtshape`'s four WHT tests live in a nested module —
  `transform::lossless_tx_tests::…`, not `transform::tests::…`. Anyone re-running these
  gates by hand should copy the name from `<binary> --list`, not from the report.
- **No gate printed `SKIP` anywhere in any run**, under
  `EC_AV1_REQUIRE_FFMPEG=1 EC_AV1_REQUIRE_AOMENC=1 EC_REQUIRE_FIXTURES=1`. The
  skipfix conversion (four attempt-loop gates) and the committed-pin rewiring hold in a
  clean tree.
- **Every pin the new gates read is tracked in its own branch.** The only
  `NOT-TRACKED` `.obu` strings in the diffs are encode scratch files (`out.obu`,
  `in.obu`, `ec-av1-deblocking-gate-fail.obu`) and the legacy hyphenated
  `ll444-lossless-key.obu`, which skipfix deliberately replaced with the committed
  underscore-named `ll444_lossless_key.obu` (and which lane-av1pins commits separately).
- **No absolute paths, no machine-specific dependencies.** With the root `fixtures/`
  symlink deleted, all 39 gates still pass (the 2 ignored ones under `--ignored`), so
  every one of them resolves its bytes from committed files inside the branch.

## Reproduction

```
S=$HOME/.cache/av1mergecheck/run.sh
$S <branch> <tip> stream::tests::<gate> [more gates...]
# archive + git init + fixtures symlink, build --release -p ec-av1 --lib,
# then one `<test-binary> --exact <gate>` invocation per gate, log at
# ~/.cache/wt/av1mc/<branch>.log, test-name list at ~/.cache/wt/av1mc/<branch>.list
```
