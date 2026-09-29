# lane-av1fixtureshape — the two live code-shape violations in `crates/ec-av1`

Branch `lane-av1fixtureshape`, worktree `~/.cache/wt/av1fixtureshape`.

| commit | what |
|---|---|
| `2364d7a7` (merged as `53b851a7`) | the two sites in `crates/ec-av1` |
| `0790e90c` (parent `12cae635`) | restores the verbatim literal in the golden7 doc comment |

Not pushed. Primary checkout clean. **The branch is rebased onto `12cae635`
(`lane-av1fixlib`), so `scripts/verify-fixture-library.sh`,
`pin-gate-audit.py`, `gen-fixture-library.sh` and `fixture-library.tsv` are
committed in this tree** and every run below is from the committed tree. An
earlier revision copied them in untracked; that is superseded.

---

## 1. The two sites, as the preflight printed them

`EC_REQUIRE_FIXTURES=1 EC_FIXTURE_SHAPE_STRICT=1
EC_FIXTURES=<primary>/fixtures scripts/verify-fixture-library.sh`

**Site 1 — invariant 1, quoted:**

```
FAIL [invariant 1]: a committed pin is reached through the gitignored root
      fixtures/ (silent SKIP on every clean checkout and runner):
  .../crates/ec-av1/src/stream.rs:23980:    /// class: a `concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/...")`
      fix: commit the pin under crates/<crate>/fixtures/ and read it with
      that crate's pin helper (crate_pin / pin_dir), never /../../fixtures/.
```

**Site 2 — invariant 4, quoted:**

```
FAIL [invariant 4]: pin-reading gates whose pins are not committed
  crates/ec-av1/src/stream.rs:31386:pinned_warp_stream_decodes_pixel_exact  /../../fixtures  bare directory literal feeding 14 runtime name(s): no committed tree satisfies it
  crates/ec-av1/src/stream.rs:31386:pinned_warp_stream_decodes_pixel_exact  concat!  14/14 pin(s) not committed+tracked, e.g. warp-mismatch
  crates/ec-av1/src/stream.rs:31386:pinned_warp_stream_decodes_pixel_exact  /../../fixtures + 14 runtime name(s)
      fix: commit each pin under crates/<crate>/fixtures/ and read it
      through that crate's pin helper, not /../../fixtures/.
```

### Site 1 was a SCANNER defect, and the report named the wrong gate

`lanes/av1fixlib.report.md` §6 attributes site 1 to
`pinned_lr_sgr_stream_call_unique_dump` reading
`/../../fixtures/lr-sgr-r7.obu` through the gitignored root. **At this base
that gate is already correct** — `stream.rs:30519` reads
`crate_pin("lr-sgr-r7.obu")` and the pin is committed at
`crates/ec-av1/fixtures/lr-sgr-r7.obu`. A direct grep of the invariant-1
pattern over `crates/*/src` and `crates/*/tests` returns exactly one hit, and
it is the line quoted above.

That one hit is a **doc comment**, in
`pinned_golden7_stream_decodes_pixel_exact`, reproducing the forbidden literal
verbatim to explain the shape that gate removed:

```rust
    /// LIVE since lane-av1pinslive r3. The old shape was the worst of the
    /// class: a `concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/...")`
    /// literal, which points at the GITIGNORED root and can therefore never be
    /// satisfied by a committed pin, combined with a bare `.expect()`. The
```

Invariant 1 was a plain `grep -rn` and could not tell prose from code, so the
crate carried an enforced "violation" that was a sentence explaining a fixed
bug. The correction is two-sided, and both halves were needed:

- **the prose stays.** The sentence is what a reader needs; it is the only
  record of why that gate's shape was the worst of its class. It is restored
  verbatim in `0790e90c` after `2364d7a7` had reworded it.
- **the scanner is fixed.** `lane-av1fixlib` `12cae635` strips `//` and
  `/* */` before invariant 1 matches, via a python scan rather than grep, and
  plants a **positive control** — a known forbidden literal in a scratch `.rs`
  — that fails loudly if the stripper ever stops matching. That is the floor
  that makes a comment-stripping bug impossible to mistake for an all-clear.

Verified in this tree with the literal present and the shape fatal by default:

```
  invariant 1: no root-fixture pin path
  code-shape violations: 0
verify-fixture-library: GREEN (303 rows)          exit 0
```

## 2. What each site said before and after

### Site 1 — doc comment of `pinned_golden7_stream_decodes_pixel_exact`

The text is **unchanged from `0dfdf0c8`**. `2364d7a7` reworded it, `0790e90c`
restores it exactly:

```rust
    /// LIVE since lane-av1pinslive r3. The old shape was the worst of the
    /// class: a `concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/...")`
    /// literal, which points at the GITIGNORED root and can therefore never be
    /// satisfied by a committed pin, combined with a bare `.expect()`. The
    /// bytes are now committed at
```

No code changed; the gate's behaviour is untouched.

### Site 2 — `stream.rs:31388`, `pinned_warp_stream_decodes_pixel_exact`

Before: one directory literal, fourteen runtime names, no committed pin.

```rust
    #[test]
    #[ignore = "reads pinned fixture paths under the gitignored fixtures dir; run manually"]
    fn pinned_warp_stream_decodes_pixel_exact() {
        if !have_ffmpeg() { ... return; }
        let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures");
        let paths: Vec<String> = match std::env::var("EC_AV1_GATE_DUMP_PIN") {
            Ok(p) => vec![p],
            Err(_) => ["warp-mismatch", "warp-flake-5", "warp-flake-7",
                "ii-flake-1", "ii-flake-2", "ii-flake-3", "ii-flake-5",
                "ii-flake-6", "ii-flake-7", "ii-flake-8", "ii-flake-9",
                "rect-flake-1", "rect-flake-2", "rect-flake-3"]
                .iter().map(|n| format!("{fixtures}/{n}.obu")).collect(),
        };
        for path in paths { check_pinned_warp_stream(&path); }
    }
```

After: fourteen committed pins, each named by a string literal through
`crate_pin` — the spelling `pinned_golden3`, `pinned_golden4`,
`pinned_golden7` and `pinned_sbpart` already use, one per pin, no second
dialect. The per-pin provenance comments stay in place, each still attached to
the pin it describes.

```rust
    #[test]
    fn pinned_warp_stream_decodes_pixel_exact() {
        if !have_ffmpeg() { ... return; }
        let pins = match std::env::var("EC_AV1_GATE_DUMP_PIN") {
            Ok(p) => vec![std::path::PathBuf::from(p)],
            Err(_) => vec![
                crate_pin("warp-mismatch.obu"),
                crate_pin("warp-flake-5.obu"),
                /* ... thirteen more, each its own literal ... */
                crate_pin("rect-flake-3.obu"),
            ],
        };
        for path in pins { check_pinned_warp_stream(&path); }
    }
```

`check_pinned_warp_stream` now takes `&std::path::Path` and reads through
`require_pin`, so an absent pin is a hard failure naming the crate-local path
rather than a raw io `expect` panic — the helper the sibling gates use.

**All fourteen pins were recoverable**, each committed byte-for-byte under
`crates/ec-av1/fixtures/`:

| pin | sha256 (16) | pin | sha256 (16) |
|---|---|---|---|
| `warp-mismatch.obu` | `24cca5beb41c1078` | `ii-flake-6.obu` | `84a1f60011dfdb77` |
| `warp-flake-5.obu` | `1a99f9e5b257eed0` | `ii-flake-7.obu` | `61ba4d114aa55ebc` |
| `warp-flake-7.obu` | `c2f8b93f5a59c56b` | `ii-flake-8.obu` | `f19d3d80ed564850` |
| `ii-flake-1.obu` | `700ded3e61e15bc3` | `ii-flake-9.obu` | `0df4874737cc6152` |
| `ii-flake-2.obu` | `edc4ad3a6c00c6bc` | `rect-flake-1.obu` | `a3869b5d2bd8a8bc` |
| `ii-flake-3.obu` | `994bfbdc952ecef1` | `rect-flake-2.obu` | `c780ddee1adf25cf` |
| `ii-flake-5.obu` | `f43fe77ea0e8ddd9` | `rect-flake-3.obu` | `3b23d204549b7bcf` |

### The `#[ignore]`, and why it came off

The old reason ("reads pinned fixture paths under the gitignored fixtures
dir") became factually false with the shape fix. The acceptance was that a
plain run must actually execute the gate — a gate un-ignored while its pins
are absent is the same no-op with a better excuse. It is not absent, and it is
not vacuous: `--nocapture` shows all fourteen paths resolving under
`crates/ec-av1/fixtures/` (not the gitignored root) with
`warp_selected_hits` carried 0 → 113 across the set. Had any pin been
unrecoverable the gate would have stayed `#[ignore]`d with the reason. Cost is
1.7 s plus ffmpeg, which `have_ffmpeg()` already gates.

## 3. Generalisation — what the tooling cannot see

Three findings, all measured. The third is **open and blocks the wave**.

**1. A gate can vanish from the census, and the obvious refactor of the code
just landed would do it.** The census finds names only through a root literal
plus `[..].iter().map(|n| format!("{var}/{n}.obu"))`, and through
`crate_pin\(\s*"([^"]+)"` — a **string literal**. The DRY spelling
`crate_pin(&format!("{n}.obu"))` matches neither, so the loop hits `continue`
and the gate is not counted at all: a green `uncommitted=0` would then mean
"this gate is absent from the census", the same silent-SKIP class wearing a
census. That is why each pin is spelled out individually. `12cae635` closes
this one: the audit now emits a `BADCALL` row and the preflight **fails** on a
non-literal `crate_pin` argument, because a gate the census cannot resolve is
unproven rather than clean.

**2. The census prints one `GATE` row per NAME, not per gate.** The `crate_pin`
branch prints inside the name loop, so the 14-pin warp gate prints 14 identical
`GATE` lines: measured 21 printed rows for 8 gates at `12cae635`, 20 for 7
once the doc comment below removes one. `COUNT total=` is the real number.
`lane-av1fixlib` owns the printing fix; it is cosmetic, not a counting bug.

**3. OPEN — `pin-gate-audit.py` strips no comments, and that silently un-counts
a gate.** Invariant 1 was fixed for comment-blindness; the census was not.
`12cae635` has no strip helper and no `code` variable in the audit at all
(`grep -c code` = 1, incidental). `ROOT_RE.search(body)` (line 131),
`JOIN_RE.search(body)` (136) and the `crate_pin` singles (146) all run on the
raw body; only the name-list branch strips `//`, as before. Measured on
`12cae635` itself, both directions:

```
  12cae635 pristine                    -> COUNT total=8 committed=21 uncommitted=0
  12cae635 + the restored doc comment  -> COUNT total=7 committed=20 uncommitted=0
```

`pinned_golden4_stream_decodes_pixel_exact` (`stream.rs:23950`) stops being
counted entirely — no `GATE` row, no `NAME` row — and `uncommitted=0` still
reads clean, so **nothing goes red**. Cause: `fn_bodies` runs a gate's body
from its `fn` line to the *next* `fn` line, so golden4's captured body spans
23950–23987 and golden7's doc comment (23974–23986) sits inside it.
`ROOT_RE` matches the comment, `root_lit` becomes `/../../fixtures/...`, the
gate takes the root-literal branch, finds no bracketed list and no
`"/fixtures/..."` literal, and hits `continue`. A comment moved a gate out of
the census.

This is finding 1's failure mode reached through the other door: the gate is
not unreadable, it is reclassified into a branch that counts nothing. It also
means the site-1 prose is only safe to land once the audit strips comments on
all three searches. Reported to `lane-av1fixlib` with both measurements, the
mechanism, the one-line shape of the fix, and a request for a census positive
control for this specific shape — their existing control only proves the
scanner finds a bad literal, and would not have caught this.

## 4. Red / green

Measured on this branch's own commits, same tree, same committed scripts.

**Red — fix reverted** (`git stash push -- crates/ec-av1/src/stream.rs
crates/ec-av1/fixtures/`, against the pre-comment-strip tool):

```
FAIL [invariant 1]: a committed pin is reached through the gitignored root
  .../crates/ec-av1/src/stream.rs:23980:    /// class: a `concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/...")`
  pin gates: total=7 committed=6 uncommitted=14 ignored=1 assertless=0
FAIL [invariant 4]: pin-reading gates whose pins are not committed
  crates/ec-av1/src/stream.rs:31386:pinned_warp_stream_decodes_pixel_exact  /../../fixtures  bare directory literal feeding 14 runtime name(s): no committed tree satisfies it
  crates/ec-av1/src/stream.rs:31386:pinned_warp_stream_decodes_pixel_exact  concat!  14/14 pin(s) not committed+tracked, e.g. warp-mismatch
```

named exactly the two sites, `uncommitted=14`.

```
EC_FIXTURE_SHAPE_STRICT=1 -> exit 1
```

**Green — fix applied** (`12cae635` tool, `0790e90c` crate, shape fatal by
default):

```
  invariant 1: no root-fixture pin path
  invariant 4: every pin-reading gate resolves through committed copies
  pin gates: total=7 committed=20 uncommitted=0 ignored=0 assertless=0
  code-shape violations: 0
verify-fixture-library: GREEN (303 rows)          exit 0
```

`total=7` rather than 8 is finding 3 above, not a regression in the fix — the
missing gate is golden4, present in the red run's tool and removed from the
count by the doc comment the preflight now correctly ignores. It is reported
as a defect in the census rather than papered over by re-adding the reword.

**Caveat on the exit code, stated not hidden.** The script's exit status is the
*LIBRARY* verdict (root present, paths resolve, no manifest drift), not the
shape verdict. In this tree the library verdict is clean — `EC_FIXTURES` points
at the primary checkout's library and the manifest is regenerated against this
tree. The earlier DRIFT reds in this lane's history were the manifest's
`required-by file:line` column generated against a different `stream.rs`;
rebasing onto `12cae635` resolved them.

## 5. Gates

Lane-scoped `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1fixtureshape`
throughout — the shared `$HOME/.cache/cargo-target` was observed serving an
`ec_av1` binary built from another worktree of the same package. `stream.rs`
`touched` before every run.

```
cargo check -p ec-av1 --all-targets
    Finished `dev` profile in 7.73s                       (no warnings)

cargo test -p ec-av1 --lib -- gate_coverage refusal_inventory
    ok. 28 passed; 0 failed; 0 ignored; 719 filtered out; 0.91s

cargo test -p ec-av1 --lib -- pinned_ golden
    ok. 19 passed; 0 failed; 2 ignored; 726 filtered out; 1.61s
      including pinned_lr_sgr_stream_call_unique_dump, pinned_golden7, pinned_sbpart,
      pinned_warp_stream_decodes_pixel_exact, rect_sizes_pinned_against_libaom

cargo test -p ec-av1 --lib -- pinned_golden7 pinned_warp
    ok. 2 passed; 0 failed; 745 filtered out; 2.02s       (after the doc-comment restore)
```

Gate bodies touched: exactly two — `pinned_warp_stream_decodes_pixel_exact`
(rewritten) and `pinned_golden7_stream_decodes_pixel_exact` (doc comment only,
no behaviour). The fourteen new pins are consumed by the warp gate alone, and
that gate is in every run above.

## 6. Handed over

`0790e90c` to `lane-av1fixlib` for the crate half. Nothing open on the fourteen
pins. One item open against their branch before it goes to the wave: the
census comment-stripping in finding 3.
