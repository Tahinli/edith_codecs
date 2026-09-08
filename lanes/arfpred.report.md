# lane-arfpred — the top ARF's PREDICTION, and a temporal filter of its source

Worktree `edith_codecs-arfpred`, branch `lane-arfpred` off main `35874aef`.
Deciding gate `encode::tests::bd_rate_film_long_gop` (48 pictures, both film
rows, BD vs libaom `cpu-used 6` / rav1e `speed 6`). **The control was RUN, not
quoted: film A +23.5 / −8.0, film B +85.3 / +6.7 — the charter's numbers to
the digit.**

## 0. The matched-rate point (film B, 48 pictures, the gate's own window)

| stream | bytes | PSNR-Y |
|---|---|---|
| ours `q150` (this HEAD) | 76409 | 45.564 |
| rav1e `speed 6 quantizer 110` | 75452 | 45.751 |
| libaom `cpu-used 6 crf 34` | 70345 | 46.539 |

Ours is +1.3% bytes for −0.19 dB against rav1e; every table below is read by
OUR decoder (`EC_AV1_BITCENSUS=1 EC_CENSUS_PERFRAME=1 syntax_census`), so all
three streams are measured by the same reader. `syntax_census` now prints
per-plane PSNR (`PSNR-Y ... U ... V`) per coded frame.

## 1. Per-level census — bytes per FAMILY per frame (B/frame)

| level | n | q | B/frame | PSNR-Y | U | V | intra% | comp% | skip% | NEW% | blocks/f | coeff | literal | mode | mv | part |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **ours top ARF** | 6 | 118 | **6992** | 45.88 | 51.60 | 51.91 | 4.7 | 20.4 | 61.4 | 38.7 | 2196 | 3483 | 1151 | 1323 | 700 | 281 |
| **rav1e L1 ARF** | 11 | 104 | **3872** | 45.97 | 51.61 | 52.01 | 0.5 | 0.0 | 80.7 | 26.0 | 1507 | 1996 | 730 | 608 | 410 | 125 |
| ours mid ARF | 6 | 142 | 1354 | 45.74 | 51.69 | 51.96 | 0.4 | 58.1 | 88.9 | 41.0 | 954 | 343 | 104 | 450 | 356 | 92 |
| rav1e L2 | 12 | 122 | 537 | 45.48 | 51.60 | 52.02 | 0.1 | 52.7 | 98.7 | 21.1 | 732 | 60 | 20 | 228 | 186 | 41 |
| ours leaf | 35 | 162 | 267 | 45.43 | 51.60 | 51.96 | 0.0 | 56.5 | 98.0 | 27.5 | 552 | 24 | 6 | 142 | 73 | 18 |
| rav1e leaf | 24 | 139 | 179 | 45.69 | 51.55 | 52.06 | 0.0 | 62.5 | 99.8 | 16.7 | 528 | 6 | 1 | 103 | 55 | 11 |
| ours key | 1 | 102 | 15827 | 47.34 | 51.94 | 52.26 | 100 | 0 | 0 | — | 1479 | 10666 | 3788 | 1046 | 0 | 148 |
| rav1e key | 1 | 80 | 20763 | 47.95 | 51.94 | 52.45 | 100 | 0 | 0 | — | 1266 | 14646 | 5102 | 828 | 0 | 140 |

**WHERE THE +3120 B/frame GOES** (top ARF, ours − rav1e): residual
(coeff+literal) **+1908 B = 61%**, mode+partition **+871 B = 28%**, mv
**+290 B = 9%**.

Three readings the charter asked for, in one line each:

* **NOT the quantizer.** rav1e codes its anchor at a FINER qindex than ours
  (104 vs 118) and still spends 45% of our bytes for +0.09 dB. Nothing on the
  q axis explains this (and lane-arfq refuted the offsets twice).
* **NOT chroma.** Per-plane PSNR at the anchor is 51.60/51.91 ours against
  51.61/52.01 rav1e's — matched to a hundredth. The whole deficit is luma.
* **NOT intra fallback.** Our anchor codes 4.7% of its AREA intra where
  rav1e codes 0.5% (lane-arfcen read 8-10% before the merges since), so the
  ceiling of an intra-decision lever at this level is a few percent of one
  level — the same order as its own noise.

What is left is the residual, and its two possible causes are the anchor's
LAG (we predict across 8 pictures, rav1e across 4) and the SOURCE the
residual is coding.

## 2. `arf_pred_census` — pricing lag against source grain (new instrument)

`encode::tests::arf_pred_census` (`--ignored`, 6 s, no encoder change): every
16x16 luma block of every ARF position of the gate's own 48-picture window
gets the SAME log-step diamond search (`last2_census`'s) against the picture
8 back, the picture 4 back, and — with the block temporally filtered first —
the picture 8 back again.

| clip | ARFs | lag 8 SAD/px | lag 4 SAD/px | tf lag 8 SAD/px | tf removes | filtered-vs-source PSNR |
|---|---|---|---|---|---|---|
| film A | 5 | 2.484 | 2.369 | 2.337 | 5.9% | 48.30 dB |
| film B | 5 | 1.199 | 1.114 | 1.059 | 11.7% | 50.14 dB |

(strength 30, the two DISPLAY-PAST neighbours — the only ones the encoder's
own group buffer holds when it codes its top ARF. With ±2 both sides it is
6.8% / 14.3% at 47.74 / 49.54 dB; strengths 10/30/60/120 are in
`$HOME/.cache/arfpred/predsweep.log`.)

**THE LAG IS NOT THE CAUSE.** Halving the anchor's prediction distance —
rav1e's whole structural advantage at this level — is worth 7.1% of the
prediction SAD on film B and 4.6% on film A, where the byte gap is 81%. What
our anchor's residual codes is mostly SOURCE GRAIN, which no motion search at
any lag can predict; a motion-compensated average of the neighbours removes
1.6x more of it than the lag does.

## 3. The lever: `arf_temporal_filter` (libaom's `arnr` in the small)

Built (`encode::arf_temporal_filter`, `speed::ARF_TF`,
`EC_AV1_ARF_TF=<strength>`): the top ARF's source picture is replaced by a
motion-compensated average of itself and its two group-buffered display
neighbours, weight `exp(-mse / strength)` per 16x16 block, chroma on the
halved luma vector with its own weight sum. **rav1e 0.8.1 has NO temporal
filter at all** (`grep -rn "temporal_filter\|arnr" rav1e-0.8.1/src` — empty),
so this is a libaom-only mechanism, not the thing rav1e beats us with.

Probe ladder, film B 48 pictures, `q150` and `q90`, each arm judged at the
CONTROL's own PSNR through the arm's own two-point slope (arfcen's rule):

| strength | q150 B / dB | q90 B / dB | slope dB/doubling | bytes at 45.564 dB | vs control |
|---|---|---|---|---|---|
| 0 (control) | 76409 / 45.564 | 346678 / 47.838 | 1.042 | 76409 | — |
| **3** | 72278 / 45.498 | 308172 / 47.759 | 1.081 | **75378** | **−1.35%** |
| 8 | 71636 / 45.413 | 308366 / 47.669 | 1.071 | 78996 | +3.4% |
| 15 | 70975 / 45.351 | 312607 / 47.606 | 1.054 | 81657 | +6.9% |
| 30 | 71814 / 45.246 | 321413 / 47.533 | 1.058 | 88453 | +15.8% |
| 60 | 73252 / 45.158 | 331390 / 47.470 | — | — | worse still |

The filter does exactly what the census said it would — 6-7% of the stream's
bytes come off at strength 15-30 — and the BD metric takes it straight back,
because the gate scores PSNR against the UNFILTERED source and the leaves
still carry the grain the anchor no longer has. Note the non-monotone byte
column: past strength 15 the stream gets BIGGER again, which is the leaves
paying for a reference that no longer resembles their own sources.

