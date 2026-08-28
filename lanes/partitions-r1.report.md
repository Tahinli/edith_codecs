# lane-partitions r1 report

VERDICT: PARTIAL — rect write-footprint plumbing landed and a REAL defect in the
square-context strip approach was found, pinned, and refused by name; pixel-exact
HORZ/VERT decode moves to r2 with the pin as its gate.

## What landed
- `decode_inter_block` gained `write_w`/`write_h`: `side` still drives syntax, the
  pixel write and every neighbour/grid stamp clip to the true footprint. New `_rect`
  siblings on `Neighbours`/`PlaneBuf` (`record_inter_rect`, `fill_lf_grid_rect`,
  `fill_skip_grid_rect`, `record_compound_ctx_rect`, `record_rect`, `record_mi_rect`,
  `reconstruct_mc_rect`); all square callers delegate — zero behavior change on
  existing paths (full lib suite 220/0, all 11 pins byte-exact).
- `RECT_PARTITION_HITS` counter + `rect_partition_hits()`.
- HORZ/VERT arms exist but REFUSE by name: "rect partition (32x16/16x32 strips need
  rectangular mvstack/context derivation, r2)". The full strip-decode implementation
  is in git history (commit 4782e57) for r2 to resurrect.

## The defect (why the arms refuse)
The builder's strip decode ran each 32x16 strip with `side=32` square contexts
(HORZ_B's accepted corner-cut). The free-recipe gate immediately found mismatches
(4/6 runs); pin `fixtures/rect-flake-1.obu`, decode frame 16, block (0,8) = HORZ:
- strip 1 (mi 0,8): decodes EXACTLY (NEWMV (0,-16), WARPED — matches aomdec).
- strip 2 (mi 4,8): aomdec NEARMV mv=(0,0); ours picked mv=(0,-16) — the DRL/stack
  differ because a true 32x16 block's mvstack scan/weights use n4_h=4, not 8.
  Range ladder diverges inside the strip (consumption via drl/ctx), recon wrong in
  3 quadrants from f16 on.
Class: decision-at-wrong-granularity. COROLLARY: HORZ_B's top strip carries the SAME
corner-cut; its pins pass only because those stacks coincide — r2's rectangular
context threading must cover HORZ_B too (noted in the arm comment).

## Gate state
`a_real_aomenc_stream_with_free_partitions_decodes_pixel_exact` re-clamped to the
rectgate recipe (rect=0, ab=1, min=32) — green 3/3 (29–33 matches/40). The free
recipe (rect=1, min=16) that found the pin is documented in the gate comment; r2
un-clamps it once rectangular contexts land.

## Evidence
- rect-flake-1.obu pinned to fixtures; repro deterministic (frame 16 luma).
- Localization: recon diff f16 quads [0,466,996,860]; range ladder exact through
  decode frame 15, diverges in frame 16 block (0,8) strip 2; per-block trace diff
  (aom EC_TRACE vs ours) shows the NEARMV mv disagreement above.
- lib suite 220/0; pins 11/11; re-clamped gate 3/3.

## r2 charter seed
Thread true bw4/bh4 through find_mv_stack (row/col scan lengths + weights), DRL ctx,
is_inter/ref/skip ctx gathers, warp num_proj_ref/samples, obmc overlap — libaom uses
xd->width/height everywhere `side` is square here. Gate = rect-flake-1 pin first,
then the free recipe with HORZ/VERT refusals forbidden. Sweep HORZ_B's strip in the
same round (same class, latent).

Orchestrator note: builder hit its turn cap twice; WIP committed by orchestrator
(4782e57), gate reactivation + test-cfg compile fix + defect hunt + refusal revert
done inline. Verification: orchestrator-run gates/suites above (cross-provider seats
still down).
