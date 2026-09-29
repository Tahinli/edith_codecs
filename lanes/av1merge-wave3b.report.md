# Merge wave 3b — two branches

Integration owner: wave 3b only. Base `main` @ `3eec02c8` (746 lib tests, the
wave-3a head). Head after this wave: `b1a462fa` (748 lib tests). **Not pushed.**

| # | branch | tip merged | conflicts | landed delta on main |
|---|--------|-----------|-----------|----------------------|
| 1 | `lane-av1rawhelper` | `cf36ba5d` | none (auto-merged) | `stream.rs` +194/-12, report +305 |
| 2 | `lane-av1oracleskip` (r1+r2) | `7bf95961` | **1 file, 2 regions** — `stream.rs` | `stream.rs` +92/-100, report +266 |
| 3 | `lane-av1oracleskip` (r3) | `7a865ee7` | **1 file, 2 regions** — `gate_coverage.rs` | `gate_coverage.rs` +167/-0, report +83 |

Branch 2 is one branch delivered in two merges because the tip moved mid-wave
(`7bf95961` → `7a865ee7`, adding r3). `7bf95961` is an ancestor of `7a865ee7`, so
the second merge's base was `7bf95961` and it carried r3 alone — `git merge-tree`
confirmed the conflict before either attempt, and `git log` confirms
`7bf95961..7a865ee7` is the single r3 commit.

Tip drift check: both dispatched tips verified against the live tree before
merging. `lane-av1rawhelper` @ `cf36ba5d` as dispatched;
`lane-av1oracleskip` was re-read after the update and re-verified before use.

## 1. lane-av1rawhelper — the arity strand

`git merge-tree` said clean. Per the wave-3a rule that a clean textual merge does
not prove a compiling tree, the merged tree was compiled and the call sites
audited rather than trusted.

**The merged tree compiles.** `cargo check -p ec-av1 --all-targets` clean.

**Every call site is 5-arg, and the depths are right.** `assert_rawvideo_matches`
gained a `bit_depth: u8` parameter. All four call sites carry it:

| site | call | belongs to gate |
|------|------|-----------------|
| 5232 | `(&obu, &stream, NAME, frames, 10)` | `the_rawvideo_helper_compares_real_samples_at_the_streams_own_bit_depth` (new) |
| 5247 | `(&obu, &stream, NAME, frames, 8)` | same gate, 8-bit arm |
| 7066 | `(&ctrl, &ctrl_stream, NAME, FRAMES, 10)` | `a_lossless_444_128_root_lossless_stream_reads_chunks_chunk_major` — **main's former `:6889`** |
| 7155 | `(&obu, &stream, NAME, FRAMES, 10)` | `a_lossless_block_clips_its_transform_grid_at_the_frame_edge` — **main's former `:6983`** |

The two main-side sites moved only by line offset (the merge's insertions); the
arguments are otherwise identical to main's, plus the depth.

**The depth is verified against the stream, not the gate's doc.** Both main-side
sites read the same pin, `ll444_128root_lossless.obu` (63429 B, fnv1a64
`0xf07d_47fc_fd51_2658`, which matches `read_pin`'s recorded values). Probed with
an independent decoder, not the crate's own helper:

```
$ ffprobe -v error -show_entries stream=codec_name,profile,pix_fmt -of default=nw=1 \
    crates/ec-av1/fixtures/ll444_128root_lossless.obu
codec_name=av1
profile=High
pix_fmt=yuv444p10le
```

**10-bit**, confirming `10` at both sites. The crate is self-policing besides: the
helper asserts the passed depth against the stream's own parsed sequence header, so
a wrong depth fails by name rather than packing nothing. The new gate exercises
both directions of that check, so it is not a one-way assert.

**E0061 red-before, reproduced here rather than taken on report.** Reverting the
one stranded site to 4-arg in the merged file:

```
error[E0061]: this function takes 5 arguments but 4 arguments were supplied
```

Restored from a saved copy; `git diff` against the merge commit was empty, i.e.
byte-identical.

## 2. lane-av1oracleskip — the routed-probe class

**Conflict shape: genuine two-intent, resolved by composition.** Both sides were
right about their own change, in the same `if` statement:

- ours (rawhelper): `if aomdec_path().is_file() { assert_rawvideo_matches(..., 10) } else { eprintln!("SKIP ...") }`
- theirs (oracleskip): `if aomdec_available(NAME) { assert_rawvideo_matches(...) }`

`@both` would have been meaningless here (competing edits of the same lines, not
additive). Resolved to **theirs' guard + ours' arity** — the probe owns the skip
decision, the depth argument stays:

```rust
if aomdec_available(NAME) {
    assert_rawvideo_matches(&obu, &stream, NAME, FRAMES, 10);
}
```

The `else { eprintln!("SKIP ...") }` arm was **not** carried over: `aomdec_available`
owns the single SKIP line, which is the branch's stated design.

**The class is closed.** Bare `if aomdec_path().is_file()` guard sites, counted on
the branch's own base and on the merged tree:

| tree | code guards | doc-comment mentions |
|------|-------------|----------------------|
| `9b2f6c9d` (base) | **18** | — |
| `7bf95961` (branch tip) | 0 | 2 |
| `b1a462fa` (merged) | **0** | 2 |

The two surviving text matches are both inside `///` doc comments — one in
`have_aomenc`'s prose, one inside `aomdec_available`'s own doc explaining the shape
it replaced. `aomdec_available(` is called at 19 sites (18 routed + the definition).

Five other `aomdec_path().is_file()` uses remain and are **not** the class: two are
`assert!(aomdec_path().is_file(), ...)` hard asserts in
`gate_444_lossy_live_exact` and `a_distance_weighted_compound_stream_decodes_pixel_exact`
(predating this lane, and already failing loudly), and one is `aomdec_available`'s
own body.

**Routing verified in both directions, on a real missing oracle.** The oracle *is*
built on this host, so the missing-oracle path was forced with
`EC_AV1_AOMDEC=/nonexistent/aomdec` rather than assumed:

- no env var → exactly **one** `SKIP <name>: no oracle aomdec at /nonexistent/aomdec -- the pixel compare is NOT running (set EC_AV1_REQUIRE_AOMDEC=1 to make this a failure)` line, and the gate passes;
- `EC_AV1_REQUIRE_AOMDEC=1` → **FAILED**, panicking with
  `no oracle aomdec at /nonexistent/aomdec -- the pixel compare this gate exists for would be skipped, and EC_AV1_REQUIRE_AOMDEC/EC_AV1_REQUIRE_AOMENC is set.`

That is the required behaviour: under `EC_AV1_REQUIRE_AOMDEC=1` the run **fails
rather than skips**. (Note when reading such output: the harness prefixes the test
name on the same line as the SKIP, so a `^SKIP` grep misses it. Count the
substring, not the line start.)

## 3. lane-av1oracleskip r3 — the anti-regression scan, and the add/add

r3 adds `gate_coverage::tests::no_tool_presence_check_outside_its_probe`, a source
scan over the test-bearing files asserting a tool presence check appears only
inside the probe that owns that path, inside an `assert!`, or in a comment. It
conflicted with the same file my wave-3a resolver fix lives in, as predicted.

**Region 1 — comment-only conflict, took ours.** Two versions of the
`enable-tx-size-search` note on `DEFAULT_ON_TOOLS`: ours (txsearch) says the entry
is SETTLED and retired on an arrival assert; theirs is the older
`lane-av1distwtd r3` note saying the count is a floor of 9 unclassified gates and
describing the method that would settle it. The method theirs asks for is
`spelling_values` → `bound_values` — which is exactly what landed. Theirs is the
pre-settlement text, superseded by the change it requests. **Pure comment
conflict; no semantic disagreement existed.** Took ours.

**Region 2 — `@both` was WRONG again, resolved by reconstruction.** The two sides
were additive at a glance (ours: two `#[test]` fns; theirs: one), and both ended
mid-statement:

```
ours  last 3: ['        assert!(', '            pinned >= 10,', '            "...found {pinned}"']
theirs last 3: ['             green without running the oracle compare:\\n  {}",', '            offenders.len(),', '            offenders.join("\\n  ")']
TAIL after >>>: ['        );', '    }', '}']
```

Git had hoisted the shared `        );\n    }\n}` out as common context. `@both`
would have emitted ours' truncated `assert!(`, then theirs' doc comment **inside an
unclosed expression** — not a parse, and not the wave-3a nested-`fn` trap either,
just broken text. Rebuilt by construction instead: ours' block closed with its own
`);` + `}`, then theirs' block in full with its own `);` + `}`, then the module's
`}`. **Both sides kept** — as instructed, the resolver strategy and the scan both
survive:

- `named_column` closure (my wave-3a strategy (3b)) present at `gate_coverage.rs:800`, wired at `:857`;
- the 128x96 mutation case added to `the_resolver_binds_a_format_built_flag_in_both_directions` present at `:1478`;
- `print_tx_size_search_census` `:1362`, `the_resolver_binds_…` `:1439`, `no_tool_presence_check_outside_its_probe` `:1565`.

**The scan is non-vacuous — proven here, not assumed.** Reverting one routed guard
to the bare shape:

```
1 bare tool presence check(s) outside a probe — each one is a gate that can report
green without running the oracle compare:
test result: FAILED. 0 passed; 1 failed
```

Restored; `git diff` against the merge commit empty.

Main's warning that the scan's "recognised all three probes" assert would red if a
branch renamed or moved a probe: it did not. The scan is green at 29 passed / 0
failed, so all three probes were located.

## Cross-branch sweep

| check | result |
|-------|--------|
| `cargo check -p ec-av1 --all-targets` after each of the 3 merges | clean, 3 for 3 |
| `cargo test -p ec-av1 --lib -- gate_coverage refusal_inventory` | **ok. 29 passed; 0 failed; 0 ignored; 719 filtered out** |
| `cargo test -p ec-av1 --lib -- --list \| wc -l` | **750** (748 tests) |
| the 3 gates both branches touch | 3/3 `ok. 1 passed; 0 failed` |
| `EC_AV1_REQUIRE_AOMDEC=1` + missing oracle | FAILED, naming the path |
| missing oracle, no env var | exactly 1 SKIP line, pass |

### Gate-count delta and its cause

| | lib tests | `wc -l` |
|---|---|---|
| `3eec02c8` (pre-wave) | 746 | 748 |
| `b1a462fa` (after) | 748 | 750 |
| **delta** | **+2** | **+2** |

Name-set diff, both directions:

```
ADDED (+2)                                                  REMOVED: none
  gate_coverage::tests::no_tool_presence_check_outside_its_probe   <- oracleskip r3
  stream::tests::the_rawvideo_helper_compares_real_samples_at_...   <- rawhelper
```

Cause per branch: **rawhelper +1** (its new two-arm helper gate),
**oracleskip r1/r2 +0** (a refactor — it reroutes existing guards and deletes 7
redundant `else` arms, adding no gate), **oracleskip r3 +1** (the scan).
`gate_coverage + refusal_inventory` moved 28 → 29 purely by the scan.

## Deliberately NOT touched

- **No push.** Three merges, all local on `main`.
- No `rustfmt`, no workspace-wide build, no other crate's tests.
- The `else { eprintln!("SKIP ...") }` arms that oracleskip deleted were not
  restored in the two conflict regions — the probe owns that line, and reviving
  them would put the class back.
- The two pre-existing `assert!(aomdec_path().is_file(), …)` hard asserts in
  `gate_444_lossy_live_exact` and `a_distance_weighted_compound_stream_decodes_pixel_exact`
  were left alone. They are not the routed-probe class (they fail loudly, not
  silently) and belong to other lanes.
- `stash@{0}` — the foreign `EC_PREDOUT8` edit to `scripts/instrument-aom-oracle.sh`
  attributed to `lane-av1cmpaudit` — still in the stash, not applied. Likewise
  `stash@{1..3}`.
- The `lane-av1rawhelper` worktree showed a transient ` M crates/ec-av1/src/gate_coverage.rs`
  at first inspection; it re-hashed clean and the branch tip was unaffected.
  Nothing from that worktree was merged.
- Every number here was measured in a lane-private
  `CARGO_TARGET_DIR=cargo-target-w3b`. The shared `$HOME/.cache/cargo-target`
  corrupted three separate readings in wave 3a (see
  `skill://counterfactual-ab-target-dir-provenance`, variant 6).
