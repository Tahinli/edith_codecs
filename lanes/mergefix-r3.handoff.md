# lane-mergefix r3 handoff

Tip: lane-mergefix 4d8a59d. Code is behaviourally identical to r2 (the r3 fix in
62a4cce was reverted in 4d8a59d); read `lanes/mergefix-r3.report.md` first.

## Do not re-hunt
The straddling-band partition-ctx divergence at mi(16,32) is SOLVED and proven:
`decode_inter_block`'s var-tx branch (decode.rs ~20857) publishes `side, side` instead
of the strip's `write_w, write_h`. With the true footprint the ladder matches the
oracle exactly (ctx 0 -> 1 at rng=51641, exit 38495) and the gate goes 3622 -> 3376 px.

## Why it is not in
Any form of that fix (4 variants measured, listed in the report) makes two attempts of
`real_aomenc_1to4_streams_..._rect_vartx_leaves_fire_before_a_named_refusal` refuse with
"a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding",
so that gate's leaf counter hits 0 and it fails. `side` at those call sites is not the
piece's parent bsize (sb128 pieces get side=64 under a 128 parent), so a parent-span
write keyed off `side` stamps the wrong span.

## r4 job
Move the partition-context band write out of `decode_inter_block` into the partition
arms (SB-level and 64-level HORZ/VERT/1:4/AB, inter and intra), one write per
partition over the PARENT span with the subsize's width above / height left, exactly
like libaom `update_partition_context(xd, mi_row, mi_col, subsize, bsize)`; delete the
`side, side` publish for the non-square case at the same time. Re-run BOTH gates in one
invocation:
`cargo test -p ec-av1 --lib -- a_frame_edge_straddling_band_decodes_pixel_exact real_aomenc_1to4_streams_decode_pixel_exact_and_rect_vartx`

## Second, independent residue
Frame 1 of the pinned stream is entropy-exact (EC_MODE ladder for all 66 blocks;
coefficient all_zero/eob ladder 225/225) and still differs by 3376 px, first at row 0
col 64 = mi(0,16): a 32x32 compound SKIP block (mode=19 ref0=1 ref1=7 mv=(0,0)).
That is a compound-prediction reconstruction defect; drive it with EC_AV1_PREFILT_DUMP
on frame 1, not with an entropy ladder.

## Environment
Stream `~/.cache/mergefix-tmp/str61.obu` md5 a14892ed0ba88b6ad2b566e251ea2d33 (recipe
`mkstr.sh`); instrumented oracle `~/.cache/mergefix-tmp/aom-build/aomdec` (EC_VARTX added
in r2). Suite log `$HOME/.cache/mergefix-suite-r3.log`.
