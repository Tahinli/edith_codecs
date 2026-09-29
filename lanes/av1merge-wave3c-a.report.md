# Merge wave 3c-a — lm444loss-corr, oraclehbd (fixlib HELD)

Integration owner run. Pre-wave `main` = `c6112723` (wave-3b merge report).
Post-wave `main` = `6c1d78a6`. Lane-private
`CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/merge3ca` for every cargo
invocation in this wave (the shared `target/` has served another worktree's
stale binary four times this batch).

Nothing was pushed.

## 1. Per-branch table

| # | branch | merge-base | tip merged | merge commit | predicted | actual | `cargo check -p ec-av1 --all-targets` |
|---|---|---|---|---|---|---|---|
| 1 | `lane-av1lm444loss-corr` | `c6112723` (= pre-wave main) | `b3033a2a` | `879c11e2` | clean (`93dec078`) | clean, no conflict | Finished, 0 warnings |
| 2 | `lane-av1oraclehbd` | `3eec02c8` | `4fcbe5ad` | `8667050b` | clean (`00336264`, re-predicted as `c2fc779b` against the post-#1 main) | clean, no conflict | Finished, 0 warnings |
| 3 | `lane-av1fixlib` | `0dfdf0c8` | `3be839be` | **NOT MERGED — held by Main mid-wave** | — | — | — |
| 4 | `lane-av1fixtureshape` | — | `c1cf1758` | **not merged separately: subsumed (§5)** | — | — | — |

Blast radius as merged, each merge diffed against its own first parent:

```
879c11e2  crates/ec-av1/fixtures/ll444_intrabc_rect_l2.obu | Bin 0 -> 121844 bytes
          crates/ec-av1/src/decode.rs                      |  21 +-
          crates/ec-av1/src/stream.rs                      | 146 ++++++++
          lanes/av1lm444loss-corr.report.md                | 408 ++++++++++++++++++
          4 files changed, 574 insertions(+), 1 deletion(-)

8667050b  lanes/av1oraclehbd.report.md      | 426 ++++++++++++++++++++++++++++
          scripts/check-aom-oracle-rungs.sh |  92 ++++++++++++
          scripts/instrument-aom-oracle.sh  | 172 ++++++++++++++++
          3 files changed, 690 insertions(+)
```

Branch #2's tip MOVED between the `merge-tree` prediction and the merge
(`e16329a6` → `4fcbe5ad`, a report-only commit documenting the zero-window
oracle swap: `lanes/av1oraclehbd.report.md | 128 +++`). The script content is
byte-identical between the two tips, so the clean prediction still held for the
code; the whole branch landed in one merge commit and its second parent is the
new tip `4fcbe5ad`.

## 2. Conflicts and how each was resolved

**There were none in either merge.** Both `git merge-tree --write-tree`
predictions returned a tree id with no conflict list, and both real `--no-ff`
merges auto-resolved with zero hunks. Per `skill://multi-lane-branch-merge-resolve`
a clean auto-merge is not evidence of a clean merge, so
`cargo check -p ec-av1 --all-targets` ran after EACH merge (`src/lib.rs`
touched first so the check could not serve a cache), and a `--list` plus a
full name-set comparison ran after the second.

**One integration fix was still required** — a defect the lane introduced that
only shows on the merged tree, because the guard that catches it landed in the
preceding wave (`b1a462fa`, wave 3b).

`lane-av1lm444loss-corr`'s new gate guarded its pixel compare with a bare
`aomdec_path().is_file()`. The oracle-presence scan merged in wave 3b
(`7a865ee7`) rejects exactly that shape outside the probe that owns it:

```
gate_coverage::tests::no_tool_presence_check_outside_its_probe
  oracle-presence scan: 8 site(s) inspected, 7 in a probe / assert / comment, 1 offender(s);
  per pattern {"affine_aomenc_path().is_file()": 1, "aomdec_path().is_file()": 6, "aomenc_path().is_file()": 1}
  1 bare tool presence check(s) outside a probe
    stream.rs:7271: if aomdec_path().is_file() {
test result: FAILED. 28 passed; 1 failed; 0 ignored; 0 measured; 720 filtered out
```

A bare `is_file()` skips the compare with **no env escape**, so under
`EC_AV1_REQUIRE_AOMENC=1` the gate would still report green with the oracle
compare never run — the "unreadable gate is unproven, not clean" class. The
resolution routes the arm through the probe that owns the path,
`aomdec_available(NAME)`, which carries both the env-escape assertion and the
SKIP line, and drops the hand-rolled `else` arm rather than keeping it
alongside. Committed as its own commit, not folded into the merge, so it stays
separately reviewable and separately revertable:

`6c1d78a6 wave-3c-a: route the lm444loss-corr gate's oracle arm through aomdec_available`

## 3. Branch 1 — `lane-av1lm444loss-corr`

Claim: one expression in `decode.rs::decode_intrabc_rect`'s lossless chroma
tail replay, `ur * (4 >> ss_y)` → `ur * ((4 << ss_y)/MI)`.

### Structural checks

| check | required | measured | verdict |
|---|---|---|---|
| `INTRABC_RECT_LOSSLESS_CHROMA4_HITS` occurrences in `decode.rs` | survive at 6 | 6 on pre-wave main, 6 on the branch tip, 6 on the merged tree | PASS |
| `decode_intrabc_rect`'s `ll_chroma` replay tail | byte-identical | the whole-file `decode.rs` delta pre-wave→merged is 1 removed line + 20 added lines: the single step expression plus its 19-line comment. Nothing else in the file moves. | PASS |
| new gate exists exactly once | 1 | 1 `fn` definition (`stream.rs:7233`), 1 registration in `--list`; 2 textual hits = the `fn` + its `const NAME` | PASS |
| gate non-vacuous | — | mutation reproduced red on the merged tree, then restored green | PASS |

The complete evidence for "one expression" is the whole-file delta:

```
$ diff <(git show c6112723:crates/ec-av1/src/decode.rs) crates/ec-av1/src/decode.rs
<   (mi_r + ur * (4 >> ss_y(fctx)), mi_c + uc * (4 >> ss_x(fctx))),
>   // lane-av1lm444loss-corr: the MI STEP between chroma units is that same
>   // footprint expressed in luma mi cells, `(4 << ss) / MI` ... (19 lines) ...
>   (mi_r + ur * ((4 << ss_y(fctx)) / MI),
>    mi_c + uc * ((4 << ss_x(fctx)) / MI)),
```

### The `(0,0,8)` shape assert

The gate's first assertion reads the stream's **own parsed sequence header** and
requires `(subsampling_x, subsampling_y, bit_depth) == (0, 0, 8)`. That is a
shape gate, not a hole. The reasoning is arithmetic: at ss (1,1) the replaced
`4 >> 1` and the new `(4 << 1) / 4` are both the integer 2, so a 4:2:0 stream
cannot make this gate fail — every other assertion in it would pass on a stream
that cannot exercise the corrected step. The assert makes that non-failure
explicit instead of silent. 4:2:0 coverage is the arithmetic plus the scoped
battery in the next paragraph, not a 4:2:0 arm of this gate (a floor-on-wrong-
output assert there would be theatre).

### Non-vacuity, measured on the MERGED tree

```
$ EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -- \
    a_lossless_444_intrabc_rect_replay_steps_by_the_units_own_mi_footprint
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 748 filtered out; finished in 0.56s

# MUT corr: put `4 >> ss_y` / `4 >> ss_x` back into decode.rs
MUT applied
thread 'stream::tests::a_lossless_444_intrabc_rect_replay_steps_by_the_units_own_mi_footprint'
  panicked at crates/ec-av1/src/stream.rs:9592:17:
a_lossless_444_intrabc_rect_replay_steps_by_the_units_own_mi_footprint: decode-order frame 0 of 6
  (6 shown, 0 hidden) differs from the oracle at byte 33216 (ours 101 vs 100), 10604 bytes differ
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 748 filtered out
MUT_EXIT=101

# restore
RESTORED byte-identical          # diff -q against the pre-mutation copy
$ grep -c 'MUT corr' crates/ec-av1/src/decode.rs
0
$ git status --porcelain        # (empty)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 748 filtered out
```

The mutation reproduces the lane's own numbers exactly — **10604** bytes differ,
first at **byte 33216**, in the **key** frame, on a luma sample. That is the
signature of a chroma-context defect: the wrongly-placed stamps corrupt the
`txb_skip_ctx` the next unit reads, the tile desyncs, and the samples that move
first are whichever block comes next in luma. The gate bites, on the merged
tree, not only in the lane's worktree.

### Fixture pin provenance

```
$ sha256sum crates/ec-av1/fixtures/ll444_intrabc_rect_l2.obu
f0de080edce35d34cdd628b71f63dc452f946e8328352b69d3c1da090a54ec67   (121844 bytes)
```

Matches the report's claimed sha256 and size, and the gate's own
`read_pin(&obu, 121844, 0x38bf_968e_979c_7a0a, NAME)` passed, so the FNV hash
in the gate is this pin's. TRACKED, not merely on disk:

```
$ git ls-tree -r HEAD crates/ec-av1/fixtures/ll444_intrabc_rect_l2.obu
100644 blob 601adaa24413ad370d4e03c5f9672e7ca6a70ef0  crates/ec-av1/fixtures/ll444_intrabc_rect_l2.obu
```

Whole-directory grouping of `crates/ec-av1/fixtures` by blob sha is empty: no
two committed fixtures are byte-identical under different names.

### Scoped gate runs (final merged tree, after the §2 fix)

```
$ cargo test -p ec-av1 --lib -- lossless 444 intrabc
test result: ok. 64 passed; 0 failed; 1 ignored; 0 measured; 684 filtered out; finished in 26.47s
```

**64 passed / 0 failed / 1 ignored** — the expected triple exactly.

The second scoped run needs a note, because its expected number is not
reproducible on today's roster:

```
$ cargo test -p ec-av1 --lib -- frame_edge partial edge odd lossless --skip 444
test result: ok. 44 passed; 0 failed; 0 ignored; 0 measured; 705 filtered out; finished in 137.65s
```

**44 passed / 0 failed**, not 17. The filter is the lane's own definition of
the set, quoted from the superseded `lanes/av1lm444loss.report.md:236` — "4:2:0
lossless + frame-edge gates (17 scoped: `frame_edge`, `partial`, `edge`, `odd`,
`lossless`, non-444)" — with the non-444 clause applied as `--skip 444`. Both
the lane report and the assignment carry "17", but neither enumerates the 17
names, and no narrower reading of that scope reproduces 17 on the current tree:
the same filter restricted to `stream::tests::` yields 23, to
`stream::tests::` + (frame_edge|lossless) yields 15, `frame_edge` alone yields 12,
`stream::tests::` + lossless yields 8. The "17" is inherited prose from a
superseded lane's report and the roster has grown since. The 44-gate run is a
strict superset of every candidate 17, and it is green, so the intent the number
stood for — no 4:2:0 lossless or frame-edge regression from the corrected step —
is met with a wider measurement than asked for.

## 4. Branch 2 — `lane-av1oraclehbd`

Claim: oracle INSTRUMENTATION + scripts only; no gate touched; the shared oracle
untouched.

### Surface

```
$ git diff --name-only 3eec02c8 4fcbe5ad
lanes/av1oraclehbd.report.md
scripts/check-aom-oracle-rungs.sh
scripts/instrument-aom-oracle.sh

$ git diff --name-only 3eec02c8 4fcbe5ad | grep -c '\.rs$'
0
```

No `.rs` file is touched, and no fixture is regenerated. The merge against
`main` re-confirms it on the merged tree:

```
$ git diff --name-only 879c11e2 8667050b | grep '\.rs$'
(no output)
```

### The checker

```
$ bash scripts/check-aom-oracle-rungs.sh
ok   legacy u8 row loops left in the derived file               0
ok   ec_dump_narrow_row call sites (4 rungs x 3 planes)         12
ok   ec_dump_finish call sites (one per narrowing rung)          4
ok   byte-count check wired into EC_AV1_PREFILT_DUMP            1
ok   byte-count check wired into EC_AV1_POSTDEBLOCK_DUMP         1
ok   byte-count check wired into EC_AV1_PREFILT_WIDE_DUMP        1
ok   byte-count check wired into EC_AV1_POSTCDEF_DUMP            1
ok   rung 12 still converts plane pointers                      1
ok   rung 12 not routed through the narrowing checker           0
ok   instrument-aom-oracle.sh derives the depth-correct, byte-checked rungs (base v3.13.3)
EXIT=0
```

**10 `ok` lines, exit 0**, against a scratch copy of the oracle tree
(`mktemp -d`, `trap rm -rf`), base `v3.13.3` = `92d4c37f`. The script also
asserts idempotency by re-running the instrumenter and `cmp`-ing the derived
file, which is the property the repair block claims.

### The shared oracle was not written to

The checker only reads the real tree (`cp -a` into `$WORK`, then the instrumenter
runs with `AOM_ORACLE_SRC=$WORK/src`). Measured before and after:

```
$ md5sum ~/.cache/aom-oracle/src/av1/decoder/decodeframe.c ~/.cache/aom-oracle/src/av1/common/reconintra.c
45e316447f7d71e72e466e5d63f76930  .../av1/decoder/decodeframe.c
aab22cb52e6b595d0ee13dd8735b722a  .../av1/common/reconintra.c
```

Those two files are already dirty in that checkout — that is the lane's own
zero-window swap, reported in its appendix, not this merge. Flagged to whoever
owns the shared-oracle question; the merge did not add to it.

## 5. Branch 3 — `lane-av1fixlib` — HELD, not merged

Main's hold arrived mid-wave: `pin-gate-audit.py` matched raw function bodies,
and because `fn_bodies` spans from a gate's `fn` line to the NEXT `fn` line, a
doc comment belonging to the next gate sits inside the previous gate's captured
body. Restoring the golden7 prose therefore reclassified
`pinned_golden4_stream_decodes_pixel_exact` into the root-literal branch, where
it finds no names and is skipped entirely — census `total=8 → 7`, no red
anywhere. Do-not-merge honoured: the branch was not merged and no part of its
work was cherry-picked.

What was measured anyway, so the go/no-go is data and not a guess.

### Tip moved during the wave

`lane-av1fixlib` was `2b4ecf71` when the wave opened and is `3be839be` now. The
new commits are exactly the two that close Main's hold:

```
3be839be report: the three-host run on the flipped tree, and the census hole with its control
f4e1ac6b census: comment-stripped matching plus a self-test in the shape that cost a gate
717a5f07 merge Kerem-5's report of the census hole (the crate-side prose restore is already in via 0790e90c)
9e433f02 restore the golden7 doc comment verbatim; the scanner is comment-blind-proof, so the history stays
c1cf1758 lane-av1fixtureshape: report — site 1 is a scanner defect, plus an open census hole that un-counts a gate
0790e90c ec-av1: restore the verbatim forbidden literal in the golden7 doc comment
```

### `lane-av1fixtureshape` is SUBSUMED — verified by ancestry

```
$ git log --oneline -1 lane-av1fixtureshape
c1cf1758 lane-av1fixtureshape: report — site 1 is a scanner defect, plus an open census hole that un-counts a gate
$ git merge-base lane-av1fixlib lane-av1fixtureshape
c1cf1758eabad8727e317d178b6aaf2129f4d20f
```

`c1cf1758` — the fixtureshape tip — **is** the merge base, i.e. it is an
ancestor of `3be839be`. So it is merged, and must not be merged separately.

Worth recording because the check was FALSE earlier in the wave, and for a
reason worth knowing: against the then-current tip `2b4ecf71` ancestry said
`NOT-SUBSUMED` and `stream.rs` really did differ
(`12cae635` blob `0070f0c4` = reworded, vs `0790e90c` blob `88ec09c7` =
restored). The tip moved, not the conclusion — a lane's subsumption claim must
be re-checked at the tip that will actually be merged, never at the tip quoted
in a message.

### Main's acceptance line, measured on the branch tip

Census, with the prose restored (`stream.rs:23981` carries
`literal, which points at the GITIGNORED root`):

```
$ git archive lane-av1fixlib | tar -x -C /tmp/3ca_fixlib && cd /tmp/3ca_fixlib
$ python3 scripts/pin-gate-audit.py | tail -1
COUNT	total=8	committed=21	uncommitted=0	ignored=0	assertless=0
```

**`total=8 committed=21 uncommitted=0 ignored=0 assertless=0`** — the required
line, not `7`. The prose-restore hazard is closed.

Census positive control, the exact shape that cost the gate:

```
$ python3 scripts/pin-gate-audit.py --self-test
SELFTEST	selftest_a_reads_a_pin	crate_pin
SELFTEST	selftest_b_reads_a_pin	crate_pin
SELFTEST	PASS	a doc comment between two gates left both counted as crate_pin
SELFTEST_EXIT=0
```

Both gates are classified `crate_pin` across a doc comment that quotes a banned
`concat!` root literal. `f4e1ac6b` also added the scanner-side strip
(`re.sub(r"/\*.*?\*/", ...)`, then a `l.split("//", 1)[0]` per line) to
`pin-gate-audit.py`, and a second strip at line 206 for the `//`-in-body case.

Preflight, on the real checkout (`~/.cache/wt/av1fixlib`, clean at `3be839be`) —
a `git archive` export is not a valid host because the root `fixtures/` tree is
gitignored and every row then reads missing:

```
$ EC_REQUIRE_FIXTURES=1 bash scripts/verify-fixture-library.sh
  invariant 1: positive control fired (the scanner can still see a literal)
  invariant 1: no root-fixture pin path
  invariant 2: every committed pin is tracked
  invariant 3: .gitignore negation present
  invariant 3: no tracked pin is shadowed by .gitignore
  invariant 4: census self-test passed (a comment cannot steal a gate)
  invariant 4: every pin-reading gate resolves through committed copies
  recovered pins: every recovered-original hash matches
  vectors: 3 referenced rows, 0 not ok
  drift: manifest rows match the code on this host
  code-shape violations: 0
verify-fixture-library: GREEN (303 rows)
PREFLIGHT_EXIT=0
```

The worktree was still clean afterwards — the preflight wrote nothing.

### One acceptance claim that needs correcting before the go-ahead

"`git diff main --stat` shows scripts/ + reports, NOT a second copy of
ec-h264/ec-opus/ec-flac conformance files" is **true in substance, false as
stated**. There is no duplicated copy: the 1054-line
`crates/ec-opus/tests/conformance.rs` edit from `b2d9b195` is gone, and
`ec-h264/tests/conformance.rs` and `ec-flac/tests/xiph_vectors.rs` are gone with
it. But fixlib still carries its own edits in four other conformance test
files, going from main to the tip:

```
crates/ec-aac/tests/oracle.rs           |  39 +++++++++---
crates/ec-aac/tests/sbr_real_library.rs | 104 ++++++++++++++++++++++++++++
crates/ec-flac/tests/encode_matrix.rs   |  67 ++++++++++++++++----
crates/ec-h264/tests/encode.rs          |  38 +++++++++++-
```

These are in scope for a lane whose charter is "kill the fixture-absent
silent-skip class" — a ~24-line `require_fixture(path, generator)` probe plus
call-site swaps that turn `let Ok(text) = read_to_string(..) else { SKIP }` into
probe → assert → return, so `EC_REQUIRE_FIXTURES=1` hard-fails instead of
skipping. They are not a second copy. Worth stating explicitly in the go/no-go
so the next reader is not surprised by a non-`scripts/` entry in the stat.

Also spotted, cosmetic: the new probe's doc comment in all four files ships an
unfilled template placeholder, `/// Fixture-presence probe for
`<what this test needs>`.`

### Verdict on fixlib

Measured ready against every line Main set — census line exact, positive
control passing, preflight GREEN — but it is **not merged**, because Main's hold
is a decision and not mine to lift. One `git merge-tree --write-tree` predicts
it clean, since fixlib touches `crates/ec-av1/src/stream.rs` (the golden7 doc
comment, `0790e90c`/`9e433f02`) and the four conformance files, and this wave's
own two commits touch `decode.rs`, `stream.rs` and `scripts/` — so the hot-file
overlap to watch is `stream.rs`. `stream.rs` is likely to conflict: this wave
added a gate at the tail of the same test module.

## 6. Cross-branch sweep

### Test NAME sets, both directions

`--list | wc -l` is not a count: libtest also prints a `running 0 tests` line and
a summary line, so pre-wave `wc -l` is 750 for 748 tests.

| | pre-wave `c6112723` | post-wave `6c1d78a6` |
|---|---|---|
| `wc -l` | 750 | 751 |
| `grep -c ': test$'` | 748 | 749 |
| summary line | `748 tests, 0 benchmarks` | `749 tests, 0 benchmarks` |

```
$ comm -23 pre post     # REMOVED
(empty)
$ comm -13 pre post     # ADDED
stream::tests::a_lossless_444_intrabc_rect_replay_steps_by_the_units_own_mi_footprint: test
$ sort post | uniq -d    # duplicate registrations
(empty)
```

**Nothing removed, nothing renamed, one addition, no duplicate registration.** A
count alone could not have distinguished this from "+2, −1"; the two
`comm` directions can, and both are reported above. The arithmetic reconciles:
`748 pre + 1 (lm444loss-corr) + 0 (oraclehbd, scripts only) − 0 = 749`.

The census family, on the merged tree, before and after the §2 fix:

```
# before the fix
$ cargo test -p ec-av1 --lib -- gate_coverage refusal_inventory count_vacuity
test result: FAILED. 28 passed; 1 failed; 0 ignored; 0 measured; 720 filtered out

# after the fix
$ cargo test -p ec-av1 --lib -- gate_coverage refusal_inventory count_vacuity
test result: ok. 29 passed; 0 failed; 0 ignored; 0 measured; 720 filtered out
```

Gates the three branches touch: `oraclehbd` touches none (scripts only);
`lm444loss-corr` adds exactly the one named above, run green under
`EC_AV1_REQUIRE_AOMENC=1` and proven red by the mutation in §3. Both `--list`
captures were taken after `touch`ing `stream.rs` and `decode.rs`, because those
tests read compile-time sources and a stale binary would report the previous
tree's census.

### Fixture-pin hygiene for the wave's new pin

Covered in §3: the pin is tracked at one blob sha, and no two committed
`crates/ec-av1/fixtures` blobs collide under different names.

### Foreign/untracked files

`git status` shows `?? lanes/av1probes.report.md` — a sibling lane's untracked
report. It was NOT staged into any commit in this wave (the wave's three commits
touch only `decode.rs` / `stream.rs` / `scripts/` / the branch reports). It is
flagged here so its owner lands it from their own worktree.

## 7. Merge commit topology

```
6c1d78a6  wave-3c-a: route the lm444loss-corr gate's oracle arm through aomdec_available
8667050b  Merge branch 'lane-av1oraclehbd'          (2 parents: 879c11e2 4fcbe5ad)
879c11e2  Merge branch 'lane-av1lm444loss-corr'     (2 parents: c6112723 b3033a2a)
```

Both merges are `--no-ff` with exactly two parents each. Nothing pushed.
