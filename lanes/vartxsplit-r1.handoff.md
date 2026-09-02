# lane-vartxsplit r1 — handoff (root cause FOUND and FIXED; gate work not done)

## Pinned stream (reproducible: two runs, identical md5)
`~/.cache/vartxsplit-tmp/s8.obu`, md5 `842355ef494455ead0352f62f84f2c56`
(generator `~/.cache/vartxsplit-tmp/gen.sh`), 8-bit 384x256 6 frames @25,
source `128+90*sin((X+N*3)/6)*sin(Y/2)+50*sin((X*Y)/37)`, oracle aomenc:
`--codec=av1 --passes=1 --end-usage=q --cq-level=10 --cpu-used=0 --threads=1 --row-mt=0
 --sb-size=64 --bit-depth=8 --input-bit-depth=8 --enable-restoration=0 --enable-palette=0
 --deltaq-mode=0 --enable-filter-intra=0 --enable-cfl-intra=0 --enable-intrabc=0
 --lag-in-frames=0 --kf-max-dist=9999 --tile-columns=0 --enable-tx-size-search=1
 --enable-rect-partitions=1 --enable-ab-partitions=1 --enable-1to4-partitions=1
 --min-partition-size=8 --max-partition-size=64 --obu`
(gaterecipe r1's recipe 1; the 10-bit 16x32 twin was NOT re-measured this round.)

## First diverging element (before the fix)
Decode-order frame 2, luma, block mi(8,16) (8x16, TX_8X16), the FIRST DC-sign symbol
of its plane-0 transform unit:
- entry range 43680 in both decoders;
- aomdec `EC_COEFF_STEP tag=sign c=0 sign=1 dcctx=0 rng=41828`
- ours     `EC_COEFF_STEP tag=sign_rect pos=0 sign=1 dcctx=1 rng=41148`
i.e. same value, wrong CDF row: our DC-sign context was 1 (negative vote) where libaom's
is 0. Everything before it — partitions, var-tx `txfm_split` ladder (1362 lines of frame 1
byte-identical; 92 lines of frame 2), coefficients — matched.

## Root cause (fixed, decode.rs)
`EC_DCDUMP mi=(8,16) wh=(8,16) vote=-2 above=[Some(true)/7,Some(false)/7]
left=[None/0,None/0,Some(true)/7,Some(true)/7]`: the two left cells at mi rows 8-9 were
EMPTY. They belong to the compound 8x8 leaf at mi(8,14) whose transform SPLIT into four
4x4 TUs. `read_inter_luma8` returns an all-zero placeholder `luma_grid` in the split case
because each 4x4 TU already wrote its own plane-0 state via `record_mi_luma`; the compound
leaf arm then called `neighbours.record_mi(leaf_mi, 8, ...)`, which rewrites all three
planes of those cells and erased the four per-TU cul_level/DC-sign records. The
single-ref twin (`decode_leaf8`, decode.rs:25091) already had the `saved_luma_ctx`
save/restore around exactly this call; the compound arm never got it (class:
twin-functions-drift / mvstack-compound-tpl-extension).

Fix: same save/restore around `record_mi` in the compound arm (decode.rs, the
`neighbours.record_mi(leaf_mi, 8, ...)` at the end of the compound 8x8 leaf).

## Result
`dump_yuv s8.obu` vs `ffmpeg -pix_fmt yuv420p`: all 6 frames byte-identical
(was: frames 2-5 differ, 91201 luma samples on frame 2).

## NOT done (next steps, in order)
1. Full suite (`systemd-run --user --unit=vartxsplit-suite-... cargo test -p ec-av1 --lib`).
2. Re-measure the 10-bit 16x32 arm (gaterecipe recipe 2: gate A's flag set,
   `--enable-ab-partitions=0 --enable-dual-filter=0 --enable-obmc=0 --enable-tx64=1`,
   attempt 16/17) — it may be the same defect or a second one; recipe 1's compound leaf
   only exists because THIS recipe leaves `--enable-obmc`/dual-filter on.
3. Turn the gate B `split-tx-8x4` arm (stream.rs ~11036, marked unproven by gaterecipe r1)
   back into a hard assert, and add the 10-bit 16x32 arm to
   `real_aomenc_1to4_streams_decode_pixel_exact_and_rect_vartx_leaves_fire_before_a_named_refusal`.
4. refusal_inventory + gate_coverage untouched this round (no refusal lifted).

## Instrumentation left in the tree (env-gated, cheap)
- `EC_DCDUMP=1`: `around_mi_rect` prints per-cell above/left DC state + vote (plane 0).
- `EC_TRACE_COEFF` rect sign line now carries `dcctx=`.
