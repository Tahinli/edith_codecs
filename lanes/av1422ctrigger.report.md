# lane-av1422ctrigger — the 4:2:2 intra-chroma trigger is REPRODUCED; the availability decision is NOT the writer (measured refutation)

**Outcome in one line: the encoder setting is found and a one-command reproducer
exists, but the "availability decision" hypothesis is REFUTED by two independent
mutation probes — so this lane delivers the reproducer and a refutation, not a
fix.** The tree is clean; no source change is proposed.

Tip: `main` = `a30aca9e`. Worktree `/home/tahinli/.cache/wt/av1422ctrigger`,
branch `lane-av1422ctrigger`, **zero commits, zero source changes**. Every edit
ever applied to this tree (the 4:2:2 sequence-header bypass probe, an
`EC_AV1_FHDUMP` frame-header dump, an `EC_SEEDTRACE` call-site print, a
`EC_422_DENY_LEFT` availability probe and an `EC_422_ANCHOR` anchor probe) is
reverted. `stream.rs:1803` reads `if seq.subsampling_x != seq.subsampling_y {`
again and the primary checkout's `git status --porcelain` is empty.

---

## 0. What the brief assumed, and what measurement says

The brief's step 1 says "the trigger is a header FIELD". **That is refuted.**
The trigger is not any header field; it is the *block structure the encoder's
partition/mode search lands on*, and the reason the previous lane's 42 fresh
encodes all dodged it is that they were not built with the same encoder recipe.

| brief's premise | measured verdict |
|---|---|
| a sequence-header field differs | **REFUTED** — §1, all 7 cells share a bit-identical shape |
| a frame-0 header field differs | **REFUTED** — §1, identical except the content-driven `base_q_idx` |
| the trigger is reproducible | **CONFIRMED** — §2, one `aomenc` command reproduces it at the *same byte* |
| the availability / `dc_top` decision is the writer | **REFUTED** — §5, two probes each break the byte-exact cells |

---

## 1. The header diff: no header field discriminates

`EC_AV1_SEQDUMP` (the crate's own sequence-header dump,
`crates/ec-av1-syntax/src/sequence.rs:375`) over all six corpus cells plus my
fresh reproducer, and an `EC_AV1_FHDUMP` frame-header dump I added temporarily
in the worktree (reverted):

| cell | geometry | `seq_profile` | `reduced_still_picture` | `use_128x128_sb` | `filter_intra` | `order_hint` | `cdef` | `tiles` | `qindex` |
|---|---|---|---|---|---|---|---|---|---|
| `Q_odd320x242` | 320x242 8 | 2 | false | true | true | true | **bits 0** | 1x1 | 25 |
| `O_odd322x242` | 322x242 8 | 2 | false | true | true | true | **bits 0** | 1x1 | 27 |
| `AB_inter_warp_odd` | 322x242 8 | 2 | false | true | true | true | **bits 0** | 1x1 | 16 |
| `S_odd326x242_10b` | 326x242 10 | 2 | false | true | true | true | **bits 0** | 1x1 | 34 |
| **`V_tile2x2_odd`** (EXACT) | 322x242 8 | 2 | false | true | true | true | **bits 2** | **2x2** | 27 |
| **`R_odd322x240`** (EXACT) | 322x240 8 | 2 | false | true | true | true | **bits 0** | 1x1 | 31 |
| **`MYREPRO`** (mine, DIVERGES) | 320x242 8 | 2 | false | true | true | true | **bits 0** | 1x1 | 26 |

Every sequence header is the same shape: `profile=2 still=false reduced=false
timing_present=false op_cnt=1 idc0=0 level0=0 frame_width_bits=9
frame_height_bits=8 frame_id_present=false use_128=true filter_intra=true
edge_filter=true interintra=true masked=true warped=true dual_filter=true
order_hint=true jnt_comp=true ref_frame_mvs=true`, differing only in
`max_w`/`max_h`.

**The control that kills the header theory is `R_odd322x240`.** It is EXACT
(17/17) and its frame-0 header field set is *identical* to the failing cells':
same profile, same flags, same `cdef_bits=0`, same `allow_intrabc=true`, same
`tx_mode=Select`, same `hdr_bits=39`, same 1x1 tiles, same
`lr_type=[None,None,None]`. The only differences are `max_h` (240 vs 242) and
`qindex` (31 vs 25) — and `base_q_idx` is the encoder's rate-control output
for *this content*, not a mode or tool flag.

So **no header field is the trigger.** The brief's hypothesis is refuted, and
this is the same conclusion `lanes/av1422h242map` reached from the other side
("it is CONTENT, and it is 4:2:2-only"), now confirmed at header granularity.

---

## 2. THE REPRODUCER — the encoder setting, named

The previous lane (`lanes/av1422llodd` §3) could not rebuild the four cells and
recorded the encoder setting as "the open factor". It is not open. The recipe is
in `lanes/av1422h242map` §1 ("R arm"), which reports reproducing `Q_odd320x242`
byte-for-byte (`17304b6cca378034`). Its failure to run for the prior lane was a
**y4m framing bug, not an aomenc limitation**: libaom's `y4m_input_fetch_frame`
(`av1/common/y4minput.c:1163-1171`) requires each frame to begin with the six
bytes `FRAME\n`. A y4m written with the full `YUV4MPEG2 ...` header on every
frame fails with `Loss of framing in Y4M input data` and encodes zero frames.

### 2a. Source (hand-built 4:2:2 y4m; ffmpeg's muxer will not write the `C422` tag)

```
ffmpeg -v error -f lavfi -i testsrc2=size=320x242:rate=25 \
       -frames:v 16 -pix_fmt yuv422p -f rawvideo src.raw -y
# then: one "YUV4MPEG2 W320 H242 F25:1 Ip A1:1 C422\n" header line,
#       then per frame "FRAME\n" + 320*242 + 2*(160*242) bytes
```

### 2b. The encode — this is the named trigger setting

```
aomenc --codec=av1 --profile=2 --input-bit-depth=8 --bit-depth=8 --limit=16 \
       --lag-in-frames=25 --auto-alt-ref=1 --enable-global-motion=1 --pass=1 \
       --cq-level=24 --threads=4 --kf-min-dist=0 --kf-max-dist=999999 \
       --width=320 --height=242 --cpu-used=0 src/testsrc2_320x242.y4m -o n.webm
ffmpeg -v error -i n.webm -c copy -f obu n.obu -y
```

`aomenc` = `/home/tahinli/.cache/aom-oracle/build/aomenc` (the instrumented
oracle tree's encoder, libaom as of 2026-09-30).

### 2c. It reproduces, at the same byte

`MYREPRO.obu`, 20 733 B, `sha256 909e58db18fd4b759…` (mine; the corpus cell
`Q_odd320x242.obu` is 21 381 B, `sha256 17304b6cca378034…` — the two differ from
byte 16 on, i.e. my `testsrc2` source bytes differ from the corpus's, so this is
a *behavioural* reproduction, not a byte-identical one):

| cell | decode frames | first bad decode frame | first bad byte |
|---|---|---|---|
| `Q_odd320x242` (corpus) | 17 | **0** | **103156** |
| **`MYREPRO` (this lane, fresh)** | 17 | **0** | **103156** |
| `O_odd322x242` | 17 | 0 | 106388 |
| `AB_inter_warp_odd` | 43 | 0 | 110876 |
| `S_odd326x242_10b` | 17 | 0 | 214112 |
| `R_odd322x240` (control, EXACT) | 17 | — | — |
| `V_tile2x2_odd` (control, EXACT) | 17 | — | — |

### 2d. Why the previous lane's 42 fresh encodes all dodged it

`lanes/av1422llodd` §3 built its sweep with
`ffmpeg + libaom-av1 -crf 30 -b:v 0 -cpu-used 6 -usage good -lag-in-frames 0`.
That is **ffmpeg's libaom wrapper with a different rate-control mode and
`--cpu-used 6`**, not the `aomenc` R arm. The defect needs
`--cpu-used 0 --lag-in-frames 25 --auto-alt-ref=1 --enable-global-motion=1`
with a constant `--cq-level`; at `-cpu-used 6` the partition search settles on
different block structures and the shape never arises. **The sweep was a
control against the wrong recipe, which is why it read as "42/42 exact, no
trigger".**

### 2e. Flag sweep on the fixed geometry (320x242, same source)

Every variant below was encoded with the recipe above (same source, same
geometry) and measured with the §3 comparator. `sha256` is the first 32 hex:

| variant | flag change | bytes | `sha256` | verdict (decode-order frames) | Y / U / V wrong |
|---|---|---|---|---|---|
| `sw_base` | (the recipe, §2b) | 20 733 | `909e58db18fd4b759…` | **0/17 exact**, first bad frame 0 | 0 / 6337 / 8398 |
| `sw_cdef1` | `--enable-cdef=1` | 20 733 | `909e58db18fd4b759…` | **0/17 exact** | 0 / 6337 / 8398 |
| `sw_cdef0` | `--enable-cdef=0` | 20 753 | `ceac79b220313b7a…` | **0/17 exact** | 0 / 6196 / 8462 |
| `sw_endq` | `--end-usage=q` | 27 377 | `aa16455036aaf142…` | **0/17 exact** | 0 / 7964 / 11614 |
| `sw_cq18` | `--cq-level=18` | 20 733 | `909e58db18fd4b759…` | **void** — see below | — |
| `sw_cq30` | `--cq-level=30` | 20 733 | `909e58db18fd4b759…` | **void** — see below | — |

**CDEF is not the discriminator.** `--enable-cdef=1` is byte-identical to the
base (it is the default), `--enable-cdef=0` produces a *different* stream that
still diverges, and the failing cells' own frame headers already carry
`cdef_bits=0` — CDEF is inert in every stream measured here, so it cannot carry
the defect.

**`--end-usage=q` is not the discriminator either**: it diverges too, at a
different magnitude (U 7964 / V 11614 vs 6337 / 8398). Both rate-control modes
produce the shape.

**The `--cq-level` rows are void, and the reason is measured, not assumed.**
`mkenc.sh` hard-codes `--cq-level=24` in its base flag list and appends the
variant flag after it; `sw_cq18` and `sw_cq30` came out **byte-identical to
`sw_base`** (same 20 733 B, same `sha256`), which proves `aomenc` takes the
**first** occurrence of a repeated flag and the appended one never took effect.
I did not re-run that axis with the flag actually removed, so the rate-control
axis is **not** swept and I am not claiming anything about it.

**Conclusion: no single aomenc tool flag turns the defect on or off.** It is the
partition/mode search's *outcome* for this content at this geometry, which is
why it is content-sensitive within one recipe (§2e, and
`lanes/av1422h242map` §4's `smptebars` control) and recipe-sensitive across
recipes (§2d).

---

## 3. Per-cell, per-plane, per-decode-frame measurement (with flip control)

`FINAL` (post-filter) decode-order dumps from both sides, compared per plane.
Both sides' `FINAL` dumps are already cropped to the display shape and have
equal byte length, so this is a straight per-plane wrong-SAMPLE count (16-bit LE
for the 10-bit cell). `Y / U / V`:

| cell | frames | Y | U | V | total | frame-0 Y/U/V |
|---|---|---|---|---|---|---|
| `Q_odd320x242` | 17 | **0** | 9603 | 14456 | 24059 | 0/422/379 |
| `O_odd322x242` | 17 | **0** | 985 | 722 | 1707 | 0/32/16 |
| `S_odd326x242_10b` | 17 | **0** | 301 | 751 | 1052 | 0/17/32 |
| `AB_inter_warp_odd` | 43 | **0** | 3705 | 4366 | 8071 | 0/32/32 |
| `V_tile2x2_odd` | 17 | 0 | 0 | 0 | **0 (EXACT)** | — |
| `R_odd322x240` | 17 | 0 | 0 | 0 | **0 (EXACT)** | — |

**Oracle-flip control, same comparator, one bit flipped in the oracle's own
frame-0 mid-frame byte:**

| cell | measured | flipped | delta | landed in |
|---|---|---|---|---|
| `Q_odd320x242` | U 9603 | U 9604 | **+1** | U, frame 0 |
| `O_odd322x242` | U 985 | U 986 | **+1** | U, frame 0 |
| `S_odd326x242_10b` | U 301 | U 302 | **+1** | U, frame 0 |
| `AB_inter_warp_odd` | U 3705 | U 3706 | **+1** | U, frame 0 |
| `V_tile2x2_odd` | 0 | 1 | **+1** | U, frame 0 |
| `R_odd322x240` | 0 | 1 | **+1** | Y, frame 0 |

The comparator bites: exactly one flipped bit produces exactly one extra wrong
sample, in the plane the flip landed in. The counts above are real.
**Luma is byte-exact on all four failing cells, every decode frame.**

**There is no "after" column: this lane changed no source, so every number above
is a before measurement and the after state is identical by construction.**

---

## 4. The seed, pinned in pixels

`Q_odd320x242`, decode frame 0, `EC_AV1_PREFILT_DUMP` from both sides
(ours de-padded from its stride-256 plane buffers, the oracle already cropped).
Wrong region: **chroma cols 116–139, rows 160–201, in both U and V** — 24 of 160
columns, 42 of 242 rows, ~0.5 % of each chroma plane. Luma: 0 wrong.

First wrong unit, U, chroma (116,160) — a 4x4 unit, wrong in **all 16** pixels:

| | value | arithmetic |
|---|---|---|
| above row (chroma y=159, x=116..119) | `202 202 202 202` — **byte-identical both sides** | sum 808 |
| left column (chroma x=115, y=160..163) | `165 164 163 152` — **byte-identical both sides** | sum 644 |
| **ours** | **182** | `(808 + 644 + 4) / 8` = `dc` (above **and** left) |
| **oracle** | **202** | `(808 + 2) / 4` = above-only |

So the samples on both edges are identical on the two sides and only the
*DC variant* differs. Our own trace names the unit
(`EC_PRED=1`, `decode.rs:4641`, the **square** `reconstruct` path, not the
4x8 `sub8_leaf_chroma422` rect path at `decode.rs:4625`):

```
OUR_PRED x=116 y=160 plane=1 side=4 side=4 mode=0 ad=0 ft=0 sum=2912 row0=[182,182,182,182]
```

and an `EC_SEEDTRACE` call-site print I added temporarily (reverted) resolved the
exact producer: **`decode.rs:26026`, the `skip` arm of `sub8_leaf_chroma422`**.
Our own block structure at the seed (`EC_AV1_TRACE`, our quarter-mi grid, `MI=4`):

```
TRACE partition_w8  mi=(40,58) ctx=0 value=1     <- 8x8 luma group at luma x=232
TRACE sub8 skip     mi=(40,58) ctx=1 value=1     <- leaf 1, SKIP, luma 232-239 y160-163
TRACE sub8 skip     mi=(41,58) ctx=1 value=0     <- leaf 2,      luma 232-239 y164-167
TRACE partition_w8  mi=(40,60) ctx=2 value=1     <- 8x8 luma group at luma x=240
TRACE sub8 skip     mi=(40,60) ctx=1 value=0
TRACE sub8 y_mode   mi=(40,60) value=12
TRACE sub8 skip     mi=(41,60) ctx=0 value=0
TRACE sub8 y_mode   mi=(41,60) value=0
```

i.e. two `BLOCK_8X8` groups each split `HORZ` into two `BLOCK_8X4` leaves, whose
4:2:2 chroma is two stacked 4x4 units at chroma x=116 and x=120. Our
`sub8_leaf_chroma422` anchor is
`((lmi.1 & !1) * MI) >> ss_x` (`decode.rs:25969`) = `(58*4)>>1` = **116**, which
matches libaom — see §5b.

---

## 5. The availability decision: NAMED OUR LINE, and REFUTED as the writer

### 5a. The line, and the libaom expression it must match

**Our line is the intra edge-availability gate, not the DC arithmetic:**

* `crates/ec-av1/src/decode.rs:21076` — `PlaneBuf::edges`, the square path
* `crates/ec-av1/src/decode.rs:21115` — `PlaneBuf::edges_rect`, the rect path

```rust
let left = (x > self.tile_x0 && down > y).then(|| …);
```

The DC arithmetic is **not** at fault and needs no change: `intra::dc`
(`crates/ec-av1/src/intra.rs:683-726`) already implements all four AV1 variants
correctly — `(None, Some) => average(l, bh)` is `dc_left`, `(Some, None) =>
average(a, bw)` is `dc_top`, `(Some, Some)` is `dc` / `dc_predictor_rect` with
libaom's own `dc_rect_multiplier` derivation (`intra.rs:732-745`), and
`(None, None) => 1 << (bit_depth - 1)` is `dc_128`. The seed's 182 is exactly
`(Some, Some)`.

**The libaom expression our gate does not implement** (`reconintra.c:1744-1747`,
consuming `set_mi_row_col`, `av1_common_int.h:1367-1379`):

```c
const int have_top  = row_off || (ss_y ? xd->chroma_up_available : xd->up_available);
const int have_left = col_off || (ss_x ? xd->chroma_left_available : xd->left_available);
…
xd->left_available          = (mi_col > tile->mi_col_start);
xd->chroma_up_available     = xd->up_available;
xd->chroma_left_available   = xd->left_available;
if (ss_x && bw < mi_size_wide[BLOCK_8X8]) xd->chroma_left_available = (mi_col - 1) > tile->mi_col_start;
if (ss_y && bh < mi_size_high[BLOCK_8X8]) xd->chroma_up_available   = (mi_row - 1) > tile->mi_row_start;
```

Our gate is missing **three** terms libaom has: the `col_off` term, the
per-plane `ss_x`/`ss_y` selection between `chroma_left_available` and
`left_available`, and the `bw < mi_size_wide[BLOCK_8X8]` narrowing clause.
(`mi_size_wide[]` is in **4x4 units** — `common_data.h:35` — so
`mi_size_wide[BLOCK_8X8] == 2` and the clause fires only for a 4-luma-px-wide
block, `bw == 1`.) At 4:2:2 `ss_y == 0`, so libaom's `chroma_up_available` is
plain `up_available` and its `ss_y` clause is dead — the prior report's reading
of that clause is confirmed, but it is not where the answer is.

### 5b. Why the seed's `chroma_left_available` cannot be 0 — and the oracle prints nothing

For the seed's `BLOCK_8X4` leaf: `bw = mi_size_wide[BLOCK_8X4] = 2`, so
`bw < 2` is **false** and `chroma_left_available = left_available = (mi_col=29 > 0)
= 1`. `n_left_px = have_left ? AOMMIN(txhpx, yd + txhpx) : 0` = 4. libaom must
therefore take `dc`, i.e. **182** — yet the oracle's pixels are 202. So the
oracle's chroma at (116,160) was **not** produced by that leaf's DC path.

Two independent measurements locate why, and neither is the availability gate:

1. **libaom's own rung proves it.** `av1_predict_intra_block`'s only early return
   before the prediction rungs is the palette write (`reconintra.c:1717-1739`,
   `if (use_palette) { …; return; }`). The oracle's `EC_PREDOUT8` ladder has
   **no entry at `mi_col=29` anywhere in the frame**, and **no plane-1 entry
   between `mi_col=24` and `mi_col=32` at `mi_row=20`** — so chroma cols
   116–127 at rows 160+ were written on a path that prints nothing. Note the
   prior lane's inference "`libaom` used `dc_top`" is an **inference from the
   value 202**, not a printed flag: `EC_PREDOUT8` (the DC path,
   `reconintra.c:1798-1807`) prints `mi_row/mi_col/plane/row_off/col_off/
   txw/txh/mode/sum/row0/col0` and **not** `have_left`/`n_left`. Only the
   *directional* `EC_PRED` rung (`reconintra.c:1863-1871`) prints
   `have_top/have_left/n_top/n_left/n_tr/n_bl/bsize`. So the oracle's DC
   availability at the seed is **unobservable with the current rungs**, and
   "`dc_top`" is a hypothesis, not a measurement.
2. **Palette is excluded for this block, and chroma-palette is excluded by
   parity.** Our `read_intra_mode_sub8` (`decode.rs:24955`) reads no palette, and
   that is correct: `av1_allow_palette` needs both block dims ≥ 8, and a
   `BLOCK_8X4` leaf has height 4. The oracle's own `EC_PALSYN_AOM` ladder
   (`decodemv.c:607-610`) has 299 entries, **every one at an even `mi_col`**,
   and **none at `mi_row=20`** — consistent with palette being legal only on
   ≥ 8x8 blocks at even `mi_col`, i.e. it cannot be the writer at `mi_col=29`.

**The 202 therefore comes from a block whose chroma plane block `mi_col` is
0 or 1, or from a unit shape I have not identified — and the current oracle
rungs cannot distinguish those.** The oracle's `EC_TRACE` partition ladder
(`decodeframe.c:1320-1331`) is also incomplete: it has 346 `EC_PART_VAL` entries
and **no root at all covering `mi_row=20, mi_col=24..31`**, which is the seed's
luma, so the oracle's block structure cannot be read off it either. This is the
documented `av1-trace-label-mismatch-class` / label-space trap, and I am not
going to guess past it.

### 5c. Two mutation probes — the availability/anchor hypotheses are REFUTED

Both probes were env-gated, built, and measured across all six cells, then
reverted.

**Probe 1 — deny the left edge to every 4:2:2 sub-8 chroma prediction**
(`EC_422_DENY_LEFT=1`, a depth counter consulted by both `edges` and
`edges_rect` and armed around `sub8_leaf_chroma422`'s three arms):

| cell | baseline | with `EC_422_DENY_LEFT=1` |
|---|---|---|
| `Q_odd320x242` | 0/17 | **0/17 (not fixed)** |
| `O_odd322x242` | 0/17 | **0/17 (not fixed)** |
| `S_odd326x242_10b` | 0/17 | **0/17 (not fixed)** |
| `AB_inter_warp_odd` | 0/43 | **0/43 (not fixed)** |
| `V_tile2x2_odd` | **17/17 EXACT** | **0/17 — BROKEN** |
| `R_odd322x240` | **17/17 EXACT** | **0/17 — BROKEN** |

The seed moves `U(116,160)` 182 → 128 (i.e. `dc_128`, both edges denied) and
`U(120,160)` 182 → 202, so the probe does reach the seed — but it fixes nothing
and destroys the two byte-exact controls. **A blanket per-arm availability
denial is refuted.** (The probe is also confounded by the deferred
reconstruction queue: the guard is live only while `sub8_leaf_chroma422` is
pushing, so units executed later in the queue are unaffected. That confounder is
why this probe is reported as *refuting* rather than as a clean negative.)

**Probe 2 — libaom's real chroma anchor floor.** libaom floors the chroma
anchor only when `mi_size_wide[bsize] == 1` and `mi_col` is odd
(`setup_pred_plane`, `reconinter.h:392-395`):

```c
if (subsampling_x && (mi_col & 0x01) && (mi_size_wide[bsize] == 1)) mi_col -= 1;
const int x = (MI_SIZE * mi_col) >> subsampling_x;
```

Our `sub8_leaf_chroma422` writes `((lmi.1 & !1) * MI) >> ss_x` (`decode.rs:25969`),
which masks the **quarter-mi** grid. Because our `MI = 4` (`decode.rs:7355`),
`lmi.1 = 2 * mi_col` is **always even**, so `lmi.1 & !1` is a **no-op that never
fires**. Porting libaom's real condition (`EC_422_ANCHOR=1`: `ss_x == 1 &&
(lmi.1 / 2) % 2 == 1 && leaf_shape.0 == 4`):

| cell | baseline | with `EC_422_ANCHOR=1` |
|---|---|---|
| the four failing cells | 0/17, 0/17, 0/17, 0/43 | **unchanged (not fixed)** |
| `V_tile2x2_odd` | **17/17 EXACT** | **0/17 — BROKEN** |
| `R_odd322x240` | **17/17 EXACT** | **0/17 — BROKEN** |

So the `& !1` no-op is **load-bearing on today's material**: the blocks it
misfloors are ones whose correct anchor is *not* libaom's floor either. **The
anchor-floor hypothesis is refuted**, and the dead mask is a latent trap for
4-px-wide sub-8 leaves that no committed 4:2:2 fixture exercises.

**Net: neither candidate is the writer, and both candidate fixes are actively
harmful. I am not landing either.**

---

## 6. Class sweep — what the defect does and does not touch

Reachable decisions measured at the seed and across the wrong region
(`Q_odd320x242` frame 0, wrong 4x4 units):

| decision | measured |
|---|---|
| both stacked 4x4 units of the 8x4 leaf | **the first (row 160) is wrong on both leaves** (`x=116` and `x=120`); the second (row 164) is **byte-identical on both sides** |
| both chroma planes | **yes** — U and V both wrong, same region, same counts order |
| both DC variants | `Q`'s seed is `dc` (ours) vs above-only (oracle); other wrong units in the region are **not** flat-202 on the oracle (`x=132,y=168`: oracle 158 vs ours 123; `x=128,y=192`: oracle 148 vs ours 137), so the class is **not** confined to the `dc`/`dc_top` choice |
| directional chroma modes sharing `have_left`/`have_top` | the neighbouring wrong units include `mode=12` (SMOOTH) and `mode=2` predictions in our trace, so the class is **not** DC-only |
| luma | **0 wrong samples, every decode frame, all four cells** — the class is chroma-only |
| post-recon filters | **0** — `PREFILT`-vs-oracle counts equal `FINAL`-vs-oracle counts exactly (`Q` 422+379 = 801 both ways), re-confirming `lanes/av1422llodd` §2 |
| 4:2:0 / 4:4:4 | **0** — `lanes/av1422h242map` §4's 4:2:0 control at width 320 is exact at every height 232–252 but 236, and `V`/`R` here are exact at the same geometries |
| entropy parse | **in sync** — luma byte-exact on every frame of all four cells |

---

## 7. Gate-ability: NO. Stated plainly, not substituted.

4:2:2 is refused at the **sequence header** —
`crates/ec-av1/src/stream.rs:1803`, `if seq.subsampling_x != seq.subsampling_y`.
**No committed fixture and no committed test can reach this code.** Every
measurement in this report required patching that guard out; the guard is
restored on this lane and `git status` is clean.

What a future lane that lifts the refusal would need, now measured and
reproducible:

* `R422_320x242.obu`, 20 733 B, `sha256 909e58db18fd4b7590501e2cecfbc090cde49eb26c9a42e15b1ef1fc265843d8`
  — **and it now has a recipe** (§2a/§2b), which the previous round lacked.
  This is the first 4:2:2 artifact in this line of work that is both committed-able
  and buildable from scratch.
* A per-decode-frame, per-plane byte-exactness gate (the comparator is
  `planecmp.py`, §3) plus a mutation proof.
* **An oracle rung that prints `have_left`/`n_left`/`have_top`/`n_top` on the DC
  path.** `EC_PREDOUT8` does not, which is why this defect survived three lanes
  of label archaeology. Adding those four fields to
  `reconintra.c:1798-1807` (and to the high-bitdepth twin at `:1774-1791`) is
  the single highest-value change for closing this defect, and it is an oracle
  change, not a decoder change.

I am not offering a source-scan substitute, and I am not committing the bypass.

---

## 8. Regression

`cargo test --release -p ec-av1 --lib -- 420 422 444 lossless warp intra
--skip bitrate_target_lands_within_5_percent_over_48_frames` on the **reverted,
bypass-free, probe-free** lane tree at `a30aca9e`, with
`EC_AV1_AOMDEC=/home/tahinli/.cache/aom-oracle/build/aomdec` and
`CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1422ctrigger`:

**`test result: ok. 181 passed; 0 failed; 2 ignored; 0 measured; 618 filtered out; finished in 232.88s`**

That is the **same count as `lanes/av1422llodd` §8's baseline** (181/0/2) on an
older tip, so nothing moved. No failure was a `/tmp`-full artefact: `/tmp` was
at 13 G of 16 G free with `/tmp/ec-av1-*` cleared before the run, and the run
reported **0 failed** on the first attempt — no red-then-green sequence to
explain.

This lane changed no source, so this is a **baseline confirmation, not
fix-verification**. `/tmp` was checked first (13 G of 16 G free,
`/tmp/ec-av1-*` cleared) so that no gate can fail with
`Os { code: 122, kind: QuotaExceeded }` — the class that reads exactly like a
red gate (`local-edquot-rust-build-tests`).

---

## 9. Handover for the next lane

1. **Add `have_top`/`have_left`/`n_top_px`/`n_left_px` to the oracle's DC-path
   rung** (`reconintra.c:1798`, 8-bit, and `:1774`, high-bitdepth). Everything
   else in this defect is currently blocked on that one blind spot: three lanes
   have now inferred the oracle's DC availability from the predicted *value*
   instead of reading it.
2. **Make the oracle's partition ladder complete.** `EC_TRACE`'s 346
   `EC_PART_VAL` entries leave `mi_row=20, mi_col=24..31` — the seed's luma —
   with no root, so the oracle's own block structure at the seed is unreadable.
   Find out why `read_partition` is not called for that region.
3. **The reproducer is now cheap: re-run the §2b command and diff frame 0.**
   Any candidate fix must take `MYREPRO.obu` from 0/17 to 17/17 **while keeping
   `V_tile2x2_odd` and `R_odd322x240` at 17/17** — those two are the control
   that killed both of my candidate fixes, and they are the only two cells that
   will catch an over-broad availability or anchor change.
4. **Do not re-chase**: the header fields (§1), the post-recon filters (§6), the
   entropy parse, the palette colour maps, `intra::dc`'s arithmetic (§5a), and
   odd/even geometry (all 42 of `lanes/av1422h242map`'s sweep).
5. **The seed's real writer is still unnamed.** It is a chroma write on the
   oracle's side that emits no prediction rung, at a luma `mi_col` where our
   leaf is 8 px wide and libaom's `chroma_left_available` is provably 1. Either
   the oracle's block structure differs from ours in a way the ladders cannot
   currently express, or the value is a residual rather than a prediction. The
   rung in (1) plus the ladder fix in (2) decide between those two.

---

## 10. Handover hygiene

* Lane tree `/home/tahinli/.cache/wt/av1422ctrigger` on `lane-av1422ctrigger`:
  **clean** apart from this report. The bypass probe, the `EC_AV1_FHDUMP`
  frame-header dump, the `EC_SEEDTRACE` call-site print, the `EC_422_DENY_LEFT`
  availability probe and the `EC_422_ANCHOR` anchor probe are **all reverted**
  (`git status --porcelain` empty; `stream.rs:1803` reads
  `if seq.subsampling_x != seq.subsampling_y {`).
* Primary checkout `/home/tahinli/Documents/Code/Rust/edith_codecs`:
  `git status --porcelain` **empty**. No relative-path leak. All edits used
  absolute paths inside the worktree.
* No push, no merge, no rustfmt. The 4:2:2 sequence-header bypass is **not**
  committed anywhere. No `4:2:2` fixture is committed (the refusal makes it
  unreachable, §7).
* Measurement scripts and the reproducer cell live outside the repo, in
  `/home/tahinli/.cache/lane-av1422ctrigger/` (`mkenc.sh`, `planecmp.py`,
  `census.sh`, `src/testsrc2_320x242.y4m`, `sweep/`).
