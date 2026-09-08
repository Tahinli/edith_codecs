# lane-seg2 — libaom `-aq-mode 1` on film B: the sub-8x8 intra reader had no segment id

Two defects, one reader. Class: **`symbol-consumption-gap`** (a syntax element
the spec codes for every block was never read on one block-shape path), with
the second half in the **`tx-grid-published-block-side`** shape (a per-mi map
stamped with the wrong footprint).

## First divergent symbol

`filmB_aom45.obu` (`crop=1920:1024`, 12 frames, libaom `-cpu-used 6 -crf 45
-aq-mode 1`), frame 0 = KEY frame, first bad luma sample (883, 888).

Cross-decoder `EC_IMODE` range ladder (aomdec `EC_TRACE_MODE_STEP` vs our
`EC_TRACE_MODE_STEP`), blocks 1..1799 identical, block 1800 diverges:

```
aom   EC_IMODE mi_row=226 mi_col=218 bsize=0 rng=35072   (BLOCK_4X4 split leaf)
ours  EC_IMODE mi_row=226 mi_col=218 fn=sub8   rng=35072
aom   ... next block EC_IMODE mi_row=226 mi_col=219 rng=43528
ours  ... next block EC_IMODE mi_row=226 mi_col=219 rng=33544
```

i.e. the desync is INSIDE the frame's first sub-8x8 intra leaf.

At `crf 5` the same ladder puts the first divergence at mi (198, 148), and the
new `EC_SEG` trace (added to both decoders) names the element exactly:

```
aom   EC_SEG mi_row=198 mi_col=148 cdf=1 pred=1 coded=1 id=2 rng=61592
ours  EC_SEG mi_row=198 mi_col=148 cdf=0 pred=0 coded=1 id=1 rng=35672
```

`cdf_index` 0 vs 1 and prediction 0 vs 1: our `av1_get_spatial_seg_pred` read
segment 0 out of the LEFT neighbour mi (198, 147).

## Root cause

`crates/ec-av1/src/decode.rs read_intra_mode_sub8` — the reader every
`BLOCK_4X4` split leaf AND every 8x4/4x8 leaf goes through — never called
spec 5.11.6's `intra_segment_id` at all:

1. **the symbol was never read.** On a stream with `segmentation_enabled` the
   leaf's `segment_id` symbol stayed in the bitstream, so the tile desynced
   from the frame's first sub-8x8 partition on (and `cur_segment_id` kept the
   PREVIOUS block's value, so the leaf also dequantized with a neighbour's
   `SEG_LVL_ALT_Q`). Fixed at `098e5e1c`.
2. **the stamp was 1x1.** Reading it with a hardcoded 1x1 mi footprint is the
   second half: the same reader serves 8x4/4x8 leaves, whose second mi cell
   stayed at segment 0 and was read back by the NEXT block as its spatial
   prediction and CDF row. Fixed at `8e58fcc7` (footprint threaded from the
   leaf's own `bw`/`bh`: 1x1 for a 4x4 leaf, `bw/4 x bh/4` for a rect one).

Both halves are needed: with only (1) the film B libaom crf 5 and 20 points are
still 9.2M/9.7M samples off; the witness below is red under either half alone.

## Same-shape sweep — every site that reads or stamps the segment map

| site (`decode.rs`) | reader | footprint passed | verdict |
| --- | --- | --- | --- |
| `read_intra_mode_rect` (8922/8927) | rect intra | `(bw/4, bh/4)` | ok |
| `decode_leaf8`/square (13031/13039) | square intra | `(side/4, side/4)` | ok |
| `read_intra_mode_sub8` (15747/15755) | 4x4 split + 8x4/4x8 intra leaf | **absent, then 1x1** | **FIXED (both halves)** |
| inter block reader (26625/26644) | inter | `(write_w/4, write_h/4)` | ok |
| `decode_inter_sub8_split4` (30394/30398) | 4x4 inter leaf | `(1, 1)` | ok (BLOCK_4X4 is 1x1 mi) |
| inter 4x8/8x4 leaf (31437/31441) | rect inter leaf | `(bw/MI, bh/MI)` | ok |
| inter 8x8 leaf (32181/32189) | 8x8 inter leaf | `(2, 2)` | ok |

Consumers of the map, all reading through the two single points
`segment_id_at` / `block_q_idx` (which is why only the WRITE side was wrong):
`spatial_seg_pred`, `predicted_segment_id`, `block_q_idx` (`SEG_LVL_ALT_Q`),
the deblocker's per-segment `lf_level`. No second write path exists.

## Witness

`stream::tests::a_segmented_key_frame_with_sub_8x8_intra_leaves_decodes_exactly`
(new, in the existing `stream::` test binary, no fixture committed):
`color=c=gray:s=384x152 , noise=alls=80:all_seed=7:allf=t+u` -> ffmpeg
`libaom-av1 -cpu-used 2 -b:v 0 -crf 5 -aq-mode 1 -g 4`, 4 frames, decoded by
ffmpeg and by us, all three planes, plus `segment_ids_seen() >= 2` so a
one-segment (vacuous) run cannot pass.

- green at HEAD: `4 frames sample-exact, 1963 segment_id symbols, 3 distinct ids`, 2.16 s
- red with the fix reverted: `FAILED. 0 passed; 1 failed` (measured 134464
  samples off pre-fix, 134082 off with only the first half of the fix)

`-cpu-used 2` is load-bearing: at `-cpu-used 6` (the straddle gate's recipe)
libaom answers this source with no sub-8x8 intra partition and the row passes
with the defect in — measured on four probe recipes.

## Ladder (12 frames, all planes, vs ffmpeg; streams from lane-golomb2)

| stream | pre-fix | after `098e5e1c` | HEAD |
| --- | --- | --- | --- |
| film B 1920x1024 libaom crf 5 / 20 / 35 / 45, `-aq-mode 1` | 4x DIFF | 2 EXACT, 2 DIFF | **4/4 EXACT** |
| film B 1920x1024 rav1e q 50 / 100 / 150 / 200 | 4/4 EXACT | 4/4 EXACT | **4/4 EXACT** |
| film A 1920x792 libaom crf 5 / 20 / 35 / 45 | 4/4 EXACT | — | **4/4 EXACT** |
| film A 1920x792 rav1e q 50 / 100 / 150 / 200 | 4/4 EXACT | — | **4/4 EXACT** |

## Instruments added (kept)

- `EC_TRACE_SEG=1` on BOTH decoders: `EC_SEG mi_row= mi_col= cdf= pred= coded= id= rng=`
  (libaom side patched in `~/.cache/aom-oracle/src/av1/decoder/decodemv.c`,
  rebuilt in place).
- `EC_HDR=1` also prints `EC_SEGHDR enabled/update_map/temporal/preskip/last_active/data`.
- `EC_TRACE_MODE_STEP=1` now prints the `skip`/`cdef`/`dq`/`mode` ladder for
  sub-8x8 leaves too, which is what separated "segment id" from "cdef".
