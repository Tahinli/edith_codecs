# lane-av1oracleskip — gate-honesty: the oracle SKIP class

**Tree.** `lane-av1oracleskip`, worktree `~/.cache/wt/av1oracleskip`. r1 was
cut from main `7f8817cb` ("Merge lane-av1pins @ 44a05b40") and is commit
`644bdbb8` after the r2 rebase onto `9b2f6c9d` ("Merge lane-av1dumpyuv @
60a7788a"). The r1 text of this report was written when the primary checkout
was mid-merge (`UU crates/ec-av1/src/stream.rs` in its index), so the worktree
was pinned to a merge commit rather than to a moving HEAD. Built in a
lane-private `CARGO_TARGET_DIR` (`~/.cache/cargo-target-av1oracleskip`).

**Scope.** (1) the three `lane-av1gates444` gates that reported green having
measured nothing on a host without the oracle; (2) the class sweep behind it,
one row per site for every site this commit touches and per shape for the rest.

---

## 1. What was wrong, exactly

`encode_444_lossy_live` (the helper all three gates call) opened with

```rust
if !have_aomenc() {
    eprintln!("SKIP {name}: no aomenc oracle at {path} -- run scripts/build-aom-oracle.sh");
    return Vec::new();
}
```

and `gate_444_lossy_live_exact` then did

```rust
let stream = encode_444_lossy_live(name, w, h);
if stream.is_empty() {
    return; // no oracle at all: the SKIP above already said so
}
```

Two exits, the second one silent, and — the part that matters for a batch — the
empty `Vec` is indistinguishable at the call site from a stream aomenc
produced and then lost. The gates
`a_444_lossy_odd_66x66_stream_decodes_pixel_exact`,
`a_444_lossy_odd_130x122_stream_decodes_pixel_exact` and
`a_444_lossy_256x128_stream_decodes_pixel_exact` inherit that shape: without the
oracle they assert nothing, encode nothing, compare nothing, and report
**passed**.

The escape hatch already existed and was already correct in the probe:
`have_aomenc()` asserts when `EC_AV1_REQUIRE_AOMENC` is set, naming the path it
looked for. So the fix is not a new mechanism — it is to stop putting a second,
silent exit in the gate, and to make the SKIP say what it is.

## 2. The class sweep

`grep -c` over the crate's three test-bearing files (there is no `tests/`
directory; the crate's tests live in `src/*.rs`):

| file:line | gate(s) | shape | fixed or reported |
|---|---|---|---|
| `stream.rs:7422` (`encode_444_lossy_live`), `stream.rs:7505` (`gate_444_lossy_live_exact`) | `a_444_lossy_odd_66x66_stream_decodes_pixel_exact`, `a_444_lossy_odd_130x122_stream_decodes_pixel_exact`, `a_444_lossy_256x128_stream_decodes_pixel_exact` | **C** — `if !have_aomenc() { SKIP; return Vec::new() }` + a silent `if stream.is_empty() { return; }` in the caller | **FIXED** |
| `stream.rs:6692` | `a_lossless_444_min_partition64_inter_stream_decodes_pixel_exact` | **B** — bare `if aomdec_path().is_file() { …compare… }`, no else, no env escape | **FIXED** |
| `stream.rs:6871` | `a_lossless_444_128_root_lossless_stream_reads_chunks_chunk_major` | B (had an `else` SKIP, still no env escape) | **FIXED** |
| `stream.rs:6934` | `a_lossless_444_min_partition8_inter_stream_decodes_sample_exact` | B (else SKIP) | **FIXED** |
| `stream.rs:7123` | `a_444_sb128_root_rect_stream_with_restoration_decodes_pixel_exact` | B (no else) | **FIXED** |
| `stream.rs:7290` | `a_444_lossy_rect4_inter_stream_decodes_pixel_exact` | B (no else) | **FIXED** |
| `stream.rs:7380` | `a_444_lossy_rect4_inter_stream_decodes_pixel_exact` (4:2:0 control arm) | B (no else) | **FIXED** |
| `stream.rs:7703` | `a_444_intrabc_rect4_reads_its_own_chroma_plane_block` | B (no else) | **FIXED** |
| `stream.rs:7834` | `a_444_sb128_witness_lr_pipeline_flushes_ru_row_0_pixel_exact` | B (else SKIP) | **FIXED** |
| `stream.rs:7979` | `a_lossless_444_defaultp_inter_strip_stream_decodes_byte_exact` | B (else SKIP) | **FIXED** |
| `stream.rs:8116` | `an_intrabc_8x8_leaf_chroma_frame_copy_at_444_decodes_without_the_oob` | B (else SKIP) | **FIXED** |
| `stream.rs:8543` | `a_lossless_444_intrabc_rect_leaf_walks_per_4x4_units` | B (else SKIP) | **FIXED** |
| `stream.rs:11063` | `a_lossless_444_rect16x4_chroma_reach_is_ss_aware` | B (else SKIP) | **FIXED** |
| `stream.rs:14076` | the superres/upscaled-bit-depth helper (`name: &str`) | B (no else) | **FIXED** |
| `stream.rs` — 153 sites | (the `aomenc`-live gates) | **A** — `if !have_aomenc() { eprintln!("SKIP …"); return; }` | REPORTED, not changed |
| `stream.rs` — 218 sites, `decode.rs` — 4, `encode.rs` — 43 | (the ffmpeg-backed gates) | **A** — `if !have_ffmpeg() { …; return; }` | REPORTED, not changed |
| `stream.rs` — 26, `decode.rs` — 1, `encode.rs` — 1 | (compound guards) | **A** — `if !have_ffmpeg() \|\| !have_aomenc() { … }` | REPORTED, not changed |
| `stream.rs:7511` | the three live 4:4:4 gates | **D** — a hard `assert!(aomdec_path().is_file(), …)` | REPORTED, kept: these gates encode LIVE, so reaching that point means aomenc resolved, and an aomdec missing beside a present aomenc is a broken oracle, not a checkout without one |

**Why shape A is reported and not changed.** Both probes (`have_ffmpeg`,
`have_aomenc`) already have the correct shape — probe, then `assert!` last,
with the env escape — and `have_ffmpeg`'s doc comment already records the
earlier version's short-circuit bug. Every shape-A site therefore already FAILS
under `EC_AV1_REQUIRE_AOMENC=1` / `EC_AV1_REQUIRE_FFMPEG=1` and prints a SKIP
when those are unset. Changing 445 call sites would restate a convention the
probe already enforces; the honest statement is that a batch run MUST set the
env var, and this commit does not change that.

**Shape B is the part the sweep found that nobody had named**, and it is worse
than the three gates Main pointed at: those three fail loudly in REQUIRE mode
(through `have_aomenc`), while all thirteen B sites had **no env escape at
all** (r2 correction: thirteen CALL SITES across **twelve** distinct gates —
`a_444_lossy_rect4_inter_stream_decodes_pixel_exact` calls the probe twice,
once for the 4:4:4 stream and once for its 4:2:0 control) — with `EC_AV1_REQUIRE_AOMENC=1` set and aomdec absent, they encoded,
decoded, asserted their geometry and counters, skipped the compare, and
reported green. Four of the thirteen printed nothing at all.

## 3. The fix

1. New probe `aomdec_available(name)` beside `have_aomenc`, in the
   `have_ffmpeg` shape: probe `aomdec_path().is_file()`, then `assert!` **last**
   that the absence is a hard failure when `EC_AV1_REQUIRE_AOMDEC` or
   `EC_AV1_REQUIRE_AOMENC` is set (the message names the path looked for and
   how to fix it), then print a SKIP naming the env var when the flag is unset
   and return the bool. All thirteen sites now call it; the nine `else { … }`
   SKIP branches they carried are deleted, so the skip notice comes from one
   place and cannot drift from the assert.
2. `encode_444_lossy_live` returns `Option<Vec<u8>>` and
   `gate_444_lossy_live_exact` takes it with `let Some(stream) = … else { return }`
   — one exit instead of two, and the empty-`Vec` sentinel that made "no
   oracle" look like "aomenc produced nothing" is gone. Nothing about what the
   gates measure changed: they still encode live, still assert the parsed
   geometry, the frame count, the full-resolution chroma extent and
   `rect4_inter_own_chroma444_hits()`, and still compare every decode-order
   frame against `aomdec` through `decode_all_frames_vs_oracle`.

## 4. Proof, all six directions

Absence is simulated the way the crate's own path resolution allows: the
`EC_AV1_AOMENC` / `EC_AV1_AOMDEC` env overrides point at
`/nonexistent/oracle/aomenc` and `/nonexistent/oracle/aomdec`, so
`aomenc_path()` / `aomdec_path()` resolve to files that are not there.

| # | condition | before this commit | after |
|---|---|---|---|
| a | oracle present, `EC_AV1_REQUIRE_AOMENC=1` | 3 passed | 3 passed, each printing its line: `66x66 … 13 own-extent 1:4 chroma gather(s)`, `130x122 … 12 …`, `256x128 … 24 …` |
| a2 | oracle present, the 11 re-routed pinned gates | 11 passed | **11 passed** (no behaviour change with the oracle there) |
| b | aomenc absent + `EC_AV1_REQUIRE_AOMENC=1` | 3 passed (green, measured nothing) | **3 FAILED**: `EC_AV1_REQUIRE_AOMENC is set but no aomenc at /nonexistent/oracle/aomenc -- run scripts/build-aom-oracle.sh` |
| b2 | aomdec absent + `EC_AV1_REQUIRE_AOMENC=1` | **3 passed, silently** — one of the three printed no SKIP line at all | **3 FAILED**: `<gate>: no oracle aomdec at /nonexistent/oracle/aomdec -- the pixel compare this gate exists for would be skipped, and EC_AV1_REQUIRE_AOMDEC/EC_AV1_REQUIRE_AOMENC is set.` |
| c | aomenc absent, env unset | 3 passed, one SKIP line per gate | 3 passed, `SKIP <gate>: no aomenc oracle at /nonexistent/oracle/aomenc -- set EC_AV1_REQUIRE_AOMENC=1 to make this a failure, or run scripts/build-aom-oracle.sh` |
| c2 | aomdec absent, env unset | 3 passed, 2 SKIP lines, 1 silent | 3 passed, 3 SKIP lines naming the gate and the path |

**(b2) is the red-before for the class fix**: the same command on the parent
commit printed `test result: ok. 3 passed` with the compare never running, and
prints `3 failed` here. `(c)` and `(c2)` remain a **developer-only
convenience** — a checkout without an oracle still runs the rest of the suite
green, and the SKIP line now says which env var turns that into a failure. No
batch run may rely on it: `EC_AV1_REQUIRE_AOMENC=1` (or the aomdec-specific
`EC_AV1_REQUIRE_AOMDEC=1`) is what makes a missing oracle red, and that is
stated in the SKIP text itself.

## 5. Pre-existing red on this base, NOT this lane's

`gate_coverage::tests::never_exercised_{8,10}bit_matches_the_gate_recipes` are
RED on base `7f8817cb` before any edit of mine (`the 8-bit list still names
["enable-rect-tx"], but a 8-bit gate now passes '=1' for them`). Reproduced on
the stashed, unmodified tree. A gate merged in `7f8817cb` spells
`--enable-rect-tx=1`, so the `enable-rect-tx` entries in
`NEVER_EXERCISED_8BIT` / `NEVER_EXERCISED_10BIT` are stale and the rule is to
delete them in the same commit as the gate that closes the hole — that gate is
not mine, and `crates/ec-av1/src/gate_coverage.rs` is one of the files the
in-flight merge on the primary checkout has staged as modified, so editing it
here would collide. **Reported, not touched.** The fix is deleting the
`("enable-rect-tx", …)` entry from both lists.

## 6. One more thing this lane does not change

`aomdec_available` deliberately keeps a SKIP (rather than the hard assert shape
D uses) because those thirteen gates decode a **pinned fixture** and their
oracle arm is one assertion among several; the live-encode gates have no
fixture to fall back on, so a missing aomdec there really is a broken oracle
and stays a hard failure. If the house wants the pinned gates to fail
unconditionally too, that is a one-line change per site and a policy call, not
a bug.

---

# r2 — the class was NOT closed: a stale base

r1 fixed thirteen call sites found on base `7f8817cb`. The merge review then
measured the CURRENT tree and found **21** bare `aomdec_path().is_file()`
occurrences there, not 13: six gates that merged after r1 branched were added
with the same silent shape, and under `EC_AV1_REQUIRE_AOMENC=1` with aomdec
absent all six reported GREEN with the compare skipped. **The lesson is bigger
than the fix: a class sweep run against a base that is still moving is a
snapshot of the past.** A lane that merges into main while others are
measuring will reopen the class behind your back unless the probe is the only
place the decision is made — which is why r2 leaves exactly one.

## 7. The sweep, re-run on the current tree

Current main at the r2 rebase: `9b2f6c9d`. `grep -c 'aomdec_path().is_file()'`
over `crates/ec-av1/src/stream.rs` gives **21**, which decomposes exactly:

| kind | count on main | what r2 did |
|---|---|---|
| bare `if aomdec_path().is_file() { …compare… }` guards (17 positive + 1 inverted `if !…`) | **18** | **all 18 routed through `aomdec_available()`** — 13 in r1, 5 in r2 |
| `assert!(aomdec_path().is_file(), …)` — a deliberate hard failure | 2 | kept (they are the strict form) |
| a doc comment naming the old shape | 1 | kept (it documents the defect) |
| the presence check inside `aomdec_available` itself | 0 (it did not exist) | **the one place the decision is made** |

**Before/after: 18 bare guard sites -> 0. The presence check now exists in
exactly one place** (`aomdec_available`, `stream.rs:9131-9160`), called from
**18 call sites**. Two of those sites sit in shared helpers, so the 18 sites
serve **20 tests**: `a_444_hbd_superres_arm` (site 14201) is called by
`a_444_lossy_superres_{10bit_d12,12bit_d12,12bit_d9}_stream_decodes_pixel_exact`
and `a_444_superres_arm` (site 46871) by
`a_444_lossy_superres_mode1_den9_stream_decodes_pixel_exact` (and the
`mode2_random_denom` gate, which calls the same helper through a second
path). Counting direct `NAME`-bearing tests only: **16 gates**, plus four more
reaching the same probe through a shared helper. The
`aomdec_path().is_file()` string now survives only in two doc comments, two
hard asserts, and the probe's own body.

### The six gates the review named, routed in r2

| gate | r2 site (post-rebase line) | shape it had | now |
|---|---|---|---|
| `a_444_lossless_sb64_intrabc_rect_chroma_walks_4x4_units` | `stream.rs:46246` | **inverted** `if !aomdec_path().is_file() { eprintln!(… "pixel arm skipped"); return; }` | `if !aomdec_available(NAME) { …; return; }` — the probe has already said SKIP or failed |
| `a_lossless_block_clips_its_transform_grid_at_the_frame_edge` | `stream.rs:6972` | `if aomdec_path().is_file() { … } else { eprintln!("SKIP … aomdec arm: no oracle aomdec at {}") }` | `if aomdec_available(NAME) { … }`, else arm deleted |
| `a_444_lossy_rect4_strip_stream_decodes_pixel_exact_at_odd_and_wide_geometries` | `stream.rs:46470` | nested `if aomdec_path().is_file()`, else arm deleted in r2 | `if aomdec_available(NAME) { … }` |
| `a_444_lossy_superres_stream_decodes_pixel_exact` | `stream.rs:46699` | `if aomdec_path().is_file() { … } else { eprintln!("… pixel arm skipped") }` | `if aomdec_available(NAME) { … }` |
| `a_444_superres_arm` (shared helper; reached by `a_444_lossy_superres_mode1_den9_stream_decodes_pixel_exact` and `a_444_lossy_superres_mode2_random_denom_stream_decodes_pixel_exact`) | `stream.rs:46886` | `if aomdec_path().is_file() { … } else { eprintln!("… pixel arm skipped") }` | `if aomdec_available(name) { … }` |
| `a_distance_weighted_compound_stream_decodes_pixel_exact` | routed in **r1** (one of the 13) | bare guard, no env escape | `if aomdec_available(NAME) { … }` |

**Redundant `else { eprintln!("… pixel arm skipped") }` arms deleted: 7** (6 in
the scripted pass, 1 found afterwards by running the gates and reading the
output — `a_lossless_block_clips_its_transform_grid_at_the_frame_edge` printed
TWO skip lines for one skip, which is how the survivor was located). After
that, a skip prints exactly one line, from the probe.

## 8. Red-before, re-proven on the CURRENT tree

Base for this proof is **r1 as rebased** (r2's edits reverted by
`git checkout --`, i.e. exactly what main `9b2f6c9d` would give with r1
applied) — the point is that a lane merging AFTER r1 branched reintroduces the
class, so the proof has to stand on the tree the six gates actually live in.

`EC_AV1_AOMDEC=/nonexistent/oracle/aomdec EC_AV1_REQUIRE_AOMENC=1`:

| gate | before r2 | after r2 |
|---|---|---|
| `a_444_lossless_sb64_intrabc_rect_chroma_walks_4x4_units` | `… 0 split; no oracle aomdec at /nonexistent/oracle/aomdec, pixel arm skipped` | **FAILED**: `no oracle aomdec at /nonexistent/oracle/aomdec -- the pixel compare this gate exists for would be skipped, and EC_AV1_REQUIRE_AOMDEC/EC_AV1_REQUIRE_AOMENC is set.` |
| `a_lossless_block_clips_its_transform_grid_at_the_frame_edge` | `SKIP … aomdec arm: no oracle aomdec at /nonexistent/oracle/aomdec` | **FAILED**, same message |
| `a_444_lossy_rect4_strip_stream_decodes_pixel_exact_at_odd_and_wide_geometries` | two `…: no oracle aomdec, pixel arm skipped` lines (one per pinned fixture) | **FAILED**, same message |

`test result: ok. 3 passed` before, **`test result: FAILED. 0 passed; 3
failed`** after — with `EC_AV1_REQUIRE_AOMENC=1` set, i.e. exactly the mode a
batch run is supposed to be in.

## 9. The other two directions, on the current tree

* **Oracle present** — the six (plus the two superres-mode gates that share
  `a_444_superres_arm`) run and pass: `a_444_lossy_superres_mode1_den9… 4
  decode-order frame(s) byte-exact vs aomdec at (256,128) upscaled from
  [(9,228)…]`, `a_444_lossy_rect4_strip… 12 / 24 own-extent 1:4 chroma
  gather(s)`, `a_444_lossless_sb64_intrabc… 4 lossless rect intra-BC block(s)
  walked 4x4 chroma units, 24576 WHT units`. **7 passed, 0 failed.**
* **Absent, env unset** — one SKIP line per gate, from the probe, naming the
  path and the escape hatch; the gates still pass. Developer-only
  convenience, unchanged from r1 and now impossible to print twice.

Acceptance: `cargo test -p ec-av1 --lib -- gate_coverage refusal_inventory` ->
**26 passed, 0 failed** on the rebased tree (the `enable-rect-tx` staleness
r1 reported is gone — lane-av1toolgates' entries were retired upstream).

## 10. What is still open after r2

* **Shape A stays.** ~445 `if !have_aomenc()` / `if !have_ffmpeg()` early
  returns, plus `have_affine_aomenc` (which does have the assert-last shape).
  They fail under the REQUIRE vars because their probes assert; a future lane
  adding a NEW guard must call a probe rather than re-derive `.is_file()`.
* **The anti-regression is procedural, not enforced.** A guard written as
  `aomdec_path().is_file()` compiles, passes review and re-opens the class. The
  mechanical guard is a source-scan test ("no `aomdec_path().is_file()` outside
  `aomdec_available` and the two documented asserts"); it was NOT added here
  because the source-scan pattern for a whole-file assertion is itself a new
  piece of machinery, and this lane's charter is the probe. It is the obvious
  next step and it is small.
