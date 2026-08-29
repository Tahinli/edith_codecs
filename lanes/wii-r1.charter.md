# lane-wii r1 charter — wedge-interintra blend

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-wii, branch lane-wii @ 6f4ca37.
Build/test ONLY: `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-wii cargo test -p ec-av1 --release --lib <name> -- --nocapture`
FOREGROUND, `nice -n 19`, `CARGO_BUILD_JOBS=4`. Never push; never touch other worktrees; fixtures/ is a symlink.
BUDGET: ~60 calls. Commit compiling WIP + report by call 45. Read ONLY what is named.

## Facts you build on (do not re-derive)
- Non-wedge interintra already decodes bit-exact (smooth-mask blend). The
  refusals to replace are decode.rs:6078 (16/32 path; `wedge` flag already
  consumed at 6075) and decode.rs:7316 (8x8 leaf path, wedge_interintra[3]).
- The COMPOUND_WEDGE codebook is on main: crates/ec-av1/src/wedge.rs
  (checksum-verified vs an independent C oracle, lanes/wedge_dump.c). It
  covers 8x8/16x16/32x32 — exactly the interintra wedge sizes here.
- libaom oracle /tmp/libaom-src:
  - av1/decoder/decodemv.c ~1546-1553: after use_wedge_interintra == 1, read
    `interintra_wedge_index` via wedge_idx_cdf[bsize] (16 symbols);
    interintra_wedge_sign is NOT read (fixed 0). CHECK our decode already has
    a wedge_idx CDF read on the compound path — reuse the same cdfs field the
    COMPOUND_WEDGE branch uses (verify the CDF is the SAME table in libaom —
    wedge_idx_cdf is shared — and that its counter-reset/save wiring already
    includes it; class cdf-counter-not-reset).
  - av1/common/reconinter.c combine_interintra + av1_build_interintra_pred:
    wedge interintra blends with av1_get_contiguous_soft_mask(wedge_index,
    1, bsize) — note the SIGN=1 there vs stored sign 0; port exactly. Chroma:
    check build_interintra_predictors_sbp / how chroma gets the mask
    (subsampled luma mask via aom_blend_a64_mask subw/subh — same shape as
    the COMPOUND_WEDGE chroma path already landed; mirror it).
  - The blend combines the INTRA predictor over the INTER predictor with the
    wedge mask replacing the smooth interintra mask — everything else about
    the existing interintra path (mode, intra pred construction) is unchanged.
- Existing interintra pins: fixtures ii-flake-*.obu are in the 15-pin default
  list — your regression canary.

## Scope
1. Replace both refusals: read interintra_wedge_index, thread it to the
   interintra blend, blend with the wedge mask (luma + chroma). Add a
   WII_HITS atomic (copy WEDGE_HITS pattern).
2. Gate: stream.rs has an interintra gate test (grep interintra in
   stream.rs tests). Add a wedge-interintra recipe variant: aomenc with
   --enable-interintra-wedge=1 (default on) + diagonal/structured content —
   the mandelbrot recipe that fired COMPOUND_WEDGE (grep the maskcomp gate)
   is the starting point; also try --enable-masked-comp=0 to push RD toward
   interintra wedge. Refusal string FORBIDDEN once fired; soft-skip zero-hit
   runs; hammer 6x with EC_AV1_GATE_DUMP self-pin
   (/tmp/claude-1000/wii-flake-N.obu). Mismatch = pin + report recon-diff
   location (EC_AV1_PREFILT_DUMP both sides), do NOT guess-fix.
3. 15-pin default list + full lib suite green.

## Done criteria
1. Both refusals replaced; WII_HITS fired live at least once (count in
   report) or the recipe search documented as residue with the blend
   codebook-verified; pins green; lib green.
2. Committed to lane-wii (wip commit after EVERY green milestone); REPORT
   lanes/wii-r1.report.md, verdict FIRST line, evidence per claim.
