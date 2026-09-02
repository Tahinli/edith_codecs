# lane-sub8 report (round 4)

## Premise correction (this is the round's main result)
Round 2/3 assumed aomenc was really choosing `PARTITION_HORZ`/`VERT` below 8x8
and AB partitions in this gate's recipe. **It is not.** Instrumented aomdec on
the gate's own stream (seed=117 cq=8, `--enable-rect-partitions=0`) reads only
`value=0` (NONE) and `value=3` (SPLIT) at every level:

```
EC_TRACE=1 ~/.cache/aom-oracle/build/aomdec --rawvideo -o /dev/null s117.obu \
  | grep EC_PART_VAL | awk '{print $4,$5}' | sort | uniq -c
     42 bsize=3 value=0    22 bsize=3 value=3    16 bsize=6 value=3
      4 bsize=9 value=3     1 bsize=12 value=3
```

So all 40 of the gate's "refusals" (`HORZ/VERT below 8x8`, `HORZ_A/B/VERT_A
below 16x16`, `a 32x32 partition type value=4/8/9`) were **hallucinated symbols
from our own desync**, not encoder choices. `--enable-rect-partitions=0` IS
honoured. Every refusal-shaped conclusion in rounds 2-3 was measuring the
desync.

## Root causes found and fixed (commit "mi-exact intra mode neighbours ...")
1. **Coarse mode neighbours (`context-read-from-one-cell` class).**
   `above_mode`/`left_mode` are one slot per 16x16; a split cell leaves two
   (8x8) or four (4x4) different modes behind, and a leaf whose above/left
   neighbour sits in such a cell read the wrong `kf_y_mode` CDF row -> desync.
   `Neighbours` now keeps a mi-granular map (`sub8_mode_col`/`sub8_mode_row`,
   `record_mode_mi`), written by `record_rect`, `record_split_luma`,
   `decode_leaf8` and every 4x4 leaf, read back with an exact-position guard
   (`mode_above_mi`/`mode_left_mi`) that falls back to the coarse slot.
2. **`has_tr_4x4`/`has_bl_4x4` never transcribed** (`encode.rs`, the
   `corner-cut` comment that named this exact ceiling). Side-4 blocks clamped
   into the 8x8 table, so a directional 4x4 leaf predicted from above-right
   samples the decoder has not written yet -- all-zero pixels in the leaf's
   bottom-right triangle. Both tables transcribed verbatim from
   `~/.cache/aom-oracle/src/av1/common/reconintra.c:65,254`; `Reach::table`
   maps side 4 to the new row.
3. Cherry-picked lane-tiny `9b1297a` (deblocker chroma tx size from the block
   span) per the charter -- it does not change this lane's residue but keeps
   the branch aligned with main's fix.

## Gate result
`a_real_aomenc_stream_with_a_sub8_split_decodes_pixel_exact` -- **still RED**,
but for a 4-pixel chroma residue instead of a desync:
- symbol ladder vs instrumented aomdec is now **exact** on both sampled
  streams (324/324 elements seed=117, 318/318 seed=100), no refusal, decode
  completes;
- luma is **bit-exact** on seed=100;
- chroma differs on 4 pixels: U/V (row 20, cols 18-19), deltas 1..3.

EVIDENCE: /tmp/.../scratchpad/{ref100.yuv,ours100.yuv} | aomenc seed=100 cq=6 recipe from the gate, decoded by ec-av1 and by ffmpeg | 4096/4096 luma bytes equal, 4/2048 chroma bytes differ (max |delta| 3); before this round the same stream refused at a hallucinated partition symbol
EVIDENCE: /tmp/.../scratchpad/{ref.txt,ours.txt} + ladder.py | EC_TRACE/EC_TRACE_MODE_STEP ladder, ours vs instrumented aomdec, seed=117 | 324 elements, no divergence (was: first divergence at element 62)

## Suite
`cargo test -p ec-av1 --lib` (EC_AV1_REQUIRE_AOMENC=1): **267 passed, 2 failed,
22 ignored**, 1124 s.
- `stream::a_real_aomenc_stream_with_a_sub8_split_decodes_pixel_exact` -- the
  4-pixel chroma residue above.
- `refusal_inventory::the_decode_path_refuses_exactly_the_listed_cases` --
  NOT investigated this round (budget). Expected shape: a refusal the
  inventory expects to fire no longer fires now that the desync is gone
  (that is what the fix was for), but it is a RED test and no refusal string
  was lifted this round.

## Open
- fix-now: the 4-pixel chroma residue (U/V row 20, cols 18-19 of the 64x64
  seed=100 frame; block = chroma 4x4 at (20,16), luma 8x8 group mi(10,8)).
  Tried and REVERTED (no effect on these pixels): OR-ing the subsampling scale
  into the chroma edge's mi (`set_lpf_parameters`' `scale_vert | ...`) in
  `edge_params`. Next step: dump post-reconstruction vs `EC_AV1_PREFILT_DUMP`
  to separate recon from deblock before touching either.
- fix-now: `refusal_inventory` red (see above).
- deferred(gate green): lifting any refusal string -- nothing lifted, none
  proven.
