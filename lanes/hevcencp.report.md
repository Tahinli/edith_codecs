# lane hevcencp: ec-h265 P-picture encoding

Base: `8de08723` (main). Branch `lane/hevcencp`, merged with main after `lane/hevcperf` landed.

## Goal

P frames in the ec-h265 encoder, which was intra-only. An IDR followed by P frames
at a configurable keyframe interval, one reference (the previous reconstruction),
short-term RPS, POC and slice_type P, num_ref_idx_l0 = 1.

## What changed

- `crates/ec-h265/src/enc_me.rs` (new): motion search. Seeds, a full integer search
  around the best seed, coarse-to-fine pattern search at 16/8/4 quarter-samples,
  then half and quarter refinement. Prediction uses the decoder's `interp_luma` with
  its rounding. Cost is SAD + lambda * 0.5 * se-bits. Whole-sample vectors read the
  reference directly, which is the same value the interpolation gives.
- `crates/ec-h265/src/enc_p.rs` (new): P-picture encoder. Quadtree analysis by
  search, CU decisions among skip, merge (5 indices), and AMVP (2 candidates), each
  chosen by SSD + lambda * bits. Residuals use the existing forward DCT, dead-zone
  quantiser, sign hiding (optional), and CABAC. Deblocking reuses the decoder's
  `deblock_picture`, with motion and bS from the decoder's own field. Uses the
  decoder's `derive_merge`, `derive_amvp`, and `predict_pu`.
- `crates/ec-h265/src/encoder.rs`: `EncoderConfig` gains `keyint` (default 1) and
  `inter_lambda_scale` (default 1/16). `encode_sequence` runs the GOP. The intra path
  is unchanged (output identical to before). `code_picture_planes` returns the
  reconstruction. `PictureRecon` type alias.
- `crates/ec-h265/src/lib.rs`: module registration.
- `crates/ec-h265/examples/hevc_gop.rs` (new): encode/decode driver.
- `scripts/hevc-gop-roundtrip.sh` (new): round trip against ec-h265 decode and ffmpeg.
- `scripts/hevc-bdrate.sh`, `scripts/bdrate.py` (new): BD-rate against libx265.
- `scripts/yuv_psnr.py` (new): per-frame PSNR.
- `crates/ec-h265/src/dec/` is not modified by this lane.

## Deviations

- Lambda: the spec-lambda (same as the intra RD) gave +82.6% BD-rate at 352x288.
  The default is `lambda_for(qp) * 1/16`. The sweep (352x288, keyint 8, QP 22-37)
  was: scale 1: +82.6%, 1/2: +45.5%, 1/4: +30.9%, 1/8: +23.1%, 1/16: +19.9%,
  1/32: +19.9% (plateau). This is documented on `inter_lambda_scale`.
- Analysis uses SAD, not SATD (ec-h264 uses SATD).

## Attempt 2: measurement and lambda units

The first BD-rate numbers were wrong in two ways, both fixed here:

- PSNR came from `yuv_psnr.py` (global MSE). The BD script now takes per-plane PSNR
  from ffmpeg's own `psnr` filter (`scripts/ffmpeg-psnr.sh`: per-frame values from the
  stats file, plane means, YUV = (6Y+U+V)/8). `scripts/bdrate.py` was checked against an
  independent centred-cubic implementation: both reproduce the same BD numbers from the
  same points. BD-rate fits log10(bytes) against PSNR; BD-PSNR fits PSNR against
  log10(bytes); both integrate over the overlap; the rate is `10^(diff)-1`. The formulas
  match VCEG-M33 / the JVET script.
- The motion search and the mode decision shared one lambda, scaled by 1/16. The
  search compares SAD, so its lambda is `sqrt(lambda_SSE)` (HM convention); the mode
  decision compares SSD and uses the spec lambda, scaled by `inter_lambda_scale`. With
  the units separated, scale 1 (the spec lambda) is best.

The earlier figures (+19.75% / +22.67%, -2.5 dB) used the old PSNR and the joint 1/16
lambda and are superseded.

### Raw points (QP 22/27/32/37, 30 frames, GOP 8 vs x265 veryfast bframes=0 keyint 8)

PSNR from ffmpeg's psnr filter. Columns: bytes, PSNR-Y, PSNR-YUV (6:1:1).

352x288, x265: (154513, 49.8417, 49.2919), (113645, 45.2243, 44.6377),
(76984, 40.1013, 39.6810), (46648, 35.6417, 35.2367)
352x288, ec-h265: (184205, 50.3800, 49.6158), (136515, 45.7563, 44.9770),
(102316, 40.9537, 40.4240), (72639, 36.5017, 36.2973)

1280x720, x265: (820596, 51.9377, 51.6526), (572921, 47.2910, 47.0053),
(368626, 42.6370, 42.4692), (196936, 38.5903, 38.4146)
1280x720, ec-h265: (955747, 53.1950, 52.6786), (714038, 48.3847, 47.8624),
(518048, 43.6537, 43.3619), (357287, 39.2657, 39.2082)

Natural clip, 1280x720 from the fixture `fixtures/bitstreams/hevc-1080p-23.976-8bit.265`
(first 30 frames, scaled to 720p):
x265: (572115, 50.4463, 50.0530), (299480, 46.9153, 46.4709),
(139360, 43.9720, 43.4984), (63633, 41.6310, 41.0723)
ec-h265: (799867, 51.4893, 51.1142), (483364, 47.4250, 47.0585),
(267924, 44.1353, 43.7812), (158026, 41.4870, 41.1634)

### BD-rate vs x265 (ffmpeg PSNR)

| clip | BD-rate (YUV) | BD-PSNR (YUV) | BD-rate (Y) | BD-PSNR (Y) |
|---|---|---|---|---|
| 352x288 testsrc2 | +21.85% | -2.66 dB | +21.35% | -2.64 dB |
| 1280x720 testsrc2 | +24.08% | -2.37 dB | +23.26% | -2.31 dB |
| 1280x720 natural (hevc fixture) | +60.59% | -2.19 dB | +64.04% | -2.27 dB |

The BD-PSNR and BD-rate are consistent with each other on each clip. On the natural clip the
x265 curve falls about 0.24 nats per dB (about 27% rate per dB), so -2.2 dB gives roughly
+60% to +70%, as the BD numbers show. On testsrc2 the x265 curve is much flatter (about
8% rate per dB), so -2.5 dB gives about +20%, as measured. The two clips are not on the same
slope, so one dB figure does not carry over between them.

The `reelc.obu` and `reelb.obu` AV1 fixtures decode to near-constant frames (one and
three luma values), so they are not used.

### Why 720p P frames cost more than intra (investigated)

Found and fixed:

1. Lambda units (the bug the 1/16 scale was masking). At 720p, GOP8 was 1.036 x intra with
   the joint lambda; with separate units it is 0.982 x intra (601936 vs 612992 bytes), PSNR-Y
   45.64 vs 46.05.
2. Motion search range below 32x32. The search reaches +-8 samples around one seed for
   blocks under 32, so a 14-row move that no seed contains is not found at 8x8/16x16. An
   exact integer shift test (frame f = frame 0 shifted by 14*f rows, 720p) shows the
   machinery works: P frames are 101801 bytes against 355414 for intra (3.5x smaller), with
   MV (0,-56) selected in 10596 CUs. A unit test `search_finds_a_large_vertical_shift`
   covers the 32x32 case.

MV histogram on testsrc2 (before the lambda split, 720p GOP8): most CUs take 0,0 (static
area, many skip/merge with zero residual). The moving band shows vertical vectors near
+56..+64 quarter samples (14-16 samples), with scatter from fractional motion. A whole-frame
vertical search between source frames 0 and 1 gives its minimum at dy=3, so only part of
the picture moves. The testsrc2 scroll is 14.4 rows per frame, which is fractional, so the
quarter-sample interpolation blurs the prediction and leaves residual.

Not fixed (remaining causes):
- Fractional motion costs residual; there is no RDOQ for inter residuals.
- The reference is the quantised reconstruction, so the prediction carries its noise.
- Greedy decisions do not account for the quality the picture gives to later P frames
  (no QP cascading).

### Verify (merged tree, `be40bdd6`+, this run)

- `cargo clippy -p ec-h265 -p ec-h265-syntax --all-targets -- -D warnings`: pass.
- `cargo test -p ec-h265 -p ec-h265-syntax --release`: pass, every test binary `ok`,
  including `conformance_intra` (9203/9203 SEI hash check).
- `scripts/hevc-gop-roundtrip.sh`: exit 0; all rows exact against ec-h265 decode and ffmpeg.

Round trip (GOP8 vs intra, QP 30, 30 frames):

| clip | intra bytes | GOP8 bytes | ratio | PSNR-Y intra / GOP8 | decode |
|---|---|---|---|---|---|
| 352x288 | 142350 | 116748 | 0.820 | 43.78 / 42.95 | exact, exact |
| 1280x720 | 612992 | 601936 | 0.982 | 46.05 / 45.64 | exact, exact |
| 350x286 | 142183 | 116441 | 0.819 | 43.63 / 42.87 | exact, exact |

### Remaining gaps

- Rate control: ConstantQp only.
- Intra CUs inside P pictures: not implemented.
- RDOQ for inter residuals; QP cascading for reference pictures.
- TMVP off; CTB 64 forced to split in P pictures; no 2NxN/Nx2N/AMP.
- Fractional-motion residual (testsrc2 14.4 rows per frame) and the natural clip at QP 37
  (2.5x the bytes of x265 at the same PSNR).
- Single-threaded P path.
