# lane-av1clipprobe — the clip-gate silent skip, and the two roots

Branch `lane-av1clipprobe` off `f33b9d41`. Commit `502c18ce`.

## 1. Reproduction (before the fix)

Lane worktree `/home/tahinli/Documents/Code/Rust/wt-av1clipprobe`, root `fixtures/`
absent (it is `.gitignore:2`, so `git worktree add` never produces it):

```
$ ls -d fixtures
ls: cannot access 'fixtures': No such file or directory

$ EC_AV1_REQUIRE_FFMPEG=1 EC_AV1_REQUIRE_AOMENC=1 \
  cargo test -p ec-av1 --release \
    a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders -- --nocapture
running 1 test
SKIP the 1080p tile round trip: no fixture
test encoder::tests::a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 752 filtered out; finished in 0.11s
EXIT=0
```

Both halves of the report: the `SKIP` line, and libtest's `ok`. The two
`EC_AV1_REQUIRE_*` flags were set for that run, and the gate still reported
green — the flags govern *ffmpeg/aomenc* absence, never the clip's.

## 2. Route

`crates/ec-av1/src/library_fixture.rs` (new, `#[cfg(test)] mod`) owns two things.

**The resolver** — `root()` reads `EC_FIXTURES` first, else
`CARGO_MANIFEST_DIR/../../fixtures`.

**The one presence probe** — `require(rel, what)` / `require_at(path, what)`:

* file present → `Some(path)`;
* absent **and** `EC_REQUIRE_FIXTURES` / `EC_AV1_REQUIRE_FFMPEG` /
  `EC_AV1_REQUIRE_AOMENC` set → `assert!` panics, naming the resolved path and
  the three ways to fix the tree (RED);
* absent and no require env → prints **one** `SKIP` line naming the resolved
  path and the same three ways out, returns `None` (documented skip).

This is the crate's *existing* shape, not a new one: the
assert-under-require / print-one-line-otherwise probe is what
`crates/ec-h264/tests/conformance.rs::require_fixtures` and the per-crate
`require_fixture` helpers do, and the file's own presence check lives inside
the probe that owns it — the shape the `no_tool_presence_check_outside_its_probe`
scan in `gate_coverage.rs` demands. No bare `is_file()` / `exists()` was added at
any call site.

`h264_clip_frames` already had the right probe shape for ffmpeg but its CALLERS
owned the absence silently; the fix is in the callers plus the one added clip
check. `h264_clip_frames` now takes the calling gate's name (`what`) so both of
its exits print exactly one line and callers `return` on `None` without printing
a second — the two "no fixture" lines per gate were the noise that hid this.

## 3. Class sweep

Every site in `crates/ec-av1` that resolved a fixture through
`CARGO_MANIFEST_DIR/../../fixtures` (i.e. outside the crate-local committed
pins, which `crate_pin` already handles with an unconditional `panic!`).
14 sites, 5 in `encoder.rs`, 8 in `encode.rs`, 2 in `stream.rs` — of which
**11 could report a green suite having asserted nothing** (non-`#[ignore]`d
`#[test]`).

| # | site (file:line at `f33b9d41`) | gate | mechanism | status |
|---|---|---|---|---|
| 1 | `encoder.rs:3263` in `h264_clip_frames` | 10 callers (below) | `clip.exists()` → `None` → each caller's own `SKIP` → `return`; **no env escape** | probe; one line per gate |
| 2 | `encoder.rs:6119` | `a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders` | via (1) | **RED under require** |
| 3 | `encoder.rs:3316` | `bytes_per_frame_target_settles_within_20_percent` | via (1) | **RED** |
| 4 | `encoder.rs:3367` | `bitrate_target_lands_within_5_percent_over_48_frames` | via (1) | **RED** |
| 5 | `encoder.rs:3460` | `bytes_per_frame_controller_never_oscillates_past_its_clamp` | via (1) | **RED** |
| 6 | `encoder.rs:5576` | `tile_wall_table_at_1080p` (`#[ignore]`, probe) | via (1) | probe |
| 7 | `encoder.rs:5820` | `filter_replay_final_matches_the_capture_decode` | via (1) | **RED** |
| 8 | `encoder.rs:5872` | `filter_stage_wall_1080p` (`#[ignore]`, probe) | via (1) | probe |
| 9 | `encoder.rs:5944` | `tile_search_wall_1080p` (`#[ignore]`, probe) | via (1) | probe |
| 10 | `encoder.rs:6179` | `quality_target_is_monotone_in_bytes_and_psnr` | via (1) | **RED** |
| 11 | `encoder.rs:2881` | `the_facade_codes_the_same_bytes_as_encode_sequence` | `unwrap_or_else` → **runs the film arm on a synthetic card** and asserts as if it were film | **RED**; the substitution now says so on a second, non-`SKIP` line |
| 12 | `encoder.rs:6042` in `rav1e_wall_reference` | 2 wall-table probes | `!has_rav1e \|\| !clip.exists()` behind ONE line — the clip's absence hid inside a tool-absence message and had no escape | split; clip through the probe |
| 13 | `encode.rs:20345` | `real_clip_encodes_within_its_quality_and_size_budget` | `clip.exists()` → `SKIP` | **RED** |
| 14 | `encode.rs:20410` | `…_at_a_straddle_size` | `clip.exists()` → `SKIP` | **RED** |
| 15 | `encode.rs:20913` | `the_encoders_own_streams_are_byte_identical_to_their_pins` | `clip.exists()` → `SKIP` — compared nothing at all | **RED** |
| 16 | `encode.rs:20494` | `calibration_sweep_base_q_idx` (`#[ignore]`, probe) | per-clip `!path.exists()` → `continue`; with every clip absent the loop ran zero times and **passed** | probe through the probe |
| 17 | `encode.rs:21561`, `:22113`, `:22672` | `bd_rate_vs_libaom_and_rav1e`, `native_gate_clips`, `tpl_intra_denominator_histogram` (all `#[ignore]`) | per-clip `!path.exists()` → drop/`continue` with no escape; `tpl` printed nothing and passed | per-clip checks through the probe; roots re-resolved |
| 18 | `encode.rs:16846`, `:19735`, `:21179` | `probe_intrabc_key_frame`, `pricer_error_census_on_clips`, `probe_screen_library` (all `#[ignore]`) | `read_to_string(…/real-library-manifest.tsv)` `Err` → `SKIP` naming no path | manifest through the probe; a present-but-unreadable manifest is now a hard error, not a second silent skip |
| 19 | `stream.rs:6296` | `a_libaom_stream_with_128_intra_blocks_in_inter_frames_decodes_exact` | `clip.exists()` → `SKIP` — decoded nothing | **RED** |
| 20 | `stream.rs:45096` | `a_straddling_frame_decodes_exactly_on_both_reference_encoders_ladders` | `clip.exists()` → `return` **from the middle of the gate**: everything above it had already run, the encoder-ladder half never did, and a half-executed gate reported green | **RED** |

Left alone on purpose:

* `stream.rs:9426 pin_dir()` — names `../../fixtures` as a *fallback* for pins
  that are committed crate-local; the gate above it (`require_pin`) panics
  unconditionally on absence, so there is no skip to close, and the preflight's
  invariant-4 census classifies gates by that root literal.
* All 44 `crate_pin` sites — already the lane-av1skipfix shape: an absent
  COMMITTED pin is a repo defect and fails with no escape.

Class-wide proof, root absent, **without** require envs — every gate prints
exactly one `SKIP` line:

```
a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders   skip_lines=1  ok. 1 passed
bytes_per_frame_target_settles_within_20_percent                       skip_lines=1  ok. 1 passed
bitrate_target_lands_within_5_percent_over_48_frames                   skip_lines=1  ok. 1 passed
bytes_per_frame_controller_never_oscillates_past_its_clamp             skip_lines=1  ok. 1 passed
quality_target_is_monotone_in_bytes_and_psnr                          skip_lines=1  ok. 1 passed
the_facade_codes_the_same_bytes_as_encode_sequence                     skip_lines=1  ok. 1 passed
filter_replay_final_matches_the_capture_decode                        skip_lines=1  ok. 1 passed
real_clip_encodes_within_its_quality_and_size_budget                   skip_lines=2  ok. 2 passed   (filter matched both straddle tests; 1 each)
real_clip_encodes_within_its_quality_and_size_budget_at_a_straddle_size skip_lines=1  ok. 1 passed
the_encoders_own_streams_are_byte_identical_to_their_pins              skip_lines=1  ok. 1 passed
a_libaom_stream_with_128_intra_blocks_in_inter_frames_decodes_exact    skip_lines=1  ok. 1 passed
a_straddling_frame_decodes_exactly_on_both_reference_encoders_ladders  skip_lines=1  ok. 1 passed
tile_wall_table_at_1080p / tile_search_wall_1080p / filter_stage_wall_1080p   skip_lines=0  1 ignored (probes, #[ignore]d)
```

Class-wide proof, root absent, `EC_AV1_REQUIRE_FFMPEG=1 EC_AV1_REQUIRE_AOMENC=1`
— **all eleven are RED**:

```
a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders   FAILED. 0 passed
bytes_per_frame_target_settles_within_20_percent                       FAILED. 0 passed
bitrate_target_lands_within_5_percent_over_48_frames                   FAILED. 0 passed
bytes_per_frame_controller_never_oscillates_past_its_clamp             FAILED. 0 passed
quality_target_is_monotone_in_bytes_and_psnr                          FAILED. 0 passed
the_facade_codes_the_same_bytes_as_encode_sequence                     FAILED. 0 passed
filter_replay_final_matches_the_capture_decode                        FAILED. 0 passed
real_clip_encodes_within_its_quality_and_size_budget                   FAILED. 0 passed
the_encoders_own_streams_are_byte_identical_to_their_pins              FAILED. 0 passed
a_libaom_stream_with_128_intra_blocks_in_inter_frames_decodes_exact    FAILED. 0 passed
a_straddling_frame_decodes_exactly_on_both_reference_encoders_ladders  FAILED. 0 passed
```

`EC_REQUIRE_FIXTURES=1` alone is enough as well (proved on the 1080p gate).

## 4. Reconciliation: the clip path resolves through `EC_FIXTURES`

**Choice: `library_fixture::root()` honours `EC_FIXTURES` first**, the
alternative being to make the preflight FAIL when the tests' default root is
absent.

Why this one:

* `EC_FIXTURES` is already the fleet-wide convention — `gen-fixtures.sh`,
  `fetch-vectors.sh`, `scan-real-library.sh`, `gen-fixture-library.sh` and
  `verify-fixture-library.sh` all take it, and every generator row in the
  manifest is resolved through it. A crate reading `$ROOT/fixtures`
  unconditionally was the odd one out, and that is the whole of the mismatch.
* The preflight cannot fix its own side of it. Failing whenever
  `$ROOT/fixtures` is absent would RED every properly-staged runner that points
  `EC_FIXTURES` at a library elsewhere — a false red, and a batch that cannot
  start.
* One direction of the fix makes the two sides *identical by construction*
  rather than keeping two roots in agreement by convention. The bad case
  (preflight GREEN over a root the gates do not read) becomes unrepresentable.

`EC_AV1_WALL_CLIP` still overrides with a path outside any root, which is why
`require_at` exists alongside `require`.

One preflight change beyond the resolver: its last line no longer says a bare
`GREEN` when mode (i) was reported — it says `GREEN-WITH-FINDINGS` and how to
make it fatal. A reader who read only the verdict was reading GREEN over a tree
whose fixture root does not exist.

`scripts/gen-fixture-library.sh` learned the resolver's **library-relative**
literal (a `library_fixture::require("video/…", …)` argument). Without it the
`RESOLVE` pass silently lost every clip row the moment a gate stopped spelling
`../../fixtures` — the preflight would have stopped checking the exact paths
whose absence this lane is about. The rule is scoped to lines that call the
resolver, so it adds no rows outside ec-av1. The manifest's row **set** is
unchanged (`fixtures/video`, both clips, `real-library-manifest.tsv` all still
present); only `required_by` line attributions moved, which is why
`scripts/fixture-library.tsv` is regenerated in this commit.

### Four cases, raw output

Gate: `a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders`.

**1 — root present, `EC_FIXTURES` unset** (symlink from
`scripts/link-fixtures.sh`; the clip is 3320956 B):

```
1080p 2x1 tiles: 84488 bytes, sample-exact
1080p 2x2 tiles: 84208 bytes, sample-exact
1080p 4x2 tiles: 84685 bytes, sample-exact
test …a_1080p_multi_tile_stream… ... ok
test result: ok. 1 passed; 0 failed; … finished in 28.77s
```

**2 — root ABSENT, `EC_FIXTURES` set** (was the false green; the gate now runs
on the very tree the preflight validated):

```
$ EC_FIXTURES=$LIB EC_AV1_REQUIRE_FFMPEG=1 cargo test … a_1080p_multi_tile_stream…
1080p 2x1 tiles: 84488 bytes, sample-exact
1080p 2x2 tiles: 84208 bytes, sample-exact
1080p 4x2 tiles: 84685 bytes, sample-exact
test result: ok. 1 passed; 0 failed; … finished in 28.73s
```

**3 — root absent, `EC_FIXTURES` unset, no require env** — the documented skip,
one line:

```
SKIP a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders:
     h264-1080p-23.976-8bit.mp4 absent at
     /…/wt-av1clipprobe/crates/ec-av1/../../fixtures/video/h264-1080p-23.976-8bit.mp4
     -- this gate proved nothing; run scripts/link-fixtures.sh, point EC_FIXTURES
     at a library root, or set EC_REQUIRE_FIXTURES=1 / EC_AV1_REQUIRE_FFMPEG=1 to
     make this a failure.
test result: ok. 1 passed; 0 failed; … finished in 0.05s
```

**4 — both absent, require env set** — RED:

```
thread '…a_1080p_multi_tile_stream…' panicked at crates/ec-av1/src/library_fixture.rs:107:5:
a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders: the library clip is
absent at /…/wt-av1clipprobe/crates/ec-av1/../../fixtures/video/h264-1080p-23.976-8bit.mp4
-- this gate would prove nothing. A linked worktree has no gitignored root `fixtures/`:
run scripts/link-fixtures.sh, or point EC_FIXTURES at a library root, or set
EC_REQUIRE_FIXTURES=1 / EC_AV1_REQUIRE_FFMPEG=1 only on a tree that really has the library.
test result: FAILED. 0 passed; 1 failed; … finished in 0.05s
```

A fifth, adversarial: `EC_FIXTURES` pointed at `$LIB/video` (one level too
deep) with `EC_REQUIRE_FIXTURES=1` — resolves to `…/fixtures/video/video/…`
and is RED, not a skip:

```
a_1080p_…: the library clip is absent at /…/fixtures/video/video/h264-1080p-23.976-8bit.mp4 …
test result: FAILED. 0 passed; 1 failed;
```

**GREEN-with-absent-root is dead** in both directions:

| | preflight before | preflight after | gate before | gate after |
|---|---|---|---|---|
| root absent + `EC_FIXTURES` set | `GREEN (305 rows)`, `resolve 0 missing` — over a root no gate read | `GREEN (295 rows)`, and the header prints `the clip gates resolve $LIB too` | `SKIP … no fixture`, `ok` | **runs**, sample-exact, or RED under require |
| root absent, `EC_FIXTURES` unset | mode (i) reported, then a bare `GREEN` last | `GREEN-WITH-FINDINGS` (or `RED` under `EC_REQUIRE_FIXTURES=1`) | `SKIP`, `ok` | one `SKIP` line, `ok`; **RED** under any require env |
| root present | `GREEN` | `GREEN (295 rows)` | runs | runs |

## 5. Non-vacuity and gate results

* Root absent + require envs → **RED** (case 4, and all eleven gates in §3).
* Root absent, no require envs → **exactly one** `SKIP` line per gate naming the
  resolved path and the escape hatch (§3 table).
* Root present → the 1080p gate still **passes**:
  `1080p 2x1 84488 / 2x2 84208 / 4x2 84685 bytes, sample-exact`, `ok. 1 passed`
  in 28.8 s release — and it passes with `EC_AV1_REQUIRE_FFMPEG=1
  EC_AV1_REQUIRE_AOMENC=1` set, so the new failure path is not armed on a good
  tree.
* `real_clip_encodes_within_its_quality_and_size_budget` (+ its straddle
  sibling), root present: `ok. 2 passed` in 6.2 s.
* The crate's own anti-regression scans, all green:
  `gate_coverage` 13 passed (incl. `no_tool_presence_check_outside_its_probe`),
  `refusal_inventory` 19 passed, `library_fixture` 1 passed.
* Preflight, root present: `GREEN (295 rows)`, `code-shape violations: 0`,
  `pin gates: total=8 committed=21 uncommitted=0 ignored=0 assertless=0`,
  invariant-1 positive control fired, invariant-4 census self-test passed.
  Under `EC_REQUIRE_FIXTURES=1` with the root absent: exit 1,
  `FAIL [mode i]`, `RED`.

New test: `library_fixture::tests::the_resolver_follows_ec_fixtures_over_the_gitignored_root`
asserts the precedence itself, so the reconciliation is a check rather than a
sentence in a comment.

## Notes for the merge owner

* `scripts/fixture-library.tsv` is regenerated. `required_by` line attributions
  moved because the edits shifted lines; the row **set** is identical, verified
  by `diff <(cut -f1,3 | sort -u)` against the pre-change manifest.
* The lane worktree needs `scripts/link-fixtures.sh` (or `EC_FIXTURES`) before
  the clip gates can do anything; a `git archive` staging excludes the
  gitignored root `fixtures/`, which is the staging gap the wave-3c-b owner
  measured. After this commit that staging is no longer silent: the clip gates
  are RED under the require envs instead of green.
