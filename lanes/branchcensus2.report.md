# Branch census 2 — disposition of all 16 unlanded lane branches

**Snapshot:** `main` = `52c62c15` ("Merge lane-av1444chromadc (REPORT ONLY) …"), 2026-09-30.
**Scope:** the 16 lane branches left after the worktree prune (76 -> 24), i.e. every
branch with at least one commit `git cherry main <branch>` marks `+`.
**Nothing was merged, pushed, rebased, or deleted. No lane branch was edited.**

---

## 0. Method, and the two detectors that lie

The candidate list came from `git cherry -v main <branch>`. That output is
**more precise than the counts suggest**: it marks each commit `-` (patch-id
already in main) or `+`. Two of the 16 branches contain commits that are in fact
already applied — `lane-av1tilerows2` has two `-` rows the raw counts hid.

`git patch-id --stable` over-reports (a fix re-derived elsewhere never matches),
so it was used only as a candidate filter. Two stronger detectors were used:

1. **Literal-set comparison** — for each unlanded source commit, extract 3-5
   distinctive added lines (counter names, format fragments, changed argument
   tuples) and test each against `git show main:<file>`. This is the only
   decisive detector.
2. **Author-declared disposition** — most branches' own last report commits state
   their fate in plain text, frequently naming the lane that superseded them.

**`git apply --reverse --check` is dead on this tree and must not be trusted.**
The prior census (`lanes/branchcensus.report.md`, main@`3aa16dd1`) used it
successfully on `lane-av1ibc128arm`. Eight merges later the same command reports
"not on main" for a commit whose content is *verbatim* in main, because the
patch's context drifted. Re-run over all 19 unlanded source commits it returns
"not on main" for **19 of 19** — including three that this report classifies as
landed. Literal reading beats it every time.

**`cherry-pick -n` conflict is not a disposition signal.** Measured over the same
19 commits: 17 conflict, 2 apply clean. Conflicts are reported here as evidence
of tree drift only, never as evidence of absence.

---

## 1. The 16 dispositions, one line each

| # | branch | commits / src | disposition | the named evidence |
|---|---|---|---|---|
| 1 | `lane-av1444chr` | 4 / 1 | **GENUINELY UNLANDED** (1 diagnostic rung, 13 lines) | `5a1388f2`'s plane-tagged `OUR_MODE` rung: `grep -c OUR_MODE main -- crates/ec-av1/src/decode.rs` = **0**. Its motivating gap is already closed on main by `RECON_PLANE`/`recon_plane()` (main `decode.rs:7315-7338`, `plane={}` on both `OUR_PRED` rungs `:21197`/`:21347`). The author deferred it himself: report "Rung gaps found" — "used for this analysis and removed before the commit; it is a 6-line addition worth landing in a follow-up". The H3 defect it chased is **open and unowned**: "the reach is faithful and the offset arithmetic is exonerated, so the only line left to change is one whose owner this round has not identified." |
| 2 | `lane-av1444rect` | 1 / 0 | **GENUINELY UNLANDED** (report only; names an OPEN defect) | `0beaf6c5` adds "Round 6 — the caller is NAMED" to `lanes/av1444rect.report.md`; `grep -c "Round 6"` on main's copy = **0**. Main's §7 still reads "both originally-named candidates eliminated by measurement, so this needs a fresh start, not a continuation" — i.e. the 4:4:4 intra-BC chroma fork (read 12465, mi (90,108), `read_coeffs_rect(w=4,h=8,skip_ctx=0)` at bit 6893) is still open. |
| 3 | `lane-av1chrtx` | 1 / 0 | **SUPERSEDED** (cosmetic residue only) | `6cc9ea6f` is a 4-line header correction to `lanes/av1readcensus.report.md` (name `lane-av1chrtx`/`~/.cache/wt/av1chrtx`, not the stale `lane-av1readcensus`). The gate name it "fixes" is **already correct on main** (`stream.rs:24115`, `a_pixel_exact_444_stream_walks_the_same_coefficient_units_the_oracle_does`, 2 refs; a repo-wide grep finds no stale spelling). Its census verdict: "COUNTING SHAPE — and not even that … The recorded 636-vs-1086 / 2052-vs-1309 signal came from an arm that DIVERGES, where the surplus is the divergence itself." |
| 4 | `lane-av1cmpaudit` | 1 / 1 | **LANDED-BY-ANOTHER-ROUTE** | `3d0001bc` adds `plane=` to `OUR_PRED` and `mode=` to `EC_PREDOUT8` rung 16. Main carries both: `RECON_PLANE`/`recon_plane()`/`PlaneGuard` (`decode.rs:7315-7338`) with `plane={}` on both `OUR_PRED` rungs; rung 16 `EC_PREDOUT8 … txh=%d mode=%d sum=%ld row0=` at both 8-bit sites in `scripts/instrument-aom-oracle.sh`, asserted by `scripts/check-aom-oracle-rungs.sh:129-148`. Carrier **`97b4ee04`** ("merge lane-av1rungs17"), which declares it: `lanes/av1rungs17.report.md:236` — "committed `3d0001bc` \| not merged as a commit …; its content is reconstructed here". The branch's own later sections (Rung-gap sweep §242-315, Rung handoff §317-327) are recorded "**not landed** — superseded by this report" (`av1rungs17.report.md:234`). |
| 5 | `lane-av1ibc128arm` | 1 / 1 | **LANDED-BY-ANOTHER-ROUTE** | `4bfe8d8e` (279 lines `decode.rs`). **All 4 distinctive literals present in main**: `hit!(INTRA_128_IN_INTER_MU_CHROMA_HITS);` (main `:44770`), `let unit_luma_w = cu_tx << ss_x(fctx);` (`:44793`), `let chunk_luma = 64usize;` (`:44803`), `let units_w = chunk_chroma_w / cu_tx;` (`:44807`) — including the branch's verbatim comment block. Carrier **`c0727d64`**, whose own message declares the provenance: "lane-av1ibc128arm, commit `4bfe8d8e` … and it has NEVER been merged to main" — carried, not merged. Main then **generalized** it per-axis: `chunk_chroma_w/h = 64 >> ss_x / 64 >> ss_y` via `29550383` (lane-av1-422bigblock) and `6558677c` (lane-av1-ibc128chunk r2). |
| 6 | `lane-av1leafsize` | 1 / 0 | **GENUINELY UNLANDED** (report only — a **retraction main lacks**) | `571b4090` adds "## 17. Round 9 — on merged main (`e719050d`): the fork is GONE". Main's `lanes/av1tilerows.report.md` is byte-identical to the branch through line 285, then jumps to the retracted §8; grep for `FORK NO LONGER EXISTS\|round 9\|217570\|ENTROPY LOCKSTEP` on main's copy = **no match**. Its proof: merged main diverges on **0 of 36 frames**, oracle reads 217570 = merged-main reads 217570, "ENTROPY LOCKSTEP over 217570 reads". It refuses attribution: "`4155c7c7` + my fix + `8a91ee14` alone **still diverges**". Main still carries §8's stale claim "the class is closed except for this one site, which is proven reachable and documented". |
| 7 | `lane-av1lm444loss` | 3 / 2 | **SUPERSEDED — author-declared DO-NOT-MERGE**; the change landed by another route | `lanes/av1lm444loss-corr.report.md:16-17` **on main**: "`lane-av1lm444loss`'s r2/r3 (`5519aaf8`, `6490933c`, `927dbf09`) is **DO-NOT-MERGE**, and the review is right". Carried by **`879c11e2`** + **`6c1d78a6`**, both ancestors of main. The fixture `ll444_intrabc_rect_l2.obu` is **byte-identical** (blob `601adaa2`, 121844 B) on main and branch. Main's `decode_intrabc_rect` already has the ss-aware geometry (`:14230`/`:14235`) and the lossless TX_4X4 raster (`:14561-14610`, `hit!(INTRABC_RECT_LOSSLESS_CHROMA4_HITS)`), and the tail replay in the corrected form `mi_r + ur * ((4 << ss_y(fctx)) / MI)` (`:14710`, via `5356cb1d`). **What would break on merge:** "Resolved 'theirs', it would have **deleted** main's `INTRABC_RECT_LOSSLESS_CHROMA4_HITS` and replaced main's `ll_chroma` with `mu_chroma`+`mu_units`". |
| 8 | `lane-av1loss444mm` | 11 / 4 | **SPLIT: fix LANDED-BY-ANOTHER-ROUTE; 3 instrument commits GENUINELY UNLANDED** | `1a6e4f56` (the r1 fix) **is on main**: the intra-in-inter lossless arm at main `decode.rs:44837-44838` carries `(cc * chunk_chroma_w, cr * chunk_chroma_h), (chunk_chroma_w, chunk_chroma_h)` — the exact tuple `1a6e4f56` introduced — via `c0727d64`. Its two counters are absent (the gate came from `4bfe8d8e` instead). **`6dfd827e`** (`TxParams.plane`) is **not** on main — main's `TxParams` (`:3030-3049`) has no `plane` field; nor are **`7586358f`**/`**`c8ed05f3`** (the plane-tagged lossless `EC_DQCOEFF` twin). All three are behaviour-free rungs whose stated purpose was to **exonerate** dequant (r9: "0 of 2053 units have non-zero levels with an empty `dq`"). The remaining defect (a) is **open**: key frame, RECONSTRUCTION-ONLY, 21 U + 28 V samples off by up to ±63, luma exact, entropy bit-identical end to end. |
| 9 | `lane-av1oraclepost` | 3 / 1 | **LANDED-BY-ANOTHER-ROUTE** (fix) **+ SUPERSEDED** (report) | `21759e06`'s two-site ss-gated intra-BC chroma plane-block footprint is on main, **generalized** to unconditional `>> ss` instead of an `own444`/`lossless` branch: `IBC_RECT_CHROMA_FOOTPRINT_444_HITS` + `IBC_OWNED_RECT_CHROMA_FOOTPRINT_444_HITS` (6 refs each) and gate `a_444_intrabc_rect_chroma_plane_block_is_the_block_footprint` (2 refs). Carriers **`10183674`** (lane-ibc444c-r4, `decode_intrabc_rect`) and **`5324a685`**/**`57834ee2`**/**`6d564cd6`** (lane-av1chromarect -r2/-r3, `decode_intrabc_owned_rect`). Its own collision record (48bbcc65): "their fix is kept, mine deferred to the rebase" — and both handed-on open items are now closed by **`e7152d26`** ("Merge lane-av1ibcfork (rebased)"), which landed the per-unit bit-interval ladder (`EC_UNITIV`) superseding this lane's `entry_bit/post_bit/bits` fields, plus gate `a_444_intrabc_rect4_witness_is_byte_exact_after_the_skip_arm_footprint` closing the mi (20,40) cell. Its report file is **absent from main**. |
| 10 | `lane-av1refusal` | 8 / 4 | **MOSTLY LANDED-BY-ANOTHER-ROUTE**; one row genuinely unlanded | `d6682aa4` landed via **`e429acfb`**/**`2ec19064`** (`a frame with no mode-info grid`). `0c834cd1` (r4) and `40849868` (r5) **both** landed in **`797fa52b`** ("lane-av1refusalspan") — main's `refusal_inventory.rs` has `fn gate_body`, `fn leading_clause`, `fn squash_source` and the `anchor strength` eprintln, all introduced by that one commit. `72b83ba1` (r3) is **split**: its proof-window bounding landed (as `gate_body`), but its **`Proof` enum + `Proof::TablePin` retag is NOT on main** (`grep -c "enum Proof" main` = 0, `grep -c TablePin main` = 0; main's `PROVEN` is still a 2-tuple at `:304`). `797fa52b` also closed the r7 finding: "Nine rows the previous measurement counted as anchored were not (their anchor was neighbour text), so the nine gates now name the refusal they prove and pin its guard site through `pins_refusal`." |
| 11 | `lane-av1refusalfix` | 5 / 2 | **SUPERSEDED** for its source; report-only residue | Its source commits are the shared base `b50fafd1`/`d6682aa4`/`72b83ba1` (same dispositions as #10). Its own `4741f0d4` states: "r3 base already closes items 1-3 and **main closes item 2**; item 5 walker test built, **direct-call parse unsolved, reverted**". Items 1-3 were then closed on main by `797fa52b`. **Item 5 is genuinely undeliverable as written** — the author reverted it, so there is no code to land. |
| 12 | `lane-av1skipfix` | 3 / 2 | **LANDED-BY-ANOTHER-ROUTE, in a stronger form** | Main's `a_real_aomenc_lossless_444_key_frame_decodes_sample_exact` (`stream.rs:7577`) already reads `crate_pin("ll444-lossless-key.obu")` with `unwrap_or_else(panic!)`, and its doc comment **credits lane-av1skipfix**: "The pin is now COMMITTED under the crate … so a missing file is a REPO DEFECT and fails unconditionally: there is no environment left to escape, which is **strictly stronger** than the `EC_REQUIRE_FIXTURES` / `EC_AV1_REQUIRE_AOMENC` opt-in hard fail this arm used to carry." The pin is on main: `crates/ec-av1/fixtures/ll444-lossless-key.obu`, 7845 B, sha256 `f496ef0a…` (`fixture-library.tsv:156`). |
| 13 | `lane-av1testcount` | 2 / 0 | **GENUINELY UNLANDED** (report only; a now-stale census) | `lanes/av1testcount.report.md` **does not exist on main** (141 lines on the branch). It reconciles the 13-branch wave exactly: base `298f75c4` = 712 tests, merged main `9623bcab` = 742, "delta 0, nothing lost, nothing renamed, nothing duplicated". `f61ebe79` corrects row 1: lane-av1superpin's tip `9646f74d` is "708 = 712 + 4 new - 8 removed" because it was cut from a tree **older** than `298f75c4`. Both are now stale — main has moved well past `9623bcab`. |
| 14 | `lane-av1tilerows2` | 10 / 4 | **PARTIALLY LANDED** — `b9535b9e` landed; **4 mislabelled-rung hunks GENUINELY UNLANDED** | `b9535b9e` is patch-id-identical to main's **`4e151813`** (same subject) — `git cherry` marks it `-`. `e55ad654` is **partial**: its `part32_pre` neighbour was fixed by another commit, but all **four** rungs it names are still wrong on main — `EC_PART … bsize=9` prints `(r32 as usize), (c32 as usize)` (main `:53397-53398`, raw 32-grid, factor `BLOCK_MI`=8 off) and `EC_PART … bsize=6` (`:35489`), `EC_PART_VAL … bsize=6` (`:35500`) and `TRACE partition_w16` (`:35506`) all print `at16.0, at16.1` (raw 16-grid) into fields labelled `mi_row=`/`mi_col=`. `bf88d5e1`'s two `set_symr_cdf("eob_pt")`/`("tx_type")` tags are **not** on main (main tags only `y_mode`/`uv_mode`). Open defect it names: round 7 "the 7-entry class is **EOB_PT 32, not tx_type**"; round 3 "the 4:4:4 inter fork is a **WALK** divergence, not a shape". |
| 15 | `lane-branchcensus` | 1 / 0 | **SUPERSEDED — main carries a corrected version that withdraws the branch's headline row** | Main's `lanes/branchcensus.report.md` is **466 lines** vs the branch's 236, and its "Corrections since the first pass (2026-09-30)" **withdraws row 1**: "`lane-av1-f9single` is the highest-value row. **WITHDRAWN 2026-09-30 — false positive** … **the fix is already on main**: `6c33f204` is a twin-flow sweep that pointed *both* copies of the four-unit arm at the covering lookup" and corrects `c6c66d15` from "comment-only" to "27 lines of real `decode.rs`". Carriers **`e8b82639`** + **`c4f5e69f`**. **Merging the branch would reintroduce a row main has already declared false.** |
| 16 | `lane-fxmerge` | 9 / 1 | **LANDED-BY-ANOTHER-ROUTE** (pins byte-identical) **+ SUPERSEDED** (tooling is a strict superset) | `93dc9e60`'s four binary pins are all on main with **identical blobs**: `golden4-pin.obu` `4265cdf0`, `golden6-mismatch.obu` `df1fb4a8`, `golden7-forwarding-mismatch.obu` `d69eb7b4`, `lr-sgr-r7.obu` `8eb1461d`. Tooling superseded: main's `verify-fixture-library.sh` is **475** lines vs the branch's 334, `pin-gate-audit.py` **496** vs 274, `gen-fixture-library.sh` **394** vs 318 (already carrying `20930fc2`'s `realpath`/`repo-relative` normalisation, 2 refs each). `scripts/fixture-library.tsv` on main is ahead by 129 insertions / 212 deletions relative to the branch. |

### Tally

- **LANDED-BY-ANOTHER-ROUTE (content is in main, arrived by another commit): 8** —
  #4, #5, #8 (fix half), #9 (fix half), #10 (3 of 4), #12, #16 (pins).
- **SUPERSEDED / REFUTED: 4** — #3, #7, #11, #15.
- **GENUINELY UNLANDED: 4 branches** — #1, #2, #6, #13 — plus the *partial*
  residue inside #8, #10, #14, ranked in §2.

**No unlanded decode-behaviour fix exists.** Every change to `decode.rs` that
alters reconstruction landed on main, by another route, in every case. The
unlanded residue is: 1 test-bookkeeping enum, ~30 lines of env-gated diagnostic
labelling across 4 branches, 1 diagnostic instrument set, and 5 report files.

---

## 2. Genuinely unlanded, ranked by value

Ranked by *reachability and gateability* per the assignment: a fix for a
reachable 4:2:0/4:4:4 decode path would outrank probe-only 4:2:2 work. There is
**no decode fix in this set**, so the ranking runs from "unblocks a named open
defect" down to "documentation of a fork that no longer exists".

### 1. `e55ad654`'s four mislabelled partition rungs — `lane-av1tilerows2`
**Value: highest. It unblocks a named, still-open defect.**
Main cannot pair its 16-level and 32-level `EC_PART` rungs against the oracle's
per-level `EC_PART_VAL` dump, because those rungs print the raw superblock/16-grid
index into fields labelled `mi_row=`/`mi_col=`. A rung whose coordinates are in
the wrong label space is worse than no rung — it produces confident false pairs.

- **What it takes:** re-derived and **measured to compile on today's main** —
  4 sites, 9 insertions / 6 deletions, one file
  (`crates/ec-av1/src/decode.rs`). `cherry-pick -n e55ad654` **conflicts**
  (context drifted), but every hunk is a 2-line argument swap at a site still
  present verbatim.
- **Proof the target form is right:** main's *own* sibling 20 lines below the
  `bsize=6` site already computes the same coordinates correctly —
  `let (mi_row0, mi_col0) = (sr * SUB_MI as usize, sc * SUB_MI as usize);`
  (`decode.rs:35574-35575`), and the `part32_pre` rung 8 lines above the
  `bsize=9` site already prints `r32 * BLOCK_MI` (`:53389-53390`). The four
  stragglers are the only sites in the file still in the old space.
- **The open defect it unblocks:** the 4:4:4 inter partition-walk divergence,
  whose 7-entry class this lane identified as **EOB_PT 32** (round 7), reached at
  read 41976 — a location that cannot be read off a mislabelled rung.

### 2. `72b83ba1`'s `Proof` enum + `Proof::TablePin` — `lane-av1refusal`
**Value: makes the refusal inventory honest about one specific row.**
Main's `PROVEN` table is a 2-tuple (`refusal_inventory.rs:304`): every row looks
equally proven. The `(4,8)` chroma row is **not** — its gate pins a table's arms
exactly but the domain the table is *reached over* is unestablished, so an
`Enumeration` tag there would claim unreachability the gate's own comment admits
it has not shown. That distinction is currently inexpressible in main.

- **What it takes:** 1 `#[cfg(test)] enum Proof {NegativeGate, Enumeration, TablePin}`
  + 1 `label()` impl + a third tuple element on **~32 rows** + updates to **6
  consumer sites** (`:706` source-scan registration, `:2547`, `:2577`, `:2583`,
  `:2649`, `:2658`). `cherry-pick -n 72b83ba1` **conflicts**; the file has grown
  from 2215 to 2715 lines since.
- **Note:** main is *not* missing r3's other half — its proof-window bounding
  landed as `gate_body` in `797fa52b`, and that commit is strictly stronger
  (brace-matched, Rust-aware, with its own non-vacuity tests).
- **The carrier says so in writing.** `lanes/av1refusalspan.report.md:27`:
  "`lane-av1refusal` (tip `910dc296`, r7) is **unmerged**, and main therefore does
  not …"; `:33-36`: it took "a **later version** of r3–r7's `body_of`: real
  boundary, missing boundary = failure, leading-clause … over it and **drop
  r3–r7's `body_of`**". That is r7's third-weak-anchor finding closed: the row
  at `:112` is one whose *neighbour* carried the refusal string, and the repaired
  gate now asserts its own body does **not** while the neighbour's does.

### 3. `6dfd827e` + `7586358f` + `c8ed05f3` — `lane-av1loss444mm`
**Value: the rung the one still-open reconstruction defect needs.**
Defect (a) — 4:4:4 lossless **key frame**, reconstruction-only, 21 U + 28 V
samples off by up to ±63, luma exact, entropy bit-identical — is open, and the
branch names its search space as three files. The plane-tagged lossless
`EC_DQCOEFF` twin is the instrument that exonerates or convicts the WHT path.

- **What it takes:** the easy half is 3 behaviour-free rungs. The load-bearing
  half is `TxParams.plane` (`decode.rs:3030-3049`), a field threaded through
  **every** transform-unit construction — wide blast radius, and it conflicts.
- **Already done for you:** the twin's stated purpose is achieved — r9 exonerated
  dequant ("0 of 2053 units have non-zero levels with an empty `dq`"). Landing
  the rung now buys diagnosis, not a fix.

### 4. `bf88d5e1` — two `set_symr_cdf` tags — `lane-av1tilerows2`
**Value: low, 2 lines, and it names the open fork's class.**
`set_symr_cdf("eob_pt")` / `set_symr_cdf("tx_type")` are absent from main (which
tags only `y_mode`/`uv_mode`). The `set_symr_cdf` helper exists (`msac.rs:428`).
Conflicts on cherry-pick; 2 lines to re-derive. Pairs only once item 1 lands.

### 5. `571b4090` — the round-9 retraction — `lane-av1leafsize`
**Value: correctness of the record.** Main's `lanes/av1tilerows.report.md` still
ends at §8's claim that one site is "proven reachable and documented". Round 9
**proves the opposite on merged main**: 0 divergence on 36 frames, 217570 reads
in lockstep, and it explicitly refuses to credit its own fix. 46 report lines.
Merging it corrects a stale open claim rather than adding work.

### 6. `0beaf6c5` — the round-6 caller identification — `lane-av1444rect`
**Value: converts a dead end into a named call site.**
Main's §7 says the 4:4:4 intra-BC chroma fork "needs a fresh start, not a
continuation". Round 6 names it: `decode_intrabc_rect`'s non-lossless whole-block
rect arm, one gather taken `around_mi_rect((mi_r, mi_c), bw, bh)` at the **luma**
footprint while serving a 4x8 chroma unit under a 4x16 luma plane block — which
coincide at 4:2:0 (hence the exact 4:2:0 twin) and do not at 4:4:4. 46 report
lines, no code. Highest information density per line in the whole set.

### 7. `f0ae03c8` + `f61ebe79` — the test-count census — `lane-av1testcount`
**Value: low and now stale.** It reconciled exactly at `9623bcab` (712 -> 742,
delta 0) and corrected a real 708-vs-712 error, but main has moved since. Land
only if a fresh count is taken in the same pass.

### 8. `5a1388f2` — the plane-tagged `OUR_MODE` rung — `lane-av1444chr`
**Value: lowest.** 13 lines, behaviour-free, explicitly deferred by its own author
to "a follow-up", and its motivating gap (bullet 1 of the report's "Rung gaps
found": `OUR_PRED` carrying no PLANE field) is **already closed on main** by
`RECON_PLANE`. Cherry-picks **clean** (the only source commit that does), so
landing it is free — but it buys little.

### 9. `6cc9ea6f` — the report header correction — `lane-av1chrtx`
**Value: cosmetic.** 4 lines naming the right branch/worktree. The gate name it
"fixes" is already correct on main. Not worth a merge on its own.

---

## 3. What would break if the SUPERSEDED branches were merged now

Named per branch, because a superseded branch is not merely redundant — several
would actively damage main.

| branch | damage on merge |
|---|---|
| `lane-av1lm444loss` | **Deletes** main's `INTRABC_RECT_LOSSLESS_CHROMA4_HITS` and replaces main's `ll_chroma` with `mu_chroma`+`mu_units`. Author-declared **DO-NOT-MERGE** by the lane that superseded it (`lanes/av1lm444loss-corr.report.md:16-17`). |
| `lane-branchcensus` | Reintroduces `lanes/branchcensus.report.md`'s row 1, which main **explicitly withdrew as a false positive** (`e8b82639`), and re-asserts `c6c66d15` as "comment-only" when it carries 27 lines of real `decode.rs`. |
| `lane-av1refusalfix` | Its report claims items 1-3 need work; `797fa52b` has since closed them, so the report would document a closed gap as open. Item 5 was reverted by its author and has no code. |
| `lane-av1chrtx` | Overwrites main's `lanes/av1readcensus.report.md` header with a branch name; body is otherwise identical. No functional damage. |
| `lane-av1skipfix` (if taken wholesale) | Its gate reads `ll444_lossless_key.obu` and asserts `len == 7851` / fnv `0xacc9_9150_513f_b145`. Main ships `ll444-lossless-key.obu` at **7845 B** (sha256 `f496ef0a…`). Applied as-is it would **panic on a missing file** and, with the branch's pin, red on the size assert. Main's form is already stronger on the axis that matters (no env escape). |
| `lane-fxmerge` (tooling half) | Would shrink `verify-fixture-library.sh` 475 -> 334 lines, `pin-gate-audit.py` 496 -> 274 and `fixture-library.tsv` by 212 lines — regressing invariants main has since added. |

---

## 4. Statement of non-interference

**Nothing was merged, pushed, rebased, tagged, or deleted. No lane branch was
edited, and no branch or worktree belonging to a lane was removed.** The census
ran read-only against `main`; all builds and measurements ran in a private
scratch worktree (`/home/tahinli/.cache/wt/census2`, branch `census2-scratch`,
created from `main`@`52c62c15`, never pushed).

- This report is written **into the scratch worktree** and committed there, not
  into the primary checkout, so the primary is left exactly as found. Publish it
  with `git show census2-scratch:lanes/branchcensus2.report.md`, or cherry-pick
  the commit named in the yield.
- **The census made exactly one edit in the entire run**, to
  `/home/tahinli/.cache/wt/census2/crates/ec-av1/src/decode.rs` — the §4.5
  re-derivation. **No write, edit or commit of any kind was made against
  `/home/tahinli/Documents/Code/Rust/edith_codecs`.**
- **A concurrent worker's writes, observed and deliberately not reverted.** At
  13:37:18 the primary was *not* empty; it carried:

  ```
   M lanes/av1422lpf.report.md      (17 insertions)
   M lanes/av1444altref.report.md   (14 insertions)
  ?? lanes/refute-av1-w3a.report.md
  ```

  All three were written between **13:36:28 and 13:37:12** — by a peer, seconds
  before that check. This census created none of them. They were **not** reverted:
  they were another lane's in-flight work, and deleting a peer's uncommitted
  output is the one irreversible act this census had no licence to take.
  The peer has since landed them as `bf5bbaec` ("refutation pass: land the
  independent audit of the six exactness merges …"), and the primary is
  **empty**:

  ```
  $ git -C /home/tahinli/Documents/Code/Rust/edith_codecs status --porcelain
  (no output — empty)
  ```

- **`main` moved during the run**: `52c62c15` -> `bf5bbaec`. Verified harmless:
  `git diff --stat 52c62c15..bf5bbaec -- crates/` is **empty** (no decode
  source changed) and the commit touches **none** of the 16 branches' report
  files. Every classification in §1 therefore stands unaltered against
  `bf5bbaec` as well as `52c62c15`.

---

## 5. Measurement appendix

### 4.1 `git cherry -v main <branch>` — the two commits that are already applied

```
lane-av1tilerows2:
  - b9535b9e  ec-av1: ss-aware chroma reach in the lossless 16x4/4x16 pair walk
  - 8e181694  lanes/av1tilerows: class sweep — 32 Reach::of_tu sites inventoried
```

`b9535b9e` is patch-id-identical to main's `4e151813` (same subject), and
`4e151813` is an ancestor of `main`. 8 of the 16 branches' commits are therefore
already applied and were never candidates.

### 4.2 Apply-ability of all 19 unlanded source commits onto `main`@`52c62c15`

```
APPLIES CLEAN : 5a1388f2, 359990dc
CONFLICT      : 3d0001bc 4bfe8d8e 5519aaf8 6490933c 1a6e4f56 6dfd827e 7586358f
                c8ed05f3 21759e06 d6682aa4 72b83ba1 0c834cd1 40849868 f385963f
                93dc9e60 e55ad654 bf88d5e1
```

Read this as *tree drift*, not as absence: 3 of the 19 (`3d0001bc`, `4bfe8d8e`,
`5519aaf8`) conflict **because their content is already in main**.

### 4.3 `git apply --reverse --check` — the detector that stopped working

```
not on main : 19 / 19   (including 4bfe8d8e, 5519aaf8, 3d0001bc — all landed)
```

### 4.4 Literal-set comparison against `git show main:crates/ec-av1/src/decode.rs`

| commit | distinctive literals checked | present | verdict |
|---|---|---|---|
| `4bfe8d8e` | `hit!(INTRA_128_IN_INTER_MU_CHROMA_HITS);`, `let chunk_luma = 64usize;`, `let units_w = chunk_chroma_w / cu_tx;`, `let unit_luma_w = cu_tx << ss_x(fctx);` | **4 / 4** | LANDED |
| `1a6e4f56` | `(cc * chunk_chroma_w, cr * chunk_chroma_h),`, `(chunk_chroma_w, chunk_chroma_h),` | **2 / 2** | LANDED |
| `5519aaf8` | `mi_r + ur * ((4 << ss_y(fctx)) / MI),`, `let (cw, ch) = (bw >> ss_x(fctx), bh >> ss_y(fctx));` | **2 / 2** | LANDED |
| `6dfd827e` | `pub(crate) plane: usize,` | **0 / 1** | unlanded |
| `e55ad654` | `sr * SUB_MI as usize,` (present, but **not at the 3 target sites**), `r32 * BLOCK_MI,` (present, but only at `part32_pre`, not at `EC_PART bsize=9`) | partial | 4 sites unlanded |
| `bf88d5e1` | `set_symr_cdf("eob_pt")`, `set_symr_cdf("tx_type")` | **0 / 2** | unlanded |

### 4.5 The one re-derivation, measured

`e55ad654`'s four rungs re-applied by hand to `main`@`52c62c15` in the scratch
worktree:

```
 crates/ec-av1/src/decode.rs | 25 +++++++++++++++++++------
 1 file changed, 19 insertions(+), 6 deletions(-)
```

19 insertions / 6 deletions — **the same shape as `e55ad654` itself**
(`git show --stat e55ad654` reports `19 insertions(+), 6 deletions(-)`),
including the branch's 10-line explanatory comment at the `bsize=9` site.

### 4.6 Build result — the re-derivation compiles and the crate is sound

Built in the scratch worktree on top of `main`@`52c62c15` with
`CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/census2 EC_NOMEMGUARD=1`:

```
$ cargo test -p ec-av1 --lib --no-run
   Compiling ec-av1 v0.1.0 (/home/tahinli/.cache/wt/census2/crates/ec-av1)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 12.57s
  Executable unittests src/lib.rs (.../deps/ec_av1-79bef263d07a873f)
```

**No errors, no warnings, no conflicts remaining.** Then the scoped gates a
`refusal_inventory.rs` change would threaten:

```
$ cargo test -p ec-av1 --lib -- every_proven_refusal refusal_inventory
test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 781 filtered out
```

`every_proven_refusal_names_a_test_that_exists` and
`every_named_gate_body_is_bounded_in_these_files` both pass on `main` **as it
stands** — so the `Proof` enum gap (§2 item 2) is an *expressiveness* gap, not a
broken gate: every row is currently proven, main just cannot say **which kind**
of proof each row carries.

```
$ cargo test -p ec-av1 --lib -- pixel_exact
test result: ok. 248 passed; 0 failed; 0 ignored; 0 measured; 553 filtered out; finished in 847.60s
```

**248 pixel-exact decode gates pass with the re-derivation applied** — including
`an_sb128_rect_strip_with_intrabc_decodes_pixel_exact`,
`an_sb128_screen_stream_with_intrabc_decodes_pixel_exact`,
`the_chroma_rect_gates_excluded_seed_46_decodes_pixel_exact` and
`real_superres_streams_with_sub8_leaf8_and_warp_decode_pixel_exact` — i.e. the
4:4:4 / superblock-128 / rect / intra-BC paths whose partition rungs were
relabelled. Behaviour-neutral, exactly as the branch's own comment claims.
```

**What this establishes:** the top-ranked disposition-3 item is not rotted. Its
cherry-pick conflicts, but the change is a 4-site, 19-line argument swap that
compiles against today's `main` and leaves every gate green — because it is
env-gated print only, exactly as its own comment claims ("Behaviour-free:
env-gated print only"). It is landable today as a hand re-derivation.

### 4.7 A method correction for the next census

The prior census (`lanes/branchcensus.report.md`, main@`3aa16dd1`) leaned on
`git apply --reverse --check` as a *secondary detector* and recorded it working
on `lane-av1ibc128arm`. It no longer discriminates (§0, §4.3). Any future census
must either re-validate that detector against a known-landed commit before
trusting it, or drop it for the literal-set comparison. Carried as managed skill
`branch-census-patch-id-disposition`.
