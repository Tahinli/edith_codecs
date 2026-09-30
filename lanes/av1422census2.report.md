# lane-av1422census2 — the 4:2:2 census re-derived at current main, with the DELTA against `cc9f2668`

**Outcome in one line: the census was right about its own tree and the later lane was
right about the current one — measured on both, the skipped-intrabc-DV-copy fix
(`b8eed69f`, landed in `1686dc8a`) closed 8 of the 17 diverging 4:2:2 cells outright,
shrank 2 more without closing them, and left 7 diverging (6 in 4:2:2, 1 in 4:4:4).**
Current numbers at `main` = `ed7c99bc`: **44 of 51 4:2:2 cells byte-exact, 7 diverging,
51 of 51 refusing by name on the shipped tree.**

Tip: `main` = `ed7c99bc`. Worktree `/home/tahinli/.cache/wt/av1422census2`, branch
`lane-av1422census2`. Two trees were used, both with the same instrument and the same
cells:

* **post** = `ed7c99bc` (current main) — worktree `av1422census2`,
  `CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1422census2`
* **pre** = `cc9f2668` (the census's own tip) — worktree `av1422census2-pre`,
  branch `lane-av1422census2-pre`, `CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1422census2-pre`

The pre tree exists so the delta is measured, not inferred from the old report. **The
only source edit ever applied is the `if false &&` 4:2:2 sequence-header bypass; both
trees carry no source diff now** (`stream.rs:1803` reads
`if seq.subsampling_x != seq.subsampling_y {` in both). No fix, no push, no merge, no
rustfmt. **This report recommends no product decision**; §7 states what is not measured.

---

## 0. The contradiction, settled

The census (`lanes/av1422census.report.md`, measured at `cc9f2668`) lists
`Q_odd320x242`, `O_odd322x242`, `AB_inter_warp_odd` and `S_odd326x242_10b` as DIVERGING.
`lanes/av1422ctintrabc.report.md` lists the same four as BYTE-EXACT on the later tree.
**Neither was wrong; they are two trees.** Measured here on both, with one instrument and
one cell list:

| cell | pre (`cc9f2668`) Y/U/V | post (`ed7c99bc`) Y/U/V |
|---|---|---|
| `Q_odd320x242` | 0 / **9603 / 14456** | 0 / 0 / 0 |
| `O_odd322x242` | 0 / **985 / 722** | 0 / 0 / 0 |
| `AB_inter_warp_odd` | 0 / **3705 / 4366** | 0 / 0 / 0 |
| `S_odd326x242_10b` | 0 / **301 / 751** | 0 / 0 / 0 |

And the census's own numbers are **reproduced exactly** on my pre tree — all 17 of its
diverging cells return its published counts to the sample (§4). So the census's tables
are a faithful record of `cc9f2668`; they are not wrong, they are **superseded**.

---

## 1. The comparator, and its liveness proof (re-proven, not inherited)

Same instrument as the census (`cmp422.py`, `run.py`, `liveness.py`, copied to
`/home/tahinli/.cache/census422b/`), same four disciplines:

1. **Geometry is an explicit argument**, parsed from aomdec's OWN y4m header
   (`W320 H242 … C422` / `C422p10 XYSCSS=422P10`), never from a file size. A frame whose
   byte length disagrees is a hard error.
2. **Plane attribution is per frame**, from that frame's own plane lengths, with a
   monotone cursor.
3. **A wrong SAMPLE is one `bps`-wide unit**; at 10-bit the comparator walks 16-bit
   samples, so the top bits cannot hide.
4. **A count over zero frames is a hard error**, naming the vacuous 0/0/0.

### 1.1 The flip control — 14 cells x 3 planes = 42 arms, 42 PASS, 0 FAIL

One oracle sample flipped per plane, per cell, in a named decode frame; the count must
move by **exactly +1 in that plane** with every other plane and every other frame
unchanged. Coverage spans the classes that have each broken a census before:

| cell | geometry | frame | baseline Y/U/V | Y arm | U arm | V arm |
|---|---|---|---|---|---|---|
| `W_intrabc` | 320x240 ss=10 d8 | 0 | 0/0/0 | `51`→`50` +1 Y | `5a`→`5b` +1 U | `f0`→`f1` +1 V |
| `Q_odd320x242` | 320x242 ss=10 d8 | 0 | 0/0/0 | +1 Y | +1 U | +1 V |
| `Q_odd320x242` | 320x242 ss=10 d8 | 7 | 0/0/0 | +1 Y | +1 U | +1 V |
| `Y_intrabc_10b` | 320x240 ss=10 d10 | 0 | 0/0/0 | `4401`→`4501` | `6801`→`6901` | `c003`→`c103` |
| `S_odd326x242_10b` | 326x242 ss=10 d10 | 0 | 0/0/0 | +1 Y | +1 U | +1 V |
| `AB_inter_warp_odd` | 322x242 ss=10 d8 | 42 (last of 43) | 0/0/0 | +1 Y | +1 U | +1 V |
| `ll420_a` | 320x240 ss=11 d8 | 0 | 0/0/0 | `96`→`97` | `8f`→`8e` | `9b`→`9a` |
| `ll444_a` | 320x240 ss=00 d8 | 0 | 0/0/0 | +1 Y | +1 U | +1 V |
| `s422_320x242` | 320x242 ss=10 d8 | 0 | 0/0/0 | +1 Y | +1 U | +1 V |
| `s422_322x240` | 322x240 ss=10 d8 | 5 | **0/22969/22534** | +1 Y | +1 U | +1 V |
| `s422_320x246` | 320x246 ss=10 d8 | 11 | **0/8339/5903** | +1 Y | +1 U | +1 V |
| `s422_384x240` | 384x240 ss=10 d8 | 14 | **0/1982/2081** | +1 Y | +1 U | +1 V |
| `s422_416x250_10b` | 416x250 ss=10 d10 | 5 | **103250/64191/62834** | +1 Y | +1 U | +1 V |
| `s444_352x242_10b` | 352x242 ss=00 d10 | 4 | **0/1166/1413** | +1 Y | +1 U | +1 V |

The load-bearing rows are the **non-zero baselines**: the flip must move a plane that is
ALREADY wrong by thousands of samples, by exactly one more, and must not disturb the
other planes. A comparator that attributed a plane to the wrong frame, dropped chroma,
or compared our samples with themselves fails at least one of these. The exact-cell rows
are the ones that can be a false green, and they are included deliberately.

The flip is bit 0 of the sample's **low** byte; at 10/12 bit the sample sits in a 16-bit
LE container and `2**bitdepth < 2**16`, so +1 is exact by construction rather than hoped
for. Each arm is restored before the next, so the planes are independent.

---

## 2. The cell list, re-derived

Same method as the census §2, re-run from scratch (`discover.py`):

1. `find /home/tahinli/.cache/cells -name '*.obu'` → **242 probe cells**.
2. All 104 files under `crates/ec-av1/fixtures` (105 tracked; 1 is not `.obu`).
3. Every candidate decoded through `aomdec --codec=av1 -o /tmp/o.y4m` and classified from
   the y4m header — the oracle's own statement of the shape.
4. Deduplicated by **content sha256**, not by name.

| step | count |
|---|---|
| 4:2:2 candidates seen | 44 |
| after sha256 dedup | **32 distinct 4:2:2 corpus cells** (identical to the census) |
| of those, committed (`git ls-files crates/ec-av1/fixtures`) | **9** |
| probe-only | 23 |
| non-4:2:2 controls kept (the census's 8 probe-cache lossless controls) | 8 |
| fresh reproducer `R422_320x242` (from `lane-av1422ctintrabc`) | 1 |
| **fresh sweep encodes at the census's recipe** | **54** (36 at 8-bit, 18 at 10-bit) |
| **total cells measured** | **95** (51 4:2:2, 22 4:2:0, 22 4:4:4) |

The 9 committed 4:2:2 fixtures are `W_intrabc`, `X_intrabc_tiled`, `Y_intrabc_10b`,
`422_allskip_2f`, `422_intrabc_sb128_strip`, `422_intrabc_sb128_strip_notxsearch`,
`422_residual_compound_warp_16f`, `422_residual_compound_warp_nolr_16f`,
`422_sb128_3f` — the same nine the census names.

### 2.1 The fresh sweep is the census's sweep, byte for byte

The 54 sweep cells were re-encoded from scratch with `mksweep.sh` at the census's recipe
and compared to the census's own `.obu` files by sha256:

```
for f in sweep/*.obu; do ... done   →   DIFF lines: 0
```

**All 54 fresh encodes are byte-identical to the census's**, so the sweep table below is
a re-measurement of exactly the same bitstreams, not a new corpus that happens to look
similar. `aomenc` at this recipe is deterministic.

---

## 3. DELTA — per cell, `cc9f2668` → `ed7c99bc`

Count columns are wrong **samples** per plane, over DECODE-order frames (hidden alt-refs
included). `frames` is the decode-frame count.

### 3.1 The 10 cells the fix moved

| cell | class | geometry | depth | frames | pre verdict | pre Y/U/V | post verdict | post Y/U/V |
|---|---|---|---|---|---|---|---|---|
| `Q_odd320x242` | 422 corpus | 320x242 | 8 | 17 | DIVERGES | 0/9603/14456 | **BYTE-EXACT** | 0/0/0 |
| `O_odd322x242` | 422 corpus | 322x242 | 8 | 17 | DIVERGES | 0/985/722 | **BYTE-EXACT** | 0/0/0 |
| `AB_inter_warp_odd` | 422 corpus | 322x242 | 8 | 43 | DIVERGES | 0/3705/4366 | **BYTE-EXACT** | 0/0/0 |
| `S_odd326x242_10b` | 422 corpus | 326x242 | 10 | 17 | DIVERGES | 0/301/751 | **BYTE-EXACT** | 0/0/0 |
| `R422_320x242` | 422 fresh reproducer | 320x242 | 8 | 17 | DIVERGES | 0/6337/8398 | **BYTE-EXACT** | 0/0/0 |
| `s422_320x242` | 422 sweep | 320x242 | 8 | 17 | DIVERGES | 0/6337/8398 | **BYTE-EXACT** | 0/0/0 |
| `s422_326x240` | 422 sweep | 326x240 | 8 | 17 | DIVERGES | 0/734/0 | **BYTE-EXACT** | 0/0/0 |
| `s422_326x246` | 422 sweep | 326x246 | 8 | 17 | DIVERGES | 0/24078/25112 | **BYTE-EXACT** | 0/0/0 |
| `s422_320x242_10b` | 422 sweep | 320x242 | 10 | 17 | DIVERGES | 0/3915/5612 | **BYTE-EXACT** | 0/0/0 |
| `s422_352x250_10b` | 422 sweep | 352x250 | 10 | 17 | DIVERGES | 0/3876/1786 | **BYTE-EXACT** | 0/0/0 |
| `s422_320x246` | 422 sweep | 320x246 | 8 | 17 | DIVERGES | 0/38348/59491 | DIVERGES **(shrunk)** | **0/8339/5903** |
| `s422_322x246` | 422 sweep | 322x246 | 8 | 17 | DIVERGES | 0/24964/25438 | DIVERGES **(shrunk)** | **0/3603/2324** |

Ten cells went from diverging to byte-exact; two more are strictly better but still
diverging. **Twelve cells moved in total.** Every one of the ten closures is a 4:2:2
cell; **no 4:2:0 and no 4:4:4 cell changed verdict** (§3.4).

The two shrink-only cells are worth a second look, because their wrong-region geometry
moved with them — which says the fix removed one writer and a second writer remains in
the same rectangle:

| cell | plane | pre wrong rows / cols | post wrong rows / cols |
|---|---|---|---|
| `s422_320x246` | U | rows 46..245, cols 94..157 | rows 46..**201**, cols **134**..157 |
| `s422_320x246` | V | rows 48..245, cols 99..159 | rows 48..**202**, cols **135**..157 |
| `s422_322x246` | U | rows 60..245, cols 108..160 | rows 60..**203**, cols **134**..160 |
| `s422_322x246` | V | rows 61..245, cols 107..160 | rows 61..**203**, cols **134**..160 |

The first-divergence coordinates are unchanged (`s422_320x246` frame 0 U(62,144);
`s422_322x246` frame 0 U(62,144)), so **the seed is the same writer that the fix did not
reach** — the fix shrank the contaminated region, it did not move its origin.

### 3.2 Where the split falls, and why

The census reported "4:2:2 8-bit: 5 of 12 exact, 7 diverge; 10-bit: 1 of 6 exact, 5
diverge". On the current tree:

| | pre (`cc9f2668`) | post (`ed7c99bc`) |
|---|---|---|
| 4:2:2 8-bit (40 cells) | 32 exact, 8 diverge | **36 exact, 4 diverge** |
| 4:2:2 10-bit (11 cells) | 2 exact, 9 diverge | **8 exact, 3 diverge** |

The split is not random and it is not a depth property. Ordering the still-diverging
cells by their **first-divergence decode frame**:

| cell | first bad frame | frames bad | class |
|---|---|---|---|
| `s422_416x250_10b` | 0 | 17/17 | luma also wrong |
| `s422_416x242_10b` | 0 | 17/17 | chroma only |
| `s422_322x240` | 0 | 17/17 | chroma only |
| `s422_320x246` | 0 | 17/17 | chroma only, region shrunk by the fix |
| `s422_322x246` | 0 | 17/17 | chroma only, region shrunk by the fix |
| `s422_352x242_10b` | 0 | 17/17 | chroma only |
| `s444_352x242_10b` | 0 | 17/17 | 4:4:4, chroma only |
| `s422_384x240` | **1** | 11/17 | chroma only |
[CORRECTED 2026-09-30 by lane-av1422444b: the `s444_352x242_10b` row above is
SUPERSEDED and this cell is NOT among the still-diverging cells at current main. The
row is a true reading of `ed7c99bc`, which was 36 commits behind `affe70dc` when this
correction was made; the fix is `f3afa5b7` ("window the 32-capped chroma palette
buffer per unit"), an ancestor of main, and it is 4:4:4-SPECIFIC — `decode_block_rect64`
caps chroma units at 32, so at `ss=00` a 64-axis luma strip's chroma is TILED and each
tiled unit had been handed the WHOLE block's palette prediction buffer, which
`PlaneBuf::reconstruct` reads as its first `side*side` entries. Measured at `affe70dc`:
0/0/0 vs ffmpeg 8.1.3 (`-pix_fmt yuv444p10le`) on all 16 shown frames AND 0/0/0 vs
instrumented aomdec on all 17 decode-order frames (16 shown + 1 hidden altref), with no
first divergence and an empty wrong-sample bbox. The pinned gate
`a_444_lossy_palette_chroma_unit_window_is_byte_exact_at_352x242_10bit`
(`crates/ec-av1/src/stream.rs:51024`) was proven to bite by mutation on the current
tree: with `palette_window` reverted in both tiled arms it fails at decode frame 0
byte 328192 (ours 209 vs 216), 0/1112/1362 through the ffmpeg comparator. Original
figures left standing above as the record of what was believed on `ed7c99bc`. Full
report: `lanes/av1422444b.report.md`.]

`S_odd326x242_10b` and `s422_352x250_10b` were 10-bit cells that closed; `s422_352x242_10b`
and `s422_416x242_10b` are 10-bit cells that did not. So **bit depth does not separate
them** — the census's "1 of 6 at 10-bit" was a property of which geometries that recipe
happened to land on, not of 10-bit 4:2:2.

The split that does hold is **reproducer reachability**, and it is a claim about the
encoder's partition search, not about the decoder: the closed cells are those whose
decoded block structure the corrected arm actually visits. `s422_384x240` is the one cell
that fails from decode frame 1 rather than frame 0 — consistent with a residual whose
origin is not the frame-0 intrabc seed, and it is the one cell the census already flagged
as starting late. **This census measures the boundary, it does not attribute it**; §8.

### 3.3 Corpus 4:2:2 — all 32 cells, current verdicts

`pre` = `cc9f2668`, `post` = current. All rows not listed in §3.1 are byte-exact on both.

| cell | ss | geometry | depth | frames | pre | post | post Y/U/V |
|---|---|---|---|---|---|---|---|
| `W_intrabc` (committed) | 10 | 320x240 | 8 | 17 | EXACT | EXACT | 0/0/0 |
| `X_intrabc_tiled` (committed) | 10 | 320x240 | 8 | 17 | EXACT | EXACT | 0/0/0 |
| `Y_intrabc_10b` (committed) | 10 | 320x240 | 10 | 17 | EXACT | EXACT | 0/0/0 |
| `422_allskip_2f` (committed) | 10 | 128x128 | 8 | 2 | EXACT | EXACT | 0/0/0 |
| `422_intrabc_sb128_strip` (committed) | 10 | 384x320 | 8 | 5 | EXACT | EXACT | 0/0/0 |
| `422_intrabc_sb128_strip_notxsearch` (committed) | 10 | 384x320 | 8 | 5 | EXACT | EXACT | 0/0/0 |
| `422_residual_compound_warp_16f` (committed) | 10 | 256x288 | 8 | 16 | EXACT | EXACT | 0/0/0 |
| `422_residual_compound_warp_nolr_16f` (committed) | 10 | 256x288 | 8 | 16 | EXACT | EXACT | 0/0/0 |
| `422_sb128_3f` (committed) | 10 | 128x128 | 8 | 3 | EXACT | EXACT | 0/0/0 |
| `AA_inter_compound` | 10 | 320x240 | 8 | 43 | EXACT | EXACT | 0/0/0 |
| `AB_inter_warp_odd` | 10 | 322x242 | 8 | 43 | DIVERGES | **EXACT** | 0/0/0 |
| `AD_inter_nogm` | 10 | 320x240 | 8 | 43 | EXACT | EXACT | 0/0/0 |
| `A_testsrc2_cpu0` | 10 | 320x240 | 8 | 17 | EXACT | EXACT | 0/0/0 |
| `B_testsrc2_cpu6` | 10 | 320x240 | 8 | 17 | EXACT | EXACT | 0/0/0 |
| `C_mandel320` | 10 | 320x240 | 8 | 16 | EXACT | EXACT | 0/0/0 |
| `D_bars` | 10 | 320x240 | 8 | 16 | EXACT | EXACT | 0/0/0 |
| `E_noglobal` | 10 | 320x240 | 8 | 17 | EXACT | EXACT | 0/0/0 |
| `F_allintra` | 10 | 320x240 | 8 | 16 | EXACT | EXACT | 0/0/0 |
| `H_10bit_testsrc2` | 10 | 320x240 | 10 | 17 | EXACT | EXACT | 0/0/0 |
| `I_10bit_mandel` | 10 | 320x240 | 10 | 16 | EXACT | EXACT | 0/0/0 |
| `J_10bit_lr0` | 10 | 320x240 | 10 | 17 | EXACT | EXACT | 0/0/0 |
| `K_sct` | 10 | 320x240 | 8 | 17 | EXACT | EXACT | 0/0/0 |
| `L_tiled` | 10 | 320x240 | 8 | 17 | EXACT | EXACT | 0/0/0 |
| `O_odd322x242` | 10 | 322x242 | 8 | 17 | DIVERGES | **EXACT** | 0/0/0 |
| `Q_odd320x242` | 10 | 320x242 | 8 | 17 | DIVERGES | **EXACT** | 0/0/0 |
| `R_odd322x240` | 10 | 322x240 | 8 | 17 | EXACT | EXACT | 0/0/0 |
| `S_odd326x242_10b` | 10 | 326x242 | 10 | 17 | DIVERGES | **EXACT** | 0/0/0 |
| `T_tilecols2` | 10 | 320x240 | 8 | 17 | EXACT | EXACT | 0/0/0 |
| `U_tilerows1` | 10 | 320x240 | 8 | 17 | EXACT | EXACT | 0/0/0 |
| `V_tile2x2_odd` | 10 | 322x242 | 8 | 17 | EXACT | EXACT | 0/0/0 |
| `ll422_allintra` | 10 | 320x240 | 8 | 1 | EXACT | EXACT | 0/0/0 |
| `ll422_noibc` | 10 | 320x240 | 8 | 16 | EXACT | EXACT | 0/0/0 |

### 3.4 Sweep 4:2:2 and the controls

| cell | ss | geometry | depth | pre | post | post Y/U/V |
|---|---|---|---|---|---|---|
| `s422_320x240` | 10 | 320x240 | 8 | EXACT | EXACT | 0/0/0 |
| `s422_320x242` | 10 | 320x242 | 8 | DIVERGES | **EXACT** | 0/0/0 |
| `s422_320x246` | 10 | 320x246 | 8 | DIVERGES | DIVERGES | **0/8339/5903** |
| `s422_322x240` | 10 | 322x240 | 8 | DIVERGES | DIVERGES | 0/22969/22534 |
| `s422_322x242` | 10 | 322x242 | 8 | EXACT | EXACT | 0/0/0 |
| `s422_322x246` | 10 | 322x246 | 8 | DIVERGES | DIVERGES | **0/3603/2324** |
| `s422_326x240` | 10 | 326x240 | 8 | DIVERGES | **EXACT** | 0/0/0 |
| `s422_326x242` | 10 | 326x242 | 8 | EXACT | EXACT | 0/0/0 |
| `s422_326x246` | 10 | 326x246 | 8 | DIVERGES | **EXACT** | 0/0/0 |
| `s422_384x240` | 10 | 384x240 | 8 | DIVERGES | DIVERGES | 0/1982/2081 |
| `s422_384x242` | 10 | 384x242 | 8 | EXACT | EXACT | 0/0/0 |
| `s422_384x246` | 10 | 384x246 | 8 | EXACT | EXACT | 0/0/0 |
| `s422_320x242_10b` | 10 | 320x242 | 10 | DIVERGES | **EXACT** | 0/0/0 |
| `s422_320x250_10b` | 10 | 320x250 | 10 | EXACT | EXACT | 0/0/0 |
| `s422_352x242_10b` | 10 | 352x242 | 10 | DIVERGES | DIVERGES | 0/4858/3949 |
| `s422_352x250_10b` | 10 | 352x250 | 10 | DIVERGES | **EXACT** | 0/0/0 |
| `s422_416x242_10b` | 10 | 416x242 | 10 | DIVERGES | DIVERGES | 0/5968/4524 |
| `s422_416x250_10b` | 10 | 416x250 | 10 | DIVERGES | DIVERGES | 103250/64191/62834 |

**4:2:0 controls: 22 of 22 byte-exact on both trees** (12 fresh 8-bit, 6 fresh 10-bit,
4 probe-cache lossless). Not one moved. **4:4:4 controls: 21 of 22 byte-exact on both
trees**, with the single exception `s444_352x242_10b` (352x242 10-bit, chroma only,
**0/1166/1413 — identical counts on both trees**, so the fix neither helped nor hurt a
format that is already on a shipping path). The 8 probe-cache lossless controls
(`ll420_a/c/d`, `ll444_a/b/c`, `ll420_allintra`, `ll444_allintra`) are byte-exact on both
trees.

That the supported formats are bit-identical across the fix is the load-bearing control:
the delta in §3.1 is 4:2:2-specific.
[CORRECTED 2026-09-30 by lane-av1422444b: the "4:4:4 controls: 21 of 22, with the
single exception `s444_352x242_10b` (0/1166/1413 — identical counts on both trees)"
sentence above is true only of the TWO trees this census had (`cc9f2668` and
`ed7c99bc`). At current main `affe70dc` — 36 commits past `ed7c99bc` — that exception
is GONE: `f3afa5b7` fixed it, so all 22 of 22 4:4:4 cells are byte-exact and the
"identical counts on both trees" reading was a property of the census's own tree pair,
not of the format. Measured at main: `s444_352x242_10b` is 0/0/0 vs ffmpeg 8.1.3 on all
16 shown frames and 0/0/0 vs instrumented aomdec on all 17 decode-order frames. The
control this paragraph was carrying still holds, and now holds twice over: the §3.1
delta is 4:2:2-specific, AND the 4:4:4 palette fix left 4:2:0 bit-identical (69 of 69
committed 4:2:0 fixtures and 18 of 18 fresh 4:2:0 sweep cells byte-exact at main, zero
skips) — 4:2:0 is structurally immune, since `nw * nh == 1` on every 4:2:0 block so the
32-cap never tiles. Original sentence left standing above.]

---

## 4. The census's numbers reproduce exactly on `cc9f2668`

Every diverging count the census published, re-measured on my pre tree with my
instrument:

| cell | census (cc9f2668) | my pre tree | |
|---|---|---|---|
| `Q_odd320x242` | 0/9603/14456 | 0/9603/14456 | match |
| `O_odd322x242` | 0/985/722 | 0/985/722 | match |
| `AB_inter_warp_odd` | 0/3705/4366 | 0/3705/4366 | match |
| `S_odd326x242_10b` | 0/301/751 | 0/301/751 | match |
| `s422_320x242` | 0/6337/8398 | 0/6337/8398 | match |
| `s422_320x246` | 0/38348/59491 | 0/38348/59491 | match |
| `s422_322x240` | 0/22969/22534 | 0/22969/22534 | match |
| `s422_322x246` | 0/24964/25438 | 0/24964/25438 | match |
| `s422_326x240` | 0/734/0 | 0/734/0 | match |
| `s422_326x246` | 0/24078/25112 | 0/24078/25112 | match |
| `s422_384x240` | 0/1982/2081 | 0/1982/2081 | match |
| `s422_320x242_10b` | 0/3915/5612 | 0/3915/5612 | match |
| `s422_352x242_10b` | 0/4858/3949 | 0/4858/3949 | match |
| `s422_352x250_10b` | 0/3876/1786 | 0/3876/1786 | match |
| `s422_416x242_10b` | 0/5968/4524 | 0/5968/4524 | match |
| `s422_416x250_10b` | 103250/64191/62834 | 103250/64191/62834 | match |
| `s444_352x242_10b` | 0/1166/1413 | 0/1166/1413 | match |

17 of 17. The census's comparator and cell derivation are sound; only its verdict column
is bound to its tree.

---

## 5. Recipe dependence, measured (not asserted)

Two cells' verdicts have been claimed to depend on the encoder recipe
(`lanes/av1422ctrigger.report.md` §2d: the previous sweep's `-cpu-used 6` dodged the
defect that `--cpu-used 0` hit). That claim is tested here directly, both on the pre
tree and on the current tree: same source (`testsrc2`, 16 frames), same four trigger
flags, same geometries, **only `--cpu-used` varied** over {0, 2, 4, 6}, 16 fresh cells.

| cell | pre (`cc9f2668`) | post (`ed7c99bc`) |
|---|---|---|
| `rc0_s422_320x242` | DIVERGES 0/6337/8398 | **BYTE-EXACT** |
| `rc0_s422_320x246` | DIVERGES 0/38348/59491 | DIVERGES 0/8339/5903 |
| `rc0_s422_322x240` | DIVERGES 0/22969/22534 | DIVERGES 0/22969/22534 |
| `rc0_s422_322x242` | BYTE-EXACT | BYTE-EXACT |
| `rc2_s422_320x242` | DIVERGES **436945**/225472/227690 | DIVERGES **436945**/225472/227690 |
| `rc2_s422_320x246` | DIVERGES 0/5372/28948 | DIVERGES 0/2584/2229 |
| `rc2_s422_322x240` | DIVERGES 0/1599/1426 | DIVERGES 0/1599/1426 |
| `rc2_s422_322x242` | DIVERGES 0/72126/73078 | **BYTE-EXACT** |
| `rc4_s422_320x242` | BYTE-EXACT | BYTE-EXACT |
| `rc4_s422_320x246` | BYTE-EXACT | BYTE-EXACT |
| `rc4_s422_322x240` | BYTE-EXACT | BYTE-EXACT |
| `rc4_s422_322x242` | BYTE-EXACT | BYTE-EXACT |
| `rc6_s422_320x242` | BYTE-EXACT | BYTE-EXACT |
| `rc6_s422_320x246` | BYTE-EXACT | BYTE-EXACT |
| `rc6_s422_322x240` | BYTE-EXACT | BYTE-EXACT |
| `rc6_s422_322x242` | BYTE-EXACT | BYTE-EXACT |

What this shows, precisely:

* **`--cpu-used 4` and `--cpu-used 6` produce 8 of 8 byte-exact cells on BOTH trees.**
  That is the census's §2d claim confirmed as an encoder-search property: at those
  efforts libaom's partition search simply never lands on the block structure that
  reaches the defective arm. The old sweep's "all clean" was a true reading of a
  search that did not visit the code.
* **`--cpu-used 0` still reaches it**, and on the current tree it reaches a *smaller*
  region than it did on `cc9f2668` — the same shrinkage seen in `s422_320x246`.
* **A new fact, not in the census: `--cpu-used 2` reaches a class nothing else does.**
  `rc2_s422_320x242` diverges **436945 wrong LUMA samples** (33% of the luma plane) plus
  225472/227690 chroma, identically on both trees. Every cell in the census and in this
  report's main sweep except `s422_416x250_10b` has **Y exactly 0**; this is the second
  luma-breaking 4:2:2 cell in the corpus, and it is a different failure from the one the
  census's class C describes. It is byte-identical on both trees, so the skipped-intrabc
  fix does not touch it.
* `--cpu-used 2` is also the only effort at which `322x242` diverges at all — the
  geometry that is exact at 0, 4 and 6. So the recipe axis is not monotone in effort.

**Consequence for any verdict:** a 4:2:2 verdict stated without its recipe is not a
statement about the decoder. Every verdict in §3 and §5 is tied to a named
`--cpu-used`.

---

## 6. What the shipped tree does today

Measured on the **clean** tree (bypass reverted, `decode_probe` rebuilt from
`stream.rs` with the refusal in force), all 95 cells:

| chroma | cells | REFUSES | BYTE-EXACT | DIVERGES |
|---|---|---|---|---|
| 4:2:2 | **51** | **51** | 0 | 0 |
| 4:2:0 | 22 | 0 | 22 | 0 |
| 4:4:4 | 22 | 0 | 21 | 1 (`s444_352x242_10b`) |

**All 51 4:2:2 cells refuse with one identical string** (`unsupported: AV1
decode_stream (a chroma format of 4:2:2 (subsampling_x != subsampling_y): …)`) — one
message across every geometry, depth and origin in the corpus, including the nine
committed fixtures. The refusal is total and uniform, and it is exactly as unconditional
as `stream.rs:1803` reads. It is also the only thing standing between the 4:4:4 defect
and shipped pixels.

---

## 7. Decision table — numbers and classes only

| | 4:2:2 | 4:2:0 control | 4:4:4 control |
|---|---|---|---|
| corpus cells measured | 32 | 4 | 4 |
| corpus byte-exact (**current**) | **32** | 4 | 4 |
| corpus diverging (**current**) | **0** | 0 | 0 |
| fresh sweep cells | 18 | 18 | 18 |
| fresh sweep byte-exact (**current**) | **11** | 18 | 17 |
| fresh sweep diverging (**current**) | **7** | 0 | 1 |
| fresh reproducer (`R422_320x242`) | 1 exact | — | — |
| **total measured** | **51** | **22** | **22** |
| **byte-exact (current)** | **44** | **22** | **21** |
| **diverging (current)** | **7** | **0** | **1** |
| **refusing (shipped tree)** | **51** | 0 | 0 |
| of the exact, committed-pin-backed | 9 | 0 | 0 |
| of the exact, probe-only (no sha in repo) | 35 | 22 | 21 |
| *(was: byte-exact at `cc9f2668`)* | *34* | *22* | *21* |
| *(was: diverging at `cc9f2668`)* | *17* | *0* | *1* |

### 7.1 Remaining failure classes, with the recipe that reproduces each

| class | cells (current) | recipe | worst cell |
|---|---|---|---|
| **A — chroma-only, U+V, every frame**, the dominant class | 4: `s422_322x240`, `s422_416x242_10b`, `s422_352x242_10b`, `s422_320x246` | `mksweep.sh`, `--cpu-used 0`, 8-bit 4 widths x 3 heights / 10-bit 3 x 2 | `s422_322x240` 45503 samples |
| **A' — same class, region shrunk by the intrabc fix, still open** | 2: `s422_320x246`, `s422_322x246` | `--cpu-used 0`, `320x246` / `322x246` | `s422_320x246` 14242 samples |
| **B — starts at decode frame 1, not frame 0** | 1: `s422_384x240` (11/17 frames bad) | `--cpu-used 0`, 384x240 | 4063 samples |
| **C — luma also wrong, 10-bit** | 1: `s422_416x250_10b` | `--cpu-used 0`, 416x250 10-bit | **230275 samples** |
| **D — not 4:2:2: 10-bit 4:4:4 chroma** | 1: `s444_352x242_10b` | `--cpu-used 0`, 352x242 10-bit 4:4:4 | 2579 samples |
| **E — NEW, found here: luma desync at `--cpu-used 2`** | 1: `rc2_s422_320x242` | **`--cpu-used 2`**, 320x242 8-bit | **890107 samples** |

[CORRECTED 2026-09-30 by lane-av1422444b: class D is now EMPTY. `f3afa5b7` fixed
`s444_352x242_10b` and is an ancestor of main; `ed7c99bc`, the tree this table was
measured on, was 36 commits behind `affe70dc` at the time of this correction. Measured
at main: 0/0/0 vs ffmpeg 8.1.3 on all 16 shown frames and 0/0/0 vs instrumented aomdec
on all 17 decode-order frames, and the whole 4:4:4 surface is clean — 18/18 fresh 4:4:4
sweep cells, 18/18 fresh 4:2:0 controls, 36/36 committed 4:4:4 fixtures, 69/69
committed 4:2:0 fixtures, zero skips. The defect was 4:4:4-SPECIFIC and had nothing to
do with the 4:2:2 family: `decode_block_rect64` caps chroma units at 32, so at `ss=00` a
64-axis luma strip's chroma is TILED and each tiled unit was handed the whole block's
palette prediction buffer. The sensitive axis is CHROMA UNIT COUNT, not frame width or
height, which is why the sibling geometries at the same depth were already exact and
why this cell's "geometry-sensitive" framing was misleading. The class D row above is
left standing as the record of what was believed on `ed7c99bc`. Full report:
`lanes/av1422444b.report.md`.]

**Worst current 4:2:2 divergence:** `s422_416x250_10b`, 230275 wrong samples. Worst
overall including the new class E: `rc2_s422_320x242`, 890107 wrong samples including
436945 luma. Smallest current 4:2:2: `s422_384x240`, 4063.

### 7.2 What a production lift would and would not cover today, as arithmetic

* Lifting the header refusal would put **44 byte-exact 4:2:2 cells** on a shipping path.
  **9** of them are committed bytes; the other 35 exist only in
  `/home/tahinli/.cache/cells/`, `/home/tahinli/.cache/lane-av1422ctintrabc/cells/` and
  this lane's sweep directory, so a lift would ship **no committed pixel gate** for any
  of them unless those fixtures are committed first.
* A lift would expose **7 diverging 4:2:2 cells** — one in seven of the 4:2:2 corpus
  measured, down from one in three at `cc9f2668`. Two of them (`s422_416x250_10b`,
  and class E if a `--cpu-used 2` stream ever arrives) fail in the **luma plane**.
* A lift would **not** fix class D: `s444_352x242_10b` is a 4:4:4 defect, already
  reachable, already diverging with the refusal standing, unchanged by the fix.
  [CORRECTED 2026-09-30 by lane-av1422444b: this constraint is GONE. Class D is empty
  at main — `f3afa5b7` fixed `s444_352x242_10b` and is an ancestor of main, while
  `ed7c99bc` (the tree these numbers were measured on) was 36 commits behind `affe70dc`
  at the time of this correction. Re-measured at main: 0/0/0 vs ffmpeg 8.1.3 on all 16
  shown frames and 0/0/0 vs instrumented aomdec on all 17 decode-order frames, and the
  full 4:4:4 surface is clean (18/18 fresh 4:4:4 sweep cells, 36/36 committed 4:4:4
  fixtures, zero skips). So a lift is no longer constrained by a 4:4:4 exception: every
  cell outside 4:2:2 is byte-exact, and 4:2:0 is structurally immune to that fix
  (`nw * nh == 1` on every 4:2:0 block, so the 32-cap never tiles) — 69/69 committed
  4:2:0 fixtures and 18/18 fresh 4:2:0 sweep cells byte-exact at main. The two 4:2:2
  bullets above are unaffected by this correction. Sentence left standing above.]
* A lift would **not** reach class E, which is invisible at every recipe the census swept.
* The lift's blast radius is not bounded by this census: §8.

**This report recommends no product decision.** The numbers above are the input to one.

---

## 8. What is NOT measured

1. **12-bit.** No 12-bit cell was encoded or measured. 4:2:2 12-bit remains entirely
   unmeasured, on both trees.
2. **Superres.** No superres 4:2:2 cell. `EC_AV1_FINAL_DUMP` is post-superres so such a
   cell would be measurable with this instrument; none was built.
3. **The `--cpu-used` axis is sampled at {0, 2, 4, 6} only**, on four geometries at 8-bit.
   Class E was found at one sample point of that axis; the axis is not swept.
4. **No other encoder knob is varied.** `--cq-level` is fixed at 24, `--lag-in-frames` at
   25, altref and global motion on, `--pass=1`, 16 frames, `testsrc2` only. Per
   `lanes/av1422ctrigger.report.md` §2e a repeated aomenc flag takes the FIRST occurrence,
   so a naive flag sweep would silently measure the base arm twice.
5. **Film grain, order_hint streams, tiles at odd sizes beyond the corpus.**
6. **Frame-size changes within one stream** — untested here.
7. **Monochrome and 4:4:0** — not codable, out of scope by the existing gate.
8. **Nothing here attributes the residual divergences to a source line.** §3.1 shows the
   fix's reach exactly, and §3.1's shrink-only pair shows a second writer inside the same
   rectangle at the same first-divergence coordinate. Which writer, is not answered.
9. **The display-order basis was not re-measured.** Everything here is the DECODE basis
   (`.f<N>` = decode index, hidden alt-refs included), as in the census. For the cells
   that closed, decode-basis exactness implies display-basis exactness; for the 7 that
   did not, no display-basis number is claimed here.
10. **No committed pixel gate exists for any 4:2:2 cell.** Every number in this report is
    probe-only evidence, exactly as in the census; the 23 probe-only corpus cells are
    pinned by no sha in this repository.
11. **Nothing was fixed.** Every failure above is a report line.

---

## 9. Should `lanes/av1422census.report.md` be annotated as stale? — verdict per line

Yes, but only on the **verdict columns and the counts derived from them**. The
comparator, the cell derivation, the liveness proof, the encoder-recipe narrative and the
negative controls are all confirmed by independent re-measurement and must stand
unannotated. Line-precise:

**STALE — must be annotated or struck:**

| lines | what is stale | replacement |
|---|---|---|
| **14–19** | "of 50 distinct 4:2:2 cells measured today, 34 decode byte-exact and 16 diverge" | at current main: 51 measured, 44 exact, 7 diverge. The sentence's whole point ("12 of 18 fresh cells diverge") becomes 7 of 18. |
| **37–38** | `4:2:2 byte-exact 34` / `4:2:2 diverging 16` | 44 / 7 (and the measured-cell count 50 → 51) |
| **210, 223, 224, 226** | corpus table rows for `AB_inter_warp_odd`, `O_odd322x242`, `Q_odd320x242`, `S_odd326x242_10b` | verdict `BYTE-EXACT`, 0/0/0 |
| **284, 289, 291, 313, 316** | sweep rows `s422_320x242`, `s422_326x240`, `s422_326x246`, `s422_320x242_10b`, `s422_352x250_10b` | verdict `BYTE-EXACT`, 0/0/0 |
| **285, 288** | sweep rows `s422_320x246`, `s422_322x246` | still `DIVERGES`, but the counts shrink: 38348/59491 → **8339/5903**, 24964/25438 → **3603/2324** |
| **374–445** (§4 per-cell detail blocks) | the 10 now-exact cells' blocks; the 2 shrunk cells' blocks | delete / restate; the blocks are a correct record of `cc9f2668` |
| **447–465** (TOTALS + the `Counter`) | `Counter({'BYTE-EXACT': 77, 'DIVERGES': 17})` and the 16-entry totals list | current over the same 94-cell list: 86 exact, 8 diverge |
| **470–488** (§4 class A/B reading) | "13 of the 16 diverging", "Class B — U-only `s422_326x240`", "worst cell `Q_odd320x242` 24059" | class B is **gone** (`s422_326x240` closed); classes shrink to 6 in 4:2:2 + 1 in 4:4:4 |
| **491–493** | "Class C — worst cell in the census by an order of magnitude" | still true of this census, but it is no longer the worst 4:2:2 cell measured overall — class E (`rc2_s422_320x242`, 890107) exceeds it and the census never saw it |
| **500–519** (§4.1 display basis) | all 16 rows | **not re-measured by me** — annotate as "display basis, measured at `cc9f2668`, not re-verified" rather than restating |
| **522–537** (§5) | the "`s444_352x242_10b` decodes, DIVERGES 0/1166/1413" row is **still true**; the "50 of 50 refuse by name" count becomes 51 of 51 | mostly stands; update the count |
| **565–592** (§7 decision table) | every number in it: 34/16, corpus 28/4, sweep 6/12, "A lift would expose 16 diverging cells", the class table with 13 + 1 + 1 + 1 | current: 44/7, corpus 32/0, sweep 11/7, 7 diverging exposed, classes A(4) + A'(2) + B(1) + C(1) + D(1) + **E(1, new)** |
| **596–603** | "Largest 8-bit: `s422_320x246`, 97839. Smallest: `s422_326x240`, 734." | `s422_326x240` closed; largest current 4:2:2 8-bit is `s422_320x246` at 14242, smallest is `s422_384x240` at 4063 |

**NOT stale — leave alone:**

| lines | why |
|---|---|
| 41–145 (§1, comparator + 48-arm flip table) | re-proven independently here, 42/42 arms, same disciplines, plus non-zero-baseline arms the census lacked |
| 149–186 (§2, cell derivation) | re-derived from scratch: 242 probe cells, 44 candidates, **32 distinct after sha dedup, 9 committed** — identical |
| **29–37** (§0 "the previous sweep used ffmpeg -cpu-used 6 over a rejected y4m") | confirmed and sharpened: `--cpu-used 4` and `6` produce 8/8 byte-exact cells on both trees (§5) |
| 236–259 (§3 recipe + grid) | the 54 fresh encodes are **byte-identical sha256** to the census's |
| 315–338 (§3.1 controls) | 4:2:0 22/22 exact and the `s444_352x242_10b` 4:4:4 defect all reproduce exactly |
| 538–560 (§6 reproducing any row) | paths and recipes all valid |
| 611–641 (§8 what is not measured) | still true; item 8's attribution refutation is untouched — and this census does not attribute either |
| 645–658 (§9 regression) | the 181-passed baseline reproduces on the current tree |

**My recommendation on the annotation:** the block at lines 3–11 that Main already added
is correct but under-scoped — it says "every DIVERGES verdict in the tables below for
those four cells … and any fresh-sweep cell of the same class". The measured class is
**wider than "the same class"**: it is exactly **12 cells** (the 10 closures of §3.1 plus
the 2 shrink-only cells, whose counts also changed). A reader who takes "the same class"
to mean "cells the fix fully closed" will leave §3.1's two shrunk rows un-annotated and
quote 38348/59491 as current. Suggest widening that sentence to name the 12 and to point
at §3.1 rather than describing the class.

---

## 10. Regression

On the clean current tree (bypass reverted), worktree `lane-av1422census2`:

```
cargo test -p ec-av1 --lib -- 422 444 420 lossless warp intra \
    --skip bitrate_target_lands_within_5_percent_over_48_frames
```

```
test result: ok. 181 passed; 0 failed; 2 ignored; 0 measured; 619 filtered out; finished in 355.82s
```

Identical to the census's own baseline (181 passed). Baseline for this lane; no source
change is proposed, so there is no "after".

---

## 11. Reproducing any row

```bash
# instrument + cells (this lane)
D=/home/tahinli/.cache/census422b            # cmp422.py run.py liveness.py discover.py mksweep.sh

# 1. derive the cell list (aomdec header classification, sha dedup)
python3 $D/discover.py /home/tahinli/.cache/wt/av1422census2 $D/cells.json
#    -> {"probe":242,"fixture":104,"c422":44,"c422_unique":32,"dup_dropped":12}

# 2. the sweep, at the census's recipe (deterministic: 54/54 sha-identical to the census)
DEPTHS=8  WIDTHS="320 322 326 384" HEIGHTS="240 242 246" OUT=$D/sweep bash $D/mksweep.sh
DEPTHS=10 WIDTHS="320 352 416"     HEIGHTS="242 250"     OUT=$D/sweep bash $D/mksweep.sh

# 3. one cell, either tree (both need the patch-run-restore 4:2:2 bypass)
EC_AV1_FINAL_DUMP=$W/oracle ~/.cache/aom-oracle/build/aomdec --codec=av1 -o $W/o.y4m CELL.obu
EC_AV1_FINAL_DUMP=$W/ours EC_NOMEMGUARD=1 ~/.cache/tgt/av1422census2/debug/examples/decode_probe CELL.obu
python3 $D/cmp422.py CELL $W/ours $W/oracle W H SSX SSY DEPTH

# 4. liveness (per plane, per cell, per frame)
python3 $D/liveness_post.py $D/live_spec.json $D/liveness_post.json     # exact + corpus
python3 $D/liveness_post.py $D/live_spec_div.json $D/liveness_div.json   # diverging baselines

# 5. the recipe axis
bash $D/mkrecipe.sh && python3 $D/run_post.py $D/cells_recipe.json $D/recipe_post.json
```

Trees: `wt/av1422census2` (`ed7c99bc`, `CARGO_TARGET_DIR=…/tgt/av1422census2`) and
`wt/av1422census2-pre` (`cc9f2668`, `…/tgt/av1422census2-pre`). Drivers `run_post.py`,
`run_pre.py`, `run_ship.py` differ only in `PROBE` and `WORK`; all three carry the
identical `cmp422.compare` comparator.
