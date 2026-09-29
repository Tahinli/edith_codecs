# lane-av1f44diverge — the two 4:4:4 "divergences with witnesses but no diagnosis"

**Base / branch** `57834ee2` (main) → `lane-av1f44diverge`, worktree
`~/.cache/wt/av1f44diverge`. **No decode-path change is left on this branch** —
the report is the deliverable. Every number below was measured in this
worktree; nothing is quoted from a sibling report without being re-run.

---

## 0. Verdict first

**Both recorded divergences are already fixed on current main, and the class is
an ENTROPY fork, not a reconstruction one.**

| witness | recorded on `4155c7c7` | measured on `57834ee2` (this lane) |
|---|---|---|
| (a) 4:4:4 "superres" 256x128 | DIVERGENT f2, first `Y(192,0)` ours 92 / oracle 106 | **0 / 0 / 0 mismatching samples on all three planes, all 4 decode-order frames** |
| (b) 4:4:4 odd 130x122 | DIVERGENT f3, first `Y(128,16)` ours 134 / oracle 170 | **0 / 0 / 0, all 4 frames** |
| (b-control) 4:4:4 odd 66x66 | EXACT | **0 / 0 / 0** (and exact on the pre-fix tree too, §5) |

The cause is `f92776ba` (lane-av1444rect, "H1"), **already merged into main**
(`git merge-base --is-ancestor f92776ba HEAD` → yes), plus its follow-up
`e10a3b25`. I reproduced the old numbers by reverting exactly that one `match`
arm (§3), which makes the red-before mine rather than inherited, and classified
the fork with the `EC_SYMR` ladder (§4).

---

## 1. Witnesses, rebuilt on current main

Recipe — the chroma-format sweep's §6.2 recipe, re-run here, `W`/`H` per arm
(`rate=25` is load-bearing: `testsrc2`'s pattern is time-parameterised):

```text
ffmpeg -f lavfi -i "testsrc2=size=<W>x<H>:rate=25" -frames:v 4 \
       -pix_fmt yuv444p -strict -1 -f yuv4mpegpipe - |
aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=20 --cpu-used=2 \
       --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 \
       --limit=4 --obu -o - -
```

| cell | bytes | sha256 | committed pin |
|---|---|---|---|
| 130x122 | 8945 | `c87ac65b77579afd2938669b0cbe3ec44c3abf775c0223cacb9d14b8c2eee27c` | `444_lossy_rect4_odd_130x122.obu` (byte-identical) |
| 256x128 | 15239 | `06621606c11ae805f784a1c9736db6301a925a4fa3154e2fc38db1df7ad9b826` | `444_lossy_rect4_wide_256x128.obu` (byte-identical) |
| 66x66 | 5938 | `973eddf4aa6b0bf18ba8bdf7c16ef8173fe51438ae5c42f5c5863e93444181ca` | unpinned (live-encode gate) |

The encodes took **1.4 s total on this box**, so the idle VPS
(`tCloud@178.105.165.182`) was not needed.

**Witness (a) is not a superres cell — measured here, not inherited.** Encoding
256x128 with and without `--superres-mode=1` gives **byte-identical** OBU
(15239 B, `06621606…` both), and `ffprobe` reports `width=256 coded_width=256`
(an active superres would show a smaller `coded_width`). The cell is a plain
4:4:4 lossy cq-20 stream; the crate already names its gate for the geometry
(`a_444_lossy_256x128_stream_decodes_pixel_exact`) rather than for a flag.

## 2. Per-plane measurement method (independent of the gates)

The gates already compare byte-for-byte, but a gate result cannot produce
*per-plane* counts, so I measured with the tools directly:

- oracle: `EC_AV1_FINAL_DUMP=<dir>/aom aomdec --codec=av1 -o out.y4m <cell>.obu`
  → `aom.f0..f3`, decode order, u8, `Y|U|V` planar, 3·W·H bytes
  (256x128 → 98304, 130x122 → 47580, 66x66 → 13068; all four frames equal size).
- ours: `cargo run -p ec-av1 --example dump_yuv -- <cell>.obu <dir>/ours`
  → `ours.f0..f3.yuv`, same layout, depth parsed from the sequence header
  (8-bit). No altrefs in these recipes (0 hidden frames, `kf-max-dist=100`),
  so display index == decode index.
- compare: a 30-line python per-plane walk (kept in `~/.cache/f44div/cmp.py`,
  outside the repo) that counts per plane per frame and names the first
  divergent sample as `plane(x,y) ours A oracle B`.

## 3. Red-before, measured by me (the pre-H1 `around_c` arm)

`crates/ec-av1/src/decode.rs:42457` `let around_c = match strip_chroma {` —
the green arm is

```rust
Some(_) if ss_x(fctx) == 0 => { hit!(RECT4_INTER_OWN_CHROMA444_HITS); around }
```

Red arm = the pre-`f92776ba` state of that one arm: the `ss_x == 0` arm deleted
and the `pw/ph` choice restored to `if ss_x == 1 && ss_y == 0 { (16,4) } else
{ (16,8) }`. Nothing else touched; `git status --porcelain` on the main
checkout was empty after every batch.

| cell | frames 0-1 | f2 | f3 | total |
|---|---|---|---|---|
| 256x128 (red) | 0 | **Y 9085/32768 first `Y(192,0)` ours 92 oracle 106; U 7985 first `U(211,0)`; V 8226 first `V(211,0)`** | Y 19818, U 15398, V 15892 | **76 404** |
| 130x122 (red) | 0, 0 | 0 | **Y 2975/15860 first `Y(128,16)` ours 134 oracle 170; U 4491 first `U(129,15)`; V 4055 first `V(102,32)`** | **11 521** |
| 66x66 (red) | 0 | 0 | 0 | **0** |

Both recorded first-wrong samples and both recorded counts reproduce **to the
byte** (11521 and the `Y(192,0)`/`Y(128,16)` firsts). Green tree, same cells,
same method: **0 / 0 / 0 everywhere** (§2), and the 4:4:4 gate family is
17/17 green.

## 4. Classification: ENTROPY fork (both witnesses)

`EC_SYMR=1` on both sides, one line per symbol read in decode order, aligned on
`(value, range, n, s, post_rng)` with the CDF-row convention reconciled
(`32768 - ours_cdf0 == oracle_cdf0`; verified by that predicate holding on every
paired read except the fork itself). Oracle lines come from the instrumented
`aom_dsp/bitreader.h` rung, which carries `site=<file>:<line>`.

| cell (red tree) | reads ours / oracle | first divergent read | mi (4-px MI) | what differs |
|---|---|---|---|---|
| 130x122 | 43 700 / 43 700 | **41 166** (frame 3) | **(12, 23)** | `pre=(32638, 36058)` **identical both sides**; `n=2 s=0`; `cdf0` ours 32310 (→ 458) vs oracle 2357 |
| 256x128 | 63 275 / 74 192 | **55 192** (frame 2) | **(11, 36)** | `pre=(38795, 44892)` **identical both sides**; `n=2 s=0`; `cdf0` ours 26963 (→ 5805) vs oracle 3789 |
| 66x66 | 28 294 / 28 294 | **none** | — | all 28 294 reads agree, CDF convention included |

Green tree, same ladder: **43 700 / 74 192 / 28 294 reads, zero divergence** on
all three cells.

**This is a CDF-row selection error, not a bit-position error.** At the fork the
two decoders hold *the same* `(value, range)` and differ only in which row of the
CDF they selected — i.e. the coder state is identical and the *context* is not.
The `pre` third field (our msac bit position) sits at a constant +15 to the
oracle's, the documented offset, and the CDF convention reconciles on every read
after the fork (+1: `32768-9909 == 22859` = the oracle's `cdf0`). So the fork is
one symbol read choosing a different neighbour-context row, and everything after
it is downstream damage.

**The fork site is a 1:4 inter strip, located from the block trace.**
`EC_MC_TRACE=1` prints every inter block's origin and write size
(`EC_IB px=.. py=.. w=.. h=..`):

- 130x122, mi(12,23) → pixel **(92, 48)**: `EC_IB px=92 py=48 side=16 w=4 h=16`
  — a **VERT_4 (4x16)** strip.
- 256x128, mi(11,36) → pixel **(144, 44)**: `EC_IB px=144 py=44 side=16 w=16 h=4`
  — a **HORZ_4 (16x4)** strip.

Both are the shapes whose pre-H1 gather used the 4:2:0 pair extent
(`(8,16)` / `(16,8)`) where 4:4:4 `is_chroma_reference` means no pair exists. The
damage profile agrees: on 256x128 f2 the chroma planes are wrong by more
samples than luma (U 7985, V 8226 vs Y 9085 of a 32768-sample plane, and the first
wrong U/V samples are *inside* the strip's own rows), which is what a wrong
*chroma* context predicts.

**Class, in one sentence:** at `ss (0,0)` a 1:4 inter strip's chroma
coefficient context was gathered over the 4:2:0 pair extent instead of the
strip's own extent, so the U unit's `get_txb_ctx` saw a left/above neighbour
libaom does not have and the arithmetic coder forked at the strip's first
2-symbol chroma read.

## 5. The 66x66 control: exact in BOTH arms, and why that is not a geometry fact

Quoted as asked: at the same settings (`cq 20`, `cpu-used 2`, yuv444p, same
oracle) 66x66 is **0 / 0 / 0 on all 4 frames, on the pre-fix tree as well as on
main**; entropy is bit-identical there in both arms (28 294 reads, no fork).

The interesting part is that the site **is** reached at 66x66 — the live gate
counts 13 own-extent 1:4 gathers there — so its immunity is not "the route never
ran". Two extra measurements narrow what it is:

| counterfactual stream (same encode flags) | 1:4 inter strips | pre-H1 verdict |
|---|---|---|
| 66x66 from `testsrc2` (the sweep's control) | 24 of 48 inter-block reads are 1:4 strips (px 0/8/12/16/32/44, both orientations) | **exact, 0/0/0** |
| 66x66 from `geq=random` (13482 B, sha `a0718b30…`) | 8 of 18 | **exact, 0/0/0** |
| 130x122 from `geq=random` (53042 B, sha `871882bd…`) | 8 of 100 | **exact, 0/0/0** |

So 66x66's immunity is **neither content-dependent** (a noise-sourced 66x66 is
also exact) **nor "no 1:4 strips"** (it has them, 8–13 by counter). What separates
the cells is whether the pre-H1 pair-extent gather returns a *different* second
neighbour row/column at some site — on 66x66 it never does.

**What I could not separate:** *why* the extra neighbour coincides at every 66x66
site (most likely the two 4-px cells the pair extent adds are covered by the
same neighbouring block, so the gather duplicates one descriptor). The
discriminating measurement is a per-site dump of both gathers' results
(`pair_mi`, `has_chroma`, orientation, and the two returned context vectors) at
the 13 66x66 sites — I did not add that instrumentation, because the class and
the fix are already settled and the question no longer gates anything.

## 6. Gates and state

Both witness cells and the control are **already gated on main** and were
re-quoted here (17 tests, all green, `cargo test -p ec-av1 --lib -- 444_lossy
rect4 444_rect --test-threads=1`):

| gate | cell | this lane's run |
|---|---|---|
| `a_444_lossy_odd_66x66_stream_decodes_pixel_exact` | 66x66 | ok — 13 own-extent gathers |
| `a_444_lossy_odd_130x122_stream_decodes_pixel_exact` | 130x122 | ok — 12 gathers |
| `a_444_lossy_256x128_stream_decodes_pixel_exact` | 256x128 | ok — 24 gathers |
| `a_444_lossy_rect4_strip_stream_decodes_pixel_exact_at_odd_and_wide_geometries` | both pinned cells | ok |
| `a_444_lossy_rect4_inter_stream_decodes_pixel_exact` | the 128x96 H1 pin | ok |

Nothing to add: each gate already (a) compares every decode-order frame against
`aomdec` with the length asserted first, (b) asserts the 4:4:4 full-resolution
chroma shape, and (c) asserts `rect4_inter_own_chroma444_hits() > 0`, so none of
them can pass vacuously.

**The non-vacuity proof, measured here.** With the same one-arm revert as §3 all
three gates go red — `test result: FAILED. 0 passed; 3 failed` — but each fails
on assert **(c)**, not on pixels: `no 4:4:4 1:4 inter strip read its chroma
context over its own extent`. The `hit!(RECT4_INTER_OWN_CHROMA444_HITS)` sits
inside the deleted arm, so the counter reads 0 and the assert fires *before* the
oracle compare. That is worth knowing when reading a red here: the pixel red is
the direct `dump_yuv`-vs-`aomdec` measurement of §3, not the gate's message.
Green again after `git checkout -- crates/ec-av1/src/decode.rs`: 3 passed.

**Recommendation for the debt list:** drop (a) and (b) as open items; they are
closed by `f92776ba`/`e10a3b25` with gates. The only residue worth carrying is
§5's last paragraph, and even that is a measurement request, not a defect.

## 7. Files

- Branch diff vs `57834ee2`: **this report only** (`lanes/av1f44diverge.report.md`).
- Throwaway instruments, outside the repo: `~/.cache/f44div/cmp.py` (per-plane
  compare), `~/.cache/f44div/symr.py` (EC_SYMR aligner),
  `~/.cache/f44div/{cmp,symr,mctrace}-*.log`, encodes in `~/.cache/f44div/*.obu`.
- Build: `CARGO_TARGET_DIR=/home/tahinli/.cache/tgt-f44div` (private).
