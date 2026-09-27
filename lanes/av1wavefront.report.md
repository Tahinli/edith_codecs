# lane-av1wavefront — defect B fixed: the wavefront replay read frame state off a default-seeded `FrameCtx`; 4:4:4 chroma lost, 4:2:0 accidentally exact

## Outcome

1. DEFECT B (lanes/av1lrflush.report.md) reproduced and fixed at the source.
   `EC_AV1_RECON_THREADS>1` diverged from serial at PREFILT, chroma-only, on
   `444_sb128rect_lr_witness.obu` (sha256 `27825e14…61e80`, verified): f1 U
   17504 samples (first (0,1)) / V 3 (first (67,29)), f2 U 25472 (first (0,0))
   / V 1472 (first (2,128)), Y byte-exact, f0 exact — the report's own numbers,
   re-measured off `EC_AV1_PREFILT_DUMP` runs at recon=2/filter=1 vs
   recon=1/filter=1.
2. ROOT CAUSE — not a stride/plane-index bug in the write records, and not a
   lost op: [`wave_worker`] seeds each recon worker with a FRESH
   [`FrameCtx`] (`FrameCtx::new()`), carrying only `bit_depth` and
   `enable_edge_filter`. Every replay-side read of the frame's chroma
   subsampling therefore saw the CONSTRUCTOR DEFAULT `subsampling = (1, 1)`
   (4:2:0) regardless of the sequence. On a 4:4:4 stream the deferred chroma
   prediction builds read `ss_x(fctx)` at CLOSURE-RUN time —
   `mv_to_q4(px, mv.1, ss_x(fctx))` in the sub-8x8 group's `build_c` (and the
   same shape in the 4x4-piece and leaf-8 chroma builds) — so every worker
   fetched its chroma reference at HALF the true position: the prediction was
   wrong, the recorded op replayed faithfully at the wrong content. `exec_intra`'s
   CfL AC read (`cfl_ac_ss(.., ss_x(fctx), ss_y(fctx))`) is the same class one
   reader over. Serial is exact because there the closure runs at push time on
   the parse thread's real context. Every earlier wavefront gate stream is
   4:2:0, where the default happens to BE the frame's pair — the defect was
   invisible to the whole existing recon-threads gate set.
3. FIX: `WaveState` carries `subsampling: (u8, u8)`; `WaveGuard::install`
   copies it from the tile's `FrameCtx` next to `bit_depth`;
   `wave_worker` seeds it into the worker context. All replay-side ss readers
   (the three `build_c` closure shapes and `exec_intra`'s CfL) are fixed by
   the one field; no recorded op changed shape, no push-site read moved.
4. Verified:
   - recon=2/filter=1 and recon=4/filter=1 now byte-identical to serial on
     all three frames of the fixture (`dump_yuv` output compare; the example
     binary rebuilt from this tree).
   - Serial byte-identical pre/post fix (same compare against the pre-fix
     binary's serial run) — the inline path never constructs a `WaveState`.
   - Gate: `444_sb128rect_lr_witness.obu` added to
     `a_real_stream_reconstructs_identically_with_one_and_four_recon_threads`
     (the 1-vs-4 recon-threads byte-exact gate, per-frame Y/U/V asserts).
     Mutation-verified: commenting the worker's `subsampling_x/y.set` seed
     makes the gate FAIL exactly on the new fixture ("frame 1 U differs at 4
     recon threads") while the three 4:2:0 fixtures still pass — the gate is
     non-vacuous and discriminates the class. Fix restored, gate green.
   - `cargo check -p ec-av1`: 0 warnings.
5. CLASS SWEEP (replay-side `FrameCtx` reads on a worker): enumerated every
   fctx read reachable from `exec_op` → `exec_intra`/`exec_mc`/PredBuild
   closures: `sample_max` (bit_depth — carried), `enable_edge_filter`
   (carried), `subsampling` (this fix), and nothing else —
   `reconstruct_mc_rect` reads only `sample_max`; `mc::predict`'s
   `interp_filter` read is not in any replay path (the deferred builds pass
   the explicitly parsed kernels to `predict_maybe_scaled`; the two
   `mc::predict` callers are `#[cfg(test)]` gate code). Parse-side ss reads
   run on the parse thread's real context and were never wrong.

## Not touched

- Entropy band stamping / `read_coeffs_rect` / `decode_rect4_16_strip`
  (Levent4's defect A territory) — nothing here lands in entropy code.
- Osman4's LR pipeline (proven clean; recon=1/filter=4 stays exact).
- The 4:2:0 group-tail chroma SKIP arm (reserved to its owning lane).

## State

Committed on lane-av1-wavefront. No push.
