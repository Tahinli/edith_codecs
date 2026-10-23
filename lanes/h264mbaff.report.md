# lane/h264mbaff report

Branch lane/h264mbaff (from main 79e5c011). Not pushed. ec-h264 now decodes MBAFF streams byte-exact.

## JVT MBAFF pass list (all exact against the reference YUV, deblocking on)
exact: CVMA1_Sony_D, CVMA1_TOSHIBA_B, CVMANL1_TOSHIBA_B, CVMANL2_TOSHIBA_B, CVMAQP2_Sony_G, CVMAQP3_Sony_D, CVMAPAQP3_Sony_E, CAMA1_Sony_C, CAMA1_TOSHIBA_B, CAMANL1_TOSHIBA_B, CAMANL2_TOSHIBA_B, CAMANL3_Sand_E, CAMA3_Sand_E, CAMACI3_Sony_C, CAMASL3_Sony_B, cama1_vtc_c, cama2_vtc_b, cama3_vtc_b, CAMP_MOT_MBAFF_L30, CAMP_MOT_MBAFF_L31, CAPAMA3_Sand_F, FRExt2_Panasonic_C, FRExt4_Panasonic_B, MR9_BT_B (24).
refused-with-reason: none. The MBAFF refusal is gone; tests/interlaced.rs `jvt_mbaff_vectors_bit_exact` is the positive sweep, and the MBAFF entries were removed from the refusal tests.

## x264 sweeps (tests/mbaff_x264.rs, generated in-test, byte-exact vs ffmpeg)
interlaced=1 tff/bff; CABAC and CAVLC; I/P/B, b-pyramid ref4, spatial and temporal direct; weightp=2/weightb=1; 8x8dct on/off; slices 3-4; qp 12/40; subme9/trellis; 192x128 (20 cases), 720x576 (5), 1920x1080 (6, opt-in `EC_H264_MBAFF_HD=1`, run on the server gate), High 10 MBAFF (5). Skips without ffmpeg/libx264, fails under EC_REQUIRE_FIXTURES.

## Fixes found on the way
- D neighbour of a bottom frame MB beside a field pair is the bottom field MB, row 7 (matched ffmpeg; was the top MB).
- Temporal direct in a field MB maps the colocated field reference to that very field (not the same-parity field).
- Re-sent PPS/SPS now rebuild the level-scale tables (stale scaling lists made FRExt2 wrong; latent bug outside MBAFF).
- Constrained intra: left availability per group of four rows; chroma DC uses per-half availability.

## Perf (1080p, 40 frames, bench_decode, base = main 79e5c011, same machine, perf stat instructions:u)
Output hashes identical on both clips (hd8 d827bd70701d528a, hd8b e0e48a22d309ddf2).
hd8 (CABAC/B/weightp): 21.583G -> 21.825G (+1.1%). hd8b (CAVLC mandelbrot): 11.863G -> 12.049G (+1.6%). Both inside +2%. The first cut was +7%; recovered by a plain (non-MBAFF) MV neighbour fast path, outlining MBAFF-only colocated/reference code, and replacing closures in compensate8 by an inline fn. PAFF stays exact (all PAFF/FRExt field tests pass).

## Gates
- clippy -D warnings clean.
- `cargo test -p ec-h264 --release --no-fail-fast` with EC_REQUIRE_FIXTURES=1 EC_H264_MBAFF_HD=1 on server host c: all pass except `real_library_frames_encode_and_decode_exactly` (known server skip: no real-library media there).
- Test edits: conformance.rs x264 "interlaced" refusal case removed (proved >= 1); refusals.rs MBAFF synthetic and vector entries removed.
- DONE.md D1 row updated.

## What is left
- No MBAFF vector or x264 case fails. Not exercised: MBAFF with 4:2:2/4:4:4 (refused for chroma format as before), MBAFF streams with multiple slices per pair row beyond x264's slices=3/4, and long-term refs in field MBs beyond MR9_BT_B.
- Table 6-4 is implemented once in src/mbaff.rs (`locate`); MBAFF deblocking lives in src/deblock_mbaff.rs.
