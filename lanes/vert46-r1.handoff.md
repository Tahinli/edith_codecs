# lane-vert46 r1 HANDOFF (tool-call cap)

Tip: `ae16fec` on `lane-vert46` (base 103b66f = lane-sb128c r8). Two files
touched: `crates/ec-av1/src/decode.rs`, `crates/ec-av1/src/stream.rs`.

## Stream

192x256 8-bit `geq`-sinusoid source, `--sb-size=128 --min-partition-size=64
--enable-rect-partitions=1 --enable-tx-size-search=0 --cq-level=55
--cpu-used=0 --limit=5`, NO `--kf-max-dist=1`. Reproduced twice, md5
`ccc3bafd6d1c42b1336a69e68482e54b`, identical to the r8 residue stream.
Recipe script: `$HOME/.cache/vert46-tmp/mk.sh`. mi 64x48 -> the RIGHT 128
superblock column is partial (`mi_col + hbs == mi_cols`), so its 128 root
reads the gathered bit; frame 1 resolves it to PARTITION_VERT = one
BLOCK_64X128 inter block.

## First diverging element (before the fix)

Frame 1 (decode order), block mi(0,32) BLOCK_64X128, plane 1 (U), mu chunk 0,
32x32 chroma unit, the DC SIGN symbol:

    pre-symbol range  39712   (identical both sides)
    aomdec            dcctx=2, post-symbol range 41596
    ours              ctx=1,   post-symbol range 64592

Ours read the left neighbour band as the 128x128 block's mu-chunk (0,0) DC
(negative) where libaom reads its mu-chunk (0,16) DC (positive).

After fixing that, the ladder's next divergence was the 128-root partition
symbol of the NEXT superblock: aomdec `EC_PART mi_row=32 mi_col=32 bsize=15
ctx=19 rng=43152`, ours entered the 64 level at rng=35032 (ctx 16).

## Root cause (two, same shape)

1. `decode.rs` `decode_inter_block`, new `mu_chroma` flag + the per-unit
   re-stamp loop before `record_inter_rect_mi` (~:20146): a block above 64
   codes its chroma as one 32x32 unit per 64x64 mu chunk and each unit stamps
   its own DC sign/level (`record_mi_chroma`, libaom `av1_set_entropy_contexts`
   inside `decode_reconstruct_tx`). The end-of-block record overwrote ALL of
   them with `neighbour_state` of the assembled whole-block grid, i.e. with
   the top-left unit's DC. Fix: collect the per-unit grids, re-stamp after the
   block record, in mu-chunk order.
2. `decode.rs` `decode_inter_block` (~:20179):
   `record_split_luma_rect_mi(at, side, side, ..)` stamped
   `above_side`/`left_side` = 128 for a 64x128 var-tx block ("a var-tx block
   is always square" stopped being true when the 128 root's HORZ/VERT arm
   landed in sb128c r7). Fix: pass the block's own `write_w`/`write_h`.

## cmp result (the only verification run)

`EC_AV1_FINAL_DUMP` decode of the stream under
`systemd-run --user --scope -q -p MemoryMax=6G`, `cmp` per decode-order frame
against the instrumented aomdec's own `EC_AV1_FINAL_DUMP`:

    f0 same  f1 same  f2 same  f3 same  f4 same  f5 same

(before: f1/f2/f4/f5 differed, f1 at byte 12033, 36073 bytes.)
Counters on that decode: `sb128_rect: edge_vert=10 inter_64x128=9`.

## What is left for the next agent

- The full `cargo test -p ec-av1 --lib` suite was STARTED and then STOPPED by
  the cap order; it has NOT been run to completion on this tip. Run it first.
- Gate arm 46 (smooth `geq` source) was added to
  `a_real_aomenc_sb128_gathered_edge_horz_partition_decodes_pixel_exact`
  (`stream.rs:~19005`) BEFORE the cap message and IS committed; it and the
  sibling sb128 gates were run once, green:
  arms 42..46 six decode-order frames pixel-exact each, arm 46 edge +20 /
  inter 128-rect +18; `intra_rect_block` 5/5 arms exact.
- Oracle: `~/.cache/aom-oracle/src/av1/decoder/decodetxb.c` now prints
  `dcctx=%d` on the `EC_COEFF_STEP tag=sign` line (env `EC_TRACE_COEFF`);
  aomdec rebuilt with `ninja aomdec` (NOT `make`). Uncommitted, keep it.
