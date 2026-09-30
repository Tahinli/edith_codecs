# lane-av1422llodd — the 242-tall odd-height 4:2:2 decode-frame-0 divergence

**Outcome: NOT root-caused and NOT fixed. The "odd height" premise is REFUTED by
measurement. The defect is localised to a 4:2:2 intra-CHROMA reconstruction
path and its exact expression is not named. No source change is proposed; the
lane tree is clean.**

Tip: `main` = `81a21c7d`. Worktree `/home/tahinli/.cache/wt/av1422llodd`, branch
`lane-av1422llodd`, **zero commits, zero source changes**. The only edits ever
applied to this tree were the 4:2:2 sequence-header bypass probe and one
temporary `EC_AV1_TRACE` print, both reverted (`git status` clean on the lane and
on the primary checkout).

---

## 0. Headline: "242 tall" is NOT the factor

`lanes/av1422llinter.report.md` §4b attributed this cluster to odd height. That
attribution is wrong, and §3 below is the control that kills it.

---

## 1. The 27-cell census, re-measured on this tip

Bypass patched in (`stream.rs:1803` `if false && seq.subsampling_x != seq.subsampling_y`),
release `decode_probe`, per-DECODE-frame byte comparison against
`/home/tahinli/.cache/aom-oracle/build/aomdec` with `EC_AV1_FINAL_DUMP` (decode
order, both sides). Harness: `/home/tahinli/.cache/lane-av1422llodd/census.sh`.

**22/27 exact on every decode frame. 4 fail, all at decode frame 0, all chroma-only:**

| cell | geometry | result |
|---|---|---|
| `AB_inter_warp_odd` | 322x242 8-bit | 0/43, first bad **decode frame 0** byte 110876 |
| `O_odd322x242` | 322x242 8-bit | 0/17, first bad **decode frame 0** byte 106388 |
| `Q_odd320x242` | 320x242 8-bit | 0/17, first bad **decode frame 0** byte 103156 |
| `S_odd326x242_10b` | 326x242 10-bit | 0/17, first bad **decode frame 0** byte 214112 |

Exact (all decode frames): `422_allskip_2f`, `422_intrabc_sb128_strip{,_notxsearch}`,
`422_residual_compound_warp_{,nolr}_16f`, `422_sb128_3f`, `AA_inter_compound`,
`AD_inter_nogm`, `A_testsrc2_cpu0`, `B_testsrc2_cpu6`, `C_mandel320`, `D_bars`,
`E_noglobal`, `F_allintra`, `H_10bit_testsrc2`, `I_10bit_mandel`, `J_10bit_lr0`,
`K_sct`, `L_tiled`, `R_odd322x240`, `T_tilecols2`, `U_tilerows1`,
**`V_tile2x2_odd` (322x242!)**, `W_intrabc`, `X_intrabc_tiled`, `Y_intrabc_10b`.

`W/X/Y` are now exact — that is `lane-av1422llinter2`'s fix, confirmed on this
tip. `V_tile2x2_odd` is **322x242 and byte-exact**, which already refutes
"242 tall" on its own.

## 1a. Oracle-flip control

Flipping one byte of the oracle's frame 0 adds exactly 1 differing byte, reported
at frame 0 plane Y (0,0) (`cmp.py --flip-sample 0` from the prior lane, re-run
unchanged). The comparator bites; the 48/801-sample diffs below are real.

---

## 2. Stage attribution: 100% pre-filter, exonerated with evidence

For every failing cell, our own `EC_AV1_PREFILT_DUMP` (mi-aligned, `W×248`
luma + chroma, stride-cropped by `stagecmp.py`) was compared against the
oracle's own `EC_AV1_PREFILT` at the same decode frame:

| cell | our PREFILT vs oracle PREFILT (frame 0) | our PREFILT == our POSTDEBLOCK == our POSTCDEF |
|---|---|---|
| `O_odd322x242` | Y **byte-exact**; U 32/38962; V 16/38962 — **total 48** | **True / True** |
| `AB_inter_warp_odd` | Y **byte-exact**; U 32; V 32 — **total 64** | **True / True** |
| `Q_odd320x242` | Y **byte-exact**; U 422; V 379 — **total 801** | **True / True** |
| `S_odd326x242_10b` | Y **byte-exact**; U 17; V 32 (16-bit samples) | **True / True** |

The FINAL-vs-oracle counts equal the PREFILT-vs-oracle counts **exactly**
(O: 32+16=48; AB: 32+32=64; Q: 422+379=801). So deblock, CDEF, loop restoration
and superres carry **zero** of the divergence. *(Method warning, already paid for
once: our non-WIDE dumps are plane-padded to 256 rows while the oracle crops to
the display shape; a naive `cmp` there reports a fake 66k-byte diff. `stagecmp.py`
de-pads by stride.)*

**First divergent samples (decode frame 0, per plane):**

| cell | plane | first bad | ours | oracle | shape of the wrong region |
|---|---|---|---|---|---|
| `Q_odd320x242` | U | (row 160, col 116) | 182 | 202 | rows 160-201, cols 116-139 |
| `Q_odd320x242` | V | (row 160, col 116) | 180 | 222 | rows 160-201, cols 116-139 |
| `O_odd322x242` | U | (row 176, col 128) | 203 | 202 | rows 176-179 c128-131; rows 204-207 c108-111 |
| `O_odd322x242` | V | (row 204, col 108) | 166 | 222 | rows 204-207, cols 108-111 |
| `AB_inter_warp_odd` | U | (row 204, col 108) | 220 | 202 | rows 204-207, cols 108-115 |
| `AB_inter_warp_odd` | V | (row 204, col 108) | 167 | 222 | rows 204-207, cols 108-115 |
| `S_odd326x242_10b` | U | (row 172, col 128) | 64 | 64 | rows 172-181, cols 128-133 (10-bit) |
| `S_odd326x242_10b` | V | (row 172, col 128) | 72 | 72 | rows 172-179, cols 128-131 |

Chroma 4:2:2 geometry: `chroma_w = W/2` (161 / 160 / 163), `chroma_h = H` (242).
Luma is byte-exact on **all four**, so the entropy parse is in sync: a
coefficient-count or CDF-row error is ruled out (§5).

---

## 3. The control that refutes "odd height" (and "odd width")

42 freshly built cells, same source (`testsrc2`), same encoder settings per row,
`ffmpeg + libaom-av1 -crf 30 -b:v 0 -cpu-used 6 -usage good -lag-in-frames 0 -g 30`,
full 17-frame decode order, `census.sh`:

**Height sweep at 320 wide, 4:2:2 / 4:2:0 / 4:4:4:**

| height | 4:2:2 | 4:2:0 | 4:4:4 |
|---|---|---|---|
| 236, 238, 240, 241, **242**, 243, 244, 248, 256 | ALL 17/17 EXACT | EXACT | EXACT |

**Width sweep at 4:2:2, heights 240 and 242:** widths 318, 320, 321, 322, 323,
324, 326, 328 × heights 240, 242 — **all 16 cells 16-17/17 BYTE-EXACT**.

So:

* **Odd height is refuted.** 320x242 4:2:2 is byte-exact; so is 241 and 243.
* **Odd width is refuted.** 321, 323, 322, 326 at height 242 are all exact.
* **4:2:0 and 4:4:4 at 242 are exact** — the defect is not "odd height at all",
  it is 4:2:2-and-something-else.
* `V_tile2x2_odd` (322x242 4:2:2) in the committed corpus is exact, which
  independently refutes "242 tall" before any new encode.

**Therefore the common factor is NOT the geometry.** It is something in the
encoder configuration of the four `lossy_all` cells that my fresh encodes do not
reproduce. Their sequence headers differ from my cells' (`…07 c5 ff f9 81` vs
`…07 c4 da f9 01` — a different `seq_profile`/`reduced_still_picture_header`
packing), and the four failing cells are the corpus's `aomenc`-built ones
(`A_testsrc2_cpu0`/`B_testsrc2_cpu6` naming, `base_q=25` measured in our
`EC_AV1_TRACE` dequant). I could not rebuild them with the local `aomenc`
(it rejects `--crf`, and the raw-yuv invocation produces no output), so the
encoder-setting difference is **named as the open factor, not pinned**.

---

## 4. Localisation of the first divergent unit

Instrument: the OBU stream is truncated to sequence header + first frame
(`obu_first_frame.py`), so both sides' `EC_PRED` ladders contain **frame 0 only**
and pairing is unambiguous. Oracle `EC_PREDOUT8` and our `OUR_PRED` are
normalised into `(plane, x, y, w, h, mode, sum)` and compared by key, not line
(`lad2.py` — the two ladders have different lengths because the oracle prints
nothing for a palette block, so a line-for-line diff is meaningless).

**First divergent prediction, in oracle decode order:**

| cell | first divergent unit | oracle | ours |
|---|---|---|---|
| `Q_odd320x242` | plane 1 (120,160) 4x4 | mode 12, sum 3232 | mode 12, sum 2912 |
| `O_odd322x242` | none in the ladder (see below) | — | — |
| `AB_inter_warp_odd` | plane 1 (112,204) 4x4 | mode 2, sum 3232 | mode 2, sum 3520 |

**Owning luma block, all four cells, identical shape.** Converting the first bad
chroma coordinate to luma (`(cx<<1, cy)`) and finding the block that covers it:

| cell | luma block | our luma units | oracle prediction print covering it |
|---|---|---|---|
| `Q_odd320x242` | (232,160) = mi(40,58) | 8x4 @y160 + 8x4 @y164 | **none** |
| `O_odd322x242` | (256,176) = mi(44,64) | 8x4 | **none** |
| `AB_inter_warp_odd` | (216,204) = mi(51,54) | 8x4 | **none** |
| `S_odd326x242_10b` | (256,172) = mi(43,64) | 8x4 (10-bit) | **none** |

**The common shape is a 2×2-mi (8×8 luma) block, split into two 8×4 luma units,
whose 4:2:2 chroma is coded as two 4×4 chroma units stacked** — i.e. the 4:2:2
"sub-8x8" chroma walk. Our parse trace names it directly
(`EC_AV1_TRACE` on `Q_odd320x242` frame 0, lines 40614-40615 of the trace):

```
TRACE partition_w8 mi=(40,58) ctx=0 value=1
TRACE sub8 skip mi=(40,58) ctx=1 value=1 rng=44808
TRACE sub8 skip mi=(41,58) ctx=1 value=0 rng=42472
```

and the chroma unit at chroma (116,160) reconstructs from a **DC** prediction of
182 (`OUR_PRED x=116 y=160 plane=1 side=4 side=4 mode=0 sum=2912`) against the
oracle's 202.

**The oracle's 202 is exactly `dc_top`** — `(sum(above) + bw/2)/bw` with the four
above samples all 202, i.e. **libaom used the above edge only**. Ours used
above+left: `(808 + 644 + 4)/8 = 182`, where the left column is
chroma col 115 rows 160-163 = 165,164,163,152 — samples that are **byte-identical
on both sides**. So the DC differs because the **left context was included by us
and excluded by libaom**, or because the *block that owns* the left column
differs.

**The first divergence is inside the chroma of luma mi(40,58), and the causal
chain is visible one block to the right**: the block at chroma (120,160) reads its
D203 prediction off chroma col 119 = the seed block's chroma, and is the first
*printed* disagreement (mode identical, 3232 vs 2912).

**libaom's expression that governs the left's availability, verbatim**
(`av1/common/av1_common_int.h:1367-1379`, `set_mi_row_col`):

```c
  xd->left_available = (mi_col > tile->mi_col_start);
  xd->chroma_up_available = xd->up_available;
  xd->chroma_left_available = xd->left_available;
  if (ss_x && bw < mi_size_wide[BLOCK_8X8])          /* bw < 2, i.e. a 4x4 luma block */
    xd->chroma_left_available = (mi_col - 1) > tile->mi_col_start;
  if (ss_y && bh < mi_size_high[BLOCK_8X8])
    xd->chroma_up_available = (mi_row - 1) > tile->mi_row_start;
```

consumed by `av1/common/reconintra.c:1744-1766`:

```c
  const int have_top  = row_off || (ss_y ? xd->chroma_up_available : xd->up_available);
  const int have_left = col_off || (ss_x ? xd->chroma_left_available : xd->left_available);
  const int n_top_px  = have_top  ? AOMMIN(txwpx, xr + txwpx) : 0;
  const int n_left_px = have_left ? AOMMIN(txhpx, yd + txhpx) : 0;
```

and then `dc_pred[n_left_px > 0][n_top_px > 0][tx_size]`
(`reconintra.c:1329-1331` → `aom_dsp/intrapred.c:181-225`:
`dc` = both, `dc_top` = above only, `dc_left` = left only, `dc_128` = neither).

**At `ss_y == 0` (4:2:2) the first term of `have_top` is `row_off || chroma_up_available`
and the `ss_y` clause never fires — libaom's 4:2:2 `chroma_up_available` is
therefore plain `up_available`.** That is the one place in this expression where
4:2:2 differs from 4:2:0, and it is where I would attack next. **I did not
confirm it**, and I am not claiming it.

---

## 5. What is ruled OUT, with the measurement that rules it out

| hypothesis | verdict | evidence |
|---|---|---|
| a loop filter / CDEF / LR / superres stage carries it | **refuted** | PREFILT-vs-oracle diff == FINAL-vs-oracle diff exactly (48/64/801/49); our PREFILT == POSTDEBLOCK == POSTCDEF on all four |
| the entropy parse desynchronises | **refuted** | luma byte-exact on all four; and the whole-frame palette colour-index ladder is **identical**: 167 colour maps per side, identical per-block shapes (`{(8,8):41, (4,16):39, (8,4):27, (4,8):26, (16,8):11, (8,16):9, (16,16):6, (32,16):3, (32,32):2, (16,32):2, (16,64):1}` on both), and all **16 544** `EC_PAL_VAL` indices identical (`EC_TRACE_PALETTE` on the oracle, `EC_AV1_TRACE` on ours) |
| a wrong coefficient COUNT (one vs two 4:2:2 chroma units) | **refuted** | a count change is a desync, and the parse is in sync (§5 row 2) |
| the palette colour-index map or the palette colours | **refuted** | identical values, identical shapes, identical counts on both sides |
| the palette-prediction handoff slot (`set_palette_pred`/`take_palette_pred`) | **refuted** | `EC_DEBUG_PAL` over frame 0: 466 `PALSET`, 0 with `stale=true` |
| per-unit palette windowing stride | **refuted for this defect** | every `palette_window` call in `decode.rs` uses `chroma_side` as the stride, which is the map's own width |
| "242 tall" | **REFUTED** | §3: 320x242, 321x242, 323x242, 326x242 4:2:2 all 17/17 exact; `V_tile2x2_odd` (322x242) exact in the committed corpus |
| "odd width" | **REFUTED** | §3 width sweep, 16/16 exact |
| "odd height at 4:2:0/4:4:4" | **REFUTED** | §3, 4:2:0 and 4:4:4 at 241/242/243 all exact |
| a decoder-side *class* already fixed today (llinter, llinter2, oddheightfork3, …) | **refuted** | those arms are inter-path or var-tx-path; this is intra chroma in a key frame, and luma is exact |

---

## 6. Geometry actually measured for the failing cells

| cell | `max_frame` | bit depth | luma plane | chroma plane (4:2:2) | our internal padded extent | oracle FINAL frame |
|---|---|---|---|---|---|---|
| `AB_inter_warp_odd` | 322x242 | 8 | 322x242 = 77 924 | 161x242 = 38 962 ×2 | 328x248 + 164x248×2 (`mi_cols=2·⌈322/8⌉=82`, `mi_rows=2·⌈242/8⌉=62`) | 155 848 B |
| `O_odd322x242` | 322x242 | 8 | 77 924 | 38 962 ×2 | as above | 155 848 B |
| `Q_odd320x242` | 320x242 | 8 | 77 440 | 38 720 ×2 | 320x248 + 160x248×2 (already mi-aligned) | 154 880 B |
| `S_odd326x242_10b` | 326x242 | 10 (u16 LE) | 78 892 | 39 446 ×2 | 328x248 + 164x248×2 | 315 568 B |

Coded height == display height == 242 for all four (no superres, `sfr=0`);
`chroma_h = 242` (4:2:2 subsamples x only); `242 = 128 + 114`, so SB row 1 is a
114-row partial row and mi row 61 (luma 244-247) lies entirely outside. **None
of the wrong samples is in that partial row** — they sit at luma rows 160-207.
That is a second, independent reason height is not the factor.

---

## 7. Gate-ability: NO. Stated plainly.

4:2:2 is refused at the **sequence header** —
`crates/ec-av1/src/stream.rs:1803`
`if seq.subsampling_x != seq.subsampling_y` — so **no committed fixture and no
committed test can reach this code**. Every measurement in this report required
patching that guard out. There is no committed 4:2:2 fixture to gate against and
none can be added without lifting the refusal, which is the product decision the
brief excludes from this lane. I am not offering a source-scan substitute.

**What a future lane that lifts the refusal would need**, measured:

* `ll422_*.obu`-style 4:2:2 fixtures at **322x242 and 320x242** (the two
  geometries that carry the bug in this corpus) — `O_odd322x242.obu` and
  `Q_odd320x242.obu` (21 490 B and 21 381 B) are the obvious candidates; they are
  in `/home/tahinli/.cache/cells/av1422lpf/lossy_all/`, not in `fixtures/`.
* A byte-exactness gate per decode frame per plane, plus a mutation proof (flip
  one chroma sample of a palette block / one `chroma_left_available` term and
  watch frame 0 go red).
* **A reproducer recipe is still missing**: 42 fresh 4:2:2 encodes across
  8 widths × 9 heights × 3 chroma formats did NOT reproduce the failure (§3), so
  the encoder setting that produces it is unknown. Without it, a committed
  fixture alone would pin the symptom without a recipe — committable, but not
  reproducible from scratch.

---

## 8. Regression

`cargo test -p ec-av1 --lib -- 420 422 444 lossless warp intra --skip
bitrate_target_lands_within_5_percent_over_48_frames` on the reverted,
bypass-free lane tree at `81a21c7d`, with
`EC_AV1_AOMDEC=/home/tahinli/.cache/aom-oracle/build/aomdec`:

**`test result: ok. 181 passed; 0 failed; 2 ignored; 0 measured; 618 filtered out; finished in 263.05s`**

**A first run of this exact command reported `175 passed; 6 failed`.** All six
are gates that spawn the external `aomdec` and stage their stream in
`std::env::temp_dir()`. The panic was
`writing the stream: Os { code: 122, kind: QuotaExceeded, message: "Disk quota exceeded" }`
— the local `/tmp` **tmpfs** was at 80% of its 16 GB quota, not a decode
regression. `rm -rf /tmp/ec-av1-*` freed it, the four named tests then passed
individually (4/4), and the whole suite returned the 181/0 baseline above.
Recorded because the class reads exactly like a red gate
(`local-edquot-rust-build-tests`).

This lane changed no source, so this is a baseline confirmation, not
fix-verification.

## 9. Handover for the next lane

1. **The first thing to instrument is `chroma_left_available` / `have_left` in
   our own decoder for the chroma of luma mi(40,58)** (`Q_odd320x242` frame 0,
   chroma `(116,160)`): print `have_left`, the left column actually gathered,
   and the DC, next to the oracle's `EC_PRED … have_left=… n_left=…` line for the
   same unit. Ours currently says "left available" (DC 182 = above+left);
   libaom's own 202 says "above only". **That single boolean is the whole
   measured delta at the seed.**
2. **The block ownership of chroma col 115 rows 160-163 is unverified.** The
   oracle prints **no** intra prediction for luma mi(40,58) at all, and no
   `EC_PALSYN_AOM` line for it, while its 4 neighbour blocks at mi(40,54/56/60/62)
   all print. Whether that is a palette block (no print), a rung that does not
   fire on this block shape, or a genuine block-structure difference between the
   two decoders is **unresolved** and is the other half of the seed. This is the
   documented `av1-trace-label-mismatch-class` trap; do not trust a line-for-line
   ladder diff here.
3. **libaom's intra per-plane walk** (`av1/decoder/decodeframe.c:995-1022`) rounds
   the unit grid **per axis**: `unit_width = ROUND_POWER_OF_TWO(AOMMIN(…), pd->subsampling_x)`,
   `unit_height = ROUND_POWER_OF_TWO(AOMMIN(…), pd->subsampling_y)`, and
   `blk_row` starts at `row >> ss_y` (`= row` at 4:2:2). Every 4:2:2 chroma extent
   in our decoder that is *not* written as an explicit per-axis pair is suspect.
4. **Find the encoder setting.** The four failing cells are `aomenc`-built with
   `base_q=25`; 42 `ffmpeg`+`libaom` encodes across 8 widths × 9 heights × 3
   formats are all exact. A cpu-used / `--enable-cdef` / `--lag-in-frames` /
   `--tile-*` / `--usage` sweep on `aomenc` (the local binary rejects `--crf`;
   use `--cq-level` and a y4m input) is the cheapest next step and is what makes
   the defect reproducible, hence gate-able.
5. **The 4:2:2 intra-chroma DC/availability class is worth a repo-wide sweep**,
   not just these cells: a `dc_top` where libaom used `dc` (or the reverse) is a
   ±1-to-±20 sample class that stays invisible to every 4:2:0 gate.

## 10. Handover hygiene

* Lane tree `/home/tahinli/.cache/wt/av1422llodd` on `lane-av1422llodd`: **clean**
  apart from this report, the bypass probe and the temporary `EC_AV1_TRACE`
  print both reverted.
* Primary checkout `/home/tahinli/Documents/Code/Rust/edith_codecs`:
  `git status --porcelain` **empty**. No relative-path leak.
* No push, no merge, no rustfmt. The 4:2:2 sequence-header bypass is **not**
  committed anywhere.
