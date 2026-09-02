# lane-sqdrift r1 -- the square-only silent pixel drift, localised to ONE block

## Verdict: RED (defect localised and bounded, not yet fixed). No refusal lifted.

## The stream (pinned, reproducible)

`/home/tahinli/.cache/sqdrift/gen.sh` (kept; recipe = lane-intra14's gate recipe with
`--enable-rect-partitions=0`): 192x128 8-bit `mandelbrot start_scale=4.76 end_scale=0.004
end_pts=8 rate=25`, hue=s=0, `-t 0.32`, real `~/.cache/aom-oracle/build/aomenc`
`--cpu-used=1 --cq-level=55 --min-partition-size=16 --max-partition-size=64
--enable-tx-size-search=0 --kf-min/max-dist=1000 --threads=1 --row-mt=0`, order-hint /
warped / obmc / masked / interintra / dist-wtd / diff-wtd / onesided / ab / 1to4 /
filter-intra / smooth-intra / paeth-intra / directional-intra / angle-delta / cdef /
restoration / palette / intrabc / cfl / ref-frame-mvs ALL `=0`.
sha256 `c6af4fb4ebdc3d74dcfa0c945c0ef2d5e1e3a0902891d9e0a97a5608776b5d55`, hashed twice
from two independent encodes -- reproducible.

## What is actually wrong (measured, not reasoned)

1. `aomdec EC_TRACE=1` partition histogram over the whole stream: only
   `value=0 (NONE)` and `value=3 (SPLIT)` at bsize 12/9/6. **No rect, no AB, no 1:4
   partition exists in this stream at all** -- so main's refusal
   "an intra-coded 1:4 (or other non-2:1) rect strip on the inter block path", which this
   stream stops at on `main@85887c7`, is a PHANTOM read out of an already-diverged
   bitstream (class `refusal-from-own-desync`).
2. Pre-loop-filter dumps, our `EC_AV1_PREFILT_DUMP` vs aomdec's, per decode-order frame:
   f0..f3 **byte-exact (luma AND chroma)**; f4 luma 3540 samples wrong, max |d| = 6,
   **chroma exact**. Post-filter reference dumps (`EC_AV1_FINAL_DUMP`) f0..f3 exact too,
   so no reference frame is poisoned going into f4.
3. The f4 damage is EXACTLY one superblock: bbox x 128..191, y 64..127 -- the last SB,
   mi (16,32), which frame 4 codes as `PARTITION_NONE` at 64x64. It is the ONLY
   64x64 PARTITION_NONE block in any inter frame of this stream.
4. Range ladder at that block's entry: our pre-read msac range for its `partition_w64`
   symbol is **33730**, identical to aomdec's `EC_PART mi_row=16 mi_col=32 bsize=12
   ctx=15 tell=25598 rng=33730`, and the ctx matches too (aomdec prints `bsl*4+ctx`
   = 12+3, ours `ctx=3`). **We enter that block bit-exactly in sync.**
5. The divergence is INSIDE that block. aomdec prints neither `EC_MODE` (=
   `read_inter_block_mode_info`) nor `EC_IMODE`, i.e. libaom takes
   `read_intra_block_mode_info` -- an INTRA block inside an inter frame. Our decoder
   takes the same arm and reads `y_mode[size_group=3] = 0 (DC_PRED)`
   (new `EC_IIS=1` rung: `TRACE iis px=128 py=64 side=64 w=64 h=64 sg=3 mode=0 skip=false`).
   Our reconstruction there is a FLAT 93, which is exactly
   `(sum(above 64) + sum(left 64) + 64) >> 7` -- textbook DC_PRED. aomdec's
   reconstruction is not flat.
6. The luma residual is NOT the defect. The block's 5 non-zero levels were pushed
   through libaom itself (`av1_inv_txfm2d_add_64x64_c`, real `libaom.a`, dequant
   replicated from `decodetxb.c:361-374` with `av1_dc/ac_quant_QTX(80,0,8)` = 74/87 and
   `av1_get_tx_scale` shift 2): **0 mismatches** against our own `residual` for those
   levels (and 2171 with the axes swapped, so the layout is right too). Our TX_64X64
   dequant + inverse transform is exact.
7. Everything after that block drifts: frame 5's SB0 `partition_w64` still reads 3, SB1
   reads 8 where aomdec reads 3 -- a CDF-state divergence carried across the frame
   boundary (frame 5 restarts its own msac, so only the adapted CDFs can carry the
   damage). f5..f7 then drift to ~24k samples / max |d| ~220 as they predict from f4.

So: **the first divergent symbol is in the mode-info or coefficient read of a 64x64
PARTITION_NONE intra-in-inter block, after `partition_w64` and at/after `y_mode`.**
The candidate set is now tiny: `skip`, `read_delta_q_params` (this is the ONE block in
the stream where `bsize == sb_size`, the exact case libaom's
`read_delta_q_params` special-cases), `is_inter`, `y_mode[3]`, `uv_mode_cfl[0]`,
`tx_depth` (`--enable-tx-size-search=0`, so a TX_64X64 must code no depth symbol), or the
64x64 coefficient read itself. `base_q_idx` in our trace takes three values (80/110/168)
across the stream, i.e. **delta_q IS present in this stream** -- that makes the
`bsize == sb_size` delta-q exception a first-class suspect.

## What changed (all env-gated instrumentation, no decode behaviour)

* `crates/ec-av1/src/decode.rs` (`decode_inter_frame_tile_with_cdfs`'s `partition_w64`
  read) -- the `EC_AV1_TRACE` line now carries `pre_rng=`, the msac range BEFORE the
  symbol, which is what makes it directly comparable to aomdec's `EC_PART ... rng=`
  (class `compare-range-not-tell`). This is the measurement that proved fact 4.
* `crates/ec-av1/src/decode.rs` (square intra-in-inter `y_mode` read) -- new `EC_IIS=1`
  rung printing `px/py/side/w/h/size_group/mode/skip` for every intra block in an inter
  frame. That is fact 5.

## Gate

None added -- there is no fix to gate yet. Adding a gate now would pin the defect, not a
capability; the reproducing recipe is `scripts`-free and lives in this report plus
`/home/tahinli/.cache/sqdrift/gen.sh`.

## EVIDENCE

EVIDENCE: /home/tahinli/.cache/sqdrift/{op,aom}.f0..f4 | our EC_AV1_PREFILT_DUMP vs aomdec EC_AV1_PREFILT_DUMP on the pinned stream, every decode-order frame compared Y/U/V | f0-f3 0 bad samples; f4 luma 3540 bad, max |d| 6, chroma 0 bad, bbox exactly x128-191 y64-127
EVIDENCE: /home/tahinli/.cache/sqdrift/{of,af}.f0..f4 | EC_AV1_FINAL_DUMP (post-filter reference buffers) both decoders | f0-f3 0 bad; f4 first bad (145,58), 3848 bad, max 6 -- deblocking only spreads the f4 damage, no reference is poisoned earlier
EVIDENCE: /home/tahinli/.cache/sqdrift/aom.trace | aomdec EC_TRACE=1 partition histogram, whole stream | bsize12 {0:2, 3:46}, bsize9 {0:58, 3:126}, bsize6 {0:504} -- zero rect/AB/1:4 partitions, so main's 1:4-strip refusal on this stream is a phantom
EVIDENCE: our EC_AV1_TRACE pre_rng vs /home/tahinli/.cache/sqdrift/aom.both | range ladder at frame 4 SB (16,32) | ours pre_rng=33730 ctx=3, aomdec rng=33730 ctx=15(=12+3) -- in sync at block entry, so the defect is inside the block
EVIDENCE: /home/tahinli/.cache/sqdrift/{h.c,aomres1.txt,ourres.txt} | the block's 5 non-zero levels through real libaom av1_inv_txfm2d_add_64x64_c vs our residual | 0/4096 mismatches (swapped-axis control: 2171) -- TX_64X64 dequant+inverse is exact
EVIDENCE: EC_IIS=1 rung on the pinned stream | our intra-in-inter mode read at the failing block | `TRACE iis px=128 py=64 side=64 w=64 h=64 sg=3 mode=0 skip=false`, reconstruction flat 93 = DC_PRED of the true neighbours; aomdec's is not flat (SMOOTH/SMOOTH_V/SMOOTH_H/PAETH all rejected numerically too, so aomdec's coefficients differ as well -- the divergence is a SYMBOL, not a predictor)

## Residue

* fix-now (next round, r2): bisect the ~6 remaining symbols of that one block. The
  cheapest decisive instrument is a `pre_rng` print on every symbol of
  `read_inter_frame_mode_info`'s square intra arm plus an aomdec-side `rng` print at the
  same points in `read_intra_block_mode_info` / `read_delta_q_params` (the oracle has no
  print there today -- add one to `scripts/instrument-aom-oracle.sh`, do NOT rebuild the
  shared aomdec in place). Start with `read_delta_q_params`' `bsize == sb_size` exception.
* deferred: 10-bit arm -- unblocked by the fix; there is nothing to gate yet.
* accepted: the two `EC_*` trace rungs ship as permanent debug instrumentation.
