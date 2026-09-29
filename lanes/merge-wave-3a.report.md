# Merge wave 3a — four branches

Integration owner: wave 3a only. Five other wave-3 branches are being reworked and
one is do-not-merge pending a one-line fix; they are **not** in this wave and were
not touched.

Base: `main` @ `9b2f6c9d` (742 lib tests). Head after this wave: `7fa70aac`
(746 lib tests). **Not pushed.**

## Per-branch

| # | branch | tip merged | conflicts | true delta vs merge base | landed delta on main |
|---|--------|-----------|-----------|--------------------------|----------------------|
| 1 | `lane-av1txsearch` | `4aebe74d` | none (auto-merged) | `gate_coverage.rs` +510/-47, `lanes/av1txsearch.report.md` +238 | `gate_coverage.rs` +463/-47, report +238 |
| 2 | `lane-av1leftwit` | `d5ab6ef4` | none (auto-merged, 3 files) | `decode_probe.rs` +9/-2, `decode.rs` +135, `stream.rs` +103/-8, report +277 | +7/-2, +119/-16, +95/-8, +277 |
| 3 | `lane-av1pinslive` | `2e5509e9` | none (auto-merged) | 4 fixtures (new blobs), `stream.rs` +246/-65, report +357 | fixtures, `stream.rs` +181/-65, report +357 |
| 4 | `lane-av1lossy128x96` | `9ecb36de` | **1 file, 2 regions** — `stream.rs` tail-append, resolved by reconstruction | `stream.rs` +188/-0, report +141 | `stream.rs` +191/-6, report +141 |

"Landed delta" is `git show --numstat <merge-commit>` — the change relative to the
accumulated pre-merge `main`, which is why it differs from the branch-vs-base
number. Both are reported; neither substitutes for the other.

### Tip drift on #3 — the dispatched sha is not on the branch

The brief dispatched `lane-av1pinslive` @ `1d438c86`. That object exists but is
**reachable from no ref and is not an ancestor of `lane-av1pinslive` or of `main`**:

```
git for-each-ref --contains 1d438c86   -> (empty)
git log --oneline -1 1d438c86          -> lanes/av1pins r2: the two pins were still #[ignore]d...
git log --oneline -1 7f815aa8          -> lanes/av1pins r2: the two pins were still #[ignore]d...   <- same subject
git diff --stat 1d438c86 7f815aa8      -> lanes/av1mergecheck.report.md | 177 +++++
```

It is the pre-amend r2 report commit, superseded by `7f815aa8`. Merging the
dispatched sha would have **deleted** `lanes/av1mergecheck.report.md` (177 lines) and
dropped r3+r4 entirely — four committed pin fixtures and 224 lines of `stream.rs`.
The branch head `2e5509e9` was merged instead.

**Unreviewed delta, flagged:** the refutation pass's "un-ignores exactly TWO gates"
describes r2. The branch head un-ignores **FIVE**:

| gate | r2 (`1d438c86`) | head (`2e5509e9`) |
|------|------------------|-------------------|
| `pinned_golden3_stream_decodes_pixel_exact` | un-ignored | un-ignored |
| `pinned_sbpart_stream_decodes_pixel_exact` | un-ignored | un-ignored |
| `pinned_golden4_stream_decodes_pixel_exact` | still ignored | un-ignored |
| `pinned_golden7_stream_decodes_pixel_exact` | still ignored | un-ignored |
| `pinned_lr_sgr_stream_call_unique_dump` | still ignored | un-ignored |

r3 additionally commits the four remaining untracked pin fixtures
(`golden4-pin.obu` 137 B, `golden6-mismatch.obu` 452 B,
`golden7-forwarding-mismatch.obu` 152 B, `lr-sgr-r7.obu` 192 B — all verified on
disk) and adds a `require_pin` helper. r4 gives `pinned_lr_sgr` the pixel asserts it
lacked. Zero gates were newly ignored. Per-gate census, base `aa0ac8c2` vs head:
5 flips `(test, ignored) -> (test, live)`, 0 flips the other way, 0 tests removed.

Per the brief, the branch was **not** pushed toward un-ignoring the three sibling
gates its report says it does not cover.

### The #4 conflict, and why `@both` was wrong

`stream.rs` conflicted in two regions, both the tail-append shape: `leftwit`,
`pinslive` and `lossy` all append `#[test] fn`s at the end of the same test module,
and git hoisted the shared trailing lines (the `let _gate_lock` / `have_ffmpeg()`
prologue, and the module's closing brace) out of the hunks as common context.

`@both` would have emitted **ours' gate head, then theirs' gate head, then the one
shared body** — nesting the second `fn` inside the first. That parses, so no
`cargo check` error would fire, but the outer gate would have an empty body and a
meaningless `#[test]` on a nested item: a silently vacuous gate.

Resolved by reconstruction instead (tail-append by construction, not marker
surgery): HEAD's `stream.rs` with the module's final `}` dropped, the lane's
single 188-line contiguous append spliced in, `}` re-added. Verified afterwards:

- `git diff --numstat HEAD -- crates/ec-av1/src/stream.rs` = `189 0` — a pure
  append, nothing else moved;
- both gates present exactly once
  (`a_real_aomenc_12bit_stream_with_two_tile_rows_and_a_two_by_two_tile_grid_decodes_pixel_exact`,
  `a_real_aomenc_444_whole_64_root_tx_size_search_stream_decodes_pixel_exact_at_128x96`);
- the flagged behaviour survives: the 128x96 gate asserts **parsed** `tx_mode` per
  arm — `TxMode::Select` on the search arm, `TxMode::Largest` on the control.

## Cross-branch defect found by the merge, and fixed here

Branches 1 and 4 were each individually green and together red.

`lane-av1txsearch` landed `print_tx_size_search_census`, whose `assert_eq!(unresolvable, 0)`
exists to go red the moment a gate body builds `--enable-tx-size-search` in a shape
the resolver cannot bind. `lane-av1lossy128x96` landed a gate written exactly that
way — `for (label, search, want_mode, want_len, want_fnv) in arms`, with the arm
table hoisted into `let arms`. The assertion fired, as designed:

```
assertion `left == right` failed: 1 gate bodies build --enable-tx-size-search into
a variable this census cannot bind
  left: 1   right: 0
```

`bound_values` strategy (3) only understood an array spelled **inline** after `in`.
With a named collection it read past the `for` line to the next `[` in the body —
the gate's unrelated `&[..]` flag list — found no digits, and returned an empty set.
Both arms were in the source the whole time.

Fixed in `7fa70aac` (integration-authored, not folded into either lane's merge
commit): new strategy (3b) follows the `for` line's collection name back to its
`let` binding and takes that column. Two details are load-bearing — a word-boundary
guard (`let arms_foo` must not answer for `let arms`), and a hop over `=`, because
the binding's **type annotation is itself bracketed**
(`[(&str, &str, TxMode, usize, u64); 2]`), so scanning for the first `[` after the
name returns the type.

**Red-before:** with `merge(set)` replaced by `let _ = set;` the new case added to
`the_resolver_binds_a_format_built_flag_in_both_directions` fails with
`a_real_aomenc_444_whole_64_root_tx_size_search_stream_decodes_pixel_exact_at_128x96:
the unmutated body should resolve to both values, got {}`. Restored → green.

## Checks

`cargo check -p ec-av1 --all-targets` after each merge — clean, no warnings, four for
four:

| after | result |
|-------|--------|
| merge 1 `1cc2f250` | Finished, 1.73s |
| merge 2 `bd588822` | Finished, 16.90s |
| merge 3 `4532caa6` | Finished, 10.14s |
| merge 4 `8dd07ed7` (post-resolution) | Finished, 3.77s |

Final, after `7fa70aac`:

```
$ cargo test -p ec-av1 --lib -- gate_coverage refusal_inventory
test result: ok. 28 passed; 0 failed; 0 ignored; 0 measured; 718 filtered out

$ cargo test -p ec-av1 --lib -- --list | wc -l
748
```

Census line, proving the resolver now binds the new gate:

```
a_real_aomenc_444_whole_64_root_tx_size_search_..._at_128x96: =0 and =1 (one arm each) [format! template resolved]
enable-tx-size-search over 259 census-selected gate bodies: =0 only 88, =1 only 13, both 20, unresolvable 0, not named 138
```

### Toolchain caveat that cost three wrong readings

`$HOME/.cache/cargo-target` is **shared with concurrent sibling agents on this
machine**, and it is keyed by package name+version, not by source path. Three
successive `cargo test` runs on an unchanged tree reported 746, then 715 (with
`refusal_inventory::tests::every_chroma_unit_a_64_axis_strip_can_present_has_a_coefficient_table`
failing "the refusal string is gone from `decode_block_rect64`" — a string that is
present at line 19003, inside that function). A sibling worktree built into the
same directory had overwritten the test binary. Every number in this report was
re-measured in a dedicated `CARGO_TARGET_DIR=cargo-target-w3a-final`; the baseline
was measured in a **second** dedicated dir, because sharing even a private dir
across two source trees reproduces the same corruption.

## Gate-count delta and its cause

| | lib tests | `wc -l` |
|---|---|---|
| `main` @ `9b2f6c9d` (pre-wave) | 742 | 744 |
| after wave 3a | 746 | 748 |
| **delta** | **+4** | **+4** |

Name-set diff, both directions (`comm`), base `9b2f6c9d` vs `7fa70aac`:

```
ADDED (+4)                                REMOVED: none
  gate_coverage::tests::print_tx_size_search_census                    <- txsearch
  gate_coverage::tests::the_resolver_binds_a_format_built_flag_...      <- txsearch
  stream::tests::a_committed_4to20_stream_presents_a_one_mi_left_...    <- leftwit
  stream::tests::a_real_aomenc_444_whole_64_root_tx_size_search_..._128x96 <- lossy128x96
```

Cause, per branch: **txsearch +2** (the census and its resolver test), **leftwit +1**,
**lossy128x96 +1**, **pinslive +0** — pinslive only removes `#[ignore]` attributes
and adds a `require_pin` helper, so it moves the *live* count, not the *listed*
count. Its five gates went from listed-and-ignored to listed-and-run, which is why
the suite executes strictly more work at the same 746.

## Deliberately NOT touched

- **No push.** All five commits sit on local `main`.
- The five wave-3 branches under rework, and the do-not-merge one — not fetched for
  merge, not touched.
- `crates/ec-av1/fixtures/**` — the four `.obu` blobs are pinslive's, landed
  byte-for-byte by the merge; no fixture was regenerated, re-encoded, or renamed.
- `lanes/av1mergecheck.report.md` — present (16153 B) and untouched. r2 deleted it
  and r3 restored it; merging the branch head leaves it exactly as `main` had it.
- `lanes/av1pins.report.md` — the lane's own report, not corrected toward the brief
  even where the brief and the report disagree.
- `stash@{0}` (a foreign `EC_PREDOUT8` edit to `scripts/instrument-aom-oracle.sh`,
  attributed to `lane-av1cmpaudit`, preserved by Emre-2) — left in the stash, not
  applied. Same for `stash@{1..3}`.
- `crates/ec-av1/src/stream.rs` in the lossy merge — no whole-file `--theirs`; main's
  `a_real_aomenc_inter_sequence_with_an_intra_1to4_strip_decodes_pixel_exact_10bit`
  (a main-side test the branch does not have) survives.
- No `rustfmt`, no workspace-wide build, no other crate's tests.
