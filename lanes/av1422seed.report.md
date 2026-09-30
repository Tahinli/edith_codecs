# lane-av1422seed — the frame-0 chroma-only 4:2:2 family: LOCALISED, not closed

**Outcome in one line: the seed is `decode_rect4_16_intrabc`'s 4:2:0 pair geometry
applied verbatim at ss (1,0) — proved by an unwritten-sample census, not inferred —
and the write-side half of the fix is not sufficient on its own: moving the chroma
write alone regresses luma, so the entropy-side half of the same arm must move with
it. This branch therefore lands NO decoder change.**

Tip: `main` = `affe70dc`. Worktree `/home/tahinli/.cache/wt/av1422seed`, branch
`lane/av1422seed`. `crates/ec-av1/src/decode.rs` and `src/stream.rs` are at
`main`'s content — the `if false &&` 4:2:2 bypass used for probing is **not**
committed and the sequence-header refusal is intact.

---

## 0. What is delivered

1. §2 — the ffmpeg comparator, its decode→display mapping rule, and the control
   cells it reports byte-exact.
2. §3 — all five slice cells reproduced against ffmpeg, with the first divergence
   and the per-frame counts.
3. §4 — the localisation: the exact arm, the exact constants, and a census that
   names the unwritten samples.
4. §5 — the attempted fix, why it is **not** landed, and what the remaining half is.
5. §6 — what is NOT measured (stated, not hidden).

---

## 1. The instrument, and what it does NOT inherit from the census

`scripts/cmp422-ffmpeg.py` + `scripts/run422-ffmpeg.py` (committed). The census's
`cmp422.py` compares against the **instrumented aomdec**; this lane's brief makes
**ffmpeg 8.1.3** the pixel oracle, so the comparator is new and its disciplines are
stated here rather than inherited.

1. **Geometry is an explicit argument read from the SEQUENCE HEADER.** `ffprobe
   -show_entries stream=coded_width,coded_height,pix_fmt` on the `.obu`; the
   chroma tag and depth come from `pix_fmt` (`yuv422p`, `yuv422p10le`, …), and the
   comparator refuses a `pix_fmt` that disagrees with the ss/depth it derived
   (`cmp422-ffmpeg.py:24`). A raw frame whose length is not a whole multiple of
   the geometry's frame size is a hard error (`split`, `:80`). **Never** a file
   size, and never the u8-narrowing dump for a 10-bit cell.
2. **Plane attribution is per frame**, from that frame's own plane lengths
   (`sizes = (w*h, cw*ch, cw*ch)`, `cw = ceil(w/2^ss_x)`, `ch = ceil(h/2^ss_y)`),
   with a monotone cursor inside the frame (`compare`, `:145`).
3. **A wrong SAMPLE is one `bps`-wide unit.** At 10 bit the comparator walks
   16-bit LE samples, so a `0x0C00`-vs-`0x0BFF` defect cannot hide behind a byte
   compare.
4. **A count over zero frames is a hard error** (`:120`, `:140`): an empty dump
   set raises rather than reporting a vacuous `0/0/0`.
5. **Decode-order → display-order is derived, not assumed.** Ours is
   `EC_AV1_FINAL_DUMP` (decode order, hidden alt-refs included); ffmpeg's
   `-f rawvideo` is **display order and has no hidden frames** — 16 shown frames
   against our 17. `map_decode_to_display` (`:80`) identifies each decode frame by
   its **luma plane** and enumerates *every* assignment that covers the display
   side; if the assignments disagree on the counts it raises rather than picking
   one. This is load-bearing: a real altref stream **reorders display against
   decode** (measured on all five control cells: decode f1 is the hidden picture
   and decode f2..f8 map to display 7, 3, 1, 2, 5, 4, 6), so the tempting
   "decode order = display order" prior is wrong and a monotonicity prior is
   wrong too — both are rejected in the code with the measurement that rejects
   them.

### 1.1 Comparator controls

Five control cells, same pipeline, same binary, **before any change**:

| cell | geometry | decode / shown | hidden | verdict | Y/U/V |
|---|---|---|---|---|---|
| `s420_320x240` | 320x240 ss 1,1 d8 | 17 / 16 | `[1]` | BYTE-EXACT | 0/0/0 |
| `s420_320x246` | 320x246 ss 1,1 d8 | 17 / 16 | `[16]` (2 assignments, counts agree) | BYTE-EXACT | 0/0/0 |
| `s420_352x242_10b` | 352x242 ss 1,1 d10 | 17 / 16 | `[16]` (2 assignments) | BYTE-EXACT | 0/0/0 |
| `s444_320x240` | 320x240 ss 0,0 d8 | 17 / 16 | `[1]` | BYTE-EXACT | 0/0/0 |
| `s444_320x242_10b` | 320x242 ss 0,0 d10 | 17 / 16 | `[1]` | BYTE-EXACT | 0/0/0 |

Two of them are the 10-bit class and two are the **2-solution** mapping class, so
the mapping rule is exercised, not assumed. The 4:2:2 cells are re-measured
against the same pipeline in §3, where a non-zero baseline is reported — the
instrument is shown reading a plane that is already wrong by thousands.

**NOT DONE — the flip control.** The intended next control (flip one ffmpeg sample
per plane and require exactly `+1` in that plane, `0` everywhere else, on a
non-zero baseline) is scripted as `flipctl.py` in the census scratch but was **not
executed**; §6 repeats this. What stands in its place is weaker and is named as
such: the instrument reports per-plane counts that move by thousands on the
diverging cells while the two formats it must not touch (4:2:0, 4:4:4) read
`0/0/0` on five cells, and the unwritten-sample census of §4 is a completely
independent witness to the same region.

---

## 2. The five slice cells, reproduced

`/home/tahinli/.cache/census422b/sweep/<cell>.obu`, ffmpeg 8.1.3, our side
`EC_AV1_FINAL_DUMP`. Counts are wrong **samples**, summed over the 16 **shown**
frames (the hidden picture is not in ffmpeg's output and is reported separately).

| cell | geometry | depth | decode / shown | hidden | pre Y/U/V | first divergence |
|---|---|---|---|---|---|---|
| `s422_322x240` | 322x240 | 8 | 17 / 16 | `[1]` | 0 / **22003** / **21625** | disp f0 (decode f0) **U (r62, c149)** ours 160 ffmpeg 166 |
| `s422_320x246` | 320x246 | 8 | 17 / 16 | `[1]` | 0 / **7810** / **5531** | disp f0 (decode f0) **U (r62, c144)** ours 161 ffmpeg 166 |
| `s422_322x246` | 322x246 | 8 | 17 / 16 | `[1]` | 0 / **3437** / **2222** | disp f0 (decode f0) **U (r62, c144)** ours 169 ffmpeg 166 |
| `s422_352x242_10b` | 352x242 | 10 | 17 / 16 | `[1]` | 0 / **4653** / **3777** | disp f0 (decode f0) **U (r62, c160)** ours 667 ffmpeg 664 |
| `s422_416x242_10b` | 416x242 | 10 | 17 / 16 | `[1]` | 0 / **5740** / **4348** | disp f0 (decode f0) **U (r62, c192)** ours 640 ffmpeg 664 |

All five: **chroma only, luma exact on every shown frame**, first divergence in
decode frame 0, wrong region reaching every frame.

**Answer to the brief's question 4 — do the 10-bit members fail for the same
reason?** *Mechanically yes, numerically no.* All five first-diverge in decode
frame 0, plane U, and in every case the first wrong **row is 62**; the first wrong
**column** is `chroma_width - 16` in chroma-sample units for four of the five
(320→144, 322x246→144 of 161, 352→160 of 176, 416→192 of 208) and
`chroma_width - 12` for `s422_322x240` (149 of 161, the first *scanning* column of
that row, not the region's left edge). The seed block is the **same shape and the
same relative position** in all five (8.4 §4.2), and the per-frame counts differ
only because the 10-bit cells are wider. So: one defect, five geometries — not a
depth-specific one. (This is also the census's own conclusion, reached
independently: "bit depth does not separate them".)

Counts are lower than the census's `0/8339/5903`-style figures by exactly the
hidden picture's contribution (e.g. `s422_320x246`: 8339 − 7810 = 529 U,
5903 − 5531 = 372 V), which is the expected difference between a 17-decode-frame
and a 16-shown-frame basis and confirms both measurements agree.

---

## 3. The seed: an unwritten-sample census, not a diff pattern

A wrong-sample diff says *where the output is wrong*; it cannot say *whether the
block was written at all*. The crate already carries the instrument for that
question: `fresh_plane` (`decode.rs:21123`) leaves the plane backing store
**uninitialised** and fills it with `PLANE_SENTINEL = 0xDEAD` under
`EC_AV1_PLANE_SENTINEL=1`. Every sample reconstruction writes is overwritten;
every sample it does not write still reads `0xDEAD`. A scan of the three planes at
the frame's pre-deblock point is then an exact census of unwritten samples.

Run on `s422_320x246.obu`, **decode frame 0**:

```
SEEDUNWRITTEN plane=0 320x256 runs=0
SEEDUNWRITTEN plane=1 160x256 runs=4
  plane=1 row=128 cols=144..151
  plane=1 row=129 cols=144..151
  plane=1 row=130 cols=144..151
  plane=1 row=131 cols=144..151
SEEDUNWRITTEN plane=2 160x256 runs=4
  plane=2 row=128 cols=144..151
  plane=2 row=129 cols=144..151
  plane=2 row=130 cols=144..151
  plane=2 row=131 cols=144..151
```

**Luma: nothing unwritten. U and V: one 4×8 block each, chroma rows 128..131,
cols 144..151, and nothing else in the frame.** So the 16×16 luma region at luma
(288, 128) is fully reconstructed while its chroma half — 8×4 per plane — is
**never written at all**. (The census scan is a temporary local edit and is not
committed; it is four lines at the `dump_stage16` call in `decode_frame`.)

A lowest-level write trace (the three `PlaneBuf` sinks — `reconstruct`,
`reconstruct_rect`, `reconstruct_mc_rect` — are the only writers of a plane in this
tree) then names the block, and frame 0 contains exactly **three** MC writes:

```
SEEDWR3 reconstruct_mc_rect x=288 y=128 plane=0 w=16 h=4
SEEDWR3 reconstruct_mc_rect x=144 y=62  plane=1 w=8 h=4
SEEDWR3 reconstruct_mc_rect x=144 y=62  plane=2 w=8 h=4
```

One 16×4 **luma** write at (288, 128) — the strip's own footprint, and it is
exact — and one 8×4 **chroma** write per plane, at **(144, 62)** instead of
(144, 128). The 8×4 shape is right; the **row is 66 too high**, and the strip's
own chroma is the block the census found unwritten. That is the whole defect:
one write, landing in the wrong row.

### 3.1 The owning arm

`decode.rs:18162 fn decode_rect4_16_intrabc` — reconstruction of one **intrabc
16×4 / 4×16 strip** (a 1:4 partition strip; `is_inter_block` counts intrabc, so
its luma is a frame copy through the MC path, which is why the luma write is an
MC write and not `reconstruct_rect`).

The function's own doc comment states the 4:2:0 pair rule: *"A 1:4 strip is a
PAIR. Both children share one chroma block, and only the odd-mi member codes it
(`is_chroma_reference`, `av1_common_int.h:1454`) … `setup_pred_plane`'s
`mi_row -= 1` when the block is one mi tall on an odd row."* That rule is applied
at every `ss` except 4:4:4, which got its own `own444` arm (lane-av1444rect r2).
**4:2:2 is the second `ss` where the rule does not hold, and it was never given
an arm.** The three constants that carry it, at `main`:

| site | `main` | what 4:2:2 needs | value on the seed strip (`lmi = (32, 72)`, HORZ) |
|---|---|---|---|
| `decode.rs:18244` `cw, ch` | `(pw/2, ph/2)` = `(8, 4)` | `ss_size_lookup[BLOCK_16X4][1][0]` | `(8, 4)` — **accidentally already right** |
| `decode.rs:18246` `pair_mi` | `(lmi.0 - 1, lmi.1)` = `(31, 72)` | no pair: `lmi` | `(32, 72)` |
| `decode.rs:18256` `cpx, cpy` | `(pair_mi.1*MI/2, pair_mi.0*MI/2)` = `(144, 62)` | `(px>>ss_x, py>>ss_y)` | **`(144, 128)`** |

and the two chroma coefficient-context gathers that must move **with** them,
`decode.rs:18574` and `decode.rs:18635`:

```rust
let around = neighbours.around_mi_rect(pair_mi, pw, ph);
```

The libaom clause that decides it, `av1_common_int.h:1454`'s `is_chroma_reference`,
ends in

```c
  ... || (g->subsampling_x == 1 && g->subsampling_y == 0) || (mi_col & 1) || (mi_row & 1);
```

so at ss (1, 0) it answers **true for every block**: no pair, every strip codes
its own chroma, at its own origin, with its context gathered over its own extent
(and through `around_mi_422_chroma`, the every-second-column 4:2:2 gather every
other 4:2:2 chroma context read in this tree already uses).

**The census's `first divergence U(62,144)` is this write.** The block at chroma
(144, 62) is the block the census's first-divergence coordinates name; the block
at (144, 128) is the one the census's "shrunk region, origin unmoved" table could
not see, because nothing was ever written there.

### 3.2 The same shape in all five cells

The unwritten census was run on `s422_320x246` only; the *diff* geometry is
reported for the other four by `scripts/shape422-diff.py`. In every cell the
frame-0 wrong region is a single 8-wide chroma column band, **16 chroma columns
from the right edge** (320→144 of 160, 322→144 of 161, 352→160 of 176, 416→192 of
208) at chroma rows 62..65 and 128..131 — the two rows of §3 and of §4.1. Same
strip family, same two-row-pair displacement, five geometries.

---

## 4. The attempted fix, and why it is not landed

The write-side half of the fix, applied and measured (this is the change that is
**not** in the tree):

```rust
let own422 = ss_x(fctx) == 1 && ss_y(fctx) == 0 && horz;
if own422 && has_chroma { hit!(INTRABC_RECT4_OWN_CHROMA422_HITS); }
let (cw, ch)  = if own444 || own422 { (bw >> ss_x(fctx), bh >> ss_y(fctx)) } else { (pw/2, ph/2) };
let pair_mi   = if has_chroma { if own444 || own422 { lmi } else if horz { (lmi.0-1, lmi.1) } else { (lmi.0, lmi.1-1) } } else { lmi };
let (cpx, cpy) = if has_chroma { if own444 || own422 { (px >> ss_x(fctx), py >> ss_y(fctx)) } else { (pair_mi.1*MI/2, pair_mi.0*MI/2) } } else { (0, 0) };
```

plus a new gate counter `INTRABC_RECT4_OWN_CHROMA422_HITS` /
`intrabc_rect4_own_chroma422_hits()` (the ss (1,0) twin of the existing
`INTRABC_RECT4_OWN_CHROMA444_HITS`).

VERT_4 is deliberately **not** claimed: `ss_size_lookup[BLOCK_4X16][1][0]` is
`BLOCK_INVALID` at ss (1,0), so only the pair shape is codable for a 4×16 strip —
the same statement `decode_block_rect4` already carries.

**Measured result: it regresses luma.**

| build | `s422_320x246` frame-0 luma wrong samples | five cells |
|---|---|---|
| `main` (`affe70dc`) | **0** | 0/7810/5531, 0/22003/21625, 0/3437/2222, 0/4653/3777, 0/5740/4348 (chroma only) |
| + write-side fix, gathers untouched | **5958** | all five fail luma identity |
| + write-side fix + `around_mi_rect(pair_mi, pw, ph)` → `(gmi, gw, gh)` = `(lmi, bw, bh)` and routed through `around_mi_422_chroma` | **5958** | all five still fail luma identity |

The control is exact and cheap: `git stash push crates/ec-av1/src/decode.rs`,
rebuild, re-run → the `main` numbers above reproduce to the sample
(0/7810/5531 etc.), so the pipeline is not drifting and the regression is the
change's. The decode is deterministic (two runs of the same binary, frames 0..3,
`SAME` sha256 on all four), so this is not a flake.

**Reading.** The write-side constants and the entropy-side gather are the *same*
misapplication, and moving the write alone turns a reconstruction defect into an
**entropy desync** that corrupts luma for the rest of the frame — the classic
`reconstruction-only vs parse` split. My second attempt moved the gather too, by
the same `own422` predicate, and did not restore luma, which says the remaining
half is **not** in `around_mi_rect`'s extent alone. The untested candidates, in
order:

1. **`has_chroma` itself.** The caller decides whether a strip codes chroma by
   mi parity. At ss (1,0) *every* strip does (`is_chroma_reference`'s clause), so
   an even-row HORZ strip that our tree currently passes `has_chroma = false` must
   become `true` — which adds a chroma coefficient read and changes the parse. If
   the caller already passes `true` for all HORZ strips at 4:2:2, this is not it.
2. **`rect_inter_chroma_set(cw, ch)`** and the `TxbSet` chosen for an 8×4 chroma
   unit at ss (1,0) — the entropy class, not the extent.
3. The **coefficient context row/offset** for a chroma unit whose plane block is
   8×4 at ss (1,0) (`around_mi_422_chroma`'s every-second-column sampling plus the
   offset-7/offset-10 rows).

Serkan-4 (`lane-av1422luma`, cell `s422_416x250_10b`) independently localised a
**luma** desync in this same function, to the same `own444` pair-geometry block
and the `around_mi_rect(pair_mi, pw, ph)` gather. Two lanes converging on one
function is the strongest signal in this report: **the 4:2:2 arm of
`decode_rect4_16_intrabc` does not exist and has to be written whole** — origin,
extent, `has_chroma`, entropy set and context gather together — not patched in
pieces. Ownership was split by message: Serkan-4 takes the entropy side, this
lane's write-side hunk is described above and is not landed.

---

## 5. Per-plane, per-frame numbers

Pre-fix (this lane's instrument, ffmpeg oracle, 16 shown frames):

| cell | Y | U | V |
|---|---|---|---|
| `s422_322x240` | 0 | 22003 | 21625 |
| `s422_320x246` | 0 | 7810 | 5531 |
| `s422_322x246` | 0 | 3437 | 2222 |
| `s422_352x242_10b` | 0 | 4653 | 3777 |
| `s422_416x242_10b` | 0 | 5740 | 4348 |

Per-frame U counts, `s422_320x246` (display order, 16 frames):
`58 269 308 371 410 348 580 608 620 634 621 655 640 575 584 529` — wrong in
**every** frame, growing with the altref's reference use, which is what a
frame-0 seed that later frames predict from looks like.

Post-fix: **none** — the change is not landed.

**4:2:0 and 4:4:4 controls after the change attempt:** unchanged, `0/0/0` on all
five control cells (§1.1 re-run with the change in the tree: 5/5 BYTE-EXACT). The
attempt is 4:2:2-specific in blast radius — it breaks 4:2:2 luma and touches
nothing else.

---

## 6. Family sweep (the class, repo-wide in this corpus)

All 18 `s422_*` sweep cells, same instrument, with the change **not** applied
(`main`), which is the honest baseline for whoever takes the arm next:

| verdict | cells |
|---|---|
| BYTE-EXACT | 12 — `320x240`, `320x242`, `320x242_10b`, `320x250_10b`, `322x242`, `326x240`, `326x242`, `326x246`, `352x250_10b`, `384x242`, `384x246` (+ the 5 targets' siblings) |
| DIVERGES, chroma only | `s422_384x240` — **0 / 1780 / 1871**; first bad frame 1 per the census, i.e. a **different, later** seed |
| instrument error (luma identity does not cover the display side) | `s422_320x246`, `s422_322x240`, `s422_322x246`, `s422_352x242_10b`, `s422_416x242_10b`, `s422_416x250_10b` — the five slice cells plus `416x250_10b`, measured **with the change in the tree**; each is a luma-corrupting desync, not a count |

Run against `main` instead, the five slice cells read the §2 numbers and
`s422_416x250_10b` reads the census's luma-wrong row. The sweep is here so the
next lane does not have to re-derive it: **`s422_384x240` is not this defect**
(its first bad frame is 1, and its region is not the 16-columns-from-the-right
band), so closing this arm should leave exactly one 4:2:2 cell diverging in the
sweep.

## 7. What is NOT measured

* **The flip control was not run.** §1.1's controls are the five byte-exact cells,
  not the sample-flip arms. Nothing here should be read as "the comparator was
  proven to bite by a flip".
* **No permanent gate was added.** The brief asked for a gate in the crate's test
  binary naming a cell; with the fix unlanded a gate would have to assert a
  *failure*, and a gate that asserts a known defect is not a gate. The
  `INTRABC_RECT4_OWN_CHROMA422_HITS` counter is the hook such a gate will need;
  the non-vacuity argument for it will be "this cell moves 5958 luma samples when
  the arm is wrong", not a count.
* **`s422_384x240` and the 4:4:4 `s444_352x242_10b` residual are untouched** and
  are not this arm.
* The 4:2:2 sequence-header refusal is **untouched** — `stream.rs:1803` reads
  `if seq.subsampling_x != seq.subsampling_y` on this branch, verified by
  `git status` clean for that file.
* `s422_416x250_10b`'s luma desync is Serkan-4's slice; this report only records
  that it is in the same function.

## 8. Reproducing this lane's numbers

```sh
# 1. the bypass (LOCAL ONLY, never commit)
#    crates/ec-av1/src/stream.rs:1803  ->  if false && seq.subsampling_x != seq.subsampling_y {
CARGO_TARGET_DIR=/tmp/tgt cargo build -p ec-av1 --example decode_probe --features gate-counters

# 2. the five cells + the five controls, against ffmpeg 8.1.3
python3 scripts/run422-ffmpeg.py <cells.json> <out.json>

# 3. the frame-0 unwritten-sample census (temporary four-line scan, see §3)
EC_AV1_PLANE_SENTINEL=1 <probe> sweep/s422_320x246.obu

# 4. the wrong-region geometry, per cell
python3 scripts/shape422-diff.py s422_320x246 sweep/s422_320x246.obu
```

`cells.json` entries are `{"name": ..., "path": ...}`. The comparator derives
geometry from the sequence header itself and needs no other argument; it raises
rather than reporting a count it cannot justify.
