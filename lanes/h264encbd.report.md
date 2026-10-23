# lane h264encbd: ec-h264 encode BD-rate against x264 veryfast

Goal (DONE.md, "ec-h264 encode"): BD-rate vs x264 `-preset veryfast` (same features, no
B-frames) is <= +20%. The lane brief recorded the last measurement as +20% to +42%, citing
`lanes/h264x264-r2.report.md`.

Base sha: `bdc74c55` (branch `lane/h264encbd` was created from `main` at `d0c7390d`).

## Result

Stopped without an encoder change. The <= +20% target already holds on every clip I could
run with the gate's matched configuration, so no RD-tool attempt was needed or made.
I could NOT reproduce the +20% to +42% figure with the clips available here, and I could
not get the r2 screen-capture clip (2560x1440) at all: no file of that size exists on this
machine.

## Method

- Gate: `bd_psnr_vs_x264` in `crates/ec-h264/tests/encode.rs`. It encodes 24 consecutive
  pictures at QP 22/26/30/34 with our encoder and with x264 (`-profile:v main -bf 0 -refs 1
  -g 10`, pinned to `scenecut=0:aq-mode=0:psy=0:8x8dct=0:ipratio=1.0:pbratio=1.0:qcomp=1.0:chroma-qp-offset=0`).
  It prints bits and PSNR per QP.
- BD-rate: the gate's per-QP lines go through a small converter to a TSV
  (`bytes`, `psnr_y`, `psnr_yuv`), then `scripts/bdrate.py --metric yuv` (anchor = x264,
  test = ours). Positive = we spend more bits.
- New this lane: `EC_H264_X264_PRESET` selects the reference preset. Default stays `medium`
  so existing gates do not move. This lane measured with `veryfast`.
- Run on the servers via `scripts/remote-run.sh h264encbd --host auto`. Some runs fell back to
  local when every host slot was busy; the test and clippy results below came from that
  local fallback, and the first run that completed on a host is noted in the table.

## BD-rate table (before = after; no encoder change)

| clip | frames | reference | BD-rate (%) | BD-PSNR-YUV (dB) | BD-PSNR luma (dB) |
|---|---|---|---:|---:|---:|
| synthetic 352x288 | 24 | veryfast | -6.82 | +0.726 | +0.723 |
| fixture `fixtures/video/h264-1080p-23.976-8bit.mp4` | 24 | veryfast | +11.01 | -1.008 | -0.632 |
| film `~/Videos/reela.mp4` 3840x1608, skip 900 | 24 | veryfast | -12.90 | +1.072 | +1.991 |
| film, same clip | 24 | medium (gate default) | -5.32 | +0.425 | +1.259 |

Worst clip: the 1080p fixture at +11.01%, inside the +20% target.

Film without the skip (`EC_H264_CLIP_SKIP` unset, veryfast): +117.9%. The x264 curve is not
monotonic there (QP 34 reads 79 dB where QP 26 reads 72 dB), so that sample is dominated by
title and fade frames. The gate's flat-window check does not catch it. Skipping 900 pictures
(about 37 s; scene cuts run up to 36 s) fixes it. This is an instrument issue, not a
measurement of the encoder.

Bits per QP, film skip 900, veryfast (ours vs x264): QP22 -6.4%, QP26 -5.2%, QP30 -5.4%,
QP34 -5.8%. Positive numbers would be a premium.

## Fps

Not measured. The gate times the whole comparison (both encoders plus ffmpeg decode), not
our encode alone. The film run took about 165 s wall time, and that includes x264 and ffmpeg.
A separate encode-only timing is needed before any speed claim.

## Verify

- `cargo test --release -p ec-h264` (local fallback, worktree, after the `EC_H264_X264_PRESET` edit):
  81 passed, 0 failed (unit); 22 passed, 1 ignored; 15 passed, 1 ignored; 1 passed; 1 passed.
  `TESTS_RC=0`. The conformance tests are in these results and pass.
- `cargo clippy -p ec-h264 --all-targets -- -D warnings`: `CLIPPY_RC=0`.
- md5 compare of the output against ffmpeg's decode: not run. The encoder did not change, so
  its output is byte-identical to base. The gate already asserts that our reconstruction
  matches ffmpeg's decode within 0.05 dB on every run.

## Not done

- No encoder change. The three RD attempts in the brief were not needed under the target.
- The r2 numbers (+20% to +42%) are not reproduced. The likely cause is the reference
  (r2 measured `medium`, and my film result at `medium` is -5.3%), the clips, or the
  skip value. The screen-capture clip is missing.
- A reference with x264's default features (no pinned `psy`/`aq`/`scenecut`/`ipratio`) is
  not measured. The gate's pins are the earlier lanes' choice for matched operating points,
  and x264 with defaults may read differently. Worth measuring before anyone claims the
  target is met on the spec's own terms.
- `scripts/remote-run.sh` has uncommitted changes in this worktree (three slots, slot locking).
  They were not made by this lane and are not in this commit.
