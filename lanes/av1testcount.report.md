# lane-av1testcount — test-count census of the 13-branch merge wave

**Read-only.** No repository file was edited except this report, which is
committed on branch `lane-av1testcount` (worktree `~/.cache/wt/av1testcount`).
Every number comes from `cargo test -p ec-av1 --lib -- --list` executed in a
throwaway `git worktree` at each commit, in per-batch private
`CARGO_TARGET_DIR`s (`~/.cache/cargo-target-tc1..4`) — the shared house target
dir is not used, because a shared dir serves a sibling lane's binary and would
have made this census a census of someone else's tree
(`skill://counterfactual-ab-target-dir-provenance` variant 5). All 15 census
worktrees were removed afterwards.

**Re-measured 2026-09-30 (lane-breconcile), because main has moved well past
`9623bcab` and a reader must not mistake this for a count of TODAY's tests.**
Both anchors re-derived from scratch with `cargo test -p ec-av1 --lib -- --list`
on fresh `git archive` extracts with private target dirs:

```
298f75c4 (pre-merge base)   712 raw --list lines, 712 unique
9623bcab (merged main)      742 raw, 742 unique
b307aaeb (this tree)        802 raw, 802 unique
```

So the load-bearing arithmetic still holds — 712 and 742 are what they were,
and they are immutable facts about two immutable commits — but the tree this
report's numbers would be compared against is now **802**, not 742. The 13
per-branch tip counts in §1 were NOT re-taken (13 more throwaway builds); they
are the lane's own figures and are not re-verified here. What is re-verified is
the pair the reconciliation stands on, plus zero duplicate registrations at
both anchors, which is the property §2 names.

## 1. The two commits

| role | commit | subject |
|---|---|---|
| pre-merge base | `298f75c4` | `gitignore: let per-crate fixture pins be added without -f` |
| merged main | `9623bcab` | `Merge lane-av1superpin @ 9646f74d — superres den=9 + mode-2 pins, shared helper, report corrections` |

The 13 merges in `298f75c4..9623bcab`, newest first, with the branch tip taken
from each merge's second parent:

| # | merge | branch | tip | tests at the tip | NEW names vs base | base tests the tip does NOT carry | note |
|---|---|---|---|---|---|---|---|
| 1 | `9623bcab` | lane-av1superpin | `9646f74d` | 708 | **+4** | **8** | **the tip predates 8 base gates** — see the correction below |
| 2 | `afa13bcf` | lane-whtshape | `f541d062` | 717 | +5 | 0 | |
| 3 | `9a9e3674` | lane-av1lm444loss | `9249442f` | 713 | +1 | 0 | |
| 4 | `764ba257` | lane-av1dualfilter10 | `c2b969c8` | 715 | +3 | 0 | |
| 5 | `a5867794` | lane-av1distwtd | `1aaf7e0b` | 714 | +2 | 0 | |
| 6 | `7f8817cb` | lane-av1pins | `44a05b40` | 712 | +0 | 0 | |
| 7 | `e96de012` | lane-av1superhbd | `a2da03e4` | 717 | +5 | 0 | |
| 8 | `b4c45166` | lane-av1tilemeasure | `df1370dd` | 716 | +4 | 0 | |
| 9 | `962945ce` | lane-av1toolgates | `4c4bd3e2` | 714 | +2 | 0 | |
| 10 | `10db0dc2` | lane-av1mutproof | `ffd8d138` | 712 | +0 | 0 | |
| 11 | `0eb19c11` | lane-av1gates444 | `555c83f3` | 715 | +3 | 0 | |
| 12 | `5e9c7715` | lane-av1skipfix | `960be978` | 712 | +0 | 0 | |
| 13 | `e443a86e` | lane-av1chrtx | `ea97eb0c` | 715 | +3 | 0 | |
| | | | **sum** | | **+32** | **8** | |

**Correction (r1, after review).** The first version of this table had a
`tests at tip (base-relative delta)` column that printed `708 (+4)` on row 1,
which does not satisfy `tip = base + delta` — 712 + 4 = 716, not 708 — while
every other row did, on the row that names the merge head. The cause: the
column conflated two different numbers, "names the tip adds" and "net change".
`lane-av1superpin`'s tip `9646f74d` is **708 = 712 + 4 new - 8 removed**: it
was cut from a tree OLDER than `298f75c4` and does not carry 8 gates the base
already had —

```
a_444_12bit_inter_sequence_decodes_pixel_exact
a_444_intrabc_rect4_reads_its_own_chroma_plane_block
a_4to20_key_frame_takes_the_left_chroma_reference_read_its_libaom_row
a_lossless_444_10bit_inter_stream_decodes_pixel_exact
a_lossless_444_128_root_lossless_stream_reads_chunks_chunk_major
a_lossless_444_rect16x4_chroma_reach_is_ss_aware
a_pinned_444_inter_stream_chroma_units_inherit_their_own_quadrants_tx_type
a_real_aomenc_odd_coded_dimension_streams_decode_pixel_exact
```

— all eight of which ARE on merged main, so nothing was lost; the merge simply
took the base's copies. The conclusion of §2 is unaffected, because it is
stated over the UNION of new names and separately over base-minus-main, and
both of those are 0. What changes is the rule a future census must state
explicitly: **a branch tip is not `base + delta` unless the tip also carries
every base test.** The per-branch column is now three numbers, and a tip with a
non-zero "does NOT carry" cell is flagged.

## 2. The arithmetic

```
pre-merge base 298f75c4                712 tests
sum of the 13 branches' NEW names    + 32   (4+5+1+3+2+0+5+4+2+0+3+0+3)
distinct new names across all 13     -  2   (two names declared by TWO branches)
                                     ------
base + distinct new                =  712 + 30 = 742
merged main 9623bcab                  742 tests
DELTA                                 0
```

The 32 is a sum over the "NEW names" column, not over net per-branch deltas:
one tip (`9646f74d`) is cut from an older base and does not carry 8 base
tests, so its net is -4 while it adds 4. Summing nets would give 712 + 20 and
would NOT reconcile — which is exactly why the per-branch column above is
three numbers now. The reconciliation that holds is the one over the UNION of
new names plus the separate, independent check that the base survives intact
(`base - main = 0`, below): 8 base tests that one branch never carried are
still on main, taken from the base side of the merge.

Every set operation, computed on the sorted name lists:

| check | result |
|---|---|
| branch test names ABSENT from merged main (a silently dropped gate) | **0** |
| tests on merged main that are in no branch and not in the base | **0** |
| tests in the base that are GONE from merged main (a rename or a removal) | **0** |
| duplicate registrations on merged main (`--list \| sort \| uniq -d`) | **0** — 742 raw lines, 742 unique |
| per-branch duplicate NAMES (the risk that matters) | **2**, both resolved below |

**The census reconciles exactly: 712 + 30 = 742, delta 0, nothing lost,
nothing renamed, nothing duplicated.** The merge dropped no gate, and no
conflict resolution silently took one side of an add/add.

## 3. The two duplicate names, checked for a lost assertion

A duplicate name across branches is the one way the count can look right while
an assertion is lost: the merge keeps one side, the other side's body is gone.
Both were diffed, body against body, at the two tips that declare them:

| duplicate name | declared by | verdict |
|---|---|---|
| `stream::tests::a_distance_weighted_compound_stream_decodes_pixel_exact` | lane-av1dualfilter10 (`c2b969c8`) and lane-av1distwtd (`1aaf7e0b`) | **byte-identical** (188 lines each, `cmp` equal) — the same gate declared twice, not two gates. Nothing lost. |
| `gate_coverage::tests::the_detector_sees_the_gates_the_spelling_filter_missed` | lane-av1distwtd (`1aaf7e0b`) and lane-av1dualfilter10 (`c2b969c8`) | **byte-identical** (61 lines each, `cmp` equal) — same. lane-av1toolgates does not declare it at all, so the pair is the one above. |

So the 2-test difference between "sum of deltas" (32) and "distinct new names"
(30) is fully explained: two gates were each written twice, identically, by two
lanes that had not seen each other. That is a coordination miss between
lane-av1dualfilter10 and lane-av1distwtd, not a merge loss.

## 4. The number for the next wave

A wave that merges N branches should satisfy
`merged == pre_merge_base + (sum of branch deltas) - (duplicate names across
branches)`, and separately report `base - merged == 0` (nothing renamed or
dropped) and `--list | sort | uniq -d` empty (nothing shadowed). This wave:
**712 -> 742, +30 distinct tests (+4.2%), 13 branches, ~2.3 tests per branch.**
The charter's expectation of "~35 gates this wave added" is close but high by
five: measured 30 distinct (32 summed before the duplicate collapse). Three of
the thirteen branches (pins, mutproof, skipfix) add no test at all, which is
where the difference lives.

## 5. Limits of this census, stated so nobody over-reads it

- It compares NAMES. A branch that changed the BODY of an existing gate in
  place (rather than adding one) moves no name and is invisible here — the
  per-branch diffs of `stream.rs` would be the tool for that, and this lane
  makes no claim about bodies.
- It counts `#[test]`s in the `ec-av1` LIB target only (`--lib`), which is
  where every gate in this crate lives; the crate has no `tests/` directory and
  no other test target.
- `--list` was used throughout; nothing was run, so a test that is registered
  but red would still count as present. That is the question this census asks
  (did it survive the merge), not whether it passes.
