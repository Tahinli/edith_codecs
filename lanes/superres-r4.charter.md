# lane-superres r4

At f167362 — r3's harness + decode work, committed verbatim by the orchestrator
at its cap and **never seen to compile or pass**. Read `lanes/superres-r3.charter.md`
(still binding) and `lanes/superres.report.md`.

1. `cargo check`, then the target gate, then the full suite. Getting f167362
   green is the whole job until it is green. COMMIT.
2. The residue r3 was chasing: 5/4096 luma pixels off by 1 at x=59..62 of a
   64-wide frame. The decisive instrument is the extended
   `scripts/superres-pin-harness.c` — run the REAL 43->64 ratio through libaom's
   own exported functions and compare column by column. r1 pinned only
   in8->out12 and in8->out16; a kernel pinned at two small symmetric ratios says
   nothing about phase arithmetic at the ratio that fails
   (class `instrument-at-bound`).
3. Then stage 4: inter-frame superres, scaled-reference MC (spec 7.11.3.3,
   libaom `av1_setup_scale_factors_for_frame` / `av1_convolve_2d_scale`).

Gate rules unchanged: `EC_AV1_REQUIRE_AOMENC=1`, `-t <seconds>` on every ffmpeg
generate, fixtures through `gradients_source`, aomenc `--threads=1 --row-mt=0`,
firing counts HARD-asserted. Two facts sibling lanes paid for: this decoder
hardcodes 64px superblocks, so any multi-superblock fixture needs
`--sb-size=64` (aomenc defaults to 128 and lands in a dead-ended partition
gap); and a per-superblock symbol cannot fire in a single-superblock fixture.

Merge note: main is at 06d29ee with `gate_coverage.rs` and `refusal_inventory.rs`,
which pin the aomenc tools no gate exercises and every decode-path refusal
string. Report the refusal strings you add, rename or remove, verbatim.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-superres`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees (edith_codecs, -realworld, -lr, -tiles, -palette) have live agents —
never build in or edit them. Never push, never merge, never touch `main`.
75-turn cap, does not reset: COMMIT AT EVERY GREEN STEP. End with
`lanes/superres-r4.report.md`, VERDICT on line 1.
