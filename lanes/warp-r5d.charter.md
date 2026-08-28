# lane-warp r5d charter — frame-13 HORZ_B mv-prediction mismatch (MiGrid stamping / mvstack corner probe)

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-warp, branch lane-warp @ d7d50f8 (or one
commit later). Build/test ONLY:
`env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-warp cargo test -p ec-av1 --release --lib pinned_warp_stream_decodes_pixel_exact -- --ignored --nocapture`
Never plain workspace cargo test; never touch other worktrees; never push; never touch fixtures/.

## Facts established across r5..r5c (do NOT re-derive; prior reports lanes/warp-r5.report.md, warp-r5c.report.md)
- Pin fixtures/warp-mismatch.obu: decodes 24/24, FAILS `frame 13 luma vs ffmpeg` (stream.rs:3501),
  quadrant (1,1) = rows/cols 32..64.
- Frame 13 quadrant (1,1) decodes through the INTER walk as PARTITION_HORZ_B: square 32x32 strip
  mv=(6,-8), leaves mv (6,8) and (6,-8) (/tmp/r5c_trace.txt:983-995 if still present; regenerate
  with EC_AV1_TRACE=1 otherwise). NOTE the leaf/square mv sign difference — if OUR decoded mv
  differs from libaom's here, the entropy path still matches (stream decodes to the end), so a
  wrong PREDICTED mv (mvstack) with compensating diff coding is possible only if... no: a wrong
  predicted mv changes the reconstructed mv and pixels but not symbol counts. That is exactly
  the observed shape (pixels wrong, no desync).
- mvstack.rs:533: the corner probe can SOLE-FILL the stack when row/col scans find nothing; the
  square's (6,-8) at quad (0,0)'s corner cell (7,7) is only reachable that way.
  scan_row/scan_col read correctly (mvstack.rs:377/450, start at mi_col/mi_row, col_shift=0 for
  offset -1).
- SUSPECT (r5c's open): the inter walk's MiGrid stamping writes wrong coordinate UNITS
  (mi vs 16px) for some blocks, so cells are missing at scan time and the corner probe
  sole-fills with the wrong candidate.
- 89122b0 (HORZ_B bottom-leaf coords trial): revert-bisected by orchestrator — IDENTICAL
  frame-13 failure with and without it. Not the cause. Its convention adjudication is STILL
  OWED: read decode_inter_block's position-param convention at its definition (~decode.rs:4774)
  and declare 89122b0 PROVEN-RIGHT or PROVEN-WRONG (revert it on the branch if wrong).

## Steps
1. `grep -n "grid.set\|fn record" crates/ec-av1/src/decode.rs` + read MiGrid::get/set (mvstack.rs)
   — establish the ONE true coordinate unit of MiGrid, then audit every inter-walk stamp site
   (square arms, HORZ/VERT arms, HORZ_B/VERT_A/VERT_B arms, SPLIT leaves) against it.
   Class decision-at-wrong-granularity + class context-read-from-one-cell.
2. For frame 13's first wrong block: dump the mvstack candidate list ours-vs-expected (libaom
   source is at /tmp/libaom-src if still present). The refmv weights class
   ([[av1-mvstack-refmv-corner]]): rng-per-symbol/value TRACE at the stack build, not hand
   reasoning.
3. Fix the stamping (or the corner-probe gating). Pin twice green. Then whole-crate
   `cargo test -p ec-av1 --release --lib`; paste final `test result:` line.
4. Adjudicate 89122b0 (see above).

## Done criteria
1. Pin PASSES twice (excerpts in report).
2. Full ec-av1 lib suite green (final test-result line).
3. All work committed to lane-warp (`wip(av1): warp r5d ...`), compiling — commit even on HANDOFF.
4. REPORT FILE lanes/warp-r5d.report.md, verdict FIRST line (PASS or HANDOFF + resume state),
   root cause with trace evidence, 89122b0 adjudication, dispositions.

## Hard rules
≤60 tool calls. AT CALL 45: STOP investigating; commit whatever compiles, write the report as
HANDOFF, only then continue if calls remain. r5b and r5c both lost findings to the cap —
the report is worth more than five more probe runs.
