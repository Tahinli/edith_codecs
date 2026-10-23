# lane hevcenctools: P-picture encoder, bring the natural clip under +25%

Base: `515a8b3e` (main, after `lane/hevcencp`). Branch `lane/hevcenctools`, worktree
`~/.cache/wt/hevcenctools`. Heavy work ran on host a through `scripts/remote-run.sh hevcenctools --host a`.

## Result

The DONE.md gate (BD-rate <= +25% against libx265 veryfast bframes=0 on every clip) is met on
all three clips with the final commit `035f3544` (plus the test re-pin `dae6d079`):

| clip | base (`515a8b3e`) | final, YUV | final, Y |
|---|---|---|---|
| 352x288 testsrc2 | +21.85% | **-5.31%** | -7.76% |
| 1280x720 testsrc2 | +24.08% | **+1.51%** | -1.76% |
| 1280x720 natural (hevc fixture, 30 frames) | +60.59% | **+6.89%** | +7.86% |

## BD-rate per step

Metric: BD-rate, YUV (6:1:1) and Y, PSNR from ffmpeg's `psnr` filter, as `scripts/hevc-bdrate.sh`
computes it. Positive means ec-h265 needs more bytes than x265. Each row is cumulative (it includes
the kept steps above it). Every step was measured on all three clips.

| step | 352x288 testsrc2 YUV / Y | 720p testsrc2 YUV / Y | 720p natural YUV / Y | kept |
|---|---|---|---|---|
| 0. base `515a8b3e` | +21.85 / +21.35 | +24.08 / +23.26 | +60.59 / +64.04 | - |
| 1. intra CUs in P (RD vs inter) | +15.21 / +15.36 | +14.76 / +14.08 | +51.09 / +56.25 | yes |
| 2. seeds: above-right, above-left, co-located | +14.93 / +15.10 | +14.77 / +14.02 | +50.89 / +56.01 | yes (small) |
| 3. TMVP on (SPS and slice flags, collocated view) | +14.02 / +14.25 | +13.16 / +12.53 | +49.95 / +55.10 | yes |
| 4. 2NxN and Nx2N, RD | +12.92 / +13.10 | +12.62 / +11.93 | +50.05 / +55.23 | yes (natural +0.1, within noise) |
| 6. dead-zone quantiser for inter residuals (1/12) | -1.02 / -2.64 | +4.89 / +2.43 | +10.16 / +11.66 | yes (the big one) |
| 5. P pictures at QP +2 over the IDR | **-5.31 / -7.76** | **+1.51 / -1.76** | **+6.89 / +7.86** | yes |

Step 6 was measured before step 5 (see Deviations). Step 5 was measured on top of step 6.

### Dead-zone and QP experiments (natural clip only, YUV / Y)

| variant | natural YUV / Y |
|---|---|
| dead zone, round to nearest (offset 1/2, step 4 state) | +50.05 / +55.23 |
| dead zone offset 1/3 | +19.12 / +21.61 |
| dead zone offset 1/6 | +11.38 / +13.23 |
| dead zone offset 1/12 (kept) | +10.16 / +11.66 |
| dead zone 1/12, P QP -2 | +14.31 / +16.07 |
| dead zone 1/12, P QP +2 (kept) | +6.89 / +7.86 |
| dead zone 1/12, P QP +4 | +6.59 / +9.73 |

P QP +4 is slightly better on YUV but worse on Y, so +2 was kept.

### Raw points (final: `035f3544`, QP 22/27/32/37, 30 frames, GOP 8)

Columns: bytes, PSNR-Y, PSNR-YUV (6:1:1), from ffmpeg's psnr filter.

352x288 testsrc2, x265: (154513, 49.8417, 49.2919), (113645, 45.2243, 44.6377), (76984, 40.1013, 39.6810), (46648, 35.6417, 35.2367)
352x288 testsrc2, ec-h265: (130521, 46.2973, 45.5516), (85890, 41.8853, 41.1257), (51154, 37.9377, 37.2968), (30963, 34.5127, 34.0810)

1280x720 testsrc2, x265: (820596, 51.9377, 51.6526), (572921, 47.2910, 47.0053), (368626, 42.6370, 42.4692), (196936, 38.5903, 38.4146)
1280x720 testsrc2, ec-h265: (692196, 48.7747, 48.3022), (442064, 44.3323, 43.8084), (246773, 40.3307, 39.9487), (130130, 37.3730, 37.1128)

1280x720 natural, x265: (572115, 50.4463, 50.0530), (299480, 46.9153, 46.4709), (139360, 43.9720, 43.4984), (63633, 41.6310, 41.0723)
1280x720 natural, ec-h265: (412489, 47.9457, 47.5935), (192147, 44.8017, 44.3627), (86503, 42.4560, 41.9458), (45490, 40.5190, 40.0145)

## Encode speed (720p, natural clip, 30 frames, GOP 8, QP 30, one run each on host a)

| build | time for 30 frames | fps | bytes |
|---|---|---|---|
| base `515a8b3e` | 73.5 s | 0.41 | 355768 |
| final `035f3544` | 189.8 s | 0.16 | 123160 |

Intra-only (keyint 1) for the final build: 10.6 s for 30 frames (2.8 fps); bytes 389211.
The host was shared with other lanes (load about 6 on 4 cores), so absolute numbers are noisy,
but the ratio is not: the final build is about 2.6x slower on P pictures. The new work per coding
unit is the partition searches (two extra half-block searches at 16 and 32), seven intra candidates,
and the partition RD evaluation. I did not profile it. This is the main open cost.

## What changed (by file)

- `crates/ec-h265/src/enc_p.rs`
  - Intra CUs inside P pictures: the decoder's own `PicState::predict_intra` gives the prediction,
    the luma mode is the MPM list plus planar, DC, horizontal and vertical, and the syntax is
    written with `prev_intra_luma_pred_flag`, `mpm_idx`/`rem`, `intra_chroma_pred_mode` 4, and one
    transform level with split flag 0. The DST and intra scans are used for 4x4 and 8x8 luma.
  - Motion search seeds: zero, parent, left, above, above-right, above-left, and the previous picture's
    vector at the block centre.
  - TMVP: `slice_temporal_mvp_enabled_flag` is 1 in every P slice. The previous picture's motion is
    published as a `SharedPic` collocated view, and the decoder's temporal merge and AMVP candidates
    run on it.
  - Partitions: 2NxN and Nx2N for 16 and 32 CUs, with AMVP per half (predictor by shorter difference).
    The four half-size transform units of the inferred split are coded with their own cbf flags.
  - Dead-zone quantiser: `quantize_offset(..., 12)` (offset 1/12) in `code_tb`. This is shared with the
    intra CUs in P pictures.
  - P pictures at `P_QP_DELTA = +2` relative to the IDR's QP.
- `crates/ec-h265/src/enc_me.rs`: the search takes a `w` x `h` block, so it can search halves.
- `crates/ec-h265/src/dec/motion_field.rs`: new `MotionField::clear`. The encoder stores a
  partition's first half so that the second half's predictor sees it, then clears the block while the
  unit is being costed. Decoder behaviour is unchanged.
- `crates/ec-h265/src/encoder.rs`: the SPS sets `sps_temporal_mvp_enabled_flag` to 1.
- `crates/ec-h265/tests/rdoq_cache_hash.rs`: re-pinned `rdoq_cache_hash_is_pinned`. The intra output
  changed by that one SPS bit (the test pins whole-AU bytes). The new values are
  `0xd501713b57091906` (416x240) and `0x5eb50f5a747ca1c2` (1920x1080). The 9203-picture conformance
  check still matches, so the intra decode is unchanged. The comment on the test says why.

## Verify (final tree `dae6d079`, on host a)

- `scripts/hevc-gop-roundtrip.sh` (after the P QP change): exit 0. Every row decodes exact with both
  ec-h265 and ffmpeg. GOP 8 at QP 30 (after the P QP offset): 352x288 65097 bytes, 720p
  testsrc2 564064 -> 325759 bytes (natural 720p is not in that script), all exact.
- `cargo clippy -p ec-h265 -p ec-h265-syntax --all-targets -- -D warnings`: pass (clean).
- `cargo test -p ec-h265 -p ec-h265-syntax --release` with `EC_H265_REF_YUV_DIR=$HOME/refyuv/hevcinter`:
  pass, all 16 test binaries `ok`, exit 0. The conformance SEI-hash test reports
  `total: 9203 pictures checked, 9203 exact, 147/147 streams accepted`.

## Deviations from the brief

- Order: step 6 (dead zone) was implemented and measured before step 5 (QP offset). The dead zone
  was the largest lever on the natural clip, so it was tested first, and the QP offset was then tested
  on top of it. Step 5's lambda variant was not tested. Only the QP offset was.
- Step 6 is a dead-zone heuristic, not RDOQ. No trellis or rate-estimate quantiser was written.
- Step 3 TMVP uses only the temporal merge and AMVP candidates the decoder derives. The encoder's
  motion search uses the co-located vector as a seed too.
- Step 4 has AMVP-only halves (no merge candidates for the halves), only for 16 and 32 CUs, and no
  AMP.
- The `rdoq_cache_hash` re-pin is an intra-output change, from the SPS bit (see Files above).

## Left / not done

- Speed: P-picture encode is about 0.16 fps at 720p, down from 0.41. Profile the partition search and
  the intra candidates before anything else.
- Step 5 lambda tweak (`inter_lambda_scale`) on top of the dead zone, and a per-picture QP cascade
  beyond the single P offset.
- RDOQ proper (rate-estimated level decisions), and sign hiding for inter residuals (the `sh` option)
  on the final configuration. Not measured.
- Merge candidates for the halves of 2NxN/Nx2N, and AMP.
- The base lane's own numbers are reproduced here (base row above matches the old report).
