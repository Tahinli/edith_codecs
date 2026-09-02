# lane-mergefix r1 handoff

Tip: see `git log -1` on lane-mergefix (fix commit "tx_size_context_txfm read
above_inter/left_inter at SUB granularity, not mi").

## Bisection of the two reds (each gate run by name on a detached worktree)

- `5d685f2` (main + intersub8): BOTH gates green.
- `d8967dd` (+ uv8): split-tx gate green, straddle gate RED (arm `68x192 cq35
  frames=5 10bit=true tile_cols=0`, first diff row 28 col 15, ours 196 vs 197).
- `4a449fd` (+ inter16ab): BOTH red.
- Neither red is a merge-resolution artifact: `git show --cc d8967dd -- crates/ec-av1/src/decode.rs`
  is EMPTY (clean merge, no hand-resolved hunk). Both are real decode defects that the
  merge UNCOVERED by lifting a refusal (class `refusal-hides-a-defect`): at `d8967dd`
  seed 54 was one of the split-tx gate's 21 named refusals (`buckets counted-exact=1
  uncounted-exact=9 named-refusals=21`); after inter16ab it decodes and mismatches.

## GATE 2 -- `a_real_aomenc_10bit_inter_sequence_with_a_split_transform_intra_block_decodes_pixel_exact` : FIXED, GREEN

Stream pinned: `~/.cache/mergefix-tmp/mk54.sh` -> `s54.obu`, md5
`c5c1c564e98b5d6348601edab6f7508e` (hashed twice, identical). 64x64 10-bit, seed 54.

First diverging element (ours `EC_TRACE_MODE_STEP` vs instrumented aomdec, decode order):
frame 4, mi(4,12) (16x16 INTRA block inside an inter frame -- the block that owns the
reported pixel diff at row 16 col 48):

    ours: EC_ISTEP mi_row=4 mi_col=12 name=tx_depth val=1 ctx=0        rng=46620
    aom : EC_ISTEP mi_row=4 mi_col=12 name=tx_depth val=1 ctx=1 cat=1  rng=51092

Everything before that (partition ladder, mode/mv ladder, all earlier blocks' coefficients)
is range-identical.

Root cause, `decode.rs` `tx_size_context_txfm`: `Neighbours::above_inter`/`left_inter` are
written PER MI cell (`record_inter_rect_mi`, `self.above_inter[c + cell]` over `0..w_mi`),
but the function indexed them `n.above_inter[mi_c / (SUB / MI)]` / `n.left_inter[mi_r / (SUB / MI)]`
-- i.e. it asked the block two mi away whether it was inter. libaom `get_tx_size_context`
(`pred_common.h`) lets an INTER neighbour contribute its BLOCK size instead of its transform
size; here the left neighbour mi(4,8) IS a var-tx-split inter 16x16, so libaom gets
`left = block_size_high(16) >= 16 = 1` while we fell back to `left_txfm[4] (=8) >= 16 = 0`
and read the `tx_depth` CDF from row 0 instead of row 1.

Hunk (2 lines + comment) to port onto main:

    -    if has_above && n.above_inter[mi_c / (SUB / MI)] {
    +    if has_above && n.above_inter[mi_c] {
    -    if has_left && n.left_inter[mi_r / (SUB / MI)] {
    +    if has_left && n.left_inter[mi_r] {

Verified: `cargo test -p ec-av1 --lib -- <that gate name>` -> ok, buckets
`counted-exact=1 uncounted-exact=0 named-refusals=0 (attempts 1)`.

## GATE 1 -- `a_frame_edge_straddling_band_decodes_pixel_exact` : STILL RED, no fix

Unchanged by the tx_depth fix. Failing arm at the tip: `192x68 cq61 frames=5 10bit=false
tile_cols=1`, frame 1 plane Y, 6859 px, first at row 0 col 64.
Stream pinned: `~/.cache/mergefix-tmp/mkstr.sh` -> `str61.obu`, md5
`a14892ed0ba88b6ad2b566e251ea2d33`.

First diverging element (same cross-decoder range ladder, EC_MODE/EC_ISTEP/EC_COEFF_STEP with
`tag=br|sign|golomb` and aomdec's no-op `tag=tx_type` dropped): frame 1 (our `EC_PICT idx=0`),
block mi(0,32) -- the 64x64 at x=128 split PARTITION_VERT into two 32x64 INTER strips
(aomdec `EC_PART mi_row=0 mi_col=32 bsize=12 value=2`). Mode/mv reads still agree
(`EC_MODE rng=46411`, `EC_MODE_VAL rng=47465` both sides). The divergence is inside the
var-tx tree of that 32x64 strip:

    ours (from 47465): txfm_split_rect ctx=1 val=1 rng=51444
                       txfm_split      ctx=4 val=1 rng=45408   (mi 0,32)
                       txfm_split      ctx=4 val=1 rng=43372   (mi 8,32)
                       then all_zero rng=64228 ...
    aom  (from 47465): [var-tx symbols NOT traced by the oracle]
                       EC_COEFF plane=0 row=0 col=0 tx_size=2 rng=43818
                       all_zero rng=64988 ...

So aomdec reaches the first TU at range 43818 where we reach 43372; both end at TX_16X16
leaves, so the SHAPE agrees and the divergence is in ctx/CDF of one of the three
`txfm_partition` symbols.

Suspect + what was already verified by hand against libaom `txfm_partition_context`
(`av1_common_int.h`) -- all three CHECKED OUT, so do not re-audit them blind:
- `decode.rs` `txfm_partition_ctx_rect` (~12291): category for TX_32X64 in BLOCK_32X64 is
  `0` (`txsize_sqr_up_map[TX_32X64] == get_sqr_tx_size(max(32,64)) == TX_64X64`), for the
  TX_32X32 children `1` -> bases 0 and 3; our ctx 1 and 4 are consistent with
  above/left = (0,1) and (1,0) respectively, which is what the neighbour state should be
  at frame top row (`above_txfm` init 64) with a decoded left SB.
- `read_block_tx_size_rect` (~12790) reads exactly 3 symbols (1 rect + 2 square), matching
  libaom's `MAX_VARTX_DEPTH == 2` recursion from TX_32X64.
- Symbol COUNT and leaf shape match aomdec's TU list.

Therefore the next step is NOT more reasoning about the ctx formula: instrument the oracle.
`~/.cache/aom-oracle/src` has NO `read_var_tx_size` trace rung (grep: only rungs from
`scripts/instrument-aom-oracle.sh`), so add a getenv-gated (`EC_VARTX`) fprintf around
`txfm_partition_cdf[ctx]` in libaom's `read_var_tx_size`/`read_tx_size_vartx` printing
`mi_row/mi_col/tx_size/ctx/rng`, rebuild only aomdec, and diff the three symbols. Gate the
print on a NEW env var so other lanes' traces are unaffected; never `git checkout` in that
tree (it carries other lanes' uncommitted instrumentation).

Second lead if the ctx turns out identical: the `txfm_partition` CDF ROW may have adapted
differently earlier (a row swap that decodes the same value can leave the range identical
while adapting the wrong row) -- check the per-frame counter-reset list for
`txfm_partition` and the key frame's own reads.

Note the earlier-arm history: at `d8967dd` this gate failed on `68x192 cq35 10bit=true
tile_cols=0`; at the tip that arm no longer fails first, so there may be more than one
defect behind this gate.
