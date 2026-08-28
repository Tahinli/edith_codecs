# lane-rectgate r1 charter — dedicated rect/ab-partition-enabled decode gate

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-rectgate, branch lane-rectgate @ cf09a3d.
Build/test ONLY: `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-rectgate cargo test -p ec-av1 --release --lib <name> -- --nocapture`
FOREGROUND, `nice -n 19`, `CARGO_BUILD_JOBS=4`. Never push; fixtures/ is a symlink — only new pinned .obu streams go in. GATE-ONLY LANE: no decoder changes expected; if the gate finds a decode defect, PIN the stream + localize (recon dump → range ladder) and report — fix only if small and root-caused, else report the pin for the next round.

## Why
Extended partitions HORZ_A/HORZ_B/VERT_A/VERT_B + rect HORZ/VERT decode landed during
the warp lane (merged 8f90552) but were only exercised incidentally: every existing
aomenc gate passes partition-pinning flags. There is NO gate that lets aomenc choose
free partitions. Debt line: "extended partitions ... lack a dedicated
rect/ab-partition-enabled gate".

## Scope r1
1. Read the warp gate (stream.rs, a_real_aomenc_stream_with_warped_motion...) and the
   interintra gate (~3480) for the recipe shape. Identify the flags that pin
   partitions (min/max partition size / sb-size / disable-rect args in the aomenc
   invocation) and REMOVE the pinning in the new gate:
   a_real_aomenc_stream_with_free_partitions_decodes_pixel_exact. Keep resolution
   small (64x64 or 128x64) so 8x8 leaves and rect splits actually appear; consider
   --enable-rect-partitions=1 --enable-ab-partitions=1 explicitly.
2. Named refusals for genuinely-unimplemented capabilities (screen content, wedge
   interintra, masked compound, partitions below the decoder's floor if any) skip the
   seed; assert matched>0. Add a partition-diversity assertion: count decoded
   partition kinds (there are existing counters or add a cheap atomic) and require at
   least one non-SPLIT/none rect or ab partition fired across the sweep — a gate that
   never sees the feature proves nothing (class gate-preset-gates-the-feature).
3. EC_AV1_GATE_DUMP self-pin on mismatch; hammer 8x with the pin armed
   (EC_AV1_GATE_DUMP=/tmp/claude-1000/rect-flake-N.obu). Pin any failure into
   fixtures/ and add to the pinned test's default list ONLY if it decodes green after
   a fix; otherwise leave the pin + localization in the report.
4. Existing 11-pin test and ec-av1 lib suite must stay green.

## Done criteria
1. New gate green 8/8 with the diversity assertion firing, OR a pinned failing stream
   + recon-dump/range-ladder localization in the report (that is a PASS for a gate
   lane — finding the defect is the product).
2. All work committed to lane-rectgate; REPORT FILE lanes/rectgate-r1.report.md,
   verdict FIRST line.
