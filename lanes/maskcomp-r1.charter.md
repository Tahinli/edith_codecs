# lane-maskcomp r1 charter — decode masked compound (comp_group_idx==1): wedge + diffwtd

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-maskcomp, branch lane-maskcomp @ cf09a3d.
Build/test ONLY: `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-maskcomp cargo test -p ec-av1 --release --lib <name> -- --nocapture`
Run builds/tests FOREGROUND with `nice -n 19` and `CARGO_BUILD_JOBS=4` (background tasks get killed on this box). Never plain workspace cargo test; never touch other worktrees; never push; fixtures/ is a symlink — only NEW pinned .obu streams captured by a gate go in.

## Current state (do not re-derive)
- comp_group_idx and compound_idx SYMBOLS are already consumed on the compound path
  with comp_group_idx==1 a NAMED refusal — grep `comp_group_idx` in
  crates/ec-av1/src/decode.rs (the refusal site is on the compound read path near the
  jnt_comp/compound_idx reads). This round replaces the refusal with the port.
- Non-wedge interintra just landed (cf09a3d): `interintra_blend`, ii_weights1d, and the
  ref_frame1=Some(0) INTRA marker in the mi grid — patterns to mirror, do not disturb.
- CDF wiring checklist is FOUR sites (memory cdf-counter-not-reset): cdf.rs default +
  cdf_state.rs field + init + reset_counts. Miss reset_counts and you get a late
  rotating flake with an exact range ladder.
- Reference libaom at /tmp/libaom-src: decodemv.c ~1600-1650 (masked_compound_used,
  compound_type read, wedge_idx/wedge_sign, mask_type for DIFFWTD), reconinter.c
  av1_make_masked_inter_predictor / build_compound_diffwtd_mask /
  av1_get_compound_type_mask, wedge tables av1_wedge_params_lookup (wedge_utils /
  reconinter.c static tables). Patched aomdec in /tmp/libaom-build (EC_TRACE2,
  EC_AV1_TELL post-label range prints); examples/inspect for per-block ground truth
  (IVF-wrap first: ffmpeg -i x.obu -c copy x.ivf). ffmpeg is the pixel oracle.

## Scope r1
1. Syntax (decodemv.c, after motion_mode on the compound path): when
   comp_group_idx==1 read compound_type (COMPOUND_TYPES symbol, compound_type_cdf
   over masked types wedge/diffwtd); wedge → wedge_index (16-ary wedge_idx_cdf[bsize])
   + wedge_sign (literal bit); diffwtd → mask_type (literal bit). Mind
   av1_is_wedge_used(bsize) — the type alphabet collapses when wedge is unusable.
2. Prediction (reconinter.c): build both refs' inter predictors, then blend with the
   64-scale mask: wedge masks from the wedge table (bsize-dependent master masks,
   flip by wedge_sign); DIFFWTD_38 / DIFFWTD_38_INV masks derived from the two
   predictions' per-pixel difference (clamp 38..64 shape — port
   build_compound_diffwtd_mask exactly). Chroma uses the subsampled mask
   (av1_get_compound_type_mask + subsampling path). Blend
   (mask*p0 + (64-mask)*p1 + 32) >> 6.
3. Grid/neighbour sweep (class neighbour-votes-all-its-fields + tx-class-cross-cutting):
   masked compound blocks are compound for every ctx gather — verify comp_group_idx
   neighbour ctx (`get_comp_group_idx_context`) reads our recorded bit, and that
   record_inter/record_compound_ctx call sites store comp_group_idx=1 correctly.
4. Gate: clone a_real_aomenc_stream_with_interintra_decodes_pixel_exact (stream.rs
   ~3480): new test a_real_aomenc_stream_with_masked_compound_decodes_pixel_exact,
   aomenc adds --enable-masked-comp=1 --enable-dist-wtd-comp=1 (keep interintra/warp
   /obmc flags as that gate has them); masked-compound refusals FORBIDDEN, other
   named refusals (screen content) skip the seed; add MASKED_COMPOUND_HITS atomic and
   assert > 0; EC_AV1_GATE_DUMP self-pin on mismatch. Hammer 8x.
5. Any pixel mismatch: pin to fixtures/ (mc-flake-N.obu), then: recon-dump diff
   (EC_AV1_PREFILT_DUMP both sides, frame files .fN, Y 4096 + U/V 1024 at 64x64) →
   range ladder (EC_AV1_TRACE part32_pre vs aomdec EC_PART rng; NOTE ours skips the
   key frame: ours[k]=aom[k+1]) → per-label TELL (EC_AV1_TELL both sides; aomdec
   prints post-adaptation, ours pre — offset by one read when comparing CDF dumps).
   Exact ladder ⇒ prediction-side; divergent ⇒ consumption.

## Done criteria
1. New masked-compound gate green with hits>0, hammered 8x clean (self-pin armed:
   EC_AV1_GATE_DUMP=/tmp/claude-1000/mc-flake-N.obu).
2. Pinned test extended with any new mc pins; all existing 11 pins still byte-exact.
3. `cargo test -p ec-av1 --release --lib` all green.
4. All work committed to lane-maskcomp (`feat(av1): ...` or `wip(av1): ...` on HANDOFF).
5. REPORT FILE lanes/maskcomp-r1.report.md, verdict FIRST line, evidence excerpts.

## Hard rules
If the full port cannot land verified, land syntax entropy-exact with the prediction
refused by name (wedge-mask blending unimplemented) — that is mergeable r1 progress.
Commit + report BEFORE you run low on budget. Never rm broad globs; never git checkout
files with other uncommitted work.
