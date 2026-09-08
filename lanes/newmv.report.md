# lane-newmv — why we code NEWMV on a fifth of the blocks libaom does

Worktree `edith_codecs-newmv`, branch `lane-newmv` off main 6b95562d. Every
BD row is the prebuilt release lib-test binary (`bd_rate_screen_native`, 12
pictures, `gop 12`, four quantizers, native `gate_crop`; `bd_rate_film_long_gop`
for the 48-picture rows), each detached under its own `systemd-run` unit, read
off the log's own clip line.

## 0. The controls reproduce

| row | control (vs libaom / vs rav1e) | charter |
|---|---|---|
| film A, 12 frames | +21.0 / -4.8 | +21.0 / -4.8 |
| film B, 12 frames | +24.9 / -1.9 | +24.9 / -1.9 |
| screen, 12 frames | +14.5 / -33.2 | +14.5 / -33.2 |
| film A, long GOP | +25.2 / -7.2 | +25.2 / -7.2 |
| film B, long GOP | +86.7 / +7.3 | +86.7 / +7.3 |

All five land to the digit.

## 1. The instrument (`EC_AV1_NEWMV_CENSUS=1`)

Per block side: how often a `LAST` `NEWMV` is searched, how often the writer
can name the vector (offered to RD), how often it wins, the mean RD margin on
each side of the decision, the searched vector's distance from `NEARESTMV`,
how often it IS `NEARESTMV`, and its mv syntax bits. Then the two pre-RD
margins' skip shares, and the search's own bound witness (class `instrument at
bound`: how often the winner was walked to at the log stage's WIDEST step).
It rides the guard `EC_AV1_SPLIT_CENSUS` already had in
`Av1Encoder::encode_frames`, so it prints on every exit and costs nothing when
unset.

Film B, gate crop, 12 frames, q=90 — CONTROL:

| side | searched | offered | won | won% | win margin | lose margin | \|mv-nearest\| | mv==nearest | mv bits |
|---|---|---|---|---|---|---|---|---|---|
| 8 | 65296 | 65296 | 13319 | 20.4% | 47 | 26 | 16.48 | 61.3% | 5.90 |
| 16 | 25500 | 25500 | 9564 | 37.5% | 106 | 35 | 17.29 | 41.6% | 6.68 |
| 32 | 18344 | 18344 | 3627 | 19.8% | 137 | 32 | 9.69 | 54.1% | 5.82 |

pre-RD margin skips: extra-reference 86.2%, leaf second reference 4.2%.
search: 293394 calls, 69.2 evals/call, 6.35 integer rounds/call; winner within
±1 pel of `pred_mv` 54.9%, winner = starting seed 17.4%, moved at the widest
step 18.9% of calls.

READ IT AS:

1. **No margin drops LAST's `NEWMV`.** `searched == offered` in every row: the
   candidate reaches RD on every inter block. The two skip margins
   (`EXTRA_NEW_SKIP_MARGIN`, `LEAF_SECOND_NEW_MARGIN`) gate only the EXTRA
   references' searches. The charter's first hypothesis is refuted by the
   instrument.
2. **The vectors we do code are the right size.** 5.8-6.7 mv bits per CODED
   `NEWMV` against libaom's 3.2-7.5 per BLOCK: the whole census gap
   (0.83-2.5 mv bits/block) is the SHARE, not the precision.
3. **It loses by very little.** The mean losing margin (26-43 RD units) is a
   quarter to a third of the mean winning one — a near-tie decision, i.e. a
   population a small change in the search or the rate weight moves.
4. **The search is not at its own bound.** Only 18.9% of calls ever move at
   the widest log step, so the initial step / distance scale is not what
   stops it. But over half its winners sit within ±1 pel of `pred_mv` and
   41-61% of them ARE `NEARESTMV`.

## 2. The sweep (film B, 12 frames)

| arm | vs libaom | vs rav1e |
|---|---|---|
| control | +24.9 | -1.9 |
| `EC_AV1_MV_LAMBDA=0.5` (block RD mv rate x0.5) | +26.3 | -0.6 |
| `EC_AV1_MV_LAMBDA=1.5` | +25.3 | -1.7 |
| `EC_AV1_MV_SEARCH_LAMBDA=0.25` | +25.2 | -1.7 |
| `EC_AV1_MV_SEARCH_LAMBDA=0.5` | +24.9 | -2.0 |
| `EC_AV1_MV_SEARCH_LAMBDA=0.75` | +24.8 | -1.9 |
| `EC_AV1_MV_SEARCH_LAMBDA=2.0` | +26.9 | -0.2 |
| `EC_AV1_MV_SUBPEL_ITERS=4` | +25.2 | -1.7 |
| **`SUBPEL_ITERS=4` + `SEARCH_LAMBDA=0.5`** | **+24.8** | **-2.1** |

The chartered lever — the mv RATE term of the BLOCK decision — is
**single-peaked at the control on both sides** (0.5 loses 1.4/1.3, 1.5 loses
0.4/0.2). That is the third instance of class `rd-rate-term-calibration` on
this encoder (lane-txd, lane-split, here): our block-level pricing of a coded
vector is already RD-optimal on this content, so the finding the charter named
as the fallback holds — it is the SEARCH, not the pricing.

The two search-side knobs are new and both are one-liners:

* `EC_AV1_MV_SEARCH_LAMBDA` (`motion::MV_SEARCH_LAMBDA`): the search compares
  **SAD** while the lambda it is handed is the block RD's **SSE-domain**
  weight (`LAMBDA_SCALE * step * step`), so charging mv bits at the full RD
  lambda over-prices motion inside the search itself and pins the winner to
  `pred_mv`. Halving it is the floor of a clean bowl; doubling it costs 2.0
  points.
* `EC_AV1_MV_SUBPEL_ITERS` (`motion::MV_SUBPEL_ITERS`): each subpel stage ran
  ONE ±1-step round, so a winner two half-pels off the integer centre was
  unreachable (class `search bounded by its heuristic`). It re-runs from its
  own winner now, up to 4 times, stopping when a round does not move.

## 3. What ships

`MV_SEARCH_LAMBDA = 0.5`, `MV_SUBPEL_ITERS = 4`. `MV_LAMBDA` stays 1.0 (the
knob stays for the next sweep).

| row | control | shipped | move |
|---|---|---|---|
| film A, 12 frames | +21.0 / -4.8 | **+20.6 / -5.1** | -0.4 / -0.3 |
| film B, 12 frames | +24.9 / -1.9 | **+24.8 / -2.1** | -0.1 / -0.2 |
| screen, 12 frames | +14.5 / -33.2 | +14.6 / -33.2 | +0.1 / 0.0 |
| film A, long GOP | +25.2 / -7.2 | **+24.7 / -7.5** | -0.5 / -0.3 |
| film B, long GOP | +86.7 / +7.3 | +86.8 / +7.4 | +0.1 / +0.1 |

Keep rule met: both film rows improve on both columns at 12 frames; at long
GOP film A is 0.5/0.3 down with film B flat inside ±0.3; screen is 0.1 worse,
inside the 0.3 the rule allows. Wall (ours, same arm's own line): film A 146.5
-> 150.4 s, film B 123.2 -> 131.4 s, film A long GOP 564.9 -> 607.0 s, film B
long GOP 417.2 -> 437.7 s — +2.7% to +7.4%, under the 15% ceiling.

Decomposed on film A (12 frames): each half alone reads +20.8/-4.9, the pair
+20.6/-5.1 — they compose. On film B the subpel iteration ALONE loses
(+25.2/-1.7) and only pays with the lighter search rate term beside it, which
is why the pair ships as a pair.

## 4. The mechanism moved, not just the bytes

Same census, same clip and quantizer, after the change:

| side | won% (was) | mv bits (was) | mv==nearest (was) |
|---|---|---|---|
| 8 | **30.7%** (20.4) | 7.82 (5.90) | 36.4% (61.3) |
| 16 | **46.5%** (37.5) | 8.21 (6.68) | 21.6% (41.6) |
| 32 | **23.3%** (19.8) | 6.76 (5.82) | 38.5% (54.1) |

The search now spends 81.7 evals/call (was 69.2) and its winner sits within
±1 pel of `pred_mv` in 42.6% of calls (was 54.9). The NEWMV share moves toward
libaom's 54-76% at 3.2-7.5 mv bits/block, i.e. the BD move is the mechanism
the census pointed at — not something else that happened to move bytes.

## 5. Pins

`the_encoders_own_streams_are_byte_identical_to_their_pins` re-taken with a
two-line comment: 8325 -> 8288 bytes at q=150 and 33014 -> 33090 at q=60.
Green at the default and at `EC_AV1_SPEED=6`. No other byte pin went red
(split suite below).

## 6. Gates

* split suite, release lib tests: s1 (`--skip stream::`) 339 passed / 0 failed
  RC=0; s2 (`stream:: --skip 10bit`) 200 / 0 RC=0; s3 (`10bit`) 42 / 0 RC=0.
* `--ignored encoder::tests::every_speed_preset_decodes_sample_exact_through_both_decoders`
  1 passed RC=0.
* `cargo check --workspace --all-targets`: 0 errors, 0 ec-av1 warnings.

## 7. What this lane did NOT do

* `deferred: allow_high_precision_mv` — the writer codes every frame with it
  off, so the finest vector the search can commit to is a quarter pel
  (`round_to_valid_mv` rounds the residual to an even 1/8-pel count). Whether
  1/8-pel vectors pay is unmeasured; it needs the header bit, the `mv_hp`
  symbols and a decode witness first, then this same gate.
* `deferred: the mv rate weight of the COMPOUND halves and of the 64x64 skip
  root` — `EC_AV1_MV_LAMBDA` reaches the single-reference `NEWMV` candidates
  only. Since the single-reference sweep came back single-peaked at 1.0,
  re-pricing the compound halves has no measured reason to be different.
* `deferred: a per-speed-preset value for MV_SUBPEL_ITERS` — it costs 5-7%
  wall at every preset, and the preset ladder's own wall/BD Pareto
  (lanes `software speed Pareto`) is where that belongs, not here.
