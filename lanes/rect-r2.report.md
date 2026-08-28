# lane-rect r2 report

VERDICT: PARTIAL -- HORZ/VERT arms resurrected and two real bugs fixed (mvstack
bw4/bh4 derivation + MiGrid stamp footprint), pin's mismatch region shrank
drastically but is NOT byte-exact yet; lib suite green (no regression).

## What landed
- `decode_inter_block`'s `bw4`/`bh4` locals (single-ref path ~5887 and compound
  path ~5225) now derive from `write_w`/`write_h` (the true rect footprint r1's
  plumbing already threads through) instead of the old `side`-square /
  `reject_residual ? bw4/2 : bw4` HORZ_B-only heuristic. This feeds
  `find_mv_stack_with_sign_bias`/`find_mv_stack_compound`'s row/col scan
  lengths, weights, DRL ctx and `clamp_mv_ref` with the block's real extent
  (mvstack.rs itself was ALREADY fully bw4/bh4-rect-capable -- scan_row uses
  bw4, scan_col uses bh4, correctly split; scope item 1 in the charter turned
  out to be a no-op, the defect was entirely in the caller).
- `overlappable_left`'s call site (motion_mode eligibility, ~6053) now passes
  `bh4` instead of `bw4` (libaom asymmetry: above-row scan uses width,
  left-col scan uses height).
- REAL found-and-fixed bug beyond the charter's scope-1 read: `grid.set` (the
  mvstack module's own `MiGrid`, distinct from `Neighbours`) in both the
  single-ref (~6199) and compound (~5298) decode paths stamped a square
  `bw4`x`bw4` footprint even for a rect block -- a HORZ strip's top half was
  claiming its own bottom strip's mi rows before that strip ever decoded,
  corrupting every subsequent mvstack scan that reads those cells back. Fixed
  to `0..bh4` (rows) x `0..bw4` (cols). This dominated the pin's actual
  mismatch, not the (already-correct) mvstack row/col split.
- `PARTITION_HORZ`/`PARTITION_VERT` arms resurrected from `4782e57` verbatim
  (side=BLOCK, reject_residual=true, write_w/write_h = 32/16 or 16/32),
  `RECT_PARTITION_HITS` counter wired.

## Gate state (NOT closed)
`pinned_warp_stream_decodes_pixel_exact` with
`EC_AV1_GATE_DUMP_PIN=fixtures/rect-flake-1.obu`: before this round's fix the
mismatch grid was dense from block row 12 on (most of the frame). After the
bw4/bh4 + grid-stamp fixes the mismatch region shrank to a handful of scattered
`o` cells starting at grid row 429 (of ~450), growing denser toward the frame
edge -- still FAILS (`assertion left == right failed: frame 16 luma vs ffmpeg`).
NOT localized further within budget: the remaining defect is downstream of the
two fixes above, likely one of the audited-but-unfixed items below (Done
criteria's ladder step (a) is not met).

Lib suite: 220/0, 17 ignored -- no regression from either fix.
Free-partition gate un-clamp (scope item 4c) and the 6x hammer sweep were NOT
attempted (budget). HORZ_B top-strip sweep (scope item 5) NOT attempted.

## Audited but left unfixed (scope items 2/3, budget)
- `find_samples`/`num_proj_ref` (warp sample gathering) still take a single
  `bw4` param and use it for BOTH the above-row AND left-column scans/limits
  (~4344-4462) -- a real bug for a rect block's warp path, unconfirmed against
  this pin (strip 1 in the pin is WARPED_CAUSAL and already matched aomdec
  byte-exact before AND after this round's fixes, so it's not implicated
  here, but it is real for any rect block whose warp-eligible strip has
  neighbours needing the true bh4).
- The OBMC blend build function (~4670, distinct from the eligibility check
  fixed above) also takes one `bw4` param used for both `overlappable_above`
  and `overlappable_left` (~4697/4718) -- same class, unfixed, unconfirmed
  against this pin (pin's blocks are WARPED/NEARMV skip, no OBMC selected).
- CDF size-class indexing (`bsize_idx = side.trailing_zeros()-3`,
  `cdfs.motion_mode`/`cdfs.obmc` only 4 rows for square 8/16/32/64) was NOT
  audited against libaom's real per-BLOCK_SIZE indexed tables (scope item 3).
  Circumstantial evidence it is NOT the cause of the remaining mismatch: the
  pin's strip 1 (WARPED_CAUSAL, `bsize_idx=2` for side=32) already decodes
  byte-exact before AND after this round, at the SAME `side`-derived index a
  true 32x16 block would also hit under the current corner-cut.
- `MiInfo::size` is a single scalar (can't record a neighbour's true
  width/height separately) -- structural limitation of `mvstack::MiGrid`,
  noted but not touched.

## Class
decision-at-wrong-granularity, but the caller-side instance (grid stamp
footprint), not the table-derivation instance the charter's scope-1 named
(mvstack.rs itself was already correct).

## Next round seed
1. Localize the remaining mismatch (grid row ~429+): recon dump + range
   ladder against aomdec `EC_TRACE`, starting from the first diverging block
   near that region (frame 16, likely a later strip or an HORZ_B/other
   partition in the same frame -- the pin decodes multiple partition types).
2. Once localized, check whether it lands in find_samples/OBMC-blend/CDF-class
   (the three unaudited items above) or is a fourth defect.
3. Un-clamp the free-partition gate (scope 4c) and hammer once the pin is
   green; sweep HORZ_B's top strip (scope 5).

## Evidence
- Compiles clean (`cargo build -p ec-av1 --release`).
- `cargo test -p ec-av1 --release --lib`: 220 passed, 0 failed, 17 ignored.
- `EC_AV1_GATE_DUMP_PIN=fixtures/rect-flake-1.obu cargo test ... pinned_warp_stream_decodes_pixel_exact -- --ignored --nocapture`:
  still FAILS (frame 16 luma), mismatch region reduced from dense (rows 12+)
  to sparse (rows 429+) by this round's two fixes.
- Committed `c5b21ad` on `lane-rect2`.

/tmp usage at report time: not re-checked this round (budget); no scratch
builds written under /tmp, CARGO_TARGET_DIR private per charter.
