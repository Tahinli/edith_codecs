# Long-GOP census v2 — film B, 48 frames, q=150 (lane-probe)

The v1 census (and every "probe-relative" number quoted from it) measured the
WRONG PIXELS. `examples/enc_probe` was handed its window by hand as
`crop=1920:1024:960:568`, i.e. the centre of a 3840x**2160** frame — film B is
coded **3840x1608**, so the gate's own window is `crop=1920:1024:960:292`. Same
q, same source file, different picture: 60466 B (v1) vs **90674 B** (v2).

Arming, now derived by one shared function (`ec_av1::probe::gate_crop`, called
by both the probe and `encode::tests::native_bd_arm`):

    EC_ENC_SS=00:40:00 EC_ENC_OUT=<dir> EC_AV1_BITCENSUS=1 \
        enc_probe <film B> gate 0 48 150      # -> 90674 bytes
    EC_AV1_BITCENSUS=1 syntax_census <dir>/ours-q150.obu <dir>/source.yuv

90674 B is **byte-exact with `encode::tests::bd_rate_film_long_gop`'s own
q=150 ladder point** (gate row: `46.76 dB/90674 B`). Every number below is
therefore the gate's stream.

Pyramid: default `mini_gop 8, arf -32, leaf +12, mid -8, key -48` (requested =
effective; the content gate leaves this clip on the pyramid). 48 pictures =
1 key + 6 ARF + 6 mid + 35 leaf.

## Per level (payload bytes as the decoder censuses them; 89514 B of the
## 90674 B stream, the rest is sequence/frame OBU overhead)

| level | frames | qindex | bytes | share | bytes/frame | PSNR-Y (display) |
|---|---|---|---|---|---|---|
| key   | 1  | 102 | 19321 | 21.6% | 19321 | 47.67 dB |
| ARF   | 6  | 118 | 48126 | 53.8% |  8021 | 46.01 dB |
| mid   | 6  | 142 | 10331 | 11.5% |  1722 | 45.79 dB |
| leaf  | 35 | 162 | 11736 | 13.1% |   335 | 45.27 dB |
| ALL   | 48 |  -  | 89514 |  100% |  1865 | 45.47 dB |

Where the bits go (census bits, per level):

| level | coeff | literal | mode | mv | partition | tx size/type |
|---|---|---|---|---|---|---|
| key | 66.6% | 22.0% | 9.4% | – | 0.8% | 1.3% |
| ALL | 53.6% | 17.1% | 17.6% | 8.0% | 2.9% | 0.7% |

Structure: ALL = intra 3.0% / single-ref 93.0% / compound 4.0% area, skip
85.7%; refs LAST 95.9%, GOLDEN 0.8%, ALTREF 0.3%; blocks 64x64 54.3%, 32x32
26.3%, 16x16 14.7%, 8x8 4.8%. The ARF level carries the group (53.8% of the
bytes on 12.5% of the frames, skip 66.4%); the leaf level is 94.8% skip area
and 335 B a picture.

## For the allocation lanes

- Quote THIS table, not v1: the level shares moved (ARF 49.3% -> 53.8%, key
  25.6% -> 21.6%) because the v1 window was a different, flatter part of the
  frame.
- Any probe run on a real film must pass `gate` as the width argument and set
  `EC_ENC_SS` to the gate's pinned seek (film A `00:35:00`, film B `00:40:00`);
  a hand-written `crop=` is how v1 went wrong.
- Regression cover: `encode::tests::the_probe_arms_the_gates_source`.
