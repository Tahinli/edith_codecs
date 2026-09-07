# lane-arfcen — why a hidden ARF of ours costs what it costs

Instrument: `census::Frame` now carries `order_hint` + `ref_hints`, and
`examples/syntax_census` prints ONE FULL REPORT PER CODED FRAME under
`EC_CENSUS_PERFRAME=1` (display-order PSNR joined to the coded frame through
its own order hint, references named by display DISTANCE). Both streams are
read by the same decoder, so every number below is comparable.

Arming (film B, 48 frames, the long-GOP gate's own window and seek):

    EC_ENC_SS=00:40:00 EC_ENC_OUT=<dir> enc_probe <film B> gate 0 48 150
        -> 90674 B, byte-exact with bd_rate_film_long_gop's q=150 point
    ffmpeg ... -g 48 -c:v librav1e -rav1e-params speed=6:quantizer=Q:...
    EC_AV1_BITCENSUS=1 EC_CENSUS_PERFRAME=1 syntax_census <stream> source.yuv

rav1e's PSNR-matched point is **quantizer 110** (45.751 dB / 75452 B), not the
125 the charter assumed (44.990 dB / 51680 B): ours reads 45.473 dB / 90674 B.
Interpolated to our PSNR rav1e needs ~65.6 kB, i.e. **we are +38% at matched
quality**.

## 1. Level census, same 48 display pictures

| | frames | qindex | bytes | share | B/frame | PSNR-Y |
|---|---|---|---|---|---|---|
| ours key | 1 | 102 | 19321 | 21.6% | 19321 | 47.67 |
| ours top ARF (dist 8) | 6 | 118 | 48126 | 53.8% | 8021 | 46.01 |
| ours mid ARF (dist 4) | 6 | 142 | 10331 | 11.5% | 1722 | 45.79 |
| ours leaf | 35 | 162 | 11736 | 13.1% | 335 | 45.26 |
| rav1e key | 1 | 80 | 20763 | 28.0% | 20763 | 47.95 |
| rav1e top ARF (dist 4) | 11 | 104 | 42590 | 57.5% | 3872 | 45.97 |
| rav1e 2nd ARF (dist 2) | 12 | 122 | 6446 | 8.7% | 537 | 45.48 |
| rav1e leaf | 24 | 139 | 4290 | 5.8% | 179 | 45.69 |

## 2. ARF vs ARF at the SAME display position

| display | ours B/q/PSNR | intra% | comp% | skip% | NEW% | rav1e B/q/PSNR | intra% | comp% | skip% |
|---|---|---|---|---|---|---|---|---|---|
| 8  | 8449 / 118 / 46.13 | 10.3 | 4.2 | 50.3 | 35.8 | 5945 / 104 / 45.99 | 1.8 | 0.0 | 73.7 |
| 16 | 8889 / 118 / 46.18 |  8.8 | 6.2 | 47.7 | 34.5 | 4869 / 104 / 45.92 | 0.3 | 0.0 | 72.8 |
| 24 | 9897 / 118 / 45.61 |  8.3 | 13.4 | 47.7 | 31.2 | 5911 / 104 / 45.28 | 0.4 | 0.0 | 73.8 |
| 32 | 4941 / 118 / 46.62 |  1.9 | 6.6 | 51.9 | 41.5 | 2087 / 104 / 46.15 | 0.0 | 0.0 | 90.0 |
| 40 | 7023 / 118 / 46.04 |  3.3 | 2.8 | 47.7 | 38.5 | 2854 / 104 / 45.53 | 0.1 | 0.0 | 85.4 |
| 47/44 | 8927 / 118 / 45.50 | 4.9 | 6.9 | 48.1 | 39.1 | 3151 / 104 / 45.37 | 0.0 | 0.0 | 86.0 |

Reference sets: ours `LAST@-8 GOLDEN@-8` (the first ARF names the SAME picture
twice) then `LAST@-8 GOLDEN@-16/-24`; rav1e `LAST@-4 LAST2@-4/-8`. Coded mv
magnitude, our top ARF: 47.1% in 2-16 px, 40.0% in 16-64 px, 0.0% over 64 px;
rav1e's: 76.5% / 8.3% / 0.0%.

## 3. Do our expensive ARFs buy cheaper leaves? No.

Per display picture, the six positions our encoder codes as top ARFs against
the same six positions in rav1e's stream, and everything else:

| | our bytes | rav1e bytes | delta |
|---|---|---|---|
| key (1 picture) | 19321 | 20763 | -1442 |
| our 6 ARF positions | 48126 | 24817 | **+23309** |
| the other 41 pictures | 22067 | 28509 | -6442 |

So the WHOLE +15.4 kB gap (and 6 kB more) sits in the six pictures we code at
prediction distance 8; our non-anchor pictures are already 23% CHEAPER than
rav1e's -- though 0.3-0.4 dB worse (leaves 45.26 vs 45.69 dB).

## 4. Ranked mechanisms, and what each one measured

Every arm below is `enc_probe`'s q=150 and q=90 points on film B (48 frames,
the gate's window), read against the control's own rate/quality slope
(3.46 dB per decade of bytes between its two points), so "better" means
fewer bytes AT the arm's own PSNR.

| # | mechanism | bytes at stake | verdict |
|---|---|---|---|
| 1 | anchor density (rav1e anchors every 2 pictures, ours every 4) | 22.1 kB of leaf+mid | **REFUTED** |
| 2 | hidden-frame rate weight (ARF codes residual on 50% of area, rav1e 26-30%) | 48.1 kB of ARF | **REFUTED** |
| 3 | motion search widening at distance 8 (`MV_DIST_SCALE`, its 4x cap) | 48.1 kB of ARF | **INERT** |
| 4 | the temporal lambda map never ran on the pyramid path | the whole stream | **KEPT, small** |
| 5 | ARF q offset / key allocation | — | already swept (lane-arfq, lane-keyq) |

1. `EC_AV1_PYRAMID` density arms: `4:-32:12:-8:-48` 108567 B/45.809 dB and
   613170 B/48.283 (3.8% better at q150, 3.8% WORSE at q90), `4:-24:12:-8:-48`
   92200/45.504 and 535188/48.102 (0.4% better, 2.7% worse), `2:-32:12:-8:-48`
   127348/45.982 and 778014/48.492 (tie, 15% worse). Denser anchors pay only
   at the low-rate end and lose at the rates the gate is scored on -- the same
   verdict lane-pyr6 got for a fourth level, now with the mini-GOP-4 shape
   (rav1e's exact structure) measured too.
2. `EC_AV1_LAMBDA_HIDDEN` (a rate weight for hidden frames only, on top of
   the q-derived lambda -- the one knob the q offsets cannot express):
   1.5 -> 84513/45.297 (4.8% WORSE), 2.0 -> 81454/45.160, 3.0 -> 75372/44.910,
   0.5 -> 106859/45.733 (0.9% better at q150, 5.5% worse at q90). 1.0 is the
   optimum; our ARF's operating point is not mispriced.
3. `EC_AV1_MV_DIST_SCALE` x `EC_AV1_MV_DIST_CAP`: (0.5,8) 90534/45.471,
   (1.0,8) 90531/45.481, (2.0,8) 90526/45.473, (1.0,16) 90650/45.474 against
   the control's 90674/45.473 -- +-0.2% bytes, +-0.01 dB. The distance-8 ARF
   is NOT search-range-bound (and its coded mv histogram, 0.0% over 64 px, is
   the content, not a clamp).

## 5. The lever: the temporal lambda map was inert on every default stream

`Av1Encoder::encode_pyramid_inter` passed `&[]` as the lookahead window with
the comment "the pyramid path reorders pictures, so its own buffer is not a
lookahead window: the temporal lambda weighting is off on this path". The
pyramid has been the DEFAULT since lane-av1pyrdef, so the propagating tpl map
lane-av1tpl2/3 measured and shipped (`TPL_DEPTH` 8, `TPL_STRENGTH` 0.5) has
been running on nothing but the flat A/B arm ever since -- class
`tool disabled in every gate recipe`.

The fix is the window the group already holds: for a frame inside the leaf
run, its display-order successors in `pending`; for the group's top ARF (the
last picture of the group, whose successors are not buffered yet) the group's
own leaves in REVERSE display order, which are exactly the pictures that
predict backward from it. `EC_AV1_TPL_PYRAMID=0` restores the old behaviour.

Probe arms on film B (control = the old `&[]`):

| arm | q150 | q90 | vs control at equal PSNR |
|---|---|---|---|
| off (control) | 90674 B / 45.473 dB | 479686 B / 47.978 dB | — |
| on, depth 8 | 91206 / 45.511 | 482161 / 47.989 | -1.9% / -0.2% |
| on, depth 4 | 91453 / 45.506 | 481555 / 47.991 | -1.4% / -0.3% |
| on, depth 2 | 91328 / 45.489 | 481497 / 47.990 | -0.6% / -0.3% |

Wall is unchanged (101.5 s vs 101.2 s for the two-point ladder).
