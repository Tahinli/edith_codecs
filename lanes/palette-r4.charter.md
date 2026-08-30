# lane-palette r4 — locate the desync; the gate must FAIL, not SKIP

At e7e16ab. Read `lanes/palette-r3.report.md`.

## State
r2's two milestones are verified green (234/0). r3 wrote a real-aomenc palette-Y
gate: `palette_hits` is hard-asserted and fires, so the reconstruction path is
non-vacuous — but the reconstructed pixels MISMATCH ffmpeg's decode of the same
bytes. r3 checked `read_palette_colors_y`, `read_uniform`,
`palette_color_index_context` and the `PALETTE_Y_COLOR_INDEX` table
line-for-line against the oracle source; all match. The desync site is unlocated.

## Job 1 — this is the priority, before any debugging
The refusal was removed and the pixels are wrong, so a real palette stream now
decodes to WRONG PIXELS instead of refusing. That is the one outcome this
project does not accept, and the gate currently SKIPs on the mismatch rather
than failing, which hides it. Fix the shape first, in its own commit:
- Either restore an accurate refusal in front of palette reconstruction ("a
  block that actually uses a palette -- the index map decodes but the
  reconstructed pixels do not match libaom yet"), keeping all the landed code
  behind it, and let the gate assert THAT refusal;
- or make the gate FAIL on the mismatch, so the defect is visible in the suite.
Pick the first if you cannot close the pixel gap this round. A SKIP on a known
wrong result is not an acceptable resting state, and neither is silent
wrongness. COMMIT this before anything else.

Note that removing `enable-palette` from `gate_coverage.rs`'s `NEVER_EXERCISED`
was right — a real block does reconstruct from a palette now — and stays right
either way.

## Job 2 — locate the desync with the ladder, not by re-reading tables
r3 already proved the tables and context functions match the oracle
line-for-line. Re-reading them again is the [[worker-cap-spent-reading]] trap.
Use the instrument instead:
- CLASS `compare-range-not-tell`: compare the msac RANGE against the oracle
  after each element, never `tell()`.
- CLASS `equal-range-means-unread`: if the reference's range is UNCHANGED where
  ours moves, we read a symbol it never wrote; if theirs moves and ours does
  not, we skipped one. Only when both move by different amounts is a table
  implicated — and r3 has already cleared the tables.
- Oracle rungs, all env-gated: `EC_TRACE=1` (partitions), `EC_TRACE_COEFF=1`,
  `EC_TRACE_MODE=1` (inter + intra mode info), `EC_AV1_PREFILT_DUMP=<prefix>`
  (per-frame PRE-filter recon — the right one for separating a prediction bug
  from a loop-filter one). If you need a palette-specific rung, add it to
  `scripts/instrument-aom-oracle.sh` in the existing shape (env-gated, silent
  when unset, idempotent, wrapper-around-impl) and rebuild with
  `scripts/build-aom-oracle.sh`. Building the rung is a first-class subtask.
- A prediction-only mismatch with a CORRECT symbol stream shows up as identical
  ranges and different pixels — check that first: if the range ladder matches
  to the end of the block, the index map and colours are right and the bug is
  in how the palette is applied (per-TU slicing, the `PALETTE_PRED`
  thread-local's lifetime, or chroma/luma plane mixing), not in the reads.

## Then
Palette UV, the rect-strip `palette_bsize_ctx` refusal (decode.rs ~1940), and
intrabc — one commit each, each with its own hard-asserted firing count.

Merge note: main is at 53b319b with `gate_coverage.rs` and
`refusal_inventory.rs`, which pin the never-exercised tools and every
decode-path refusal string. Report every refusal string you add or remove,
verbatim.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees (edith_codecs, -realworld, -lr, -superres, -tiles) have live agents —
never build in or edit them, and do NOT create additional worktrees. Never push,
never merge, never touch `main`. 75-turn cap, does not reset: COMMIT AT EVERY
GREEN STEP. End with `lanes/palette-r4.report.md`, VERDICT on line 1.
