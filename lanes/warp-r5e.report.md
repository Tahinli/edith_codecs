# lane-warp r5e report — PASS (run by orchestrator inline after two builder task kills)

PASS: pinned_warp_stream_decodes_pixel_exact 24/24 pixel-exact vs ffmpeg, green twice;
ec-av1 lib suite 217 passed / 0 failed. Fix committed f231bab.

## Root cause (the whole r3→r5e chain)
libaom's `av1_is_interp_needed` DERIVES the interp filter (Regular) instead of reading
switchable_interp symbols when `motion_mode == WARPED_CAUSAL` — keyed on the symbol value,
not projection validity. Our single-ref path suppressed the read only for GLOBALMV
(decode.rs resolve_interp_filter call), so a warped block consumed 1–2 extra symbols.

Where it bit: the hidden no-show altref (decode-order 13, display 23) is four 32x32 blocks —
NEWMV SIMPLE / NEARMV OBMC / NEWMV WARPED skip / NEWMV OBMC. Our symbols matched libaom's
through the first three (mvs transposed-equal), then B10's warped tail over-read and B11's
partition_w32 decoded phantom HORZ_B (5) where libaom reads NONE. The corrupted ALTREF then
leaked into shown frames 13+ through zero-mv skip compound copies (display 13 quad (1,1) is
32x32 NEAREST_NEARESTMV zero-mv skip) — class gate-blind-to-hidden-frames, verbatim.

## Method (evidence)
- libaom `examples/inspect` (built in /tmp/libaom-build; needs IVF wrap:
  `ffmpeg -i fixtures/warp-mismatch.obu -c copy x.ivf`) — ground-truth per-block
  bsize/mode/mv/ref/motion_mode: /tmp/claude-1000/r5e-inspect.json.
- Our EC_AV1_TRACE decode-order frame-13 section: /tmp/claude-1000/r5e-ours-trace.log.
- Frame index mapping decode→display from inspect: display 13 = decode 16; decode 13 =
  display 23 no-show. r5c/r5d's "frame 13 HORZ_B" trace was the HIDDEN frame, and the
  HORZ_B itself was desync garbage, not content.

## What else this explains
- The r5 extended partitions (HORZ_B/VERT_A/VERT_B arms) decoded desync GARBAGE on this pin —
  the pin never legitimately contained them (min/max partition 32 in the gate recipe!). They
  are exercised by hand only via desync; their correctness is UNPROVEN by this pin. They stay
  (harmless spec surface) but need a rect-partition-enabled gate before being trusted.
  DISPOSITION: deferred(rect/ab-partition gate) — the warp gate recipe pins
  --enable-rect-partitions=0 --enable-ab-partitions=0.
- r5d's bh4 fix + 89122b0 coordinate fix stay (adjudicated PROVEN-RIGHT r5d).

## Still owed on the lane (in flight / next)
- Warp gate flip: refuses-or-matches → must-decode (named_refusals == 0, warp_selected_hits
  > 0); 40-seed sweep measuring now (r5e-warpgate.log).
- Full workspace gate + merge to main + push + teardown per only-main-survives.
- Debts rewrite: warp leaves the NAMED-refusal list.
