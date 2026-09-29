# lane-av1rungs17 — script rungs 16/17 (EC_PREDOUT8, EC_PREDND)

Base: `f33b9d41` (main, `lanes/av1merge-wave3c-b`). Branch: `lane-av1rungs17`.
Method: **reconstruction** onto the current script, not replay of the old one.

## 1. What was actually absent from main

The task brief said "main's `decode.rs` already prints `OUR_PRED … plane=`-style
lines, so the C-side may be ahead of the script-side". **That premise is wrong, and
the correction matters because it changes the size of the hole:**

```
$ grep -c 'OUR_PRED x={x} y={y} plane=' crates/ec-av1/src/decode.rs   # on main
0
$ git merge-base --is-ancestor 3d0001bc HEAD   # lane-av1cmpaudit's commit
NOT ancestor
```

Main has **zero** `plane=` on `OUR_PRED` (both sites are the lane's pre-change form
`OUR_PRED x={x} y={y} bw=…` / `… side={side} …`), and no `RECON_PLANE` thread-local
or `PlaneGuard` — main's `recon_planes` hits are an unrelated `ReconPlanes` Y/U/V
struct. So **the whole of commit `3d0001bc` is absent from main, not just its script
half**: the Rust twin field as well as the two rungs.

Genuinely absent, in full:

| item | where it lived | on main |
|---|---|---|
| `RECON_PLANE` / `recon_plane()` / `PlaneGuard`, `plane=` on both `OUR_PRED` sites | `3d0001bc` | absent |
| rung 16 — `EC_PREDOUT8` at both 8-bit prediction sites, with `mode=` | `3d0001bc` | absent (`grep -c 'EC_PREDOUT8\|EC_PREDND' scripts/instrument-aom-oracle.sh` → `0`) |
| rung 17 — `EC_PREDND`, the hbd non-directional site | staged, uncommitted in `av1cmpaudit` | absent |

Present on main and **deliberately not touched**: rungs 1–15, the HBD repair block
(`ec_dump_narrow_row`, `ec_dump_finish`, the halved-pointer fix) and
`scripts/check-aom-oracle-rungs.sh`.

### `stash@{0}` is a false lead — nothing of rung 16 is in it

The stash message claims *"EC_PREDOUT8 rung 16 in `scripts/instrument-aom-oracle.sh`"*.
Its actual content is two Rust files from an unrelated lane:

```
$ git stash show --stat stash@{0}
 crates/ec-av1/src/decode.rs | 106 ++++++++++++-----
 crates/ec-av1/src/stream.rs | 185 ++++++++++++++++++++++++++++
$ git ls-tree -r stash@{0}^3
100644 blob ec4a3e66…  lanes/ibc444c.report.md          # the lane it came from
```

No script file, no `RECON_PLANE`, no `plane=` in its `OUR_PRED`. The stash's byte-copy
(`/tmp/foreign-instrument-aom-oracle.sh.keep`) is already gone. **Nothing was lost
twice**: the rung-16 script change exists once, in `3d0001bc`, and the rung-17 change
exists once, staged in `av1cmpaudit`. Both are now reconstructed here.

## 2. Reconstruction

`~/.cache/wt/av1rungs17`, branched at `f33b9d41`. Nothing was cherry-picked or
`git apply`'d; the rung bodies were re-applied to the **current** files.

* `scripts/instrument-aom-oracle.sh` — rungs 16 and 17 appended **after** the repair
  block. The diff against main is a *single* hunk, `@@ -1076,0 +1077,153 @@`, i.e. pure
  append with zero deletions: the repair block cannot have been overwritten, because
  `git diff` shows not one of its lines changing.
* `crates/ec-av1/src/decode.rs` — the `RECON_PLANE` thread-local, `recon_plane()`,
  `PlaneGuard`, the set-and-restore in `exec_intra`, and `plane=` on both `OUR_PRED`
  sites. Re-derived against current main: `exec_intra` is still the single caller of
  `PlaneBuf::reconstruct{,_rect}` (`reconstruct_rect` @4439, `reconstruct` @4455, both
  inside `fn exec_intra` @4405), so the thread-local is still set for every call that
  reaches a `reconstruct`.
* `scripts/check-aom-oracle-rungs.sh` — extended, see §3.

## 3. The checker: 10 ok preserved, 4 added, all non-vacuous

The checker previously restored only `av1/decoder/decodeframe.c` to pristine, so a
rung living in `reconintra.c` was never exercised by it — it would have taken the
"already instrumented" branch and asserted nothing. It now also restores
`av1/common/reconintra.c` from the base ref before deriving.

```
$ bash scripts/check-aom-oracle-rungs.sh
ok   legacy u8 row loops left in the derived file               0
ok   ec_dump_narrow_row call sites (4 rungs x 3 planes)         12
ok   ec_dump_finish call sites (one per narrowing rung)         4
ok   byte-count check wired into EC_AV1_PREFILT_DUMP            1
ok   byte-count check wired into EC_AV1_POSTDEBLOCK_DUMP        1
ok   byte-count check wired into EC_AV1_PREFILT_WIDE_DUMP       1
ok   byte-count check wired into EC_AV1_POSTCDEF_DUMP            1
ok   rung 12 still converts plane pointers                      1
ok   rung 12 not routed through the narrowing checker           0
ok   rung 16 EC_PREDOUT8 install sites (both 8-bit paths)       2
ok   rung 16 EC_PREDOUT8 sites carry mode= (EC_PREDND field order) 2
ok   rung 17 EC_PREDND install sites (hbd non-directional)      1
ok   rung 17 EC_PREDND site carries mode=                       1
ok   instrument-aom-oracle.sh derives the depth-correct, byte-checked rungs (base v3.13.3)
exit 0
```

All ten pre-existing checks still print `ok` and the exit code is still 0. The four new
ones are the ones a rebuild must not lose: rung 16 at **both** 8-bit sites (a one-site
install looks fine on the directional arm and prints nothing for every non-directional
block), `mode=` surviving on both, rung 17 present, and rung 17's `mode=`.

A fifth, non-`check` assertion covers the claim in rung 16's own comment that it is
idempotent in **both** directions: the checker synthesises the no-`mode=` form the
oracle tree's git HEAD carries, re-derives, and requires the result to land back on the
identical bytes.

**Non-vacuity** (each mutation run against the real checker; every one reds on the
specific assertion, exit 1):

| mutation | result |
|---|---|
| rung 17 deleted from the script | `FAIL rung 17 EC_PREDND install sites … got 0, want 1` |
| rung 16 installs only the non-directional site | `FAIL rung 16 EC_PREDOUT8 install sites … got 1, want 2` |
| rung 16's directional site loses `mode=` | `FAIL rung 16 EC_PREDOUT8 sites carry mode= … got 1, want 2` |

## 4. Idempotence and correct derivation from pristine

Starting from a copy of the oracle tree with **both** files restored to `v3.13.3`
(`reconintra.c` has 0 rung sites):

```
$ bash scripts/instrument-aom-oracle.sh
EC_PREDOUT8 installed (both 8-bit paths, with mode=)
EC_PREDND instrumented (non-directional, high bitdepth)
```

* **Insert-only on pristine**: derived `reconintra.c` vs pristine is **41 added lines,
  0 removed**, every one of them inside the two rung bodies. No stray edits.
* **Second run is a no-op** — both files byte-unchanged:
  ```
  EC_PREDOUT8 already carries mode= (no-op)
  EC_PREDND already instrumented (no-op)
  reconintra.c: NO-OP      decodeframe.c: NO-OP
  ```
* **Fidelity to the live hand-patched tree**: the derived `reconintra.c` is
  byte-identical to `~/.cache/aom-oracle/src/av1/common/reconintra.c` on the rung
  bodies; the only differences are the two comment lines (mine say
  `EC_INSTRUMENTED_PREDOUT8 …`, the hand-patch says `EC_PREDOUT8 … (lane-d792)`) and
  three instruments that belong to *other* lanes and are in neither this script nor
  main's (see §7).

## 5. (a) insert-only on pristine / no-op when already instrumented

Covered in §4: 41 insertions, 0 deletions on pristine; second run no-op; the checker
now asserts both, plus the no-`mode=` upgrade round-trip.

## 6. (b) and (c) — the whole reason these rungs exist

One configure, two `aomdec` binaries, differing **only** in `av1/common/reconintra.c`
(the second is one recompile plus a relink, not a second libaom build). Built from
`$P/src` after `scripts/instrument-aom-oracle.sh` derived it; then `reconintra.c` was
replaced with the pristine `v3.13.3` file and rebuilt.

### (b) WITH rungs — 12-bit `fixtures/av112bit-key.obu`

```
$ EC_PRED=1 ./aomdec-with-rungs --rawvideo -o out-with.yuv av112bit-key.obu
EC_PREDND  lines: 118
EC_PREDOUT8 lines: 0
```

```
EC_PREDND mi_row=0 mi_col=0 plane=0 row_off=0 col_off=0 txw=16 txh=8 mode=0 n_top=0 n_left=0 sum=262144 row0=2048,2048,…
EC_PREDND mi_row=0 mi_col=0 plane=1 row_off=0 col_off=0 txw=8  txh=4 mode=0 n_top=0 n_left=0 sum=65536  row0=2048,…
EC_PREDND mi_row=0 mi_col=0 plane=2 row_off=0 col_off=0 txw=8  txh=4 mode=0 n_top=0 n_left=0 sum=65536  row0=2048,…
```

Note `EC_PREDOUT8` prints **0** on a 12-bit stream — that is the point of rung 17, not
a defect in it.

### (c) WITHOUT rungs — same build recipe, same stream

```
$ EC_PRED=1 ./aomdec-no-rungs --rawvideo -o out-without.yuv av112bit-key.obu
stderr lines total: 0
EC_PREDND lines: 0
```

Completely silent. Nothing is printed, so nothing can be diffed.

### Rung 16 on the 8-bit arm, `fixtures/ii-flake-1.obu`

```
WITH rungs:    EC_PREDOUT8 15   carrying mode= 15/15
               EC_PREDOUT8 mi_row=0 mi_col=0 plane=0 row_off=0 col_off=0 txw=32 txh=32 mode=0 sum=131072 row0=128,…
               EC_PREDOUT8 mi_row=0 mi_col=0 plane=1 row_off=0 col_off=0 txw=16 txh=16 mode=0 sum=32768  row0=128,…
WITHOUT rungs: EC_PREDOUT8 0
```

### The rungs do not change the decode

```
6b29cc506f88c8c89afd8b999f218947  out-with.yuv      (12-bit)
6b29cc506f88c8c89afd8b999f218947  out-without.yuv
482b5e04208ad5ff077e970c8cee6439  f8-with.yuv       (8-bit)
482b5e04208ad5ff077e970c8cee6439  f8-without.yuv
```

### The Rust twin now pairs with the oracle

`decode_probe` on the same 12-bit fixture: **177 `OUR_PRED` lines, 177 of them carrying
`plane=`**, values `plane=0 plane=1 plane=2`. The first three pair field-for-field with
the first three `EC_PREDND` lines above — same `plane`, same `16x8`/`8x4` extent, same
`mode=0`, same `sum=262144`/`65536`:

```
@…/decode.rs:4439:16 OUR_PRED x=0 y=0 plane=0 bw=16 bh=8 mode=0 ad=0 ft=0 sum=262144 row0=[2048, …]
@…/decode.rs:4439:16 OUR_PRED x=0 y=0 plane=1 bw=8  bh=4 mode=0 ad=0 ft=0 sum=65536  row0=[2048, …]
@…/decode.rs:4439:16 OUR_PRED x=0 y=0 plane=2 bw=8  bh=4 mode=0 ad=0 ft=0 sum=65536  row0=[2048, …]
```

Build is clean: `cargo check -p ec-av1 --lib` → `Finished`, no warnings, no errors.

## 7. Left standing, deliberately

`EC_DR` / `EC_DR_OUT` (4 sites) are in the live oracle tree and in **no** rung of the
script — main's or this one. Same class, different lane's instrument; out of scope here.
Worth a lane of its own.

The rung-gap census from `av1cmpaudit`'s staged report (preserved verbatim at
`~/.cache/tmp/r17proof/av1cmpaudit.report.staged.md`) puts the wider class at
**84 rung/env names in the live tree, 40 covered by the script, 48 dropped by a
rebuild before this round; rungs 16/17 close 2, 46 remain.** That number is
attributed, not re-derived here.

## 8. Disposition of `~/.cache/wt/av1cmpaudit`

Worktree left intact and untouched at `3d0001bc`; nothing was deleted.

| uncommitted item | disposition |
|---|---|
| staged `scripts/instrument-aom-oracle.sh` (rung 17) | **landed here** as rung 17 |
| staged `lanes/av1cmpaudit.report.md` (+87 lines) | **not landed** — superseded by this report. Its rung-gap census is carried forward in §7 with attribution; byte copy preserved at `~/.cache/tmp/r17proof/av1cmpaudit.report.staged.md` (327 lines) |
| `stash@{0}` in the primary checkout | **untouched.** Its message is wrong — it holds `decode.rs`/`stream.rs` from `ibc444c`, not rung 16. Not mine to land |
| committed `3d0001bc` | not merged as a commit (it predates the HBD repair block, so a replay would clobber it); its content is reconstructed here |
