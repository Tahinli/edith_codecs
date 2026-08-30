# lane-sbpart r4 — finish the rect64 bisect

At 58f7fb6. Read `lanes/sbpart-r3.charter.md` (still your charter — r3 never
reported) and `lanes/sbpart-r2.report.md`.

r3 stopped at its turn cap with the words "compiles clean, now run the gate". I
committed its edits to `decode.rs` and `stream.rs` verbatim so nothing was lost.
**Their state is unverified by me: run the gate first.** If it is green, r3
solved it and your job is to prove that, tidy the diagnostics out, and merge
main. If it is still red, keep bisecting.

The gate is `a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact`.
It reaches `decode_block_rect64` (`sb_rect_hits() > 0` on real aomenc content)
and mismatched ffmpeg's luma from partway through row 0 of a 192x128 frame.
Method is `EC_AV1_GATE_DUMP` plus the oracle's `EC_TRACE`/`EC_TRACE_COEFF` rungs,
not hand-tracing; add a rung to `scripts/instrument-aom-oracle.sh` if a fact is
not observable (numbers 6, 7, 8 and 8b are taken on main — take 9). r2's two
suspects, still unchecked as far as I know: the corner-embed stride versus
`dequant_and_inverse_typed_wh`'s w/h ordering for 64x32 vs 32x64; and
`default_scan(TX32)` versus the shared `scan32`. If you touch scanning, sweep the
transposed copy in the SAME round — a scan weight can use the cross axis and
square candidates hide every axis swap ([[scan-weights-cross-axis]]).

Gate-recipe facts already paid for, do not rediscover: `--enable-tx-size-search=0`
is REQUIRED or every strip hits its own tx-depth refusal first; and
`--enable-ab-partitions=0` does NOT gate AB partitions at 64x64, so use plain
`gradients_source` rather than a blended source.

Then merge `main` into this branch and resolve it yourself — main is at a8724cb,
far ahead (tile rows, delta_q/delta_lf, key-frame superres, palette-Y, a
`bit_depth != 8` refusal, three guard tests). Expect `refusal_inventory` and
`gate_coverage` to fail until this branch's lists match; that is them working.

A branch that lifts a refusal without a gate does not merge to main.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbpart`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms; fixtures
through `gradients_source(seed, w, h, tail)`; ffmpeg generates bounded with `-t`;
`EC_AV1_REQUIRE_AOMENC=1`; aomenc `--threads=1 --row-mt=0 --sb-size=64`. The
oracle is SHARED with five sibling lanes — env-gated rungs only, never a
throwaway patch left in the tree. Sibling worktrees have live agents — never
build in or edit them. Never push, never merge into main. 75-turn cap, does not
reset: COMMIT AT EVERY GREEN STEP. End with `lanes/sbpart-r4.report.md`, VERDICT
on line 1.
