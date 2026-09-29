# lane-av1f9singleland — the "unlanded" fix was ALREADY ON MAIN (census false positive); landed instead the arm-attribution gap: the four-unit 4:4:4 witness is 88/96 single-reference and 8/8 of the non-vacuity bar, and no gate said so

## 1. The premise is false: `c7fd5afa` is a duplicate, not an unlanded fix

The ticket said the fix on `lane-av1-f9single` tip `c7fd5afa` "never reached
main" and that "main still reads `if side > 64` around `decode.rs:42271`".
Both statements are wrong. The cherry-pick and the blame:

```
$ git -C ~/.cache/wt/av1f9singleland cherry-pick --no-commit c7fd5afa
Auto-merging crates/ec-av1/src/decode.rs
CONFLICT (content): Merge conflict in crates/ec-av1/src/decode.rs
error: could not apply c7fd5afa...
```

```
$ git merge-base --is-ancestor 6c33f204 HEAD && echo YES   # the "compound twin"
YES
$ git merge-base --is-ancestor c7fd5afa HEAD && echo YES   # the "unlanded" fix
NO
```

`git blame` on the single-reference arm's two sites attributes BOTH to
`6c33f204` — the commit the ticket describes as the *already-landed compound
twin*:

```
$ git blame -L 42250,42262 -s crates/ec-av1/src/decode.rs
6c33f2047 42261)   leaf_tx_types.push((row, col, tw, th, tu_tx_type));
$ git blame -L 42636,42644 -s crates/ec-av1/src/decode.rs
6c33f2047 42636)   let cu_rel_mi = (cr * (cu_tx / MI), cc * (cu_tx / MI));
6c33f2047 42637)   let cu_tx_type = covering_leaf_tx_type(&leaf_tx_types, cu_rel_mi)
6c33f2047 42639)   if covering_leaf_tx_type(&leaf_tx_types, cu_rel_mi).is_some() {
6c33f2047 42640)       hit!(CHROMA_QUAD_LEAF_TX_HITS);
6c33f2047 42641)       if cu_tx_type != luma_tx_type {
6c33f2047 42642)           hit!(CHROMA_QUAD_LEAF_TX_DIFF_HITS);
```

`6c33f204` ("4:4:4 inter chroma units inherit their own quadrant's luma
tx_type") collected `leaf_tx_types` at EVERY size and pointed BOTH copies of
the four-unit arm at the covering lookup. Its own message calls the
single-reference side "the single-reference twin" — the commit was a
twin-flow sweep, not a compound-only change, and the branch census read the
*commit subject* rather than the diff.

**Main's form is also the better one.** The branch's version inlines a
four-clause `.iter().copied().find(...)` closure; main extracts the shared
`covering_leaf_tx_type` (decode.rs:38785) that the intra 128 path, the rect
sibling and both four-unit arms all call. Cherry-picking the branch would have
reintroduced a fourth inline copy of a convention main had already
deduplicated. So the conflict is a genuine "do not take this" signal, not
something to resolve by hand.

The `if side > 64` the ticket pointed at (decode.rs:42304 on this tree) is a
DIFFERENT guard — the 128x128 mu-chunk walk from lane-sb128c r1, which is
correctly `side > 64`. The leaf-push guard the fix was about no longer exists;
the push is unconditional.

## 2. What was actually missing: the witness cannot say which arm it witnessed

`CHROMA_QUAD_LEAF_TX_HITS` and `..._DIFF_HITS` are bumped from **both** arms
of the four-unit 4:4:4 chroma arm, and
`a_pinned_444_inter_stream_chroma_units_inherit_their_own_quadrants_tx_type`
asserts only that pair. `decode_inter_block` carries two textually parallel
copies of that arm — the compound prediction arm (the `build` closure at
decode.rs:39879) and the single-reference arm — so either can be reverted
while the other keeps the shared total comfortably non-zero.

Measured by arm, on the existing witness `fixtures/444_quad_leaf_tx_type.obu`
(27933 B, sha256 `a06c9f7a0862252f6ab7ebdcec340d0602e730e38089365f14747db37f8e78d5`),
with a temporary arm-separating counter:

```
TEMP-PROBE 444_quad_leaf_tx_type.obu: frames 4, quad 96 (diff 8), of which SINGLE-REF 88 (sr diff 8)
TEMP-PROBE 444_rect_strip_leaf_tx_type.obu: frames 3, quad 8 (diff 2), of which SINGLE-REF 8 (sr diff 2)
```

**88 of 96** covering-leaf units are the single-reference arm's, and **all 8**
of the DIFFERS units — the number the existing gate uses as its non-vacuity
bar — are the single-reference arm's. The compound arm contributes 8 of 96
route hits and none of the 8 that change an answer. The gate's non-vacuity
number is 100% single-reference, and nothing in the tree said so.

The other 26 pinned 4:4:4 fixtures reach the arm **zero** times
(`444_intrabc_rect4_witness`, `444_leaf8_oob`, `444_lossy_rect4_*`,
`444_lossy_superres_*`, `444_sb128rect_lr_witness`, and every `ll444_*`), so
that one witness is the only thing pinning the single-reference copy at all.

## 3. Change (additive: +149/-0, no decode-semantics edit)

- `decode.rs`: `CHROMA_QUAD_LEAF_TX_SINGLEREF_HITS` +
  `CHROMA_QUAD_LEAF_TX_SINGLEREF_DIFF_HITS` and their accessors, bumped only
  in the single-reference arm.
- `stream.rs`: `the_pinned_444_quadrant_witness_is_the_single_reference_arms_and_not_only_the_compound_arms`
  — asserts the single-ref arm holds the majority of the shared total, that
  its own DIFFERS count clears the non-vacuity bar, and that the 4:2:0 control
  fires neither new counter.

The compound arm is untouched; the two arms stay textually comparable, which is
the point of a twin:

```rust
// COMPOUND arm, decode.rs:40876-40885              // SINGLE-REF arm, decode.rs:42679-42693
let cu_rel_mi = (cr * (cu_tx / MI), cc * (cu_tx / MI));   let cu_rel_mi = (cr * (cu_tx / MI), cc * (cu_tx / MI));
let cu_tx_type = covering_leaf_tx_type(&leaf_tx_types,    let cu_tx_type = covering_leaf_tx_type(&leaf_tx_types,
    cu_rel_mi)                                                cu_rel_mi)
    .unwrap_or(luma_tx_type);                                     .unwrap_or(luma_tx_type);
if covering_leaf_tx_type(&leaf_tx_types, cu_rel_mi)         if covering_leaf_tx_type(&leaf_tx_types, cu_rel_mi)
    .is_some() {                                                 .is_some() {
    hit!(CHROMA_QUAD_LEAF_TX_HITS);                              hit!(CHROMA_QUAD_LEAF_TX_HITS);
    if cu_tx_type != luma_tx_type {                              hit!(CHROMA_QUAD_LEAF_TX_SINGLEREF_HITS);
        hit!(CHROMA_QUAD_LEAF_TX_DIFF_HITS);                      if cu_tx_type != luma_tx_type {
    }                                                              hit!(CHROMA_QUAD_LEAF_TX_DIFF_HITS);
}                                                                  hit!(CHROMA_QUAD_LEAF_TX_SINGLEREF_DIFF_HITS);
                                                                  }
                                                              }
```

## 4. Mutation / red-before: three independent reds, all on the single-ref arm

The two hunks `c7fd5afa` proposed were each re-applied as a mutation to prove
the single-reference arm is load-bearing on its own, and the new gate was
proven non-vacuous a third way.

**Red 1 — covering lookup reverted to the block-level type** (hunk 2 of the
proposed fix, `Some(cu_tx_type)` -> `Some(luma_tx_type)`, single-ref arm only):

```
a_pinned_444_inter_stream_chroma_units_inherit_their_own_quadrants_tx_type:
  decode-order frame 1 of 4 (4 shown, 0 hidden) differs from the oracle at byte
  36635 (ours 127 vs 126), 777 bytes differ
test result: FAILED. 0 passed; 1 failed
```

**Red 2 — leaf push gated back behind `if side > 64`** (hunk 1, the pre-`6c33f204`
state, single-ref arm only):

```
a_pinned_444_inter_stream_chroma_units_inherit_their_own_quadrants_tx_type:
  decode-order frame 1 of 4 (4 shown, 0 hidden) differs from the oracle at byte
  36635 (ours 127 vs 126), 777 bytes differ
test result: FAILED. 0 passed; 1 failed
```

777 bytes, the same number `6c33f204` reports for its own pre-fix state —
confirming the single-reference arm was the arm that needed the fix all along,
and that the pre-`6c33f204` defect really was 777/640/554 wrong V samples.

**Red 3 — the new gate's own non-vacuity.** The single-reference arm's covering
route disabled, leaving the compound arm intact:

```
the_pinned_444_quadrant_witness_is_the_single_reference_arms_and_not_only_the_compound_arms:
  the single-reference arm resolved 0 of 8 covering-leaf chroma units -- the shared
  counter is no longer dominated by the arm this gate witnesses, so a revert of the
  single-reference copy would leave the shared total comfortably non-zero
test result: FAILED. 0 passed; 1 failed
```

Note the shape of Red 3: the shared total is still **8** (all compound). A
gate reading only the shared counters sees a healthy non-zero number while the
arm it exists to witness is dead. That is the gap, demonstrated.

**Green after restore** (both mutations reverted):

```
a_pinned_444_inter_stream_chroma_units_inherit_their_own_quadrants_tx_type:
  quad-resolved chroma units 96, of which 8 differed                                    ... ok
the_pinned_444_quadrant_witness_is_the_single_reference_arms_and_not_only_the_compound_arms:
  88 of 96 covering-leaf units are the single-reference arm's, 8 of which changed an answer ... ok
```

## 5. Neighbouring gates re-run (all green, all with the oracle required)

`EC_AV1_REQUIRE_AOMENC=1` against the instrumented
`~/.cache/aom-oracle/build/{aomenc,aomdec}`, so no arm silently skipped:

| Set | Filter | Result |
| --- | --- | --- |
| 444 four-unit arm (existing) | `quadrants_tx_type` | 1 passed, 0 failed — 96 units, 8 differed |
| 444 four-unit arm (NEW) | `single_reference_arms` | 1 passed, 0 failed — 88 of 96, 8 changed an answer |
| 4:4:4 family | `444` | **39 passed**, 0 failed (18.66s) |
| intrabc_rect family | `intrabc_rect` | **7 passed**, 0 failed |
| rect-strip sibling | `rect_strip` | **10 passed**, 0 failed (15.94s) |
| `covering_leaf_tx_type` unit test | `leaf_tx_type` | 1 passed, 0 failed |
| 4:2:0 control | `inter_block` | 3 passed, 0 failed |
| 4:2:0 chroma tx | `chroma_tx` | 2 passed, 0 failed |
| 4:2:0 streams | `420` | 2 passed, 0 failed |

`cargo check -p ec-av1 --all-targets`: clean, **0 warnings** (private
`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1f9singleland`).

The 4:2:0 sets are unchanged because the covering consumer is gated on
`ss_x == 0 && ss_y == 0`; at 4:2:0 the arm is unreachable and the new
counters are asserted at exactly `(0, 0)` there by the new gate's own control.

## 6. Merge verification onto the advanced main tip

Main advanced 35 commits past this lane's base (`3aa16dd1` -> `57834ee2`)
while the lane ran, so the merge was proved rather than assumed:

```
$ git merge-tree --write-tree main 0b5b46d8
45857ebeda17d40d140238accb80344f40331ade      # exit 0, no conflict
```

Built as a real two-parent commit and re-ran against it:

```
cargo check -p ec-av1 --all-targets   ->  Finished, 0 warnings
single_reference_arms  ->  88 of 96 covering-leaf units, 8 changed an answer   ok
quadrants_tx_type      ->  quad-resolved chroma units 96, of which 8 differed   ok
```

Same numbers as on the lane base, so nothing in those 35 commits moved the
witness.
 
## 7. Not done / out of scope

- The compound arm is still only 8-of-96 witnessed. A witness that exercises
  the compound four-unit arm's DIFFERS condition would need a stream whose
  *compound* blocks carry skipped luma leaves; the one pinned 444 witness does
  not. Not attempted — out of this lane's scope (it is a compound-arm coverage
  question, not a single-reference one), and flagged here rather than left
  implied.
- No new fixture was encoded. The census named `a6tools444_26frame_444_inter.obu`;
  no such fixture exists in this tree and no `aomenc` is on PATH, so its
  26-frame claim could not be re-measured. It is not needed: the census's own
  measurement, and this lane's, agree the fix is already in.
- The full `--lib` suite was not run: the box is under three concurrent sibling
  suites (`av1pinspec`, an `a_444_intrabc_owned_rect` lane, and a
  `--test-threads=4` run). Per-lane scoping only; project-wide validation is
  the integrator's call.
- `lanes/av1f9single.report.md` from the branch was NOT brought over. It
  documents measurements of an arm state that never existed on main, and its
  "counter left compound-only" claim is false against main (the counters are in
  both arms). Keeping it would re-seed the same false-positive census entry.
