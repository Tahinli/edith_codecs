# lane-wii report (r1 recon HANDOFF + r2 flash build; orchestrator-verified and finished)

VERDICT: PASS — wedge-interintra decodes for real; gate fires it live every run (wii_hits 2-7, hard-asserted); 14 pins green; lib 221/0.

## What landed
- 84a8fa7 (flash r2 build, orchestrator-committed at its cap): both refusals
  (16/32 path + 8x8 leaf) replaced. Symbol stream: after use_wedge_interintra
  == 1, ONE adapting symbol `wedge_idx[bsize]` (rows 6/9/3 = 16x16/32x32/8x8),
  NO sign bit — sign fixed 0 (libaom blockd.h INTERINTRA_WEDGE_SIGN). Blend:
  wedge codebook mask replaces the smooth interintra mask in interintra_blend
  ((m*intra + (64-m)*inter + 32)>>6, intra weighted); chroma box-averages the
  luma-stride mask 2x2 ((m00+m01+m10+m11+2)>>2), exactly blend_a64_mask's
  subw==subh==1 path. WII_HITS atomic.
- Gate a_real_aomenc_stream_with_interintra_wedge_decodes_pixel_exact:
  gradients/mandelbrot alternating content, --enable-interintra-wedge=1,
  masked-comp off.

## Orchestrator fixes on top of the flash build
- Gate hang: builder's `mandelbrot=...end_pts=24` does NOT bound the source
  (300+ frames generated; 1h CPU, pipe deadlock, two hung orphan processes).
  Bound with `-t` like every landed mandelbrot recipe. Class instance:
  gate's own fixture generation unbounded (kin to gate-loader-slurps-whole-file).
- Verifier finding (opus, claim 5 REFUTED): the gate's forbidden-refusal
  strings were vacuous (no "wedge"/"interintra" refusal exists any more) and
  wii_hits was only a soft note. Vacuous asserts replaced with a comment;
  wii_hits==0 hardened to a HARD assert after 6/6 hammer runs each fired 2-7
  hits.

## Verification
- Cross-provider: opus source-level verification vs libaom — claims 1-4, 6
  CONFIRMED (symbol order decodemv.c:1549-1554; sign 0 blockd.h:40 — the r1
  charter's SIGN=1 came from an ENCODER search heuristic, compound_type.c:691;
  blend blend.h:23-28 + blend_a64_mask.c:251-259; INTERINTRA_HITS move weakens
  no gate since the interintra gate pins wedge=0). Claim 5 (gate) refuted and
  fixed above.
- Orchestrator re-ran: hardened gate green (wii_hits=3), 6 prior hammer runs
  green (hits 6,7,2,6,5,3), 14-pin list green, lib 221/0 (gate excluded run)
  + gate separately.

## Residue
- 25-29/40 gate attempts refuse on non-IDENTITY single-ref GLOBAL MOTION
  (mandelbrot pan) — a real, now well-measured capability gap; queue.
- r1 flash round burned its whole cap on recon (zero edits); r2 charter
  carried the handoff + two corrections (SIGN=1 error, literal(4) error).
