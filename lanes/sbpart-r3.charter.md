# lane-sbpart r3 — bisect the rect64 luma mismatch

At 9379e99. Read `lanes/sbpart-r2.report.md` first — r2 built
`decode_block_rect64` and wired it at the SB-level `PARTITION_HORZ`/`PARTITION_VERT`
match (was a `_ =>` catchall), and built the gate that proves it fires. I
committed r2's uncommitted gate verbatim for you; it is RED on purpose.

## The one job
`a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact`
reaches the new code (`sb_rect_hits() > 0` against real aomenc content) and
mismatches ffmpeg's luma from partway through row 0 of a 192x128 frame. Bisect
it. This is a pixel defect with a bit-exact oracle available, so hand-tracing is
the wrong instrument: use `EC_AV1_GATE_DUMP` plus the oracle's `EC_TRACE` /
`EC_TRACE_COEFF` rungs (`scripts/instrument-aom-oracle.sh`, rebuilt with
`scripts/build-aom-oracle.sh` into `~/.cache/aom-oracle`), exactly the way
`decode_block_rect`'s own r3–r5 desync was closed. Add a rung rather than guess
when a fact is not observable.

r2's two prime suspects, neither checked: the corner-embed stride in the new
function versus `dequant_and_inverse_typed_wh`'s w/h ordering for non-square
64x32 vs 32x64 grids; and `default_scan(TX32)` versus the `scan32` other call
sites reuse (r2 believed these identical but never diffed them). Note the class
[[scan-weights-cross-axis]]: a scan weight or step can use the CROSS axis, and
square candidates hide every axis swap — if you touch scanning, sweep the
transposed copy in the SAME round.

Since the symbol reads are already right (the partition fires and the stream
does not desync), the defect is in reconstruction, not entropy — check the
range ladder only if a coefficient value proves wrong.

## Gate recipe facts r2 paid for — do not rediscover them
- `--enable-tx-size-search=0` is REQUIRED or every strip hits its own
  `tx-depth != 0` refusal before the pixels matter.
- `--enable-ab-partitions=0` does NOT gate AB partitions at 64x64: r2's blended
  `gradients`+`testsrc2` source made aomenc pick SB-level AB on ~40/40 attempts
  anyway. Plain `gradients_source` alone reaches real HORZ/VERT reliably. That
  aomenc quirk is out of this lane's scope; do not chase it.

## After it is green
Commit, then stage 2: the 32x32 `part32` values, then the inter path. Do not
start either while the gate is red.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbpart`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms; fixtures
through `gradients_source(seed, w, h, tail)`; every ffmpeg generate bounded with
`-t`; `EC_AV1_REQUIRE_AOMENC=1`; aomenc `--threads=1 --row-mt=0 --sb-size=64`.
Sibling worktrees have live agents — never build in or edit them. Never push,
never merge into main. 75-turn cap, does not reset: COMMIT AT EVERY GREEN STEP.
End with `lanes/sbpart-r3.report.md`, VERDICT on line 1.
