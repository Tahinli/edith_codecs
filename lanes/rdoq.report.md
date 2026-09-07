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

| row | control (`EC_AV1_RDOQ=0`) | RDOQ, lambda x1.0 | lambda x0.7 |
|---|---|---|---|
| bars 1080p | +2.2 / -14.0 | -0.4 / -16.3 | +0.1 / -15.9 |
| bars 2160p | +12.7 / -9.8 | +10.5 / -11.7 | +10.9 / -11.4 |
| film A | +37.1 / +7.3 | **+26.3 / -0.5** | +27.6 / +0.3 |
| film B | +52.5 / +22.3 | **+37.5 / +9.7** | +39.5 / +11.6 |
| screen | +33.4 / -23.8 | **+29.1 / -25.7** | +29.6 / -25.5 |

The control row-for-row reproduces the charter's numbers on this head, so the
baseline is not stale. The keep rule (both film rows down on both columns,
screen not worse by 0.3) passes with room: film A -10.8 / -7.8, film B
-15.0 / -12.6, and the screen capture comes DOWN on both columns too.
lambda x0.7 is worse than x1.0 on every row.
