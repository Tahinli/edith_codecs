# lane-mergefix r4 handoff

Tip: lane-mergefix 6bc2c8b (loop filter skip/pu_edge suppression + `blk_org_grid`).
Read `lanes/mergefix-r4.report.md`.

## Do not re-hunt: decode-order frame 1 is NOT a compound-prediction defect
Its pre-filter reconstruction is BIT-EXACT vs the oracle (0 px, both before and after
this round's fix). The r3 charter's "3376 px starting at row 0 col 64 = a 32x32 compound
skip block" was reading the GATE's frame numbering: the pixel gate compares SHOWN frames,
so its "frame 1" is DECODE-order frame 2.

## First differing prediction sample, and the rung that showed it
Decode-order frame 2, pre-filter (rung: ours `EC_AV1_PREFILT_DUMP16` cropped 192x68 u16 LE
vs aomdec `EC_AV1_PREFILT_DUMP` cropped 192x68 u8):
`row 0 col 97: ours 42, aomdec 8` (col 96 MATCHES). All 1306 differing px lie in the
single 32-px column band x=96..127, rows 0..63 -- one 32-wide block column. Frames 0 and 1
are 0 px at this rung. That band is the whole remaining straddling-gate failure.

## Second, smaller residue (open)
Decode frame 1 post-deblock is 177 px off ON THE SHIPPED TREE, all in rows 61-67
(bottom straddling band), cols 130-191: 143 px we filter that libaom leaves alone, 15 the
other way -- a y=64 horizontal-edge decision in the partial last mi row. Numbers this
round: 7244 (before the fix) -> 255 (suppression without `pu_edge`) -> 177 (shipped).

## Rung gotchas (cost this round two runs)
- aomdec `EC_AV1_PREFILT_DUMP16`/`POSTDEBLOCK_DUMP16` SEGFAULT on an 8-bit stream; use the
  8-bit rungs there.
- aomdec `EC_AV1_POSTDEBLOCK_DUMP` writes the mi-ALIGNED buffer (192x72 luma, 20736 bytes
  total) while its `EC_AV1_PREFILT_DUMP` writes the crop (192x68, 19584): crop before diffing.
- New rung `EC_LFPARAMS=1` prints the frame's loop filter params; frame 1 is
  `level=[20,16,20,26] delta_enabled=true delta_update=false ref_deltas=[1,0,0,0,-1,0,-1,-1]`,
  so levels are NOT the defect.

## Environment
Stream `~/.cache/mergefix-tmp/str61.obu` md5 a14892ed0ba88b6ad2b566e251ea2d33 (`mkstr.sh`);
oracle `~/.cache/mergefix-tmp/aom-build/aomdec`; artifacts `~/.cache/mergefix-tmp/r4_*`;
gate logs `$HOME/.cache/mergefix-r4-gates.log`, `$HOME/.cache/mergefix-r4-sib.log`.
