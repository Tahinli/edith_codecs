# lane-mergefix r2 handoff

Tip: lane-mergefix, commit "var-tx above/left txfm contexts were never reset at a tile
boundary". Read `lanes/mergefix-r2.report.md` first -- it has the full ladder.

## Fixed this round (do not re-hunt)
`Neighbours::start_tile` now resets `above_txfm` over the tile's column span and
`Neighbours::start_row` resets `left_txfm`, both to `TXFM_CTX_INIT` (64). All 14
`txfm_partition` symbols of the pinned stream now match the instrumented oracle in ctx and
range. Oracle instrumentation: `EC_VARTX=1` on `~/.cache/mergefix-tmp/aom-build/aomdec`
(additive fprintf in `read_tx_size_vartx`, shared oracle build untouched).

## The remaining defect (gate still RED, 3622 px, first at frame 1 row 0 col 64)

Stream `~/.cache/mergefix-tmp/str61.obu` (md5 a14892ed0ba88b6ad2b566e251ea2d33,
`~/.cache/mergefix-tmp/mkstr.sh`), 192x68 cq61 tile_cols=1, frame 1.

FIRST DIVERGING SYMBOL: the 64x64 partition of superblock mi(16,32) (last SB row, only 1 mi
row inside the frame; second tile column). Both decoders enter at rng=51641.

    aomdec: EC_PART mi_row=16 mi_col=32 bsize=12 ctx=13 rng=51641 -> value=3 (SPLIT), rng 38495
    ours  : EC_PARTG mi_row=16 mi_col=32 bsize=12 ctx=0  rng=51641 -> gathered bit 0 -> HORZ

aomdec's ctx is printed as `bsl*4 + ctx` => its real ctx is 1 (above=1, left=0); ours is 0.
`Neighbours::partition_ctx_mi` (decode.rs ~4985) reads `above_side_mi[mi_c] * 2 <= side`, so
`above_side_mi[32]` is 64 in ours and must be 32: the block above is the 32x64 strip of the
SB-level PARTITION_VERT at mi(0,32). Find the SB-level (64x64) PARTITION_VERT arm of the
INTER tile path and make its two 32x64 strips record their own width/height into
`above_side_mi`/`left_side_mi` (`record_rect_mi`, w=32 h=64), not the parent's 64.
The 32-level and 16-level gathered reads at that SB already have the right ctx (0/0).

Reproduce the ladder:
  ours : `EC_TRACE_PART=1 EC_TRACE_MODE=1 <target>/debug/examples/decode_probe str61.obu`
  aom  : `EC_TRACE_MODE=1 EC_TRACE_COEFF=1 EC_VARTX=1 ~/.cache/mergefix-tmp/aom-build/aomdec --rawvideo -o /dev/null str61.obu`

Also open (class `equal-range-means-unread`, may be the same defect): after the fix ours
reads ONE txfm_partition symbol the oracle never reads --
`EC_ISTEP mi_row=16 mi_col=36 name=txfm_split_rect val=0 ctx=12 rng=37790` -- at the same
bottom-edge SB row. Expect it to disappear once the partition above is right.

Sibling gates NOT re-run: split-transform gate, sb128c intra_rect_block +
gathered_edge_horz, uv8/intersub8/inter16ab/rectchroma2. Run them in ONE
`cargo test -p ec-av1 --lib -- <names>` before merging.
