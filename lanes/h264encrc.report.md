# lane h264encrc: ec-h264 encode (recon match, rate control, speed)

Base: `d0c7390d` (main). Worktree `~/.cache/wt/h264encrc`, branch `lane/h264encrc`.

## Goal and result

| DONE.md item | Result |
|---|---|
| Output decodes in ffmpeg identically to the encoder's reconstruction | **Met.** 28/28 md5 matches (7 sizes x 2 sources x 2 modes). |
| Rate control within +-5% over 60 s | **Met on 6/6 targets**, but 1080p at 1 Mb/s is at +4.83%, close to the edge. |
| 1080p30 realtime with threads | **Not met.** 1.86 fps at 4 threads, 1.78 at 6 threads on a shared server (target 30). |

## Changes

- `crates/ec-h264/src/enc/rc.rs`: the leaky-bucket correction was clamped to +-2 QP, and
  the bucket's upper bound was 8 base. Now the gain is 1 QP per picture-budget of debt
  clamped to +-6, the bucket upper bound is 16 base, and an IDR's complexity is taken as
  measured (no smoothing). Effect on 1080p 1 Mb/s: +7.35% to +4.83%. The gain change
  alone and the IDR change alone were each within 0.05% of each other, so the improvement
  comes from the clamp and bucket bounds.
- `crates/ec-h264/src/enc/mod.rs`, `timing.rs` (new), `lib.rs`: per-stage timers
  (`Encoder::timings()`, `EncodeTimings`): pad, band wall/busy/slowest, stitch, deblock,
  output.
- `crates/ec-h264/src/enc/mb.rs`, `cabac_enc.rs`, `entropy.rs`: `entropy_bits` now probes
  a copy of the coder state with an empty output buffer at the same bit alignment, instead
  of cloning the whole slice writer for every candidate. Output is byte-identical (checked
  on a 2 s 1080p clip: 1272540 bytes before and after). The speed gain was negligible.
- `crates/ec-h264/Cargo.toml`: feature `enc-profile` (default off) compiles per-stage
  counters in `mb.rs` (`mb_timed!`). It costs nothing when off.
- `crates/ec-h264/examples/h264_clip_encode.rs` (new): encodes raw I420 (file or stdin),
  writes the Annex B stream and the reconstruction, prints fps and stage times.
  `H264_TRACE=1` prints per-picture QP and size.
- `scripts/h264-lane-check.sh` (new): `recon`, `rate [size:bitrate:kind ...]`, `fps` modes.
  Sources are ffmpeg `testsrc2`, with `noise=alls=24:allf=t` for the noisy case. No real
  camera clip exists under `fixtures/` (every video fixture is generated from testsrc2).

Not committed: `scripts/remote-run.sh` shows as modified in this worktree (someone else's
change to the shared script: three slots per host and a per-lane sync lock). I did not
touch it, so it is left out of the commit.

## Verify

Commands, all run through `scripts/remote-run.sh h264encrc --host auto` (hosts a, b, c):

- `scripts/h264-lane-check.sh recon` (host a): RECON_EXIT=0, 28 MATCH, 0 MISMATCH.
- `cargo test --release -p ec-h264` (host a): TEST_EXIT=0. Results: 81 passed; 22 passed,
  1 ignored; 15 passed, 1 ignored; 1 passed; 1 passed (doctests). Ignored counts are
  unchanged from before.
- `cargo clippy -p ec-h264 --all-targets -- -D warnings` (host a): CLIPPY_EXIT=0.

### Recon match (md5 of encoder reconstruction vs md5 of `ffmpeg -i x.h264` decode)

30 frames per case, 4 threads. Each size was run as CQP (QP 30) and as ABR (2 Mb/s).
The encoder refuses odd dimensions (it needs even), so "odd" here means sizes that are
not macroblock multiples and odd macroblock counts.

| size | sources | result |
|---|---|---|
| 176x144, 64x48, 350x242, 210x118, 352x288, 1280x720, 1920x1080 | testsrc2, noisy | 28/28 MATCH |

The md5 values are in the script's output (`~/h264encrc-recon.log` on the server).

### Rate control (60 s, 30 fps, 4 threads, noisy testsrc2)

| size | target | achieved (b/s) | error | |
|---|---|---|---|---|
| 1280x720 | 1 Mb/s | 1 015 577 | +1.56% | PASS |
| 1280x720 | 2 Mb/s | 2 001 360 | +0.07% | PASS |
| 1280x720 | 4 Mb/s | 3 999 692 | -0.01% | PASS |
| 1920x1080 | 1 Mb/s | 1 047 914 (run 2: 1 048 284) | +4.79% / +4.83% | PASS (marginal) |
| 1920x1080 | 4 Mb/s | 4 004 711 | +0.12% | PASS |
| 1920x1080 | 8 Mb/s | 7 999 755 | -0.00% | PASS |

Before the rc change (original code, same clips): 1080p 1 Mb/s +7.35% (FAIL), 1080p
4 Mb/s +1.00%, 1080p 8 Mb/s +0.15%, 720p 1 Mb/s +2.95%. The 720p 2 and 4 Mb/s baseline
runs did not complete (host c was busy for 10 min).

The 1080p 1 Mb/s trace (before the change) showed P pictures at about QP 41-44 and under
their budget, and IDR pictures well over theirs, with IDR QPs stepping up by the full
clamp every GOP. I did not confirm the mechanism after the change, so the residual
+4.8% at 1 Mb/s is not explained. Next step: trace IDR target against actual bits per
GOP at 1 Mb/s.

### Speed (1080p30 noisy testsrc2, 10 s, 4 Mb/s, one server shared with other lanes)

| threads | fps |
|---|---|
| 1 | 0.89 |
| 2 | 1.43 |
| 4 | 1.86 |
| 6 | 1.78 |

Earlier run on host c with the original code: 0.94 / 1.51 / 2.17 / 2.31. Load averages on
the hosts were 2 to 5 on 4 cores, so the numbers are low.

Stage breakdown (1 thread, 2 s 1080p, `enc-profile`), share of encode time:

| stage | seconds | share |
|---|---|---|
| intra RD, total (IDR and P-picture intra candidates) | 36.4 | 56% |
| of which intra 8x8 RD | 15.6 | 24% |
| of which intra 4x4 RD | 8.6 | 13% |
| of which chroma mode RD | 4.5 | 7% |
| of which intra 16x16 RD | 1.2 | 2% |
| inter RD candidates (`encode_inter_mb` loop) | 11.7 | 18% |
| `choose_inter` (motion and shape search) | 13.9 | 21% |
| `intra_pre_cost` | 1.3 | 2% |
| state save/load, entropy clone | 0.8 | 1% |
| deblock, pad, stitch, output | about 0.4 | <1% |

Serial stages (deblock, stitch, pad, output) are under 1% of the time. The bands are
86 to 91% balanced at 2 to 6 threads. The CPU sum does grow with thread count: 333 s at 1
thread, 377 s at 2, 546 s at 4 and 881 s at 6 over the same 300 frames. Part of that is
slice boundaries and part is the shared host, which I did not separate. Scaling is
therefore about 2.1x at 4 threads, not 4x. The per-core cost is the larger limit: about
1.1 s of CPU per 1080p frame at 1 thread, against about 0.03 s per frame that 30 fps on 4
cores would allow. Every P macroblock is
coded both as intra and as inter, and the intra branch alone is over half the time. That
is mode-decision code, which the h264encbd lane is working in. Cutting it would change
the bitstream and its BD-rate.

## What is left

- Speed: 30 fps at 1080p on 4 cores needs roughly 8x per core. The mode-decision cost
  above is where it is. The remaining bit-exact work is small. Thread scaling is also
  short of 4x (see above).
- Rate: 1080p 1 Mb/s is at +4.83%, which is inside the bound but not by much. The IDR
  overshoot mechanism needs confirming with a trace.
- The `enc-profile` counters stay in the tree behind a default-off feature.
