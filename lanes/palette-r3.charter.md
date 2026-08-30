# lane-palette r3 — verify what r2 typed in, then the gate

At 2c85008. Read `lanes/palette.report.md` (r1's plan) and
`lanes/palette-r2.charter.md`.

## State
r2 committed two milestones, both cap-interrupted before verification:
- `760a0c8` — palette colour-index CDF tables + the 4-site wiring.
- `2c85008` — palette-Y reconstruction: colour cache, delta colours, the
  wavefront index map, prediction wiring.
Neither has been seen to pass the suite. That is your first job.

## Your job
1. `export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette`, `cargo check`,
   then `EC_AV1_REQUIRE_AOMENC=1 nice -n 19 cargo test -p ec-av1 --lib -j4`
   (timeout >= 600000 ms). Report the count against 234/0 for this tree.
   Getting 2c85008 green is the whole job until it is green. COMMIT.
2. The gate. Palette is a screen-content tool: aomenc only picks it on flat,
   few-colour, repetitive content, so the fixture likely needs a synthetic
   few-colour pattern rather than a gradient — and it must be deterministic, so
   hash it twice and prove it. `--enable-palette=1`, probably
   `--tune-content=screen`, plus `--threads=1 --row-mt=0`. Two facts sibling
   lanes paid for: this decoder hardcodes 64px superblocks so any
   multi-superblock fixture needs `--sb-size=64`, and a per-superblock symbol
   cannot fire in a single-superblock fixture. HARD-assert a thread-local count
   of palette blocks reconstructed. COMMIT.
3. Then palette UV, then the rect-strip `palette_bsize_ctx` refusal at
   decode.rs ~1940, then intrabc. One commit each.

A duplicate effort on branches `lane-palette-sn`, `-fa`, `-fb` (worktrees
removed, branches kept) covered the same CDF tables. Ignore them unless yours
is missing something; `lane-palette-sn`'s report notes it transcribed
`default_palette_{y,uv}_color_index_cdf` from
`~/.cache/aom-oracle/src/av1/common/entropymode.c:679-784` as one const per
palette size.

Merge note: main is at 06d29ee with `gate_coverage.rs` (pins the aomenc tools no
gate exercises — `enable-palette` and `enable-intrabc` are both on it) and
`refusal_inventory.rs` (pins every decode-path refusal string). Report every
refusal string you add or remove, verbatim.

Hard rules: foreground `nice -n 19 cargo ... -j4`; every `cargo test` a timeout
>= 600000 ms. Sibling worktrees have live agents — never build in or edit them,
and do NOT create additional worktrees. Never push, never merge, never touch
`main`. 75-turn cap, does not reset: COMMIT AT EVERY GREEN STEP. End with
`lanes/palette-r3.report.md`, VERDICT on line 1.
