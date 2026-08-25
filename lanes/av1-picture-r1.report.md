# lane-av1-picture r1 — ec-av1 encodes a picture

## What landed

`crates/ec-av1/src/encode.rs`: a key-frame picture encoder.

- `Picture` — an 8-bit planar 4:2:0 picture.
- `key_frame_headers(width, height, base_q_idx)` — the sequence and frame
  headers the tile writer's subset is coded under (one tile, TX_MODE_LARGEST,
  no in-loop filtering, no CDF adaptation).
- `encode_key_frame(&Picture, base_q_idx, deadzone) -> Encoded` — an AV1 stream
  plus the encoder's own reconstruction.

Every block is 32x32 luma / 16x16 chroma, DC-predicted from the reconstruction
above and to its left exactly as the decoder will (`dc_predict`, spec 7.11.2),
residual through `forward_and_quantize`, reconstruction through
`dequant_and_inverse`. Superblocks are split into their four quadrants in the
order the decoder walks them.

## Gates (`cargo test -p ec-av1`, 69 green)

| test | what it holds |
|---|---|
| `ffmpeg_decodes_exactly_what_the_encoder_reconstructed` | 64x64, 96x64 and 160x96 test cards: **every sample of every plane** equals the encoder's reconstruction after an ffmpeg decode. Prediction reads the reconstruction, so one sample of drift would spread down and right — this is an equality, not a tolerance |
| `the_encoded_picture_is_the_one_that_went_in` | it is a reconstruction *of the right picture*: luma PSNR > 36 dB, chroma > 40 dB at q 100 |
| `fidelity_and_rate_move_with_the_quantizer` | bytes and PSNR both monotone in q (70/90/110) and in the deadzone (0.5/0.3/0.15) |
| `a_flat_picture_costs_almost_nothing` | a flat picture is under 100 bytes and every block after the first reconstructs its exact value |
| `a_picture_off_the_block_grid_is_refused` | off-grid sizes, a short plane and an out-of-band q are errors, not panics |
| `a_frame_of_real_video_decodes_to_what_the_encoder_reconstructed` | the same equality gate on a frame of real video (`EC_AV1_CLIP`, `EC_AV1_CLIP_SKIP`) |

## His data (640x352, q_idx 100, all sample-exact against ffmpeg)

| clip | bytes | luma PSNR |
|---|---|---|
| `he_is_not_the_only_one.mp4` @0s | 11 429 | 42.61 dB |
| OBS screen capture 2026-08-17 @5s | 33 674 | 38.45 dB |
| a 1080p AV1 film @5s | 32 366 | 39.26 dB |

## Where that sits against libaom

Same film frame, same size, libaom-av1 `-still-picture -cpu-used 4`:

| crf | bytes | PSNR |
|---|---|---|
| 30 | 15 923 | 38.42 dB |
| 40 | 8 413 | 34.91 dB |
| 50 | 4 402 | 31.58 dB |

Interpolating libaom to our 32.4 kB puts it near 41.7 dB against our 39.26 dB:
**about 2.4 dB behind at this rate.** That is the expected size of what is not
implemented yet rather than a defect — the encoder has one block size, one
transform type, DC prediction only, no rate-distortion decision and no CDF
adaptation. Each of those is a lane.

## Not in this lane

- block sizes other than 32x32, and the partition decision between them
- intra modes other than DC_PRED (the tile writer already codes all thirteen)
- transform types other than DCT_DCT, and transform size selection
- rate-distortion optimisation; the deadzone is a fixed parameter, unswept
- CDF adaptation (`disable_cdf_update` is set), CDEF, loop filter, loop
  restoration, superres
- inter frames, and pictures that are not a whole number of 32x32 blocks
