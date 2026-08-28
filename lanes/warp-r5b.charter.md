# lane-warp r5b charter — continuation HANDOFF: frame-13 quadrant-(1,1) luma mismatch

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-warp, branch lane-warp @ 876b0b1.
Previous round (report: lanes/warp-r5.report.md): extended inter partitions HORZ_B/VERT_A/VERT_B
implemented; pin no longer refuses, decodes 24/24 frames, FAILS `frame 13 luma vs ffmpeg`
(stream.rs:3501). Do NOT revisit the partition work or anything about warp itself —
frames 1-2 hold ALL the warp blocks and they PASS.
Build/test ONLY: `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-warp cargo test -p ec-av1 --release --lib pinned_warp_stream_decodes_pixel_exact -- --ignored --nocapture`
Never plain workspace `cargo test`; never touch other worktrees; never push.

## Known facts (do not re-derive)
- Mismatch is frame 13, luma, quadrant (1,1) = rows 32..64, cols 32..64 of the 64x64 frame.
- EC_AV1_TRACE run artifact from last round: /tmp/warp_r5_trace.txt (may be gone; regenerate
  with EC_AV1_TRACE=1 if needed). Only 10 single-ref EC_TRACE blocks in the whole stream, all
  frames 1-2. Frames 3+ printed key-walk partition traces only — frame 13 is either a KEY frame
  or an inter frame decoding all-intra blocks; UNRESOLVED which.
- Fixture: fixtures/warp-mismatch.obu (aomenc seed 49, 64x64, 24 frames); oracle = ffmpeg
  (the pin test compares against ffmpeg_decode_sequence itself).

## Steps (from the previous builder's dying words)
1. `grep -n "TRACE partition" crates/ec-av1/src/decode.rs` — key walk ~3369 vs inter ~7304:
   determines whether frames 3+ (incl. 13) go through the key/intra walk.
2. Instrument frame 13's failing quadrant: if key walk → per-block intra trace (mode/ctx +
   pixel stats) for rows/cols 32..64; if inter-all-intra → mirror the EC_TRACE at ~decode.rs:5801
   into the is_inter=0 branch at ~decode.rs:4923.
3. Diff ours vs ffmpeg at frame 13 block granularity; find the FIRST wrong block; root-cause.
   Classes to check first: context read from one cell (neighbour ctx gathered over the span?),
   trial map not restored, CDF row held constant. If entropy desync suspected: count symbol
   reads per block (class symbol-consumption-gap) before tracing one; compare msac RANGE not
   tell() across decoders.
4. Fix. Pin twice green. Then full crate suite: `cargo test -p ec-av1 --release --lib`
   (whole lib), paste final `test result:` line.

## Done criteria
1. Pin PASSES twice (output excerpts in report).
2. Full ec-av1 lib suite green (final test-result line in report).
3. All work committed to lane-warp (`wip(av1): warp r5b ...`), compiling — commit even on HANDOFF.
4. REPORT FILE lanes/warp-r5b.report.md: verdict first (PASS or HANDOFF + exact resume state),
   root cause named with trace evidence, what changed, dispositions.

## Hard rules
≤60 tool calls (commit + report BEFORE the cap: at call ~50 stop investigating, commit, write
the report as HANDOFF). Never checkout files with uncommitted work. Never touch fixtures/ contents.
