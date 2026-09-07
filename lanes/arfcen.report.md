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
