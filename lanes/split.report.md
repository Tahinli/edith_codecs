# lane-split — the split decision's own calibration: swept, and the control wins

lane-txd left one live hypothesis behind its refuted arm (class
`rd-rate-term-calibration`): the correct `tx_depth` price is a one-sided push
AWAY from splitting, so maybe what is mis-calibrated is the weight the split
trial's rate is judged under, not the price. This lane restored the correct
pricing (`60c3cdf6` + its pins), built the knob and the census, and swept.
The sweep is single-peaked and its peak IS the control weight, and every
point on it is worse than the row-0-priced control. The pricing fix goes back
out again; the census and the swept knob stay.

## The knob

`EC_AV1_SPLIT_LAMBDA` (`encode::split_lambda_mult`, constant `SPLIT_LAMBDA =
1.0`) multiplies the RATE term of the transform-SPLIT trial only — the intra
`tx_depth` depth>0 candidates in `code_square` and the inter var-tx depth-1
trial in `commit_inter_luma`. Below 1 the split is charged less for its bits
and fires more; above 1, less. The flat arm is untouched, so this is exactly
the depth-0-versus-split margin and nothing else.

## Sweep — film B gate row, WITH the correct `tx_depth` pricing on

Deciding gate, 12-frame native, BD-rate vs libaom cpu-used 6 / rav1e speed 6.

| `EC_AV1_SPLIT_LAMBDA` | film B | bars 2160p |
|---|---|---|
| 0.6 | +28.4 / +0.5 | +10.5 / −11.8 |
| 0.8 | +26.1 / −0.9 | +10.0 / −12.2 |
| **1.0 (control weight)** | **+25.2 / −1.6** | **+8.7 / −13.4** |
| 1.25 | +25.9 / −1.1 | +8.9 / −13.2 |
| 1.5 | +26.2 / −0.9 | +9.3 / −13.0 |
| 2.0 | +26.3 / −0.8 | +11.7 / −11.4 |
| — control, row-0 pricing (`2caf213b`) | **+24.9 / −1.9** | +8.7 / −13.4 |

The λ=1.0 row reproduces lane-txd's arm to the decimal, so the sweep is
against a reproduced control. The curve has ONE optimum, it sits at the
weight the encoder already uses, and it is still 0.3 worse on both columns
than the row-0-priced control. No re-weighting of the split's rate recovers
what correct pricing costs — so the split trial's λ was not the mis-calibrated
term either, and the class `rd-rate-term-calibration` reading of lane-txd's
result is refuted on its own terms.

## Census 1 — the split DECISION (`EC_AV1_SPLIT_CENSUS=1`)

Film B gate crop, 12 frames, q150, the shipped (row-0-priced) encoder. Rows
are TRIALS OFFERED by the RD search, which includes partition candidates that
later lose — not coded blocks; the coded-block view is census 2 below.

```
site  frame  side  offered   split  share%  win_rate  win_dist  lose_rate  lose_dist
intra key      16     7504    1671   22.3      19.07       335      25.97        444
intra key      32     1876     166    8.8      62.25      1381      78.58       1553
intra inter     8    12424    4620   37.2       9.26       362      10.26        533
intra inter    16     4704     942   20.0      23.29      1714      28.73       2109
inter inter    16     1711     167    9.8      14.96      1480      21.14       1741
inter inter    32      936     250   26.7      37.86      5869      38.32       6302
inter inter    64     5280     483    9.1      15.77      8333      16.02       8461
```

`win_rate`/`lose_rate` are bits (including the `tx_depth` / `txfm_split`
symbols), `win_dist`/`lose_dist` squared error. Read: the winner is cheaper on
BOTH terms in every row — the loser is never a rate-versus-distortion trade
the weight could tip, which is the same thing the sweep says from the outside.

With the correct pricing ON the shares fall exactly where lane-txd's price
table predicts (intra key 16: 22.3% → 21.0%, intra inter 8: 37.2% → 32.9%,
inter var-tx 32: 26.7% → 27.9%) — a 4-point drop on the inter frame's intra
blocks, which are the 66% ctx-2 population.

## Census 2 — ours vs libaom, coded blocks, same crop and rate

Both streams read by OUR decoder (`EC_AV1_BITCENSUS=1 syntax_census`), film B
gate crop 1920x1024 @ 00:40:00, 12 frames, `-g 12`. Ours q150 = 27343 payload
bytes; libaom `-cpu-used 6 -crf 30` = 28590. Split share = the share of coded
blocks whose PUBLISHED luma transform is smaller than the block, read as the
mass the tx-size histogram moves down against the block-size histogram.

| | key frame | whole stream |
|---|---|---|
| ours | 4.8% | 1.0% |
| libaom crf 30 | 7.6% | 2.1% |

libaom splits about twice as often as we do at the same rate — so we are on
the LOW side of libaom's split rate, and the sweep says every step toward
libaom's rate (λ 0.8, 0.6) costs 1.2 and 3.5 BD points on film B. The split
rate is not where film B's +25% lives; a lane that goes after it by moving
this margin is measuring the wrong knob.

## Gate — the shipped tree

Encoder output is BYTE-IDENTICAL to `2caf213b` (both stream pins green and
UNMOVED at 8325 / 33014, at the default preset and at `EC_AV1_SPEED=6`), so
the rows are the control's, re-measured here:

| row | shipped |
|---|---|
| film A | +21.0 / −4.8 |
| film B | +24.9 / −1.9 |
| screen | +14.5 / −33.2 |
| bars 1080p | −2.0 / −17.6 |
| bars 2160p | +8.7 / −13.4 |

`bd_rate_film_long_gop` was not run: nothing ships that moves a byte.

## Suite

* `--skip stream::` 338 passed / 0 failed / 32 ignored; `stream:: --skip
  10bit` 200 / 0 / 15; `10bit` 42 / 0 / 1. All RC=0.
* `--ignored encoder::tests::every_speed_preset_decodes_sample_exact_through_both_decoders`
  passed.
* `encode::tests::the_split_census_buckets_are_distinct_and_in_bounds` — the
  census index enumerated over its whole domain (class
  `enumerate-table-domain`).
* `timeout 900 cargo check --workspace --all-targets -j4`: 0 errors, 0 ec-av1
  warnings.

## Disposition

`deferred: the tx_depth context pricing — built twice (lane-ctx2, lane-txd),
now swept against its own split weight and still losing at every point. What
would unblock it is NOT another calibration knob on this margin: the census
says the losing arm is worse on rate AND distortion together, so the depth
trial is choosing between two arms the search already separates cleanly.
Whatever film B's +25% is, it is not the transform-split margin.`

`deferred: a bug found while building the census — encoder.rs used
bool::then_some to build a Drop guard, which CONSTRUCTS and then DROPS the
guard when the flag is off. Fixed here (then_some -> then); swept the class: the crate's
other 9 `then_some` calls all pass a reference or a `Copy` scalar, none of
which has a `Drop` impl.`
