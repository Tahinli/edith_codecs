# Merge wave 3e — pins5, txsizeaudit (chromahalvings HELD)

Integration owner run. Pre-wave `main` = `ec4e9528` (wave-3d, section 8).
Nothing pushed. Lane-private `CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/merge3e`,
`EC_NOMEMGUARD=1` on every invocation.

## 1. Per-branch table

| # | branch | merge-base | tip merged | merge commit | merge-tree | conflicts | `cargo check -p ec-av1 --all-targets` |
|---|---|---|---|---|---|---|---|
| 1 | `lane-av1pins5` | `ec4e9528` (== pre-wave main) | `e1f78b8e` | `7ec417ad` | clean (`dab8a3a8`) | none | Finished, 0 warnings |
| 2 | `lane-av1txsizeaudit-r5` | `ec4e9528` | `c0727d64` | `5f39325d` | clean (`399797dc`) | none | Finished, 0 warnings |
| 3 | `lane-chromahalvings` | `f33b9d41` (wave-3c-b, **stale**) | `69e80133` | — | — | — | — |
| — | `lane-chromahalvings-r3` | — | `149b2ad8` | already an ancestor of `main` | — | — | — |

Branch 3 is **deferred(next wave)**, not merged. `69e80133`'s own message is
"4 sites wrong at 4:4:4, all reachable, **none fixed here**" — the fix Main is
waiting on from Volkan-2 (`decode_intrabc_owned_rect`'s extent plus its ten
`px/2, py/2` prediction origins, with a gate separating the two halves) is not
on the branch yet, so there is nothing to land and no region to resolve. Its
base is also five merges behind, so it needs a re-rebase before it is a merge
candidate at all. `lane-chromahalvings-r3` is already in `main`
(`git merge-base main lane-chromahalvings-r3` == its own tip), so there is
nothing to fetch from it either. When it lands, the region to watch is
`decode_intrabc_owned_rect`'s `bw/2, bh/2` (verified untouched by txsizeaudit
above) plus whatever it does to the ten `px/2, py/2` prediction origins in the
same function.

**Counts are per module, not one combined filter.** Running the four guard
modules as a single `cargo test -- gate_coverage refusal_inventory
count_vacuity pin_inventory` reports fewer than the sum of the four
per-module runs, because libtest's multiple positional patterns are a union of
substrings over test NAMES, not a set of module selectors — a test whose name
matches two of the four is counted once. The per-module numbers in §5 and §6
(18 + 19 + 2 + 3 = 42 before merge #2, 18 + 20 + 2 + 3 = 43 after) are the
ones to quote; the combined figure is a union, not a sum.

## 2. Conflicts and how each was resolved

**Zero conflicts in both merges.** Both `git merge-tree --write-tree`
predictions returned a bare tree id with no conflict list, and both real
`--no-ff` merges auto-resolved with zero hunks (`Auto-merging` on
`stream.rs` for #2, no `CONFLICT` line).

The `stream.rs` contention Main predicted for merge #2 did not materialise: the
branch's `stream.rs` delta is +192 and 0 removals, appended in a region pins5
does not touch, and pins5's own `stream.rs` delta is +98/−8 in a different
region. Both survive:

```
$ grep -c frame_count_diagnosis crates/ec-av1/src/stream.rs     # pins5
4
$ grep -c ENCODED_FRAMES     crates/ec-av1/src/stream.rs       # pins5
4
$ git diff 7ec417ad 5f39325d -- crates/ec-av1/src/gate_coverage.rs | wc -l   # pins5's +779 file
0
```

Because a clean auto-merge is not evidence of a clean merge, `cargo check -p
ec-av1 --all-targets` ran after EACH merge with `src/lib.rs` touched first so
the check could not serve a cache. Both `Finished`, 0 warnings.

`decode_intrabc_owned_rect`'s `bw/2, bh/2` line was **not** pre-resolved. The
txsizeaudit delta is 305 lines in `suppress_internal_lf_edges` and the
intra-in-inter walk; neither its hunks nor `merge-tree` put a hunk near that
line, and the region is untouched in the merged tree — left free for
`lane-chromahalvings`' witness to land on.

## 3. Shape checks (the two failure modes Main named)

**Foreign files.** For each merge, the diff against the first parent was
compared byte-for-byte against the branch's own delta, not read:

```
$ git diff ec4e9528 7ec417ad > /tmp/3e_merge.diff
$ git diff ec4e9528 e1f78b8e > /tmp/3e_branch.diff
$ cmp /tmp/3e_merge.diff /tmp/3e_branch.diff
IDENTICAL -- no silent line restoration, no foreign file
  crates/ec-av1/src/gate_coverage.rs | 779 +++++++++++++++++++++++++++++
  crates/ec-av1/src/stream.rs        |  98 ++++-
  lanes/av1pins.report.md            | 512 ++++++++++++++++++++++++++++++
  3 files changed, 1381 insertions(+), 8 deletions(-)
```

```
$ git diff --stat 7ec417ad 5f39325d
 crates/ec-av1/examples/decode_probe.rs |    5 +
 crates/ec-av1/src/decode.rs            |  305 ++++++---
 crates/ec-av1/src/refusal_inventory.rs |   66 ++
 crates/ec-av1/src/stream.rs            |  192 ++++++
 lanes/av1ibc128arm.report.md           |  196 ++++++
 lanes/av1tilerows.report.md            |   38 +-
 lanes/av1txsizeaudit.report.md         | 1097 ++++++++++++++++++++++++++++
 7 files changed, 1796 insertions(+), 103 deletions(-)
```

**Exactly** the declared set: 7 files, 1796 insertions, 103 deletions. The two
foreign reports (`av1ibc128arm.report.md` +196, `av1tilerows.report.md` ±38)
are inside the declared scope, as Main stated — `av1ibc128arm`'s because the
intra-in-inter fix `4bfe8d8e` has never been merged and lands with this branch;
`av1tilerows.report.md`'s because its 38 lines are that lane's own edit, not a
swallowed file.

**Foreign files, the strongest form of the check.** For each merge the diff
against the first parent is compared BYTE-FOR-BYTE against the branch's own
delta with `cmp`, not read with `--stat` and eyeballed. `git diff --stat` alone
proves the file SET; it cannot prove the hunks, so a 3-way apply that silently
restored a removed line would still print the same stat. `cmp` on the two
diffs is what actually rules that out, and it is cheap.

**Silent line restoration.** Removal counts, per hot file, merge against
branch:


| file | merge `grep -c '^-[^-]'` | branch | verdict |
|---|---|---|---|
| `gate_coverage.rs` (merge #1) | 0 | 0 | match |
| `stream.rs` (merge #1) | 8 | 8 | match |
| `decode.rs` (merge #2) | 97 | 97 | match |
| `stream.rs` (merge #2) | 0 | 0 | match |
| `refusal_inventory.rs` (merge #2) | 0 | 0 | match |

No region where the merge removed fewer lines than the branch intended. The 97
`decode.rs` removals are the branch's own: the collapsed 4:2:0 arm
(`luma_span = cu_tx * 2`, `cu_around = neighbours.around_mi(...)`, the
`tu_reach` argument list) plus the `4bfe8d8e` intra-in-inter fix. The two
diffs are not byte-identical for merge #2, and the difference is entirely hunk
ordering and context, because the first parent now carries pins5's `stream.rs`
and `gate_coverage.rs`; the `--stat` and the per-file removal counts above are
the shape evidence.

**Behavioural scope of the 305-line `decode.rs` change.** The removals are not
only C1; they include the collapsed 4:2:0 arm of the intra-in-inter walk that
`4bfe8d8e` rewrote. The scoped family that covers that route, with the oracle
forced on, is green on the merged tree:

```
$ EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -- intra_in_inter interintra
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 755 filtered out; finished in 27.77s
```

7/0, including the new 4:4:4 128-root per-mu-chunk gate and the 4:2:0
`a_real_aomenc_stream_with_interintra_decodes_pixel_exact` that the collapsed arm
used to serve. Not a substitute for the full suite, which is Main's, but it is
the family that would red first if the arm had been dropped instead of
rewritten.

## 4. A ref hazard found and NOT walked into

`lane-av1txsizeaudit` — the ref name — does **not** point at the re-verified
work:

```
$ git for-each-ref --format='%(refname:short) %(objectname:short)' | grep txsizeaudit
lane-av1txsizeaudit     dc80b6fa
lane-av1txsizeaudit-r5  c0727d64
```

`dc80b6fa` is not a descendant of `c0727d64`; it is a whole history line behind,
based on `9b2f6c9d`, and `git diff c0727d64..dc80b6fa` is **67 files,
+627/−13617** — it deletes every committed pin fixture. Merging the ref NAME
would have produced an add/add conflict in
`lanes/av1txsizeaudit.report.md` and landed an ancient `decode.rs`:

```
$ git merge-tree --write-tree --name-only main dc80b6fa
8422378dac55a358d6e8150178c228f9c936fe65
lanes/av1txsizeaudit.report.md
CONFLICT (add/add): Merge conflict in lanes/av1txsizeaudit.report.md
```

The worktree `~/.cache/wt/av1txsizeaudit` is checked out on
`lane-av1txsizeaudit-r5` at `c0727d64`, so the tip in the worktree and the tip
in the ref disagree. I merged the **sha** after checking both. Reported to Main;
the ref wants repointing or deleting before any later wave merges it by name.


**Resolved, not left as a hazard.** Main repointed the ref after this was
reported (`git branch -f lane-av1txsizeaudit c0727d64`), so
`lane-av1txsizeaudit` is now an ancestor of `main` and a later wave merging
that NAME gets the correct tree. Recorded because the failure mode is silent
until it is not: a ref that lags a rebase by a whole history line still
resolves, still merges, and produces a plausible-looking commit.

## 5. Branch 1 — `lane-av1pins5`

Five new CI tests, one gate fix at `stream.rs:3895`, the misattributed-red
diagnosis in all three `ffmpeg_decode_sequence*` helpers, the generalised
count-vacuity guard, and the 43-row ledger.

Guard families on the merged tree, `gate_coverage.rs` / `stream.rs` /
`refusal_inventory.rs` touched first because all four read compile-time sources:

```
$ cargo test -p ec-av1 --lib -- gate_coverage
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 742 filtered out
$ cargo test -p ec-av1 --lib -- refusal_inventory
test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 741 filtered out
$ cargo test -p ec-av1 --lib -- count_vacuity
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 758 filtered out
$ cargo test -p ec-av1 --lib -- pin_inventory
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 757 filtered out
```

18 / 19 / 2 / 3 — the author's four modules, reproduced on the merged tree.

The sweep reads the numbers Main flagged, and a reviewer comparing totals should
read the second one:

```
$ cargo test -p ec-av1 --lib -- count_vacuity --nocapture
count-vacuity sweep: 60 site(s); 17 spec-pinned by a prior assert, 43 not
```

60 grand total (down from 61, one site main made spec-pinned), **43 not
spec-pinned**, against the guard's unpinned ceiling 43 and total floor 40. The
43 rows are disposed `deferred(needs a lane that classifies by READING each
gate)` — carried forward, not closed here.

## 6. Branch 2 — `lane-av1txsizeaudit-r5`

Dispositions kept as declared:

- **C1 landed** — `suppress_internal_lf_edges` publishes the skipped 128 root's
  own per-axis chroma extent, `((w_mi * MI) >> ss_x(fctx))` /
  `((h_mi * MI) >> ss_y(fctx))`, replacing the hardcoded `/2`. No observable
  effect: the widening is defeated by the spec-7.14.2 `skip_at` term and the
  method is only ever called for an already-skipped block, so the masking is
  total. That is precisely why it is pinned by a source scan and not by pixels.
- **C2 NOT landed** — 4:2:2-only, refused by name at the sequence header, and
  an ungated change to a 4:2:2 path is the hazard `EC_AV1_ALLOW_422_PROBE`
  exists to prevent.
- **Classifiers unchanged** — 50 `around_mi(` sites, 49 correct, 1 instance,
  0 new.

Both newly-landed gates run green with the oracle forced on:

```
$ EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -- \
    a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact \
    the_skipped_128_root_chroma_suppression_publishes_the_blocks_own_per_axis_chroma_extent
test refusal_inventory::tests::the_skipped_128_root_chroma_suppression_publishes_the_blocks_own_per_axis_chroma_extent ... ok
test stream::tests::a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 760 filtered out
```

The first is the `4bfe8d8e` intra-in-inter fix from `lane-av1ibc128arm` landing
with this branch, as declared; the second is C1's source scan. Guard families
after this merge, same `touch`-first discipline:

```
gate_coverage      18 passed; 0 failed
refusal_inventory  20 passed; 0 failed     (19 + C1's new gate)
count_vacuity       2 passed; 0 failed
pin_inventory       3 passed; 0 failed
```

## 7. Cross-branch name-set sweep

`--list | wc -l` is not a count — libtest also prints a `running 0 tests` line
and a summary line, so the raw line count runs 2 high.

| | pre-wave `ec4e9528` | post-merge-2 `5f39325d` |
|---|---|---|
| `wc -l` | 757 | 764 |
| `grep -c ': test$'` | 755 | 762 |
| summary | `755 tests, 0 benchmarks` | `762 tests, 0 benchmarks` |

```
$ comm -23 pre post     # REMOVED
(empty)
$ comm -13 pre post     # ADDED
gate_coverage::count_vacuity_tests::every_locally_derived_oracle_count_is_reported: test
gate_coverage::count_vacuity_tests::the_count_vacuity_sweep_finds_the_known_sites: test
gate_coverage::pin_inventory_tests::every_pin_a_gate_reads_is_committed_under_the_crate: test
gate_coverage::pin_inventory_tests::the_pin_scanner_sees_every_shape: test
gate_coverage::pin_inventory_tests::the_pin_scanner_sees_the_directory_literal_shape: test
refusal_inventory::tests::the_skipped_128_root_chroma_suppression_publishes_the_blocks_own_per_axis_chroma_extent: test
stream::tests::a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact: test
$ sort post | uniq -d
(empty)
```

**755 → 762, seven added, none removed, none renamed, no duplicate
registrations.** Five from pins5 (2 `count_vacuity` + 3 `pin_inventory` — its
"5 new CI tests"), two from txsizeaudit/ibc128arm (C1's source scan + the
intra-in-inter pixel-exact gate). Arithmetic: `755 + 5 + 2 = 762`. A count
alone could not have told this apart from a rename or a loss, which is why both
`comm` directions are printed.

## 8. Topology

```
5f39325d  Merge commit 'c0727d64'            (2 parents: 7ec417ad c0727d64)
7ec417ad  Merge branch 'lane-av1pins5'       (2 parents: ec4e9528 e1f78b8e)
```

Both `--no-ff`, exactly two parents each. `origin/main` unchanged — nothing
pushed.

`d587f53f` (this report's first cut) is `origin/main`: Main verified the
partial on the pushed tree and pushed it. `lane-av1txsizeaudit` has since been
repointed to `c0727d64`, so the ref hazard in section 4 is closed for later
waves. Nothing in this wave's second cut has been pushed.
