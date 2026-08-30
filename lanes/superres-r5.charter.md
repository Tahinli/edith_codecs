# lane-superres r5 — build the instrument, then close the 2-pixel residue

At 48ba479. Read `lanes/superres-r4.report.md`.

## State
Everything but the last two pixels. The refusal-by-name, the spec 7.16 upscaler
pinned against real libaom, the stage-3 wiring, the `--superres-kf-denominator`
flag, the chroma-crop `ROUND_POWER_OF_TWO` fix and the margin threading are all
committed and verified: 237 passed, 1 failed — only
`a_real_aomenc_superres_key_frame_sequence_decodes_pixel_exact`, now **2 of 4096
luma pixels off by 1** (frame 2, rows 17-18, output column 62), down from 5.

r4 ruled out, by hand-verifying against the spec: the `filter8`/`filter14` tap
arithmetic, CDEF (forced off in this fixture), intra-prediction edge
availability using the wrong width, and a "row-swap" lead it debunked as a
numerical coincidence in smooth-gradient content. Its live suspect is the
deblock LEVEL decision (`edge_params`) for the edge whose transform straddles
the `frame_width` -> `true_width` margin.

## Your job is to stop hand-tracing and build the instrument
r4's own conclusion: hand-tracing our arithmetic cannot separate "our deblock
level or mask is wrong" from "our pre-deblock reconstruction is wrong", because
both are consistent with our own output. The only thing that can is libaom's
ground truth for that row.

`scripts/instrument-aom-oracle.sh` already carries env-gated rungs in exactly
the shape you need — `EC_TRACE=1` (partitions), `EC_TRACE_COEFF=1`,
`EC_TRACE_MODE=1`, and `EC_AV1_PREFILT_DUMP=<prefix>` (per-frame PRE-filter
recon). Add a rung that dumps the **post-deblock, pre-superres** row content
over `frame_width..true_width`, following the existing rungs exactly:
env-gated, silent when unset, idempotent, wrapper-around-impl. Rebuild with
`scripts/build-aom-oracle.sh`. Then diff it against `decode.rs`'s stashed
margin at the failing row.

Building that rung IS the deliverable of this round if it takes the whole
budget. Commit the script change as soon as it builds — it is reusable by every
lane, and instrument-building is a first-class subtask here, never an excuse.

Then: fix what it localises, get the gate green, and only after that start
stage 4 (inter-frame superres, spec 7.11.3.3 scaled-reference MC).

Merge note: main is at 53b319b with `gate_coverage.rs` and
`refusal_inventory.rs`. Report every refusal string you add or remove, verbatim.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-superres`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees (edith_codecs, -realworld, -lr, -tiles, -palette) have live agents —
never build in or edit them. Never push, never merge, never touch `main`.
75-turn cap, does not reset: COMMIT AT EVERY GREEN STEP. End with
`lanes/superres-r5.report.md`, VERDICT on line 1.
