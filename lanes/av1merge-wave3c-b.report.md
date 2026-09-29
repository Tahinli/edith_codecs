# Merge wave 3c-b — four branches in, three retired, three integration fixes, one VPS suite


> **VERDICT: the wave is merged and green locally; the VPS full-suite run is
> still executing and its totals are NOT in.** Four merges landed with zero
> conflicts, three branches retired, **integration fix #1 turned out to be a fix
> for a red that does not exist** (the stale entries were deleted back in
> `1cc2f250`) and **integration fix #2 likewise** (all five `.obu` pins are
> committed, index == worktree == HEAD). A **third** real problem was found and
> fixed as `c548ae8f`: the fixture manifest no longer regenerated on the merged
> tree. Two branches' stated numbers did not survive checking: `--ignored
> --list` is 51 → **49**, not 51 → 50, and one staging mistake would have
> certified a suite full of silent SKIPs. Details and the exact command to read
> the run's result are in §6.

`CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/merge3cb` (lane-private; the shared
target dir has served another worktree's stale `ec_av1` binary six times this
batch). `EC_NOMEMGUARD=1` throughout. `touch` on every source-scan module
before any census run.

---

## 1. Per-branch table

| # | branch | tip taken | merge base | commits | files | conflicts | `cargo check -p ec-av1 --all-targets` | merge commit |
|---|--------|-----------|-----------|---------|-------|-----------|------------------------------------------|--------------|
| 1 | `lane-av1loss444kf` | `c894ed0d` (live, = assigned) | `c6112723` | 1 | `decode.rs`, `stream.rs`, report | **none** | clean, 0 warnings | `b94bc532` |
| 2 | `lane-av1refusalspan` | `026851cc` (live, = assigned) | `745c60e7` | 5 | `decode.rs`, `refusal_inventory.rs`, `stream.rs`, report | **none** | clean, 0 warnings | `2b7caad3` |
| 3 | `lane-av1fixlib` | `3be839be` (live, = assigned) | `0dfdf0c8` | 19 | 32 (incl. 14 new fixture blobs + 4 scripts) | **none** | clean, 0 warnings | `75bf8f2f` |
| 4 | `lane-av1probes` | `23fb353d` (live, = assigned) | `745c60e7` | 2 | `encode.rs`, `encoder.rs`, `stream.rs`, `transform.rs`, report | **none** | clean, 0 warnings | `be6449ad` |

All four tips were re-read from the live tree before merging and every one
equals the assigned sha — **no tip moved**.

Order: loss444kf → refusalspan → fixlib → probes. It is a *choice*, not a
constraint: every pairwise `git merge-tree --write-tree` (all 4 × main plus all
6 branch pairs, 10 predictions) returned exit 0 with a tree and no conflict
list, and all four merges auto-merged with no marker. The order was picked so
the two branches that touch the same hot file (`stream.rs`, all four of them)
land adjacent to their own gate runs.

### Conflicts, per region

**There were none** — and that is exactly the claim skill://multi-lane-branch-merge-resolve
says must not be taken on trust, so each merge was followed by
`cargo check -p ec-av1 --all-targets` *and* the source-scan guard families, not
by the clean auto-merge message:

| merge | `decode.rs` | `stream.rs` | other | structural check |
|-------|-------------|-------------|-------|------------------|
| `b94bc532` | auto (gate-coverage census confirms `stream.rs:47096/47104` etc. land once) | auto | report added | clean |
| `2b7caad3` | auto | auto | `refusal_inventory.rs` added wholesale | clean |
| `75bf8f2f` | auto (5 lines) | auto (66) | 6 other crates' tests + `scripts/` | clean; `cargo check --workspace --all-targets` clean |
| `be6449ad` | untouched | auto | `encode.rs`/`encoder.rs`/`transform.rs` | clean |

`@both` was never used. Two adjacent branches *did* touch the same line
(`crates/ec-av1/src/encoder.rs`, the inter-timing print) from opposite
directions and ort still auto-merged them; the merged text is the sum of both
(`saturating_sub` overlap accounting added by `lane-av1probes`, the bucket
labels kept from the lane's own change) — verified by the workspace check and
by reading the merged block, not by the absence of a marker.

Whole-workspace compile of the merged tree (fixlib edited six other crates'
test files, which `cargo check -p ec-av1` does not cover):

```
$ CARGO_TARGET_DIR=…/merge3cb cargo check --workspace --all-targets
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 8.54s
warning: function `decode_capture` is never used
   --> crates/ec-vorbis/tests/oracle.rs:1091:4
```

That warning is **pre-existing**, not this wave: `decode_capture` is a thin
wrapper that is already unused at `745c60e7`
(`git show 745c60e7:crates/ec-vorbis/tests/oracle.rs` line 1025, same wrapper,
same zero call sites). Left alone — it is not a decode-path change and fixing it
would be scope I was not given.

---

## 2. Per-branch verification on the merged tree

### 2.1 `lane-av1loss444kf` — 4:4:4-lossless KF chroma reach

Reviewer verification was taken as given (revert-the-two-lines reproduces the
49-sample fingerprint; both changed arms sit under `lossless(fctx)` inside
`chroma_444`). Re-proved the **gate's** non-vacuity independently, by
mutation, on the merged tree — the two `Reach::of_tu` lines and nothing else:

```
$ cp crates/ec-av1/src/decode.rs /tmp/decode.rs.bak   # both sites replaced
$ cargo test -p ec-av1 --lib -- a_lossless_444_sub8_rect_leaf_chroma_reach_decodes_the_key_frame_pixel_exact
---- … stdout ----
panicked at crates/ec-av1/src/stream.rs:46597:9:
assertion `left == right` failed: lossless-444-sub8-rect-leaf-chroma-reach: the 4:4:4
lossless key frame differs from aomdec in 49 samples (first Some((1, 201, 200, 160, 159))
as (plane, x, y, ours, oracle)) -- a sub-8x8 RECT leaf's chroma unit must be given the
reach libaom's has_bottom_left computes for the LEAF, not a standalone BLOCK_4X4 lookup
at the unit's own position
  left: 49
 right: 0
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 752 filtered out
```

Byte-identical to the lane's recorded fingerprint (`49 samples, first
Some((1, 201, 200, 160, 159))`). Decoder restored from the backup and the gate
re-run green — `git diff crates/ec-av1/src/decode.rs` empty after restore.

Invariant sets (the three the lane named), on the merged tree, private target
dir:

```
$ cargo test -p ec-av1 --lib -- 'lossless' '444' --test-threads=1
test result: ok. 49 passed; 0 failed; 0 ignored; 0 measured; 704 filtered out; finished in 28.76s
$ cargo test -p ec-av1 --lib -- '420' --test-threads=1
test result: ok.  2 passed; 0 failed; 0 ignored; 0 measured; 751 filtered out; finished in 0.85s
$ cargo test -p ec-av1 --lib -- 'intrabc' --test-threads=1
test result: ok. 22 passed; 0 failed; 1 ignored; 0 measured; 730 filtered out; finished in 36.54s
```

The lane's own report says "-- lossless 444: 48/0". I measure **49/0**, and the
one-test difference is the lane's new witness gate, which its own filter matches:

```
$ cargo test -p ec-av1 --lib -- 'lossless' '444' --test-threads=1 | grep -c '^test .* \.\.\. ok$'
49
$ … | grep -c a_lossless_444_sub8_rect_leaf_chroma_reach          # the lane's own new gate
1
49 - 1 = 48                                                     # == the lane's figure
```

So 48 is that same set minus the gate the lane added — a counting artefact of
writing the battery line before its own witness gate landed, not a disagreement.

### 2.2 `lane-av1refusalspan` — brace-bounded proof bodies

Distribution line prints, from the test itself (`--nocapture`):

```
test refusal_inventory::tests::every_proven_refusal_names_a_test_that_exists ...
  anchor strength: 26 rows quote the WHOLE refusal string, 7 quote its LEADING CLAUSE, 0 match neither: []
  refusal inventory: 33 refusals + 1 capability claims, 33 proven
ok
test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 734 filtered out; finished in 0.50s
```

**26 / 7 / 0, 19 passed, 0 failed** — the lane's claim reproduced exactly, not
"close to". The two new hardening tests are in that 19:
`a_body_carrying_another_function_is_refused`,
`a_gate_body_is_bounded_by_its_own_closing_brace`.

The seven gate bodies the lane edited, run individually on the merged tree:

| gate | result |
|------|--------|
| `every_intra_in_inter_shape_the_census_lists_has_a_size_group_row` | 1 passed / 0 failed |
| `every_shape_that_allows_motion_variation_has_a_motion_mode_cdf_row` | 1 passed / 0 failed |
| `a_var_tx_tree_never_presents_a_leaf_larger_than_the_unit_it_entered` | 1 passed / 0 failed |
| `every_rect_transform_shape_the_census_lists_has_a_coefficient_table_and_scan` | 1 passed / 0 failed |
| `every_inter_record_publishes_an_obmc_readable_filter` | 1 passed / 0 failed |
| `a_selected_reference_with_an_empty_ref_frame_idx_slot_refuses_by_name` | 1 passed / 0 failed |
| `every_frame_size_a_header_can_code_has_a_mode_info_grid` | 1 passed / 0 failed |

### 2.3 `lane-av1fixlib` — fixture-library preflight

```
$ EC_REQUIRE_FIXTURES=1 scripts/verify-fixture-library.sh
verify-fixture-library: root=… fixtures=…/fixtures EC_REQUIRE_FIXTURES=1
  shape: 305 rows
  resolve: 0 missing, 0 empty
FINDING: 85 pinned fixture(s) have NO generator -- provenance is prose in a gate comment only…
  invariant 1: positive control fired (the scanner can still see a literal)
  invariant 1: no root-fixture pin path
  invariant 2: every committed pin is tracked
  invariant 3: .gitignore negation present
  invariant 3: no tracked pin is shadowed by .gitignore
  pin gates: total=8 committed=21 uncommitted=0 ignored=0 assertless=0
  invariant 4: census self-test passed (a comment cannot steal a gate)
  invariant 4: every pin-reading gate resolves through committed copies
  recovered pins: every recovered-original hash matches
  vectors: 3 referenced rows, 0 not ok
verify-fixture-library: GREEN (305 rows)
EXIT=0
```

Census line is the lane's exact string, including `assertless=0`. Both positive
controls fire (invariant 1's planted literal, invariant 4's self-test).
Shape is fatal by default (`EC_FIXTURE_SHAPE_STRICT:-1`). The `85 … NO
generator` block is a **FINDING, not a failure** — it is the pre-existing
provenance prose the lane documented and it does not move the exit status.

```
$ python3 scripts/pin-gate-audit.py --self-test
SELFTEST	selftest_a_reads_a_pin	crate_pin
SELFTEST	selftest_b_reads_a_pin	crate_pin
SELFTEST	PASS	a doc comment between two gates left both counted as crate_pin
EXIT=0
```

**No duplicated conformance files.** `crates/*/tests/conformance.rs` resolves
to exactly three tracked paths, all distinct by md5:

```
1232e9b8844197abf2004e96afaf60e3  crates/ec-h264/tests/conformance.rs
4652a7d83fc27e1060a205f232458db4  crates/ec-opus/tests/conformance.rs
bc27bc88c8f5b8c583fc3012e171ebcf  crates/ec-h265/tests/conformance.rs
$ md5sum … | awk '{print $1}' | sort | uniq -d        # empty
```

Golden7 prose restored verbatim (`0790e90c` in the lane's history, carried by
the merge) — invariant 1 reads `no root-fixture pin path` *with* the literal
present in the doc comment, which is the point of the comment-stripping scanner.


### 2.4 `lane-av1probes` — ignored-probe audit

**No decoder change**: the diff touches `encode.rs`, `encoder.rs`, `stream.rs`,
`transform.rs` and the report. `crates/ec-av1/src/decode.rs` is not in
`git diff 745c60e7..23fb353d --name-only`.

`--ignored --list` count: **51 → 49, not 51 → 50.** Two gates were un-ignored,
not one, and the second is not this lane's:

| gate | un-ignored by | evidence |
|------|---------------|----------|
| `encoder::tests::a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders` | `lane-av1probes` (`8772e338`) | `#[ignore = "1080p encode: minutes, run it with --ignored"]` present at `745c60e7:crates/ec-av1/src/encoder.rs:6092`, absent at HEAD |
| `stream::tests::pinned_warp_stream_decodes_pixel_exact` | `lane-av1fixlib` (`2364d7a7` + `53b851a7`, "the warp gate's fourteen pins are committed") | `#[ignore = "reads pinned fixture paths under the gitignored fixtures dir…"]` removed and the pin list rewritten to `crate_pin` |

Attribute census on the merged tree:

```
$ git grep -h -E '^\s*#\[ignore' 745c60e7 -- 'crates/ec-av1/src/*.rs' | wc -l   # 51
$ git grep -h -E '^\s*#\[ignore' HEAD    -- 'crates/ec-av1/src/*.rs' | wc -l   # 49
```

Confirmed on a **clean checkout** on a VPS host, not just the worktree:

```
$ cargo test -p ec-av1 --all-targets -- --list 2>/dev/null | grep ': test$' | wc -l   # 753
$ cargo test -p ec-av1 --all-targets -- --ignored --list 2>/dev/null | grep ': test$' | wc -l   # 49
```

11 ignore reasons rewritten with measured walls / named env vars, e.g.

* `wall table, 16 cells x 2 passes at 1080p: 27 min measured; needs a host with >=8 cores`
* `20 min at 1080p and it TIMES OUT on a 4-core host -- run it on >=8 cores`
* `scratch: needs EC_AV1_PIN plus EC_AV1_PIN_W/H/N at the pin's REAL geometry (defaults 64/64/1 are wrong for every other pin); prints the first divergent pixel per plane`

The un-ignored gate **executes on a clean runner**, proven on host
`fedora-8gb-nbg1-3` from the `git archive` checkout (no `#[ignore]`, no
`--ignored`):

```
$ cargo test -p ec-av1 --lib -- a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders --test-threads=1 --nocapture
running 1 test
test encoder::tests::a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders ...
  1080p 2x1 tiles: 84488 bytes, sample-exact
  1080p 2x2 tiles: 84208 bytes, sample-exact
  1080p 4x2 tiles: 84685 bytes, sample-exact
ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 752 filtered out; finished in 1138.43s
```

All three byte counts are the lane's own (84488 / 84208 / 84685). The lane
measured 30.6 s release / 64 s on a 4-core VPS; the **1138 s** here is the same
gate in the suite's *debug* profile on a 4-core host, so the un-ignore costs
about 19 minutes of plain-suite wall clock there. That is a fact the next
wave's batch planner needs, not an objection to the un-ignore: the gate
asserts a per-frame first-differing-luma-sample comparison through BOTH
decoders, which is the whole point of un-ignoring it.


Inter-timing print: the merged `encode.rs` block replaces the additive
five-bucket total with an explicit non-additive overlap
(`let overlap = interpolation.saturating_sub(motion_search);`) and labels bucket
1 "OVERLAPPING, not additive" — a reporting change to an `#[ignore]`d print,
no decode path.

---

## 3. Name-set delta (`comm`, both directions)

Pre-merge list taken from the live tree at `745c60e7` before the first merge;
post-merge list from the tip. Both from `cargo test -p ec-av1 --all-targets -- --list`.

```
$ comm -13 names_pre.txt names_post.txt     # only in POST (added)
refusal_inventory::tests::a_body_carrying_another_function_is_refused
refusal_inventory::tests::a_gate_body_is_bounded_by_its_own_closing_brace
refusal_inventory::tests::every_named_gate_body_is_bounded_in_these_files
stream::tests::a_lossless_444_sub8_rect_leaf_chroma_reach_decodes_the_key_frame_pixel_exact

$ comm -23 names_pre.txt names_post.txt     # only in PRE (removed)
                                     (empty)

749 -> 753  (+4, -0)
```

Attribution: 3 from `lane-av1refusalspan` (`refusal_inventory.rs`), 1 from
`lane-av1loss444kf`
(`a_lossless_444_sub8_rect_leaf_chroma_reach_decodes_the_key_frame_pixel_exact`,
the witness gate). `lane-av1fixlib` and `lane-av1probes` add **no** test names:
fixlib's work is scripts + fixtures, probes' is ignore-reason prose and the
un-ignore of an existing test.

**Nothing was renamed or dropped.** That is the wave-3c-a lesson applied: a
tail-append conflict can silently duplicate or lose a definition, so the
removed set is checked empty rather than eyeballed.

---

## 4. Integration fixes

### Fix #1 — the two `never_exercised_*_matches_the_gate_recipes` reds: **already resolved, no edit made**

The ticket asks to delete `"enable-rect-tx"` from the 8-bit list and
`"enable-dual-filter"` + `"enable-rect-tx"` from the 10-bit list. **Those
entries do not exist in this tree, and did not exist before this wave either.**
Measured:

```
$ python3 -c "… parse NEVER_EXERCISED_8BIT / NEVER_EXERCISED_10BIT …"
NEVER_EXERCISED_8BIT -> []
NEVER_EXERCISED_10BIT -> []

$ git show 745c60e7:crates/ec-av1/src/gate_coverage.rs | <same parse>
NEVER_EXERCISED_8BIT -> []
NEVER_EXERCISED_10BIT -> []
```

Both lists carry only prose comments where the entries used to be, ending in
`];`. The deletion already happened, in the **merge that brought
`lane-av1txsearch` into main**:

```
$ git diff 4aebe74d 1cc2f250 -- crates/ec-av1/src/gate_coverage.rs | grep -E '^[-+].*enable-(rect-tx|dual-filter)'
-        "enable-rect-tx",
+    // `enable-rect-tx` LEFT this list on 2026-09-29 (merge wave 2,
-        "enable-dual-filter",
+    // `enable-dual-filter` LEFT this list on 2026-09-29 (merge wave 2,
-    ("enable-rect-tx", "hole at both depths, see the 8-bit list"),
+    // `enable-rect-tx` LEFT this list on 2026-09-29 (merge wave 2,

$ git log -1 --format='%H %P %s' 1cc2f250
1cc2f250d98fce423e6b08e93817c33b9be0303e 9b2f6c9ddb5c704e7649900c704b911e6822cd43
  4aebe74d4bb4c32f25d17aa3daaf4537aa77a50e
  Merge lane-av1txsearch @ 4aebe74d — format!-flag detector, enable-rect-tx entry, unresolvable==0 assert
```

Walking the history of the two lists commit-by-commit shows the entries present
at `4aebe74d` and absent from the merge that resolved it forward — the ticket's
red is the **pre-`1cc2f250`** state, one wave behind the tree I was given.

Both tests therefore pass on the merged tree, run with `touch
crates/ec-av1/src/gate_coverage.rs` first so the `include_str!` snapshot is the
post-merge text:

```
$ touch crates/ec-av1/src/gate_coverage.rs
$ cargo test -p ec-av1 --lib -- gate_coverage::tests::never_exercised
test gate_coverage::tests::never_exercised_10bit_matches_the_gate_recipes ... ok
test gate_coverage::tests::never_exercised_8bit_matches_the_gate_recipes ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 751 filtered out; finished in 0.18s
```

Whole module, with the census printed:

```
$ cargo test -p ec-av1 --lib -- gate_coverage --nocapture
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 740 filtered out; finished in 0.87s
NEVER_ON_8BIT (0 of 10, over 217 8BIT gates):
NEVER_EXERCISED_8BIT (0 of 26):
NEVER_EXERCISED_10BIT (0 of 26):
NEVER_ON_10BIT (0 of 10, over 118 10BIT gates):
gate_coverage: 263 real-aomenc gates, 116 of them 10-bit
enable-tx-size-search over 263 census-selected gate bodies: =0 only 88, =1 only 13, both 20, unresolvable 0, not named 142
```

Independently reproduced on VPS host `fedora-8gb-nbg1-3` from the `git archive`
checkout: `13 passed; 0 failed`.

**No commit was made for this item.** Making the named edit would have meant
deleting comments that are the record of *why* the entry left the list.

### Fix #2 — the five `.obu` "in the index but not in the tree": **false, they are committed**

The claim is that `444_lossy_rect4_odd_130x122`, `444_lossy_rect4_wide_256x128`,
`444_lossy_superres_256x128_d9`, `444_lossy_superres_256x128_d12` and
`444_lossy_superres_mode2_256x128` sit in the git index but not in the
committed tree, and are absent from the VPS library. Three independent
measurements say otherwise.

**(a) In the committed tree**, with sizes:

```
$ git cat-file -e HEAD:<path> && git cat-file -s HEAD:<path>
8945 / 15239 / 17437 / 14808 / 16620   (one per file, all exit 0)
$ git ls-tree HEAD crates/ec-av1/fixtures/ | grep -E '444_lossy_(rect4|superres)'
100644 blob 4ea09b93e94748bd7bec0cf5d92510560a0e6a99	…/444_lossy_rect4_odd_130x122.obu
100644 blob 98599fb2fbddbbdd145b93eabf456832e9922880	…/444_lossy_rect4_wide_256x128.obu
100644 blob ffa96b126624768a7835267b1d0bda1bdca94d6f	…/444_lossy_superres_256x128_d9.obu
100644 blob 58a1674ff42cf79dd0c18d46bdbac9739bb79ff8	…/444_lossy_superres_256x128_d12.obu
100644 blob a6b9763ca4bce74e1fc54db597ee373f171c86e7	…/444_lossy_superres_mode2_256x128.obu
```

**(b) Index == HEAD == worktree**, all three byte-identical:

```
$ for f in …; do w=$(git hash-object $p); i=$(git rev-parse HEAD:$p); echo "$f $w $i $( [ "$w" = "$i" ] && echo MATCH || echo DIFF )"; done
444_lossy_rect4_odd_130x122        worktree=4ea09b93…  head=4ea09b93…  8945   MATCH
444_lossy_rect4_wide_256x128       worktree=98599fb2…  head=98599fb2…  15239  MATCH
444_lossy_superres_256x128_d9     worktree=ffa96b12…  head=ffa96b12…  17437  MATCH
444_lossy_superres_256x128_d12    worktree=58a1674f…  head=58a1674f…  14808  MATCH
444_lossy_superres_mode2_256x128  worktree=a6b9763c…  head=a6b9763c…  16620  MATCH

$ git status --porcelain -- crates/ec-av1/fixtures/     # empty == clean
```

There is no stale index entry to delete and nothing to commit: all five landed
in `a9e5034e` (av1444edge, two cells), `e47d8f60` (av1superpin, two cells) and
`46e0f19d` (av1444edge r3, the first superres cell), all ancestors of the wave
base.

**(c) The manifest sees them, with sha256 and `tracked=yes`** — the preflight's
own inventory, which is what a host consumes:

```
$ grep -E '444_lossy_(rect4_(odd|wide)|superres_(256x128_d9|256x128_d12|mode2_256x128))' scripts/fixture-library.tsv
crates/ec-av1/fixtures/444_lossy_rect4_odd_130x122.obu         crates/ec-av1/src/stream.rs:47096  captured  -  ok  yes  c87ac65b…
crates/ec-av1/fixtures/444_lossy_rect4_wide_256x128.obu        crates/ec-av1/src/stream.rs:47104  captured  -  ok  yes  06621606…
crates/ec-av1/fixtures/444_lossy_superres_256x128_d9.obu      crates/ec-av1/src/stream.rs:47632  captured  -  ok  yes  3d619ee0…
crates/ec-av1/fixtures/444_lossy_superres_256x128_d12.obu     crates/ec-av1/src/stream.rs:47246  captured  -  ok  yes  450caa3e…
crates/ec-av1/fixtures/444_lossy_superres_mode2_256x128.obu   crates/ec-av1/src/stream.rs:47689  captured  -  ok  yes  c18496cf…
crates/ec-av1/fixtures/444_lossy_superres_mode2_256x128.obu   crates/ec-av1/src/stream.rs:47738  captured  -  ok  yes  c18496cf…
```

They are crate-local pins (`crates/ec-av1/fixtures/`), not library rows, so
"absent from the VPS fixture library" is not a defect either: the library is the
**root** `fixtures/` tree, and these five never belonged to it. They travel with
the commit — they are in the `git archive` tarball used for the VPS run
(`tar tzf … | grep fixtures/444_lossy_superres_mode2_256x128.obu` → present), and
on the host they are read through `crate_pin`.

**Disposition: no action.** The class the ticket worried about — a pin that is
not committed — is real and does exist in this repo, but it is not these five.
It exists as **two pins missing from the manifest**, which is Fix #3 below.

### Fix #3 (mine, `c548ae8f`) — the manifest was stale on the merged tree

Found by actually running the fixlib check the ticket asks for, on the merged
tree rather than on the lane's own tree:

```
$ EC_REQUIRE_FIXTURES=1 scripts/verify-fixture-library.sh
FAIL: this tree's library does not match what the code reaches
      (DRIFT: the committed manifest's rows differ from a regeneration here).
--- /dev/fd/63   +++ /dev/fd/62
-crates/ec-av1/fixtures/444_lossy_superres_256x128_d12.obu  …stream.rs:46912  captured  -  ok  -  450caa3e…
+crates/ec-av1/fixtures/444_lossy_superres_256x128_d12.obu  …stream.rs:47246  captured  -  ok  -  450caa3e…
verify-fixture-library: RED (library verdict: mode i, mode ii or drift)
EXIT=1
```

31 changed rows. Normalising away the `file:line` column to separate a line shift
from a real content change:

```
$ comm -13 <(rows mod line-number) <(regen mod line-number)     # only in regen
crates/ec-av1/fixtures/ll444_128root_lossless.obu  captured - ok yes  bbffb979d20d…
crates/ec-av1/fixtures/ll444_intrabc_rect_l2.obu   captured - ok yes  f0de080edce…
$ comm -23 …                                                     # only in committed
                                     (empty)
```

So: 29 line-number shifts (the merges moved code in `stream.rs`/`encode.rs`,
which the manifest records per row) plus **two rows that do not exist at all** —
`ll444_intrabc_rect_l2.obu` at `stream.rs:7248` and a third `ll444_128root_lossless.obu`
site at `stream.rs:7302`. Both are real pins (committed blobs `601adaa2`,
`b888abd1`), added by `5356cb1d` and their gates, and the manifest lane simply
predates them.

Regenerated and committed as its own commit (not folded into any merge), which
is also the action the preflight's own failure text prescribes
("or commit the regenerated manifest"):

```
$ ./scripts/gen-fixture-library.sh
gen-fixture-library: 305 rows -> scripts/fixture-library.tsv (191 unnamed media literals, 4 unreferenced committed pins)
$ EC_REQUIRE_FIXTURES=1 ./scripts/verify-fixture-library.sh   # after
verify-fixture-library: GREEN (305 rows)     EXIT=0
```

Idempotence proven before committing, because a self-feeding manifest is a
known failure mode: `regen(regen(tree)) == regen(tree)` (`diff -q` clean).

---

## 5. Staging discipline, and one thing that would have certified a false green

**Hash-checked staging, both hosts.** `git archive HEAD` (never a worktree tar),
scp, extract under `$HOME`, then `comm -3` the
`find crates -type f | sort | xargs sha256sum` lists:

```
# both keys re-sorted, because sha256sum output starts with the hash and
# comm on the raw lines reports "not in sorted order" then interleaves
# IDENTICAL rows as if they differed — the very false reading this check exists to prevent
$ awk '{print $2"\t"$1}' local-crates.sha256 | LC_ALL=C sort > local.ps
$ awk '{print $2"\t"$1}' hostA.sha256     | LC_ALL=C sort > hostA.ps
$ comm -3 local.ps hostA.ps ; echo $?
                                    (no output)
0
rows local=495 rows hostA=495
```

Host `2.28.124.204`: empty, exit 0, 495 = 495. Host `178.105.165.182`:
identical, 495 = 495. Both staging dirs also carry the two gitignored
`lanes/*.expected.txt` dumps (24440 / 10129 B) that two earlier reds were pure
staging gaps for.

**Fleet correction taken from Main**: `51.195.223.40` is DOWN
(`connect … port 22: Connection timed out`); the corrected login is
`tCloud@<ip>`. Both runs below are on the two live hosts, one heavy job each.

**The staging gap I hit and closed.** The first staging on both hosts had **no
root `fixtures`**, and the suite was launched that way. Five minutes in, the
un-ignored 1080p gate printed:

```
test encoder::tests::a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders ... SKIP the 1080p tile round trip: no fixture
ok
```

`h264_clip_frames` (`crates/ec-av1/src/encoder.rs:3251`) resolves its clip at
`CARGO_MANIFEST_DIR/../../fixtures/video/h264-1080p-23.976-8bit.mp4` — the
root, gitignored tree. The clip **is** on both hosts
(`library/fixtures/video/h264-1080p-23.976-8bit.mp4`, 3320956 B); the staging
dir simply had no `fixtures` entry, so the gate skipped GREEN having read
nothing. The suite on `2.28.124.204` was **stopped and relaunched** with
`ln -sfn ~/gates/library/fixtures ~/gates/wave3cb/fixtures` (it was 5 minutes
in, 59 tests). Re-run with the symlink in place, the gate executes:

```
=== 1080p MULTI-TILE GATE, PLAIN RUN (root fixtures now present)
test encoder::tests::a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders ... ok
```

Sharp edge worth naming, because it is a silent-skip class the preflight does
**not** cover: pointing `EC_FIXTURES` at the real library makes RESOLVE report
`0 missing, 0 empty` and the preflight GREEN **while the gates' own root is
absent**, because the preflight validates a path (`$EC_FIXTURES/…`) that the
clip-reading gates never read (`$ROOT/fixtures/…`). The two roots can diverge
and the preflight cannot see it. The mitigation is the documented symlink; the
residual is that `EC_FIXTURES` gives a *false* green when the symlink is
missing. Not fixed here — it is a change to another lane's guard, and the
correct fix (have RESOLVE check the root the tests resolve through) is a design
decision for that lane's owner.

---

## 6. VPS full suite on the merged tree

### 6.0 The baseline this is measured against

`745c60e7` is already certified on the fleet, so the post-merge run's value is
the **delta**. Baseline, read directly off the host rather than taken on trust:

```
$ ssh tCloud@51.195.223.40 'cat ~/gates/pm2-av1.rc'
0
$ … 'grep -m1 "^running " ~/gates/pm2-av1.log; grep -m1 "^test result:" ~/gates/pm2-av1.log'
running 748 tests
test result: ok. 697 passed; 0 failed; 51 ignored; 0 measured; 1 filtered out; finished in 20234.61s
$ … 'grep -c SKIP ~/gates/pm2-av1.log'
0
$ ls -ld ~/gates/repo-postmerge2/fixtures
… -> /home/tCloud/gates/library/fixtures
```

Three things make it a fair comparison rather than a number to be argued with:
748 running + 1 filtered = 749 names, which is exactly the `--all-targets`
count I took at `745c60e7` before merging; **zero** `SKIP` lines, so no gate
skipped green having read nothing; and that staging dir carries the root
`fixtures` symlink, i.e. the same library-reachability state as my run (the
state whose absence made my first staging silently skip — §5).

Name arithmetic for the wave, from the measured lists rather than from the
lane reports: 749 names at the base, 753 at the tip (+4 added, −0 removed),
and 51 → 49 `#[ignore]` attributes (−2 un-ignored, attributed in §2.4). So
dispatched (total − ignored) goes 698 → 704, and the expected pass count is
**697 + 6 = 703** (or 704 if the base run's single "filtered out" no longer
filters). Measured numbers below.

### 6.1 The merged-tree run

**STATUS: STILL RUNNING at the time of writing — the totals are NOT in.** I am
reporting that rather than a prediction. Every other number in this report is
measured.

Host `tCloud@2.28.124.204` (`fedora-8gb-nbg1-2`), unit `wave3cb-suite.service`,
staging `~/gates/wave3cb`, log `~/gates/wave3cb/suite.log`:

```
systemd-run --user --unit=wave3cb-suite \
  --property=MemoryMax=6G --property=WorkingDirectory=$HOME/gates/wave3cb \
  --setenv=CARGO_TARGET_DIR=$HOME/gates/target-wave3cb --setenv=TMPDIR=$HOME/gates/tmp-wave3cb \
  --setenv=EC_AV1_REQUIRE_AOMENC=1 --setenv=EC_AV1_REQUIRE_FFMPEG=1 --setenv=EC_NOMEMGUARD=1 \
  --setenv=EC_FIXTURES=$HOME/gates/library/fixtures --setenv=EC_REQUIRE_FIXTURES=1 \
  --setenv=EC_AV1_AOMENC=$HOME/.cache/aom-oracle/build/aomenc \
  --setenv=EC_AV1_AOMDEC=$HOME/.cache/aom-oracle/build/aomdec \
  --setenv=PATH=$HOME/.cargo/bin:$HOME/gates/bin:$HOME/.cache/aom-oracle/build:/usr/local/bin:/usr/bin:/bin \
  /bin/bash $HOME/gates/wave3cb/run.sh

# run.sh: preflight -> pin-gate-audit --self-test -> cargo test -p ec-av1 --lib -- --test-threads=1
```

The same unit also ran the preflight and the audit self-test **first**, so those
two verdicts are already final and are quoted in §2.3. Launched 09:00 UTC on
2026-09-29; the equivalent certified run at `745c60e7` took 20234 s on this
fleet, and this one carries the newly un-ignored 1080p gate (+1138 s measured
in debug on a 4-core host), so ~6 h is the expected wall.

Read the result with:

```
ssh tCloud@2.28.124.204 "bash -c 'grep -E \"^running |^test result:\" ~/gates/wave3cb/suite.log; \
  echo ok=\$(grep -c \"ok\$\" ~/gates/wave3cb/suite.log) \
       ignored=\$(grep -c ignored ~/gates/wave3cb/suite.log) \
       FAILED=\$(grep -c FAILED ~/gates/wave3cb/suite.log) \
       SKIP=\$(grep -c SKIP ~/gates/wave3cb/suite.log)'"
```

The `SKIP=` count is not decoration: it is the check that would have caught the
staging gap in §5, and the certified baseline's `SKIP` count of 0 is what makes
697 a comparable number.

**Second run, in parallel, for the delta.** Host `tCloud@178.105.165.182`
(`fedora-8gb-nbg1-3`), unit `wave3cb-base.service`, staging `~/gates/wave3cb-base`,
log `~/gates/wave3cb-base/suite.log`: the **pre-wave** tree `745c60e7` staged by
the identical recipe (same `git archive` → `git add -A` index → root `fixtures`
symlink → same `--setenv` block, only the `CARGO_TARGET_DIR` differing), so the
baseline is measured in-session rather than quoted from another peer's log.
Its result should reproduce `697 passed; 0 failed; 51 ignored`; if it does not,
the discrepancy is itself the finding.

Expected, from the name arithmetic in §6.0 and the per-branch attribution in
§3 and §2.4 — stated as a prediction to be checked, not as a result:

| | base `745c60e7` | merged tip `c548ae8f` | delta | why |
|--|--|--|--|--|
| names | 749 | 753 | +4 | 3 `refusal_inventory` + 1 `loss444kf` witness |
| `#[ignore]`d | 51 | 49 | −2 | 1080p multi-tile (probes), warp pin gate (fixlib) |
| dispatched (total − ignored) | 698 | 704 | +6 | |
| expected passed | 697 | **703** | **+6** | 4 new gates + the 2 un-ignored gates, all green here |
| expected failed | 0 | 0 | 0 | |

Where each of the six lands, all six measured green on the merged tree before
the suite was staged: the three `refusal_inventory` tests inside the 19/0 module
run, the `loss444kf` witness inside the 49/0 `-- lossless 444` battery, the
warp pin gate inside a plain `--nocapture` run on the clean checkout
(`warp_selected_hits 99 → 104 → 113 → 113`, `ok`), and the 1080p gate's three
tile grids (84488 / 84208 / 84685 bytes, sample-exact).

Accounting that must close when the run lands:
`passed + failed + ignored == dispatched` — i.e. `703 + 0 + 49 == 704` minus
whatever the base run's single "filtered out" does on this tree. If the merged
run reports `1 filtered out` too, `702 + 0 + 49 + 1 == 752` with `753` names and
the missing test is the one to name.

---

## 7. Retired, not merged

| branch | tip | why not merged | evidence |
|--------|-----|----------------|----------|
| `lane-av1refusal` (Tolga-4) | `910dc296` | **Superseded by `lane-av1refusalspan`**, which is the corrected version of the same test over the same rows' gate bodies. Merging both would land two competing window-boundary implementations of one function. | Same merge base (`ea97eb0c`); both rewrite `refusal_inventory.rs`'s anchor scan — refusal `+346/-13`, refusalspan `+740/-26`. The span lane additionally ships `body_braces`/`gate_definitions`/`last_fn_name`/`leading_clause` plus two mutation-driven hardenings (`81eaadfd` stray-closer, `566d61bd` spin guard) that the refusal branch has no counterpart for. |
| `lane-av1fixtureshape` | `3be839be` | **Contained in `lane-av1fixlib`** — verified by ancestry at the tip, not by diff. | `git merge-base lane-av1fixlib lane-av1fixtureshape` == `3be839be` == `git rev-parse lane-av1fixtureshape`; `git merge-base --is-ancestor` → contained. **Re-checked at merge time** (skill://lane-merge-order: containment can change sign as a tip moves). |
| `lane-av1refusalfix` | `4741f0d4` | **Report-only and superseded.** Its single commit is `lanes/av1refusalfix.report.md` (+287 lines) — zero code. Its own message says "item 5 walker test built, direct-call parse unsolved, reverted", and items 1–3 are already closed by `main`. | `git diff --stat lane-av1refusalfix~1..lane-av1refusalfix` → `lanes/av1refusalfix.report.md | 287 +++` and nothing else. |

---

## 8. Things not taken on trust

| claim | how it was checked | result |
|-------|--------------------|--------|
| four tips at the assigned shas | `git rev-parse` on the live branch + `git worktree list` | all four match; none moved |
| "no conflicts" | 10 `git merge-tree --write-tree` predictions, then `cargo check -p ec-av1 --all-targets` after every merge | 0 conflicts, 4 clean checks — and the guard families run *after the last* merge, not trusted from any lane's own green |
| the two `never_exercised_*` reds | parsed the lists at HEAD **and at `745c60e7`** | entries already gone since `1cc2f250`; no edit needed |
| the five `.obu` are uncommitted | `cat-file -e HEAD:<path>`, `ls-tree`, `hash-object` vs `rev-parse`, `status` | all five committed, index==worktree==HEAD |
| fixlib preflight is green | ran it on the merged tree, not on the lane's | RED (drift) → regenerated manifest → GREEN; the lane's own claim would have been a false green |
| `--ignored --list` 51 → 50 | attribute census at both shas + clean-checkout count on a VPS host | 51 → **49**; second un-ignore is fixlib's warp gate |
| the un-ignored 1080p gate runs | ran it on a clean VPS checkout, twice | first SKIP (no `fixtures` symlink), then executes |
| the shared `CARGO_TARGET_DIR` | never used it | lane-private `…/tgt/merge3cb` for every local cargo invocation |
