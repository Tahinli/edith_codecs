# lane-gm r2 charter — CONTINUATION. r1 was pure recon; its HANDOFF is committed at lanes/gm-r1.handoff.md (d9890e3). READ THAT FIRST, then this.

Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-gm, branch lane-gm (main merged: the aomenc oracle now lives at ~/.cache/aom-oracle, gates work).
Build/test ONLY: `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-gm CARGO_BUILD_JOBS=4 nice -n 19 cargo test -p ec-av1 --release --lib <name> -- --nocapture`
FOREGROUND. Never push. BUDGET ~55 calls: WIP COMMIT after every green milestone; commit + report by call 45.
libaom source oracle: ~/.cache/aom-oracle/src (NOT /tmp any more).

## r1's open items — ALL RESOLVED by the orchestrator, do NOT re-read these
1. `gm_get_motion_vector` (mv.h:231) confirmed verbatim, incl. the spec's
   swapped-axis TRANSLATION bug (`row = wmmat[0] >> 13`, `col = wmmat[1] >> 13`,
   GM_TRANS_ONLY_PREC_DIFF = 16-3). ROTZOOM/AFFINE:
   `x = block_center_x(mi_col,bsize)`, `y = block_center_y(mi_row,bsize)`,
   `xc = (mat[2] - (1<<16))*x + mat[3]*y + mat[0]`,
   `yc = mat[4]*x + (mat[5] - (1<<16))*y + mat[1]`,
   `row = convert_to_trans_prec(hp, yc)`, `col = convert_to_trans_prec(hp, xc)`.
   `convert_to_trans_prec(hp,c)` = `ROUND_POWER_OF_TWO_SIGNED(c, 13)` when hp
   else `ROUND_POWER_OF_TWO_SIGNED(c, 14) * 2`.
   `is_integer` (force_integer_mv) -> `integer_mv_precision`: per component,
   `mod = v % 8; if mod != 0 { v -= mod; if abs(mod) > 4 { v += mod > 0 ? 8 : -8 } }`.
2. WarpParams (crates/ec-av1-syntax/src/frame.rs:366): `params[0..6]` IS
   `wmmat[0..6]`, same order, same WARPEDMODEL_PREC_BITS fixed point; `model`
   is GmType; `invalid` is already computed (`warp_valid`). Nothing to add
   to the parser.
3. gm candidates are NOT clamped in add_ref_mv_candidate; they are passed in
   as `gm_mv_candidates[2]` computed ONCE per block (mvref_common.c:795-830)
   from the CURRENT block's centre, and every scan receives them.

## One site r1 MISSED (orchestrator found it) — must be in scope
`setup_ref_mv_list`'s temporal path compares candidates against the gm mv, not
against zero: mvref_common.c:369-370 (single) and :397-400 (compound) use
`abs(this_refmv - gm_mv_candidates[0]) >= 16` for the `zero_mv_ctx` decision.
Ours hardcodes the zero comparison in mvstack.rs (`first_sample_far =
cand.mv.0.abs() >= 16 || cand.mv.1.abs() >= 16`). That is a MODE CONTEXT input,
so getting it wrong is an ENTROPY desync, not a pixel error. Fix it with the
rest.

## Implementation order (WIP commit each)
1. `gm_get_motion_vector` port + unit tests: one TRANSLATION case and one
   ROTZOOM case, expected values hand-computed IN THE TEST COMMENT showing the
   arithmetic (both hp and !hp), plus a force_integer_mv case.
2. mvstack: thread `gm_mv: [(i32,i32); 2]` + a per-neighbour `is_global_mv`
   flag (new `MiInfo` field; ctor sites decode.rs ~5416/~6348/~6600 per r1)
   into `find_mv_stack_with_sign_bias` and `find_mv_stack_compound`:
   - a neighbour that `is_global_mv_block` contributes the CURRENT block's
     `gm_mv[ref]`, not its own mv (mvref_common.c:88-92, 116-117);
   - `is_global_mv_block` = mode is GLOBALMV/GLOBAL_GLOBALMV AND bsize >= 8x8
     AND `gm.wmtype > TRANSLATION` (blockd.h; IDENTITY and TRANSLATION are
     both FALSE here);
   - the `zero_mv_ctx` comparison above.
   Square/IDENTITY callers must be unchanged — the 14-pin list proves it.
3. decode.rs: GLOBALMV single-ref (~6041-6070, currently hardcodes (0,0)) and
   GLOBAL_GLOBALMV compound (~5378) assign the derived mv(s).
4. Filter suppression — THE TRAP, two DIFFERENT predicates, never unify:
   `is_nontrans_global_motion` (reconinter.h ~420, used by
   `av1_is_interp_needed`) is false ONLY when `wmtype == TRANSLATION`, so
   IDENTITY COUNTS AS NONTRANS. Our `resolve_interp_filter` suppress arg is
   `is_globalmv || warped_selected` today, which is right only because every
   model is IDENTITY; it must become
   `(is_globalmv && gm.model != Translation) || warped_selected`.
5. motion_mode: `motion_mode_allowed` returns SIMPLE_TRANSLATION for an
   `is_global_mv_block` block when `!cur_frame_force_integer_mv` — so those
   blocks read NO motion_mode symbol (sites ~6180 and the 8x8 leaf ~6965).
   GLOBALMV under a TRANSLATION model still reads OBMC/WARPED normally.
6. MC: no new warp path (r1 verified) — global blocks predict through the
   existing `inter_predict` with the derived mv.
7. stream.rs: narrow/remove the whole-frame refusal (~203-218).

## Gate ladder (in order)
(a) 14-pin default list (`pinned_warp_stream_decodes_pixel_exact -- --ignored`);
(b) `a_real_aomenc_stream_with_interintra_wedge` — its 25-29/40 global-motion
    refusals MUST drop; print before/after counts in the report;
(c) free-partition + AB gates; (d) full lib
    `-- --skip a_real_aomenc_stream_with_interintra_wedge`.
Any mismatch: `EC_AV1_GATE_DUMP=/tmp/claude-1000/gm-flake-N.obu` self-pin, then
report the pin + the recon-diff location (EC_AV1_PREFILT_DUMP both sides).
Do NOT guess-fix. Landing with AFFINE still refused while TRANSLATION/ROTZOOM
decode pixel-exact is a GOOD outcome; shipping a wrong decode is not.

## Done criteria
Refusal narrowed or gone with pixel-exact evidence + refusal-count delta;
gates green; wip commits; report lanes/gm-r2.report.md, verdict FIRST line.
