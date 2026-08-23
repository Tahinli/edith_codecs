# lane survey r1 — which encoder in this repo is furthest from its reference

Measured 2026-08-23 on main (`f814c3e`..`4ebfd6a`), every row run inline in
this checkout; the omp seats were all quota-exhausted, so nothing here is a
worker's report of a measurement, it is the measurement.

The question the lane exists to answer: the opus lane has been running for
many rounds and its 14-row gate now reads parity-plus on every row, so where
does the NEXT fix lane aim? Ranked by distance from the reference, worst
first.

## Ranked table

| # | crate | metric vs reference | ours | reference | gap | evidence |
|---|-------|--------------------|------|-----------|-----|----------|
| 1 | ec-h264 | **no encoder-vs-reference gate exists** | — | — | unmeasured | `grep -n x264 crates/ec-h264/tests/conformance.rs` — x264 appears only as a source of streams for DECODER conformance |
| 2 | ec-vorbis | corr vs libvorbis, 14-row real-library sweep at 96/128k | 0.9876–0.9970 | 0.9865–0.9968 | worst row `nik@96` **+0.0027 to libvorbis**; 6 of 14 rows ours ahead, 14/14 PASS | `cargo test -p ec-vorbis --release --test oracle real_library_sweep_vs_reference -- --ignored --nocapture` |
| 3 | ec-opus | corr + opus_compare err_ratio, 14-row library gate at 64/96k | corr 0.9816–0.9954 | 0.9794–0.9948 | **all 14 rows ours ahead on corr**; err_ratio >1.10 on 2 rows (naz@64 2.697, naz@96 4.499) | `cargo test -p ec-opus --release --test conformance encoder_library_gate_vs_libopus -- --ignored --nocapture` (~12 min) |
| 4 | ec-mp3 | encoder corr vs incumbent bar | VBR 0.99801 | bar 0.99787 | **+0.00014 margin** — thinnest pass in the repo | `cargo test -p ec-mp3 --release` |
| 5 | ec-image | JPEG/WebP decode vs incumbent | worst `tiny.jpg` corr 0.999829 / 49.5 dB; WebP `lossy.webp` 36.06 dB | incumbent 36.09 dB | −0.03 dB on WebP, max per-sample delta 5 (JPEG) | `cargo test -p ec-image --release` |
| 6 | ec-aac | encoder worst-channel corr vs incumbent bar | mono 96k 1.0000, 5.1 384k 0.9999, HE core 24k 0.9909 | 0.9846 / 0.9648 | **ours ahead on every row** | `cargo test -p ec-aac --release` |
| 7 | ec-flac | encoded size vs ffmpeg, same input | 7 231 957 B | 7 528 465 B | **ratio 0.961, ours smaller**; 50/50 xiph vectors bit-exact | `cargo test -p ec-flac --release` |
| 8 | ec-alac | encode/decode vs ffmpeg | 8/8 fixtures bit-exact | — | **no gap** (lossless) | `cargo test -p ec-alac --release` |
| 9 | ec-ac3, ec-h265, ec-truehd, ec-matroska | suite gates | all green | — | no reference gap surfaced | `cargo test -p <crate> --release` |

Every default suite in the tree is green, zero failures, zero unexplained
skips: ec-vorbis 21 passed / 3 ignored (the ignored three are the
`--ignored` reference sweeps, one of which is row 2 above), ec-h264 108
passed / 1 ignored (`real_clip_t8x8_bd_psnr`, needs `EC_H264_CLIP`), ec-h265
35, ec-image 40, ec-truehd 16, ec-aac 16 (sbr_real_library) + the rest.

## The worst two, in mechanism terms

**1. ec-h264 has no rate-quality reference at all.** Every other encoder in
this repo is measured against the thing it replaces; the H.264 encoder is
measured against ITSELF (`8x8 BD-PSNR over q22/26/30/34: +0.540 dB`) and
against absolute PSNR floors (`library clip 2560x1440: 12 pictures, 4300
kbit/s asked 8000, luma PSNR 54.68 dB`). x264 is already invoked in
`conformance.rs` to *produce* streams, so the encoder half is one test away:
encode the same clip at matched rate with both, report BD-PSNR. Until that
exists, "our H.264 encoder is N% off x264" has no answer, and the
`transform_8x8` default-on debt is being decided without one. This ranks
first because an unmeasured gap is worse than a measured small one.

**2. ec-vorbis trails libvorbis by 0.0027 corr at its worst row** (`nik@96`),
where ec-opus now leads libopus on all 14 of its rows. The two lanes share a
source list and a table format, so the shapes are comparable: vorbis's losing
rows (`nik`, `zaur`, `dl8a`) are the same sources whose rate rows sit
*under* the reference's kbps (nik@96 −1.60%, zaur@96 −1.43%, dl8a@96
−0.64%), i.e. we are spending fewer bits and paying for it, while every row
where we spend MORE (sadie +2.74%, hein +2.05%) we win. That is the
rate-loop-windup class in [[vorbis-rate-loop-windup]] read from the other
side: not a transient debt spike, a steady under-spend. The next vorbis lane
should measure per-frame spend against libvorbis on `nik@96` before touching
psy.

## What could not run, and why

- **Nothing was skipped for missing fixtures.** The one apparent silent skip
  (`SKIP: ~/.cache/heaac-fixtures/... absent` in ec-aac) was the survey
  worktree lacking a `.cache` symlink, not a missing fixture; re-run in the
  main checkout the SBR suite takes 42.96 s instead of 9.41 s with every
  fixture present. Same class as the known `fixtures` symlink rule.
- **Two ec-aac SBR anchors are env-gated** and skip by default:
  `sbr_instrument_anchor_controls` (`EC_AAC_SBR_INSTRUMENT_ANCHOR=1`) and
  `sbr_actual_noise_fraction` (`EC_AAC_SBR_NOISE_ANCHOR=1`). Both were run
  for this survey. The gate is legitimate — the second one sets process env
  vars mid-run, which is unsound alongside parallel tests — but being
  invisible to default runs let a stale instrument sit unread for rounds
  (fixed in `4ebfd6a`; its SKIP lines now print the exact command).
- **`real_clip_t8x8_bd_psnr`** needs `EC_H264_CLIP` pointed at a media file;
  its last measured value (+0.827 dB BD on natural 720p, 0.000 on obs 1440p)
  is in `lanes/h264rd-r2.report.md` and is the open `transform_8x8` debt.
- **ec-opus's 14-row gate takes ~12 min**, so its numbers here are the
  re-measurement taken at `41cf1d1` earlier this batch, not a fresh run.

## Instrument defects found and fixed while surveying

Both were found because a green suite printed a number that could not be
true, which is the only reason to read a passing suite's output at all.

1. `core_only_matches_reference` printed `corr 0.402349` for a pair that
   actually correlates 0.999845 — the coarse lag search stepped by 11 and the
   true peak is one sample wide. Fixed in `f814c3e`; class swept, the sibling
   copies in three other test files were already stride-1.
2. `sbr_actual_noise_fraction` asserted a "whole-HF" noise fraction that had
   silently started summing a dozen core-decoded bands carrying 10x the
   energy of the SBR bands, reading 0.0397 against a 0.17 floor. Nothing
   regressed: HF-only it reads 0.1774 against round-44's 0.1770. Fixed in
   `4ebfd6a`.
