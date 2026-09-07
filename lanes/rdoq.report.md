# lane-rdoq — rate-distortion optimised quantisation

Head: off main `bfb88c88`. Scope: the coefficient quantiser and its two call
sites in the search; no decoder change.

## What was missing

`transform::quantize` rounded every coefficient on its own with a fixed
deadzone, blind to what the level costs through the coefficient CDFs. Every
reference has the pass that decides that against the entropy coder (libaom
`av1_optimize_txb`, rav1e `optimize_txb`). Coefficient bits are 53.6% of our
stream (`lanes/census-longgop-v2.md`).

## Instrument — what the quantiser hands the pass (`EC_AV1_RDOQ_CENSUS=1`)

Over every TRIAL block of a 12-frame native-gate row (four quantizers), i.e.
the bits at stake inside the search, not in the bitstream:

| row | blocks | coded coeffs | +-1 | trailing +-1 | zeroed by RDOQ | lowered | trial bits |
|---|---|---|---|---|---|---|---|
| bars 1080p | 11.26 M | 243.3 M | 63.5% | 3.6% | 5.47 M | 0.79 M | -2.50% |
| bars 2160p | 9.96 M | 225.8 M | 60.9% | 3.5% | 4.42 M | 0.74 M | -2.09% |
| film A | 30.68 M | 124.2 M | 68.7% | 14.2% | 10.11 M | 0.54 M | -10.31% |
| film B | 28.56 M | 68.5 M | 78.1% | 17.6% | 8.09 M | 0.31 M | -14.97% |
| screen | 13.92 M | 89.8 M | 61.8% | 6.8% | 3.45 M | 0.25 M | -5.31% |

Two thirds to four fifths of every coded coefficient is a `+-1`, and on the
two real films one coefficient in six is a `+-1` sitting in the run at the END
of the scan -- the levels an eob-shortening pass takes back whole.

## The pass (`tile::rdoq`)

* candidates per coefficient `{level, level-1, ..., 0}`, one step at a time,
  walked in REVERSE scan order so the tail is decided first and a dropped tail
  is retried against the shorter eob it leaves;
* every candidate priced by re-coding the whole block through `coeff_bits` --
  the same CDFs, q-context, neighbour `skip_ctx`/`sign_ctx` and `write_coeffs`
  the search already prices with and the writer will code with, so no second
  cost model can drift from the writer;
* distortion in the transform domain, mirroring the dequantizer: a level's
  pixel-domain error is `(coeff - level * q) / 8`
  (`INVERSE_GAIN_RECIPROCAL`), so `step^2 = (ac_q/8)^2` divides out of every
  comparison and the DC's `(dc_q/ac_q)^2` is the only per-position weight;
* lambda = the search's own `LAMBDA_SCALE` in those units
  (`EC_AV1_RDOQ_LAMBDA` multiplies it);
* run INSIDE the trial loop: the reconstruction, the price the search ranks by
  and the levels the writer takes are all the pass's grid, so the search
  decides on RDOQ'd blocks.

Bounded, because a candidate here costs a whole `write_coeffs` where rav1e's
incremental model costs a table lookup: levels above 2 are never candidates,
and a block spends at most 24 prices, taken from the tail backwards
(`EC_AV1_RDOQ_MAXLEVEL` / `EC_AV1_RDOQ_BUDGET`). Unbounded the q=150 pin is
9853 -> 8959 B at +256% wall on that clip; bounded it is 9059 B at +26%.

## 12-frame `bd_rate_screen_native` (1920-wide crops, 4 quantizers, vs libaom cpu-used 6 / rav1e speed 6)

| row | control (`EC_AV1_RDOQ=0`) | SHIPPED (lambda x2.2) |
|---|---|---|
| bars 1080p | +2.2 / -14.0 | -0.9 / -16.7 |
| bars 2160p | +12.7 / -9.8 | +9.3 / -12.8 |
| film A | +37.1 / +7.3 | **+24.4 / -1.8** |
| film B | +52.5 / +22.3 | **+35.5 / +7.6** |
| screen | +33.4 / -23.8 | **+28.1 / -25.7** |

The control row-for-row reproduces the charter's numbers on this head, so the
baseline is not stale. The keep rule (both film rows down on both columns,
screen not worse by 0.3) passes with room: film A -12.7 / -9.1, film B
-17.0 / -14.7, and the screen capture comes DOWN on both columns too.

### The rate-term sweep (`EC_AV1_RDOQ_LAMBDA`, BD vs libaom / vs rav1e)

| x | film A | film B | screen |
|---|---|---|---|
| 0.7 | +27.6 / +0.3 | +39.5 / +11.6 | +29.6 / -25.5 |
| 1.0 | +26.3 / -0.5 | +37.5 / +9.7 | +29.1 / -25.7 |
| 1.3 | +25.6 / -0.9 | +37.0 / +9.3 | +28.8 / -25.8 |
| 1.6 | +24.6 / -1.6 | +35.9 / +8.2 | +28.7 / -25.7 |
| **2.2** | **+24.4 / -1.8** | **+35.5 / +7.6** | **+28.1 / -25.7** |
| 3.2 | +24.7 / -1.5 | +36.1 / +7.6 | +29.4 / -25.0 |

One bowl, at the same multiplier on all three rows, and it is a factor of TWO
above the weight the mode search prices the same bits at. The likeliest cause
is named in `rdoq_lambda`'s doc comment: the price is taken against tables
that have not adapted yet, which over-states what a coefficient really costs,
and this is the one decision taken per coefficient.

### The deadzone, re-swept WITH the pass (`EC_AV1_DEADZONE`, at lambda x1.0)

| deadzone | film A | film B | screen |
|---|---|---|---|
| 0.42 | +48.1 / +20.7 | +55.7 / +23.8 | +29.0 / -25.6 |
| **0.5 (shipped)** | **+26.3 / -0.5** | **+37.5 / +9.7** | **+29.1 / -25.7** |
| 0.6 | +49.3 / +21.5 | +58.4 / +26.6 | +31.4 / -24.8 |

It was fitted without the pass and it survives it: both neighbours are 20+
points worse on both films. Rounding to nearest and letting the RD pass do
every zeroing is strictly better than a quantiser that guesses at it, in
either direction. 0.35 was NOT run: 0.42 is already 22 points the wrong way
and the arm below it can only be further out (deferred, one gate arm).

## Preset gate (`crate::speed::RDOQ`)

| row | preset 0 off | preset 0 on | preset 6 off | preset 6 on |
|---|---|---|---|---|
| film A | +37.1 / +7.3 (77s) | +24.4 / -1.8 (138s) | +46.0 / +14.5 (32s) | +30.4 / +2.9 (60s) |
| film B | +52.5 / +22.3 (78s) | +35.5 / +7.6 (130s) | +67.8 / +35.0 (30s) | +43.5 / +14.2 (51s) |
| screen | +33.4 / -23.8 (45s) | +28.1 / -25.7 (79s) | +40.3 / -20.0 (15s) | +34.9 / -22.0 (34s) |

About 2x wall, and it pays at both ends of the measured range. It also
DOMINATES the ladder: preset 6 with the pass beats preset 0 without it on
every row and both columns, at less wall. So the lever is on from preset 0 to
6; 7..=10 are the real-time rungs, are unmeasured here, and keep the cheaper
choice (`EC_AV1_RDOQ=1` turns it on there).

Wall caveat: every number above is one gate row with a second gate arm running
beside it on the same box (the lane runs two arms in parallel), which is what
the gate's own "noisy" label means. Best-of the five preset-0 RDOQ arms
(lambda 1.0/1.3/1.6/2.2/3.2, all the same work) is film A 131.7s, film B
118.6s, screen 78.5s against the control's 77.1 / 78.1 / 45.1s -- 1.7-1.8x.
