# lane-mergefix r3 report

## Verdict
Root cause of the `a_frame_edge_straddling_band_decodes_pixel_exact` partition-ctx
divergence FOUND and proven against the instrumented oracle; the fix is NOT shipped
because all three variants of it turn a green sibling gate red (class
`fix-trades-sibling-gate`). Branch is code-identical to r2 plus a `lane-mergefix r3`
comment at the site (`crates/ec-av1/src/decode.rs:20852`) naming the defect and the
three measured variants. Straddling gate stays RED at 3622 px, exactly as r2 left it.

## Root cause (proven)
`decode_inter_block`'s var-tx branch (decode.rs ~20857) calls
`record_split_luma_rect_mi(at, side, side, ..)` on the comment's premise "a var-tx
block is always square, so `write_w == write_h == side`". FALSE: a superblock-level
`PARTITION_VERT` inter strip is 32x64 with `side == 64`, so the strip stamps the
PARENT's 64 into `above_side_mi`/`left_side_mi`. `Neighbours::partition_ctx_mi`
(decode.rs ~4985) then reads `above_side_mi[32] * 2 <= 64` as false and picks
partition CDF row 0 where libaom picks row 1. Class `tx-grid-published-block-side`,
7th instance.

EVIDENCE: ladder at mi(16,32) of the pinned stream (md5 a14892ed0ba88b6ad2b566e251ea2d33,
192x68 cq61 tile_cols=1 8-bit, `~/.cache/mergefix-tmp/mkstr.sh`) |
`EC_TRACE_PART=1 <target>/debug/examples/decode_probe str61.obu` vs
`EC_TRACE_MODE=1 aomdec --rawvideo -o /dev/null str61.obu` |
ours `EC_PARTG mi_row=16 mi_col=32 bsize=12 ctx=0 rng=51641` -> with `write_w/write_h`
`ctx=1 rng=51641` then `bsize=9 rng=38495`, matching aomdec `EC_PART ... ctx=13
(= bsl*4+1) rng=51641 -> value=3 (SPLIT) rng=38495`.

EVIDENCE: gate delta | `cargo test -p ec-av1 --lib -- a_frame_edge_straddling_band_decodes_pixel_exact`
with the fix applied | 3622 -> 3376 differing pixels, same first pixel (frame 1, row 0
col 64, ours 56 vs ffmpeg 178).

## Why it is not shipped (the trade, measured 3 ways)
With the fix, `real_aomenc_1to4_streams_decode_pixel_exact_and_rect_vartx_leaves_fire_before_a_named_refusal`
goes green -> RED: two 8-bit attempts (16, 17) that previously decoded pixel-exact now
stop at the named refusal `unsupported: AV1 tile (a non-skip rectangular
(HORZ/VERT/HORZ_B) strip needs rectangular residual coding)`, so the gate's rect var-tx
leaf counter drops to 0 and it fails its own hard assert.

Variants measured, each with both gates in one invocation:
1. `record_split_luma_rect_mi(at, write_w, write_h, ..)` -- straddling 3376, 1to4 "0 leaves, 0 refusals" FAIL.
2. keep the call at `side, side`, republish only `above_side_mi`/`left_side_mi` per piece with `write_w/write_h` -- straddling 3376, 1to4 2 refusals FAIL.
3. same, but over the PARENT span (`rmi & !(n-1)`, `n = side / MI`; libaom's `update_partition_context` takes `bsize`, not the piece) -- straddling 3376, 1to4 2 refusals FAIL.
4. variant 3 restricted to VERT shapes only (`write_w * 2 == side && write_h == side`) -- straddling 3376, 1to4 2 refusals FAIL.

Reading: `side` at those 1:4-gate call sites is not necessarily the piece's PARENT
bsize (sb128 streams pass `side = 64` for pieces whose parent is 128), so a
parent-span write keyed off `side` stamps the wrong span there. The band write belongs
in the partition arms, where the parent bsize and origin are known -- that is the r4 job.

## Swept sites (class sweep, `side`/parent dimension into a per-mi side band)
- decode.rs:20857 `record_split_luma_rect_mi(at, side, side, ..)` in `decode_inter_block` var-tx branch -- THE DEFECT (documented, not changed).
- decode.rs:6791 `record_split_luma_rect_mi(at_mi, bw, bh, ..)` -- already true footprint, OK.
- decode.rs:10798 `record_split_luma(at, side, ..)` in `decode_block` -- square intra block, `side` is its own, OK.
- decode.rs:5383 `record_mi_rect` / :5567 `record_split_luma_rect_mi` band writes -- take w/h from the caller, OK.
- decode.rs:8596, :11235, :11565, :11919, :21364, :21847 direct `above_side_mi`/`left_side_mi` writes -- all use the block's own `bw`/`bh` (or the documented 4/8 constants), OK.
- `record_rect_mi`/`record_inter_rect_mi`/`record_compound_ctx_rect_mi` square wrappers (:4821, :5097, :5497) -- callers are square blocks, OK.
No second instance of the parent-dimension shape found.

## Second residue (unchanged by all of the above)
With the fix in place, frame 1 of the pinned stream is element-exact against the oracle
in BOTH ladders yet still differs by 3376 px: EC_MODE ladder identical for all 66
blocks of frame 1 (first divergence is frame 2's block at mi(0,16)); the
`tag=all_zero`/`tag=eob` range ladder is identical for its first 225 entries (all of
frames 0-1) and only then diverges. The first bad pixel is frame 1 row 0 col 64 =
mi(0,16), a 32x32 compound skip block (`EC_MODE ... is_inter=1 skip=1 side=32`,
`mode=19 ref0=1 ref1=7 mv0=(0,0)`), so frame 1 is a RECONSTRUCTION defect (compound
prediction), not an entropy one.
EVIDENCE: ~/.cache/mergefix-tmp/{ours.txt,aom.txt,oc.txt,ac.txt} | `EC_TRACE_MODE=1` and
`EC_TRACE_COEFF=1` on both decoders over str61.obu | `paste`d EC_MODE rng columns equal
through frame 1 (66 blocks), coefficient ladders equal for 225/225 entries.

## Suite
`systemd-run --user --unit=mergefix-suite-r3-1788352226 ... cargo test -p ec-av1 --lib -j3`
(log `$HOME/.cache/mergefix-suite-r3.log`, run WITH variant 1 applied):
`test result: FAILED. 404 passed; 2 failed; 37 ignored` --
`a_frame_edge_straddling_band_decodes_pixel_exact` (3376 px) and
`real_aomenc_1to4_streams_..._rect_vartx_leaves_fire` (the trade above).
After reverting to the shipped state, the two gates re-run in one invocation give
`1 passed; 1 failed`: 1to4 ok, straddling 3622 px (r2's number, no regression).

## Disposition
- fix-now for r4: move the partition-context band write out of `decode_inter_block` and
  into the partition arms (parent bsize + origin known there), then re-run BOTH gates.
- deferred: frame-1 compound-skip reconstruction defect at mi(0,16) -- unblocked by a
  prediction-side ladder (EC_AV1_PREFILT_DUMP on frame 1), independent of the above.
