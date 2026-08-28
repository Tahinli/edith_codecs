# lane-partitions r1 charter — decode rect partitions HORZ + VERT (inter tiles)

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-partitions, branch lane-partitions @ 5e25f76.
Build/test ONLY: `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-partitions cargo test -p ec-av1 --release --lib <name> -- --nocapture`
FOREGROUND, `nice -n 19`, `CARGO_BUILD_JOBS=4`. Never plain workspace cargo test; never push; never touch other worktrees; fixtures/ is a symlink — only new pinned .obu streams go in.

## Facts (rectgate r1 just proved these — do not re-derive)
- decode_frame's inter-tile `match part32` (crates/ec-av1/src/decode.rs ~7550-7945)
  handles ONLY PARTITION_NONE / SPLIT / HORZ_B; HORZ(1), VERT(2), HORZ_A(4),
  VERT_A(6), VERT_B(7), HORZ_4(8), VERT_4(9) fall to the generic named-refusal `_` arm.
- The free-partition recipe (git history of lanes/rectgate-r1, and the doc comment on
  a_real_aomenc_stream_with_free_partitions_decodes_pixel_exact in stream.rs) matched
  0/40 because of this — that recipe is your gate once HORZ/VERT land.
- HORZ_B's existing arm (decode.rs ~7782, lane-warp r5) is the pattern to follow:
  32x32-context partition symbol, strip decode via decode_inter_block with rectangular
  reject_residual handling (a non-skip 32x16 strip refuses residual by name — see
  ~4983). Read its whole arm plus the `reject_residual` plumbing before writing code.
- aomenc quirk: --enable-1to4-partitions=0 does NOT suppress VERT_4 in this build, so
  your gate CANNOT exclude _4 partitions by flag — the _4 refusal must skip the seed
  by name instead.

## Scope r1: PARTITION_HORZ and PARTITION_VERT at the 32x32 level
1. Implement the HORZ (two 32x16 strips) and VERT (two 16x32 strips) arms mirroring
   HORZ_B's strip machinery. libaom reference: decodeframe.c decode_partition
   (PARTITION_HORZ/VERT sub-block walk), spec 5.11.4. Mind: partition ctx updates
   (above/left partition context bytes — HORZ/VERT set different above/left context
   than SPLIT; grep how HORZ_B updates them and check libaom's
   update_ext_partition_context), neighbour records at 32x16/16x32 granularity
   (record_inter/record_mi/grid.set spans — a rectangular block covers a non-square
   mi span: sweep EVERY grid/neighbour write that assumes size means square, class
   context-read-from-one-cell / neighbour-votes-all-its-fields), tx size for
   rectangular blocks (TX_32X16/TX_16X32 — check the tx-size derivation and the
   deblock tx_grid fill), and chroma at the subsampled rectangle.
2. If a rectangular residual path is needed beyond skip (HORZ_B currently refuses
   non-skip strips), keep that refusal wording for r1 — entropy-exact skip-strip
   decode is the milestone; residual is r2. But if the existing refusal already
   covers it, just extend it to the new arms.
3. Gate: reactivate the FREE recipe in
   a_real_aomenc_stream_with_free_partitions_decodes_pixel_exact (rect=1, ab=1, no
   min/max clamp — the doc comment describes it): HORZ/VERT refusals now FORBIDDEN;
   HORZ_A/VERT_A/VERT_B/HORZ_4/VERT_4 + screen-content refusals still skip the seed
   by name; assert matched > 0 and that a HORZ or VERT arm actually fired (extend
   EXTENDED_PARTITION_HITS or add RECT_PARTITION_HITS). Hammer 8x with
   EC_AV1_GATE_DUMP self-pin armed (/tmp/claude-1000/rect-flake-N.obu).
4. Any mismatch: pin to fixtures/, recon-dump diff (EC_AV1_PREFILT_DUMP both sides) →
   range ladder (EC_AV1_TRACE part32_pre vs patched aomdec EC_PART rng at
   /tmp/libaom-build/aomdec; ours[k]=aom[k+1], ours skips the key frame) → per-label
   EC_AV1_TELL (aomdec prints post-adaptation, ours pre). Fix root cause, rerun.

## Done criteria
1. Free-partition gate green 8/8 with rect hits > 0; existing 11 pins byte-exact;
   `cargo test -p ec-av1 --release --lib` all green.
2. All work committed to lane-partitions; REPORT lanes/partitions-r1.report.md,
   verdict FIRST line, evidence excerpts (gate counts, pin results).
Commit + report BEFORE budget runs out; entropy-exact + named residual refusal is
mergeable progress. Never rm globs; never checkout files with uncommitted work.
