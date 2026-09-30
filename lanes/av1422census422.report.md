# lane-av1422census422 — the two non-vacuity prerequisites for the 4:2:2 lift

**Branch:** `lane/av1422census422`. **Base:** `main` = `19b16ffd`.
**Worktree:** `/home/tahinli/.cache/wt/av1422census422`.
**Changed:** `crates/ec-av1/src/decode.rs`, `crates/ec-av1/src/stream.rs`,
`crates/ec-av1/src/refusal_inventory.rs`, `crates/ec-av1/examples/decode_probe.rs`.
**Not touched, by instruction:** the sequence-header refusal, `film_grain.rs`,
`decode_rect4_16_intrabc`, the reserved group-tail chroma SKIP arm.

Both prerequisites named in `lanes/av1422liftrisk.report.md` §5 **R7** and **R8** are
closed, and each is measured rather than argued. No 4:2:2 unit was found missing from
any coefficient table, so there is no defect to report on that front (§2).

---

## 1. The two findings, in one line each

* **R7 — the strip enumeration silently dropped 4:2:2.** Fixed: the walk is now
  `{(0,0), (1,0), (1,1)}`, the count moved `8 -> 12`, and **the 4:2:2 half resolves
  entirely inside the five existing table arms, so no new arm was needed** (§2).
* **R8 — no 4:2:2 unit census.** Fixed: the per-plane coefficient-unit census is armed
  for 4:2:2 as well as 4:4:4, and its counters **move on every 4:2:2 cell measured**
  while the 4:2:0 and 4:4:4 numbers are **bit-identical before and after** (§3).

---

## 2. The strip enumeration (`refusal_inventory.rs`)

### 2.1 The change

`every_chroma_unit_a_64_axis_strip_can_present_has_a_coefficient_table` walked
`[(0,0), (1,1)]` and asserted `checked == 8`, justifying the missing format with
"every mixed format is refused at the sequence header". It now walks
`[(0,0), (1,0), (1,1)]` and asserts `checked == 12`.

### 2.2 The new count and the derivation — no unit was missing

The four 64-axis strips in the domain (ratio 2 or 4, both sides 16..64) crossed with
the three codable formats, chroma shape `bw >> ss_x, bh >> ss_y`, unit
`(chroma_w.min(32), chroma_h.min(32))`:

| strip | 4:4:4 (0,0) | **4:2:2 (1,0)** | 4:2:0 (1,1) |
|---|---|---|---|
| 64x16 | 32x16 | **32x16** | 32x8 |
| 64x32 | 32x32 | **32x32** | 32x16 |
| 32x64 | 16x32 | **16x32** | 16x32 |
| 16x64 | 8x32 | **8x32** | 8x32 |

`4 strips x 3 formats = 12` checks. The 32-per-axis cap (`decode.rs`, `let (uw, uh) =
(chroma_w.min(32), chroma_h.min(32));`) is why 4:2:2 needs nothing new: a 4:2:0 32x64
strip is a 16x32 chroma unit and its 4:2:2 counterpart is a **16x64** one, but the cap
resolves both to `(16, 32)`. Same for 16x64 -> `(8, 32)`.

**Every 4:2:2 unit is already in the table**: `{(32,32), (32,16), (16,32), (32,8),
(8,32)}`, asserted exactly by the pre-existing `arms == handled` pin. The
`hit == handled` non-vacuity assert is unchanged, which is the point — the widening
added formats, not arms. **No unit with no coefficient table was found; the
`decode_block_rect64` `_` arm stays dead for 4:2:2.**

### 2.3 Which half of the file was right, and why

The sibling `every_chroma_unit_decode_block_rect_can_present_has_a_coefficient_table`
(the 32-level strips, same file) already walked `[(0,0), (1,0), (1,1)]` and said why:
`ec-av1-syntax`'s `color_config` only ever READS `subsampling_y` when
`subsampling_x == 1` (`sequence.rs:481`), so `(0,1)` is uncodable and the reachable set
is exactly those three — and it walked `(1,0)` "so the proof does not depend on that
guard staying in place".

**The sibling was right; this half was wrong.** "The header refuses it" is a statement
about today's tree, not about the strip domain. A domain that shrinks silently when a
guard moves is not an enumeration — it is a snapshot of one guard's current value, and
the failure mode is the quiet one: no red, no output change, no counter, just a proof
that quietly stops being a proof. The sibling's phrasing is the generalizable rule and
is now mirrored here: walk what `color_config` can produce, and let the header guard be
irrelevant to the proof.

### 2.4 Non-vacuity: the widening is load-bearing, proved by mutation

The obvious mutation (delete a table arm) trips the pre-existing exact-table pin at
`refusal_inventory.rs:2132` before the walk, so it does not test the new code. The
mutation that does is re-narrowing the format list:

```
$ cargo test -p ec-av1 --lib every_chroma_unit_a_64_axis_strip
# with (1, 0) removed from the loop:
assertion `left == right` failed: the strip domain is not the four 64-axis strips
  x three codable chroma formats (4:4:4, 4:2:2, 4:2:0)
  left: 8
 right: 12
```

So a future re-narrowing **fails** instead of passing quietly — which is the exact
property the old test lacked. Both mutations were reverted and byte-verified
(`diff -q` against a pre-mutation copy: `RESTORED IDENTICAL`).

Restored, the test passes:

```
test refusal_inventory::tests::every_chroma_unit_a_64_axis_strip_can_present_has_a_coefficient_table ... ok
test refusal_inventory::tests::every_chroma_unit_decode_block_rect_can_present_has_a_coefficient_table ... ok
```

---

## 3. The 4:2:2 unit census (`decode.rs`)

### 3.1 The mechanism

The census counted one unit per `read_coeffs` / `read_coeffs_rect` call — i.e. per
transform unit that reads a `txb_skip` symbol — split per plane, and
`set_census_444` armed it **only** at `ss (0,0)`:

```rust
// before
CENSUS_444.with(|c| c.set(ss_x == 0 && ss_y == 0));
```

So at 4:2:2 every `census_444_units()` / `census_444_coded()` read zero **by
construction**, and a byte-exact 4:2:2 gate could assert those counters, be green, and
have proven nothing.

The arming is now the disjoint complement — every format with at least one unsubsampled
luma axis, 4:2:0 still off:

```rust
// after (set_census_nonsub)
CENSUS_NONSUB.with(|c| c.set(ss_x == 0 || ss_y == 0));
```

Three deliberate choices:

1. **Renamed** `CENSUS_444` / `census_444_units` / `census_444_coded` /
   `set_census_444` -> `CENSUS_NONSUB` / `census_nonsub_units` / `census_nonsub_coded`
   / `set_census_nonsub`. A census called `444` that fires on 4:2:2 is the same
   lying-name defect this lane exists to remove, and the file's own convention is
   already to name a counter for its domain.
2. **The two format pairs keep disjoint conditions**, so the 4:4:4 exact counts and the
   4:2:0 zero control cannot move because of the widening. Measured in §3.3.
3. **The accessors are now `pub`**, not `pub(crate)`, and `decode_probe` prints them
   (`census_units: luma=.. u=.. v=.. | coded: luma=.. u=.. v=.. | units_total=..`).
   Without this the counters are unreachable from the probe, which is the only
   instrument that can read a 4:2:2 stream at all while the header refusal stands.
   `census_unit_n()` (already `pub`) is printed alongside as the **unconditional**
   per-frame total, so one line says both how many units were counted per plane and
   whether the census was armed at all — a 4:2:0 cell's `luma=0 u=0 v=0` is then
   distinguishable from an empty walk without a second run.

Nothing else changed: the counting sites, the plane stamping, and the trace are
untouched. The counters are per-PLANE, not per-format, so a 4:2:2 cell's `[luma, u, v]`
split is directly comparable with a 4:4:4 cell's on the same walk shape.

### 3.2 Before / after, same cells, same probe, same bypass

Two builds, both from `main` + the probe print, differing in **one expression**:

* **BEFORE** — a throwaway detached worktree at `main`, print-only patch, arming
  reverted to `(ss_x == 0 && ss_y == 0)`.
* **AFTER** — this lane, arming `(ss_x == 0 || ss_y == 0)`.

Both carry the probe bypass, so both can decode 4:2:2 at all.

| cell | format | BEFORE units `[L,U,V]` | AFTER units `[L,U,V]` | AFTER coded `[L,U,V]` | `units_total` (both) |
|---|---|---|---|---|---|
| `s422_320x240` | 4:2:2 8b | `[0, 0, 0]` | **`[2338, 1328, 1328]`** | `[1020, 843, 949]` | 4994 |
| `s422_320x242_10b` | 4:2:2 10b | `[0, 0, 0]` | **`[2046, 1149, 1149]`** | `[860, 799, 825]` | 4344 |
| `s422_326x240` | 4:2:2 8b | `[0, 0, 0]` | **`[2798, 1433, 1433]`** | `[1200, 876, 1029]` | 5664 |
| `s422_416x250_10b` | 4:2:2 10b | `[0, 0, 0]` | **`[2927, 1426, 1426]`** | `[1068, 837, 996]` | 5779 |
| `g422_t5` (film grain) | 4:2:2 8b | `[0, 0, 0]` | **`[1443, 529, 529]`** | `[238, 268, 269]` | 2501 |

The BEFORE column is the vacuity proof in its strongest form: on a 4:2:2 cell the old
census read `[0, 0, 0]` while the same decode walked **4994 units**. It was not that
the walk was empty; it was that the census was not looking.

### 3.3 Controls — unchanged, with the numbers

| control | format | BEFORE | AFTER | verdict |
|---|---|---|---|---|
| `fixtures/av1_192x128_8bit_intra64_in_inter.obu` | 4:2:0 | `[0,0,0]` / coded `[0,0,0]`, total 405 | `[0,0,0]` / coded `[0,0,0]`, total 405 | identical |
| `sweep/s420_320x240.obu` | 4:2:0 | — | `[0,0,0]`, total 3514 | census stays off at 4:2:0 by construction, as the table row above shows for the before/after pair |
| `fixtures/ll444_minp8_inter.obu` | 4:4:4 | `[3764, 3764, 3764]`, coded `[975, 1793, 1859]`, total 11292 | **same** | identical |
| `fixtures/444_lossy_rect4_inter_witness.obu` | 4:4:4 | `[629, 372, 372]`, coded `[130, 253, 278]`, total 1373 | **same** | identical |

The two 4:4:4 rows are the numbers the existing gate
`a_pixel_exact_444_stream_walks_the_same_coefficient_units_the_oracle_does` pins against
libaom's own `EC_ECDUMP_IN` census (paired by `post_rng`). It passes unchanged on this
branch, together with its 4:2:0 zero control:

```
$ EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib \
    a_pixel_exact_444_stream_walks_the_same_coefficient_units
test stream::tests::a_pixel_exact_444_stream_walks_the_same_coefficient_units_the_oracle_does ... ok
```

Whole-sweep confirmation over all 54 cells (18 per format,
`/home/tahinli/.cache/census422b/sweep`, run through the ffmpeg comparator with the
widened census): **18/18 4:2:0 cells BYTE-EXACT with `[0,0,0]`; 18/18 4:4:4 cells
BYTE-EXACT with non-zero per-plane counts; 17/18 4:2:2 cells BYTE-EXACT with non-zero
per-plane counts, 1 DIVERGES** (`s422_384x240`, `U=1780 V=1871 Y=0` — a chroma-only
divergence, and the one cell the risk study's R4 already flags as owned elsewhere).

Incidental good news for the lift: the 7 divergences the ffmpegbase census recorded
(`s422_320x246`, `s422_322x240`, `s422_322x246`, `s422_352x242_10b`, `s422_384x240`,
`s422_416x242_10b`, `s422_416x250_10b`) have collapsed to **one** on today's tree.

### 3.4 Comparator liveness — 324/324 arms

`scripts/flipctl-422.py`, 9 cells x 36 arms (3 sample positions x 3 planes x 2 shown
frames x both directions), mapping re-derived from luma identity:

```
$ python3 flipctl-422.py cells_flip9.json
s422_320x240:     baseline [0, 0, 0]     17 decode / 16 shown, hidden [1], 1 mapping solution(s), 36 arms
s422_320x242_10b: baseline [0, 0, 0]     ... 36 arms
s422_326x240:     baseline [0, 0, 0]     ... 36 arms
s422_416x250_10b: baseline [0, 0, 0]     ... 36 arms
s422_384x240:     baseline [0, 1780, 1871] ... 36 arms
s420_320x240:     baseline [0, 0, 0]     ... 36 arms
s420_320x242_10b: baseline [0, 0, 0]     ... 36 arms
s444_320x240:     baseline [0, 0, 0]     ... 36 arms
s444_320x242_10b: baseline [0, 0, 0]     ... 36 arms

324 PASS, 0 FAIL of 324 flip arms
```

Every flipped sample moved exactly the owning plane by exactly 1 and no other plane at
all, on both the oracle arm and the ours arm. So the `BYTE-EXACT` verdicts in §3.3 are
produced by a comparator that reads both sides, and the 4:2:2 cells' census numbers come
from streams that are genuinely byte-exact.

### 3.5 The exact commands

```sh
# worktrees
git worktree add -b lane/av1422census422 /home/tahinli/.cache/wt/av1422census422 main
git worktree add --detach /home/tahinli/.cache/wt/census422-base main      # BEFORE build
# BEFORE build = main + probe print, arming reverted to (ss_x == 0 && ss_y == 0)
# AFTER  build = this lane

# probe bypass, patch-run-restore, in BOTH scratch trees, never committed:
#   stream.rs:1803  if seq.subsampling_x != seq.subsampling_y {
#              -> // TEMP-PROBE-BYPASS lane-av1422census422: patch-run-restore, never committed.
#                 if false && seq.subsampling_x != seq.subsampling_y {

CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/census422-base cargo build -p ec-av1 --example decode_probe
CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/census422      cargo build -p ec-av1 --example decode_probe

# census before/after, same cells, both binaries
BASE=/home/tahinli/.cache/tgt/census422-base/debug/examples/decode_probe
AFTER=/home/tahinli/.cache/tgt/census422/debug/examples/decode_probe
$BASE  /home/tahinli/.cache/census422b/sweep/s422_320x240.obu     | grep census_units
$AFTER /home/tahinli/.cache/census422b/sweep/s422_320x240.obu     | grep census_units

# ffmpeg comparison (copy of scripts/run422-ffmpeg.py with PROBE pointed at the
# AFTER binary; cmpff.py imported unchanged)
/home/tahinli/.cache/tgt/census422/debug/examples/decode_probe   # via EC_AV1_FINAL_DUMP
python3 run422.py cells_all.json out_all.json

# comparator liveness
python3 flipctl-422.py cells_flip9.json        # 324 PASS, 0 FAIL

# scoped gates on the restored, clean tree
cargo check -p ec-av1 --lib --tests --examples                       # clean, no warnings
EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -- \
  every_chroma_unit_a_64_axis_strip \
  a_pixel_exact_444_stream_walks_the_same_coefficient_units          # 2 passed
cargo test -p ec-av1 --lib every_chroma_unit                         # 2 passed
cargo test -p ec-av1 --lib census                                    # 12 passed, 3 ignored
```

---

## 4. Which arms the new census reaches, and which it cannot see

This is the part a future gate must not over-read.

**The census's structural reach is exactly: every transform unit that reads a `txb_skip`
symbol.** There are only two such sites in the decoder — `read_coeffs` and
`read_coeffs_rect` — and both call `census_unit` on every exit path (the `all_zero`
early return included). So the census counts every residual transform unit of every
plane, at every block shape, and its per-plane split is stamped by
`read_plane` / `read_inter_plane` / `read_inter_plane_rect` / `read_coeffs_rect`.

**Reached and counted on the four 4:2:2 cells measured** (arm counters from the same
runs; these are the arms a 4:2:2 gate can honestly claim the census witnesses):

| arm | `s422_320x240` | `s422_320x242_10b` | `s422_326x240` | `s422_416x250_10b` |
|---|---|---|---|---|
| `chroma422_square` | 64 | 57 | 58 | 68 |
| `chroma422_rect` | 304 | 282 | 300 | 326 |
| `chroma422_sub8` | 236 | 188 | 189 | 198 |
| `cfl_ac` 4:2:2 arm | 152 | 132 | 134 | 164 |
| `troy_chroma` dir-1:4 pairs | 9 | 16 | 7 | 18 |
| `inter_rect` 64x32 / 64x16 | 2 / 0 | 4 / 0 | 6 / 4 | 4 / 0 |
| `rect4_16_pair` intrabc | 0 | 0 | 0 | 1 |

Square, rect and sub-8 chroma 4:2:2 units, the 4:2:2 CfL AC arm, the 1:4 chroma
direction pairs, the 64-axis inter strips and the rect4_16 intrabc pair are all walked
by these cells, and all of them route their coefficients through the two readers the
census instruments — so their units are inside the measured counts. `chroma422_square +
chroma422_rect + chroma422_sub8 = 604` on `s422_320x240` against 2656 chroma units
(1328 U + 1328 V): the arm counters count *blocks* and the census counts *units*, so
they are not expected to agree, and the census is the finer of the two.

**Cannot be seen by this census, and therefore NOT witnessed by these cells:**

1. **`chroma422_chunk` — the 128-root CHUNKS chroma walk: `0` on all four cells.** Its
   coefficients do go through `read_inter_plane_rect` -> `read_coeffs_rect`, so the
   census *would* count them; no cell here reaches the arm. A 4:2:2 gate cannot claim
   128-root chunked chroma on this evidence.
2. **`chroma422_pair_wide`: `0` on all four cells.** Same reasoning, same conclusion.
3. **IntraBC / DC-only blocks.** They read no `txb_skip`, so they contribute **zero
   units** — by design (the census counts units, and an IntraBC block's DC values are
   not a transform unit). `ibc128: mu_chroma=0 intra128=0` on all four cells anyway,
   so no 4:2:2 IntraBC arm is witnessed here.
4. **Skipped blocks** (no residual) and palette blocks with no coded residual —
   invisible for the same reason. A cell whose walk is mostly skip would show a small
   non-zero count and still have exercised few coefficient paths; the count is a
   lower bound on *units*, never a statement that every arm ran.
5. **Anything the sequence header refuses.** With the guard standing, no committed test
   can produce these numbers; they are probe measurements. A committed 4:2:2 gate has
   to wait for the lift (see §6).

---

## 5. What a 4:2:2 gate can now assert that it could not before

**One line:** a byte-exact 4:2:2 gate can now assert that the per-plane coefficient-unit
census is non-zero on every plane (`census_nonsub_units() > 0` and
`census_nonsub_coded() > 0` for LUMA/U/V), which is a real non-vacuity requirement on
the stream it decoded — before this lane that assertion read `[0, 0, 0]` on every
4:2:2 cell by construction and was therefore satisfied by a decoder that had walked
nothing at all.

Concretely, R8's requirement — "the reachability counter for the arm each cell witnesses
must be `> 0`" — is now writable for 4:2:2, and once paired with a per-cell expected
count it also becomes a **walk-shape** assertion comparable against libaom's
`EC_ECDUMP_IN` census on a 4:2:2 stream, which is what the 4:4:4 gate already does with
its pinned `[3764, 3764, 3764]` / `[975, 1793, 1859]`.

---

## 6. Status, and what is deliberately not done

**Delivered:** the R7 enumeration widened to three formats with the count at 12 and the
4:2:2 membership proven (and a mutation proof that the widening bites); the R8 census
widened to 4:2:2 with measured deltas, unchanged 4:2:0/4:4:4 controls, a 324/324
comparator liveness control, and an honest arm-reach census of its own.

**Not done, with the unblock:**

* **No committed 4:2:2 gate.** The end-to-end gate needs the refusal lifted; a
  committed one is red today for a reason that has nothing to do with this lane. Hand
  the measured expectations in §3.2 over with the lift — they are the numbers a
  committed gate should pin, and §3.5 is the exact command set to re-derive them.
* **The 128-root CHUNKS and pair-wide 4:2:2 arms are still unwitnessed** (§4 items 1-2).
  They need a recipe that reaches them; the census is now able to measure whichever
  cell that is.

---

## 7. Incidents and observations, reported rather than buried

1. **A relative-path edit leaked into the PRIMARY checkout, and was caught and reverted.**
   My first two `edit` calls used the relative path `crates/ec-av1/src/...` while the
   shell CWD was the primary checkout, so they landed in
   `/home/tahinli/Documents/Code/Rust/edith_codecs` instead of the lane worktree — the
   tool reported success and the worktree looked untouched. Caught by
   `git status --porcelain` in the primary (2 modified files), the diff was confirmed to
   be entirely mine (57 insertions, all of them census text), saved to a patch file,
   and the primary was restored with `git checkout --`; `git status --porcelain` in the
   primary is **empty**. The work was then re-applied with **absolute paths**. Final
   state: primary clean, `grep -c 'TEMP-PROBE-BYPASS\|if false &&'` = 0 in every tree I
   touched. This is the same near-miss the liftrisk study reported from the other
   direction (§0.1) — the check that found it is `git status` in the primary, and it
   should be the first thing after the first edit, not the last.
2. **`EC_AV1_FINAL_DUMP` is written PRE-grain, so a film-grain stream can never compare
   byte-exact through it.** Measured on `g422_t5.obu` (4:2:2 + `--film-grain-test=5`):
   our dump differs from ffmpeg 8.1.3 in **100396 of 153600 bytes** (Y 76.7%, U 55.5%,
   V 52.6%, max delta 16) with the default decoder, with `-c:v libaom-av1`, and with
   `-c:v libdav1d` — and our own output is **byte-identical with and without
   `EC_AV1_NO_GRAIN=1`**. The cause is the dump's placement, not the decode: the dump
   writes `picture` at `stream.rs:2215` ("the frame exactly as it is about to be stored
   into the reference slots"), while `apply_grain` runs later at `stream.rs:1326` on
   the emitted output. ffmpeg's rawvideo is post-grain. **This is not a defect of my
   lane and I did not touch it** — but the grain lane's report §6 lists `g422_t5.obu` as
   byte-exact, and that is only reproducible through the in-memory `decode_stream`
   comparator it used, not through the dump path. Worth knowing before a lift writes a
   grain-cell gate on the dump path. My census numbers for `g422_t5` are therefore
   reported as walk-shape only, with no exactness claim attached.
3. **Probe bypass hygiene, verified.** Both scratch trees carried
   `// TEMP-PROBE-BYPASS lane-av1422census422` + `if false &&` at `stream.rs:1803` for
   the measurements only. Restored with `git checkout --`, then verified: the guard
   reads plain at line 1803, `grep -c 'TEMP-PROBE-BYPASS\|if false &&'` = 0, the
   restored probe **refuses** `s422_320x240` by name again, and the
   `census422-base` worktree is removed (`git worktree list` no longer lists it). The
   probe binary that the committed scripts hardcode
   (`/home/tahinli/.cache/tgt/av1422seed/...`) was never written by this lane; my own
   `tgt/census422` binary was rebuilt from the clean tree and re-verified to refuse.
4. **FYI, not mine:** `git worktree list` shows two *other* lanes' trees carrying a
   `TEMP-PROBE-BYPASS` in `crates/ec-av1/src/stream.rs` right now
   (`~/.cache/wt/av1422luma-probe`, `~/.cache/wt/av1422tailskip`). Each lane restores
   its own; flagging it only because a bypass left in a tree is one `git add -A` from
   admitting 4:2:2 silently.
