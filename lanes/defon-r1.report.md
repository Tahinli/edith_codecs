# lane-defon r1 — NEVER_ON default-on tools spelled on, and proven to fire

Branch `lane-defon` off main 61c6a5b. Two commits: 46df1fc (six tools, written by the
first agent before a session limit killed it mid-suite) and 6e32d5e (this round: the two
tools that commit left open, plus a coverage-classification bug it introduced).

Premise at 61c6a5b (`cargo test -p ec-av1 --lib gate_coverage -- --nocapture`):
NEVER_ON_8BIT 6/10, NEVER_ON_10BIT 7/10. `aomenc --help` (oracle build) confirms
`--loopfilter-control` takes 0/1/2/3 with `1 = ... (default)` and that
`--fwd-kf-dist=-1` (no repetitive forward keyframes) is the default.

## Tool -> gate -> counter -> result

| tool (on-value spelled) | gate | counter hard-asserted | result |
|---|---|---|---|
| `--loopfilter-control=1` 8-bit | `a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact` | `deblock_hits()` delta (pre-existing) | PASS |
| `--loopfilter-control=1` 8-bit | `a_real_aomenc_inter_sequence_with_deblocking_decodes_pixel_exact` | `deblock_hits()` | PASS |
| `--loopfilter-control=1` 10-bit | `a_real_aomenc_10bit_inter_sequence_decodes_pixel_exact` | `deblock_hits()` | PASS |
| `--enable-tx-size-search=1` 8+10-bit (one `bit_depth`-parameterised helper) | `a_real_aomenc_inter_sequence_with_tx_select_decodes_pixel_exact{,_10bit}` | `txfm_split_hits()` per attempt | PASS |
| `--enable-directional-intra=1` 8-bit | `a_real_aomenc_stream_with_directional_chroma_decodes_pixel_exact` | `directional_uv_hits()` | PASS |
| `--enable-diff-wtd-comp=1` 8-bit | `a_real_aomenc_stream_with_masked_compound_decodes_pixel_exact` | NEW `diffwtd_hits() > 0` | PASS (80/80 pixel-exact, masked_compound_hits=7898 wedge_hits=4254) |
| `--enable-onesided-comp=1` 8-bit (flipped from `=0`) | `a_real_aomenc_stream_with_interintra_decodes_pixel_exact` | NEW `uni_comp_hits() > 0` | PASS |
| `--enable-fwd-kf=1 --fwd-kf-dist=8` **8-bit and 10-bit** | `a_real_aomenc_altref_sequence_hidden_frames_decode_pixel_exact` (the only gate that decodes `show_frame == 0` frames, and it loops both depths) | NEW `fwd_kf_hits()` delta per depth | PASS — 42 frames decoded, 2 hidden, **3 forward keyframes**, all pixel-exact vs the oracle in decode order, at each depth |
| `--enable-interintra-wedge=1` **10-bit** | NEW `a_real_aomenc_10bit_stream_with_interintra_wedge_decodes_pixel_exact` | `wii_hits`, `interintra_hits` (both via `ten_bit_tool_gate`) | PASS — 8/8 attempts pixel-exact, wii_hits=3 interintra_hits=8 |
| `--enable-onesided-comp=1` **10-bit** | NEW `a_real_aomenc_10bit_stream_with_onesided_compound_decodes_pixel_exact` | `uni_comp_hits` | PASS — 8/8 attempts pixel-exact, uni_comp_hits=5 |
| `--enable-diff-wtd-comp=1` 10-bit | — | — | NOT LANDED, see residue |

EVIDENCE: $HOME/.cache/defon-suite.log | `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j3 -- --nocapture` (and each gate above run by name first) | see the per-gate stdout lines quoted in the result column

## What changed this round (6e32d5e)
- `crates/ec-av1/src/decode.rs` — `FWD_KF_HITS` thread-local + `fwd_kf_hits()` / `note_fwd_kf()`,
  same before/after pattern as the other gate counters.
- `crates/ec-av1/src/stream.rs` (`decode_stream`, just before the `if header.show_frame` output
  arm) — `if header.frame_type == FrameType::Key && !header.show_frame { decode::note_fwd_kf(); }`.
  Counted after the tiles decode, so the assert covers decoded-and-compared frames only.
- `crates/ec-av1/src/stream.rs` — `--enable-fwd-kf=1 --fwd-kf-dist=8` at the head of the altref
  hidden-frames recipe (aomenc keeps the FIRST occurrence of a repeated flag) + a hard
  `fwd_kf > 0` assert per bit depth; two new 10-bit gates through `ten_bit_tool_gate`.
- `crates/ec-av1/src/gate_coverage.rs` — `NEVER_ON_8BIT` is now `&[]`; `NEVER_ON_10BIT` keeps
  `enable-diff-wtd-comp` (with the measurement that justifies it) and `multi-tile`.

## Defect found and fixed inside the instrument
`gate_coverage` splits gate bodies on `"\n    #["`, so a doc comment lands in the PRECEDING
gate's chunk. My first draft of the two new docs used the literal `yuv420p10le`, which
`is_ten_bit()` treats as a 10-bit marker — it silently reclassified the gate above as 10-bit
(8-bit gate count 45 -> 44, 10-bit 15 -> 18). Reworded to "10-bit"; counts back to 45/17
(15 pre-existing + my 2). Class: [[gate-blind-to-feature]]'s instrument twin — prose in a
gate file is parsed as recipe.

## Coverage after (verbatim)
```
gate_coverage: 59 real-aomenc gates, 17 of them 10-bit
NEVER_ON_8BIT (0 of 10, over 45 8BIT gates):
NEVER_ON_10BIT (2 of 10, over 17 10BIT gates):
    enable-diff-wtd-comp (--enable-diff-wtd-comp): off in 1, defaulted in 16, on in 0
    multi-tile (--tile-columns,tile-rows): off in 0, defaulted in 17, on in 0
```

## Refusals lifted
None. No decode behaviour changed; this lane only makes the gates drive the default-on tools
explicitly and hard-assert that they fired.

## Residue
- deferred: `enable-diff-wtd-comp` at 10 bits — MEASURED, not assumed: a 10-bit
  masked-compound gate with `--enable-diff-wtd-comp=1 --enable-masked-comp=1
  --enable-dist-wtd-comp=0` decoded 8/8 attempts pixel-exact with `diffwtd_hits == 0`.
  That is encoder choice, not a decoder defect: `ten_bit_tool_gate`'s only fixture source is
  `gradients`, whose smooth content never wins a difference-weighted mask, while the 8-bit
  gate needs `mandelbrot`'s hard diagonal edge. The flag was deliberately REMOVED from the
  landed 10-bit recipe rather than left spelled-on-unasserted (a flag a gate turns on without
  proving it fired is coverage theatre). What unblocks it: a mandelbrot (or otherwise
  hard-edged) 10-bit source in `ten_bit_tool_gate`.
- accepted: `multi-tile` at 10 bits — covbd2's finding, outside this charter.

## Suite totals
`EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-defon nice -n 10 cargo test -p ec-av1 --lib -j3 -- --nocapture`
(log: `$HOME/.cache/defon-suite.log`, 1141 s):

```
test result: FAILED. 299 passed; 1 failed; 27 ignored; 0 measured; 0 filtered out
```

The one failure is **not this lane's** and is not a gate:
`decode::tests::nz_map_ctx_offset_tables_match_the_rect_rule` (decode.rs:16368) —
`32x64 nz_map offset at display (row 0, col 2): left: 6, right: 11`. It compares two static
tables; this lane's diff against its base 61c6a5b touches no table, scan or offset line in
decode.rs (only a new `FWD_KF_HITS` thread-local), and every real-aomenc gate passed.
Disposition: flagged, inherited from the branch base — whoever merges should re-run it on
main's tip, where a sibling lane may already own it.
