# Merge wave 4 — integration report

Base `aa0ac8c2` (wave 2, "clean-checkout census"). Four branches, four
`--no-ff` merge commits, **zero conflicts** on any of them. No push.

## Per-branch table

| # | Branch | Tip merged | Merge base | True delta (base → tip) | Conflicts |
|---|--------|-----------|-----------|-------------------------|-----------|
| 1 | `lane-av1recipehunt` | `1acf1818` | `aa0ac8c2` | 9 files, +1164/-545 | none |
| 2 | `lane-av1rect8x16` | `ee56661f` | `afa13bcf` | 2 files, +493/-0 | none |
| 3 | `lane-h264reallib` | `25c996b2` | `a21f3680` | 7 files, +562/-96 | none (one auto-merged hunk, `crates/ec-h264/tests/conformance.rs`) |
| 4 | `lane-av1dumpyuv` | `60a7788a` | `afa13bcf` | 4 files, +437/-26 | none |

**True delta is measured against each branch's own merge base, not against an
older tip.** `git diff a21f3680 1acf1818` for branch 1 shows 52 files / +10975
of rebase noise; `aa0ac8c2 → 1acf1818` is the real 9-file delta. Branch 3's
merge base is also `a21f3680` (it forked before wave 2), which is why its
delta is quoted against that commit.

### 1. `lane-av1recipehunt` @ `1acf1818` — merged first

Landed first by design: it is the only branch touching `src/stream.rs` and
`src/encode.rs`, and it introduces the three fixture pins the later branches'
reports cite.

- **Three TRACKED fixture pins** (verified with `git ls-tree` at the merged
  HEAD, and `stat` on disk):

  | Fixture | Bytes | Blob sha |
  |---------|-------|----------|
  | `crates/ec-av1/fixtures/rect64_dq_drift.obu` | 8384 | `29f6e5dabcb1630baf593ea977952550a5a2bb49` |
  | `crates/ec-av1/fixtures/intra14_256x192_8bit.obu` | 35179 | `1d345e07f94c2a61b7849d88970b4ab0408f05e1` |
  | `crates/ec-av1/fixtures/intra14_256x192_10bit.obu` | 34727 | `b16114a42e4ad49c59a1e9d9839556c4caf2ddd9` |

  All three sizes match the reviewer's independent recomputation.
- `src/stream.rs` +952/-545 (the rebase-noise bulk of this branch), plus
  `src/encode.rs` +117, `src/tile.rs` +68, and the two example probes.
- **Gates: 3 un-ignored, 1 deleted** (see "Gate count" below).
- Dedupe result: **no duplicate blobs** in `crates/ec-av1/fixtures/`
  (`git ls-tree -r` grouped by blob sha — no sha appears under two names).
  Nothing to dedupe; no dedupe needed to be mentioned in a message.

### 2. `lane-av1rect8x16` @ `ee56661f`

Pure test-only `refusal_inventory.rs` +246/-0 plus its report. Zero merge
surface against wave 3 and against branch 1.

**Fixed in the merge commit:** the inventory comment at
`crates/ec-av1/src/refusal_inventory.rs:556` read *"All nine call sites"*,
which contradicts the test's own `assert_eq!(sites, 8)` at line 1642 and the
lane report. The nine is a count of **textual `decode_block_rect(` matches in
`decode.rs`**, which is 1 declaration + 8 call sites — the test skips the
declaration explicitly (`if src[..i].ends_with("fn ") { continue; }`). The
comment now states that arithmetic. One line of prose, no code change.

### 3. `lane-h264reallib` @ `25c996b2`

Real-media-library test sites reshaped to `#[ignore]` with fact-based reasons,
so a site that cannot run because ffmpeg/ffprobe or a media file is absent no
longer reads as "passed". Four follow-ups in the same tip:

- `ec-probe`: a silent skip replaced by a **named** SKIP that states what was
  missing rather than returning quietly;
- `ec-aac`: two `env::var("HOME").unwrap()` → `unwrap_or_default()`;
- `ec-h264`: one hardcoded absolute manifest path → `CARGO_MANIFEST_DIR`-relative.

> **Stale line numbers — deliberately not amended.** The branch's commit
> message cites `file:line` locations that the **same commit's** rustfmt pass
> renumbered. The citations were accurate when written and are stale at this
> tip. Recorded here and in the merge message; the message text itself is
> left alone rather than "fixed", because editing it would desynchronise the
> record of what the lane actually did.

### 4. `lane-av1dumpyuv` @ `60a7788a`

Three non-test surfaces now carry the sample depth as data:

- `crates/ec-av1/examples/dump_yuv.rs` — **derives** depth from the stream's
  own sequence header (spec 5.5.2 `color_config.bit_depth`) before a byte is
  written. Never taken from the file name, the output extension, or a
  convention. `--depth N` is accepted only as a claim, and a mismatch against
  the parsed header is a hard error naming both depths.
- `scripts/raw_to_y4m.py` — **asserts** depth as an explicit positional
  argument (`W H FPS out.y4m DEPTH`, DEPTH = bytes per sample) and validates
  the byte count is a whole number of frames at the claimed depth.
- `scripts/lr-sgr-pin-harness.c` — **takes** `bit_depth`/`highbd` as
  parameters and names the failure mode when a high-bit-depth capture is fed
  to the 8-bit arm.

> **Disclosed limit, not a silent hole.** A 10-bit stream of N frames is
> *exactly* the same byte count as 2N 8-bit frames, so **length alone can
> never distinguish them** — no length test can. That is stated in the
> `raw_to_y4m.py` module docstring, and is why the depth is an explicit
> argument rather than something inferred from the byte count.

## Gate count: 742 → 742 (net zero), cause accounted

`cargo test -p ec-av1 --lib -- --list`:

```
742 tests, 0 benchmarks
```

Wave 2's baseline was the same measure: wave 2's own report line 155 reads
`25 passed; 0 failed; 0 ignored; 0 measured; 717 filtered out` = 742. After
wave 4 the same filter reads `26 passed; … 716 filtered out` = 742.

**Net zero is arithmetic, not luck.** Wave 4 moved the lib test count in
three directions at once:

| Change | Count | Why |
|--------|-------|-----|
| `a_128_superblock_clip_whose_drl_index_the_write_time_stack_cannot_carry_decodes_exact` **deleted** | −1 | `DRL_CLAMP_HITS` is provably unreachable (its coverage claim moved to `encode.rs:15689`, the pricer's own offer-set enumeration) |
| `every_chroma_unit_decode_block_rect_can_present_has_a_coefficient_table` **added** (branch 2) | +1 | new `refusal_inventory` enumeration + witness gate |
| 3 gates **un-ignored** | ±0 | `#[ignore]`d tests still appear in `--list`; un-ignoring changes runnability, not the roster |

### The deletion and its replacement

Deleted in `4eab9e40`/`83a0066e`. The gate asserted `drl_clamp_hits() > 0` on
a real 1280x768 sb128 clip; the counter is unreachable, so the gate was a
guaranteed red once run. Its coverage claim is now carried by the pricer's
offer-set enumeration at `crates/ec-av1/src/encode.rs:15689`, whose doc
comment names the deleted gate so the lineage is greppable. The four
1280x768 `force_sb128(true)` own-reconstruction gates the reviewer verified
remain in the tree (`src/encode.rs:18653`, `:18755`, `:18852`, `:18941`, and
siblings) and justify the deletion.

`grep -rn a_128_superblock_clip_whose_drl_index crates/` now returns exactly
one hit — the `encode.rs:15691` doc comment naming it as replaced. No dangling
test reference.

### The three un-ignored gates, run

```
test stream::tests::a_real_aomenc_inter_sequence_with_an_intra_1to4_strip_decodes_pixel_exact ... ok
test stream::tests::a_real_aomenc_inter_sequence_with_an_intra_1to4_strip_decodes_pixel_exact_10bit ... ok
test stream::tests::a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_and_delta_q_decodes_pixel_exact ... ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 739 filtered out
```

All three are green **un-ignored**, not vacuously skipped — the un-ignore is
honest.

## Verification outputs

`cargo check -p ec-av1 --all-targets`, run after **each** merge, and re-run
with `src/lib.rs` + `examples/dump_yuv.rs` touched so the cached-green case
could not mask a stale binary:

```
    Checking ec-av1 v0.1.0 (/home/tahinli/Documents/Code/Rust/edith_codecs/crates/ec-av1)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.41s
```

Clean — zero errors, zero warnings.

Branch 3 touches four crates outside `ec-av1`, so the check was widened for
that merge only:

```
cargo check -p ec-av1 -p ec-aac -p ec-h264 -p ec-matroska -p ec-probe --all-targets
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.16s
```

`cargo test -p ec-av1 --lib -- gate_coverage refusal_inventory`:

```
test result: ok. 26 passed; 0 failed; 0 ignored; 0 measured; 716 filtered out; finished in 0.86s
```

Green. The +1 against wave 2's 25 is branch 2's new gate.

`cargo test -p ec-av1 --lib -- --list | wc -l` → **744** lines, of which 742
are `: test` entries; the summary line itself prints `742 tests, 0 benchmarks`.
(744 = 742 test names + the `running 0 tests` and summary lines.)

Smoke: `python3 scripts/raw_to_y4m.py` with no args prints the usage line
including the now-required `DEPTH` argument, and the module imports cleanly.

## Notes

- **NO PUSH.** The four merge commits are local on `main`.
- Wave 3 was **not** merged and `src/refusal_inventory.rs` was not edited
  beyond the one-line comment fix in merge 2.
- The pre-commit hook's whole-staged-tree `format-staged.py` produced **no**
  churn in any of the four merges: `git status --porcelain` was empty after
  each commit, and the recipehunt merge's tree is byte-identical to `1acf1818`.
  The accepted-churn escape hatch was not needed.
