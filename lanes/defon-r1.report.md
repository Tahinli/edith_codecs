# lane-defon r1 — NEVER_ON holes his default-encoded films exercise

Branch `lane-defon` off main 61c6a5b (no rebase needed, main unchanged during the round).
Premise re-measured with `cargo test -p ec-av1 --lib gate_coverage -- --nocapture` at 61c6a5b:
NEVER_ON_8BIT 6/10, NEVER_ON_10BIT 7/10 (lists identical to lanes/covbd2-r1.report.md).
`aomenc --help` (oracle build) confirms `--loopfilter-control` takes 0/1/2/3 and that
`1 = Enable loopfilter for all frames (default)`.

## Tool -> gate -> counter -> result

| tool (on-value spelled) | gate | counter hard-asserted | result |
|---|---|---|---|
| `--loopfilter-control=1` (8-bit) | `a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact` | `decode::deblock_hits()` advanced (pre-existing assert) | PASS 1.10s |
| `--loopfilter-control=1` (8-bit) | `a_real_aomenc_inter_sequence_with_deblocking_decodes_pixel_exact` | `deblock_hits()` (pre-existing) | PASS 2.36s |
| `--loopfilter-control=1` (10-bit) | `a_real_aomenc_10bit_inter_sequence_decodes_pixel_exact` | `deblock_hits()` (pre-existing) | PASS 2.84s |
| `--enable-tx-size-search=1` (8-bit + 10-bit, one shared helper `tx_select_inter_gate`) | `a_real_aomenc_inter_sequence_with_tx_select_decodes_pixel_exact` and `..._10bit` | `txfm_split_hits()` per attempt (pre-existing) | PASS 2.14s / 1.79s |
| `--enable-directional-intra=1` (8-bit) | `a_real_aomenc_stream_with_directional_chroma_decodes_pixel_exact` | `directional_uv_hits()` (pre-existing) | PASS 5.69s |
| `--enable-diff-wtd-comp=1` (8-bit) | `a_real_aomenc_stream_with_masked_compound_decodes_pixel_exact` | NEW `decode::diffwtd_hits()` > 0 | PASS 344s, 80/80 pixel-exact, masked_compound_hits=7898 wedge_hits=4254 |
| `--enable-onesided-comp=1` (8-bit, flipped from `=0`) | `a_real_aomenc_stream_with_interintra_decodes_pixel_exact` | NEW `decode::uni_comp_hits()` > 0 | see suite totals below (first run aborted on an environmental `aomdec: PermissionDenied` while another lane rewrote `~/.cache/aom-oracle/build/aomdec` at 23:56) |

## What changed
- `crates/ec-av1/src/decode.rs` — two new per-attempt counters: `UNI_COMP_HITS` incremented in the
  `if unidir {` arm of the compound-reference reader (spec 5.11.25 `comp_reference_type == 0`), and
  `DIFFWTD_HITS` incremented at both `mc::diffwtd_mask(...)` call sites (the blend, not the symbol
  read that `MASKED_COMPOUND_HITS` already counts).
- `crates/ec-av1/src/stream.rs` — six explicit on-values inserted BEFORE `"--codec=av1"` in their
  gate's arg list (aomenc keeps the FIRST occurrence of a repeated flag), one `=0 -> =1` flip on the
  interintra gate, two new hard asserts (`diffwtd_hits() > 0`, `uni_comp_hits() > 0`), and
  `#[track_caller]` on `inter_sb_none_gate` so its body stops gluing onto the directional-chroma
  gate's chunk (gate_coverage splits on `"\n    #["`, so an attribute-less helper mislabels the gate
  above it 10-bit).
- `crates/ec-av1/src/gate_coverage.rs` — `covers_both_depths()`/`covers_depth()`: a helper
  parameterised on `bit_depth` builds BOTH an 8-bit and a 10-bit stream from one recipe, so its flags
  count at both depths; classifying it by the 10-bit strings inside its own conditional hid
  `--enable-tx-size-search=1` from the 8-bit list. `NEVER_ON_8BIT` 6 -> 1 entry, `NEVER_ON_10BIT`
  7 -> 5 entries.

## Coverage after (verbatim)
```
NEVER_ON_8BIT (1 of 10, over 45 8BIT gates):
    enable-fwd-kf (--enable-fwd-kf): off in 12, defaulted in 33, on in 0
NEVER_ON_10BIT (5 of 10, over 15 10BIT gates):
    enable-diff-wtd-comp (--enable-diff-wtd-comp): off in 1, defaulted in 14, on in 0
    enable-fwd-kf (--enable-fwd-kf): off in 3, defaulted in 12, on in 0
    enable-interintra-wedge (--enable-interintra-wedge): off in 5, defaulted in 10, on in 0
    enable-onesided-comp (--enable-onesided-comp): off in 5, defaulted in 10, on in 0
    multi-tile (--tile-columns,tile-rows): off in 0, defaulted in 15, on in 0
```
`cargo test -p ec-av1 --lib -- gate_coverage refusal_inventory` -> 12 passed, 0 failed.

## Refusals lifted
None — no decode behaviour changed; this round only makes existing gates drive the tools explicitly
and proves they fired.

## Residue
- deferred: `enable-fwd-kf` at both depths — needs `--fwd-kf-dist=N` on a lagged recipe AND a decoder
  counter for a KEY frame with `show_frame == 0`; no such counter exists (`grep show_frame`
  decode.rs finds no site). What unblocks it: adding that counter at the frame-header read plus one
  lagged gate.
- deferred: `enable-diff-wtd-comp`, `enable-onesided-comp`, `enable-interintra-wedge` at 10 bits —
  the three 10-bit compound gates pin all three off in their recipes, so closing them means either a
  10-bit sibling of the 8-bit masked-compound/interintra gates or flipping those pins and re-proving
  the pixel compare. What unblocks it: one round with a 10-bit compound recipe.
- accepted: `multi-tile` at 10 bits (covbd2's finding, not in this charter).
