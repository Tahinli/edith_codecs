# lane hevcencspeed3: ec-h265 P-picture speed, fast defaults

Base: `fbfe7c9e` (main). Branch `lane/hevcencspeed3`, worktree `~/.cache/wt/hevcencspeed3`.

## Result

**Not met.** The DONE.md criterion (1080p30 at >= realtime on <= 6 threads) is not reached.
1080p30 runs at 6.90 fps locally (0.23x realtime, 6 threads) and at 2.45 fps on server b (4 vCPU).
The lane got about 1.24x locally and 1.30x on server b at 6 threads, and about 1.35x single-thread.

The P path is decided per CU with many full RD candidates (5 merge, 2 AMVP, up to 7 intra modes,
each with transform, quantise and CABAC estimate), plus a full integer walk. The lane cut the
obvious waste but did not profile the rest (no perf on the server, and no stage timers were added).

## Changes (`crates/ec-h265/src/enc_p.rs`, `enc_me.rs`)

There is no separate Fast preset in ec-h265. The new fast-path decisions are the P-path defaults,
and every threshold can be overridden by an `EC_H265_*` env var (read once per process) for the sweep.
The I-picture path (`ctu.rs`, `encode_idr`) is not touched.

1. Integer scan gate: `EC_H265_FULL_GATE`, default 2.0 per sample. When the seeds already cost less
   than 2 per sample, the 33x33 (or 17x17) whole-sample walk is skipped.
2. Merge early termination: the merge loop stops at the first residual-free candidate below
   `EC_H265_SKIP_COST` (2.0 per sample). Before this, the early skip only gated AMVP and intra.
3. Intra preselection: `EC_H265_INTRA_K`, default 2. The 7 intra modes are ranked by luma SAD of
   their prediction, and only the best 2 get full RD.
4. Intra gate: `EC_H265_INTRA_GATE`, default 0 (off). Skips intra when the inter best is below it.
5. Existing thresholds made tunable: `EC_H265_FULL_LARGE` (16), `EC_H265_FULL_SMALL` (8),
   `EC_H265_SPLIT_SAD` (0.25). Their defaults equal the old constants.

Not done: a transform-tree depth restriction (none measured), RDOQ changes (already on, not
touched), and a separate fast preset.

## Sweeps (host b, 1080p, 30 frames, 1 thread, QP 30, GOP 8)

| setting | secs | bytes |
|---|---|---|
| base (fbfe7c9e) | 38.14 | 657003 |
| FULL_LARGE=8 | 33.76 | 668761 |
| FULL_LARGE=4, FULL_SMALL=4 | 30.81 | 686770 |
| SPLIT_SAD=0.5 | 36.89 | 657510 |
| SKIP_COST=4 | 38.01 | 655331 |
| FULL_GATE=0.5 | 33.46 | 657445 |
| FULL_GATE=1 | 32.34 | 658065 |
| FULL_GATE=2 | 32.25 | 658645 |
| FULL_GATE=4 | 32.20 | 666167 |
| FULL_GATE=2, INTRA_K=2 | 30.76 | 661200 |
| FULL_GATE=2, INTRA_K=3 | 32.35 | 658765 |
| FULL_GATE=2, INTRA_GATE=1 | 32.39 | 658956 |
| FULL_GATE=2, INTRA_GATE=2 | 31.97 | 658961 |
| merge early termination only | 34.02 | 654982 |
| merge early termination + FULL_GATE=2 + INTRA_K=2 (final defaults) | 28.42 | 655389 |

## Speed (hevc_gop via hevc-thread-bench, 1080p, 30 frames, QP 30, GOP 8)

| host | threads | before fps | after fps | after realtime x |
|---|---|---|---|---|
| b (server, 4 vCPU) | 1 | 0.79 | 1.07 | 0.036 |
| b | 4 | - | 2.41 | 0.080 |
| b | 6 | 1.88 | 2.45 | 0.082 |
| local | 1 | 1.92 | 2.50 | 0.083 |
| local | 6 | 5.57 | 6.90 | 0.230 |

Local "before" is main `fbfe7c9e` built in a temporary worktree (since removed). The server 1-thread
"before" is the same encode in the same clip and command (38.14 s, the sweep base).

## BD-rate vs libx265 veryfast, bframes=0, keyint 8 (`scripts/hevc-bdrate.sh`, QP 22/27/32/37, host b)

"Before" is the recorded final of `lanes/hevcenctools.report.md` (same script and clips; not re-run
this session). "After" is this lane's final defaults.

| clip | before YUV / Y | after YUV / Y | limit |
|---|---|---|---|
| 352x288 testsrc2 | -5.31 / -7.76 | -2.88 / -5.43 | <= +25 |
| 1280x720 testsrc2 | +1.51 / -1.76 | +4.99 / +1.45 | <= +25 |
| 1280x720 natural (`nat-1280x720.yuv`) | +6.89 / +7.86 | +11.19 / +12.17 | <= +25 |

Intra-only (GOP 1, 352x288, same script): BD-rate -29.48% YUV, BD-PSNR +7.17 dB YUV (limit >= -1.0 dB).
The intra path is not changed, so this should match main.

## Recon match (`scripts/hevc-gop-roundtrip.sh`, host b, 30 frames, QP 30)

Every row is `exact` for both ec_h265_decode and ffmpeg_decode against the encoder reconstruction:
352x288 GOP 1 and 8, 1280x720 GOP 1 and 8, 350x286 GOP 1 and 8 (6 rows, all exact).

## Rate control (`scripts/hevc-rc.sh`, host b, 60 s 1280x720 testsrc2, keyint 30)

| target kbps | achieved kbps | error | psnr_y | ec decode | ffmpeg decode |
|---|---|---|---|---|---|
| 1000 | 1000.1 | +0.01% | 38.25 | match | match |
| 2500 | 2500.4 | +0.02% | 42.01 | match | match |
| 5000 | 4999.5 | -0.01% | 47.09 | match | match |

All within +-5%.

## Verify

- `cargo test --release -p ec-h265` with `EC_H265_REF_YUV_DIR=$HOME/refyuv/hevcinter`, host b: exit 0,
  every test binary `ok`, 0 failed (`TESTS_EXIT=0`).
- `cargo clippy -p ec-h265 --all-targets -- -D warnings`, host b: exit 0.
- Conformance, host a: `cargo test --release -p ec-h265 --test conformance_intra -- --nocapture` with
  `EC_H265_REF_YUV_DIR=$HOME/refyuv/hevcinter`:
  `total: 9203 pictures checked, 9203 exact, 147/147 streams accepted` (exit 0).
- `enc_p.rs` is 1970 lines. Every file stays under 5000 lines.
- Local `cargo build` ran twice, for the compile check after the intra knobs and for the local timing
  (`CARGO_TARGET_DIR=~/.cache/tgt-hevcencspeed3`, 6 threads). That broke the rule of building on the server.
  Everything else built on host b or host a.

## What is left

- 1080p30 realtime at 6 threads is not reached: about 7 fps local and 2.5 fps on the server. A
  per-candidate profile is needed first. The next levers are reducing the 5 merge + 2 AMVP
  candidates to a SAD-ranked short list, and avoiding the per-candidate context clone and CABAC
  counter in `eval_candidate`.
- BD-rate cost of the fast defaults: about +2.4 points on the 720p natural clip (+6.89 to +11.19 YUV)
  and +3.5 on the 720p testsrc2 clip. The gate and intra preselection were the main sources.
- Not checked: the 4-vCPU server scaling beyond 4 threads (6 threads is oversubscribed there).
