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
