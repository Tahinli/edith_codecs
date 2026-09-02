# lane-intrasplit r2 — RED (gate now FIRES at 10-bit and MISMATCHES; 8-bit arm still never fires)

Base: 45bdc87 (r1) + main 48b35ab + lane-sqdrift 7d498c0 + lane-r14 b86eb38 +
lane-inter16ab 4811488 + lane-intra14 f4b5419 (live tips read with `git log`, NOT the
charter's stale `af7de9f`/`8313c4a`).

## Merges (step 1)
- `git merge main` (48b35ab) — clean.
- `lane-sqdrift 7d498c0` — clean.
- `lane-r14 b86eb38` — conflict in `stream.rs`: git interleaved THREE different gates
  (the two no-CFL `uv_mode` gates from sqdrift/sbrect10 and r14's 64-axis 1:4 gate).
  Resolved by reconstructing each side's full test and keeping all three
  (`a_real_aomenc_inter_frame_with_a_64x64_intra_block_reads_the_no_cfl_uv_alphabet`,
  `a_real_aomenc_square_only_inter_sequence_with_a_64x64_intra_superblock_decodes_pixel_exact`,
  `real_aomenc_1to4_streams_decode_pixel_exact_and_rect_vartx_leaves_fire_before_a_named_refusal`).
- `lane-inter16ab 4811488` — clean.
- `lane-intra14 f4b5419` — conflicts in `decode_probe.rs` (kept both counter blocks),
  `refusal_inventory.rs` (kept intra14's NARROWED "an intra-coded 16x4/4x16 strip on the
  inter block path"; dropped both main's broad 1:4 line and intra14's copy of THIS lane's
  lifted split-tx refusal) and `decode.rs` (kept both thread-locals; kept both the
  sbrect10 `is_cfl_allowed` narrowing and intra14's `INTRA_IN_INTER_MODE` skip/mode split
  in `read_intra_mode_rect`, in that order).
- `crates/ec-av1/src/cdf.rs` is byte-identical to main (`git diff main -- cdf.rs` = 0 lines).

## Fix this round (the merge cross-product the ledger predicted)
`crates/ec-av1/src/decode.rs` `rect_inter_luma_set` + `crates/ec-av1/src/cdf_state.rs`
`TxbSet::LumaRect8x4Inter{,Inter1}` — with lane-r14's general `read_var_tx_size` in, a
SPLIT 16x4/4x16 inter strip resolves to an 8x4 leaf (`sub_tx_size_map[TX_16X4] == TX_8X4`,
oracle `av1/common/common_data.h:180`) and hit `unreachable!("rect_inter_residual_supported
gates every shape that reaches here")`. The new set is `LumaRect4x8`'s coefficient tables
(`get_txsize_entropy_ctx(TX_8X4)` = TX_8X8, 32-position `eob_pt`) with the INTER `tx_type`
row `txsize_sqr_map[TX_8X4]` = TX_4X4 (`inter_tx_type_4` / `_set1`). No refusal was lifted
by this: `rect_inter_residual_supported` (the BLOCK-footprint gate) is untouched.

EVIDENCE: $HOME/.cache/intrasplit-gate-r2b.log | temporary `(w={w} h={h})` in the
unreachable, gate arm re-run | panic message `... (w=8 h=4)`.

## Gate result: RED
`cargo test -p ec-av1 --lib -- --test-threads=1 split_transform_intra_strip`
- 8-bit arm: 40/40 attempts still stop at OTHER lanes' named refusals — histogram
  non-skip rect strip residual 14, inter SB-level AB 8, split intra strip w/ 64x32 unit 4,
  non-DC chroma on 8x8 inter leaf 4, inter partition below 8x8 4, 32x64 unit 2, Golomb
  tail 2, angle delta on 8x8 intra leaf 1, intra 16x4 inside a 16-level 1:4 partition 1.
- 10-bit arm: **one attempt (seed 74, cq 12, cpu 2) now decodes all 8 frames and FIRES**
  (`depth1=1`) — and MISMATCHES ffmpeg: frame 1 Y first diff at (163, 58), 5203 samples;
  frames 2-7 ~24.5k each (drift). This is the first time this lane's lifted path was ever
  pixel-compared on a real stream.

EVIDENCE: $HOME/.cache/intrasplit-gate-r2.log | 2 arms x 40 attempts, merged tree | 8-bit
0 firing / 40 refusals; 10-bit 1 firing attempt, frame 1 Y 5203 samples wrong.

### What is known about the 10-bit mismatch (r3 starts here)
Stream pinned: `<scratchpad>/intrasplit/s74.obu`, md5 `50ea2b42423f1c8b4eed9fa48c4775a6`
(192x128 yuv420p10le, mandelbrot start_scale 3.08, cq 12 cpu-used 2, recipe = the gate's).
- The ONLY split-transform intra strip in the whole 8-frame stream is in decode-order
  frame 1 at `mi_row=8 mi_col=0 bw=32 bh=16 depth=1` (pixels x 0..31, y 32..47) — new
  env-gated print `EC_SPLITSTRIP=1` (decode.rs, next to the counter bump).
- Its own footprint is pixel-CLEAN in frame 1 (raster first diff is at (163, 58), i.e. the
  LAST block of the same 32-px block row), so the strip's coefficients and prediction are
  right and the divergence is downstream of it — either a symbol this arm reads/does not
  read after the strip, or another shape's defect in that block row.
- Not yet done: the msac RANGE ladder vs the instrumented aomdec. Our tags for frame 1
  are `EC_MODE`/`EC_MODE_VAL`/`EC_STACK` (`EC_TRACE_MODE=1`, 338 lines for the stream);
  aomdec's are `EC_IMODE*` for the key frame and `EC_MODE*`/`EC_MODE_MV`/`EC_STACK`
  (219 lines) — the two emitters do NOT line up 1:1 and need reconciling before diffing
  (compare RANGE, never tell()).
- Do NOT triage this with the probe's `-o` dump: `decode_probe` writes 8-bit samples for a
  10-bit stream, so a byte compare against aomdec's `--rawvideo` output is meaningless
  (frame 0 reads as 22667 "diffs" although the gate proves it exact).

## Film probes
EVIDENCE: <scratchpad>/intrasplit/{hg1200,troy1800}.obu | `decode_probe` with
EC_AV1_FINAL_DUMP=1 on the merged tree | new stop strings:
- 2160p10 HDR AV1 film, -ss 1200: `a split intra strip whose transform unit is 32x64`
  (was "a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual
  coding" at r1) — 0 frames dumped.
- 1080p10 AV1 film, -ss 1800: `a COMPOUND_WEDGE mask on a rectangular inter block (the
  wedge codebook is square-only)` (was "an intra-coded 1:4 (or other non-2:1) rect strip
  on the inter block path" at r1, lifted by lane-intra14) — 0 frames dumped.

## Suite
See `$HOME/.cache/intrasplit-suite-r2.log` (unit `intrasplit-suite-r2-*`, MemoryMax=10G).

## Residue
- fix-now (r3): the 10-bit seed-74 mismatch above — the lane's own gate finally fires and
  is red; the refusal must NOT be considered lifted until it is green.
- deferred(lane owning 64-axis luma tables): the "split intra strip whose transform unit is
  64x32 / 32x64" refusals — now the FIRST blocker on the 2160p film segment.
- deferred(other lanes): the 8-bit arm cannot fire until the non-skip rect-strip residual,
  inter SB-level AB and sub-8x8 inter refusals are lifted (14+8+4 of 40 attempts).
