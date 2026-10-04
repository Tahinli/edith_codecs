# lane-av1muchunk422div — where `422_muchunk_compound_128root.obu` diverges

Base: `main` @ `efefb050`. Branch: `lane/av1muchunk422div`.

**Verdict: the divergence is the 128-root COMPOUND `side > 64` mu-chunk walk's
**last mu chunk** (`cr = 1, cc = 1`) — the very arm this fixture is pinned to
prove (`MU_CHUNK_COMPOUND_UNITS`, `decode.rs` 43996 / 45799). The walk's own
addressing is verified correct; the defect is in that chunk's chroma
coefficient stream (sign/context level), not in the square-vs-per-axis stride
the `lane-av1422lpf` fix touched. NO FIX APPLIED (see Disposition).**

Fixture: `crates/ec-av1/fixtures/422_muchunk_compound_128root.obu`, 895 bytes,
sha256 `9942bae279a60ba6fd4d35ba55469271317b655d5ea03158125deb4428e26ae7` (=
the campaign stream `~/.cache/av1muchunk422b/i8_tsrc128_iis.obu`), 128x128,
4:2:2 (ss 1,0), one 128x128 superblock, whole frame = one inter block per
frame, 6 shown frames + 1 hidden alt-ref, 7 decode-order frames.

## 1. Diff counts, per plane per frame

Decode order, ours (`do.fN`, post-loop-filter) vs the instrumented oracle
(`~/.cache/aom-oracle/build/aomdec`, `EC_AV1_FINAL_DUMP`, decode order):

| decode frame | 0 | 1 | 2 | 3 | 4 | 5 | 6 (hidden) |
|---|---|---|---|---|---|---|---|
| Y | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| U | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| V | 0 | 0 | **2138** | **2063** | **2105** | **1988** | 0 |

Display order, ours (`dump_yuv`) vs `ffmpeg -i X.obu -pix_fmt yuv422p -f
rawvideo` (6 shown frames): frame 0 exact, 1 V=2063, 2 V=2138, 3 V=2105,
4 V=1988, 5 exact. Same numbers: **V only; Y and U bit-exact everywhere.**

Display→decode map (measured, dump match): 0→0, 1→3, 2→2, 3→4, 4→5, 5→1,
6 = hidden.

## 2. The first wrong sample and the block that owns it

* **First sample that differs from the oracle, decode frame 2 (= display
  frame 2), plane V: chroma `(x=36, y=60)`, ours 113 vs oracle 112.**
  Luma `(72, 60)` → **mi `(row 15, col 18)`** — the *last mi row of the
  top-right mu chunk* (`cr = 0, cc = 1`), luma rows 60..63.
* The wrong region as a whole: V chroma `cols 29..63 × rows 60..127`
  (= luma `x 58..127 × y 60..127`). Two parts:
  * **intrinsic**: chroma `x 32..63 × y 64..127` = luma `x 64..127 × y 64..127`
    = **mi rows 16..31 × cols 16..31 = the mu chunk `(cr = 1, cc = 1)`**, the
    last chunk of the walk (all 2048 samples wrong on frames 2 and 4);
  * **bleed**: V rows 60..63 and cols 29..31 (the 4-row/3-col deblock band
    around the chunk seam, values within ±4) — deblocking of the seam fed by
    the wrong chunk below it.

## 3. Which arm owns it — measured, not assumed

* Temporary env-gated probe at both compound/single-reference mu-chunk
  allocation sites (`EC_AV1_MUDBG`, since removed; tree reverted) prints
  `pict= site= plane= chunk= unit= at_mi= cu= dst_start= chroma_side=
  chroma_stride= res_sum= res_nz=`. Result:
  * compound copy (`is_compound` arm, site 44009-44010) fires for
    **decode frames 2 and 4 only** — exactly the two intrinsically wrong
    frames — 16 units each (4 chunks × 2 planes × 2 unit rows);
  * the single-reference twin (site 45830-45832) fires for **decode frame 1
    only**, which is **bit-exact**;
  * every unit's geometry is as designed for 4:2:2: chunk chroma
    `32 x 64` (`units_w=1, units_h=2`), `cu=(32,64)`/`(32,96)` for chunk
    `(1,1)` V, `dst_start=8224/12320`, `chroma_side=128` (grid stride),
    `chroma_stride=64` (plane/prediction stride), `chroma_buf_h=128`.
  ⇒ the addressing is **correct**; the walk writes exactly the region that is
  wrong, but it is not writing it into the wrong place.
* Decode frames 3 and 5 run **neither** walk and emit **no** coefficient units
  at all (skip blocks; the `if skip` arm pushes `ZERO_RESIDUAL`) — their V
  region is wrong by **inheritance**: they are compound SKIP blocks
  (`NEAREST_NEARESTMV`, mv (0,0), `skip=1` per the oracle's `AOMMB` rung), so
  their prediction is a straight copy of the already-wrong references.
* Pre-loop-filter dump (`EC_AV1_PREFILT_DUMP`) vs our post dump: inside
  chunk `(1,1)` the samples whose loop filter was the identity are wrong
  anyway (decode 2: 294 of 297; decode 4: 411 of 429) ⇒ **the divergence is in
  reconstruction, not in CDEF/LR/deblock** (the bleed band is deblock only).
* `EC_CPRED` at the chunk `(1,1)` unit origin: our V **and** U prediction are
  `mean|diff| = 0.00` against reference frame 0's plane at the *same*
  coordinates — i.e. the prediction (identity, no subpixel) is not the
  divergence.
* Per-unit dequantised-coefficient pairing against the oracle's
  `EC_DQCOEFF` (`OUR_DQ`, unit-for-unit, column-major→row/col mapped):
  identical for the whole stream up to decode frame 2's last two chroma
  units, where they part company — the penultimate unit has the *same
  positions and magnitudes with different signs* (e.g. ours `(1,0) = -725` vs
  oracle `+725`, ours `(1,1) = +725` vs oracle `-725`, `nz` equal at 30), and
  the last chroma unit of frame 2 exists on our side and not on the oracle's
  (46 vs 45 non-zero unit dumps over the stream). A sign-level divergence at
  fixed magnitudes and positions is a **coefficient-context** divergence, not
  a quantiser or geometry one.

**Conclusion.** The first wrong sample and the whole intrinsic region belong
to the **compound `side > 64` mu-chunk walk's last chunk**
(`decode_inter_block`, the `is_compound` copy, `decode.rs` 43908-44023 — the
counter site is 43996; the single-reference twin at 45649-45849 is NOT
implicated and is bit-exact). The chroma coefficient stream for that chunk
diverges at sign/context level, and because a mu chunk's chroma units are the
**last** coefficient reads of the block, the divergence changes exactly those
coefficients and desyncs nothing observable after them — which is why Y and U
(and every earlier chunk of V) stay bit-exact while only the last chunk's V
moves.

## 4. Disposition — no fix, for cause

The charter's fix gate was: *fix only if the site is that walk **and**
restoring the old square reds a new gate.* The site is that walk, but the
square→per-axis stride replacement (`lane-av1422lpf` sites 2/5, `chroma_side`
→ `chroma_stride`) is **verified not to be the defect**: the probe shows every
4:2:2 mu-chunk unit already addresses the prediction/grid at the right stride
and offset, and restoring the square would re-introduce the measured OOB the
lpf lane closed (`range end index 68 out of range for slice of length 64`,
`W_intrabc.obu` frame 1) without touching this divergence. So the square
restore is not the fix and no change was made.

The remaining localization (which context field of the last chunk's V unit —
`txb_skip` offset, `dc_sign`, or the per-plane neighbour state recorded by the
chunk above/beside) needs a paired entropy-symbol ladder
(`EC_ECDUMP`/`EC_TRACE_COEFF` ours vs the instrumented oracle) — i.e. a fix
lane, not this diagnostic one. Note the walk's chroma gather
(`neighbours.around_mi_rect(unit_mi, unit_luma_w, unit_luma_h)`, written for
the unit's true 64x32 luma footprint) differs from the byte-exact intra
twin's (`around_mi(unit_mi, unit_luma_w)`, square 64x64) and from the
square-block arm's 4:2:2 rule (`around_mi_422_chroma`, "one chroma column per
pair of luma mi cells", `decode.rs` 43684-43701) — the first thing a fix lane
should compare.

The pinned gate
`a_422_muchunk_compound_128root_pinned_stream_reaches_its_unit_walk` is
untouched (counter witness, deliberately no oracle compare).

## 5. Repro

```text
git worktree add -b lane/av1muchunk422div ~/.cache/wt/av1muchunk422div efefb050
cd ~/.cache/wt/av1muchunk422div
CARGO_TARGET_DIR=~/.cache/tgt-av1muchunk422div cargo build -p ec-av1 --example dump_yuv
$B crates/ec-av1/fixtures/422_muchunk_compound_128root.obu /tmp/ours      # display order
ffmpeg -v error -i crates/ec-av1/fixtures/422_muchunk_compound_128root.obu \
  -pix_fmt yuv422p -f rawvideo /tmp/ref.yuv                              # 6 shown frames
EC_AV1_DECODE_ORDER_DUMP=/tmp/do $B X.obu /tmp/x                         # decode order, post-filter
EC_AV1_PREFILT_DUMP=/tmp/pre $B X.obu /tmp/x                             # decode order, pre-filter
EC_AV1_FINAL_DUMP=/tmp/aom/a ~/.cache/aom-oracle/build/aomdec --codec=av1 -o /tmp/o.y4m X.obu
# ours, decode order, post-filter vs the oracle's decode-order dump:
#   f0,f1,f6 exact; f2 V=2138  f3 V=2063  f4 V=2105  f5 V=1988  (Y,U exact everywhere)
```
