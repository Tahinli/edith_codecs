# Branch reconciliation — landing (or refuting) the census's ranked residue

**Branch:** `lane-breconcile`, worktree `~/.cache/wt/breconcile`, forked from
`main`@`7538a841`.
**Nothing pushed, nothing merged to `main`, no rustfmt, no lane branch edited or
deleted.** The primary checkout was never written to (§6).

**Inputs.** `lanes/branchcensus2.report.md` §2, ranked 1-9. Its §1 snapshot was
`main`@`52c62c15`; rank #1 had already landed as `7538a841` before this lane
started, so this pass covers ranks #2-#9.

**The governing fact.** The census's §2 ranking is a snapshot of *what was
missing*, and it was measured, not argued: every classification in it survives.
What does NOT always survive is the *reason* it gave. Two of the nine items rest
on a rationale written against a weaker artifact that main has since replaced —
#2's `Proof::TablePin` and #4/#3's premise about which gate a row names. Those
were re-checked against main's current code and one is refuted (§1.3, §1.5).

---

## 1. One line per rank

| # | census item | branch | verdict | commit | evidence |
|---|---|---|---|---|---|
| 1 | `e55ad654` four mislabelled partition rungs | `lane-av1tilerows2` | **ALREADY LANDED** | `7538a841` (base) | out of scope for this pass |
| 2 | `Proof` enum + `Proof::TablePin` | `lane-av1refusal` | **LANDED, `TablePin` REFUTED** | `29951312` | 21/0; 3 red-before mutations; §1.3 |
| 3 | `6dfd827e` + `7586358f` + `c8ed05f3` | `lane-av1loss444mm` | **LANDED** (one helper dropped as dead) | `2be41696` | check 0/0; 248/0 pixel-exact; §1.5 |
| 4 | `bf88d5e1` two `set_symr_cdf` tags | `lane-av1tilerows2` | **LANDED** | `d100dc47` | check 0/0; 248/0; §1.4 |
| 5 | `571b4090` round-9 retraction | `lane-av1leafsize` | **LANDED** | `47bcb11e` | §1.5a — cherry-picked clean |
| 6 | `0beaf6c5` round-6 caller | `lane-av1444rect` | **LANDED** | `d3226c2e` | blob byte-identical; §1.5b |
| 7 | `f0ae03c8` + `f61ebe79` test-count census | `lane-av1testcount` | **LANDED, re-measured** | `13b752ff` | 712/742/802 re-derived; §1.7 |
| 8 | `5a1388f2` plane-tagged `OUR_MODE` | `lane-av1444chr` | **LANDED** | `b307aaeb` | check 0/0; 248/0; §1.8 |
| 9 | `6cc9ea6f` report header | `lane-av1chrtx` | **LANDED** (cosmetic) | `b307aaeb` | text-only; §1.9 |

Six source commits, two report-only, one report-only-plus-source. Diffstat of
the whole pass: **488 insertions, 19 deletions, 7 files** — 46 of the
insertions are report text.

### 1.2 What each landed commit actually is

Seven commits on `lane-breconcile`, oldest first:

```
13b752ff lanes/av1testcount: the 13-branch wave reconciled exactly, re-measured
b307aaeb ec-av1: the plane-tagged OUR_MODE per-TU mode trace; fix the readcensus header
2be41696 ec-av1: the lossless OUR_DQW twin, plane-tagged, with the raw levels beside dq
d100dc47 ec-av1: tag the EC_SYMR reads in read_eob and read_coeffs' tx_type
29951312 refusal_inventory: say WHICH kind of proof each PROVEN row carries
d3226c2e docs(av1444rect): round 6 -- the fork's caller is NAMED
47bcb11e lanes/av1tilerows: round 9 -- the 4:4:4 128-root leaf-size fork is gone on main
```

Every commit message states what was NOT landed with it and why. No open defect
was fixed: this lane lands instruments and records.

### 1.3 #2 `Proof` — the enum landed, `TablePin` is REFUTED

**What landed** (`29951312`): `enum Proof { NegativeGate, Enumeration }` with a
`label()`, a third tuple element on **all 32** `PROVEN` rows, both consumer
loops updated, and one new test.

The census's premise was that main's `PROVEN` is a 2-tuple so every row "looks
equally proven". That is true and it is worth fixing: 7 rows carry a gate that
drives a stream into the guard (`NEGATIVE`, red on removal), 25 carry a gate
that sweeps a domain and shows nothing reaches it (`ENUMERATION`, green on
removal). Those are different standards and the table said so nowhere.

**What is refuted** is `Proof::TablePin`. The census wrote, of the `(4,8)`
chroma-table row: *"its gate pins a table's arms exactly but the domain the
table is REACHED OVER is unestablished, so an `Enumeration` tag there would
claim unreachability the gate's own comment admits it has not shown."* That
comment is the BRANCH's, not main's. `lanes/branchcensus2.report.md:108-112`
quotes it as if it were main's — the census read the branch file.

On main that row names a different, stronger gate:
`every_chroma_unit_decode_block_rect_can_present_has_a_coefficient_table`
(`refusal_inventory.rs:1666`), which

1. reads all eight `decode_block_rect` call sites' own literal `bw`/`bh`
   (`assert_eq!(sites, 8, "the number of decode_block_rect call sites changed")`),
2. walks them through `bw >> ss_x` / `bh >> ss_y` for all three codable
   subsamplings and asserts each has a table row,
3. pins the table exactly (six arms), and
4. asserts the residual is UNPRODUCIBLE: `!produces.contains(&(4, 8)) &&
   !produces.contains(&(8, 4))`, "the caller set can now produce a 4x8/8x4
   chroma shape but the table has no such row -- the chroma refusal is LIVE".

The reach domain r3 called unestablished is measured. `Enumeration` is the
honest tag, and a third variant with zero rows is dead weight. The enum's doc
comment records exactly when to re-add it. (The census was right that the row
is *representable*; it was wrong about which way.)

**Non-vacuity — three mutations, each reverted, each RED:**

| mutation | result |
|---|---|
| flip the `(4,8)` row's tag to `NegativeGate` | RED — *"is tagged NEGATIVE …, but its gate enumerates the caller domain"* |
| turn all 7 `NegativeGate` rows into `Enumeration` | RED — *"no row is tagged NEGATIVE, so the NegativeGate variant is dead weight"* |
| **weaken the GATE**: replace `assert_eq!(sites, 8, "…call sites changed")` with `let _ = sites;` (the gate itself still passes) | RED — *"…no longer carries \"the number of decode_block_rect call sites changed\", so the Enumeration tag … claims a reach domain the gate has stopped measuring"* |

The third is the one that matters and the one a weaker test would miss: the tag
is pinned to evidence inside the gate's **body**, so a future edit that guts the
gate's caller-domain measurement reds the taxonomy rather than leaving the row
quietly overclaiming. Mutating the test's own expectation list instead would
have proved nothing, and did not.

**Suite:** `every_proven_refusal_names_a_test_that_exists`,
`every_named_gate_body_is_bounded_in_these_files`, and the new test —
**21 passed, 0 failed** (20 pre-existing + 1 new). Measured distribution printed
by the new test: **7 NEGATIVE, 25 ENUMERATION of 32 PROVEN rows**.

### 1.4 #4 — the two `set_symr_cdf` tags (`d100dc47`)

Two lines, `crates/ec-av1/src/decode.rs`. `set_symr_cdf` existed with 24 call
sites; nothing tagged the two reads the 4:4:4 partition-walk work pairs against
the oracle's per-read `site=`. `SYMR_CDF` is a `Cell<&'static str>` thread-local
reset after each symbol (`msac.rs:491`) and read only by the `EC_SYMR` eprintln,
so the tag is a trace label and nothing else; placement matches the 24 existing
`y_mode`/`uv_mode` tags.

### 1.5 #3 — the lossless `OUR_DQW` twin (`2be41696`)

Re-derived onto current main, and it did **not** cherry-pick: main has since
generalised `dequant_and_inverse_wht4x4` to any shape (lane-whtshape's 4x4
raster), so `TxParams::run` no longer calls the 4x4 entry. `plane` is threaded
through **both** functions so the raster path's tiles carry it too.

- `TxParams.plane`, set by the reader that OWNS the unit (`plane_idx` at the
  three multi-plane sites, `0` at `decode_leaf_rect8`, which is luma-only —
  `plane_q_delta(0, …)` and `push_intra(0, …)` at that arm).
- `OUR_DQW`, the LOSSLESS half of the oracle's `EC_DQCOEFF` rung, under the same
  env var, with `plane=` and `lv=` (the RAW level grid the dequant consumed, so
  "dequant produced nothing" is distinguishable from "the reader produced
  nothing").

**The open defect it instruments:** item (a) of `lanes/av1loss444mm.report.md`
r10 — 4:4:4 lossless **key frame**, reconstruction-only, 21 U + 28 V samples off
by up to ±63, luma exact, entropy bit-identical. Named there with a three-file
search space. Note the census's own caveat is kept: r9 already exonerated
dequant ("0 of 2053 units have non-zero levels with an empty `dq`"), so this rung
buys **diagnosis, not a fix**.

**One artifact dropped:** `census_plane()`, added by `7586358f`. On the branch's
own tip (`87d1518d`) it is defined and **never called** — `6dfd827e` replaced its
one use with the field. Not landed.

### 1.5a #5 — the round-9 retraction (`47bcb11e`)

`git cherry-pick -x 571b4090` **applied clean** (the census expected drift).
Verification is text, by blob:

- main's `lanes/av1tilerows.report.md` and the branch's differ — expected, and
  not a problem: main carries two RETRACTION blocks in §8
  (`RETRACTED by lanes/av1txsizeaudit.report.md §3.1` and *"a subsampling-
  parameterised expression can only be certified by the measured value at every
  subsampling"*) that the branch predates. Landing must ADD round 9 on top of
  those, not overwrite them.
- The round-9 section itself — the 45 lines from `## 17. Round 9` to EOF — is
  **byte-identical** to the branch's (`diff` of the `sed -n '/Round 9/,$p'` window
  of both blobs: no output).

This is the commit that changes what the record says: main ended at §8's claim
that one site is "proven reachable and documented", and round 9 measures 0
divergence over 36 frames and 217570 reads in entropy lockstep on merged main,
and explicitly refuses to attribute it to its own fix.

### 1.5b #6 — the round-6 caller (`d3226c2e`)

Also cherry-picked clean. Strongest verification available:

```
git rev-parse lane-breconcile:lanes/av1444rect.report.md
  = 69c428477984ea530d1fed94807c1876febd7820
git rev-parse lane-av1444rect:lanes/av1444rect.report.md
  = 69c428477984ea530d1fed94807c1876febd7820
```

Byte-identical blobs. Main's §7 said the 4:4:4 intra-BC chroma fork "needs a
fresh start, not a continuation"; round 6 names it:
`decode_intrabc_rect`'s non-lossless whole-block rect arm, one
`around_mi_rect((mi_r, mi_c), bw, bh)` gather taken at the **luma** footprint and
reused for a 4x8 chroma unit under a 4x16 luma plane block — coincident at 4:2:0
after subsampling, not at 4:4:4. That defect is still open; this records its call
site and does not fix it.

### 1.7 #7 — the test-count census, re-measured (`13b752ff`)

The census called this stale and said to land it only with a fresh count. Both
anchors were re-derived from scratch — fresh `git archive` extracts, private
target dirs, `cargo test -p ec-av1 --lib -- --list`:

```
298f75c4 (pre-merge base)   712 raw --list lines, 712 unique
9623bcab (merged main)      742 raw, 742 unique
b307aaeb (this tree)        802 raw, 802 unique
```

**The reconciliation stands**: 712 + 30 distinct = 742, delta 0, and zero
duplicate registrations at both anchors. Those are immutable facts about
immutable commits. But the tree this report would now be compared against is
**802**, and the report says so in a header paragraph so no reader mistakes it
for a count of the present.

The 13 per-branch tip counts were **not** re-taken (13 more throwaway builds) and
the report states which figures are re-verified and which are the lane's own.

### 1.8 / 1.9 #8 and #9 (`b307aaeb`)

`5a1388f2` cherry-picked clean — the only source commit in the census's set that
did. 13 lines, env-gated print only, at `read_plane`, where `plane_idx` is a
parameter and therefore nameable. The census notes its motivating gap closed on
main by `RECON_PLANE`/`recon_plane()`; that is true and is not a reason to skip
it. `RECON_PLANE` stamps the plane at RECONSTRUCTION (which does not know its
plane); this rung names it at the READER. Its reason to exist is the class
`rung-mislabels-plane`, which has already produced two wrong claims this session
— `read_coeffs_rect` labelling its unit `plane=0`, then a sweep reading a
phantom second luma unit out of that label. The H3 defect the lane chased is
still open and unowned.

`6cc9ea6f` is the 4-line header correction only. The census is right that the
gate name it was blamed for is already correct on main
(`a_pixel_exact_444_stream_walks_the_same_coefficient_units_the_oracle_does`),
so nothing else moved.

---

## 2. Behaviour-freeness

Claim per landed commit, and the proof each one got.

| commit | claim | proof | result |
|---|---|---|---|
| `d100dc47` | env-gated trace tag only | `cargo check --all-targets` + pixel-exact | 0 err 0 warn; **248/0** |
| `2be41696` | `TxParams.plane` reaches nothing but an `eprintln` | same | 0 err 0 warn; **248/0** |
| `b307aaeb` | env-gated `eprintln` in `read_plane` | same | 0 err 0 warn; **248/0** |
| `29951312` | `#[cfg(test)]` taxonomy + a test | 21/0 + 3 mutations | **21 passed, 0 failed** |
| `13b752ff`, `47bcb11e`, `d3226c2e` | report text only | blob / text comparison | §1.5a, §1.5b, §1.7 |

Common run, on the final tree:

```
$ CARGO_TARGET_DIR=$HOME/.cache/tgt/breconcile EC_NOMEMGUARD=1 \
    cargo test -p ec-av1 --lib -- pixel_exact
test result: ok. 248 passed; 0 failed; 0 ignored; 0 measured; 554 filtered out
```

248 includes `real_superres_streams_with_sub8_leaf8_and_warp_decode_pixel_exact`,
`the_chroma_rect_gates_excluded_seed_46_decodes_pixel_exact`,
`an_sb128_rect_strip_with_intrabc_decodes_pixel_exact`,
`an_sb128_screen_stream_with_intrabc_decodes_pixel_exact` and the pinned 4:2:0
odd-height and 4:4:4 quadrant witnesses — i.e. exactly the cells the new
`plane` field and the two new tags touch.

```
$ cargo test -p ec-av1 --lib -- every_proven_refusal refusal_inventory \
      every_named_gate_body every_proven_row which_kind_of_proof
test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 781 filtered out
```

---

## 3. Regression on the final tree

```
$ cargo test -p ec-av1 --lib -- 420 422 444 lossless warp intra \
      --skip bitrate_target_lands_within_5_percent_over_48_frames
test result: ok. 181 passed; 0 failed; 2 ignored; 0 measured; 619 filtered out
   (271.45s)

$ cargo check -p ec-av1 --all-targets
    Finished `dev` profile — 0 errors, 0 warnings
```

The 2 ignored are pre-existing `#[ignore]` attributes in the crate, not
anything this pass added; nothing in this lane changed an ignore.

`main` advanced `7538a841 -> 0f9ea788` during this run.
`git diff --stat 7538a841..main -- crates/` is **one new fixture**
(`420_lossless_tallinter_8x16.obu`) and zero source lines, so every
classification above is unaltered against `0f9ea788` as well.

---

## 4. Deliberately NOT landed

| artifact | why |
|---|---|
| `Proof::TablePin` (rank #2) | **REFUTED.** Its one row's gate on main measures the reach domain the tag's premise says is unestablished, so the tag would overclaim and the variant would carry no rows. §1.3. |
| `census_plane()` (`7586358f`) | **DEAD on the branch's own tip** — defined, never called; `6dfd827e` replaced its use with `TxParams.plane`. |
| `lane-av1tilerows.report.md` §9-§16 (`fdaec88c`…`d5ffc0c9`, `bf88d5e1`'s report half) | rounds 2-7 are measurements on an OLD main, and round 9 (landed, #5) measures the fork GONE on merged main — 0/36 divergence, entropy lockstep over 217570 reads. Landing §16 would re-record a fork that no longer exists. |
| `lane-av1loss444mm`'s 8 report commits | report-only history of an open defect; the defect is still open and its owner has not started. |
| `lane-av1refusal`'s `lanes/av1refusalclaim.report.md` (r1-r7) | report-only; r7's third-weak-anchor finding was closed on main by `797fa52b`, which took a LATER, stronger version of r3-r7's boundary and **dropped** `body_of`. Merging the branch report would document as open what main closed. |
| `lane-av1444chr`'s 3 report commits (H3 rounds 2-4) | report-only; they record a narrowing whose conclusion is "the owner of the remaining line has not been identified", which is still true and belongs with whoever picks it up, not with an instrument landing. |
| `lane-av1lm444loss` | **Author-declared DO-NOT-MERGE** (`lanes/av1lm444loss-corr.report.md:16-17`). Not touched. Census §3 names what merging it would break. |
| rank #1 `e55ad654` | already on this branch's base as `7538a841`. |

---

## 5. Branch containment, for a later prune

`git cherry` is patch-id and **cannot see a re-derivation**, so the column below
is by CONTENT (distinctive literals grepped in this tree), which is the only
decisive detector.

| branch | unlanded commits | touches `crates/` | source residue left | report residue left | prune? |
|---|---|---|---|---|---|
| `lane-av1leafsize` | **0** | — | none | none | **YES** — fully contained (`git cherry` says so too) |
| `lane-av1444rect` | **0** | — | none | none | **YES** — blob-identical report |
| `lane-av1chrtx` | 1 | none (report only) | none | none | **YES** — `6cc9ea6f`'s content is in `b307aaeb` |
| `lane-av1testcount` | 2 | none (report only) | none | none | **YES** — report imported at its corrected revision |
| `lane-av1444chr` | 4 | `5a1388f2` | **none** — `OUR_MODE plane=` present | 3 H3 report commits (§4) | source-complete; prune only if the reports are abandoned |
| `lane-av1tilerows2` | 8 | `e55ad654`, `bf88d5e1` | **none** — both landed (`7538a841`, `d100dc47`) | §9-§16, deliberately (§4) | source-complete |
| `lane-av1loss444mm` | 11 | `1a6e4f56`, `7586358f`, `6dfd827e`, `c8ed05f3` | **none** — `1a6e4f56` was on main via `c0727d64`; the other three are in `2be41696` | 8 report commits (§4) | source-complete |
| `lane-av1refusal` | 8 | `d6682aa4`, `72b83ba1`, `0c834cd1`, `40849868` | **none** — `d6682aa4` via `e429acfb`/`2ec19064`, `0c834cd1`+`40849868` via `797fa52b` (`fn gate_body`, `fn leading_clause`, `fn squash_source` all present), `72b83ba1`'s Proof half in `29951312` | `lanes/av1refusalclaim.report.md` r1-r7 (§4) | source-complete |

**Every one of the eight census branches is now source-complete in this work.**
The only residue left anywhere is REPORT text, and each item of it has a stated
reason in §4. Two branches are fully contained and prunable today.

Verification of the "none" column, run in this tree:

```
fn every_frame_size_a_header_can_code_has_a_mode_info_grid   stream.rs:1, refusal_inventory.rs:1
fn gate_body                                                 refusal_inventory.rs:2
fn leading_clause                                            refusal_inventory.rs:1
fn squash_source                                             refusal_inventory.rs:1
(cc * chunk_chroma_w, cr * chunk_chroma_h),                  decode.rs:4
set_symr_cdf("eob_pt")                                       decode.rs:1
set_symr_cdf("tx_type")                                      decode.rs:1
OUR_DQW plane=                                               transform.rs:1
pub(crate) plane: usize,                                     decode.rs:1
OUR_MODE plane=                                              decode.rs:1
```

---

## 6. Non-interference

- **No push, no merge to `main`, no rebase of any lane branch, no tag, no
  branch or worktree deleted.** All seven commits live on `lane-breconcile` in
  `~/.cache/wt/breconcile`.
- **The primary checkout was never written to.**
  `git -C /home/tahinli/Documents/Code/Rust/edith_codecs status --porcelain`
  is empty, verified at the end of the run. Every path this lane edited or
  created is under `/home/tahinli/.cache/wt/breconcile/`.
- `lane-av1lm444loss` was not checked out, read from, or written to.
- Scratch builds used `CARGO_TARGET_DIR=$HOME/.cache/tgt/{breconcile,tc-a,tc-b,tc-c}`
  — lane-private, never the house dir. The three `git archive` extracts for the
  test-count re-measurement lived in `/tmp/tc` and were removed;
  `df -h /tmp` was 22% used before the suite runs and no `/tmp/ec-av1-*` staging
  existed.

## 7. Method notes for the next pass

- **A census item's RATIONALE decays faster than its content.** Two of nine
  items here rest on a lane's own commit message describing an artifact main
  has since replaced. The census read the branch file and attributed its comment
  to main (`branchcensus2.report.md:108-112`). Re-read the CURRENT gate before
  carrying a tag that was minted against an older one.
- **`git cherry-pick` beat the census's conflict prediction on 3 of 4
  report-only commits** (only `72b83ba1` and the source commits conflicted).
  Try the cherry-pick before re-deriving; it is 2 seconds and it is decisive.
- **A helper can be dead on its own branch's tip.** `census_plane()` was added
  by `7586358f` and made dead by `6dfd827e`; grep the TIP, not the commit.
- **A test that pins a taxonomy must pin the SUBJECT, not its own expectation
  list.** Mutating the test's expectation proves nothing (measured: green).
  Mutating the gate's body is what reds it.