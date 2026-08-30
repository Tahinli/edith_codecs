# lane-lr r6 — one pixel

At 0f36e31. Read the r5 section of `lanes/lr.report.md`.

**Correction to the r5 charter, on the record:** it said HEAD had "never been
seen to compile". That was the orchestrator's inference from a cap-time commit,
and it was wrong — r4's work compiles clean and all four charter steps
(Wiener + self-guided at both call sites, the gate widened to assert `Ok` and
pixel-exactness, the `stream.rs` refusal dropped) are genuinely finished. r5
spent its round diagnosing instead, which was the right call.

## The defect
`a_real_aomenc_stream_with_restoration_reads_lr_symbols_correctly` fails
deterministically — reproduced identically on every rerun, not a flake:
**one pixel of 6144**, V plane, row 61 col 6, ours 195 vs ffmpeg 194. Seed 46,
filter `Sgrproj { ep: 6, xqd: [-16, -32] }`, inside chroma's last partial
restoration stripe [60, 64) of a 64px plane. Y and U are bit-exact on every
attempt; only this one (plane, pixel, filter) combination diverges.

## Ruled out by r5, do not redo (class `worker-cap-spent-reading`)
- The stripe-boundary substitution (`lr_sample`), hand-verified line-by-line
  against libaom's `setup_processing_stripe_boundary` /
  `save_deblock_boundary_lines` for the failing stripe. Matches exactly.
- The dense (`r1 = 1`) SGR arm's A/B grid values and final combine, dumped live
  and independently recomputed in Python from the real cdef/deblocked bytes.
  Correct.
- Standalone C reimplementations of the box filter (`/tmp/lr_ref.c`,
  `/tmp/lr_probe.c`) produced mutually inconsistent numbers across runs of
  identical code. Do not trust them or their 194-vs-195 claim; they are not
  evidence. If you want C ground truth, call libaom's own exported function
  through a harness linked against `~/.cache/aom-oracle/build/libaom.a`, the way
  `scripts/superres-pin-harness.c` does — that is the pattern that works here.

## Start here
The fast (`r0 = 2`) arm's individual neighbour A/B reads at (row 61, col 6),
which r5 ran out of budget to instrument the same way it did the dense arm.
If that also checks out, go upstream to the V-plane CDEF/deblock pixel data
feeding the stripe.

Sibling lanes built oracle rungs you can use: `EC_AV1_POSTDEBLOCK_DUMP` and
`EC_AV1_PREFILT_WIDE_DUMP` (in `scripts/instrument-aom-oracle.sh`, aomdec
already rebuilt on this box) give libaom's own post-deblock and wide pre-filter
buffers — exactly the upstream check above, without hand-tracing. Note also that
this is a partial stripe at a plane edge and a 1-LSB divergence: rounding in the
partial-stripe height or an edge-clamped neighbour is the shape to suspect.

## Then
Once green: COMMIT immediately. Then remove any remaining LR refusal, confirm
`gate_coverage.rs`'s `NEVER_EXERCISED` no longer needs `enable-restoration`
(main's copy — you may not have the file), and write the r6 report.

Merge note: main is at 53f5358 and has moved (multi-tile key frames, chroma
smooth/paeth, CDEF index reads, three reworded refusals, and
`refusal_inventory.rs` which pins every decode-path refusal string). Consider
`git merge main` here and resolving it yourself before the branch is merged
back — you know which side owns each hunk. Report every refusal string you add,
rename or remove, verbatim.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-lr`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees have live agents — never build in or edit them. Never push, never
merge into `main`. 75-turn cap, does not reset: COMMIT AT EVERY GREEN STEP.
Update `lanes/lr.report.md` with an r6 section, VERDICT first.
