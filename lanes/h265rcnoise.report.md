# lane/h265rcnoise: Bitrate mode on near-black with temporal noise

## Root cause (refined)

The cliff is real, but the dive into it comes from two things together: the `2^(QP/6)` model
extrapolating a cheap picture, and the leaky bucket. On the noise clip, 29 predicted pictures at
QP 26..30 cost ~90 B each, so the bucket holds a large credit. At the next IDR (coded around QP
20) the first P picture takes the bucket term (up to -12 QP) on top of the step clamp and lands at
QP 10: one 41 KB picture, then a climb back through 21 KB (QP 11), 2 KB (QP 14) and so on. That
repeats every GOP.

## Design (crates/ec-h265/src/enc_rc.rs, P pictures only; IDR and ConstantQp untouched)

1. `MAX_DESCENT = 4`: after the bucket, a P picture is at most 4 QP below the previous P
   picture. The bucket can no longer turn saved bits into a multi-step dive; a cliff is met one
   observed step at a time. A flat clip still reaches QP 0 in about 7 pictures.
2. Observed cliff floor: `seen[qp]` keeps the bits and index of the last P picture coded at each
   QP. Cost is monotone in QP, so a P picture at QP q that cost more than
   `CLIFF_RATIO (1.5) * target + CREDIT_SHARE (0.8) * bucket credit` means no QP <= q is
   chosen for `CLIFF_AGE (120)` P pictures. Spending credit on one expensive picture is allowed
   (the credit term), a plain overspend is not. The table and picture count are carried across
   GOPs by `continue_from`, so the output is still independent of the worker count.

Constants are documented in the source.

## Results (1280x720, 30 fps, 150 frames, keyint 30, server host c)

| Source | Target | Before | After |
|---|---|---|---|
| moving | 500 | +2.3% | +2.3% (511.5) |
| moving | 2000 | -0.6% | -0.6% (1987.5) |
| moving | 6000 | -1.1% | -1.1% (5936.8) |
| noise | 500 | +14.2% (QP 10..47) | +0.8% (504.1, QP 10..27) |
| noise | 2000 | +8.1% | -0.05% (1999.1) |
| noise | 6000 | -1.9% | -1.0% (5941.4) |
| flat | 500/2000/6000 | 40.5 kb/s, QP 0 | 40.4 kb/s, QP 0 |
| mandel | 500 | +5.5% | +5.5% (527.4, QP pinned 51) |
| mandel | 2000 | +2.5% | +2.7% (2053.4) |
| mandel | 6000 | -0.4% | -0.5% (5971.3) |

Attempts: (1) cliff floor alone, 24-picture memory: noise +7.4/+7.9/-5.2, the observation expired
before the next GOP. (2) Memory 120 with a hard ratio: undershoot -19/-32/-15 (floor never
lets the bucket spend). (3) Credit-aware floor: -2.7/+0.9/-1.6 (still a one-off hit per GOP
start before the first observation). (4) Added the descent cap: above table.

## Tests

`noisy_near_black_lands_within_five_percent` (tests/rate_control.rs): 720p, 90 frames, PRNG
noise 14..18 around 16, targets 500 and 2000 kbps, within 5%. Before the change: 500 kbps is
+11.84% (fails). After: +2.38% and -0.29%. The cliff does not show at 320x240 (baseline there is
within +4.9% at every target tried), hence 720p; it runs in a few seconds.

Diff: `crates/ec-h265/src/enc_rc.rs`, `crates/ec-h265/tests/rate_control.rs`, this report.
