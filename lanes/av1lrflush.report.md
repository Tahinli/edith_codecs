# lane-av1lrflush — the r2 "RU-row-0 flush" defect REFUTED; f2 residual re-attributed to inter-rect entropy recon; one new threading defect named (r3)

## Outcome

1. THE NAMED DEFECT DOES NOT EXIST. r2's DEFECT 3 (stop-with-site: "`pipe_run_lr`'s
   `rrows: Some` branch refills only caller-passed RU rows, and the
   `ls.note(k-2)`/`ls.close(nb, false)` band release never flushes RU row 0 when the
   top SB row carries a 128-root rect") is refuted by direct measurement at this HEAD
   (48411287, fixture sha256 `27825e14…61e80` verified): with the filter pipeline
   engaged (`EC_AV1_FILTER_THREADS=4`, recon serial — `pipe_on=true` on both inter
   frames, witnessed by the new unconditional `PIPELINE_TAKEN` counter), the decoded
   output is **byte-identical to the whole-frame path on all three frames**, including
   frame 2 — the inter frame whose top SB row carries the 128-root rect. The band
   release provably flushes RU row 0 (f2 luma is a single RU row `(0,160)`,
   unit_size 128, 2 SGR units `ep=11 xqd=[0,73]/[0,72]`; f1's SGR-heavy restoration —
   11754 changed samples — also reproduces exactly through the pipeline).
2. r2's stage ladder was misaligned by one stage. Measured at this HEAD with the
   pinned fixture and the current oracle (all dumps cross-checked frame-aligned; f0/f1
   byte-exact at every stage): frame 2 already diverges at **PREFILT** —
   Y 6952 / U 9568 / V 13496 samples — so POSTDEBLOCK/POSTCDEF are NOT byte-exact and
   the damage is introduced BEFORE loop restoration. r2's reported "FINAL" diff
   (Y 7308 / U 9685 / V 14012) matches today's POSTCDEF diff (Y 7307 / U 9685 /
   V 13722) almost sample-for-sample: the r2 comparisons paired our post-CDEF bytes
   against the oracle's FINAL and so on, one rung off. The "our final equals
   post-CDEF above row 63" reading is likewise stale — our LR actually changed rows
   from row 2 (Y) and covers all rows on V; U is `RestorationType::None` on f2 in
   BOTH decoders (oracle's U final == oracle's U post-CDEF, 0 rows differ), so the
   r2 parser reading (`lr_params` per-plane `f(2)`) is confirmed correct.
3. DEFECT A (real f2 residual — NOT this lane's): serial recon diverges at PREFILT
   with first diffs **Y (98,128), U (64,128), V (0,128)** — all three planes enter at
   column 128, i.e. inside the inter 64x128 block at (128,0), everything downstream
   carrying it. Site: the inter rect-strip entropy/recon path — the entropy lane's
   assigned territory (Levent4's `decode_rect4_16_strip` inter band stamping; their
   independent t6 finding — skipped inter 1:4 strips leaving stale band cells that
   flip coefficient contexts — is plausibly this class one size up). STOP per
   charter; nothing in the entropy path was touched here.
4. DEFECT B (NEW, needs its own lane): the wavefront reconstruction
   (`EC_AV1_RECON_THREADS>1`) diverges from the serial decoder at PREFILT on this
   fixture's inter frames — **chroma-only write-replay loss**: f1 U 17504 samples
   (first (1,0)), V 3 (29,67); f2 U 25472 (first (0,0)), V 1472 (first (128,2)); Y
   byte-exact on both frames, and f0 (keyframe) exact everywhere. Serial filters are
   unaffected (recon=2/filter=1 still diverges; recon=1/filter=4 is exact), so this
   is the wavefront write recording/replay machinery (lane-wave1 class), not the
   filter pipeline, not LR, not entropy. Never covered by any gate — every pixel gate
   decodes with default threads.
5. FIX: nothing to fix in the LR pipeline. What landed instead is the refutation
   pinned as a regression gate: `a_444_sb128_witness_lr_pipeline_flushes_ru_row_
   0_pixel_exact` — serial decode vs `override_filter_threads(4)` decode of the same
   pinned fixture, asserting (a) `PIPELINE_TAKEN` delta == 2 (the arm really
   pipelined both inter frames; robust against the once-per-process
   `filter_threads()` env cache that would otherwise silently serialise the arm),
   (b) `LR_STRIPE0_HITS` delta > 0 (a real filter ran on RU row 0's own first stripe
   through the pipeline), (c) pipelined == serial byte-exact on every frame, and
   (d) frames 0/1 byte-exact vs oracle aomdec AND ffmpeg THROUGH the pipeline.
   Mutation-verified: hard-disabling the RU-row-0 release in `pipe_run_lr`'s band
   walk turns the gate red at assert (c) ("pipelined frame 1 plane 0 differs",
   class lr-band-release); reverted, green.
6. Gate-extension status for the original acceptance item: extending
   `a_444_sb128_root_rect_stream_with_restoration_decodes_pixel_exact` to assert f2
   byte-exact vs aomdec/ffmpeg is **deferred** — f2 is not green and its fix belongs
   to the entropy lane (DEFECT A). Unblocks as soon as that fix lands in this
   branch's ancestry: change the two `take(2)` arms to `take(3)` in that gate. Its
   current scoping comment (naming the LR band release as the residual site) is now
   known-stale; left untouched here to keep the lane diff free of another lane's
   gate, to be corrected in the same deferred edit.

## Verification

- `cargo check -p ec-av1`: 0 warnings, 0 errors (target
  `$HOME/.cache/cargo-target-av1lrflush`; scoped `rustfmt --check` drift is
  repo-wide pre-existing, untouched files fail it identically).
- `stream::tests::a_444_sb128_witness_lr_pipeline_flushes_ru_row_0_pixel_exact`: PASS.
- `stream::tests::a_444_sb128_root_rect_stream_with_restoration_decodes_pixel_exact`:
  PASS (f0/f1 pins intact).
- Restoration battery (`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib
  restoration`): 10 passed, 0 failed — includes the 4 real-aomenc gates r2 ran
  (`…reads_lr_symbols_correctly`, `…superres_key_frame_with_cdef_and_loop_
  restoration…`, `…non_420_subsampled…`, `…skipped_8x8_intra_leaf…`) plus the
  10-bit-stripe and sibling LR gates.
- Mutation proof: `pipe_run_lr` band-walk mutated to never release RU row 0 → new
  gate FAILED at the pipeline==serial assert; reverted → PASS (red/green cycle in
  this lane, target dir reused, rebuild verified by the failure itself).
- Stage evidence produced with env-gated dumps on both decoders
  (`EC_AV1_PREFILT_DUMP`/`EC_AV1_POSTDEBLOCK_DUMP`/`EC_AV1_POSTCDEF_DUMP`/
  `EC_AV1_FINAL_DUMP`, per-frame strided slices, 192x160x3 per frame); all temp
  probes and the throwaway example removed before commit.

## Decision

- LR pipeline: EXONERATED, gate-pinned. No production code changed in
  restoration.rs; decode.rs gained only the unconditional `PIPELINE_TAKEN` counter +
  accessor (the non-vacuity witness; the wave-stats-gated `PIPE_FRAMES` is not
  readable by gates).
- DEFECT A: handed to the entropy lane with first-diff coordinates (see Outcome 3);
  cross-referenced with Levent4's t6 context-zeroing finding.
- DEFECT B: named for a wavefront lane — reproducible 100% (deterministic), chroma
  write replay under `EC_AV1_RECON_THREADS>1`; fix owner not this lane.
- NOT touched: entropy band stamping / `read_coeffs_rect` / `decode_rect4_16_strip`
  (sibling's), the 4:2:0 group-tail chroma SKIP arm (reserved), sub8 rect arm,
  leaf8 OOB, wavefront write-replay code.

## State

- Branch lane-av1-lrflush, commit on top of 48411287 (no push): the gate + the
  `PIPELINE_TAKEN` counter + this report.
