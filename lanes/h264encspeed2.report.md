# lane h264encspeed2: ec-h264 Fast preset speed, second round

Base: `a449b7fb` (main). Worktree `~/.cache/wt/h264encspeed2`, branch `lane/h264encspeed2`.

## Result

DONE.md "1080p30 encodes at >= realtime with threads" (`Preset::Fast`): **met on the local 12-core
machine**: 41.7 fps (1.39x realtime) at 6 threads on the hard noisy testsrc2 clip, 93.7 fps on
plain testsrc2. Not met on the 4-vCPU server (8.5 fps at 6 threads, host load 3 to 5, and the server core
is about 2.2x slower than the local one single-threaded).

| Criterion | Result |
|---|---|
| speed, local, 6 threads, noisy 1080p30 | 41.72 fps, 1.39x |
| BD-rate vs x264 veryfast (h264encbd method, 1080p library clip) | +12.46% (was +10.57%), limit 20 |
| `scripts/h264-lane-check.sh recon` | 28 MATCH, 0 MISMATCH |
| rate control | 6/6 within 5% (table below) |
| `cargo test --release -p ec-h264 --no-fail-fast` | all pass (81; 22 + 1 ignored; 15 + 1 ignored; 4; 1; 11; 1) |
| `cargo clippy -p ec-h264 --all-targets -- -D warnings` | clean |
| `real_library_frames_encode_and_decode_exactly` with `EC_REQUIRE_FIXTURES=1`, local | pass |

## Where the time went (60 pictures, 1 thread, host a, `enc-profile`)

Before: `choose_inter` 56% (sub-sample refinement alone 4.4 of 6.0 s per 30 pictures), inter
candidate coding 32%, and on the clean clip Intra_8x8/4x4 on P macroblocks. `perf` locally then
showed `satd4`, `mc_luma`, `code_luma_4x4` at the top.

## Changes (all in `crates/ec-h264/src/enc/`, plus scripts)

All are `Preset::Fast` only; Balanced is unchanged. Defaults live in `FastKnobs` (`mb.rs`), with
`EC_H264_*` sweep overrides in `fast_knobs()` (`mod.rs`).

1. Motion search: duplicate seeds are skipped, the whole-sample walk opens at 2 samples and is
   capped at 3 passes, and sub-sample refinement is half-sample only: four axis neighbours plus the
   one diagonal the two winning axes point at, ranked on SAD (`me_sub=1`, `me_satd=false`). The final
   cost is still SATD so the skip and intra comparisons keep their scale. The motion-vector rate
   multiplier is computed once per macroblock.
2. Intra_8x8 is skipped in P pictures (`p_skip_i8`); I pictures keep it. BD-neutral (13.54 -> 13.59),
   saves 10 s of 33 on the clean clip.
3. Inter transform size: the 8x8 trial is made only when the 4x4 coding left 1 to 16 levels
   (`inter_tx=3`, `tx8_nz_limit=16`). The existing test `ffmpeg_decodes_bit_exactly` requires inter
   8x8 macroblocks to be coded, so 4x4-only was not kept. 4x4-only was +0.35 BD worse.
4. The Intra_16x16 pre-cost in P pictures tries DC first. When the inter SATD already passes the
   gate against the DC SATD (an upper bound of the best mode) the other three modes are not costed
   (`lazy_i16`). Gated macroblocks are coded straight into the picture and writer: no state
   save/restore, no entropy clone, no extra `ssd_mb`.
5. Early P_Skip (`early_skip=8`): when the prediction at the P_Skip vector has a small SAD, a small
   largest difference and a small chroma SATD against the quantiser step, the macroblock is coded
   as P_Skip with no search and no residual (`InterChoice::force_skip`). 20 was too aggressive
   (BD +18.6%).
6. `satd4` rewritten on i16 rows with slice windows. Small gain.
7. `enc-profile` gets four more counters (`me_seeds`, `me_int`, `me_sub`, `me_chroma_skip`
   unused), so 16 slots.
8. Scripts: `scripts/h264-speed.sh` (speed on cached noisy and clean clips), `scripts/h264-bd.sh`
   (the h264encbd BD-rate), `scripts/h264-sweep.sh` (both for one env setting).

No `std::arch` SIMD: the crate has `#![forbid(unsafe_code)]` and the targets were reached without it.

## Sweeps (host a, 60 pictures, 1 thread, BD vs x264 veryfast)

| setting | noisy s | clean s | BD-rate % |
|---|---|---|---|
| start (cap + step 2, qpel on, SATD) | 23.8 | 37.4 | 11.76 |
| half-sample only, SATD | 19.0 | 33.8 | 13.73 |
| half-sample only, SAD | 17.7 | 32.8 | 13.54 |
| no sub-sample | 13.2 | 28.8 | 15.04 |
| + skip Intra_8x8 in P | 17.0 | 23.4 | 13.59 |
| + 4x4-only inter | 13.5 | 20.3 | 13.94 |
| + axis+diagonal half-sample | 11.3 | 17.8 | 14.05 |
| + lazy Intra_16x16 gate | 9.2 | 13.6 | 11.88 |
| + early skip 8 | 8.3 | 3.4 | 12.82 |
| + 8x8 trial only below 16 levels (final) | 9.1 | 3.8 | 12.46 |

## Verify

Everything through `scripts/remote-run.sh h264encspeed2` unless noted.

- BD: `scripts/h264-bd.sh` (host a): bd_rate_pct=12.46, bd_psnr_db=-1.183.
- Recon: `scripts/h264-lane-check.sh recon` (host c): 28 MATCH, 0 MISMATCH.
- Rate (`h264-lane-check.sh rate`, host c): 720p 1 Mb/s +1.59%, 2 Mb/s +0.08%, 4 Mb/s +0.01%;
  1080p 1 Mb/s +4.84% (marginal, was +4.89%), 4 Mb/s +0.12%, 8 Mb/s +0.01%.
- Tests and clippy on host b: see Result.
- Speed: `scripts/h264-lane-check.sh fps`, host a and locally (`CARGO_TARGET_DIR=~/.cache/tgt-h264encspeed2`).
  Noisy 10 s: server 5.58 / 7.31 / 8.45 / 8.54 fps at 1/2/4/6 threads (was 2.07 / 2.68 / 2.74 / 3.05);
  local 12.33 / 18.16 / 34.83 / 41.72. Rows in `lanes/bench.md`.

## What is left

- server numbers stay far below 30 fps; the server is shared and slower. The local machine clears it.
- Bands are equal row ranges, so a content-heavy band sets the wall time. Not touched.
- A transposed SIMD SATD, an SAD-ranked integer search with hex pattern and sub-sample plane reuse
  are possible further gains, not needed for the item.
