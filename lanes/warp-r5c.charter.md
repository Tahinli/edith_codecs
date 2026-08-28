# lane-warp r5c charter — frame-13 INTRA-walk quadrant-(1,1) luma mismatch

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-warp, branch lane-warp @ 89122b0.
Build/test ONLY: `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-warp cargo test -p ec-av1 --release --lib pinned_warp_stream_decodes_pixel_exact -- --ignored --nocapture`
Never plain workspace `cargo test`; never touch other worktrees; never push; never checkout
files with uncommitted work; never touch fixtures/ contents.

## Facts established (do NOT re-derive)
- Pin fixtures/warp-mismatch.obu (aomenc seed 49, 64x64, 24 frames, oracle = ffmpeg in-test):
  decodes 24/24, FAILS `frame 13 luma vs ffmpeg` (stream.rs:3501), mismatch isolated to
  quadrant (1,1) = rows 32..64, cols 32..64.
- Frame 13 goes through the KEY/INTRA walk: with EC_AV1_TRACE=1, key-walk partition traces
  print WITHOUT `mi=` at w64 (decode.rs:3355) while the inter walk prints WITH `mi=`
  (decode.rs:7330); frames 3+ show the key-walk shape. All warp/inter machinery is exonerated
  for this frame (frames 1-2 hold every inter/warp block and PASS).
- The extended-partition work (r5) and the HORZ_B coord trial (r5b, commit 89122b0) are in the
  INTER walk only — not frame 13's code path. Do not touch them for the mismatch. SECONDARY
  task (only if calls remain after the pin is green): adjudicate 89122b0's coordinate
  convention — read decode_inter_block's position parameter convention (mi vs 32-block index)
  at its definition and write PROVEN-RIGHT or PROVEN-WRONG with the line numbers in the report;
  revert the commit if wrong.
- Intra walk: decode.rs ~3340-3660 (w64 → w32 → w16 → w8 partition ladder with TRACE prints).
  Key-frame intra decode of a block: decode_key_frame-side leaf functions called from there.

## Steps
1. Reproduce once with EC_AV1_TRACE=1, capture to /tmp/r5c_trace.txt. Confirm frame 13 is the
   14th decoded frame in the trace (count frame boundaries; beware hidden/no-show frames —
   class gate-blind-to-hidden-frames: decode order may not equal the pin's display index 13).
2. Localize: dump our frame-13 recon and ffmpeg's (the test already prints a diff map on
   failure — the o/./X grid; read it first). Find the FIRST mismatching block in raster order
   within rows/cols 32..64.
3. For that block, trace the intra symbols (mode, partitions, tx, coeffs) and check classes in
   this order: context read from one cell (ctx gathered from a single neighbour cell where the
   span is mixed-size); CDF row held constant; trial map not restored; mirror the decoder's
   shift chain. If entropy desync suspected: count symbol reads per block FIRST
   (class symbol-consumption-gap), and compare msac RANGE not tell().
4. Root-cause, fix, pin twice green, then whole-crate `cargo test -p ec-av1 --release --lib`;
   paste the final `test result:` line.

## Done criteria
1. Pin PASSES twice (output excerpts in report).
2. Full ec-av1 lib suite green (final test-result line).
3. All work committed to lane-warp (`wip(av1): warp r5c ...`), compiling — commit even on HANDOFF.
4. REPORT FILE lanes/warp-r5c.report.md, verdict first (PASS or HANDOFF + exact resume state),
   root cause named with trace evidence, the 89122b0 adjudication (or `not reached`),
   dispositions.

## Hard rules
≤60 tool calls. At call ~50: STOP investigating, commit whatever compiles, write the report as
HANDOFF. An unwritten report or an uncommitted worktree = the round is void; your last-round
predecessor lost its findings exactly this way.
