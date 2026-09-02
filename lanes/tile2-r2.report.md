# lane-tile2 r2 — tile-bounded temporal MV reads

## Verdict: RED (the chartered fix landed and is proven correct, but it does NOT
## close the regression it was chartered to close). Branch is NOT mergeable.

Tip `3d100df` on `lane-tile2` (parent `1859cac`).

## What changed
- `crates/ec-av1/src/mvstack.rs` — new `tpl_sample_inside_tile()` (right above
  `add_candidate`) = libaom `is_inside` (`mvref_common.h:68`) applied to
  `add_tpl_ref_mv`'s own odd/even-adjusted `mi_pos` (`mvref_common.c:336-340`).
  Called at all FOUR temporal-sample read sites: the single-ref base grid loop,
  the single-ref `tpl_sample_pos` extension, and the compound twins of both.
  `MiGrid::tile_bounds()` added next to `tile_origin()` (clamped to the grid).
- Frame-wide STORAGE from r1 is unchanged and correct: `av1_setup_motion_field` /
  `motion_field_projection` write `tpl_mvs` for the whole frame and know nothing
  about tiles; the tile restriction lives only at READ time (`add_tpl_ref_mv`).
- `crates/ec-av1/src/decode.rs` + `motion_field.rs` — env-gated `EC_TPL_SRC` dump
  of the STORED motion field (values + ref_frame per 8x8 cell) under
  `EC_TRACE_TPL`, plus `MotionField::debug_get`. This is the instrument that
  localised the residual defect below.

## The fix is proven correct, and it is NOT the regression's cause
Pinned stream `~/.cache/tile2-tmp/strad1.obu` (192x68, cq35, 8-bit,
`--tile-columns=1`, the exact gate recipe; regenerate with
`~/.cache/tile2-tmp/strad.sh 1`), md5 `12fb76d8910765657733604c4c70b77c`,
hashed twice.

EVIDENCE: ~/.cache/tile2-tmp/{o.tpl,a.tpl} | EC_TRACE_TPL on ours (decode_probe) and on the instrumented oracle aomdec, same stream | the SET of temporal samples probed is now line-for-line identical for every block (ours emits each compound sample twice, once per side; after collapsing that, 1102 vs 1105 lines and the only remaining differences are VALUES, never positions). Before the fix we probed extension samples at mi_row=16 (the bottom straddling mi row, mi_rows=18) that libaom's is_inside rejects.
EVIDENCE: $HOME/.cache/tile2-r2-strad.log | single test, systemd unit | `192x68 cq35 frames=5 10bit=false tile_cols=1 frame 1 plane Y: 164 pixels differ, first at row 59 col 146` — byte-identical to r1's failure, i.e. the tile-bounded read changes NOTHING on this stream.

So the charter's premise ("our temporal-MV consumer reads cross-tile samples
that libaom rejects") is DISPROVEN for this arm: no cross-tile temporal sample
is geometrically reachable here (the only left-reaching sample is `-2` mi and
`check_sb_border` keeps it inside the block's own 64x64 superblock). The fix is
kept because it is libaom-cited conformance and it does fire (it is what makes
the sample lists match at the bottom mi row) — but it is stream-inert here.

## Where the regression actually is (measured, r3's anchor)
The first cross-decoder divergence on this stream is a decoded MV in the BOTTOM
STRADDLING mi row of tile column 1, not a filter and not the entropy stream:

EVIDENCE: ~/.cache/tile2-tmp/{ours.mode,aom.mode} | EC_TRACE_MODE both decoders | every 32x32 block of tile 1 (mi_row=8, mi_col=32/40) matches in mode, refs, mv0 and stack size on ALL 5 decode-order frames; at mi_row=16 the third traced frame has ours mi_col=34 mv0=(0,0) / mi_col=36 mv0=(-1,-1) where aomdec has mi_col=34 mv0=(-1,-1) / mi_col=36 mv0=(0,0), same stack size 2 -> a stack ORDER/weight difference, not a symbol difference.
EVIDENCE: ~/.cache/tile2-tmp/ours2.err (EC_TPL_SRC) vs a.tpl | stored motion field row 8 (mi rows 16-17) of the oh=1 frame | ours cols16..23 = (0,0),(0,0),(-1,-1)x6; the values aomdec reads back there are (0,0)@16, (-1,-1)@17, (0,0)@18-20, (0,20)@21-23. Our own row 7 is (0,0)@16-19,(0,20)@20-23, i.e. libaom's row 8 looks like our row 7 shifted one 8x8 column right.

The 164 wrong luma pixels are at row 59 (rows 56..63), i.e. the deblock/CDEF
bleed ABOVE the 4-px straddling row band 64..67 — consistent with the bottom-row
blocks (mi_row=16) being predicted from the wrong MV.

## Open residue
- fix-now(next round): the straddling-band regression. Anchor: mi_row=16,
  mi_col=34/36, tile column 1, on `~/.cache/tile2-tmp/strad1.obu`; dump the full
  MV stack (`EC_STACK`) for those two blocks in both decoders and compare the
  candidate ORDER and weights — mode/refs/stack-size already match, so the
  difference is which candidate NEARESTMV/NEARMV lands on. Both the neighbour
  reach clamps at the frame's last mi row (mi_rows=18 with only 4 visible px)
  and the temporal candidate's weight are live suspects.
- deferred(unblocked by the above): the `#[ignore]`d var-tx multi-tile arm
  (frame 3, U plane, 16 bytes, ±1) — not touched this round.
