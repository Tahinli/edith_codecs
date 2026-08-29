# lane-rect r2 report

VERDICT: PASS — HORZ/VERT decode for real; rect-flake-1/2/3 byte-exact; free gate un-clamped 6/6; 14-pin list green; lib 220/0.

## What landed (lane-rect2, on f9f9767)
- c5b21ad/faadb91 (builder): find_mv_stack bw4/bh4 split; grid.set stamps the true
  rect footprint; HORZ/VERT arms resurrected from 4782e57.
- 20f93eb: motion_mode/obmc CDFs gain BLOCK_16X32/32X16 rows (class
  cdf-row-held-constant).
- 64a20c7 (rect-flake-1, 4 root causes): MiInfo.size_h everywhere (col scans step
  and weight by it); find_samples/find_projection true dims (warp samples at the
  neighbour's real center); obmc_blend per-pass caps + overlap (above=write_h/2,
  left=write_w/2); tx_h_grid so deblock horizontal edges gate on tx HEIGHT
  (1-px chroma seam). HORZ_B top strip flipped to a true (BLOCK, SUB) 32x16.
- 6d5c75d (rect-flake-2): scan_row/scan_col weight `inc` must min by the
  candidate's CROSS axis (libaom mi_size_high in the row scan, mi_size_wide in
  the col scan) — all 4 sites (single + compound). Square candidates hide it; a
  32x16 strip above tied weights, reordering DRL entry 1 and suppressing the -5
  extended row scan. EVIDENCE: aom EC_DRL w0=688 w1=672 + EC_ROWSCAN
  row_offset=-5 weight=2 vs our tie 688/688; pin byte-exact after.
- 441003d (rect-flake-3): overlappable_left stepped the vertical walk by the
  neighbour's WIDTH; a 32x16 left strip swallowed the strip below it and rows
  48-63 blended strip1's OBMC prediction. EVIDENCE: recon diff exactly the
  second strip's band, ranges exact everywhere. Class sweep: all other grid
  walks step by the matching axis (mvstack 919/1577 + decode 4293/4404/4426 are
  horizontal, correctly width-stepped).

## Gates
- 14 pins green (warp-mismatch, warp-flake-5/7, ii-flake-1..3,5..9,
  rect-flake-1/2/3) — EVIDENCE: default pin test inside lib run.
- Free-partition gate un-clamped (--enable-rect-partitions=1
  --min-partition-size=16): 6/6 with EC_AV1_GATE_DUMP self-pin armed.
- Full lib: 220 passed / 0 failed.

## Residue
- HORZ_A/VERT_A/VERT_B/HORZ_4/VERT_4 still refuse by name (machinery is now
  rect-capable; next lane).
- rect-flake-2/3 fixture .bak copies + /tmp dumps are scratch, not repo.
