# lane-av1422anom — the odd-width 4:2:2 anomaly is a 4:2:0 defect, and the previous lane's "18/18 exact" was a tautology

Base `11d366b1`, worktree `~/.cache/wt/av1422anom`, branch `lane-av1422anom`.

## Verdict

**Answer to the question I refused to answer last lane: (b) — the decoder is
wrong, and it is wrong far more widely than 4:2:2.**

Three findings, in descending order of importance:

1. **The previous lane's headline "18/18 cells byte-exact, 0 wrong samples on Y,
   U and V" was produced by a comparator that could not report a difference.**
   It packed our own decoded samples into a buffer and then compared that
   buffer against the same samples; the oracle's bytes were never read. It
   reported 0 wrong on every cell, including cells that differ from
   `aomdec --rawvideo` in tens of thousands of samples. **That result is
   retracted.** The corrected table is below.
2. **Corrected, the defect is not a 4:2:2 matter at all. It is an ordinary
   4:2:0 odd-frame-HEIGHT reconstruction defect**, reachable today with no
   bypass and no refusal involved: a 320x232 4:2:0 stream decodes with **17 658
   wrong chroma samples out of 593 920, in all 16 frames including the
   keyframe**. Two more heights fail. Two pass. It is present on both
   `6d564cd6` and `11d366b1` with identical numbers.
3. **The anomaly itself is exonerated of being 4:2:2-specific.** The original
   `O_odd322x242` observation (odd-width 4:2:2) reproduces and is real, but it
   is one instance of the height-driven 4:2:0 defect, and the same shape at
   4:2:0 (height 242) happens to pass while height 232 does not.

The 4:2:2 refusal is untouched, as instructed. **This lane does not recommend
lifting it**, and the new evidence argues against lifting: a cell family the
refusal was standing in for is broken.

## 1. What the rung captures, and whether it can be the returned object

`decode_frame` (`crates/ec-av1/src/stream.rs`) takes the dump prefix as a
parameter, not a thread-local, and the `EC_AV1_FINAL_DUMP` block is the last
statement before `Ok(FrameOutput { picture, .. })`, built from `picture.y`,
`picture.u`, `picture.v` — the same `Picture` value it returns. There is no
later write inside `decode_frame`, and the caller wraps that value in an `Arc`
and only clones it.

I measured it rather than reading it, with three capture points in one
process, on both the odd-width 4:2:2 cell and its 4:2:0 control:

| comparison | `O_odd322x242` (4:2:2) | `P_odd420_322x242` (4:2:0) |
|---|---|---|
| P0 (in `decode_frame`) vs P1 (caller, immediately after return) | **0 of 17 frames differ** | **0 of 17 differ** |
| P0 vs the oracle's rung-12 dump | **17 of 17 frames differ** | 0 of 17 differ |

So the rung is **not** mis-timed and **not** mis-strided: it and the returned
picture are the same object, byte for byte, at the same instant. The reading
that was wrong last lane was mine — I concluded "presented output is exact"
from a counter that never compared against the oracle. Verdict (b): the stored
reference really differs, and the presented output differs too.

## 2. The discriminating measurement

Both readings come from the same `picture`, so there is nothing to reconcile —
the earlier "exact presented, differing decode-order" contradiction was an
artefact of the broken comparator. With a comparator that actually reads the
oracle's bytes:

- `O_odd322x242` (322x242 → chroma 161x242): **1612 wrong samples** (U 32,
  V 1580), 13/16 frames exact, differing in all 16.

### The trigger is HEIGHT, not width

Same source, same encoder recipe (`--profile=2`, `--cq-level=45`,
`--cpu-used=0`), varying one dimension:

| cell | shape | chroma | wrong Y | wrong U | wrong V | total | verdict |
|---|---|---|---|---|---|---|---|
| `T_422_322x240` | 322x240 | 161x240 | 0 | 0 | 0 | **0** | EXACT |
| `S_422_320x242` | 320x242 | 160x242 | 0 | 422 | 22566 | 22988 | DIVERGES |
| `R_422_324x242` | 324x242 | 162x242 | 0 | 712 | 25489 | 26201 | DIVERGES |
| `X_422_320x232` | 320x232 | 160x232 | 0 | 1042 | 106342 | 107384 | DIVERGES |
| `X_422_320x248` | 320x248 | 160x248 | 0 | 32 | 2185 | 2217 | DIVERGES |

Odd chroma width is not the trigger: `T` has chroma width 161 (odd) and is
exact, while `S`/`R` have even chroma widths and fail. 320, 322 and 324 all
fail at height 242, so width parity is irrelevant.

### It is not a 4:2:2 defect either

The 4:2:0 controls, same recipe at `--profile=0`:

| cell | shape | wrong Y | wrong U | wrong V | total | verdict |
|---|---|---|---|---|---|---|
| `Y_420_320x232` | 320x**232** | 0 | 200 | 17658 | **17858** | DIVERGES |
| `Y_420_320x242` | 320x**242** | 0 | 0 | 0 | **0** | EXACT |
| `Y_420_320x248` | 320x**248** | 0 | 717 | 29936 | **30653** | DIVERGES |
| `P_odd420_322x242` | 322x242 | 0 | 0 | 0 | 0 | EXACT |
| `Q_oddh420_320x242` | 320x242 | 0 | 0 | 0 | 0 | EXACT |

**4:2:0 fails at heights 232 and 248 and passes at 240 and 242.** The passing
heights are not the multiples of 8 (232 and 248 are both multiples of 8 and
both fail), so this is not a simple alignment rule. 4:2:0 needs no bypass and
no refusal: **this is reachable by any user decoding ordinary 4:2:0 today.**

### The error is in RECONSTRUCTION, before every loop filter

For `R_422_324x242` frame 0, comparing the decoder's own stage dumps (cropped
from their padded extents) against the oracle's final frame:

| stage | differing samples vs oracle final |
|---|---|
| `EC_AV1_PREFILT_DUMP` (post-reconstruction, **pre**-deblock) | **1424** (U 712, V 712) |
| `EC_AV1_POSTDEBLOCK_DUMP` | 1424 (identical region) |
| `EC_AV1_POSTCDEF_DUMP` | 1424 (identical region) |

The wrong region is already present at `PREFILT` and unchanged through deblock,
CDEF and loop restoration, so **all three filters are exonerated**. Two further
checks agree: a `--enable-restoration=0` encode of the same shape still fails
(20 232 wrong), and the wrong samples are present in the **keyframe**, which
has no inter prediction at all — so motion compensation is exonerated too.
The defect is in block reconstruction / residual / transform.

The region for `R_422_324x242` frame 0 is chroma cols 108–127, rows 134–161 in
both U and V; for `O_odd322x242` it is chroma cols ~107–133, rows ~102–143.
Interior, not an edge.

### It reproduces on both bases

Re-measured on `6d564cd6` (a separate worktree at that exact commit, the same
comparator): `O_odd322x242` 1612, `X_422_320x232` 107384, `X_422_320x248` 2217,
`Y_420_320x232` 17858, `Y_420_320x248` 30653, and `P_odd420_322x242` /
`Y_420_320x242` 0 — **identical to the `11d366b1` numbers.** Not a regression
from `lane-av1odd440`; present on both.

## 3. The corrected cell table

Every cell below, measured with a comparator that reads the oracle's bytes.
`R` = REFUSED at the sequence header on committed code, `P` = PANIC,
`D` = DIVERGES, `E` = EXACT. "wrong" is per-plane differing samples over all
shown frames.

### Committed 4:2:2 fixtures (all still refuse; all still exact)

| cell | class | wrong Y | wrong U | wrong V | frames exact |
|---|---|---|---|---|---|
| `422_allskip_2f.obu` | R → E | 0 | 0 | 0 | 2/2 |
| `422_sb128_3f.obu` | R → E | 0 | 0 | 0 | 3/3 |
| `422_intrabc_sb128_strip.obu` | R → E | 0 | 0 | 0 | 5/5 |
| `422_intrabc_sb128_strip_notxsearch.obu` | R → E | 0 | 0 | 0 | 5/5 |
| `422_residual_compound_warp_16f.obu` | R → E | 0 | 0 | 0 | 16/16 |
| `422_residual_compound_warp_nolr_16f.obu` | R → E | 0 | 0 | 0 | 16/16 |

**The six committed 4:2:2 pins are genuinely byte-exact.** That part of the
previous lane's result stands.

### Fresh cells — the retraction

| cell | recipe delta | class | wrong Y | wrong U | wrong V | total | frames exact |
|---|---|---|---|---|---|---|---|
| `A_testsrc2_cpu0` | testsrc2 320x240, cq 24 | R → E | 0 | 0 | 0 | 0 | 16/16 |
| `B_testsrc2_cpu6` | A + `--cpu-used=6` | R → E | 0 | 0 | 0 | 0 | 16/16 |
| `C_mandel320` | mandelbrot 320x240 | R → E | 0 | 0 | 0 | 0 | 16/16 |
| `D_bars` | smptebars 320x240 | R → E | 0 | 0 | 0 | 0 | 16/16 |
| `E_noglobal` | A + `--enable-global-motion=0` | R → E | 0 | 0 | 0 | 0 | 16/16 |
| `F_allintra` | A + `--lag-in-frames=0 --auto-alt-ref=0` | R → E | 0 | 0 | 0 | 0 | 16/16 |
| `H_10bit_testsrc2` | 10-bit 4:2:2 320x240 | R → E | 0 | 0 | 0 | 0 | 16/16 |
| `I_10bit_mandel` | 10-bit 4:2:2 mandelbrot | R → E | 0 | 0 | 0 | 0 | 16/16 |
| `J_10bit_lr0` | H + `--enable-restoration=0` | R → E | 0 | 0 | 0 | 0 | 16/16 |
| `K_sct` | A + `--cq-level=18 --tune=ssim` | R → E | 0 | 0 | 0 | 0 | 16/16 |
| `L_tiled` | A + `--tile-columns=1 --tile-rows=1` | R → E | 0 | 0 | 0 | 0 | 16/16 |
| **`O_odd322x242`** | testsrc2 **322x242** | R → **D** | 0 | 32 | 1580 | **1612** | 13/16 |
| `T_422_322x240` | testsrc2 322x**240** | R → E | 0 | 0 | 0 | 0 | 16/16 |
| **`S_422_320x242`** | testsrc2 320x**242** | R → **D** | 0 | 422 | 22566 | **22988** | 12/16 |
| **`R_422_324x242`** | testsrc2 324x**242** | R → **D** | 0 | 712 | 25489 | **26201** | 14/16 |
| **`X_422_320x232`** | testsrc2 320x**232** | R → **D** | 0 | 1042 | 106342 | **107384** | 12/16 |
| **`X_422_320x248`** | testsrc2 320x**248** | R → **D** | 0 | 32 | 2185 | **2217** | 14/16 |
| `V_422_320x242_lr0` | S + `--enable-restoration=0` | R → **D** | 0 | 422 | 19810 | **20232** | 12/16 |
| `P_odd420_322x242` | 4:2:0 322x242 | **E** | 0 | 0 | 0 | 0 | 16/16 |
| `Q_oddh420_320x242` | 4:2:0 320x242 | **E** | 0 | 0 | 0 | 0 | 16/16 |
| **`Y_420_320x232`** | 4:2:0 320x**232** | **D** | 0 | 200 | 17658 | **17858** | 0/16 |
| `Y_420_320x242` | 4:2:0 320x**242** | **E** | 0 | 0 | 0 | 0 | 16/16 |
| **`Y_420_320x248`** | 4:2:0 320x**248** | **D** | 0 | 717 | 29936 | **30653** | 0/16 |

**The one correction to the previous lane's "frames_exact" column**: it divided
the per-frame byte count by the frame count before slicing, so its
`frames_exact` values were meaningless (it reported 13/16 where the truth is
0/16). The per-plane wrong counts were the only sound column it produced — and
those were computed against itself.

The previous lane's twelve 320x240 cells and its six committed pins are all
confirmed exact. **The retraction is confined to the odd-geometry cells and to
the claim that the comparison was measuring anything.**

## 4. What is committed

One fixture and two gates. **No decoder fix — see `not_done`.**

`crates/ec-av1/fixtures/420_oddheight_320x232.obu`, 15 773 bytes,
sha256 `6832c3cd867f5fc491b45fd67f77c38453db0ca6d461e7d225e6276f4d0784c8`,
fnv1a64 `0x37877f15307c3b`.

Source and recipe (ffmpeg 8.1.2, `aomenc` = `~/.cache/aom-oracle/build/aomenc`,
Sep 29 build):

```
ffmpeg -v error -f lavfi -i testsrc2=size=320x232:rate=24:duration=1 \
       -pix_fmt yuv420p -y p232.y4m          # sha256 f8e8077764e9b1b8e6b97ffa5f7d6f10ed627c6924f3451bcba14d3594903b47
aomenc --codec=av1 --profile=0 --input-bit-depth=8 --limit=16 \
       --lag-in-frames=25 --auto-alt-ref=1 --enable-global-motion=1 \
       --pass=1 --cq-level=45 --threads=4 --kf-min-dist=0 --kf-max-dist=999999 \
       --width=320 --height=232 --cpu-used=0 p232.y4m -o Y232.webm
ffmpeg -v error -i Y232.webm -c copy -f obu -y 420_oddheight_320x232.obu
```

- **`count_rawvideo_diffs`** — a permanent COUNTING oracle comparison, the
  counterpart to `assert_rawvideo_matches` (which panics on the first
  difference and so can only answer one cell per process). It returns
  `None` on a size mismatch rather than reporting "exact", and it derives the
  plane split from the decoded picture instead of a formula.
- **`the_counting_oracle_diff_reports_a_real_difference`** — the non-vacuity
  test. It decodes the pinned witness, which is known to differ, and asserts a
  non-zero count and `frames_exact < frames`. This is the test that would have
  caught last lane's comparator.
- **`the_pinned_420_oddheight_witness_is_pinned_and_ratchets_its_reconstruction_defect`**
  — pins the fixture by size and fnv1a64 and **ratchets the measured defect**:
  `(0, 200, 17658)` wrong and 0/16 frames exact. It is deliberately not an
  exactness gate, because the decoder is not exact. Its failure message names
  the fix path (assert `(0, 0, 0)` when the defect is gone).

Both gates are mutation-proven: flipping one fixture byte fails
`420_oddheight_320x232.obu bytes drifted`; changing the expected count to
17659 fails with `the measured defect moved`.

## 5. Gates run

- `the_counting_oracle_diff_reports_a_real_difference` — pass
- `the_pinned_420_oddheight_witness_is_pinned_and_ratchets_its_reconstruction_defect` — pass
- both mutation proofs — red as intended, then restored green

## 6. `not_done`

- **No decoder fix.** I localised the defect to reconstruction (present at
  `PREFILT`, keyframe affected, LR-off still fails) and to a height-dependent
  trigger, but I did not find the root cause, and I am not shipping a
  speculative change to a bit-exact decoder on a hunch. The exact next step:
  the region is small and reproducible, so take `Y_420_320x232` frame 0 (700
  differing samples, **luma exact**, chroma only) and bisect with the decoder's
  own `EC_COEFF_STEP` / `EC_PRED` rungs against the oracle at the first
  differing block. The height dependence (232 and 248 fail, 240 and 242 pass,
  232 and 248 both multiples of 8) is the strongest lead: it says the bug is
  in a per-superblock or per-row-band extent computation whose remainder is
  handled for some remainders and not others, and the 4:2:0 chroma height at
  232 is 116 (= 29 chroma MI rows) against 121 at 242.
- **The byte-exactness gate Main asked for cannot be written** until the fix
  lands. The ratchet gate is the honest substitute and is explicitly labelled
  as such.
- **Only heights 232/240/242/248 at width 320 were swept.** The full
  width x height grid is not mapped, so the trigger's real extent is unknown.
- **The 4:2:2 lift question is untouched**, as instructed. The new evidence
  argues against lifting: a cell family in the refused neighbourhood is broken.
- The remaining fresh cells (`O`, `S`, `R`, `X_232`, `X_248`, `V`, `Y_232`,
  `Y_248`) are **not** pinned; only the 4:2:0 320x232 witness is, because it is
  the one reachable without a bypass. Their recipes and sha256 are in
  `lanes/av1422remeasure.report.md` and in the table above.
- **Full suite not run** (lane rules: scoped only).
