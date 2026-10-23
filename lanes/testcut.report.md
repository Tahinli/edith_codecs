# testcut: test-cost reduction report

Plan: evidence-based reduction (minimization literature + industry practice). The cost metric is wall-clock time, not count.

## Phase 0: baseline (unmodified main fa398fad, 2026-10-23)

Runs used `--release`, `EC_REQUIRE_FIXTURES=1` and libtest `--report-time`.
- ec-av1 ran on host a (`--test-threads=4`, 12G memguard, aom dir).
- The other 41 crates ran on host c; ec-hw and ec-va were excluded (GPU, local only).
- Per-test data is in `lanes/testcut/av1.tsv` and `lanes/testcut/rest.tsv` (crate, binary, test, status, exec_time_s).

| | value |
|---|---|
| timed tests | 2544 |
| summed test time | 20,197 s |
| tests < 1 s | 1850 (73%), 196 s total (1%) |
| tests 1–10 s | 382, 1,339 s |
| top 50 / 100 / 200 tests | 51% / 69% / 84% of time |
| ec-av1 share | 13,942 s (69%); lib wall about 60 min |
| next | ec-aac 1,653 s, ec-h265 1,629 s, ec-h264 1,029 s, ec-opus 356 s |

**Conclusion:** count-based reduction cannot speed up the workflow, because removing every sub-second test would save about 3 minutes of test time. The cost sits in about 200 tests. Mostly these are ec-av1 `stream::tests::*real_aomenc*`, which re-run ffmpeg and aomenc on every run (about 450 call sites, no cache, serialized by `lock_gate_counters`), and ec-av1 `encoder::tests` long sequences (the top 4 take 2,700 s).

### Failures on main (pre-existing, not testcut's)
- ec-ac3 `stack_budget::the_measured_inventory_is_accurate`: Ac3Decoder measures 3728 B, inventory says 3704.
- ec-h264 `encode::real_library_frames_encode_and_decode_exactly`: no 720p+ H.264 mp4 in the manifest on host c.
- ec-subs `real_library::a_real_pgs_stream_decodes_to_plausible_bitmaps`: display-set count assert (9).
- ec-truehd `atmos_synth::{object_substream_contents_do_not_change_the_pcm_output, synthetic_atmos_decodes_bit_exact_to_the_original_7_1}`: 7.1 remux absent on host c.
- ec-av1: 816 ok / 0 failed / 49 ignored (lib); all integration binaries ok.

## Phase 1: consolidation (no checks dropped), merged to main at 4c6db14e

Integration-test binaries were merged into one `tests/it/main.rs` per crate, following matklad 2021: each `tests/*.rs` file is a separate link. Groups of three or more tests that differed only by a constant became single table-driven tests that collect every failing case name. Byte-identical helpers were deduplicated. Some files stayed separate binaries:
- files that set env vars (`set_var` is unsafe in edition 2024)
- files that install a global allocator or panic hook
- files that hold process-global state
- files that assert wall-clock speed

| crate | integration binaries | `#[test]` fns |
|---|---|---|
| ec-vp9 | 26 → 2 | 60 → 51 |
| ec-h265 | 21 → 2 | 198 → 182 |
| ec-h264 | 12 → 2 | 166 → 165 |
| ec-aac | 11 → 4 | 95 → 95 |
| ec-mp4 | 10 → 1 | 87 → 72 |
| ec-mpegts | 9 → 1 | 58 → 58 |
| ec-riff | 7 → 1 | 45 → 45 |
| ec-ac3 | 6 → 1 | 79 → 77 |
| ec-truehd | 6 → 1 | 28 → 28 |
| ec-vp8 | 6 → 2 | 74 → 74 |
| ec-image | 5 → 3 | 105 → 99 |
| ec-av1 | – | 883 → 874 (10 process-global superblock tests run as 1 ignored suite) |
| **total** | **119 → 20** | **1878 → 1820** |

Also: root `Cargo.toml` now has `[profile.dev] debug = "line-tables-only"`.

**Verification** (`--release`, `EC_REQUIRE_FIXTURES=1`, host c):
- Every test of the 11 crates passes except the 4 baseline failures listed above.
- `superblock_size_global_suite` is ok (646 s).
- `cargo clippy --tests -D warnings` is clean on all 12 crates.

**Assertion audit:** comparing assert counts between main and the merged tree, the net removals are:
- duplicated helper copies
- per-test asserts folded into table loops
- one byte-identical duplicate test (`whole_output_digest_reads_the_first_word`, which existed in both h265 conformance files)

Count dropped by only 58 tests, as expected. The win is link and compile time (119 → 20 test binaries), not runtime. Runtime is dominated by about 200 heavy tests (Phase 0).

## Phase 1b: external-tool output cache (ec-av1), merged at df8ae5a6

Test builds of ec-av1 (`#[cfg(test)]` `src/tool_cache.rs`) now cache the output of the ffmpeg, aomenc and aomdec oracles. The cache is content-addressed: the key covers the tool binary identity (path, length, mtime), args, envs, cwd, stdin, and the contents of every existing input file.
- Only successful runs are stored, and only when every path argument existed before the run.
- Shell-script shims (`#!`) are never cached, because they read side files that the key cannot see.
- The decoder or encoder under test always runs. The cache only replaces reference generation.

Settings and coverage:
- `EC_TOOL_CACHE_DIR` sets the cache location (default `~/.cache/ec-tool-cache`, about 1.4 G); `EC_TOOL_CACHE=0` disables it.
- 203 call sites were converted, and `probe::run_with_stdin` is routed through the cache.

| ec-av1 lib, host a, `--test-threads=4` | result | wall |
|---|---|---|
| cold cache | 817 ok / 0 failed / 40 ignored | 3227 s |
| warm cache | 817 ok / 0 failed / 40 ignored | 1361 s (−58%) |

Diff check: no test or assertion removed (+1 cache unit test, +2 asserts); clippy `-D warnings` clean.

## Phase 2: evidence-gated deletion of heavy tests (2026-10-23 to 10-24)

Protocol: `lanes/testcut.phase2.md`. Each lane report has the full rows.

| lane | candidates | deleted | runtime saved | report |
|---|---|---|---|---|
| ec-h264 | 7 | 4: `every_quantiser_with_motion_matches_ffmpeg`, `every_quantiser_matches_ffmpeg`, `high_profile_8x8_matches_ffmpeg`, `x264_hi444_10bit` | 222 s | `lanes/testcut/p2-h264.md` |
| ec-h265 | 5 | 3: `pixel_pipeline_runs_over_conformance_streams`, `cu_syntax_sweep_over_conformance_streams`, `motion_derivation_is_deterministic_over_conformance_streams` | 317 s | `lanes/testcut/p2-h265.md` |
| ec-av1 stream | 98 | 0 (every candidate has unique coverage) | 0 | `lanes/testcut/p2-av1stream.md` |
| ec-av1 encoder | 38 | 0 (unique coverage) | 0 | `lanes/testcut/p2-av1enc.md` |
| ec-aac, misc crates (earlier) | – | 0 | 0 | `lanes/testcut/p2-aac.md`, `p2-misc.md` |
| **total** | | **7** | **≈539 s** | |

**Kept on evidence:**
- `x264_hi444_8bit` (K'=4).
- `corrupt_streams_never_panic` and `truncated_and_corrupted_conformance_streams_never_panic`, each killing 4 mutants the fast suite misses; they are also the only corrupt-input robustness tests.
- `ffmpeg_decodes_bit_exactly_at_every_shape` (K'=1, a hang mutant).
- `noisy_near_black_lands_within_five_percent`.

**Evidence validity.** The mutant results looked valid but were not, in four ways, and each one would have made the fast suite look like it killed everything:
1. Before 2bb4439, the cargo runner needed `.git`. In cargo-mutants scratch copies it never started.
2. The scratch copies skip the gitignored `fixtures`. Under `EC_REQUIRE_FIXTURES=1` a fixture test then failed on every mutant, and `--baseline skip` hid this.
3. `--re` selection got mangled through remote-run, ssh and `bash -c`. Mutants are now applied by index (`scripts/mutate.py`, `mutrun.py`).
4. On the 4-core servers, timeouts fired under load.

The final runs use unmutated baselines, log sampling per status, and a conservative timeout rule: a fast-suite TIMEOUT is not a kill. Any test that fails on an unmutated tree is excluded from kill attribution. These are `real_library_frames_encode_and_decode_exactly` (needs personal media) and `aom_vectors_match_per_frame_md5`.

**Caveats:**
- Samples were small (10–30 mutants per candidate), because each mutant pays a release build plus test runs.
- The h265 motion and cu_syntax deletions rest on 1 T-kill each.
- The coverage rule (unique coverage means keep) blocked every av1 deletion, even though the fast suite killed 31 of 32 heavy-killed stream mutants. A larger sample, or a fine-grained test rewrite, is the next lever there.
- Names of deleted tests in the `.config/nextest.toml` fast filter are harmless leftovers.
