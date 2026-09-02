# lane-vert46 r1 -- the INTER 64x128 block at a partial 128 superblock

Branch `lane-vert46`, worktree `edith_codecs-vert46`, base 103b66f
(= lane-sb128c r8). `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vert46`.

## Premise (re-measured, not inherited)

The sb128c r8 residue stream is reproducible: the arm-46 recipe (192x256
8-bit `geq` sinusoids, `--sb-size=128 --min-partition-size=64
--enable-rect-partitions=1 --cq-level=55 --cpu-used=0 --limit=5`, WITHOUT
`--kf-max-dist=1`) hashed twice to `ccc3bafd6d1c42b1336a69e68482e54b`, the
same md5 as `$HOME/.cache/sb128c-r8/vert46.obu`. Frame layout: mi 64x48, two
128 superblocks per row, the RIGHT column partial (`mi_col + hbs == mi_cols`),
so its 128 root reads the gathered bit. Frame 1 resolved it to
PARTITION_VERT = one BLOCK_64X128 inter block; frames 0 and 3 were exact,
1/2/4/5 differed (first differing byte 12033 = luma (x=129, y=62)).

## Two root causes (both entropy, found on the msac range ladder)

1. `crates/ec-av1/src/decode.rs:20146` (new) + `:17400`, `:18379`, `:19483`
   -- CHROMA ENTROPY CONTEXT RE-STAMPED PER BLOCK. A block wider or taller
   than 64 codes its chroma as one 32x32 unit per 64x64 mu chunk, and each
   unit stamps its own DC sign / level into the neighbour bands
   (`record_mi_chroma`, libaom `av1_set_entropy_contexts` inside
   `decode_reconstruct_tx`). The end-of-block record then OVERWROTE all of
   them with `neighbour_state` of the ASSEMBLED whole-block grid, i.e. with
   the top-left unit's DC. First diverging element: frame 1, block mi(0,32),
   plane 1, chunk 0, the DC sign symbol -- pre-symbol range 39712 on both
   sides, aomdec `dcctx=2`, ours `ctx=1`, post-symbol range 41596 vs 64592.
   Ours read the left neighbour as the 128x128 block's mu-chunk (0,0) DC
   (negative) where libaom reads its mu-chunk (0,16) DC (positive). Fix:
   collect the per-unit chroma grids and re-stamp them after the block record.
2. `crates/ec-av1/src/decode.rs:20179` -- VAR-TX BLOCK RECORDED AS SQUARE.
   `record_split_luma_rect_mi(at, side, side, ..)` wrote
   `above_side`/`left_side` = `side` (128) for a 64x128 var-tx block; the
   comment "a var-tx block is always square" stopped being true when the 128
   root's HORZ/VERT arm landed (sb128c r7). The NEXT superblock's 128-root
   partition symbol then read ctx 16 where aomdec reads ctx 19 (aomdec
   `EC_PART mi_row=32 mi_col=32 bsize=15 ctx=19 rng=43152`, ours entered the
   64 level at rng=35032). Fix: pass the block's own `write_w`/`write_h`.

Both are the same shape as memory classes `context-read-from-one-cell` and
`tx-grid-published-block-side` -- a neighbour band published with the BLOCK's
geometry instead of the transform unit's / the block's true footprint.

EVIDENCE: $HOME/.cache/vert46-tmp/{our,aom}.f0..f5 | EC_AV1_FINAL_DUMP decode_probe v.obu vs instrumented aomdec, cmp per frame | 6/6 decode-order frames byte-identical (was 4 of 6 differing, 36073 bytes on frame 1)
EVIDENCE: $HOME/.cache/vert46-tmp/{a.lad,o.lad2} | EC_TRACE_COEFF range ladders, aomdec vs ours | first divergence moved 81 -> 109 -> none of 120 elements

## Gate

New arm 46 of the existing sb128 inter gate
(`a_real_aomenc_sb128_gathered_edge_horz_partition_decodes_pixel_exact`,
`crates/ec-av1/src/stream.rs:19005`): the same 192x256 VERT geometry as arm
44/45 but with the SMOOTH `geq` source instead of `mandelbrot` -- that is what
makes the encoder put a 64x128 edge block next to neighbours whose partition
context and chroma DC sign actually decide symbols. Arms 42-45 are unchanged
and still hard-assert the edge/inter counters; every decode-order frame
(hidden alt-refs included) is compared against the instrumented aomdec.

    cargo test -p ec-av1 --lib sb128 -- --nocapture
    ... seed 42 384x320 8bit HORZ:  6 frames pixel-exact (1 hidden), edge +2  inter +2
    ... seed 43 384x320 10bit HORZ: 6 frames pixel-exact (1 hidden), edge +24 inter +24
    ... seed 44 192x256 8bit VERT:  6 frames pixel-exact (1 hidden), edge +2  inter +2
    ... seed 45 192x256 10bit VERT: 6 frames pixel-exact (1 hidden), edge +2  inter +2
    ... seed 46 192x256 8bit VERT:  6 frames pixel-exact (1 hidden), edge +20 inter +18
    test result: ok. 3 passed; 0 failed; 1 ignored (sibling sb128 gates included)

EVIDENCE: $HOME/.cache/vert46-gate-r1.log | cargo test -p ec-av1 --lib sb128 --nocapture | 5/5 edge arms + 5/5 intra-rect arms + the CfL/1to4 gate pixel-exact, arm 46 fires 20 edge / 18 inter 128-rect blocks

Siblings re-run in the same command (COMMON's SIBLING GATES rule):
`a_real_aomenc_sb128_intra_rect_block_decodes_pixel_exact` 5/5 arms exact
(128x64 +28/+30/+14, 64x128 +10/+10) and the sb128 CfL/1to4 gate.

## Refusals

None lifted or added -- both defects were silent wrong answers, not refusals.

## Suite

`cargo test -p ec-av1 --lib` under a systemd unit (MemoryMax=10G):
see `$HOME/.cache/vert46-suite-r1.log` (totals appended below when it lands).

## Residue

- accepted: the fix collects the per-unit chroma grids into a small `Vec` per
  128-root block (2 or 4 units x 2 planes of 32x32 i32). Only blocks above 64
  pay it; below 64 `mu_chroma` is false and the vector is never allocated.
- deferred: the intra (key-frame) 128 path has the same end-of-block chroma
  record shape but its own reader; sb128c r8's intra gate is green on 5 arms,
  so no instance is measured there. Unblocked by a key-frame stream whose 128
  block sits LEFT of another coded block -- the arm-46 source with
  `--kf-max-dist=1` is exactly that and passes today.
