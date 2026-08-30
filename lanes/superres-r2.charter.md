# lane-superres r2 — finish the stage-3 gate, then inter-frame superres

Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-superres`, branch
`lane-superres`, at 150bbdc (+ the pin harness commit).

## State (r1's, verified where it says so)
Committed and green:
- `79252c8` — `use_superres` refused BY NAME in `crates/ec-av1/src/stream.rs`.
  It was silent wrongness before: the header parsed fully but decode stored the
  `Picture` at the downscaled `frame_width` instead of `upscaled_width`. Gate
  `a_real_aomenc_stream_with_superres_refuses_by_name` proves it against a real
  `--superres-mode=1` stream.
- `aec76d1` — `crates/ec-av1/src/superres.rs`: the spec 7.16 horizontal
  upscaler (`upscale_row` / `upscale_plane`), coefficient table and constants
  transcribed from libaom `resize.c` / `convolve.c`, pinned against the REAL
  exported `av1_get_upscale_convolve_step` / `av1_convolve_horiz_rs_c` /
  `av1_resize_filter_normative` through a C harness. Three unit tests green.
  The harness is now committed at `scripts/superres-pin-harness.c` (it lived in
  a reapable scratchpad — class `oracle-in-reaped-dir`); build it against
  `~/.cache/aom-oracle/build/libaom.a` if you need to extend the pins.
- `150bbdc` — stage-3 wiring: the `use_superres` refusal narrowed to
  `frame_type != Key`; key frames call `crate::superres::upscale_picture` after
  `decode_key_frame_tile_with_cdfs`, before storage/output; `SUPERRES_HITS`
  thread-local + `superres_hits()`; `upscale_picture` covers Y/U/V with 4:2:0
  chroma halved by `div_ceil(2)`. Compiles clean.

r1 also corrected an error in the original charter: libaom's
`decodeframe.c` calls `superres_post_decode()` BETWEEN `av1_cdef_frame` and
`av1_loop_restoration_filter_frame` — **superres runs BEFORE loop restoration**,
not after. Recorded in `superres.rs`'s module doc. Not load-bearing while LR is
refused, but lane-lr will need it.

## Not landed
- The stage-3 gate `a_real_aomenc_superres_key_frame_sequence_decodes_pixel_exact`
  (in 150bbdc) FAILS with `unsupported: AV1 tile (a partition type this encoder
  never writes)`. That is a pre-existing capability gap in the bare
  `--superres-mode=1 --superres-denominator=12 --cpu-used=0` recipe, not a
  superres defect — the same shape the warp gate's original recipe hit.
- r1's fix was in flight when its budget ran out and **did not apply** — the
  worktree is clean. Redo it: give the gate the restrictive flag set the warp
  gate uses (`--enable-rect-partitions=0 --enable-ab-partitions=0
  --enable-1to4-partitions=0 --enable-filter-intra=0 --enable-smooth-intra=0
  --enable-paeth-intra=0 --enable-directional-intra=0 --enable-angle-delta=0
  --enable-tx-size-search=0 --enable-cdef=0 --enable-restoration=0
  --max-partition-size=32 --min-partition-size=32 --enable-palette=0
  --enable-intrabc=0 --enable-cfl-intra=0`), around stream.rs:3860-3875.
- Stage 4 (inter frames referencing a differently-sized reference — superres
  mode 2 / qthresh) is not started. It needs spec 7.11.3.3 scaled-reference
  motion compensation, which is genuinely new work, not wiring.
- `lanes/superres.report.md` was never written.

## Order of work — COMMIT AFTER EVERY GREEN STEP
1. Apply the flag-set fix; run just that gate
   (`cargo test -p ec-av1 --lib a_real_aomenc_superres_key_frame_sequence -j4 -- --nocapture`,
   timeout >= 600000 ms). It must decode pixel-exact AND hard-assert
   `superres_hits() > 0`.
2. Full scoped suite `EC_AV1_REQUIRE_AOMENC=1 nice -n 19 cargo test -p ec-av1 --lib -j4`
   (~4 min, timeout >= 600000 ms). Baseline: 232/0 on this tree; main now
   carries 234/0 with two new guard tests you will inherit at merge. COMMIT
   stage 3.
3. Stage 4: inter-frame superres. Scaled-reference MC, spec 7.11.3.3; libaom
   `av1_setup_scale_factors_for_frame` / `av1_convolve_2d_scale`. Its own gate,
   its own firing counter, its own commit.

## Note on the coverage guard
Main now carries `crates/ec-av1/src/gate_coverage.rs`, which derives from the
gate source the aomenc tools switched off in every gate and on in none, and
pins that set. The flag list above only disables tools already in the pinned set
(cdef, intrabc, paeth-intra, palette, smooth-intra) plus tools other gates leave
on, so it will not trip the guard — but run the full suite before committing.

## Gate rules (mandatory)
`EC_AV1_REQUIRE_AOMENC=1` on every run; `-t <seconds>` on every ffmpeg generate;
fixtures through `gradients_source(seed, w, h, tail)`; aomenc
`--threads=1 --row-mt=0`; the firing count is a HARD assert, never a warning.

## Hard rules
- `export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-superres`. Foreground
  builds, `nice -n 19 cargo ... -j4`, every `cargo test` given a timeout of at
  least 600000 ms — a 120 s default kills the suite mid-run.
- Sibling worktrees (edith_codecs, -chroma, -realworld, -lr, -tiles) have live
  agents. Never build in or edit them.
- NEVER push, never merge, never touch `main`. Commit on `lane-superres` only.
- 75-turn cap; commit at every green step; near the cap commit whatever
  compiles as `wip(av1): ...`.
- Finish with `lanes/superres.report.md`, VERDICT on the first line.
