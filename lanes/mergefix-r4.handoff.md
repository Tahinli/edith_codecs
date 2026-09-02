# lane-mergefix r4 handoff

Tip: lane-mergefix (this commit). Only change is the `EC_LFPARAMS` debug rung.

## Do not re-hunt: frame 1 is NOT a compound-prediction defect
Frame 1 pre-filter reconstruction is BIT-EXACT vs the oracle (0 px). The rung that showed
it: our `EC_AV1_PREFILT_DUMP16` (cropped 192x68, u16 LE) vs aomdec `EC_AV1_PREFILT_DUMP`
(cropped 192x68, u8). First differing prediction sample: NONE — there is none.
The divergence appears at the POSTDEBLOCK rung: aomdec's `EC_AV1_POSTDEBLOCK_DUMP` is the
mi-ALIGNED buffer (192x72 luma, 20736 bytes total), ours is the crop; crop to 68 rows
before comparing or you will read garbage. Frame 1 postdeblock: 7244 differing px, first
at row 0 col 0. Against the shared pre-filter picture libaom changes 718 px, we change
7345 — we filter 6753 px libaom leaves alone.

## r5 job
1. Port spec 7.14.2 / libaom `set_lpf_parameters` suppression into `edge_params`
   (decode.rs ~13276, whose comment states the now-false invariant):
   filter only when `(curr_level || pv_lvl) && (!pv_skip || !curr_skipped || pu_edge)`,
   `curr_skipped = skip_txfm && is_inter`.
2. Check the inter/var-tx LF TU grid: `EC_TXGRID_TRACE=1` shows all of frame 1's
   `mi_row=16` publishing `w_mi=2 h_mi=2 tx_px=8` 8x8 TUs; libaom gives a skip-inter block
   one whole-block `max_txsize_rect_lookup` TU.
Levels are ruled out: `EC_LFPARAMS=1` (new) prints frame 1 as
`level=[20,16,20,26] sharpness=0 delta_enabled=true delta_update=false
ref_deltas=[1,0,0,0,-1,0,-1,-1] mode_deltas=[0,0]`; max |delta| vs pre-filter is 10 in
BOTH decoders, so it is edge COUNT, not edge strength.

## Environment
Stream `~/.cache/mergefix-tmp/str61.obu` md5 a14892ed0ba88b6ad2b566e251ea2d33 (`mkstr.sh`);
instrumented oracle `~/.cache/mergefix-tmp/aom-build/aomdec`. Note `EC_AV1_PREFILT_DUMP16`
SEGFAULTS on this 8-bit stream in aomdec — use the 8-bit rung there.
Artifacts: `~/.cache/mergefix-tmp/r4_{pre,ours,aom,apd}.f*`, `r4_lfgrid.txt`.
