# lane-tile2 r2 HANDOFF (work COMMITTED, tree clean)

Tip `c1e5122` (`3d100df` = the fix). Full report: `lanes/tile2-r2.report.md`.

## Landed
`mvstack::tpl_sample_inside_tile` — libaom `is_inside` (mvref_common.h:68) as
`add_tpl_ref_mv` applies it (mvref_common.c:340), at all four temporal-sample
read sites (single-ref base loop + extension, compound base loop + extension).
`MiGrid::tile_bounds()`. Storage stays frame-wide (r1's `get_any`) — correct,
libaom's projection is frame-wide.
New instrument: `EC_TPL_SRC` dump of the STORED motion field under
`EC_TRACE_TPL` (decode.rs `build_motion_field`, `MotionField::debug_get`).

## Still RED (blocks the merge)
`stream::tests::a_frame_edge_straddling_band_decodes_pixel_exact`:
`192x68 cq35 frames=5 10bit=false tile_cols=1 frame 1 plane Y: 164 pixels
differ, first at row 59 col 146` — byte-identical to r1, the tile check is
stream-inert here. Do NOT re-charter "cross-tile temporal reads": disproven,
the sample lists now match aomdec line-for-line.

## r3's anchor (measured this round)
Stream `~/.cache/tile2-tmp/strad1.obu` (md5 12fb76d8910765657733604c4c70b77c,
`~/.cache/tile2-tmp/strad.sh 1` regenerates it; mi_cols=48, mi_rows=18).
- All 32x32 blocks of tile 1 (mi_row=8, mi_col=32/40) match aomdec in mode,
  refs, mv0 and stack size on all 5 decode-order frames (EC_TRACE_MODE).
- First divergence: mi_row=16 (bottom straddling mi row), mi_col=34/36 —
  ours (0,0)/(-1,-1), aomdec (-1,-1)/(0,0), SAME stack size 2 => candidate
  ORDER/weight, not a symbol. The 164 wrong luma px at rows 56..63 are that
  band's deblock/CDEF bleed upward.
- Stored field (EC_TPL_SRC) row 8 of the oh=1 frame, cols 16..23:
  ours (0,0),(0,0),(-1,-1)x6; aomdec reads back (0,0),(-1,-1),(0,0)x3,(0,20)x3.
NEXT: dump the full MV stack (EC_STACK) for mi_row=16 mi_col=34 and 36 on both
decoders and compare candidate order + weights. Suspects: the neighbour reach
clamp at the LAST mi row (mi_rows=18 with only 4 visible px of the 8-px band),
and the temporal candidate's weight-2 insertion there.

## Suite
`$HOME/.cache/tile2-suite-r2.log` (unit `tile2-suite-r2`, MemoryMax=10G),
started on `3d100df`; see the report / the log's own `test result:` line.
