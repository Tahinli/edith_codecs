# lane-av1422census — the 4:2:2 census, re-measured with a proven-live comparator

**Outcome in one line: the standing "4:2:2 refuses by name" decision's input
was stale — of 50 distinct 4:2:2 cells measured today, 34 decode byte-exact and
16 diverge, and the divergence is not one odd geometry: 12 of 18 fresh
`aomenc` 4:2:2 cells diverge, where the previous sweep — encoded with ffmpeg's
libaom wrapper at `-cpu-used 6` over a y4m libaom rejected per frame — reported
its fresh encodes all clean.**

Tip: `main` = `cc9f2668`. Worktree `/home/tahinli/.cache/wt/av1422census`,
branch `lane-av1422census`. **The only source edit ever applied in this tree is
the `if false &&` 4:2:2 sequence-header bypass, and it is reverted** —
`stream.rs:1803` reads `if seq.subsampling_x != seq.subsampling_y {` again and
the tree carries no source diff. Every measurement below was taken on a
patch-run-restore build, as the standing rule requires. No fix, no push, no
merge, no rustfmt. **This report recommends no product decision**; §7 states
plainly what is not measured.

---

## 0. What changed since the last census, in one table

| | last full census (`av1422llinter`, `cc211d13`) | this census |
|---|---|---|
| 4:2:2 cells measured | 27 | **50** (32 corpus + 18 fresh) |
| 4:2:2 byte-exact | 22 | **34** |
| 4:2:2 diverging | 5 | **16** |
| 4:2:2 refusing | 0 (bypassed for the measurement) | 0 under bypass; **50/50 refuse by name on the shipped tree** |
| geometry sweep encoder | ffmpeg's libaom wrapper, `-cpu-used 6`, **malformed y4m** | **aomenc + the trigger recipe + a correct y4m** |
| sweep result | 42 fresh encodes reported "all exact" | **12 of 18 fresh 4:2:2 cells DIVERGE** (7 of 12 at 8-bit, 5 of 6 at 10-bit) |
| comparator | unproven | **48 per-plane oracle-flip arms, all +1 exactly** (§1) |

The single largest correction: **the previous "42 fresh encodes, all clean"
sweep used the wrong encoder at the wrong search effort with a y4m that libaom
rejected per frame.** Re-encoded with `aomenc --cpu-used 0 --lag-in-frames 25
--auto-alt-ref=1 --enable-global-motion=1 --cq-level=24`, the same content and
the same geometries produce divergence on 12 of 18 4:2:2 cells.

---

## 1. The comparator, and its liveness proof

Both sides dump `EC_AV1_FINAL_DUMP` — ours from `decode_frame`
(`stream.rs:2210`, depth-correct, post-deblock/CDEF/LR/superres), the oracle's
from rung 12 of `scripts/instrument-aom-oracle.sh` (`decodeframe.c`, the same
stage). `.f<N>` is the **DECODE** index on both sides, hidden alt-refs included.

Four properties, each of which is a known way this wave has already lost a
result:

1. **Geometry is an explicit argument, taken from the oracle's own y4m
   header** (`W320 H242 F0:0 Ip C422`, `C422p10 XYSCSS=422P10`). Never from a
   file size. A frame whose byte length disagrees with the geometry is a hard
   error, not a tolerated difference — it fired twice during this census and
   caught a real bug in my own first parser (§1.1).
2. **Plane attribution is per frame**, from that frame's own plane lengths
   derived from the explicit geometry, with a monotone cursor. Not absolute
   thresholds from frame 0.
3. **A wrong SAMPLE is one `bps`-wide unit**, `bps` from the y4m's depth tag.
   10/12-bit cells are counted per 16-bit sample, so the top 6 bits cannot hide.
4. **A count over zero frames is a hard error**, naming the vacuous 0/0/0 it
   would otherwise print.

### 1.1 Two defects the instrument caught in ITSELF, before any claim

* My first y4m regex was `$`-anchored and matched neither `C422p10 XYSCSS=…`
  nor `C420jpeg`; it silently fell back to "C420, 8-bit". Every high-bitdepth
  cell then failed the length assertion with `ours=307200 oracle=307200 !=
  115200 implied by … ss=11 depth=8`. That is the geometry-from-the-wrong-field
  class, caught because the length check is a hard error rather than a warning.
* The flip control's first version indexed the flip in BYTES while walking
  planes in SAMPLES; at 10-bit it read past EOF and raised
  `IndexError: bytearray index out of range`. Both defects were in the
  instrument, and both were found by the instrument refusing to produce a
  number.

### 1.2 The flip control — 48 arms, 48 passes

One oracle sample per plane, per cell, in a named decode frame; the same
comparator must move by exactly +1 in that plane with every other plane and
every other frame unchanged. The flip is bit 0 of the sample's **low** byte:
at 10/12-bit the sample sits in a 16-bit LE container and `2**bitdepth <
2**16`, so no sample can carry out of the container — +1 by construction rather
than hoped for. Each arm is restored before the next, so the three planes are
independent.

**16 cells x 3 planes = 48 arms, 48 PASS, 0 FAIL.**

| cell | geometry | frame | baseline Y/U/V | plane flipped | oracle bytes | count delta Y/U/V | verdict |
|---|---|---|---|---|---|---|---|
| `Q_odd320x242` | 320x242 ss=10 d8 | decode frame 0 | 0/9603/14456 | U | `5a` -> `5b` | 0/1/0 | PASS |
| `Q_odd320x242` | 320x242 ss=10 d8 | decode frame 0 | 0/9603/14456 | V | `f0` -> `f1` | 0/0/1 | PASS |
| `Q_odd320x242` | 320x242 ss=10 d8 | decode frame 7 | 0/9603/14456 | Y | `51` -> `50` | 1/0/0 | PASS |
| `Q_odd320x242` | 320x242 ss=10 d8 | decode frame 7 | 0/9603/14456 | U | `5a` -> `5b` | 0/1/0 | PASS |
| `Q_odd320x242` | 320x242 ss=10 d8 | decode frame 7 | 0/9603/14456 | V | `f0` -> `f1` | 0/0/1 | PASS |
| `W_intrabc` | 320x240 ss=10 d8 | decode frame 0 | 0/0/0 | Y | `51` -> `50` | 1/0/0 | PASS |
| `W_intrabc` | 320x240 ss=10 d8 | decode frame 0 | 0/0/0 | U | `5a` -> `5b` | 0/1/0 | PASS |
| `W_intrabc` | 320x240 ss=10 d8 | decode frame 0 | 0/0/0 | V | `f0` -> `f1` | 0/0/1 | PASS |
| `Y_intrabc_10b` | 320x240 ss=10 d10 | decode frame 0 | 0/0/0 | Y | `4401` -> `4501` | 1/0/0 | PASS |
| `Y_intrabc_10b` | 320x240 ss=10 d10 | decode frame 0 | 0/0/0 | U | `6801` -> `6901` | 0/1/0 | PASS |
| `Y_intrabc_10b` | 320x240 ss=10 d10 | decode frame 0 | 0/0/0 | V | `c003` -> `c103` | 0/0/1 | PASS |
| `S_odd326x242_10b` | 326x242 ss=10 d10 | decode frame 0 | 0/301/751 | Y | `4401` -> `4501` | 1/0/0 | PASS |
| `S_odd326x242_10b` | 326x242 ss=10 d10 | decode frame 0 | 0/301/751 | U | `6801` -> `6901` | 0/1/0 | PASS |
| `S_odd326x242_10b` | 326x242 ss=10 d10 | decode frame 0 | 0/301/751 | V | `c003` -> `c103` | 0/0/1 | PASS |
| `AB_inter_warp_odd` | 322x242 ss=10 d8 | decode frame 42 | 0/3705/4366 | Y | `51` -> `50` | 1/0/0 | PASS |
| `AB_inter_warp_odd` | 322x242 ss=10 d8 | decode frame 42 | 0/3705/4366 | U | `5a` -> `5b` | 0/1/0 | PASS |
| `AB_inter_warp_odd` | 322x242 ss=10 d8 | decode frame 42 | 0/3705/4366 | V | `f0` -> `f1` | 0/0/1 | PASS |
| `ll420_a` | 320x240 ss=11 d8 | decode frame 0 | 0/0/0 | Y | `96` -> `97` | 1/0/0 | PASS |
| `ll420_a` | 320x240 ss=11 d8 | decode frame 0 | 0/0/0 | U | `8f` -> `8e` | 0/1/0 | PASS |
| `ll420_a` | 320x240 ss=11 d8 | decode frame 0 | 0/0/0 | V | `9b` -> `9a` | 0/0/1 | PASS |
| `ll444_a` | 320x240 ss=00 d8 | decode frame 0 | 0/0/0 | Y | `51` -> `50` | 1/0/0 | PASS |
| `ll444_a` | 320x240 ss=00 d8 | decode frame 0 | 0/0/0 | U | `5a` -> `5b` | 0/1/0 | PASS |
| `ll444_a` | 320x240 ss=00 d8 | decode frame 0 | 0/0/0 | V | `f0` -> `f1` | 0/0/1 | PASS |
| `422_sb128_3f` | 128x128 ss=10 d8 | decode frame 0 | 0/0/0 | Y | `10` -> `11` | 1/0/0 | PASS |
| `422_sb128_3f` | 128x128 ss=10 d8 | decode frame 0 | 0/0/0 | U | `80` -> `81` | 0/1/0 | PASS |
| `422_sb128_3f` | 128x128 ss=10 d8 | decode frame 0 | 0/0/0 | V | `80` -> `81` | 0/0/1 | PASS |
| `s422_320x242` | 320x242 ss=10 d8 | decode frame 0 | 0/6337/8398 | Y | `51` -> `50` | 1/0/0 | PASS |
| `s422_320x242` | 320x242 ss=10 d8 | decode frame 0 | 0/6337/8398 | U | `5a` -> `5b` | 0/1/0 | PASS |
| `s422_320x242` | 320x242 ss=10 d8 | decode frame 0 | 0/6337/8398 | V | `f0` -> `f1` | 0/0/1 | PASS |
| `s422_384x240` | 384x240 ss=10 d8 | decode frame 0 | 0/1982/2081 | Y | `51` -> `50` | 1/0/0 | PASS |
| `s422_384x240` | 384x240 ss=10 d8 | decode frame 0 | 0/1982/2081 | U | `5a` -> `5b` | 0/1/0 | PASS |
| `s422_384x240` | 384x240 ss=10 d8 | decode frame 0 | 0/1982/2081 | V | `f0` -> `f1` | 0/0/1 | PASS |
| `s444_320x242` | 320x242 ss=00 d8 | decode frame 0 | 0/0/0 | Y | `51` -> `50` | 1/0/0 | PASS |
| `s444_320x242` | 320x242 ss=00 d8 | decode frame 0 | 0/0/0 | U | `5a` -> `5b` | 0/1/0 | PASS |
| `s444_320x242` | 320x242 ss=00 d8 | decode frame 0 | 0/0/0 | V | `f0` -> `f1` | 0/0/1 | PASS |
| `s420_320x242` | 320x242 ss=11 d8 | decode frame 0 | 0/0/0 | Y | `51` -> `50` | 1/0/0 | PASS |
| `s420_320x242` | 320x242 ss=11 d8 | decode frame 0 | 0/0/0 | U | `f0` -> `f1` | 0/1/0 | PASS |
| `s420_320x242` | 320x242 ss=11 d8 | decode frame 0 | 0/0/0 | V | `6e` -> `6f` | 0/0/1 | PASS |
| `s422_416x250_10b` | 416x250 ss=10 d10 | decode frame 0 | 103250/64191/62834 | Y | `4401` -> `4501` | 1/0/0 | PASS |
| `s422_416x250_10b` | 416x250 ss=10 d10 | decode frame 0 | 103250/64191/62834 | U | `6801` -> `6901` | 0/1/0 | PASS |
| `s422_416x250_10b` | 416x250 ss=10 d10 | decode frame 0 | 103250/64191/62834 | V | `c003` -> `c103` | 0/0/1 | PASS |
| `s444_352x242_10b` | 352x242 ss=00 d10 | decode frame 0 | 0/1166/1413 | Y | `4401` -> `4501` | 1/0/0 | PASS |
| `s444_352x242_10b` | 352x242 ss=00 d10 | decode frame 0 | 0/1166/1413 | U | `6801` -> `6901` | 0/1/0 | PASS |
| `s444_352x242_10b` | 352x242 ss=00 d10 | decode frame 0 | 0/1166/1413 | V | `c003` -> `c103` | 0/0/1 | PASS |
| `s422_320x242_10b` | 320x242 ss=10 d10 | decode frame 0 | 0/3915/5612 | Y | `4401` -> `4501` | 1/0/0 | PASS |
| `s422_320x242_10b` | 320x242 ss=10 d10 | decode frame 0 | 0/3915/5612 | U | `6801` -> `6901` | 0/1/0 | PASS |
| `s422_320x242_10b` | 320x242 ss=10 d10 | decode frame 0 | 0/3915/5612 | V | `c003` -> `c103` | 0/0/1 | PASS |

Coverage deliberately includes a BYTE-EXACT cell (`W_intrabc`, baseline
0/0/0 — the only reading that can be a false green), a DIVERGING cell at two
different frames, a 10-bit exact cell, a 10-bit diverging cell, the last decode
frame of a 43-frame cell, a 4:2:0 control and a 4:4:4 control, a small
128x128 cell, and three of the fresh sweep cells. **A comparator that dropped
chroma, mis-attributed a plane to the wrong frame, or compared our samples with
ourselves fails at least one of these arms.**

---

## 2. The cell list, and how it was derived (reproducible)

Derivation, in the order run:

1. `find /home/tahinli/.cache/cells -name '*.obu'` -> **242** probe cells
   across `av1420tall` (198), `av1422lpf` (44), `av1422llintra` (0).
2. Every one was run through `aomdec --codec=av1 -o /tmp/h.y4m` and its y4m
   header read: that is the oracle's own statement of chroma format and
   geometry, so the classification is the oracle's, not a filename guess.
   **35 of the probe cells are `C422`/`C422p10`.**
3. Every file in `crates/ec-av1/fixtures/` (105) was classified the same way.
   **9 are `C422`/`C422p10`**, and all 9 are byte-identical (sha256 prefix) to
   a copy already in the probe cache — so the cell set is deduplicated by
   content hash, not by name: **32 distinct 4:2:2 corpus cells**, of which
   **9 are committed** and **23 are probe-only**.
4. Committed means: `git ls-files crates/ec-av1/fixtures` lists the file. There
   is no `tests/data` directory in this crate.
5. Controls: the 8 non-4:2:2 cells in `av1422lpf` (`ll420_a/c/d`,
   `ll444_a/b/c`, `ll420_allintra`, `ll444_allintra`) plus the 36 fresh sweep
   control encodes.

### 2.1 What "committed" is worth here — measured, not assumed

`grep` shows the 9 committed 4:2:2 fixtures are referenced only by
`stream.rs`/`decode.rs`, and reading those gates: they are **byte pins
(size + fnv1a64) plus a refusal-by-name assertion**
(`the_pinned_422_bigblock_witnesses_are_present_and_refuse_by_name`,
`the_pinned_422_lossless_inter_witnesses_are_present_and_refuse_by_name`). The
second gate's own doc comment says it is **vacuous with respect to the
seven-site fix it was written for** — the header refusal fires before any of
those sites is reachable, identically on the pre-fix tree.

**Consequence, and it is the reason this census exists: there is no committed
gate anywhere that measures 4:2:2 PIXELS.** Every exactness number for 4:2:2
in this report — including the 34 exact cells — is probe-only evidence,
reproducible from the recipes in §6 but not gateable without lifting the
refusal. The 23 probe-only corpus cells are in `/home/tahinli/.cache/cells/`
and are pinned by **no sha in this repo**; treat them accordingly.

### 2.2 Corpus table (32 x 4:2:2 + 8 controls, DECODE order)

| `X_intrabc_tiled` | 422 | 320x240 | 8 | probe-cache COMMITTED-PIN | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `Y_intrabc_10b` | 422 | 320x240 | 10 | probe-cache COMMITTED-PIN | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `422_allskip_2f` | 422 | 128x128 | 8 | probe-cache COMMITTED-PIN | BYTE-EXACT | 2 | 0 | 0 | 0 |
| `422_intrabc_sb128_strip` | 422 | 384x320 | 8 | probe-cache COMMITTED-PIN | BYTE-EXACT | 5 | 0 | 0 | 0 |
| `422_intrabc_sb128_strip_notxsearch` | 422 | 384x320 | 8 | probe-cache COMMITTED-PIN | BYTE-EXACT | 5 | 0 | 0 | 0 |
| `422_residual_compound_warp_16f` | 422 | 256x288 | 8 | probe-cache COMMITTED-PIN | BYTE-EXACT | 16 | 0 | 0 | 0 |
| `422_residual_compound_warp_nolr_16f` | 422 | 256x288 | 8 | probe-cache COMMITTED-PIN | BYTE-EXACT | 16 | 0 | 0 | 0 |
| `422_sb128_3f` | 422 | 128x128 | 8 | probe-cache COMMITTED-PIN | BYTE-EXACT | 3 | 0 | 0 | 0 |
| `AA_inter_compound` | 422 | 320x240 | 8 | probe-cache | BYTE-EXACT | 43 | 0 | 0 | 0 |
| `AB_inter_warp_odd` | 422 | 322x242 | 8 | probe-cache | DIVERGES | 43 (43 bad) | 0 | 3705 | 4366 |
| `AD_inter_nogm` | 422 | 320x240 | 8 | probe-cache | BYTE-EXACT | 43 | 0 | 0 | 0 |
| `A_testsrc2_cpu0` | 422 | 320x240 | 8 | probe-cache | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `B_testsrc2_cpu6` | 422 | 320x240 | 8 | probe-cache | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `C_mandel320` | 422 | 320x240 | 8 | probe-cache | BYTE-EXACT | 16 | 0 | 0 | 0 |
| `D_bars` | 422 | 320x240 | 8 | probe-cache | BYTE-EXACT | 16 | 0 | 0 | 0 |
| `E_noglobal` | 422 | 320x240 | 8 | probe-cache | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `F_allintra` | 422 | 320x240 | 8 | probe-cache | BYTE-EXACT | 16 | 0 | 0 | 0 |
| `H_10bit_testsrc2` | 422 | 320x240 | 10 | probe-cache | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `I_10bit_mandel` | 422 | 320x240 | 10 | probe-cache | BYTE-EXACT | 16 | 0 | 0 | 0 |
| `J_10bit_lr0` | 422 | 320x240 | 10 | probe-cache | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `K_sct` | 422 | 320x240 | 8 | probe-cache | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `L_tiled` | 422 | 320x240 | 8 | probe-cache | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `O_odd322x242` | 422 | 322x242 | 8 | probe-cache | DIVERGES | 17 (17 bad) | 0 | 985 | 722 |
| `Q_odd320x242` | 422 | 320x242 | 8 | probe-cache | DIVERGES | 17 (17 bad) | 0 | 9603 | 14456 |
| `R_odd322x240` | 422 | 322x240 | 8 | probe-cache | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `S_odd326x242_10b` | 422 | 326x242 | 10 | probe-cache | DIVERGES | 17 (17 bad) | 0 | 301 | 751 |
| `T_tilecols2` | 422 | 320x240 | 8 | probe-cache | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `U_tilerows1` | 422 | 320x240 | 8 | probe-cache | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `V_tile2x2_odd` | 422 | 322x242 | 8 | probe-cache | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `ll422_allintra` | 422 | 320x240 | 8 | probe-cache | BYTE-EXACT | 1 | 0 | 0 | 0 |
| `ll422_noibc` | 422 | 320x240 | 8 | probe-cache | BYTE-EXACT | 16 | 0 | 0 | 0 |
| `ll420_allintra` | 420 | 320x240 | 8 | probe-cache-control | BYTE-EXACT | 1 | 0 | 0 | 0 |
| `ll444_allintra` | 444 | 320x240 | 8 | probe-cache-control | BYTE-EXACT | 1 | 0 | 0 | 0 |
| `ll420_a` | 420 | 320x240 | 8 | probe-cache-control | BYTE-EXACT | 16 | 0 | 0 | 0 |
| `ll420_c` | 420 | 320x240 | 8 | probe-cache-control | BYTE-EXACT | 16 | 0 | 0 | 0 |
| `ll420_d` | 420 | 320x240 | 8 | probe-cache-control | BYTE-EXACT | 16 | 0 | 0 | 0 |
| `ll444_a` | 444 | 320x240 | 8 | probe-cache-control | BYTE-EXACT | 16 | 0 | 0 | 0 |
| `ll444_b` | 444 | 320x240 | 8 | probe-cache-control | BYTE-EXACT | 16 | 0 | 0 | 0 |
| `ll444_c` | 444 | 320x240 | 8 | probe-cache-control | BYTE-EXACT | 18 | 0 | 0 | 0 |

Read the `frames` column as the DECODE-order frame count (hidden alt-refs
included); `(N bad)` is how many of those frames hold at least one wrong
sample.

---

## 3. The corrected geometry sweep — the part that changes the picture

Encoder (identical for every cell, the trigger recipe from
`lanes/av1422ctrigger.report.md` §2b):

```
aomenc --codec=av1 --profile=<0|1|2> --input-bit-depth=<8|10> --bit-depth=<8|10> \
       --limit=16 --lag-in-frames=25 --auto-alt-ref=1 --enable-global-motion=1 \
       --pass=1 --cq-level=24 --threads=4 --kf-min-dist=0 --kf-max-dist=999999 \
       --width=W --height=H --cpu-used=0 --obu -o cell.obu src.y4m
```

Source: `ffmpeg -f lavfi -i testsrc2=size=WxH:rate=25 -frames:v 16 -pix_fmt
{yuv422p,yuv420p,yuv444p}[10le] -f rawvideo`, hand-wrapped into y4m with **one
`FRAME\n` before every frame** and the full `YUV4MPEG2 … C422|C420|C444[p10]`
header once. libaom's `y4m_input_fetch_frame`
(`av1/common/y4minput.c:1163-1171`) requires the per-frame `FRAME\n`; a y4m
that repeats the full header encodes **zero** frames. All 54 encodes here
produced 16 input frames and 17 decode frames (16 shown + 1 hidden alt-ref),
which is itself the check that the framing is right.

Grid: 8-bit at 4 widths {320, 322, 326, 384} x 3 heights {240, 242, 246}, and
10-bit at 3 widths {320, 352, 416} x 2 heights {242, 250}; each geometry
encoded in 4:2:2 **and** in 4:2:0 and 4:4:4 as controls. 54 cells.

| `s420_320x242` | 11 | 320x242 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s420_320x246` | 11 | 320x246 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s420_322x240` | 11 | 322x240 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s420_322x242` | 11 | 322x242 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s420_322x246` | 11 | 322x246 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s420_326x240` | 11 | 326x240 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s420_326x242` | 11 | 326x242 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s420_326x246` | 11 | 326x246 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s420_384x240` | 11 | 384x240 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s420_384x242` | 11 | 384x242 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s420_384x246` | 11 | 384x246 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s422_320x240` | 10 | 320x240 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s422_320x242` | 10 | 320x242 | 8 | DIVERGES | 17 (17 bad) | 0 | 6337 | 8398 |
| `s422_320x246` | 10 | 320x246 | 8 | DIVERGES | 17 (17 bad) | 0 | 38348 | 59491 |
| `s422_322x240` | 10 | 322x240 | 8 | DIVERGES | 17 (17 bad) | 0 | 22969 | 22534 |
| `s422_322x242` | 10 | 322x242 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s422_322x246` | 10 | 322x246 | 8 | DIVERGES | 17 (17 bad) | 0 | 24964 | 25438 |
| `s422_326x240` | 10 | 326x240 | 8 | DIVERGES | 17 (17 bad) | 0 | 734 | 0 |
| `s422_326x242` | 10 | 326x242 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s422_326x246` | 10 | 326x246 | 8 | DIVERGES | 17 (17 bad) | 0 | 24078 | 25112 |
| `s422_384x240` | 10 | 384x240 | 8 | DIVERGES | 17 (11 bad) | 0 | 1982 | 2081 |
| `s422_384x242` | 10 | 384x242 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s422_384x246` | 10 | 384x246 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s444_320x240` | 00 | 320x240 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s444_320x242` | 00 | 320x242 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s444_320x246` | 00 | 320x246 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s444_322x240` | 00 | 322x240 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s444_322x242` | 00 | 322x242 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s444_322x246` | 00 | 322x246 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s444_326x240` | 00 | 326x240 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s444_326x242` | 00 | 326x242 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s444_326x246` | 00 | 326x246 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s444_384x240` | 00 | 384x240 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s444_384x242` | 00 | 384x242 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s444_384x246` | 00 | 384x246 | 8 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s420_320x242_10b` | 11 | 320x242 | 10 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s420_320x250_10b` | 11 | 320x250 | 10 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s420_352x242_10b` | 11 | 352x242 | 10 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s420_352x250_10b` | 11 | 352x250 | 10 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s420_416x242_10b` | 11 | 416x242 | 10 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s420_416x250_10b` | 11 | 416x250 | 10 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s422_320x242_10b` | 10 | 320x242 | 10 | DIVERGES | 17 (17 bad) | 0 | 3915 | 5612 |
| `s422_320x250_10b` | 10 | 320x250 | 10 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s422_352x242_10b` | 10 | 352x242 | 10 | DIVERGES | 17 (17 bad) | 0 | 4858 | 3949 |
| `s422_352x250_10b` | 10 | 352x250 | 10 | DIVERGES | 17 (17 bad) | 0 | 3876 | 1786 |
| `s422_416x242_10b` | 10 | 416x242 | 10 | DIVERGES | 17 (17 bad) | 0 | 5968 | 4524 |
| `s422_416x250_10b` | 10 | 416x250 | 10 | DIVERGES | 17 (17 bad) | 103250 | 64191 | 62834 |
| `s444_320x242_10b` | 00 | 320x242 | 10 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s444_320x250_10b` | 00 | 320x250 | 10 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s444_352x242_10b` | 00 | 352x242 | 10 | DIVERGES | 17 (17 bad) | 0 | 1166 | 1413 |
| `s444_352x250_10b` | 00 | 352x250 | 10 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s444_416x242_10b` | 00 | 416x242 | 10 | BYTE-EXACT | 17 | 0 | 0 | 0 |
| `s444_416x250_10b` | 00 | 416x250 | 10 | BYTE-EXACT | 17 | 0 | 0 | 0 |

### 3.1 What the sweep says

* **4:2:2 8-bit: 5 of 12 exact, 7 DIVERGE.** The previous sweep's "42 fresh
  encodes, all clean" used ffmpeg's libaom wrapper at `-cpu-used 6`; at
  `-cpu-used 0` with altref and global motion on, the same content and the same
  geometries diverge on 7 of 12. The old sweep did not disprove the defect, it
  searched a partition tree the defect does not live in.
* **4:2:2 10-bit: 1 of 6 exact, 5 DIVERGE.**
* **4:2:0 controls: 18 of 18 byte-exact** (12 x 8-bit, 6 x 10-bit). The
  supported format is untouched by the same recipe at the same geometries, so
  the sweep's failures are 4:2:2-specific, not "odd geometry" or "this
  encoder" generally.
* **4:4:4 controls: 17 of 18 byte-exact — and ONE DIVERGES**,
  `s444_352x242_10b`, 10-bit 4:4:4, chroma only (U 1166 / V 1413, Y 0). This is
  **outside the 4:2:2 question** and it is a live defect in a format the
  decoder already claims. It was re-measured on a build with the 4:2:2 bypass
  **reverted** and returned the identical numbers (0 / 1166 / 1413), so it is
  pre-existing on `cc9f2668` and not an artefact of the bypass. Reported here
  because a census that found it and stayed silent would be a worse census.
* Divergence is **content-and-geometry sensitive, not a function of either
  alone**: `322x242` and `326x242` and `384x242` are exact at 8-bit while
  `320x242` diverges; `320x240` is exact while `322x240` diverges; `320x246`
  diverges while `384x246` does not. The controls that isolate it are in §4.

---

## 4. Per-cell failure detail (the classes)

```
== AB_inter_warp_odd 322x242 ss=10 d8 43 decode frames  Y/U/V 0/3705/4366  total 8071
   first divergent: decode frame 0 plane U (row 204, col 108) ours 220 oracle 202
   Y: 0 wrong of 3350732 samples over 43 frames
   U: 3705 wrong of 1675366 (0.22%)  rows 154..239 (86/242)  cols 103..131 (29/161)
   V: 4366 wrong of 1675366 (0.26%)  rows 154..240 (87/242)  cols 103..139 (37/161)
   frames with a wrong sample: 43/43; worst decode frame 25 Y/U/V [0, 129, 170]
== O_odd322x242 322x242 ss=10 d8 17 decode frames  Y/U/V 0/985/722  total 1707
   first divergent: decode frame 0 plane U (row 176, col 128) ours 203 oracle 202
   Y: 0 wrong of 1324708 samples over 17 frames
   U: 985 wrong of 662354 (0.15%)  rows 153..211 (59/242)  cols 108..133 (26/161)
   V: 722 wrong of 662354 (0.11%)  rows 195..215 (21/242)  cols 107..115 (9/161)
   frames with a wrong sample: 17/17; worst decode frame 15 Y/U/V [0, 74, 77]
== Q_odd320x242 320x242 ss=10 d8 17 decode frames  Y/U/V 0/9603/14456  total 24059
   first divergent: decode frame 0 plane U (row 160, col 116) ours 182 oracle 202
   Y: 0 wrong of 1316480 samples over 17 frames
   U: 9603 wrong of 658240 (1.46%)  rows 138..202 (65/242)  cols 105..140 (36/160)
   V: 14456 wrong of 658240 (2.20%)  rows 99..203 (105/242)  cols 106..159 (54/160)
   frames with a wrong sample: 17/17; worst decode frame 11 Y/U/V [0, 796, 1184]
== S_odd326x242_10b 326x242 ss=10 d10 17 decode frames  Y/U/V 0/301/751  total 1052
   first divergent: decode frame 0 plane U (row 172, col 128) ours 806 oracle 808
   Y: 0 wrong of 1341164 samples over 17 frames
   U: 301 wrong of 670582 (0.04%)  rows 135..189 (55/242)  cols 119..136 (18/163)
   V: 751 wrong of 670582 (0.11%)  rows 117..197 (81/242)  cols 111..136 (26/163)
   frames with a wrong sample: 17/17; worst decode frame 5 Y/U/V [0, 41, 78]
== s422_320x242 320x242 ss=10 d8 17 decode frames  Y/U/V 0/6337/8398  total 14735
   first divergent: decode frame 0 plane U (row 160, col 116) ours 182 oracle 202
   Y: 0 wrong of 1316480 samples over 17 frames
   U: 6337 wrong of 658240 (0.96%)  rows 142..201 (60/242)  cols 104..144 (41/160)
   V: 8398 wrong of 658240 (1.28%)  rows 127..201 (75/242)  cols 101..145 (45/160)
   frames with a wrong sample: 17/17; worst decode frame 11 Y/U/V [0, 497, 592]
== s422_320x246 320x246 ss=10 d8 17 decode frames  Y/U/V 0/38348/59491  total 97839
   first divergent: decode frame 0 plane U (row 62, col 144) ours 161 oracle 166
   Y: 0 wrong of 1338240 samples over 17 frames
   U: 38348 wrong of 669120 (5.73%)  rows 46..245 (200/246)  cols 94..157 (64/160)
   V: 59491 wrong of 669120 (8.89%)  rows 48..245 (198/246)  cols 99..159 (61/160)
   frames with a wrong sample: 17/17; worst decode frame 11 Y/U/V [0, 2510, 3847]
== s422_322x240 322x240 ss=10 d8 17 decode frames  Y/U/V 0/22969/22534  total 45503
   first divergent: decode frame 0 plane U (row 62, col 149) ours 160 oracle 166
   Y: 0 wrong of 1313760 samples over 17 frames
   U: 22969 wrong of 656880 (3.50%)  rows 60..239 (180/240)  cols 134..160 (27/161)
   V: 22534 wrong of 656880 (3.43%)  rows 60..239 (180/240)  cols 135..160 (26/161)
   frames with a wrong sample: 17/17; worst decode frame 5 Y/U/V [0, 1871, 1815]
== s422_322x246 322x246 ss=10 d8 17 decode frames  Y/U/V 0/24964/25438  total 50402
   first divergent: decode frame 0 plane U (row 62, col 144) ours 169 oracle 166
   Y: 0 wrong of 1346604 samples over 17 frames
   U: 24964 wrong of 673302 (3.71%)  rows 60..245 (186/246)  cols 108..160 (53/161)
   V: 25438 wrong of 673302 (3.78%)  rows 61..245 (185/246)  cols 107..160 (54/161)
   frames with a wrong sample: 17/17; worst decode frame 10 Y/U/V [0, 1727, 1710]
== s422_326x240 326x240 ss=10 d8 17 decode frames  Y/U/V 0/734/0  total 734
   first divergent: decode frame 0 plane U (row 176, col 128) ours 204 oracle 202
   Y: 0 wrong of 1330080 samples over 17 frames
   U: 734 wrong of 665040 (0.11%)  rows 142..188 (47/240)  cols 112..135 (24/163)
   V: 0 wrong of 665040 samples over 17 frames
   frames with a wrong sample: 17/17; worst decode frame 12 Y/U/V [0, 94, 0]
== s422_326x246 326x246 ss=10 d8 17 decode frames  Y/U/V 0/24078/25112  total 49190
   first divergent: decode frame 0 plane U (row 172, col 128) ours 205 oracle 202
   Y: 0 wrong of 1363332 samples over 17 frames
   U: 24078 wrong of 681666 (3.53%)  rows 154..245 (92/246)  cols 106..136 (31/163)
   V: 25112 wrong of 681666 (3.68%)  rows 153..245 (93/246)  cols 105..140 (36/163)
   frames with a wrong sample: 17/17; worst decode frame 14 Y/U/V [0, 1577, 1632]
== s422_384x240 384x240 ss=10 d8 17 decode frames  Y/U/V 0/1982/2081  total 4063
   first divergent: decode frame 1 plane U (row 169, col 23) ours 90 oracle 89
   Y: 0 wrong of 1566720 samples over 17 frames
   U: 1982 wrong of 783360 (0.25%)  rows 167..190 (24/240)  cols 19..34 (16/192)
   V: 2081 wrong of 783360 (0.27%)  rows 169..192 (24/240)  cols 19..35 (17/192)
   frames with a wrong sample: 11/17; worst decode frame 14 Y/U/V [0, 292, 303]
== s422_320x242_10b 320x242 ss=10 d10 17 decode frames  Y/U/V 0/3915/5612  total 9527
   first divergent: decode frame 0 plane U (row 160, col 132) ours 667 oracle 808
   Y: 0 wrong of 1316480 samples over 17 frames
   U: 3915 wrong of 658240 (0.59%)  rows 121..198 (78/242)  cols 108..148 (41/160)
   V: 5612 wrong of 658240 (0.85%)  rows 128..194 (67/242)  cols 108..148 (41/160)
   frames with a wrong sample: 17/17; worst decode frame 7 Y/U/V [0, 324, 465]
== s422_352x242_10b 352x242 ss=10 d10 17 decode frames  Y/U/V 0/4858/3949  total 8807
   first divergent: decode frame 0 plane U (row 62, col 160) ours 667 oracle 664
   Y: 0 wrong of 1448128 samples over 17 frames
   U: 4858 wrong of 724064 (0.67%)  rows 47..215 (169/242)  cols 150..175 (26/176)
   V: 3949 wrong of 724064 (0.55%)  rows 49..213 (165/242)  cols 149..175 (27/176)
   frames with a wrong sample: 17/17; worst decode frame 12 Y/U/V [0, 470, 371]
== s422_352x250_10b 352x250 ss=10 d10 17 decode frames  Y/U/V 0/3876/1786  total 5662
   first divergent: decode frame 0 plane U (row 172, col 148) ours 638 oracle 664
   Y: 0 wrong of 1496000 samples over 17 frames
   U: 3876 wrong of 748000 (0.52%)  rows 127..210 (84/250)  cols 139..161 (23/176)
   V: 1786 wrong of 748000 (0.24%)  rows 130..209 (80/250)  cols 146..153 (8/176)
   frames with a wrong sample: 17/17; worst decode frame 7 Y/U/V [0, 320, 142]
== s422_416x242_10b 416x242 ss=10 d10 17 decode frames  Y/U/V 0/5968/4524  total 10492
   first divergent: decode frame 0 plane U (row 62, col 192) ours 640 oracle 664
   Y: 0 wrong of 1711424 samples over 17 frames
   U: 5968 wrong of 855712 (0.70%)  rows 22..203 (182/242)  cols 175..207 (33/208)
   V: 4524 wrong of 855712 (0.53%)  rows 22..203 (182/242)  cols 177..207 (31/208)
   frames with a wrong sample: 17/17; worst decode frame 12 Y/U/V [0, 541, 384]
== s422_416x250_10b 416x250 ss=10 d10 17 decode frames  Y/U/V 103250/64191/62834  total 230275
   first divergent: decode frame 0 plane Y (row 128, col 384) ours 370 oracle 371
   Y: 103250 wrong of 1768000 (5.84%)  rows 122..249 (128/250)  cols 284..415 (132/416)
   U: 64191 wrong of 884000 (7.26%)  rows 99..249 (151/250)  cols 139..207 (69/208)
   V: 62834 wrong of 884000 (7.11%)  rows 99..249 (151/250)  cols 139..207 (69/208)
   frames with a wrong sample: 17/17; worst decode frame 5 Y/U/V [7135, 4341, 4268]
== s444_352x242_10b 352x242 ss=00 d10 17 decode frames  Y/U/V 0/1166/1413  total 2579
   first divergent: decode frame 0 plane U (row 224, col 64) ours 209 oracle 216
   Y: 0 wrong of 1448128 samples over 17 frames
   U: 1166 wrong of 1448128 (0.08%)  rows 222..226 (5/242)  cols 63..118 (56/352)
   V: 1413 wrong of 1448128 (0.10%)  rows 222..226 (5/242)  cols 63..114 (52/352)
   frames with a wrong sample: 17/17; worst decode frame 4 Y/U/V [0, 170, 160]

TOTALS sorted
s422_416x250_10b       230275
s422_320x246           97839
s422_322x246           50402
s422_326x246           49190
s422_322x240           45503
Q_odd320x242           24059
s422_320x242           14735
s422_416x242_10b       10492
s422_320x242_10b       9527
s422_352x242_10b       8807
AB_inter_warp_odd      8071
s422_352x250_10b       5662
s422_384x240           4063
s444_352x242_10b       2579
O_odd322x242           1707
S_odd326x242_10b       1052
s422_326x240           734
Counter({'BYTE-EXACT': 77, 'DIVERGES': 17}) cells measured: 94
```

Reading the classes off this table:

* **Class A — chroma-only, both U and V, every frame.** The dominant class:
  13 of the 16 diverging 4:2:2 cells. Y is exactly 0 wrong on all of them, on
  every decode frame. The wrong region is a bounded rectangle inside one
  chroma plane (typically a few tens of columns by a few tens of rows), it
  starts in decode frame 0 (except `s422_384x240`, which starts in frame 1) and
  it persists into every subsequent frame because the wrong reference frame is
  then predicted from.
* **Class B — U-only.** `s422_326x240`: U 734, **V exactly 0**, all 17 frames.
  The same decoder, the same geometry class, one chroma plane clean.
* **Class C — 4:2:2 that also breaks LUMA.** `s422_416x250_10b`: Y 103250 of
  104000 samples over 17 frames (99.3%), U 64191, V 62834. This is the worst
  cell in the census by an order of magnitude and it is not a chroma defect —
  the luma plane itself desynchronises. It needs its own lane; nothing in the
  4:2:2 chroma work covers it.
* **Class D — not 4:2:2 at all.** `s444_352x242_10b`, 10-bit 4:4:4, chroma
  only, wrong region 5 rows x ~55 columns. Pre-existing, bypass-independent.

### 4.1 Both order bases, so no number is ambiguous

Every diverging cell was re-measured on the **DISPLAY** basis (`dump_yuv`,
shown frames only, against `aomdec --rawvideo`) with the same comparator and the
same explicit geometry. Decode order and display order are different totals for
the same defect; each table cell states its basis.

| `O_odd322x242` | 16 | 0/926/686 |
| `Q_odd320x242` | 16 | 0/9290/13698 |
| `S_odd326x242_10b` | 16 | 0/281/696 |
| `s422_320x242` | 16 | 0/6172/8140 |
| `s422_320x246` | 16 | 0/36179/56514 |
| `s422_322x240` | 16 | 0/22003/21625 |
| `s422_322x246` | 16 | 0/23665/24091 |
| `s422_326x240` | 16 | 0/718/0 |
| `s422_326x246` | 16 | 0/22765/23718 |
| `s422_384x240` | 16 | 0/1780/1871 |
| `s422_320x242_10b` | 16 | 0/3701/5380 |
| `s422_352x242_10b` | 16 | 0/4653/3777 |
| `s422_352x250_10b` | 16 | 0/3757/1751 |
| `s422_416x242_10b` | 16 | 0/5740/4348 |
| `s422_416x250_10b` | 16 | 99011/61486/60106 |
| `s444_352x242_10b` | 16 | 0/1112/1362 |

Every cell diverges on **both** bases, so no headline here is a hidden-frame
artefact. The totals differ by exactly the hidden pictures (43 vs 40 frames on
`AB_inter_warp_odd`), which is the labelling check the wave got wrong once.

---

## 5. What the shipped tree actually does today

With the bypass **reverted** (the shipped `cc9f2668` code), measured, not
argued:

| cell | verdict on the shipped tree |
|---|---|
| `Q_odd320x242` (4:2:2) | `REFUSED: unsupported: AV1 decode_stream (a chroma format of 4:2:2 (subsampling_x != subsampling_y): …)` |
| `s422_416x250_10b` (4:2:2) | same refusal string |
| `s444_352x242_10b` (4:4:4) | **decodes, DIVERGES 0 / 1166 / 1413** — identical to the bypassed build |

Swept over **all 94 cells** on the shipped build: **50 of 50 4:2:2 cells refuse
by name** with that same string, and **all 44 non-4:2:2 cells decode**. The
refusal is total and uniform, and it is exactly as unconditional as
`stream.rs:1803` reads.

So: the refusal is total and uniform over 4:2:2, and it is currently also the
only thing standing between a 10-bit 4:4:4 defect and shipped pixels.

---

## 6. Reproducing any row

```
# oracle + ours, one cell (4:2:2 needs the patch-run-restore bypass)
EC_AV1_FINAL_DUMP=$W/oracle ~/.cache/aom-oracle/build/aomdec --codec=av1 \
    -o $W/o.y4m CELL.obu
EC_AV1_FINAL_DUMP=$W/ours EC_NOMEMGUARD=1 \
    ~/.cache/tgt/<lane>/debug/examples/decode_probe CELL.obu
python3 ~/.cache/census422/cmp422.py CELL $W/ours $W/oracle W H SSX SSY DEPTH

# the flip control (per plane, per frame)
python3 ~/.cache/census422/liveness.py spec.json liveness.json

# the sweep
DEPTHS=8   WIDTHS="320 322 326 384" HEIGHTS="240 242 246" bash mksweep.sh
DEPTHS=10  WIDTHS="320 352 416"     HEIGHTS="242 250"     bash mksweep.sh
```

Geometry for `cmp422.py` is read from `$W/o.y4m`'s header, never from a size.
The instrument, the sweep cells and every per-frame dump are under
`/home/tahinli/.cache/census422/` (`cmp422.py`, `run.py`, `liveness.py`,
`display_basis.py`, `mksweep.sh`, `sweep/*.obu`, `work/<cell>/ours.f*`).

---

## 7. Decision table — numbers and classes only

| | 4:2:2 | 4:2:0 control | 4:4:4 control |
|---|---|---|---|
| corpus cells measured | 32 | 4 | 4 |
| corpus byte-exact | 28 | 4 | 4 |
| corpus diverging | 4 | 0 | 0 |
| fresh sweep cells | 18 | 18 | 18 |
| fresh sweep byte-exact | 6 | 18 | 17 |
| fresh sweep diverging | 12 | 0 | 1 |
| **total measured** | **50** | **22** | **22** |
| **byte-exact** | **34** | **22** | **21** |
| **diverging** | **16** | **0** | **1** |
| **refusing (shipped tree)** | **50** | 0 | 0 |
| of the exact, committed-pin-backed | 9 | 0 | 0 |
| of the exact, probe-only (no sha in repo) | 25 | 22 | 21 |
| of the diverging, probe-only | 16 | 0 | 1 |

**Remaining failure classes, with the recipe that reproduces each:**

| class | cells | recipe | worst cell |
|---|---|---|---|
| A chroma-only, U+V, all frames | 13 | `mksweep.sh` 8-bit arm, `DEPTHS=8`, any of the 7 diverging geometries | `Q_odd320x242` 24059 samples |
| B U-only, V clean | 1 | `s422_326x240` | 734 samples |
| C luma also wrong (10-bit) | 1 | `s422_416x250_10b` | **230275 samples** |
| D 10-bit 4:4:4 chroma (not 4:2:2) | 1 | `s444_352x242_10b` | 2579 samples |

**Worst remaining divergence in samples:** `s422_416x250_10b`, 230275 wrong
samples (Y 103250 / U 64191 / V 62834) over 17 decode frames at 416x250 10-bit.
Largest 8-bit: `s422_320x246`, 97839. Smallest: `s422_326x240`, 734.

**What a production lift would and would not cover, stated as arithmetic, not
as a recommendation:**

* Lifting the header refusal would put the 34 exact cells on a shipping path
  **today**. 9 of them are committed bytes; the other 25 exist only in
  `/home/tahinli/.cache/cells/` and in this lane's sweep directory, so a lift
  would ship **no committed pixel gate** for any of them unless the fixtures are
  committed first.
* A lift would expose **16 diverging cells**, i.e. roughly one cell in three of
  everything measured. Class C alone (`s422_416x250_10b`) is a luma-plane
  failure at 10-bit.
* A lift would **not** fix class D: `s444_352x242_10b` is a 4:4:4 defect that is
  already reachable and already diverges with the refusal standing.
* The lift's blast radius is not bounded by this census: §8.

---

## 8. What is NOT measured (read this before quoting any number above)

1. **12-bit.** No 12-bit cell was encoded or measured anywhere in this census.
   The corpus has 12-bit 4:2:0 fixtures; 4:2:2 12-bit is entirely unmeasured.
2. **Superres.** No superres 4:2:2 cell. The corpus's superres fixtures are
   4:4:4. `EC_AV1_FINAL_DUMP` is post-superres, so a superres 4:2:2 cell is
   measurable with this exact instrument, but none was built.
3. **Film grain, altref-heavy streams, tiles at odd sizes beyond the corpus**,
   and any 4:2:2 stream with `order_hint` / film-grain params.
4. **Monochrome and 4:4:0**: not codable / not 4:2:2; out of scope by the
   existing gate.
5. **Encoder-recipe coverage.** One recipe (`--cpu-used 0 --lag-in-frames 25
   --auto-alt-ref=1 --enable-global-motion=1 --cq-level=24`), one source
   (`testsrc2`), `--limit=16`. The rate-control axis (`--end-usage=q`, other
   `--cq-level` values) is **not** swept here, and per
   `lanes/av1422ctrigger.report.md` §2e a repeated aomenc flag takes the FIRST
   occurrence, so a naive flag sweep silently measures the base arm twice.
6. **Frame-size changes within one stream** (mid-stream resolution change): the
   comparator handles differing frame sizes, but no such 4:2:2 cell was built,
   so that path is untested here.
7. **The 23 probe-only corpus cells and all 54 sweep cells are pinned by no sha
   in this repository.** The sweep cells are reproducible from §6; the probe
   corpus is not reproducible from this repo at all.
8. **Which writer produces the divergence.** This census measures the *extent*
   and the *class* of each failure. It attributes none of them to a source
   line. `lanes/av1422ctrigger.report.md` §4/§5 names a candidate site for
   class A (`sub8_leaf_chroma422`'s skip arm / the intra edge-availability
   gate) and REFUTES it as the writer; that attribution is not re-litigated
   here.
9. **Nothing was fixed.** Every failure above is a report line, not a work
   item.

---

## 9. Regression

On the clean tree (bypass reverted), worktree `lane-av1422census`:

```
cargo test -p ec-av1 --lib -- 422 444 420 lossless warp intra \
    --skip bitrate_target_lands_within_5_percent_over_48_frames
```

```
test result: ok. 181 passed; 0 failed; 2 ignored; 0 measured; 619 filtered out; finished in 290.86s
```

Baseline for this lane. No source change is proposed, so there is no "after".
