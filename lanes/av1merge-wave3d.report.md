# Merge wave 3d — three branches in, two stale duplicates retired, two manifest fixes

Base `main` = `f33b9d41` (the wave-3c-b tip). Wave tip = **`97b4ee04`**. Nothing pushed.

`CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/merge3cb` (lane-private), `EC_NOMEMGUARD=1`,
`touch` on every source-scan module before a census run, `cargo check -p ec-av1
--all-targets` after **every** merge.

> **VERDICT: all three branches merged, zero conflicts, every claim re-measured on
> the merged tree, and the two checks Main named pass. The clipprobe lane shipped a
> manifest that was stale on its own source — fixed as its own commit, twice in this
> wave, because every branch that appends to `stream.rs` or `decode.rs` moves the
> manifest's `required-by` line numbers.**

---

## 1. Per-branch table

| # | branch | tip taken (live) | base | files | conflicts | check | merge |
|---|---------|------------------|------|-------|-----------|-------|-------|
| 1 | `lane-av1clipprobe` | `ca963bb5` | `f33b9d41` | 9 (new `library_fixture.rs`) | **none** | clean | `b2c9f8a4` |
| 2 | `lane-ibc444c-r4` | `fa70a68c` | `f33b9d41` | 3 | **none** | clean | `10183674` |
| 3 | `lane-av1rungs17` | `142bb61c` | `f33b9d41` | 4 | **none** | clean | `97b4ee04` |

All three tips re-read from the live tree; every one equals the assigned sha, and
all three branch off the **same** base `f33b9d41`, so this is three parallel
lanes off one point, not a chain.

`git diff main..<branch>` is misleading here and I did not use it: main has since
gained clipprobe, so that form shows clipprobe's own files as *deletions*
(11 files, −951). Every scope check below is against the branch's **true base**
`f33b9d41`, which is what the ticket's file lists were derived from.

### Retired, not merged

| branch | tip | why | evidence |
|--------|-----|-----|----------|
| `lane-ibc444c` | `6b78f492` | stale duplicate | superseded by `lane-ibc444c-r4`; its tree was the one missing 45 files / 6984 deletions |
| `lane-ibc444c-r3` | `17f03d90` | stale duplicate | same; `r4` is the rebuild onto `f33b9d41` |

---

## 2. `lane-av1clipprobe` — the clip gates stop skipping green

Scope confirmed as ticketed: `git diff f33b9d41..ca963bb5 --name-only` lists
`encode.rs`, `encoder.rs`, `lib.rs`, `library_fixture.rs` (new), `stream.rs`,
the report and 3 scripts. **No decoder file** — `decode.rs`, `mc.rs`,
`transform.rs`, `quant.rs`, `cdf_state.rs`, `tile.rs` all absent.

### The three behavioural cases, measured on the merged tree

**(1) root absent + require env set → RED, naming the path and both ways out:**

```
$ EC_FIXTURES=/tmp/absent-root EC_AV1_REQUIRE_FFMPEG=1 \
  cargo test -p ec-av1 --lib -- a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders
panicked at crates/ec-av1/src/library_fixture.rs:110:5:
a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders: the library clip is
absent at /tmp/absent-root/video/h264-1080p-23.976-8bit.mp4 -- this gate would prove
nothing. A linked worktree has no gitignored root `fixtures/`: run
scripts/link-fixtures.sh, or point EC_FIXTURES at a library root, or set
EC_REQUIRE_FIXTURES=1 / EC_AV1_REQUIRE_FFMPEG=1 only on a tree that really has the
library.
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 753 filtered out
```

This is the fix working, not a defect — Main's note (b) confirmed on the merged
tree, and it is the exact staging mistake I made in wave 3c-b, now loud.

**(2) root absent, no require env → exactly ONE `SKIP` line, then `ok`:**

```
$ EC_FIXTURES=/tmp/absent-root cargo test … --nocapture | grep -c SKIP
1
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 753 filtered out
```

**(3) root present, require envs set → still passes, so the new failure path is
not armed on a good tree:**

```
$ EC_AV1_REQUIRE_FFMPEG=1 EC_AV1_REQUIRE_AOMENC=1 cargo test … --nocapture
  1080p 2x1 tiles: 84488 bytes, sample-exact
  1080p 2x2 tiles: 84208 bytes, sample-exact
  1080p 4x2 tiles: 84685 bytes, sample-exact
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 753 filtered out; finished in 457.43s
```

The three byte counts are the lane's own. The new resolver precedence test
(`library_fixture::tests::the_resolver_follows_ec_fixtures_over_the_gitignored_root`)
is 1 passed / 0 failed.

### The manifest rule from Main's note (a)

`scripts/fixture-library.tsv` was **not** a conflict (the branch is strictly
ahead of its own base), so nothing to choose between. I took the lane's version
as the base, then ran both checks:

```
$ EC_REQUIRE_FIXTURES=1 scripts/verify-fixture-library.sh
verify-fixture-library: RED (library verdict: mode i, mode ii or drift)
  shape: 295 rows
$ diff <(grep -v '^#' scripts/fixture-library.tsv | cut -f1 | sort -u) \
       <(scripts/gen-fixture-library.sh /tmp/r.tsv >/dev/null; grep -v '^#' /tmp/r.tsv | cut -f1 | sort -u)
(exit 0 — unique path set identical, no lost coverage)
```

**Row-set check: empty, as required. The verifier: RED.** The two rows the
regeneration has and the committed file lacks are the two
`library_fixture::require(` sites this lane added in `stream.rs`
(`6298`, `45101`), plus the whole `stream.rs` `required_by` column is ~2 lines
stale. The lane's own report explains its 295-vs-305 as coarser attribution for
three paths — true, and the unique path set confirms no coverage was lost — but
it does not explain why the artifact does not regenerate from the source the
same lane committed. See Fix 1.

I also tested and **refuted** the host-shape theory the lane raised (it blamed
its worktree's symlinked `fixtures` versus the primary checkout's real dir):

```
$ ln -sfn "$PWD/fixtures" /tmp/liblink
$ EC_FIXTURES=/tmp/liblink scripts/gen-fixture-library.sh /tmp/r_sym.tsv   # 297 rows
$ scripts/gen-fixture-library.sh /tmp/r.tsv                               # 297 rows
$ diff /tmp/r_sym.tsv /tmp/r.tsv
(empty)
```

The generator is invariant under a symlinked versus real fixture root, so the
295-vs-297 gap is staleness in the committed artifact, not staging shape.

---

## 3. `lane-ibc444c-r4` — the 4:4:4 intra-BC rect chroma footprint

**Scope, against the true base:**

```
$ git diff f33b9d41..lane-ibc444c-r4 --stat
 crates/ec-av1/src/decode.rs | 106 +++++--
 crates/ec-av1/src/stream.rs  | 183 +++++++++++
 lanes/ibc444c.report.md     | 720 ++++++++++++++++++++++++++++++++++++++
 3 files changed, 989 insertions(+), 20 deletions(-)          # code = +271/-20
$ git diff f33b9d41..lane-ibc444c-r4 --name-only -- scripts/     # (empty)
$ git diff f33b9d41..lane-ibc444c-r4 -- crates/ec-av1/src/decode.rs \
      | grep -cE 'mu_chunk_order|read_intra_chroma_lossless'      # 0
```

Exactly the three files, `scripts/` untouched, the two named symbols absent —
the ticket's scope claim confirmed rather than taken on trust.

### Hazard 1 — a 3-way apply that resurrects removed lines

The hazard is real for a patch-rebuilt branch, so I checked the shape rather
than the clean status:

```
$ git diff f33b9d41..lane-ibc444c-r4 -- crates/ec-av1/src/stream.rs | grep -c '^-[^-]'
0
```

`stream.rs` is a **single contiguous tail append** (hunk `@@ -47988,4 +47988,187 @@`,
183 added, **0 removed**), so nothing can have been resurrected inside it — there
is no deletion to undo. After the merge I checked the same thing across the
whole wave for `stream.rs`: the only 10 deletions between `f33b9d41` and this tip
are clipprobe's own removal of the two `../../fixtures/...` literals it replaced
with the resolver, and clipprobe's 2 resolver call sites plus this lane's 1 new
gate are each present exactly once. A rebuild-from-main reconstruction was
therefore not needed — and I say so rather than implying I re-derived the file.

### Hazard 2 — a foreign file swallowed by the commit

```
$ git diff 7a7b9665..10183674 --stat
 crates/ec-av1/src/decode.rs | 106 +++++--
 crates/ec-av1/src/stream.rs | 183 +++++++++++
 lanes/ibc444c.report.md    | 720 +++++++++++++++++++++++++++++++++++++++
```

Three files, and each later merge in this wave was checked the same way
(`149b2ad8..97b4ee04` → 4 files: `decode.rs`, 2 scripts, the report).

### Red-before, reproduced on the merged tree

Reverting **only** the two arithmetic lines the lane names — `let (cpx, cpy) =
(px >> ss_x(fctx), py >> ss_y(fctx));` and `let (cw, ch) = (bw >> ss_x(fctx),
bh >> ss_y(fctx));` back to the 4:2:0 halving — nothing else:

```
panicked at crates/ec-av1/src/stream.rs:48161:13:
a_444_intrabc_rect_chroma_plane_block_is_the_block_footprint: plane Y diverges from the
oracle at sample 205264 of 307200 (x=464, y=320) -- the exact prefix this gate pins is
307200 and it only grows; the chroma tail is the OPEN entropy fork in
lanes/ibc444c.report.md r2
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 754 filtered out
```

**luma sample 205264** — the lane's figure, and the exact ceiling
`lanes/av1444rect.report.md` rounds 3-4 documented. Decoder restored from a
byte copy (`git diff` empty afterwards) and the gate re-run green:

```
a_444_intrabc_rect_chroma_plane_block_is_the_block_footprint: 14 rect intra-BC block(s)
sized at 4:4:4; 4:2:0 twin byte-exact; 4:4:4 luma byte-exact (307200/307200), U and V
byte-exact to sample 51360 -- the chroma tail is the OPEN entropy fork
(lanes/ibc444c.report.md r2), NOT an assertion
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 754 filtered out
```

The gate's honesty is the part I checked hardest, because the lane retracted an
earlier over-claim: it pins an exact **prefix** per plane and treats the chroma
tail as an open entropy fork, in the doc and in the panic message, with a counter
assert (`hits > 0`) so the gate cannot measure the un-fixed halving.

### The lane's scoped families, on the merged tree

| family | lane's MAIN+LANE claim | measured here |
|---|---|---|
| `444` | 37 / 0 | **37 passed / 0 failed** |
| `lossless` | 25 / 0 | **25 passed / 0 failed** |
| `intrabc_rect` | 7 / 0 | **7 passed / 0 failed** |
| `gate_coverage` | 13 / 0 | **13 passed / 0 failed** |
| `refusal_inventory` | 19 / 0 | **19 passed / 0 failed** |

**Left out, as instructed:** the hardcoded `let (cw, ch) = (bw / 2, bh / 2);`
that survives at `decode.rs:16758` (`decode_intrabc_owned_rect`) and
`decode.rs:19846` (`cfl_ac_q3_at`) is not folded in — neither is reached on this
witness, so folding it would be an unmeasured change owned by another lane.

---

## 4. `lane-av1rungs17` — oracle rungs 16/17 and the Rust twin

**Merged, not rebased.** Main's note said "rebase it onto current main"; I merged
it instead, because the branch is checked out in the lane's live worktree
(`~/.cache/wt/av1rungs17`) and rebasing its tip from the primary checkout would
rewrite another author's branch under a running worktree. The three-way merge
has the same base `f33b9d41`, so the code outcome is identical and the rewrite
does not happen. The resulting tree is `149b2ad8` + the branch tip.

Main's supersession accepted: the checker is expected at **14 ok**, not 10.

```
$ ./scripts/check-aom-oracle-rungs.sh
ok   legacy u8 row loops left in the derived file               0
ok   ec_dump_narrow_row call sites (4 rungs x 3 planes)         12
ok   ec_dump_finish call sites (one per narrowing rung)         4
ok   byte-count check wired into EC_AV1_PREFILT_DUMP            1
ok   byte-count check wired into EC_AV1_POSTDEBLOCK_DUMP        1
ok   byte-count check wired into EC_AV1_PREFILT_WIDE_DUMP       1
ok   byte-count check wired into EC_AV1_POSTCDEF_DUMP           1
ok   rung 12 still converts plane pointers                      1
ok   rung 12 not routed through the narrowing checker           0
ok   rung 16 EC_PREDOUT8 install sites (both 8-bit paths)       2
ok   rung 16 EC_PREDOUT8 sites carry mode= (EC_PREDND field order) 2
ok   rung 17 EC_PREDND install sites (hbd non-directional)      1
ok   rung 17 EC_PREDND site carries mode=                       1
ok   instrument-aom-oracle.sh derives the depth-correct, byte-checked rungs (base v3.13.3)
EXIT=0
```

**14 ok, exit 0** — the four new assertions are present, including the one that
closes the silent gap: rung 16 asserted installed at **both** 8-bit sites and
`mode=` on both, rung 17 installed once and carrying `mode=`. Before this lane,
`reconintra.c` was never restored to pristine by the checker, so a rung there was
never exercised and nothing was asserted.

### The hot-file integrity note, checked rather than assumed

```
$ git diff f33b9d41..lane-av1rungs17 -- scripts/instrument-aom-oracle.sh | grep -c '^-[^-]'
0
$ git diff f33b9d41..lane-av1rungs17 -- scripts/instrument-aom-oracle.sh | grep '^@@'
@@ -1074,3 +1074,156 @@ else:
$ git diff 149b2ad8..97b4ee04 -- scripts/instrument-aom-oracle.sh | grep -c '^-[^-]'
0
```

**One hunk, zero deletions, before and after the merge** — the HBD repair block,
`ec_dump_finish` and rungs 1-15 are provably intact, which is what makes a clean
auto-merge on this file meaningful rather than lucky.

### What main was missing, confirmed

```
$ grep -c 'OUR_PRED.*plane=' crates/ec-av1/src/decode.rs   # before this merge: 0
$ grep -c 'OUR_PRED.*plane=' crates/ec-av1/src/decode.rs   # after:            2
```

Both `OUR_PRED` sites now carry `plane=`, plus `RECON_PLANE`, `recon_plane()`
and `PlaneGuard` (14 added lines matching those symbols in the branch's diff).
Main had **zero** `OUR_PRED … plane=` lines, confirming that the whole commit —
not just the script half — was absent.

**`stash@{0}` is not what Main's note says it is.** Its message claims the
`EC_PREDOUT8` rung-16 edit, and it is clearly a false lead for this merge, but
`git stash show --stat stash@{0}` reports `scripts/instrument-aom-oracle.sh | 70
++++`, not `lane-ibc444c`'s `decode.rs`/`stream.rs`. Either way it is **not
needed**: `lane-av1rungs17` carries the rung-16/17 work committed
(`scripts/instrument-aom-oracle.sh +153`). I left the stash untouched.

I did not rebuild the oracle to reproduce the 118-`EC_PREDND`-line measurement:
that moves every lane's oracle binary and is the lane's own evidence, not an
integration claim. The checker output above is what I verified.

---

## 5. Name-set delta (`comm`, both directions)

`cargo test -p ec-av1 --all-targets -- --list`, private target dir.

```
$ comm -13 names_3cbtip.txt names_3d_final.txt      # only in POST
library_fixture::tests::the_resolver_follows_ec_fixtures_over_the_gitignored_root
stream::tests::a_444_intrabc_rect_chroma_plane_block_is_the_block_footprint

$ comm -23 names_3cbtip.txt names_3d_final.txt      # only in PRE
                                                   (empty)

753 -> 755  (+2, -0)
```

Per merge: clipprobe `+1`, ibc444c-r4 `+1`, **rungs17 `+0`** (oracle
instrumentation and the Rust twin; it adds no Rust gate, which is consistent
with its 755 → 755 step measured between the two merges). Nothing renamed,
nothing dropped.

---

## 6. Integration fixes

Both are the same wave-3c-b step recurring: **a generated artifact must be
regenerated when the source it indexes moves.** Each landed as its own commit,
never folded into a merge.

### Fix 1 — `7a7b9665`, the clipprobe manifest was stale on its own source

295 committed rows, 297 from this lane's own generator over the source it
committed. Two kinds of staleness, both real:

* the whole `crates/ec-av1/src/stream.rs` `required_by` column is ~2 lines off
  (committed `stream.rs:8156`, regenerated `8154`) — the artifact predates the
  lane's last `stream.rs` edit;
* three rows missing outright, the three `library_fixture::require(` sites the
  lane added: `stream.rs:6298`, `stream.rs:45101`, `library_fixture.rs:151`.

```
$ EC_REQUIRE_FIXTURES=1 scripts/verify-fixture-library.sh      # after
  shape: 297 rows / resolve: 0 missing, 0 empty
  invariant 1: positive control fired; no root-fixture pin path
  invariant 2: every committed pin is tracked
  invariant 3: negation present; no tracked pin shadowed
  pin gates: total=8 committed=21 uncommitted=0 ignored=0 assertless=0
  invariant 4: census self-test passed; every pin-reading gate resolves
  the clip gates resolve …/fixtures too (EC_FIXTURES first, else $ROOT/fixtures)
  verify-fixture-library: GREEN (297 rows)   exit 0
```

No coverage lost: unique path set **159 before and 159 after**, Main's row-set
check empty, regeneration idempotent.

### Fix 2 — `149b2ad8`, the manifest after the ibc444c gate

297 → 299 rows. This one is a pure tail append, so there is **no** line shift at
all; the whole diff is the two new rows, which are real new coverage and are that
gate's own witnesses:

```
+crates/ec-av1/fixtures/420_intrabc_rect4_witness.obu  crates/ec-av1/src/stream.rs:48071
+crates/ec-av1/fixtures/444_intrabc_rect4_witness.obu  crates/ec-av1/src/stream.rs:48069
```

plus the host-state unnamed-literal count 191 → 193, which the drift comparison
excludes by design. Unique path set 159 before and after; Main's row-set check
empty; idempotent; `GREEN (299 rows)`, exit 0.

*(I first wrote a commit message claiming "every other delta is a 1:1 line
attribution" for Fix 2. That was wrong — a tail append shifts nothing — and I
amended the message before moving on. A wrong claim in a committed message is
the same defect class this wave is about.)*

---

## 7. Everything checked after the LAST merge, not per lane

| check | result |
|---|---|
| `cargo check -p ec-av1 --all-targets` | clean, 0 warnings |
| conflict markers anywhere in `crates/` or `scripts/` | none |
| `gate_coverage` | 13 passed / 0 failed |
| `refusal_inventory` | 19 passed / 0 failed |
| `library_fixture` | 1 passed / 0 failed |
| `EC_REQUIRE_FIXTURES=1 scripts/verify-fixture-library.sh` | **GREEN (299 rows)**, exit 0 |
| `scripts/pin-gate-audit.py --self-test` | PASS (unchanged by this wave) |
| `scripts/check-aom-oracle-rungs.sh` | **14 ok**, exit 0 |
| Main's tsv row-set check | empty, twice |
| name-set `comm` both directions | +2 / −0 |

## 8. VPS full suite for wave 3d

Host `tCloud@51.195.223.40` (`vps-4733167b.vps.ovh.net`), unit
`wave3d-suite.service`, staging `~/gates/wave3d`, logs `~/gates/wave3d-suite.log`
and `~/gates/wave3d/suite.log`.

Staging used the recipe this batch validated, with both corrections:

1. `git archive HEAD` — never a worktree tar — scp, extract under `$HOME`.
2. `git init -q` **plus `git add -A`**: 1693 index entries. A bare `git init`
   leaves the index empty, `pin-gate-audit.py`'s `git ls-files --error-unmatch`
   then fails for every pin, and the census reads `committed=0 uncommitted=21` —
   a staging artefact that looks exactly like the defect invariant 4 exists to
   catch. This is the wave-3c-b correction.
3. `ln -sfn ~/gates/library/fixtures fixtures`: the clip gates now **fail
   loudly** without it. That is the clipprobe fix working, not a staging defect.
4. `systemd-run --user` with explicit `WorkingDirectory`, `CARGO_TARGET_DIR`,
   `TMPDIR`, `EC_NOMEMGUARD=1`, `EC_AV1_REQUIRE_AOMENC=1`,
   `EC_AV1_REQUIRE_FFMPEG=1`, `EC_REQUIRE_FIXTURES=1`, `EC_FIXTURES`,
   `EC_AV1_AOMENC`, `EC_AV1_AOMDEC`, and `PATH` **carrying
   `$HOME/.cargo/bin`** — `systemd-run` resets `PATH`, and a missing `cargo`
   there silently empties every measurement in the script.
5. Both gitignored `lanes/*.expected.txt` dumps present (24440 / 10129 B).

### Hash check, both directions, re-keyed

    rows local=496 host=496
    $ comm -3 local3d.ps hostC3d.ps
                                        (no output)
    comm_exit=0
    $ comm -3 local3d.ps local3d.ps | wc -l     # control: comm really compares
    0

The re-keying (`awk '{print $2"\\t"$1}' | LC_ALL=C sort`) is not optional: raw
`comm` on `sha256sum` output prints "not in sorted order" and then interleaves
byte-identical rows as if they differed — the exact false reading this check
exists to prevent. The self-comparison control is there so an empty `comm -3`
cannot be mistaken for a broken one.

### What the unit reported before the suite started

    === host=vps-4733167b.vps.ovh.net commit=08f8ffb3622f80056542b36cb3da34a1998bc38b
    === root fixtures -> /home/tCloud/gates/library/fixtures; library files=793
    === PREFLIGHT (EC_REQUIRE_FIXTURES=1, root linked)
    preflight_exit=0 :: verify-fixture-library: GREEN (299 rows)
      shape: 299 rows
      resolve: 0 missing, 0 empty
      invariant 1: positive control fired (the scanner can still see a literal)
      invariant 1: no root-fixture pin path
      invariant 2: every committed pin is tracked
      invariant 3: .gitignore negation present
      invariant 3: no tracked pin is shadowed by .gitignore
      pin gates: total=8 committed=21 uncommitted=0 ignored=0 assertless=0
      invariant 4: census self-test passed (a comment cannot steal a gate)
      invariant 4: every pin-reading gate resolves through committed copies
    === PIN-GATE-AUDIT SELF-TEST
    SELFTEST	PASS	a doc comment between two gates left both counted as crate_pin
    selftest_exit=0

So both regenerated manifests and the whole 3c-b chain are GREEN **from a
`git archive` checkout on a runner** — the state the 3c-b report could not reach.

**One check does NOT run on this host, and it is provisioning, not the repo:**

    $ ./scripts/check-aom-oracle-rungs.sh ; echo $?
    no oracle source at /home/tCloud/.cache/aom-oracle/src/av1/decoder/decodeframe.c
    1
    $ ls ~/.cache/aom-oracle/
    build

This host carries only `~/.cache/aom-oracle/build` (the `aomdec` and `aomenc`
binaries) and **no `src/`**, so the rung checker — which compares the derived
instrumented files against pristine oracle source — cannot run here. The
**14 ok / exit 0** figure in §4 and §7 is the local measurement. The suite needs
only the binaries, so it is unaffected; recorded so nobody reads the exit 1 as a
regression.

### Totals

*(appended when the unit exits.)*
