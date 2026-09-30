# lane-av1422llinter — the residual divergence on the 16-frame lossless 4:2:2 cells

**Outcome: localised and isolated to a single chroma format, but NOT fixed and NOT
gate-able.** The first offending msac read is named and bounded; the site inside
`decode.rs` is not, and §7 says plainly why it cannot be gated from a committed test.

Tip: `main` = `ac00e9fb` (Merge lane-av1422llpred2). Worktree
`/home/tahinli/.cache/wt/av1422llinter`, branch `lane-av1422llinter`.
**No source change was made.** The only edit ever applied to this tree was the
4:2:2 sequence-header bypass probe, and it is reverted (`git status` clean).

---

## 1. Re-measurement on the tip (the numbers in the brief were partly stale)

Geometry first, because the brief's numbers invited a wrong reading: the cell is
**320x240**, not 300x256. Byte totals coincide (16 frames x 153600 B) because
4:2:2 luma is `320*240` and chroma `2 x 160*240`, so a 300x256 guess gives the
same frame size and silently mislabels every row/column. All row/column claims
below use 320x240.

Bypass patched in (`stream.rs:1803` `if false && seq.subsampling_x != seq.subsampling_y`),
`decode_probe` release build, compared per plane against the pre-existing oracle
`/home/tahinli/.cache/cells/av1422lpf/oracle/{W,X,Y}.yuv` (DISPLAY order, 16 frames).

Comparator: `/home/tahinli/.cache/lane-av1422llinter/cmp.py`, with an
**oracle-flip control** (`--flip-sample 0`) that XORs one oracle byte.

| cell | result |
|---|---|
| `W_intrabc.obu` (130320 B) | frame 0 byte-exact; **frame 1** Y 60575/76800, U 30740/38400, V 31932/38400 differ; first sample frame 1 Y (0,0) ours 74 / oracle 81 |
| `X_intrabc_tiled.obu` (131696 B) | same shape: frame 0 exact; **frame 1** Y 42325, U 19863, V 20461 differ, first sample Y (0,0) ours 74 / oracle 81 |
| `Y_intrabc_10b.obu` (202281 B) | **CORRECTION to the brief.** It does *not* refuse at frame 0. Re-measured on a **freshly built** bypassed binary in a separate detached worktree at the same commit: this cell is **300x256, 10-bit** (not 320x240). It emits **4 of the oracle's 17 decode frames** (`U.f0..U.f3`, 307200 B each) and then stops. Frame 0 byte-exact; **frame 1** first bad sample at byte 36. **Honest flag:** the brief's `unsupported: AV1 tile (a Golomb tail longer than this decoder reads)` **does not reproduce** — three re-runs of a freshly built bypassed binary (`EC_PROBE_OUT`, `EC_PROBE_OUT16`, and no env) produce **no message at all** and simply stop after 4 frames. I report the reproducible fact and flag the discrepancy rather than asserting either version. Whatever stops it, it is not a frame-0 refusal and the decoder is not completing the cell. |

**Flip control (both cells):** flipping oracle byte 0 adds **exactly 1** differing
byte, reported at frame 0 plane Y (0,0). The comparator bites.

### Display order vs decode order — the brief's frame 1 is NOT decode frame 1

`W_intrabc.obu` has **23 frame headers**: 17 coded frames (16 `show=true`, 6 of
those `show_existing`, so 10 newly-coded-and-shown) + 6 `show_existing` + 7
hidden. The 16-frame YUV is therefore **DISPLAY order**; `EC_AV1_FINAL_DUMP` is
**DECODE order** on both sides (`EC_AV1_FINAL_DUMP=<p>` -> `<p>.f<N>`, 17 files each).

Display slot -> decode index: `0->0, 1->4, 2->5, 3->6(se), 4->8, 5->9(se),
6->10, 7->11(se), 8->12, 9->15, 10->16(se), 11->17, 12->18(se), 13->20, 14->21(se), 15->22`.

**The divergence tracks the HIDDEN frames, not the shown ones** — exactly the
mis-localisation the brief warned about:

| decode frame | hidden? | result vs oracle (decode-order FINAL, 153600 B) |
|---|---|---|
| 0 | shown | **BYTE-EXACT** |
| **1** | **hidden (show=false)** | **138708/153600 differ — 90%** |
| 2 | hidden | 127180 |
| 3 | hidden | 83617 |
| 4 | shown (= display 1) | 123247 |

So the first bad frame is **decode frame 1 = the first inter frame = the first
frame that predicts from a reference slot**, and it is *hidden*. The brief's
"frame 1 is the first inter block" is right in spirit and wrong in index: the
first *shown* inter frame is decode frame 4.

**What frame 0 exercises that frame 1 does not:** frame 0 is `type=Key intra=true
intrabc=true` — pure intra + intrabc, no reference slot read, no MV, no
interp filter, no OBMC, no warp. Frame 1 is the first `type=Inter`: it is the
first frame that (a) fetches a reference slot, (b) reads a motion vector, (c)
applies an interp filter, (d) runs OBMC, and (e) applies warp. Frame 0 being exact
therefore exonerates the whole intra + intrabc path and says nothing about inter.

---

## 2. Stage attribution: every post-recon filter is INERT, proven

**Header evidence:** every frame of W has `deltalf=false`, `lf_level=[0,0,0,0]`,
`cdef_bits=0`, `lr=[None,None,None]`, `superres=false/8`. There is no loop
filter, no CDEF, no loop restoration and no superres to run.

**Measured evidence (both sides, decode frame 1).** Oracle `P`=PREFILT,
`Dk`=POSTDEBLOCK, `Cd`=POSTCDEF, `F`=FINAL:

* oracle: `F == P`, `Cd == Dk`, and `P != Dk` by 29775 B.
* **`Dk.f1 == P.f2` and `Cd.f1 == P.f2`** (byte-identical to the *next* frame's
  prefilter). The oracle's postdeblock/postcdef rungs are **frame-offset by one**,
  so the 29775 B is a rung-scheduling artefact, **not** a deblock. Oracle
  deblock/CDEF/LR: **inert**, proven by identity, not assumed.
* ours: `P == Dk == Cd` on f0..f3, i.e. our deblock and CDEF are no-ops too.
* **PREFILTER vs FINAL on decode frame 1 differs by 138708 B — the identical
  count to the FINAL-vs-oracle difference.** So the whole divergence is already
  present *before* any filter: prediction + residual. Deblock, CDEF, LR,
  superres and the deblock are each **exonerated with evidence**.

(Ours' `P`/`Dk`/`Cd` dumps are 163840 B = 320x256 Y + 160x256 U + 160x256 V,
plane-padded to 256 rows; the oracle's are 153600 B crop-to-240. Comparing the
first 153600 B of ours against the oracle is a stride mismatch and must be
cropped first — `cmpcrop.py` does that. A naive `cmp` there reports a fake
66825 B diff.)

---

## 3. Controlled single-variable isolation: it is 4:2:2 and nothing else

Same source content, same encoder, same settings, same 16 frames, same 320x240,
same `tx_mode=Only4x4`, warp + alt-refs + OBMC + ref-mvs all on, all lossless
(`base_q=0`), differing **only** in `-pix_fmt`:

| cell | bytes | result |
|---|---|---|
| `bisect/ll_420.obu` (4:2:0) | 156294 | **25/25 decode frames BYTE-EXACT** |
| `bisect/ll_444.obu` (4:4:4) | 314506 | **25/25 BYTE-EXACT** |
| `bisect/ll_wshape.obu` (4:2:2) | 203530 | frame 0 exact, **frame 1 bad at byte 160** |

This is a fresh reproducer built with **ffmpeg + libaom only** (no aomenc on this
host): `testsrc2 320x240 yuv422p -> libaom-av1 -crf 0 -b:v 0 -cpu-used 6
-usage good -lag-in-frames 2 -f obu`. Variants with `-lag-in-frames 0` and
`-enable-cdef 0` fail **identically** (frame 1, byte 160), so alt-refs and CDEF
are not involved.

**The same source losslessly encoded as 4:2:0 and 4:4:4 is exact. It is 4:2:2.**

---

## 4. Full 4:2:2 corpus census (`lossy_all/`, 27 cells)

`census.sh` decodes each cell on both sides with `EC_AV1_FINAL_DUMP` and reports
per-DECODE-frame byte-exactness. **22/27 are exact on every frame**, including the
ones a reader would guess are implicated:

| exact | not exact |
|---|---|
| `422_residual_compound_warp_16f` **16/16** (compound + warp + residual) | `W_intrabc` exact 1/17, first bad **decode frame 1** byte 32 |
| `AA_inter_compound` **43/43** | `X_intrabc_tiled` exact 1/17, first bad **decode frame 1** byte 32 |
| `AD_inter_nogm` 43/43, `AB_inter_warp_odd` 0/43 *(see §4b)* | `Y_intrabc_10b` exact 1/4, first bad **decode frame 1** byte 36 |
| `422_intrabc_sb128_strip` 5/5, `422_allskip_2f` 2/2, `422_sb128_3f` 3/3 | `O_odd322x242` 0/17 f0, `Q_odd320x242` 0/17 f0, `S_odd326x242_10b` 0/17 f0 |
| `A,B,C,D,E,F,H,I,J,K,L,R_odd322x240,T,U,V` all 17/17 or 16/16 | |

### 4b. A SECOND, unrelated defect is hiding in this census

`AB_inter_warp_odd`, `O_odd322x242`, `Q_odd320x242`, `S_odd326x242_10b` all fail
at **decode frame 0, byte ~103k-214k** — i.e. inside the **intra** key frame,
with **no** inter frame involved and no reference slot read. They are all
**242 pixels tall**; `R_odd322x240` (240 tall, odd *width* 322) is **17/17 exact**.
So that cluster is an **odd-HEIGHT** defect, not a chroma-format one, and it is a
different mechanism from the W/X/Y one. It is adjacent to `lane-av1oddheightfork3`
(closed today) and is **not** this lane's subject — flagging it, not claiming it.

---

## 5. The first divergent unit, and what it is not

### 5.1 Pixel onset (reproducer `ll_wshape.obu`, decode frame 1)

| plane | leading exact prefix | first differing sample | ours | oracle |
|---|---|---|---|---|
| Y | **160** | row 0, col 160 | 210 | 41 |
| U | 56 | row 0, col 56 | 17 | 16 |
| V | 49 | row 0, col 49 | 29 | 28 |

Wrong region: a clean staircase, 246/300 16x16 Y blocks — the exact region is a
staircase in the top-left and the wrong region grows down-and-right. Per-column
and per-row diff counts are ~uniform, so there is no systematic column or row
strip (an apparent "1-sample vertical strip" in an ASCII map was a misread; the
histogram is flat).

### 5.2 Prediction vs residual, separated by the residual-map identity test

Frame 0 is byte-exact on both sides, so it is a trustworthy reference. For a
lossless frame the residual is `prefilter(f1) - final(f0)`, and because these
blocks are zero-MV, that map is the true residual. Both sides' maps:

| plane | residual-map agreement | first residual-map difference |
|---|---|---|
| Y | 27543/76800 (35%) | (0,160): ours **+169**, oracle **0** |
| U | 13021/38400 (33%) | (0,56): ours +1, oracle 0 |
| V | 12707/38400 (17%) | (0,49): ours +1, oracle 0 |

At the seed the oracle's residual is **0** (its recon equals the reference
verbatim) and ours is **+169**. Because the block's decoded mode/ref/MV agree with
the oracle's (§5.3), the prediction formula is the same, so **the difference is in
the coefficients, not the motion compensation.**

**Method warning, recorded because it already bit once:** a first run of this
test passed *our own* prefilter as both sides' prediction and reported a bogus
"RESIDUAL MAPS IDENTICAL -> prediction is at fault". The correct pairing is
`UF.f0` (our ref) + `U.f1` (our pred) vs `OP.f0` + `OP.f1`.

### 5.3 The parse fork, from the paired `EC_MODE`/`EC_MODE_VAL` ladder

On this clean reproducer the two ladders **do share a label space** (mi in 4x4
units, same `mode`/`ref0`/`ref1`/`mv0`/`rng` fields; the oracle's rung is
`decodemv.c:1467/1479`, ours `decode.rs:40795`). Unlike the `noibc` cell — where
they carry 3846 vs 3400 lines and the mi labels transpose, the documented
`av1-trace-label-mismatch-class` trap — here they align element for element.

```
line 0   O  mi(0,0)  mode=13 ref0=1 mv=(0,0)  rng=35922
         U  mi(0,0)  mode=13 ref0=1 mv=(0,0)  rng=35922      <-- IDENTICAL
line 1   O  mi(0,4)  mode=13 ref0=1 mv=(0,0)  rng=43856
         U  mi(0,4)  mode=13 ref0=1 mv=(0,0)  rng=36912      <-- RNG FORK, values still equal
line 11  O  mi(0,20) mode=13 ref0=1 mv=(0,0)  rng=48823
         U  mi(4,16) mode=16 ref0=1 mv=(0,80) rng=50600      <-- STRUCTURE FORK
```

**The first divergent unit is the 2nd inter block of decode frame 1, mi (0,4) in
4x4 units = luma (0,16).** The decoded *values* of that block are identical on
both sides; the msac `rng` is not (43856 vs 36912). So the offending read lies in
the reads **between** block 1's `EC_MODE_VAL` and block 2's — i.e. block 1's
tx-size / transform-partition / coefficient region — and it desynchronises the
coder without changing the next few decoded values. By line 11 the *block
structure itself* forks, so the rest of the frame is unrecoverable.

**Why the pixel onset (col 160) is later than the parse onset (col 16):** the
first ten inter blocks are all `mode=13` (NEARESTMV) with `mv=(0,0)` and, on the
oracle, zero residual. Prediction is then the reference verbatim, so a desynced
coder is *invisible* in the pixels until the first block that actually carries a
nonzero residual. Pixel onset is therefore a lower bound on the parse fork, not a
contradiction of it.

---

## 6. The named mechanism, as far as the evidence carries it

* **Ours, `crates/ec-av1/src/decode.rs`, the inter coefficient/tx region of the
  first inter block** — bounded but not narrowed to a line. The reads between
  `EC_MODE_VAL` of inter block 1 and of inter block 2 are the block's
  transform-size / var-tx partition reads and the per-plane coefficient reads.
  The defect is 4:2:2-chroma-specific (§3), sits in the **inter** path (§1), and
  is invisible in intra/intrabc (§1) and in every filter (§2).
* **libaom, the mirror of that region**: `av1/decoder/decodetxb.c`
  `read_coeffs` / `read_transform_block` / `read_block_tx_size`, reached from
  `av1/decoder/decodeframe.c` `parse_decode_block` for an inter block.
* **What produces what is observed:** one extra or one missing msac read in the
  4:2:2 chroma coefficient region. That desynchronises the coder for the rest of
  the frame while leaving the next few symbols' *values* intact (a skewed CDF
  returns the same symbol from a different state), so the failure presents as
  "recon looks right for a while, then a block with a nonzero residual comes out
  wrong by a large amount" — which is exactly the +169 at (0,160) and the
  138708/153600 blow-up.
* **The 4:2:2-specific read this points at** is the chroma transform extent /
  coefficient unit for an inter block. AV1's smallest transform unit is 4x4
  *chroma* samples, which in 4:2:2 is **8x8 luma** (subsampling only in x); a code
  path that derives the chroma TU from a 4:2:0-shaped extent will read a
  different number of coefficients than libaom. **I did not confirm this at a
  line** — see §7.

**Alternative I could not exclude:** a CDF-table or `dc_sign`/`sign` context
selection that is chroma-shape-dependent, rather than a unit-count error. Both
produce "same values, different rng". Distinguishing them needs a paired
coefficient ladder whose unit boundaries line up; per `av1-coeff-ladder-shape`
that is a real project (four documented rung-shape asymmetries, and the raw
counts here are 1255383 vs 657583 `tag=base` reads on the `noibc` cell, i.e. the
unit decomposition genuinely differs). **Not attempted to completion.**

---

## 7. Gate-ability: NO. Stated plainly.

**This fix cannot be gated from any committed test, and I am not going to
describe it as gated.**

4:2:2 is refused at the **sequence header** — `stream.rs:1803`
`if seq.subsampling_x != seq.subsampling_y` — so no committed fixture and no
committed test can reach the inter coefficient path where this defect lives. To
observe it at all, the guard must be patched out. The proof I have is therefore
**measurement + a single-variable control**, which is the strongest evidence
available here and is *not* a regression gate:

* **Control (proves the comparator bites):** oracle-flip on both W and X adds
  exactly 1 differing byte, at frame 0 Y (0,0).
* **Control (proves it is 4:2:2):** one source, one encoder, one setting, three
  chroma formats — 4:2:0 **25/25 exact**, 4:4:4 **25/25 exact**, 4:2:2 bad at
  decode frame 1. Any non-chroma-format explanation dies here.
* **Control (proves it is inter, not intra):** decode frame 0 is byte-exact on
  both W and the reproducer; the fork is in decode frame 1's second inter block.
* **Control (proves it is not a filter):** prefilter already carries 100% of the
  difference, with filter-inertness proven by rung identity in §2.

**No source-scan substitute is offered.** A source-scan gate would be asserting
something about code text, not about the decoder's behaviour on 4:2:2, and this
lane's acceptance forbids dressing that up as a gate.

**The reproducer is nonetheless committable as bytes** — `ll_wshape.obu` is
203530 B, built from `ffmpeg + libaom` with no aomenc dependency, and §3's
command line is recorded above verbatim. A future lane that *lifts* the 4:2:2
refusal (the product decision is the user's, not this lane's) can turn it into a
byte-exactness gate immediately, and should mutation-prove it then.

---

## 8. What was refuted, and how

| claim | verdict | how |
|---|---|---|
| "a loop filter / CDEF / LR / superres stage carries it" | **refuted** | prefilter == the whole difference (138708 both sides); every header has `lf_level=0`, `cdef=0`, `lr=None`, no superres; oracle deblock/CDEF proven inert by `Dk.f1 == P.f2` |
| "the oracle's 29775 B postdeblock delta is a deblock" | **refuted** | it is a rung frame-offset: `Dk.f1` and `Cd.f1` are byte-identical to `P.f2` |
| "display frame 1 is the first bad frame" | **refuted** | display frame 1 = decode frame 4; decode frame 1 (hidden) is already 90% wrong |
| "the divergence is in the shown frames" | **refuted** | it tracks the hidden frames; decode frame 1 is `show=false` |
| "W is 300x256" | **refuted** | `max_frame=320x240`; the byte totals coincide, which is how the error hides |
| "frame 0 is the last exact frame because intra is broken late" | **refuted** | frame 0 is intra+intrabc and is byte-exact on all three planes on both cells |
| "it is a chroma-format-generic 4:2:2 inter bug" | **refuted** | same source/settings: 4:2:0 25/25 exact, 4:4:4 25/25 exact |
| "it is 4:2:0-vs-4:2:2 independent of losslessness" | **refuted** | `422_residual_compound_warp_16f` (4:2:2, 16 frames, compound+warp) is 16/16 exact, but it is **lossy** (`base_q=73..114`). Lossy 4:2:2 inter is fine. It needs **lossless** (`base_q=0`, `tx_mode=Only4x4`) **and** 4:2:2 **and** inter |
| "it is alt-refs or CDEF" | **refuted** | `-lag-in-frames 0` and `-enable-cdef 0` variants fail identically (frame 1, byte 160) |
| "residual is in sync, so it is a prediction bug" | **refuted** | that reading came from a **void test** that passed our own prefilter as both sides' prediction. Correct pairing: residual maps agree only 35% (Y); at the seed ours is +169 where the oracle is 0 |
| "it is one defect" | **partly refuted** | the census splits it: the W/X/Y cluster is lossless 4:2:2 inter; the `O_odd322x242`/`Q_odd320x242`/`S_odd326x242_10b`/`AB_inter_warp_odd` cluster fails at decode **frame 0** (intra, no reference slot) and is **odd-height**, with `R_odd322x240` (odd width, even height) exact. Two mechanisms. §4b |
| "the `EC_MODE` ladders can be paired on `noibc`" | **refuted** | 3846 vs 3400 lines, mi labels transpose at line 96 — but frames 0-1 of that cell are byte-exact, so a parse fork there is impossible; it is the documented label-space trap. Only the clean `ll_wshape` reproducer pairs |

---

## 9. Regression

```
cargo test -p ec-av1 --lib -- 420 422 444 lossless warp intra \
  --skip bitrate_target_lands_within_5_percent_over_48_frames
```

**`test result: ok. 181 passed; 0 failed; 2 ignored; 0 measured; 618 filtered out; finished in 290.40s`** (309 s wall including the build).

Run on the reverted (bypass-free) tree. No source change was made by this lane,
so this is a baseline confirmation, not a fix-verification.

---

## 10. Handover

1. **Reproduce in one command** (no aomenc needed):
   `ffmpeg -f lavfi -i testsrc2=size=320x240:rate=25:duration=1.0 -pix_fmt yuv422p -f rawvideo src.yuv`
   then `ffmpeg -f rawvideo -pix_fmt yuv422p -s 320x240 -r 25 -i src.yuv -c:v libaom-av1 -crf 0 -b:v 0 -cpu-used 6 -usage good -lag-in-frames 2 -pix_fmt yuv422p -f obu ll_wshape.obu`.
   Bypass the header guard, then `EC_AV1_FINAL_DUMP` both sides: frame 0 exact,
   frame 1 first bad at byte 160.
2. **Attack point:** the inter coefficient / tx-size reads of the **first inter
   block** of the first inter frame, 4:2:2 chroma only. Instrument with the
   `EC_COEFF_STEP` ladder on both sides and **count reads per unit before
   comparing values** (per `av1-coeff-ladder-shape`); the raw counts on the
   `noibc` cell were 1255383 vs 657583 `tag=base`, so the unit decomposition needs
   reconciling before any value diff means anything.
3. **The specific hypothesis to test first:** whether the chroma transform unit
   for an inter block is derived from a 4:2:0-shaped extent. In 4:2:2 the minimum
   chroma TU is 4x4 chroma = **8x8 luma** (subsampling in x only). A 4:2:0-shaped
   derivation reads a different coefficient count and desyncs the coder exactly as
   observed.
4. **Do not** treat §4b's odd-height cluster as part of this lane.
5. **Gating stays impossible** until the 4:2:2 refusal is lifted (§7).
