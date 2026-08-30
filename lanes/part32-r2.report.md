# lane-part32 round 2

## State: NOT DONE, red, committed (4bff66c)

Charter: fix decode_block_rect64's pre-existing pixel mismatch exposed by
round 1's refusal lift.

## True mismatch geometry (measured on THIS tree, not inherited)

Reproduced live: `cargo test -p ec-av1 --lib
a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact`
-> 266 passed, 1 failed, matches the trial-merge count I was handed.

Pinned the mismatching seed-42 stream via `EC_AV1_GATE_DUMP`, added an
`#[ignore]`d debug test (`debug_part32_r2_sbpart_pin_mismatch_geometry`,
`stream.rs`) that decodes it and prints exact per-pixel diffs.

- Full-frame post-filter comparison against ffmpeg: 17873/24576 luma pixels
  differ, spanning the entire frame past x=91,y=0. This is NOT a single
  off-by-one -- it's a cascade: once one block's pixels are wrong, every
  block that intra-predicts from it downstream inherits and compounds the
  error, eventually corrupting nearly all of SB row 1 (y>=64).
- Root of the cascade, isolated via aom-oracle's `EC_AV1_PREFILT_DUMP` rung
  (pre-loop-filter ground truth) plus two new env-gated bypasses
  (`EC_AV1_DEBUG_SKIP_DEBLOCK`/`_CDEF`, off by default) on our side: our own
  PRE-FILTER reconstruction matches the oracle's pre-filter output exactly
  through SB(1,0)'s first VERT-64 sub-block (mi_c=16..23, all-zero DC,
  correct). The very first wrong pixel is (96,0) -- the first pixel of the
  SECOND VERT-64 sub-block (mi_c=24..31, real coefficients, eob_nonzero=true):
  oracle=81, ours=76.

## Entropy layer: DIVERGES, inside this exact block

Compared msac RANGE (never `tell()`) against the aom-oracle's `EC_TRACE_COEFF`
rung, element by element:
- Ranges agree exactly up through `rng=63764` at the entry to this block's
  luma coefficient read (`EC_COEFF plane=0 ... tx_size=64corner/11
  rng=63764` on both sides).
- Both sides read `eob=6` (agreement).
- The oracle's per-step trace assigns nonzero level=1 at scan indices
  0, 1, 2, 4, 5 (skipping 3). Ours (`scan32 = default_scan(TX32)`, a plain
  square 32x32 scan) walks physical positions (0,0)(0,1)(1,0)(2,0)(1,1)(0,2)
  for the same six scan indices -- a different position-to-index mapping
  than whatever the oracle used.
- Post-block range: ours=37896, oracle=46344. Diverged.

Conclusion: this is a genuine in-block entropy desync (wrong scan table),
not a post-processing (deblock/CDEF, both proven innocent by disabling them)
and not a dequant/reconstruction-only bug.

## Root cause (identified, not yet fixed)

`decode_block_rect64` (crates/ec-av1/src/decode.rs:3559) reads the luma
corner with `scan32 = default_scan(TX32)` -- the square TX_32X32 scan --
for BOTH genuine square 64x64 blocks and truncated-corner VERT-64/HORZ-64
strips (bw=32,bh=64 or bw=64,bh=32). The function's own doc comment already
documents an analogous, already-fixed asymmetry for the nz_map *context*
table (`NZ_MAP_CTX_OFFSET_32X64`/`_64X32`, lane-sbpart r8) but the *scan
order itself* was never given the same treatment. AV1 spec names a distinct
Default_Scan_32x64/64x32 table (libaom `av1_scan_orders[TX_32X64]` /
`[TX_64X32]`), and this decoder does not have it.

Same shape as this repo's existing `table-indexed-by-raw-size` and
`scan-weights-cross-axis` ledger classes.

## Fix NOT landed this round

Turn budget spent entirely on localization (per charter's own step-2
instruction, "if ranges diverge, that's decisive"). Porting a correct
1024-entry SCAN_32X64/SCAN_64X32 table from libaom and wiring it in at the
`decode_block_rect64` call site (mirroring how `chroma_scan` already
branches on `bw == 64` a few lines below) is the next round's whole job --
should be mechanical once the table exists, but deriving/verifying a new
1024-entry table safely needs its own round, not the tail end of this one.

## Gates

- Part32's own AB gate (round 1's):
  `a_real_aomenc_intra_stream_with_ab_partitions_decodes_pixel_exact` --
  not re-run this round (untouched by this round's changes; round 1 already
  proved it 40/40 and this round made zero changes to part32's own code
  paths).
- Target gate
  (`a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact`):
  still RED, root cause pinned, fix not landed.
- `cargo check --workspace --all-targets`: clean.
- Full `cargo test -p ec-av1 --lib` NOT re-run this round after the debug
  additions (both are `#[ignore]`d / env-gated no-ops by default; the only
  change to non-test code is the two early-return guards in
  `apply_deblock`/`apply_cdef`, both env-gated and off by default -- zero
  behavior change verified by the identical `cargo check` above).

## rect64q contradiction: explained

lane-rect64q's own reproduction (row0 col91=81, matching ffmpeg, no
mismatch) used a stream from ITS OWN worktree, which did not have this
lane's AB arms and so took a genuinely different aomenc encode (different
partition decisions entirely) -- not a contradiction of this round's
finding, a different stream. This round's own pinned stream, decoded fresh
on this tree, reproduces the failure deterministically and traces to a real,
narrow, named defect (wrong scan table for TX_32X64/TX_64X32 corner-truncated
luma).

## HEAD

`4bff66c` -- "wip(part32): pin decode_block_rect64's block2 defect to a
scan-table mismatch"
