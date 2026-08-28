# lane-interintra r1 charter — implement non-wedge interintra prediction (decode)

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-interintra, branch lane-interintra @ 8f90552.
Build/test ONLY: `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-interintra cargo test -p ec-av1 --release --lib <name> -- --nocapture`
Never plain workspace cargo test; never touch other worktrees; never push; fixtures/ is a symlink — never write into it except NEW pinned .obu streams captured by a gate.

## Current state (do not re-derive)
- The interintra SYMBOL is consumed and interintra==1 is a NAMED refusal at
  crates/ec-av1/src/decode.rs:5645-5654 (single-ref path, side 16/32, gated
  enable_interintra_compound && !skip_mode). This round replaces the refusal with the port.
- Warp/OBMC/skip_mode/interp-suppression all landed and merged — leave them alone.
- Reference: libaom at /tmp/libaom-src (decodemv.c read_interintra_mode_info ~1100,
  reconinter.c av1_build_interintra_predictor / combine_interintra,
  reconintra.c av1_build_intra_predictors_for_interintra). Patched aomdec (EC_TRACE2 prints)
  builds in /tmp/libaom-build; libaom examples/inspect exists there for ground truth
  (wrap OBU in IVF first: ffmpeg -i x.obu -c copy x.ivf). ffmpeg is the pixel oracle.

## Scope, r1 = NON-WEDGE interintra only
1. Syntax (decodemv.c read_interintra_mode_info): after interintra==1 read
   interintra_mode (4-ary, interintra_mode_cdf[size_group]); if seq
   enable_interintra_wedge && bsize in wedge-allowed set: read wedge_interintra flag —
   if THAT reads 1, keep a NAMED refusal ("wedge interintra unimplemented") for r2;
   wedge flag 0 → non-wedge path below. CDFs: interintra_mode + wedge_interintra flags
   exist in libaom's default tables — add to cdf.rs/cdf_state.rs mirroring how the
   interintra flag CDF was added (grep cdfs.interintra for the pattern; remember CDF
   adaptation: new tables join the adapt map the same way).
2. Semantics: ref_frame[1] = INTRA_FRAME. CRITICAL cross-cutting sweep (class
   tx-class-cross-cutting + neighbour-votes-all-its-fields): interintra blocks are
   single-ref for mvstack purposes but their neighbour RECORD carries ref1=INTRA_FRAME
   in libaom — grep every context gathered from ref1/compound-ness (comp_inter ctx,
   comp_ref ctx, filter ctx ref-match) and match libaom's read of an interintra
   neighbour. motion_mode is NOT read for interintra blocks (ref_frame[1]==INTRA_FRAME
   fails is_motion_variation_allowed) — the current code reads motion_mode after the
   refusal site; make interintra==1 skip that read exactly as libaom does.
3. Prediction (reconinter.c): build the intra predictor for the block's interintra mode
   (II_DC/II_V/II_H/II_SMOOTH map to DC/V/H/SMOOTH intra preds with
   use_filter_intra=0, from the block's own above/left recon like a normal intra block),
   then combine_interintra: weighted blend of inter and intra preds with the
   ii_weights_1d table (mode-dependent direction), all planes (chroma at subsampled
   size; interintra applies to chroma too — check av1_build_interintra_predictor plane loop).
4. Gate: clone the warp gate shape (stream.rs a_real_aomenc_stream_with_warped_motion_
   refuses_or_matches, ~3311): new test a_real_aomenc_stream_with_interintra_decodes_
   pixel_exact with aomenc flags from that gate BUT --enable-interintra-comp=1
   --enable-interintra-wedge=0 --enable-smooth-interintra=1, warp/obmc ON (they are
   supported now); non-wedge interintra refusals FORBIDDEN, wedge refusal allowed
   (should not fire with wedge=0), EC_AV1_GATE_DUMP self-pin on mismatch, and assert the
   interintra counter fired (add an INTERINTRA_HITS atomic like WARP_SELECTED_HITS).
5. Any pixel mismatch: pin the stream (cp into fixtures/), diff with
   /tmp/libaom-build/examples/inspect + patched aomdec EC_TRACE2, range-ladder first
   (TRACE part32_pre rng= exists under EC_AV1_TRACE; aomdec prints EC_PART rng=):
   ranges exact everywhere = prediction-side; divergent = consumption. Fix, rerun.

## Done criteria
1. New interintra gate green with interintra blocks actually fired (counter > 0), run 3x.
2. Existing suite: `cargo test -p ec-av1 --release --lib` 217+ passed, 0 failed.
3. All work committed to lane-interintra (`wip(av1): interintra r1 ...`), even on HANDOFF.
4. REPORT FILE lanes/interintra-r1.report.md, verdict FIRST line, evidence excerpts.

## Hard rules
<=60 tool calls. AT CALL 45: commit whatever compiles + write the report as HANDOFF.
Charter-sized: if the port cannot fit, land syntax+semantics (entropy-exact, prediction
still refused by name) as r1 and hand prediction to r2 — an entropy-exact refusal is
mergeable progress; a half-blended prediction is not.
