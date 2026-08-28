# lane-warp r5e charter — frame-13 HORZ_B: build libaom inspect, diff per-block mv/ref, root-cause leaf B

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-warp, branch lane-warp @ 0004e2e (+ report
commit). Prior reports: lanes/warp-r5.report.md, r5c, r5d — read r5d FIRST, all facts there hold.
Build/test ONLY:
`env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-warp cargo test -p ec-av1 --release --lib pinned_warp_stream_decodes_pixel_exact -- --ignored --nocapture`
Never plain workspace cargo test; never touch other worktrees; never push; never touch fixtures/
contents. C builds of libaom go in /tmp/libaom-build (exists, CONFIG_INSPECTION=1), NOT under any
cargo dir.

## The one question this round answers
Frame 13, quadrant (1,1), HORZ_B (strip mv=(6,-8), leafA=(6,8), leafB=(6,-8), all ref=4=GOLDEN?):
our entropy matches (stream decodes; symbol counts fine) but every block's pixels are wrong,
leafB 256/256. EITHER our reconstructed mv/ref per block differs from libaom's (mv PREDICTION
defect — mvstack winner) OR mv/ref match and our MOTION COMPENSATION is wrong for these blocks
(subpel filter path, ref buffer, position). libaom inspect decides in one run.

## Steps
1. Build the inspect tool: in /tmp/libaom-src cmake build dir /tmp/libaom-build,
   `make inspect` (examples/inspect.c; target name `inspect`). If the target is absent,
   reconfigure with `-DCONFIG_INSPECTION=1 -DENABLE_EXAMPLES=1`. Budget: <=8 calls. If it will
   not build in that budget, STOP and use plan B: aomdec patched printf at
   av1/decoder/decodeframe.c read_inter_block_mode_info (dump mi_row,mi_col,bsize,mv,ref) —
   still <=8 more calls.
2. Run inspect on fixtures/warp-mismatch.obu, extract frame 13 (decode order! count carefully —
   hidden frames may shift the index; match by comparing frame dimensions/count), quadrant
   (1,1) rows/cols 32..64: per-block bsize, mv, ref_frame, interp filter, motion_mode.
3. Diff against ours (EC_AV1_TRACE=1 run, /tmp/r5c_trace.txt shape). Decision table:
   - mv/ref DIFFER → mvstack winner defect: dump both candidate lists for the first differing
     block; classes av1-mvstack-refmv-corner (weights), context-read-from-one-cell (stamping),
     neighbour-votes-all-its-fields. Fix the stack build.
   - mv/ref MATCH → MC defect: leafB (bottom-right 16x16, filter=[0,0]) is the lead — check
     its MC source coordinates (16px vs mi confusion at the leaf call site was already one bug,
     class decision-at-wrong-granularity), ref slot mapping for GOLDEN, subpel filter selection
     for filter=[0,0], and OBMC/interintra application on leaves.
4. Fix. Pin twice green. Whole-crate `cargo test -p ec-av1 --release --lib`, paste final line.

## Done criteria
1. The decision-table verdict (mv/ref differ vs match) stated WITH the two dumps excerpted.
2. Pin PASSES twice, full lib suite green — or HANDOFF with the verdict at minimum.
3. All work committed (`wip(av1): warp r5e ...`) — commit even on HANDOFF.
4. REPORT FILE lanes/warp-r5e.report.md, verdict FIRST line.

## Hard rules
<=60 tool calls. AT CALL 40: commit whatever compiles + write the report as HANDOFF, then
continue only if calls remain. r5b/r5c/r5d ALL lost their commit+report to the cap — do not
be the fourth.
