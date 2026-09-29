# lane-av1cheapwins — recover 2 of 5 cheap wins, triage 3

Base `main` @ `3aa16dd1`. Worktree `/home/tahinli/.cache/wt/av1cheapwins`, branch
`lane-av1cheapwins`. Private `CARGO_TARGET_DIR=/home/tahinli/.cache/ct/av1cheapwins`.

**Landed: A** (1 commit) and **B** (2 commits, the second is my line-ref correction).
**Triaged, not landed: C1/C2/C3** (table in §4).

## Commits on this branch

```
3a621d80 scripts: re-point the oracle DEPTH ASSUMPTION blocks at today's decode.rs lines   <- mine
abd4fb0e scripts: document the 1-byte-per-sample depth assumption on oracle dump rungs    <- 98d878d6
71f1f09c ec-av1: ignore leaked GIT_DIR when the test runner locates the repo              <- 082ef153
```

No conflicts on either cherry-pick. Commands:

```
git worktree add -b lane-av1cheapwins /home/tahinli/.cache/wt/av1cheapwins 3aa16dd1
git -C /home/tahinli/.cache/wt/av1cheapwins cherry-pick 082ef153     # A
git -C /home/tahinli/.cache/wt/av1cheapwins cherry-pick 98d878d6     # B
```

---

## A — `lane-infra-gitdir` @ `082ef153` — LANDED

### What the change actually does

One line in `.cargo/config.toml`. The cargo target runner is a `bash -c` string
that locates the repo before exec'ing `scripts/memguard-runner.sh`:

```diff
-runner = [... "root=$(git rev-parse --show-toplevel) && exec \"$root/scripts/memguard-runner.sh\" \"$@\"" ...]
+runner = [... "root=$(env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE git rev-parse --show-toplevel) && exec ..." ...]
```

Scope check: the three `env -u` vars are stripped **only** for the root-locating
`git rev-parse`. They are not stripped for the test binary, not exported, and
`scripts/memguard-runner.sh` is not touched (`.cargo/config.toml` is the only file
in the commit). So the blast radius is exactly "which directory is treated as the
repo root when locating the runner" — nothing else.

### Direction 1 — leaked `GIT_DIR` (the case the fix claims)

Isolating the command itself, `GIT_DIR` leaked to a package dir, cwd =
`crates/ec-av1`:

```
leaked GIT_DIR=/home/tahinli/Documents/Code/Rust/edith_codecs/crates/ec-av1/.git
OLD: fatal: not a git repository: '.../crates/ec-av1/.git'
     OLD runner=MISSING->exit 127          <- root empty, the reported symptom
NEW: NEW root=[/home/tahinli/Documents/Code/Rust/edith_codecs]
     NEW runner=FOUND
```

End to end, one named test (`temporal_delimiter_round_trips`, `-p ec-av1-syntax`),
OLD runner supplied via `--config` at CLI precedence, NEW = the committed one:

| leaked `GIT_DIR` | OLD | NEW |
|---|---|---|
| (unset — normal case) | exit=0 | exit=0 |
| `<wt>/av1cheapwins/.git` | exit=127 | exit=0 |
| `<main>/.git/worktrees/av1cheapwins` | exit=127 | exit=0 |
| `<main>/.git` | exit=127 | exit=0 |
| `<main>/crates/ec-av1/.git` | exit=128 | exit=0 |

The 127 is the exact reported failure:

```
--: line 1: /home/tahinli/.cache/wt/av1cheapwins/crates/ec-av1-syntax/scripts/memguard-runner.sh: No such file or directory
error: test failed, to rerun pass `-p ec-av1-syntax --lib`
   exit=127
```

### Direction 2 — `GIT_DIR` unset (normal case), no regression

```
$ env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE cargo test -p ec-av1-syntax --lib temporal_delimiter_round_trips
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.01s
     Running unittests src/lib.rs (.../deps/ec_av1_syntax-5faa43bfed45d870)

running 1 test
test tests::temporal_delimiter_round_trips ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 20 filtered out; finished in 0.00s
EXIT=0
```

### One finding worth recording — the fix is necessary but not sufficient while checkouts disagree

Measuring OLD vs NEW in a *worktree* while the primary checkout is still on
unfixed `main`, the leak case failed **nondeterministically** (exit 127 on some
runs, 0 on others), because a leaked `GIT_DIR` pointing into the primary checkout
makes cargo also load the primary checkout's `.cargo/config.toml`, and the
unfixed runner wraps the fixed one:

```
Caused by:
  process didn't exit successfully: `bash -c 'root=$(git rev-parse --show-toplevel) && exec ...' -- bash -c 'root=$(env -u GIT_DIR ...)' -- ...
  (exit status: 127)
```

Confirmed the diagnosis rather than assuming it — with the **fixed** runner
supplied for both levels, every leak passes 5/5:

```
LEAK=<wt>/av1cheapwins/.git                          BOTH configs fixed: 0 0 0 0 0
LEAK=<main>/.git/worktrees/av1cheapwins              BOTH configs fixed: 0 0 0 0 0
LEAK=<main>/.git                                     BOTH configs fixed: 0 0 0 0 0
LEAK=<main>/crates/ec-av1/.git                       BOTH configs fixed: 0 0 0 0 0
```

So the 127s were the split-state artifact of testing pre-merge, not a defect in
the change: **once this merges, every checkout carries the fix and the leak case
holds everywhere.** It does mean the fix only becomes fully effective when the
whole machine is on it — worth knowing, not worth handing back.

### Verdict

Landed. Both directions hold; behaviour is unchanged outside the leak case.

---

## B — `lane-av1oraclerungdepth` @ `98d878d6` — LANDED (with my correction)

### Correction to the charter's premise

The charter described this as "a loud abort plus a checker". **It is neither.**
The commit is **comments only** — 63 added lines to
`scripts/instrument-aom-oracle.sh`, every one a `#` comment, plus a 177-line
report. There is no abort, no assertion, no runtime check. The branch's own
report §3 argues why (the script is a source patcher with no `bit_depth`, no
frame index and no produced-file path to assert on) and that reasoning holds.

So it closes the trap at the **human/script-reading level**, not the runtime
level. A lane that never opens the script still gets a 0-byte dump with no error.
Landed anyway: the four rungs are byte-paired with u8-narrowing decoder dumps by
design, so any loud abort would need both sides changed plus every existing
byte-pairing gate re-pinned. That is a separate lane, correctly deferred.

### Generator evidence

Comments-only, mechanically proven three ways:

```
$ git diff 3aa16dd1 -U0 -- scripts/instrument-aom-oracle.sh | grep -E '^\+[^+]' | grep -vE '^\+ *#'
OK: still comments-only            # zero non-comment added lines

$ bash -n scripts/instrument-aom-oracle.sh
OK

$ python3 ... compile every <<'PY…' heredoc body
heredoc bodies: 19  compiled: 19  failed: none
```

The decisive one — added lines must not land inside a triple-quoted string, since
those *are* emitted into the oracle C. AST-walked all 50 C-emitting string
literals and mapped their lines back to the script:

```
triple-quoted C-emitting string literals found: 50
added lines that land INSIDE an emitted C string: NONE
any emitted C string containing 'DEPTH': [732, 998]
```

The two `DEPTH` hits are the pre-existing `YV12_FLAG_HIGHBITDEPTH` symbol, not
this commit's text (`DEPTH ASSUMPTION` / `DEPTH TRAP` appear 6 and 2 times, all
in shell or Python comments). **Emitted oracle bytes are unchanged.**

### Every substantive claim re-verified against today's tree — all still true

```
rung 1  PREFILT       HBD-check=False  fwrite(y_buffer + r*y_stride, 1, y_crop_width, f)
rung 6  POSTDEBLOCK   HBD-check=False  fwrite(y_buffer + r*y_stride, 1, y_width, f)
rung 7  PREFILT_WIDE  HBD-check=False  fwrite(y_buffer + r*y_stride, 1, y_width, f)
rung 15 POSTCDEF      HBD-check=False  fwrite(y_buffer + r*y_stride, 1, y_width, f)
rung 12 FINAL         HBD-check=True   fwrite(ec_s + r*ec_st[ec_pl], 2, ...)   <- depth-aware
```

The load-bearing justification also holds: the decoder-side pairs really do
narrow to u8 on purpose (`dump_stage` at `decode.rs:20048`, narrowing at `:20054`;
`dump_prefilter_wide` at `:20078`, narrowing at `:20090`).

### What I had to fix before landing

The four `DEPTH ASSUMPTION` blocks cited decoder line numbers that had **drifted**
(`decode.rs` is now 56341 lines; `:35263`, `:35336`, `:35356`, `:19734` and the
"same at `:53126`/`:53048`/`:53148`" duplicates no longer point at the narrowing
code). A comment whose entire job is "read this before you blame your decoder"
must not point at the wrong lines. Re-pointed all four (commit `3a621d80`):

| block | was | now |
|---|---|---|
| rung 1 PREFILT | `decode.rs:35263-35273` | `decode.rs:20048-20054` |
| rung 6 POSTDEBLOCK | `:35336`, dup `:53126` | `:20048-20054`, dup `:53489` |
| rung 7 PREFILT_WIDE | `:19734-19762`, dup `:53048` | `:20078-20090`, dup `:53489` |
| rung 15 POSTCDEF | `:35356`, dup `:53148` | `:20048-20054`, dup `:53489` |

`stream.rs:2129-2155` (rung 12's u16 LE writer) was already correct — verified,
left alone. Script-internal refs (`:146`, `:352`, `:416`, `:730`, `:934`) all
verified correct. Still comments-only after the correction.

### Dry run

The generator's first act is rewriting `~/.cache/aom-oracle/src`, so I sandboxed
it with `AOM_ORACLE_SRC` rather than touch the shared tree:

```
$ AOM_ORACLE_SRC=<sandbox> bash scripts/instrument-aom-oracle.sh
already instrumented (no-op)
no oracle source at <sandbox>/av1/decoder/decodemv.c
SCRIPT EXIT=1
```

The sandbox copy was **already instrumented** (9 `EC_INSTRUMENTED` markers — so is
`aom-oracle`, `aom-oracle2`, `aom-oracle3`), so the rungs took their no-op path
and the run stopped on a file I had not seeded. A true end-to-end patcher run
needs pristine libaom source, which does not exist in any local oracle tree and
would require a re-extract. **Not demonstrated** — the AST proof above is the
strongest available substitute. The shared oracle was verified untouched
(`decodeframe.c` mtime unchanged, 9 markers before and after, sandbox deleted).

---

## C — TRIAGE ONLY, nothing landed

| branch @ sha | charter said | what it actually is | verdict |
|---|---|---|---|
| `lane-av1444chr` @ `49e30db7` | `decode.rs +13` | **Report-only** (`lanes/av1444chr.report.md` +37, zero `crates/` change) — a bounded probe that exonerates the reach: `tu_reach` and `of_tu` agree on all 528 units, and the walk has no 4-wide block, so `below_left=false` is correct. Own commit says "the only line left to change is one whose owner this round did not identify", and names the settling measurement (block-layout census) that must be re-run on merged main. | **record only** — it is a dead end worth not re-chasing, and its own next step is explicitly unre-run. Nothing to land. |
| `lane-av1-ibcwrite` @ `25f5c729` | `decode.rs +33/-4` | **Real change, instrumentation only** — adds the env-gated `EC_AV1_PIXPROBE=x,y` rung (`pix_write` at 4 reconstruction-store sites). It *names* an open defect (8x8 leaf at (296,128) stores pred 170 + res 19 = 189 where the oracle's all-zero `TX_32X32` stores 170, so the partition tree diverges around skipped intrabc leaves) and fixes nothing. `EC_AV1_PIXPROBE` is **absent from main**, so not superseded. | **land after rebase** — pins a still-open defect with an inert-when-unset rung; but **not a clean cherry-pick**: `reconstruct_rect`/`reconstruct` still exist, while the two mc sites moved to `decode.rs:36489`/`:36502` in a since-restructured `reconstruct_mc_rect`, so the hunks need manual re-anchoring. |
| `lane-av1-c10` @ `a2806383` | `mc.rs +214` | **Report-only** (`lanes/av1c10.report.md`, +6/-5) — a *disclosure correction* to a report that had wrongly called the branch test-only. The `mc.rs +214` is real but lives in ancestor `295dc3fe`, not at this sha. That change is the `u8` cast moving onto the `let m` binding in `diffwtd_mask`, and it is genuinely byte-identical: `m` is clamped to `[0,64]` first, so both forms emit the same mask bytes. **Main does not have it** (main: `let m = (...).clamp(0, 64);` / branch: `... .clamp(0, 64) as u8;`). | **record only** at this sha (a report edit that duplicates whatever the report becomes). The *substance* — `295dc3fe`'s byte-identical cast move plus its compound 8/10-bit gate — is **land after rebase** if that gate is still wanted. |

Note on all three: none is an ancestor of `main` (`git merge-base --is-ancestor`
= no for all three), and each branch tip is exactly the sha in the charter.

---

## Build check

My edits are `.cargo/config.toml` and `scripts/instrument-aom-oracle.sh` — zero
Rust. But the config change gates *every* cargo invocation, so the crate used
for the A proof was checked under it:

```
$ cargo check --all-targets -p ec-av1-syntax
    Checking ec-core v0.1.0
    Checking ec-av1-syntax v0.1.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.30s
EXIT=0
```

`crates/ec-av1` was not checked: I changed nothing in it, and the box is loaded.

## What I deliberately did not do

- **Did not land any of C.** Triaged only, as scoped.
- **Did not run the generator against the shared oracle.** Its first act rewrites
  `~/.cache/aom-oracle/src`; I used `AOM_ORACLE_SRC` to sandbox and deleted the
  sandbox. Shared tree verified untouched.
- **Did not rebuild the oracle.** A true end-to-end patcher run needs pristine
  libaom source, which no local oracle tree has. Left undone rather than faked.
- **Did not add a loud abort / size assertion to the four rungs.** That changes
  emitted bytes and breaks the u8 byte-pairing by design; it needs both sides plus
  a re-pin of every existing gate. Correctly deferred, and the branch says so.
- **Did not run any full suite.** Scoped to one named test in one crate.
- **Did not touch the main checkout.** `git status --porcelain` there is empty
  after every batch.
- **Did not push.**
