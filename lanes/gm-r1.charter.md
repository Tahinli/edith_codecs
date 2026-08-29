# lane-gm r1 charter — non-IDENTITY global motion decodes (GLOBALMV + global warp MC)

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-gm, branch lane-gm @ 2143b92.
Build/test ONLY: `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-gm cargo test -p ec-av1 --release --lib <name> -- --nocapture`
FOREGROUND, `nice -n 19`, `CARGO_BUILD_JOBS=4`. Never push; never touch other worktrees; fixtures/ is a symlink.
BUDGET ~60 calls: WIP COMMIT after every green milestone; commit compiling state + report by call 45. Recon is pre-done below — do not re-derive it.

## Measured motivation
The wedge-interintra gate refuses 25-29/40 attempts with "global motion ...
not IDENTITY" — mandelbrot's pan/zoom makes aomenc code ROTZOOM/TRANSLATION
global motion. Killing this refusal converts those attempts to decodes.

## Recon already done (anchors, do not re-search)
- Frame-header params ALREADY PARSED: ec-av1-syntax frame.rs
  read_global_motion_params (~1709) fills header.global_motion[7] with
  WarpParams { model: WarpModel, params/wmmat }. Read its struct shape first.
- The whole-frame refusal to remove: crates/ec-av1/src/stream.rs ~204-218
  (`any(|gm| gm.model != WarpModel::Identity)`).
- Single-ref GLOBALMV site: decode.rs ~6041-6070 (`is_globalmv = !not_zero`;
  the GLOBALMV arm currently assigns mv (0,0)).
- Compound GLOBAL_GLOBALMV: decode.rs ~5378 (`compound_mode == 6`), also
  assigns zeros today.
- mvstack pad/default candidates: crates/ec-av1/src/mvstack.rs — the stack
  pads short lists with (0,0) entries; libaom pads with
  gm_get_motion_vector results for the block's ref (setup_ref_mv_list's
  gm_mv_candidates). grep `0, 0` pad sites + the module doc.
- Our warp machinery (from WARPED_CAUSAL): crates/ec-av1/src/warp.rs
  `warp_affine` (~329) + shear derivation. Global warp MC reuses it with the
  GLOBAL wmmat instead of the locally-derived one.
- Interp-filter read suppression: decode.rs ~6166-6200 comments
  (`is_nontrans_global_motion`, warp-verify caveat: current suppression
  assumes IDENTITY globals). libaom av1_is_interp_needed
  (av1/common/reconinter.c) returns false when the block uses a
  NON-TRANSLATION global model spanning >=8x8 — with non-IDENTITY globals
  live this read-suppression condition MUST be ported exactly or every
  GLOBALMV block bit-drifts.

## libaom ground truth (read these, transcribe exactly)
- /tmp/libaom-src/av1/common/mv.h:231 gm_get_motion_vector: block-center
  derivation, TRANSLATION/ROTZOOM/AFFINE cases, allow_hp/force_integer
  rounding (is_integer), the >>3 / OFFSET semantics. This feeds BOTH
  assign_mv and the mvstack gm candidates.
- /tmp/libaom-src/av1/common/reconinter.c av1_allow_warp + the
  global-warp branch of av1_build_inter_predictor / warp path selection
  (when is_global fires warp_plane vs plain MC with the derived mv):
  block >=8x8, no scaling here.
- /tmp/libaom-src/av1/common/warped_motion.c av1_get_shear_params — global
  params must pass shear validation or fall back (av1_warp checks
  wm->invalid? read how decode handles invalid global params).

## Scope (in order, WIP commit each)
1. Port gm_get_motion_vector into decode (or warp.rs) with unit tests
   pinning a few (wmmat, center, hp/int) -> mv values TRANSCRIBED from a
   hand-run of the C formula (show the arithmetic in the test comments).
2. Thread header.global_motion into decode_inter_block/decode_inter_block8:
   GLOBALMV single-ref + GLOBAL_GLOBALMV compound assign the derived mv;
   mvstack gm candidates (pass the gm params into find_mv_stack pad sites
   per libaom setup_ref_mv_list).
3. MC: blocks whose mode is GLOBALMV (or whose selected mv comes from a
   non-translation global model per av1_allow_warp) go through warp_affine
   with the global wmmat + shear params; TRANSLATION model = plain MC with
   the derived mv. Port av1_is_interp_needed's global condition at the
   interp-filter read.
4. Remove the stream.rs whole-frame refusal.
5. GATE LADDER: (a) 14-pin default list; (b) wedge-interintra gate
   a_real_aomenc_stream_with_interintra_wedge (its 25-29/40 global-motion
   refusals must DROP — print before/after refusal counts in the report;
   any new mismatch = EC_AV1_GATE_DUMP self-pin /tmp/claude-1000/gm-flake-N.obu,
   report the pin + recon-diff location, do NOT guess-fix);
   (c) free-partition + AB gates; (d) full lib suite EXCEPT run the wedge
   gate separately (it is slow): `-- --skip a_real_aomenc_stream_with_interintra_wedge`.
6. If mismatches appear (likely — this is a value-heavy port): pin, localize
   with EC_AV1_PREFILT_DUMP both sides + the EC_AV1_TELL ladder, fix the
   earliest divergence only, iterate. Landing with SOME streams still
   mismatching = report the pins and leave the stream.rs refusal for exactly
   the model kinds still broken (e.g. keep refusing AFFINE, decode
   TRANSLATION/ROTZOOM) rather than shipping a wrong decode.

## Done criteria
Refusal narrowed or gone with pixel-exact evidence; gates green; wip commits;
REPORT lanes/gm-r1.report.md, verdict FIRST line, refusal-count before/after.
