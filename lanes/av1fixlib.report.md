# lane-av1fixlib — the fixture preflight, and the sweep that motivated it

Branch `lane-av1fixlib`, rebased onto the merge wave (`main` is an ancestor; the
last merge is recorded in the log). Not pushed.

## What this lane is

A test that cannot find its fixture prints `SKIP` and returns, so the suite
reports PASS having tested nothing. That cost a 3-hour suite red from a missing
repo-root `fixtures/` and three hosts whose fixture libraries differed silently.
Two halves: kill the shape in the test crates, and ship a preflight that keeps
hosts honest.

## 1. The sweep — and what this branch actually carries

`EC_REQUIRE_FIXTURES` is the escape: unset, an absent fixture SKIPs as before
(local convenience); set, it is a HARD FAILURE naming the path and the script
that regenerates it. The helper's order is load-bearing — probe, assert, return
— and never a probe folded into the same `if` as the escape, which is the
short-circuit that made the in-tree `have_ffmpeg` model silently green.

**Crates and files carrying the guard on this branch** (exact list, replacing the
earlier "four crates" claim, which was wrong in the same way a stale ignore
reason is):

| crate | files | what |
|---|---|---|
| ec-aac | `tests/oracle.rs`, `tests/sbr_real_library.rs` | 5 + 9 fixture sites, plus a `probed >= 16` floor on the synthetic HE-AAC matrix |
| ec-flac | `tests/encode_matrix.rs` | corpus guard + a `sources.len() >= 5` floor |
| ec-h264 | `tests/encode.rs` | real-library manifest read and its "no H.264 in the manifest" escape |
| ec-mp3 | `tests/encode_matrix.rs` | the WAV corpus guard, `>= 8` floor, a `read_dir` that swallowed IO errors as an empty list |
| ec-vp9 | `tests/hbd_exact.rs`, `inter_pixels_exact.rs`, `subsampling_exact.rs`, `scratch_interdump.rs` | `Option`-returning corpus helpers and a per-vector skip |
| ec-vorbis | `tests/oracle.rs` | **no guard, deliberately** — see below |

**ec-vorbis has no `EC_REQUIRE_FIXTURES` guard and does not get one.** Its three
fixture sites sweep the operator's own media library; making that a hard failure
would turn one person's home directory into a red fleet on every other host. The
crate carries non-empty floors instead (`rows > 0` on each sweep), because a run
that encoded nothing is a measurement of nothing even when the corpus is
legitimately absent.

**ec-h264 `conformance.rs`, ec-opus `conformance.rs` and ec-flac
`xiph_vectors.rs` are main's version verbatim on this branch.** The wave landed
the same class fix for those three crates in its own `require_fixtures` shape.
Carrying both would be two conventions in one file and a 1054-line diff in a
preflight lane, so the conversion was dropped rather than kept. Residual gaps in
main's versions, for whoever owns them: `jvt_full_sequence_bit_exact` still ends
in a bare `assert!(failed.is_empty())` over a zero-iteration-capable loop, its
per-vector `find_stream(dir) -> None => continue` is still a silent per-vector
skip, and `xiph_vectors.rs` still has three loop ends with no floor.

Also out of scope by design, and reported rather than converted: tool-absence
SKIPs (ffmpeg/x264/aomenc on PATH — a different gate with its own env) and host
media corpora.

## 2. The preflight

`scripts/fixture-library.tsv` is GENERATED, never hand-typed. Columns:

```
path <TAB> required-by file:line <TAB> class <TAB> generator <TAB> status <TAB> tracked <TAB> sha256
```

`class` is separate from `generator` because a recovered witness is not a
re-encode: `regenerated` (192+ rows, a script reproduces the bytes), `captured`
(committed bytes whose only provenance is a gate comment — a FINDING, no recipe
recorded), `recovered-original` (the runner-library bytes, sha256 recorded).

`scripts/verify-fixture-library.sh` checks SHAPE, RESOLVE (every path exists and
every directory is non-empty) and DRIFT (regenerating reproduces the committed
ROWS), and distinguishes two failure modes with different fixes:

- **mode i** — the fixture ROOT is absent. `fixtures/` is gitignored
  (`.gitignore:2`), so a linked worktree never has it; fix the TREE
  (`scripts/link-fixtures.sh`, or point `EC_FIXTURES` at a staged library).
- **mode ii** — a path is absent, or a directory is empty, INSIDE a tree that has
  a root. Fix the LIBRARY: run the generator named in the row.

`scripts/pin-gate-audit.py` is the gate census. Matching `crate_pin("X")` is
blind to a gate that reads N pins through a directory literal plus a runtime
name list — `pinned_warp_stream_decodes_pixel_exact`
(`crates/ec-av1/src/stream.rs:31099` in the tree this was written against) reads
fourteen that way, none committed under the crate, and the gate is `#[ignore]`d,
so it is a no-op that reports nothing. The audit resolves the root literal,
strips line comments (the name list has comments between its items, which is
why a bracket-shaped parse finds nothing), anchors on the `format!` that
consumes the list, and enumerates every runtime name. It also classifies
assertions through the helper CALL GRAPH, not the gate body: the warp gate has
no `assert` of its own but calls `check_pinned_warp_stream`, which asserts all
three planes per frame — a body-only read calls it assertless, which is the
misclassification that happened earlier today.

Census, identical on this box and on all three runner hosts:

```
pin gates: total=8 committed=7 uncommitted=14 ignored=3 assertless=0
```

## 3. The four invariants, and how each fails

All four are reported on every run; the count is always printed.

1. **No committed pin through the gitignored root.** The shape
   `concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/<file>")` can never be
   satisfied by a committed pin, so its gate skips on every clean checkout and
   runner — the film-grain instance combined it with `.unwrap()` and PANICKED
   instead. A root *subdirectory* is deliberately not this shape: a generator
   produces those.
2. **Every committed pin present AND tracked.** A pin on disk but untracked dies
   with the machine's scratchpad and its gate skips GREEN. `tracked` is
   computed at generation time, so a host with no `.git` asserts the committed
   value.
3. **The `.gitignore` negation is intact** — `!crates/*/fixtures/**` checked
   statically (works with no `.git`), plus `git check-ignore` in a git tree. A
   plain `git add` of a new pin has to work: that is what let four recovered and
   three regenerated pins land today.
4. **The pin-reading gate census** above, including the bare-directory verdict.

**Exit status: shape is now FATAL BY DEFAULT.** The LIBRARY verdict (mode i, mode ii,
drift) decides the exit under `EC_REQUIRE_FIXTURES=1`. The four code-shape
invariants fail only under `EC_FIXTURE_SHAPE_STRICT=1`. A shape violation is a
defect in a file this preflight does not own — two live sites in `crates/ec-av1`
— and a batch that cannot start because another crate has two open lines helps
nobody. Nothing is hidden: the count is printed every run and the sites are named
every run.

The split existed only while two `crates/ec-av1` violations were open and owned by
another lane. Kerem-5's `2364d7a7` closed both — the root pin now reads
`crate_pin("lr-sgr-r7.obu")`, and all fourteen warp pins are committed under
`crates/ec-av1/fixtures/` and read through the pin helper — so
`SHAPE=${EC_FIXTURE_SHAPE_STRICT:-1}` is the default now. `--strict` and
`EC_FIXTURE_SHAPE_STRICT=0` are the escape hatches, in that direction.

Measured exit codes after the flip:

| mode | exit |
|---|---|
| A intact, `EC_REQUIRE_FIXTURES=1` | 0 — GREEN (303 rows) |
| B one fixture renamed, flag set | 1 — `FAIL [mode ii]`, names path, both sites, generator |
| C same rename, flag unset | 0 — the same list printed SKIP-shaped |
| D `EC_FIXTURES=/nonexistent`, flag set | 1 — `FAIL [mode i]`, names `.gitignore:2` |
| E intact, flag set, `--strict` | 0 — was 1 before the flip |
| F a reintroduced `concat!` root-pin line, flag set | 1 — `FAIL [invariant 1]` |

F is the point of the flip: reintroducing the shape is now fatal with no flag.

Three defects this lane's own tooling had, all found by running it off its own
tree, and all the class it exists to prevent:

1. Off-git the `tracked` column was recomputed as `no-git` for all 37 pin rows,
   so every host reported DRIFT with a good library.
2. Invariant 1 could not see a comment: a doc comment that REPRODUCES the
   forbidden literal to explain what was fixed read as a live violation for a
   whole round. It now strips comments before matching, the way the gate census
   already did.
3. The census's single-name branch neither tracked `ignored` (every `crate_pin`
   gate reported `ignored=0` whether or not it was `#[ignore]`d) nor noticed a
   `crate_pin` whose name is built at runtime — such a gate would have vanished
   from the census silently, the same blind spot in a new place. It now counts
   `ignored` and prints a `BADCALL` row the preflight fails on, because a gate
   the census cannot resolve is UNPROVEN, not proven clean.

## 4. Proof

**Green, fixtures present, `EC_REQUIRE_FIXTURES=1`:**

| suite | result |
|---|---|
| ec-h264 conformance | 23 passed, 0 failed |
| ec-h264 encode | 12 passed, 1 ignored |
| ec-flac xiph_vectors / encode_matrix | 4 passed / 2 passed |
| ec-opus conformance | 28 passed, 16 ignored |
| ec-aac oracle | 14 passed |
| ec-mp3 encode_matrix | 5 passed, 2 ignored |
| ec-vorbis oracle | 8 passed, 3 ignored |

(`EC_NOMEMGUARD=1` throughout: the repo's memguard runner aborts on a stale
systemd scope. CI plumbing, not a test bypass.)

**Red, by renaming a real fixture, flag set, all restored:**

- `mv fixtures/video/h264-open-gop.mp4{,.bak}` →
  `panicked at crates/ec-h264/tests/conformance.rs:83:5: EC_REQUIRE_FIXTURES=1
  but fixture …/fixtures/video/h264-open-gop.mp4 is absent -- regenerate with:
  scripts/gen-fixtures.sh` → `0 passed; 1 failed`
- `mv …/flac-test-files-main/subset{,.bak}` → the same assert, 2 failed
- `mv …/opus_testvectors/testvector01.bit{,.bak}` → the same assert, 1 failed

**Preflight, four modes:**

- intact, flag set → `GREEN (290 rows)`, exit 0
- one fixture renamed, flag set → exit 1, `FAIL [mode ii]` naming the path, both
  referencing sites and the generator
- same rename, flag unset → exit 0, the same list printed SKIP-shaped
- `EC_FIXTURES=/nonexistent` → exit 1, `FAIL [mode i]` naming `.gitignore:2` and
  "fix the TREE, not the code"

**Not vacuous.** On two runner hosts: hardlink-copy the staged library, remove one
file from the COPY, rerun → RED naming the path, both referencing sites, the
generator and the drift row; the real library untouched (793/802 files before
and after) and GREEN.

**Reproducible artifact.** `cp tsv; regenerate; cmp` → byte-identical, on this
box. On all three hosts, `grep -c '/home/' scripts/fixture-library.tsv` → 0: the
generated header carries no absolute path (it says `worktree-relative` and
resolves through `EC_FIXTURES` at run time), so the drift check is not
structurally red off the generating worktree.

**Three-host run** (source tarball, `fixtures/` excluded, each host's own library
at `~/gates/library/fixtures` via `EC_FIXTURES`) — 178.105.165.182 (793 files),
51.195.223.40 (793), 2.28.124.204 (802): all three `verify_exit=0`,
`GREEN (290 rows)`, `resolve: 0 missing, 0 empty`, the same census, `code-shape
violations: 2`.

**The hosts really do differ, and the preflight is right to be silent about it.**
h1 and h2 have byte-identical library listings (same md5 over `find | sort`, 793
files); h3 has 802 — nine extra paths (`hbd-r5/*`, `part32/troy-extract.obu`,
`realworld/*`, `sbpart/seed42.*`, `sub8`). No code literal reaches any of them.
That is the measured blind spot, not a sync failure.

## 5. Blind spots, stated rather than papered over

- Enumeration is grep-derived. 188 media-name literals in this tree resolve under
  no enumerated fixture directory; they are covered only as far as their parent
  directory, which must exist and be non-empty. The h3 difference above is
  exactly this blind spot.
- The pin-gate audit anchors on a `format!` against the root variable. A gate
  that builds its path by `PathBuf::push` in a loop, by string concatenation, or
  from a name read at runtime out of a manifest is not enumerated; it would need
  its own shape added.
- The audit resolves one level of helper indirection for assertion
  classification. `assertless=0` today, so nothing is currently hidden by that
  limit, but it is a real limit.
- The `tracked` column and the fixture-root path are committed provenance, not
  host state: off-git the column is normalised and the path is never written, so
  a host asserts the committed value instead of recomputing what it cannot know.
- The recovered-pin self-validation has two halves. This script decides the hash
  half — every `recovered-original` row must hash to its recorded sha256, and a
  re-encode fails it. **The behavioural half now runs too:**

  ```
  CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1fixlib EC_NOMEMGUARD=1 \
    cargo test -p ec-av1 --lib -- \
      --exact stream::tests::pinned_golden7_stream_decodes_pixel_exact \
      --include-ignored --nocapture
  non_last_ref_hits before=0 after=2
  test stream::tests::pinned_golden7_stream_decodes_pixel_exact ... ok
  test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 745 filtered out
  ```

  The recovered pin reproduces exactly the 0→2 its doc records — the
  self-validation a re-encode cannot pass. Two notes for whoever repeats it: the
  test is LIVE in the merged tree (no `#[ignore]`), so `--include-ignored` is
  belt-and-braces, and a lane-scoped `CARGO_TARGET_DIR` is REQUIRED — the shared
  `$HOME/.cache/cargo-target` held a stale `ec_av1` binary from another worktree
  of the same package, and its panic pointed at a line that does not exist in
  this source.
- `pinned_lr_sgr_stream_call_unique_dump` stays `#[ignore]`d for a true reason:
  it prints per-frame mismatch counts and asserts nothing.

## 6. The two shape violations: what they were, and who fixed them

A CORRECTION to an earlier version of this section, because a report naming an
already-fixed gate sends the next reader to the wrong file.

- `pinned_lr_sgr_stream_call_unique_dump` was named here as reading
  `/../../fixtures/lr-sgr-r7.obu`. **It does not.** At the base this was measured
  on, that gate already read `crate_pin("lr-sgr-r7.obu")` (`stream.rs:30519`)
  and the pin is committed at `crates/ec-av1/fixtures/lr-sgr-r7.obu`.
- The only invariant-1 hit in the tree was a **DOC COMMENT** inside
  `pinned_golden7_stream_decodes_pixel_exact` that reproduces the forbidden
  literal verbatim while explaining the shape it removed. A `grep` cannot tell
  prose from code. The comment is correct and stays; the scanner was wrong.
- The one real violation was `pinned_warp_stream_decodes_pixel_exact`
  (`stream.rs:31386`): the `/../../fixtures` directory literal plus a 14-name
  runtime list, none of the fourteen committed under the crate.

All of it is closed as of `53b851a7` (merging Kerem-5's `2364d7a7`): the warp
gate's fourteen pins — `warp-mismatch`, `warp-flake-5`, `warp-flake-7`,
`ii-flake-1/2/3/5/6/7/8/9`, `rect-flake-1/2/3` — are committed under
`crates/ec-av1/fixtures/` and each is read through `crate_pin` with a string
literal, and the gate is no longer `#[ignore]`d because a plain run proves it
executes (`warp_selected_hits` 0 → 113, 1 passed in 1.8 s plus ffmpeg). Census
after the fix: `total=8 committed=21 uncommitted=0 ignored=0 assertless=0`,
`code-shape violations: 0`, and shape is fatal by default.

The four pins recovered earlier in this lane (`golden4-pin.obu` 137 B
`1754023e…`, `golden6-mismatch.obu` 452 B `c56909b9…`,
`golden7-forwarding-mismatch.obu` 152 B `81b3bf65…`, `lr-sgr-r7.obu` 192 B
`6b95b20e…`) are committed with `recovered-original` provenance; they lived only
in the gitignored root, i.e. only in each machine's scratchpad.
