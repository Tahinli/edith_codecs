# lane-partab r1 charter — AB partition arms (HORZ_A / VERT_A / VERT_B) decode

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-partab, branch lane-partab @ 6f4ca37.
Build/test ONLY: `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-partab cargo test -p ec-av1 --release --lib <name> -- --nocapture`
FOREGROUND, `nice -n 19`, `CARGO_BUILD_JOBS=4`. Never push; never touch other worktrees; fixtures/ is a symlink.
BUDGET: ~60 calls. Commit compiling WIP + report by call 45. Read ONLY what is named.

## Facts you build on (verified this batch, do not re-derive)
- HORZ/VERT/HORZ_B already decode for real: 32x16/16x32 strips thread true
  write_w/write_h through mvstack (MiInfo.size_h), warp samples, OBMC, deblock
  tx_h_grid, and the motion_mode/obmc CDF rect rows (cdf.rs rows 4=BLOCK_16X32,
  5=BLOCK_32X16). All sub-block sizes an AB partition produces (16x16, 32x16,
  16x32) ALREADY decode.
- The INTER dispatch is decode.rs ~8013 `match part32 {` with arms
  NONE/HORZ/VERT/SPLIT/HORZ_B; the catch-all `_ =>` at ~8616 refuses
  HORZ_A(4)/VERT_A(6)/VERT_B(7)/HORZ_4(8)/VERT_4(9).
- Spec sub-block layout at a 32x32 area, mi origin (r,c) in 4x4 units of 8
  (i.e. `at = (r32*2, c32*2)` is 16px units — CHECK the existing arms' `at`
  arithmetic and copy it; lane-warp r5 once lost a round to `at` being 16px
  units, decode.rs:4858-9 comment):
  - HORZ_A: 16x16 at (r,c) + 16x16 at (r,c+16px) + 32x16 strip at (r+16px,c)
  - VERT_A: 16x16 at (r,c) + 16x16 at (r+16px,c) + 16x32 strip at (r,c+16px)
  - VERT_B: 16x32 strip at (r,c) + 16x16 at (r,c+16px) + 16x16 at (r+16px,c+16px)
  (verify each against /tmp/libaom-src/av1/common/enums.h + decodeframe.c
  decode_partition's PARTITION_HORZ_A/VERT_A/VERT_B cases — the A/B naming is
  easy to mirror-swap; libaom is the oracle.)
- decode_inter_block's trailing 3 args are (is_rect_strip: bool, write_w, write_h)
  in mi-of-16px units (SUB=16px class, BLOCK=32px class) — copy how the HORZ and
  VERT arms pass them for the strip + how SPLIT passes for 16x16 leaves.
- Partition CDF/ctx for the arms is already read correctly (VERT_ALIKE/
  HORZ_ALIKE gathers include values 4..9); ONLY the dispatch refuses.

## Scope
1. decode.rs ~8013 match: add PARTITION_HORZ_A / PARTITION_VERT_A /
   PARTITION_VERT_B arms composing the SAME decode_inter_block calls the
   HORZ/VERT/SPLIT/HORZ_B arms already make (order = spec decode order).
   Declare consts PARTITION_HORZ_A=4, PARTITION_VERT_A=6, PARTITION_VERT_B=7
   next to PARTITION_HORZ_B. Mind per-arm neighbour/ctx recording: copy
   exactly what the existing arms do after each sub-block (record_* calls).
2. HORZ_4/VERT_4 and the INTRA-frame dispatch (~3624) stay refusing — do not
   touch them.
3. Gate ladder, in order:
   a. full 15-pin default list green (pins contain HORZ_B streams — regression
      canary): cargo test -p ec-av1 --release --lib pinned_warp -- --nocapture
   b. free-partition gate a_real_aomenc_stream_with_free_partitions_decodes_
      pixel_exact 6x with EC_AV1_GATE_DUMP self-pin
      (/tmp/claude-1000/partab-flake-N.obu). A mismatch = pin it, report the
      recon-diff location (EC_AV1_PREFILT_DUMP both sides), do NOT guess-fix.
   c. full lib suite.
4. If aomenc's free recipe never emits an AB partition (grep your decode for
   the arm actually firing — add a PARTAB_HITS atomic like WEDGE_HITS), find a
   recipe that does (aomenc --enable-ab-partitions=1 is default-on; try
   structured content / lower cq) and add a dedicated gate test that forbids
   the AB refusal string, soft-skipping on zero-hit runs like the maskcomp
   gate. Verified-but-unfired is reportable; unverified is not landable.

## Done criteria
1. AB arms decode; hits proven fired at least once (atomic count in report);
   pins green; free gate 6/6; lib green.
2. Committed to lane-partab (wip commit after EVERY green milestone); REPORT
   lanes/partab-r1.report.md, verdict FIRST line, evidence per claim.
