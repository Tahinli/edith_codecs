# lane-interintra r1 report

VERDICT: PASS — non-wedge interintra decodes pixel-exact; two defects found and fixed en route.

## What landed (commit 277b9a0 on lane-interintra)
1. **Interintra syntax + prediction** (charter scope): interintra flag →
   interintra_mode (4-ary, size-group CDF) → wedge flag (bsize-row CDF;
   wedge==1 remains a NAMED refusal for r2). Non-wedge blend
   `(mask*intra + (64-mask)*inter + 32) >> 6` with ii_weights1d
   (II_DC=const 32, II_V/H/SMOOTH from the 1-D falloff, scale 128/side),
   all planes, on both the 16/32 single-ref path and the 8x8 leaf.
   motion_mode is not read for interintra blocks. New CDFs joined
   cdf.rs/cdf_state.rs including reset_counts and per-frame save/restore.
2. **Defect 1 (ii-flake-1..8): interintra neighbours donated warp samples.**
   libaom `av1_findSamples` (mvref_common.c:1155) requires
   `ref_frame[1] == NONE_FRAME`; an interintra neighbour carries
   ref1=INTRA_FRAME and is excluded. Our mi grid recorded it as plain
   single-ref → extra warp sample → invalid projection → translation
   fallback → pixel drift on the next WARPED_CAUSAL block. Fix: grid
   records `ref_frame1 = Some(0)` for interintra blocks (both grid.set
   sites); `find_samples`' existing `ref_frame1.is_none()` filter then
   matches libaom, and `num_proj_ref` (same function) with it. The
   mvstack compound ref-diff scan gained a `rf1 > 0` guard mirroring
   libaom's `can_rf > INTRA_FRAME`; all other ref_frame1 consumers compare
   against real refs 1..7 or require mv1 and are inert to the marker.
3. **Defect 2 (ii-flake-9, NOT interintra): `switchable_interp` missing
   from `Cdfs::reset_counts`.** libaom zeroes every CDF counter when the
   adapted context is saved at frame end (`av1_reset_cdf_symbol_counters`,
   decodeframe.c:5588). Ours never reset this one table's counters, so
   after >15 accumulated reads the update rate slowed to >>5 while aom
   stayed at >>4; the rows drifted a few counts apart and the filter read
   flaked only on read-dense streams. Localization: range ladder exact
   into the block, post_motion_mode range equal, post_interp_filter range
   divergent; per-read CDF dumps showed equal values but step +9 (ours) vs
   +16 (aom) on ctx 8. Field-list audit (script vs struct): no other Cdfs
   field is missing from reset_counts; MvComponentCdfs fully covered.

## Evidence
- Pinned streams: 11/11 byte-exact in one run (`pinned_warp_stream_decodes_pixel_exact`,
  fixtures warp-mismatch, warp-flake-5/7, ii-flake-1,2,3,5,6,7,8,9).
- Interintra gate `a_real_aomenc_stream_with_interintra_decodes_pixel_exact`:
  12/12 hammer clean post-fix (23–33 exact of 40 seeds, rest =
  screen-content refusals; interintra_hits 11–22 per run; before the
  fixes: 7/8 runs failed).
- Warp gate 3/3 post-fix (counter reset touches all switchable-filter streams).
- Full `cargo test -p ec-av1 --release --lib`: 218 passed, 0 failed.

## Verification note
Cross-provider seats exhausted/stalling (opencode-go monthly, kimi-code 5h,
zai flash timeouts); orchestrator verified source-level against libaom C
directly (decodemv.c read_interintra_mode_info, reconinter.c
combine_interintra, mvref_common.c av1_findSamples,
entropy.c av1_reset_cdf_symbol_counters) — same fallback as warp close,
documented per standing rule.

## Classes
- Defect 1 = neighbour-votes-all-its-fields + symbol-consumption-gap's
  prediction-side kin (range ladder exact ⇒ prediction defect).
- Defect 2 = NEW CLASS: adaptation-state half of the CDF contract —
  values right, counter wrong; presents as a late, content-dependent
  rotating flake exactly like a consumption gap but with an exact range
  ladder up to the divergent read. Memory written: cdf-counter-not-reset.
