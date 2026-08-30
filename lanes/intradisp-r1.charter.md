# lane-intradisp r1 charter — decode rect (HORZ/VERT) partitions on INTRA frames

Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-intradisp, branch lane-intradisp @ 00de8d3.
Build/test ONLY: `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-intradisp CARGO_BUILD_JOBS=4 nice -n 19 cargo test -p ec-av1 --release --lib <name> -- --nocapture` (full lib ~70s; run it whole).
Set EC_AV1_REQUIRE_AOMENC=1 on gate runs. Never push, never merge, never touch sibling worktrees (lane-gm is live).
libaom oracle: ~/.cache/aom-oracle/src (v3.13.3); aomenc/aomdec/inspect in ~/.cache/aom-oracle/build/.
BUDGET: commit after every green milestone; report by the time you are 3/4 through.

## Why now
The free-partition and AB gates refuse ~17/40 attempts on KEY frames with
`unsupported: AV1 tile (a partition type this encoder never writes (value=1|2|...))`
raised at decode.rs:3874 (inside the 32x32 intra dispatch) and decode.rs:3882
(the 64x64 level). The INTER side already decodes HORZ/VERT/HORZ_A/VERT_A/VERT_B.
The blocker was the square-only intra predictor -- that is now FIXED and merged:
`intra::predict` and `Edges` take `(bw, bh)` and are C-verified over 568 rect
cases (see lanes/intrarect_dump.c and lanes/intrarect-r1.report.md on main).

## Scope: PARTITION_HORZ (1) and PARTITION_VERT (2) on intra frames ONLY
Leave AB (4/6/7) and 1to4 (8/9) refusing this round -- land the smaller,
provable step first.

1. `decode_block` (decode.rs:1962) currently takes one `side`. Widen it to a
   rect block the same way the inter side did: the strip is a real
   32x16/16x32 block, so it needs
   - the true (bw, bh) into `intra::predict` (already rect-capable);
   - the right TRANSFORM size for the strip: check libaom's
     `max_txsize_rect_lookup[bsize]` in ~/.cache/aom-oracle/src/av1/common/common_data.h
     for BLOCK_32X16/BLOCK_16X32 (TX_32X16/TX_16X32) -- our transform code
     already has rect TX support on the inter path (grep TX32X16/TX_16X32 or
     the tx-size enum in transform.rs); reuse it, do not invent a new path;
   - the correct CDF ROWS. Every table indexed by bsize must use the strip's
     own row, not the 32x32 row (class cdf-row-held-constant, which cost the
     inter lane a whole round): check `intra_frame_y_mode`, `uv_mode`,
     `txfm_partition`/`tx_size`, `skip`, and the txb sets. `size_group_lookup`
     for BLOCK_32X16/BLOCK_16X32 is what indexes the y-mode CDF -- read it in
     common_data.h, do not guess.
2. Wire the two arms into the intra dispatch (the `match part32` at
   decode.rs ~3624, whose `_ =>` at :3873 is the refusal) plus the 64x64-level
   `_ =>` at :3882 if it is reachable for HORZ/VERT.
3. Neighbour/context recording must stamp the strip's TRUE footprint
   (above/left mode arrays, skip grid, tx grids) -- the inter side learned this
   the hard way; mirror what the inter HORZ/VERT arms do.

## Gate ladder (in order, commit at each green step)
(a) full lib suite -- every existing stream must stay bit-identical;
(b) 14-pin default list: `cargo test -p ec-av1 --release --lib pinned_warp_stream_decodes_pixel_exact -- --ignored`;
(c) `a_real_aomenc_stream_with_free_partitions_decodes_pixel_exact` -- report
    the intra-refusal count before and after (that is this lane's score);
(d) the AB gate and the intra gates by name (filter_intra, tx_select,
    directional_chroma, intra_with_deblocking).
Any pixel mismatch: pin it with EC_AV1_GATE_DUMP=/tmp/claude-1000/intradisp-flake-N.obu,
then localize -- our EC_AV1_PREFILT_DUMP vs the oracle's (same env var on
~/.cache/aom-oracle/build/aomdec) to find the first bad frame, then
EC_TRACE=1 on aomdec vs EC_AV1_TRACE on ours to compare msac RANGE per
partition symbol. Do NOT guess-fix; a pinned, localized mismatch reported
honestly is a good outcome.

## Done criteria
Intra HORZ/VERT decode pixel-exact with the free-partition gate's intra
refusals measurably reduced; suite + pins green; AB still refusing by name;
report lanes/intradisp-r1.report.md with VERDICT first line and the
before/after refusal counts.
