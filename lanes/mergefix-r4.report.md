# lane-mergefix r4 report

## Verdict: the r3/charter premise is FALSE — frame 1 is not a prediction defect

Frame 1 of the pinned stream reconstructs BIT-EXACT (0 differing luma px, whole
cropped frame) against the instrumented oracle's pre-filter dump. The 3376 px the
straddling gate reports are introduced by OUR DEBLOCK, not by compound prediction:
we filter ~10x more pixels than libaom on that frame.

EVIDENCE: ~/.cache/mergefix-tmp/{r4_pre.f1,r4_aom.f1,r4_ours.f1,r4_apd.f1} |
`EC_AV1_PREFILT_DUMP16=<p> decode_probe str61.obu` vs
`EC_AV1_PREFILT_DUMP=<p> aomdec --rawvideo -o /dev/null str61.obu`, then the same pair
of POSTDEBLOCK rungs (aomdec's dump is the mi-ALIGNED buffer 192x72, ours the crop
192x68 — crop before comparing) |
frame 0 pre 0 / postdeblock 0; frame 1 pre 0 / postdeblock 7244 differing luma px;
against the shared pre-filter picture libaom changes 718 px, we change 7345
(6753 pixels we filter that libaom leaves alone, 126 the other way).

## Where it points (not yet fixed)
Filter levels are not the cause: frame 1 parses `level=[20,16,20,26] sharpness=0
delta_enabled=true delta_update=false ref_deltas=[1,0,0,0,-1,0,-1,-1] mode_deltas=[0,0]`
(new rung `EC_LFPARAMS`, decode.rs ~12157), and the pattern is "extra edges", not
"stronger filtering" (max |delta| vs pre-filter is 10 on BOTH decoders).

The suspect is the loop-filter TU grid on inter frames plus the missing skip
suppression of spec 7.14.2 / libaom `set_lpf_parameters`
(`if ((curr_level || pv_lvl) && (!pv_skip || !curr_skipped || pu_edge))`, where
`curr_skipped = skip_txfm && is_inter`). `edge_params` (decode.rs ~13276) documents
that it deliberately omits that term under the invariant "a coded block's transform is
always its own full size" — an invariant var-tx broke. `EC_TXGRID_TRACE` shows the whole
`mi_row=16` row of frame 1 publishing 8x8 TUs (`w_mi=2 h_mi=2 tx_px=8`), i.e. an 8-px
interior edge lattice that libaom does not filter when both sides are skip-inter and the
edge is not a PU edge.

## Changed
- `crates/ec-av1/src/decode.rs` ~12157: `EC_LFPARAMS` rung (frame-level loop filter
  params) — the instrument that ruled the level out. No behavioural change.

## Gates
Not re-run this round (no behavioural change shipped); the straddling gate stands where
r2/r3 left it (3376/3622 px, see r3 report).

## Disposition
- fix-now (r5): port `set_lpf_parameters`' skip/pu_edge suppression + verify the inter
  var-tx / skip-inter LF TU grid publishes the block's max rect tx size.
- accepted: r3's partition-context finding stays unshipped (sibling-gate trade).
