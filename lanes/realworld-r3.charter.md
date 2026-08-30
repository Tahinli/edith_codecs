# lane-realworld r3 — the part64 silent desync, then delta_q / delta_lf

Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-realworld`, branch
`lane-realworld`, at ad0832c. Read `lanes/realworld-r2.report.md` first — it
carries the spec refs, the CDF wiring checklist and a ready-made
refusal-reproduction recipe. Do not re-derive it.

## State — blocker 1 is CLOSED
`bd61617` (cdef_idx reads) + `080acfd` (its gate) + `ad0832c` (report). Gate
`a_real_aomenc_stream_with_cdef_decodes_pixel_exact`: 40/40 pixel-exact, zero
refusals, `cdef_idx_hits()` hard-asserted `> 0` (24-40 per run). Suite 233/0.

r2's finding worth keeping: `cdef_bits` selects among per-SUPERBLOCK strength
profiles, so a single-64x64-superblock fixture can NEVER reach `bits > 0` —
20 attempts kept `cdef_idx_hits()` at 0. The gate needed a 128x64, two-superblock
fixture.

## Job 1 — a silent desync, fix it before anything else
`decode_inter_frame_tile_with_cdfs` (decode.rs ~9397) reads `part64` and then
throws it away (`let _ = part64;`), blindly assuming SPLIT. A real non-SPLIT
`part64` in an inter frame **silently desyncs** — it does not refuse by name.
That is the one thing this project treats as never acceptable: refuse by name or
decode, never desync.

Minimum: make it refuse by name, accurately, and prove the refusal with a test.
Better, if it is small: handle the non-SPLIT cases the way the intra path does.
Either way this is its own commit, and the refusal string must describe what
THIS DECODER does not do — not what an encoder supposedly never writes.

Related, out of scope but record it: at `--cpu-used=0` aomenc picks SB-level
`HORZ_4` / `VERT_B` partitions that the intra `part64` match at decode.rs:5049
does not cover, even with `--enable-rect/ab/1to4-partitions=0`. r2 sidestepped
it with `--cpu-used=4`. Do NOT chase it here; note it in the report.

## Job 2 — delta_q / delta_lf (blocker 2), unstarted
Spec 5.11.15 `read_delta_qindex`, 5.11.16 `read_delta_lflevel`; libaom
`av1/decoder/decodemv.c` `read_delta_qindex` / `read_delta_lflevel`. New
ADAPTING CDFs: four wiring sites — struct field, defaults array, save/restore,
per-frame counter reset (`cdf_state.rs`; `reset2`/`reset3` are length-generic
and save/restore is a whole-struct Clone, so the defaults array is what needs
hand-checking, but verify the counter reset covers the new tables — class
`cdf-counter-not-reset`: a missing reset gives right values at the wrong
adaptation rate). Plus the running quantizer / loop-filter-delta state threaded
through block decode; the deltas are read once per superblock at the first
non-skip block. Then remove the refusal at stream.rs
("a frame with delta_q_present or delta_lf_present set"). Its own gate, its own
hard-asserted firing counter, its own commit.

## Gate rules
`EC_AV1_REQUIRE_AOMENC=1` on every run; `-t <seconds>` on every ffmpeg generate;
fixtures through `gradients_source(seed, w, h, tail)`; aomenc
`--threads=1 --row-mt=0`; firing counts are HARD asserts via thread-local
`Cell<usize>` counters. Size the fixture so the feature can actually fire —
r2's cdef lesson generalises: a per-superblock symbol needs more than one
superblock.

## Note for the merge
Main now carries two guards you do not have yet: `gate_coverage.rs` (pins the
aomenc tools no gate exercises — `enable-cdef` is on that list and your gate
closes it, so that entry must be deleted at merge) and `refusal_inventory.rs`
(pins every decode-path refusal string; adding or removing one fails until the
list is updated). Say in your report exactly which refusal strings you added or
removed, verbatim, so the merge can update both lists.

## Hard rules
`export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-realworld`; foreground builds
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees (edith_codecs, -chroma, -lr, -superres, -tiles, -palette) have live
agents — never build in or edit them. Never push, never merge, never touch
`main`. 75-turn cap, does not reset: commit at every green step. End with
`lanes/realworld-r3.report.md`, VERDICT on line 1.
