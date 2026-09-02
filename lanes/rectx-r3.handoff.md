# lane-rectx r3 handoff — one defect fixed, one precisely located

TOTALS `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib` (CARGO_TARGET_DIR=~/.cache/cargo-target-rectx):
**269 passed, 1 failed, 24 ignored** (1042s). The single failure is this lane's own gate,
`stream::tests::a_real_aomenc_stream_with_a_coded_rect_strip_below_16x16_decodes_pixel_exact`;
it now stops on an unrelated third blocker ("a 32x32 partition type this decoder does not code
(value=7)" = PARTITION_VERT_B at 32x32) with the recipe as written. No other test regressed
(268 -> 269 passed is this round's new pin).

## Do next, in this order

1. **Per-mi y-mode neighbour grid.** `Neighbours::above_mode` (decode.rs:1806) / `left_mode`
   are per-16x16 cell; libaom reads `above_mi`/`left_mi` off the real mi grid. Reproduce in
   one command:
   - stream: `ffmpeg -v error -f lavfi -i rgbtestsrc=size=64x64 -pix_fmt yuv420p -t 1 -vframes 1
     -f yuv4mpegpipe g.y4m` then `aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=50
     --cpu-used=4 --threads=1 --row-mt=0 --sb-size=64 --kf-max-dist=0 --enable-rect-partitions=1
     --enable-ab-partitions=0 --enable-1to4-partitions=0 --enable-tx-size-search=0
     --enable-cdef=0 --enable-restoration=0 --reduced-tx-type-set=1 --min-partition-size=8
     --max-partition-size=32 --obu -o t.obu g.y4m`
   - ours: `EC_TRACE_COEFF=1 EC_AV1_RECTX_TRACE=1 EC_TRACE_MODE_STEP=1
     target/debug/examples/decode_probe t.obu`
   - oracle: `EC_TRACE_COEFF=1 EC_TRACE_MODE=1 ~/.cache/aom-oracle/build/aomdec --rawvideo
     -o /dev/null t.obu`
   - THE ELEMENT: leaf `mi=(4,12)`, bw=16 bh=8, outer_at=(1,3). Both enter at range **59792**.
     Oracle `EC_IMODE_VAL mode=2 uv_mode=2 skip=0 -> rng=49036`; ours `mode=0 uv_mode=13 ->
     rng=47236`. First 15 transform blocks of the frame are bit-exact before it.
   - Fix shape: a per-mi mode grid written wherever `fill_skip_grid`/`fill_skip_grid_rect`
     (13 sites) is written, read by `read_intra_mode*`'s `above_ctx`/`left_ctx`.
2. **Then re-pick the gate recipe.** `stream::tests::sweep_rectx_recipes` (`#[ignore]`,
   `--nocapture`) sweeps 9 lavfi sources x 7 cq x reduced-tx-type-set 0/1 and prints
   `fired=`/refusal/`mismatched=` per cell plus an 8x8 mismatch map. After the filter_intra fix
   exactly one cell decodes to completion (rgbtestsrc cq=50 rtx=1, fired=4) and it mismatches
   on defect (1). Every other cell refuses; the dominant refusal is
   "a HORZ_A/HORZ_B/VERT_A partition below 16x16" — this aomenc build writes AB partitions
   below 16x16 even with `--enable-ab-partitions=0` (126/126 cells swept; same quirk the ledger
   records at SB level). Wiring HORZ_A/HORZ_B/VERT_A below 16x16 may be the cheaper route to a
   green gate than recipe hunting.
3. Only then lift "a coded (non-skip) HORZ/VERT rect strip below 16x16" and "a non-skip
   rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding" in
   `refusal_inventory.rs` + `gate_coverage.rs`. NOT lifted this round.

## Do NOT re-do

- The r2 coefficient path (scans, `EOB_PT_128_LUMA`/`EOB_PT_32_CHROMA`, txb sets, nz-map/br
  ctx, DC-sign ctx) is range-exact against aomdec for a real non-skip TX_16X8 leaf + both
  TX_8X4 chroma halves. Don't re-derive it.
- `base_ctx_rect`'s 11/16 rect nz-map offsets are CORRECT as written (display-orientation
  convention: our (row,col) are display row/col, libaom's flat `av1_nz_map_ctx_offset` tables
  are column-major so their INDEX packing, not their rule, is what differs). Swapping the two
  constants was tried this round: the 126-cell sweep went from 2 full decodes to 0 (every cell
  refused). Dead end.
- `SCAN_8X4`/`SCAN_4X8` were re-checked verbatim against
  `~/.cache/aom-oracle/src/av1/common/scan.c` this round. They are right.
