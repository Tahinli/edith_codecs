# Long-GOP census: 48 pictures, gop 48, ours vs rav1e vs libaom

The 48-frame recipe of `encode::tests::bd_rate_film_long_gop` (film rows only,
`-g 48`, one tile, threads 1, 4:2:0 8-bit), censused frame by frame with
`EC_AV1_BITCENSUS=1` + `examples/syntax_census.rs`: every number below is read
back out of the STREAM by our own decoder, so all three encoders are measured
by the same reader. Clips: film A = 1920x768 crop at 00:35:00 of the 1080p
film, film B = 1920x1024 crop at 00:40:00 of the 2160p HDR film, the BD gate's
own crops -- our streams reproduce the gate's byte counts exactly (film B
76,541 B, film A 195,531 B at q150), so this is the gate's own material.

## Matched points

PSNR-Y is the census tool's own, over the 48 DISPLAYED pictures in display
order (hidden frames and `show_existing_frame` make coded order a different
sequence). Reference points were re-encoded at intermediate quantizers until
they landed inside 0.3 dB of ours.

| clip | ours (2-level, q150) | reference | delta PSNR | bytes ours | bytes theirs | ours over |
|---|---|---|---|---|---|---|
| film B | 44.858 dB | rav1e speed 6 q125 | -0.13 dB | 76,541 | 51,680 | **+48.1%** |
| film B | 44.858 dB | libaom cpu-used 6 | n/a | 76,541 | n/a | see the gap below |
| film A | 42.645 dB | rav1e speed 6 q125 | +0.26 dB (ours better) | 195,531 | 157,690 | +24.0% |
| film A | 42.645 dB | libaom cpu-used 6 crf42 | -0.30 dB | 195,531 | 130,795 | +49.5% |

FILM B / LIBAOM IS NOT CENSUSED: our decoder does not read libaom's film B
streams. `crf35` fails outright (`AV1 tile: a Golomb tail longer than this
decoder reads`) and `crf20`/`crf45` decode to 25-28 dB of garbage on two
pictures out of three, while every film A libaom stream decodes correctly
(census PSNR 46.150 dB against ffmpeg's 46.116 on `crf20`). That is a DECODER
gap on 10-bit-sourced 1920x1024 libaom material, found here, not an encoder
finding -- and it is why the film B column below is ours vs rav1e only.

## Bytes per hierarchy level

Levels are derived from each stream itself: `key`, hidden frames split by
their own qindex (`arf top` = the lowest, i.e. the group's own anchor;
`arf mid` = the internal ones), `leaf` = every shown coded frame. Percentages
are of the censused stream.

### film B (ours 75,433 B censused / rav1e 50,326 B)

| level | ours n / q | ours bytes (share, mean) | rav1e n / q | rav1e bytes (share, mean) |
|---|---|---|---|---|
| key | 1 @ 150 | 8,399 (11.1%, 8,399) | 1 @ 100 | 15,900 (31.6%, 15,900) |
| arf top | 6 @ 118 | **53,508 (70.9%, 8,918)** | 11 @ 120 | 26,604 (52.9%, 2,418) |
| arf mid | -- | -- | 12 @ 137 | 4,431 (8.8%, 369) |
| leaf | 41 @ 166 | 13,526 (17.9%, 329) | 24 @ 153 | 3,391 (6.7%, 141) |

### film A (ours 194,342 B / rav1e 156,302 B / libaom 129,387 B)

| level | ours | rav1e | libaom |
|---|---|---|---|
| key | 1 @ 150: 20,079 (10.3%) | 1 @ 101: 34,700 (22.2%) | 1 @ **63**: 47,707 (**36.9%**) |
| arf top | 6 @ 118: 94,076 (48.4%, mean 15,679) | 11 @ 122: 64,282 (41.1%, mean 5,843) | 1 @ 95: 14,513 (11.2%) |
| arf mid | -- | 12 @ 138: 28,365 (18.1%, mean 2,363) | 22 @ 129..168: 49,319 (38.1%, mean 2,242) |
| leaf | 41 @ 166: 80,187 (41.3%, mean 1,955) | 24 @ 154: 28,955 (18.5%, mean 1,206) | 24 @ 163/168: 17,848 (13.8%, mean 743) |

## Where the gap is

**Not in the leaves, and not in the key's byte count -- it is the ALLOCATION
and the number of levels.**

1. **Our key frame is the COARSEST anchor in our own stream.** Our ladder is
   key 150 / arf 118 / leaf 166: the one frame the whole 48-picture GOP
   predicts from is coded 32 q-steps ABOVE the ARFs. rav1e codes key 100 /
   top 120 / mid 137 / leaf 153 and libaom key 63 / top 95 / internal
   129..168 / leaf 163: in both references the key is the BEST frame in the
   stream by 20-100 q-steps and takes 22-37% of the bytes; ours takes 10-11%.
   Film B display-0 PSNR: rav1e 47.1 dB against our 45.0 dB at a stream that
   is 48% bigger overall.
2. **One hidden level against two.** rav1e's mini-GOP at speed 6 is FOUR
   pictures with two hidden levels (top ARF at the group's last picture at
   q120, an internal ARF at its midpoint at q137, two leaves at q153) -- 23
   hidden frames in 48 pictures against our 6. libaom is deeper still: 23
   hidden frames over qindex 95..168. Our 8-picture group carries one hidden
   frame at q118 and then seven leaves that are 4 to 7 pictures away from
   their nearest good reference.
3. **The cost lands on the top ARF, not the leaf.** On film B our 6 ARFs are
   70.9% of the stream at 8,918 B each, where rav1e's 23 hidden frames total
   31,035 B. Our leaves are already cheap (329 B, 17.9% of the stream) --
   the 12-frame census's "leaves are 11.5x rav1e" finding does NOT hold at
   48 pictures with the shipped 8:-32:16 pyramid. Film A is the same shape
   with a heavier leaf term (1,955 B vs 1,206 B).

The structural fix these three point at is a THIRD LEVEL, which is what
lane-pyr5 built (`Pyramid::mid_q_offset`): a second hidden frame at the
midpoint of each mini-GOP, leaves referencing the nearer of the two ARFs.
Censused at the same q150 point, film B:

| stream | bytes | PSNR-Y | key | arf top | arf mid | leaf |
|---|---|---|---|---|---|---|
| ours 8:-32:16 (2-level) | 75,433 | 44.858 | 8,399 | 6 @ 53,508 | -- | 41 @ 13,526 |
| ours 8:-32:16:-8 (3-level) | 82,496 | **45.096** | 8,399 | 6 @ 53,508 | 6 @ 11,472 | 35 @ 9,117 |

The mid ARF costs 11,472 B and takes 4,409 B back off the leaves (41 leaves at
329 B -> 35 at 260 B) for +0.24 dB, which is a BD win: see `lanes/pyr5.sweep.txt`.

## The unclaimed lever

Levels 1 above is NOT addressed by the third level and is the larger of the
two: our key frame gets no negative q offset at all, while both references
spend a fifth to a third of the whole stream on theirs. A `key_q_offset` on
`Pyramid` (or a rate-loop `KEY_WEIGHT` re-fit) is the next lane; it is deferred
here because it is the rate-allocation axis, not the reference structure this
lane was chartered for.

## Reproducing

```
ffmpeg -v error -ss 00:35:00 -i <film A> -frames:v 48 -vf crop=1920:768:0:12 \
  -f rawvideo -pix_fmt yuv420p filmA.yuv        # film B: -ss 00:40:00, crop=1920:1024:960:292
ffmpeg -v error -y -f rawvideo -pix_fmt yuv420p -s 1920x1024 -r 24 -i filmB.yuv \
  -an -threads 1 -g 48 -c:v librav1e \
  -rav1e-params speed=6:quantizer=125:tile_cols=1:tile_rows=1:threads=1 -f obu rav1e.obu
EC_ENC_VF=null EC_ENC_OUT=<dir> EC_AV1_PYRAMID=8:-32:16:off \
  cargo run --release -p ec-av1 --example enc_probe -- filmB.y4m 1920 1024 48 150
EC_AV1_BITCENSUS=1 cargo run --release -p ec-av1 --example syntax_census -- <stream> filmB.yuv
```
