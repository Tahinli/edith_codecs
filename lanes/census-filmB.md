# Syntax census: film B against rav1e speed 6, at matched PSNR

What this is: a decode-side census of the streams themselves. Both encoders'
output is read by OUR decoder (`examples/syntax_census.rs` +
`crates/ec-av1/src/census.rs`, `EC_AV1_BITCENSUS=1`), so every number below is
the same measurement applied to both sides. Bits are charged per CDF TABLE --
the accountant maps a symbol's table address to its `Cdfs` field name, so a
coding tool joins the census the day its CDF does, with no call-site labels
(class `gate-blind-to-feature`). The census total tracks the tile payload to
within 0.03% on every stream measured here (e.g. film B ours 596,540 bits
censused vs 596,712 payload bits), and the block funnel covers 100.00% of the
frame area on all four streams -- both printed by the tool.

Recipe (the native BD gate's own, `native_gate_clips`): 12 frames, gop 12,
one tile, threads 1, 4:2:0 8-bit.
Film A = 1920x768 crop of a 1080p film at 00:35:00. Film B = 1920x1024 crop of
a 2160p HDR film at 00:40:00, converted to 8-bit. Both clips are extracted
once, losslessly, and BOTH encoders read those exact 12 frames.
Ours: `base_q_idx {150,120,90,60}`. rav1e: `speed=6:quantizer={50,100,150,200}:
tile_cols=1:tile_rows=1:threads=1`.

## Matched points

Mean luma PSNR over the 12 displayed pictures, computed in the census tool
(ffmpeg's PSNR of an OBU stream is unusable -- no timing; ledger dead-end):

| clip | ours | rav1e | matched pair | bytes ours | bytes rav1e | delta |
|---|---|---|---|---|---|---|
| film B | q120 46.774 dB | q100 46.636 dB | +0.138 dB apart | 74,589 | 47,256 | **+57.8%** |
| film A | q120 44.602 dB | q100 44.149 dB | +0.453 dB apart (ours better) | 121,888 | 89,386 | +36.4% |
| film A | q90 46.151 dB | q50 46.366 dB | -0.215 dB apart (rav1e better) | 218,303 | 199,850 | +9.2% |

The film B pair is inside the 0.2 dB window; the film A control has no pair
inside it (the ladders interleave), so both bracketing pairs are given. Film A
at the SAME recipe pair (q120 vs q100) is +36.4% while ours is 0.45 dB ahead
-- i.e. film A's gap is smaller than film B's and is measured with a quality
credit in our favour, which only widens the difference between the two clips.

## Where the bytes are, per frame type

| clip | frame type | ours | rav1e | delta |
|---|---|---|---|---|
| film B | key (1) | 14,572 | 25,146 | **-10,574** (ours smaller) |
| film B | arf (3 ours / 5 rav1e) | 37,433 | 20,143 | +17,290 |
| film B | leaf (8 ours / 6 rav1e) | 22,584 | 1,967 | +20,617 (**11.5x**) |
| film A | key (1) | 31,488 | 49,342 | -17,854 (ours smaller) |
| film A | arf | 52,386 | 28,091 | +24,295 |
| film A | leaf | 38,014 | 11,953 | +26,061 (3.2x) |

The gap is entirely in the INTER frames on both clips, and our key frame is
always the smaller one -- because it is also the worse one: rav1e codes its
key at qindex 68/70 and ours at 120. rav1e's frame-0 luma PSNR on film B is
48.38 dB against our 46.78 dB.

What separates film B from film A is the LEAF ratio: 11.5x against 3.2x.
Film B is a near-static, dark clip; rav1e's leaves cost 328 bytes each and are
99.2% skip area. Ours cost 2,823 bytes each at 70.0% skip.

## The census, ours vs rav1e (whole stream, matched point)

| | film B ours q120 | film B rav1e q100 | film A ours q120 | film A rav1e q100 |
|---|---|---|---|---|
| bytes | 74,589 | 47,256 | 121,888 | 89,386 |
| qindex key/arf/leaf | 120 / 104 / 128 | 68 / 95-112 / 129 | 120 / 104 / 128 | 70 / 97-114 / 131 |
| coded blocks | 27,747 | 10,980 | 24,459 | 14,268 |
| block sizes | 32x32 78.2%, 16x16 18.7%, 8x8 3.1% | **64x64 45.2%**, 16x16 27.2%, 32x32 22.0%, 8x8 5.6% | 32x32 62.4%, 16x16 31.3%, 8x8 6.3% | 16x16 39.7%, 32x32 22.4%, **64x64 22.0%**, 8x8 16.0% |
| luma tx | 32x32 77.6%, 16x16 18.7% | **64x64 45.2%**, 16x16 27.2%, 32x32 22.0% | 32x32 62.4%, 16x16 31.3% | 16x16 39.7%, 32x32 22.4%, 64x64 22.0% |
| area intra / single / compound | 15.2 / 59.7 / 25.1% | 9.0 / 56.4 / 34.6% | 17.0 / 46.3 / 36.6% | 12.1 / 50.9 / 37.0% |
| **skip area** | **52.5%** | **84.8%** | 38.9% | 76.4% |
| refs (area) | LAST 48.8 GOLDEN 28.8 ALTREF 7.3 | LAST 75.1 LAST2 3.0 ALTREF 12.9 | LAST 66.3 GOLDEN 9.0 ALTREF 7.7 | LAST 70.5 LAST2 3.5 ALTREF 13.8 |
| single-ref modes | NEW 11.7 GLOBAL 1.8 NEAREST 79.6 NEAR 7.0 | NEW 22.8 GLOBAL 1.1 NEAREST 58.2 NEAR 17.9 | NEW 16.7 GLOBAL 1.6 NEAREST 72.3 NEAR 9.4 | NEW 20.1 GLOBAL 1.1 NEAREST 57.0 NEAR 21.9 |
| coded mv \|max\| (1/8 pel) | <=8 8.8, <=32 36.0, <=128 44.6, <=512 10.4% | <=8 4.2, <=32 25.8, <=128 55.9, <=512 14.0% | <=8 35.7, <=32 42.7, <=128 20.0% | <=8 27.3, <=32 45.1, <=128 26.9% |
| tx_type symbols | 12,660 (set3/1 78%, set6/1 22%) | 2,108 | 9,600+ | 2,108 |
| coeff bits | 329,971 (55.3%) | 230,617 (61.0%) | 610,650 (62.6%) | 471,565 (66.0%) |
| mode bits | 114,631 (19.2%) | 33,154 (8.8%) | 118,197 (12.1%) | 56,582 (7.9%) |
| mv bits | 28,291 (4.7%) | 22,760 (6.0%) | 30,445 (3.1%) | 22,229 (3.1%) |
| partition bits | 16,507 (2.8%) | 6,074 (1.6%) | 19,331 (2.0%) | 9,923 (1.4%) |
| tx_size bits | 3,240 | 0 (TxMode not Select) | 6,960 | 0 |
| literal (raw) bits | 101,867 (17.1%) | 84,706 (22.4%) | 190,425 (19.5%) | 153,593 (21.5%) |
| biggest tables | base_luma_32 25.9%, base_luma_16 10.0% | **base_luma_64 28.1%**, base_luma_32 13.0% | base_luma_32 25.2%, base_luma_16 14.1% | base_luma_64 20.1%, base_luma_32 13.1% |
| segmentation / LR | off / off-or-luma-Wiener | on / switchable on all 3 planes | off / luma only | on / all 3 planes |

Tool fire counts: our streams code no palette, CfL-only chroma aside, no
filter-intra, no warp, no OBMC, no interintra and no wedge symbols on these
two film clips at all -- every one of those tables is absent from the family
list, which is the census saying the tool never fired (film content, screen
tools off). rav1e's streams likewise show no palette/intrabc; it does code
`comp_group_idx`/`inter_compound_mode` (compound 34.6% of area on film B
against our 25.1%).

## Top 3 differences, ranked by bytes at stake on film B (gap = 27,333 B)

The three overlap -- they are three views of the same inter-frame failure --
so the byte figures must not be added.

1. **Anchor quality and the rate ladder: ~20,600 B (75% of the gap) sits in
   the leaves.** rav1e spends its bits on the key frame (53% of its whole
   stream) and coasts: qindex 68 key -> 129 leaf, a 61-step spread, leaves
   99.2% skip at 328 B each. Ours codes the key at 120, the ARFs at 104 and
   the leaves at 128 -- a 24-step spread with the ARF only 16 below the key --
   so no frame is ever a good enough reference for the next one to skip, and
   every leaf re-codes residual (70% skip, 2,823 B each). Mechanism: our
   pyramid q offsets are nearly flat and our key is far too coarse. This is
   also exactly why film B is twice as far as film A: a near-static clip pays
   for a good anchor once and coasts for 11 frames, which is the regime our
   allocation cannot enter.
2. **No 64x64 (or 128x128) coded blocks or transforms: ~12,600 B (46%).**
   Our largest coded block on both clips is 32x32; rav1e codes 45.2% of film
   B's blocks as 64x64 (73.8% of its leaf blocks) and takes 28.1% of its
   coefficient bits through `base_luma_64`. We therefore code 27,747 blocks
   where rav1e codes 10,980, and our mode+mv+partition+tx_size bits are
   162,669 against rav1e's 61,988: +100,681 bits = +12.6 kB of pure per-block
   overhead. On film A the same term is +79k bits (30% of that gap), so the
   missing large blocks cost film B relatively more -- flat, static content is
   exactly what a 64x64 block is for.
3. **Skip share 52.5% vs 84.8% -> coefficient bits +99,354 bits (+12.4 kB).**
   We code residual over ~32 points more of the frame area than rav1e does
   (leaves: 70.0% vs 99.2%). Partly a consequence of (1) -- a worse reference
   leaves a real residual -- and partly the decision itself: our leaves code
   12,660 tx_type symbols against rav1e's 2,108, i.e. we are choosing coded
   transforms where rav1e chooses none at all.

Two smaller, unpriced differences the census also shows, both of which our
encoder simply does not use: segmentation (rav1e enables it on every frame)
and loop restoration on all three planes (rav1e: switchable everywhere; ours:
off, or luma Wiener on about half the frames).

## Reproducing

```
EC_ENC_VF=null EC_ENC_OUT=<dir> cargo run --release -p ec-av1 --example enc_probe \
  -- <clip.mkv> 1920 1024 12 150,120,90,60
ffmpeg -f rawvideo -pix_fmt yuv420p -s 1920x1024 -r 24 -i clip.yuv -an -threads 1 -g 12 \
  -c:v librav1e -rav1e-params speed=6:quantizer=100:tile_cols=1:tile_rows=1:threads=1 \
  -f obu rav1e-q100.obu
EC_AV1_BITCENSUS=1 cargo run --release -p ec-av1 --example syntax_census -- <stream> clip.yuv
```
