# lane-rectchroma2 r1 -- rect INTER blocks predicted chroma with the WIDE kernel

## Premise re-measured first

lane-interedge r1's residue reproduced on this tree (main 2c04819 + lane-cdef + lane-golomb
173b793): mandelbrot 192x128 (the CONTROL size -- zero frame-edge nodes), 6 frames,
`--enable-rect-partitions=1 --min-partition-size=8 --enable-tx-size-search=0 --lag-in-frames=0`,
intra tools off, cq 20/32/45, 8- and 10-bit: **luma exact, chroma differs on frames 2..5 in all
six arms**. Streams hashed twice, identical.

EVIDENCE: `~/.cache/rectchroma2-tmp/c192-128-{8,10}-{20,32,45}.obu` (each sha256 twice, equal)
| `decode_probe` with `EC_AV1_FINAL_DUMP` vs `ffmpeg -pix_fmt yuv420p[10le]`, every decode-order
frame, per plane | e.g. cq32 8-bit: `[(2,'chroma (0,32)..(1,40) n=17'), (4,...), (5,...)]`, luma
exact in all 6 arms.

## Stage bisect

Regenerated the same recipe with `--loopfilter-control=0` (deblock off; CDEF/LR already off in
that recipe): **the chroma mismatch survives unchanged** -> not a filter-stage defect, so the
charter's suspect (d) (chroma deblock levels / uv tx grid for rect inter blocks) is exonerated.
The surviving diffs are +-1 on scattered samples inside 4-wide-by-8-tall and 8-wide-by-4-tall
CHROMA regions (e.g. frame 4: chroma x32..35 rows 26..39, and x48..55 rows 16..19) -- i.e. the
chroma halves of 8x16 and 16x8 LUMA blocks.

EVIDENCE: `~/.cache/rectchroma2-tmp/n192-128-8-32.obu` (`gen_nolf.sh`) | decode + full-plane
compare | `MISMATCH [(2,'chroma ... n=15'), (4,...n=36), (5,...n=45)]`, luma exact.

## Root cause (charter suspect (a), confirmed)

`crates/ec-av1/src/decode.rs` `decode_inter_block` predicts a RECTANGULAR block over its
enclosing SQUARE buffer (`side`x`side`, chroma `chroma_side`x`chroma_side`; `write_w`/`write_h`
govern only the pixel write -- the lane-partitions r1 corner-cut). The 4-tap decision lived
inside `mc::predict_with_filters` and read that BUFFER's dimensions. libaom
`av1_get_interp_filter_params_with_block_size` (reconinter.h, called per axis from
`inter_predictor` with `w` then `h`) reads the PREDICTION BLOCK's dimensions: `w <= 4` ->
`av1_interp_4tap[0]` (SHARP and REGULAR both) / `[1]` (SMOOTH). Luma is never <= 4 wide on this
path, so only chroma of a rect block (4x8 / 8x4 / 4x16 ...) was affected -- exactly the
"luma exact, chroma wrong" signature.

- `crates/ec-av1/src/mc.rs`: `predict_with_filters_kern` / `predict_scaled_kern` /
  `predict_compound_intermediate_kern` -- the existing three entry points keep their signatures
  and delegate with `kern = block dims`; `horizontal_scaled_pass` takes `kern_w`.
- `crates/ec-av1/src/mc.rs`: `RECT_NARROW_KERN_HITS` + `rect_narrow_kern_hits()` /
  `reset_rect_narrow_kern_hits()` / `note_narrow_kern()` -- counts predictions whose kernel
  choice DIFFERS from what the square buffer would have given (one bump per prediction).
- `crates/ec-av1/src/decode.rs`: the 12 prediction calls inside `decode_inter_block` now pass
  `write_w/write_h` (luma) and `write_chroma_w/write_chroma_h` (chroma) as the kernel dims.

Class sweep: every other `mc::predict*` call site passes a buffer whose dims ARE the block's
(`obmc_neighbour_pred` takes the neighbour's true w/h; the 8x8 leaf path is square, chroma 4x4;
the CfL bilinear path is unrelated), so no sibling site needed the change.

## Gate (GREEN)

```
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-rectchroma2 EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 \
PATH="$HOME/.cache/aom-oracle/build:$PATH" cargo test -p ec-av1 --lib narrow_kernel -- --nocapture
```
`a_real_aomenc_rect_inter_block_predicts_chroma_with_the_narrow_kernel_pixel_exact`
(`crates/ec-av1/src/stream.rs`): real aomenc, mandelbrot 192x128 AND its transposed twin
128x192 (so 4x8 and 8x4 chroma both fire), 8- and 10-bit, cq 20/32/45, 6 frames,
`--lag-in-frames=0` so decode order == display order and EVERY frame is compared on Y, U AND V
against ffmpeg. CDEF and loop restoration left ON. Streams encoded twice and asserted equal.
`compared == 12` is a hard assert (a refusal fails the gate, never SKIPs), and the narrow-kernel
counter -- reset per attempt, read only on compared attempts -- must be nonzero at BOTH depths
and in BOTH orientations.

EVIDENCE: gate stdout | 12 aomenc -> ec-av1 -> ffmpeg attempts, 6 frames each, all three planes
| `12/12 arms 6 frames full-plane exact; narrow-kernel hits 24/4/16 (192x128 8-bit cq20/32/45),
18/26/36 (10-bit), 42/20/18 and 36/36/34 for 128x192` -- 0 refusals, 0 out-of-scope mismatches.

Before the fix the SAME recipe mismatched chroma in every arm (the premise sweep above), so the
gate is not vacuous.

## Refusals
None lifted (no inventory string covers this defect -- it was a wrong-pixels defect behind an
already-accepted path). `refusal_inventory.rs`/`gate_coverage.rs` untouched by intent, both green
in the suite below.

## Suite
`$HOME/.cache/rectchroma2-suite-r1.log`: **388 passed, 0 failed, 35 ignored** (1668s), including the new gate. Baseline before this round on the same tree shape was 387/0/35 (this round adds one gate).

## Residue
- accepted: `predict_scaled_kern` (scaled reference / superres) got the same fix by construction
  but is not exercised by this gate -- superres rect-chroma has no aomenc recipe on this lane.
- deferred(lane-interedge merge): that lane's frame-edge gate can now become a FULL-PLANE arm;
  its luma-only compare was a workaround for exactly this defect.
