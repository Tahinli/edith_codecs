# lane-libcen — why libaom `cpu-used 6` is so far ahead on the long-GOP gate

Instrument: the gate's own window and recipe. Film B (2160p source, gate crop
1920x1024 at 00:40:00, 48 pictures, `gop 48`), libaom exactly as
`encode::tests::external_ladder` runs it (`ffmpeg -f rawvideo -r 24 -threads 1
-g 48 -c:v libaom-av1 -cpu-used 6 -b:v 0 -crf N -f obu`; there are no
`-usage`/`-lag-in-frames`/`-tile-*` flags in the gate). Every number below is
read by OUR decoder through `EC_AV1_BITCENSUS=1 EC_CENSUS_PERFRAME=1
syntax_census`, so both streams are measured by the same reader.
`examples/syntax_census` gained one line per level -- `tools (symbols/bits)` --
which names the coding tools a stream actually codes (class
`gate-blind-to-feature`).

## 0. The PSNR-matched point

Mean PSNR-Y over the 48 displayed pictures, computed by `syntax_census`:

| stream | bytes | PSNR-Y |
|---|---|---|
| ours q150 | 88290 | 45.663 |
| libaom crf 30 | 92335 | 46.976 |
| libaom crf 35 | 65546 | 46.415 |
| libaom crf 40 | 48740 | 45.856 |
| **libaom crf 41** | **45938** | **45.733** |
| libaom crf 42 | 43088 | 45.600 |

crf 41 is +0.070 dB of ours (inside the 0.15 dB band), so the matched pair is
**ours 88290 B vs libaom 45938 B: +92% bytes at matched quality**.

## 1. Level census, film B, the SAME 48 display pictures

libaom's structure is derived from the order hints: the key at 0, then a
`lag-in-frames` ALTREF a long way ahead (hint 30, then hint 47), then a
strictly nesting pyramid -- every following coded frame splits the interval it
lands in, so its level is that interval's depth. Ours is the shipped
`8:-32:12:-8:-48` (key q102, top ARF q118 at distance 8, mid ARF q142 at
distance 4, leaves q162), grouped by its own qindex.

libaom `cpu-used 6` crf 41 (48 coded frames, 44802 payload bytes):

| level | frames | bytes | share | B/frame | qindex | PSNR-Y | skip% | intra% | comp% | mean ref dist |
|---|---|---|---|---|---|---|---|---|---|---|
| key | 1 | 21919 | 48.9% | 21919 | 58 | 46.69 | 0.0% | 100.0% | 0.0% | — |
| L1 (ALTREF hint 30) | 1 | 10076 | 22.5% | 10076 | 79 | 47.47 | 62.8% | 3.3% | 0.0% | 30.0 |
| L2 (incl. ALTREF hint 47) | 2 | 4551 | 10.2% | 2275 | 130 | 45.40 | 79.7% | 0.7% | 15.0% | 22.4 |
| L3 | 3 | 2140 | 4.8% | 713 | 146 | 45.80 | 92.2% | 0.1% | 29.9% | 11.1 |
| L4 | 6 | 2297 | 5.1% | 382 | 151 | 45.55 | 95.4% | 0.0% | 35.8% | 8.4 |
| L5 | 10 | 2003 | 4.5% | 200 | 158 | 45.88 | 97.9% | 0.0% | 45.8% | 6.8 |
| shown leaves | 25 | 1816 | 4.1% | 72 | 164 | 45.63 | 99.9% | 0.0% | 65.7% | 3.8 |

ours q150 (48 coded frames, 87129 payload bytes):

| level | frames | bytes | share | B/frame | qindex | PSNR-Y | skip% | intra% | comp% | mean ref dist |
|---|---|---|---|---|---|---|---|---|---|---|
| key | 1 | 19321 | 22.2% | 19321 | 102 | 47.67 | 0.0% | 100.0% | 0.0% | — |
| top ARF (dist 8) | 6 | 48164 | 55.3% | 8027 | 118 | 46.04 | 48.6% | 5.7% | 32.4% | 16.7 |
| mid ARF (dist 4) | 6 | 9652 | 11.1% | 1608 | 142 | 45.86 | 84.7% | 0.6% | 55.4% | 8.6 |
| leaves | 35 | 9992 | 11.5% | 285 | 162 | 45.51 | 96.7% | 0.1% | 58.5% | 1.9 |

Block sizes, family bits (coeff/mode/mv/partition/literal, kB) and the tools
each level actually codes:

| level | block sizes | fam bits kB | tools (symbols) |
|---|---|---|---|
| aom key | 16x16 41%  32x32 24%  64x64 20%  8x8 4%  128x128 3% | 14.8/0.6/0.0/0.2/5.6 | palette_y 978  palette_uv 816  filter_intra 749  cfl 127  delta_q 120 |
| aom L1 | 32x32 46%  16x16 27%  64x64 14%  128x128 4% | 6.0/0.6/0.7/0.2/2.1 | motion_mode 956  palette_y 104  filter_intra 103  delta_q 77 |
| aom L2 | 32x32 40%  16x16 22%  64x64 19%  128x128 8% | 1.7/0.7/1.0/0.2/0.7 | motion_mode 1382  delta_q 123  obmc 13 |
| aom L3 | 32x32 34%  64x64 23%  16x16 19%  128x128 18% | 0.3/0.5/0.9/0.2/0.2 | motion_mode 1064  delta_q 127  obmc 62 |
| aom L4 | 128x128 32%  64x64 27%  32x32 25%  16x16 10% | 0.3/0.7/1.0/0.2/0.1 | motion_mode 1268  delta_q 174  obmc 88 |
| aom L5 | 128x128 52%  64x64 23%  32x32 17%  16x16 4% | 0.1/0.7/0.9/0.2/0.0 | motion_mode 1085  obmc 142  delta_q 136 |
| aom leaves | **128x128 88%**  64x64 8%  32x32 3% | 0.1/0.7/0.8/0.1/0.0 | obmc 976  motion_mode 199 |
| ours key | 32x32 68%  16x16 32% | 12.6/1.8/0.0/0.1/4.1 | filter_intra 1514  cfl 177 |
| ours top ARF | 32x32 33%  16x16 32%  64x64 19%  8x8 16% | 27.3/6.1/3.3/1.3/8.7 | motion_mode 4066  filter_intra 1716  obmc 569 |
| ours mid ARF | 64x64 49%  32x32 26%  16x16 21%  8x8 4% | 3.6/2.5/1.7/0.5/1.0 | motion_mode 1518  obmc 376  filter_intra 195 |
| ours leaves | **64x64 90%**  32x32 6%  16x16 3% | 1.6/5.2/2.0/0.5/0.4 | motion_mode 6604  obmc 876  filter_intra 92 |

Whole-stream shares (`syntax_census` ALL row):

| | ours | libaom |
|---|---|---|
| skip area | 87.1% | **94.8%** |
| compound area | 53.6% | 50.7% |
| refs by area | LAST 96.3%, GOLDEN 0.6%, ALTREF 0.2% | LAST 69.0%, GOLDEN 14.4%, BWDREF 9.7%, ALTREF 4.7% |
| single-ref modes | NEW 28.3% GLOBAL 1.4% NEAREST 52.1% NEAR 18.1% | NEW 56.8% GLOBAL 0.1% NEAREST 35.0% NEAR 8.1% |
| blocks | 64x64 58% 32x32 21% 16x16 16% 8x8 5%, **no 128x128** | 128x128 41% 32x32 22% 64x64 18% 16x16 13% |
| tx_type sets used | set3 74% set6 26% (DCT/ADST only) | set3 27% set6 26% set13 32% set17 9% |
| bits: coeff/mode/mv/partition/literal | 52.9 / 18.3 / 8.3 / 2.9 / 16.8 % | 53.0 / 10.3 / 12.4 / 2.7 / 20.1 % |
| tools never coded by us | — | none of wedge / interintra / intrabc / global motion fire in EITHER stream on this clip; libaom codes palette on its key (978 symbols) and per-SB `delta_q` in EVERY frame, we code neither |

## 2. Film A as control (1920x768 gate crop, 00:35:00, same 48 pictures)

| stream | bytes | PSNR-Y |
|---|---|---|
| ours q150 | 214929 | 43.511 |
| **libaom crf 39** | **154739** | **43.484** |
| libaom crf 40 | 147174 | 43.332 |
| libaom crf 45 | 109468 | 42.366 |

Matched to 0.027 dB: **+38.9% bytes**, which is the long-GOP gate's own
+39.2% on this row, so the probe and the gate agree.

| level | ours B (share) | libaom B (share) |
|---|---|---|
| key | 39722 (18.6%) @ 45.12 dB, q102 | 52613 (34.3%) @ 44.52 dB, q55 |
| anchors | 6 top ARFs 81282 (38.0%) @ q118 | L1+L2 31050 (20.2%) @ q79/q122 |
| mid | 6 mid ARFs 27450 (12.8%) @ q142 | L3+L4 27322 (17.9%) @ q132/q143 |
| leaves | 35 leaves 65169 (30.5%) @ q162 | L5 + 24 shown 42346 (27.6%) @ q149/q156 |

The same shape as film B, twice: **our key frame is CHEAPER and BETTER than
libaom's on both films** (film A -24% bytes, +0.60 dB; film B -12% bytes,
+0.98 dB). The whole gap is inter -- film A ours 173901 B vs libaom 100718 B
(+73%), film B ours 67809 B vs libaom 22883 B (+196%).

## 3. Ranked mechanisms, bytes at stake at the matched point

| # | mechanism | class | film B | film A | evidence |
|---|---|---|---|---|---|
| 1 | anchor COUNT: we code 7 forward-only anchors per 48 pictures (key + 6 distance-8 ARFs), libaom codes 3 (key + one ALTREF per ~16-picture GF group) | (a) structure | +36.6 kB of the 42.4 kB gap | +50.2 kB of 60.2 kB | level tables above; our anchors are NOT individually inefficient -- per anchor frame we spend 37.3 kbit of coefficients against libaom's 49.2 kbit, and per non-skip mi 0.59 bits against its 1.08 |
| 2 | the 128x128 superblock: our sequence header codes `use_128x128_superblock = 0`, so a 128 root is impossible; libaom codes 41% of its blocks (88% of its shown-leaf blocks) at 128x128 | (b) tool we lack | ~5-8 kB (our mode+partition bits 16.1 kB vs libaom's 5.8 kB; our leaf MODE bits 5.2 kB exceed our leaf COEFF bits 1.6 kB) | ~10 kB (mode+partition 27.8 kB vs 14.2 kB) | block-size rows; the decoder already reads 128 roots (`av1-128-block-plane-order`) |
| 3 | per-superblock `delta_q` from tpl (libaom's `deltaq-mode`, on in EVERY libaom frame: 77-757 symbols per level); we never code a `delta_q` symbol | (a) allocation | not directly separable | not directly separable | `tools` rows; `encoder.rs` has no delta_q path at all |
| 4 | extended transform sets: our streams code set3/set6 symbols only (DCT/ADST), libaom codes set13/set17 on 41% of its `tx_type` symbols | (b)+(c) | txtype bits 0.3% vs 1.4%, but it moves the 53% coefficient bucket | same | `tx_type symbols` row |
| 5 | residual efficiency at matched q (RDOQ) | (c) | our anchors already spend FEWER coefficient bits per frame and per non-skip mi than libaom's; the excess is non-skip AREA (51.4% at distance 8 vs libaom's 37.2% at distance 30), i.e. prediction, not quantisation | same | area/skip rows |
| 6 | wedge / interintra / intrabc / global motion | (b) | ZERO symbols in EITHER stream on both films | same | `tools` rows -- these are not the gap on film content; OBMC fires in both |
