# lane/h265rcfloor: does Bitrate mode undershoot on near-black?

Question: an edith HEVC GOP export at `RateControl::Bitrate { kbps: 2000, fps }` of near-black
test footage averaged 73 kb/s. Is that a model bug (QP cannot descend), or is the content too
simple to spend the rate at QP 0?

## Method

`crates/ec-h265/examples/hevc_rc.rs encode` at 1280x720, 30 fps, keyint 30, 150 frames (5 s),
raw yuv420p sources made with ffmpeg on the server (host c). The example now also prints
`qp_min` and `qp_max` next to `mean_qp`. Kbps is the coded stream size over 5 s.

Sources:
- moving: `testsrc2`
- near-black noise: `color=c=0x101010` plus `noise=alls=2:allf=t`
- near-black flat: `color=c=0x101010` with no noise
- mandelbrot: `mandelbrot` (high detail)

## Measurements (baseline code, before this lane's test was added)

| Source | Target kbps | Achieved kbps | Error | QP min..max (mean) |
|---|---|---|---|---|
| moving (testsrc2) | 500 | 511.5 | +2.3% | 27..47 (42.2) |
| moving (testsrc2) | 2000 | 1987.5 | -0.6% | 22..33 (32.2) |
| moving (testsrc2) | 6000 | 5936.3 | -1.1% | 20..27 (21.3) |
| near-black, noise | 500 | 570.9 | +14.2% | 10..47 (24.5) |
| near-black, noise | 2000 | 2162.6 | +8.1% | 5..29 (15.0) |
| near-black, noise | 6000 | 5886.1 | -1.9% | 3..27 (11.5) |
| near-black, flat | 500 | 40.5 | -91.9% | 0..27 (0.83) |
| near-black, flat | 2000 | 40.5 | -98.0% | 0..27 (0.83) |
| near-black, flat | 6000 | 40.5 | -99.3% | 0..27 (0.81) |
| mandelbrot | 500 | 527.4 | +5.5% | 27..51 (43.0) |
| mandelbrot | 2000 | 2049.9 | +2.5% | 23..39 (35.6) |
| mandelbrot | 6000 | 5976.9 | -0.4% | 15..33 (28.9) |

The QP max of 27 on the flat rows is the first IDR, which is coded before the model has a
measurement. Every later picture of the flat clip sits at QP 0 (mean 0.83 over 150 pictures).

## Conclusion

The undershoot is content-driven, not a model bug.

- Flat near-black: the model does descend. The QP reaches the HEVC floor of 0 within about
  eight predicted pictures (four QP steps per picture, plus the bucket term) and stays there.
  At QP 0 the clip still costs about 40 kb/s, which is the skip and header overhead of 150
  pictures of nothing to code. The output is identical at 500, 2000 and 6000 kbps. No setting
  of this encoder can spend 2 Mb/s on it, so QP 0 is the correct end state. The 73 kb/s the
  teammate saw is the same regime (their footage was not available, and our flat clip gives
  40.5 kb/s).
- No QP floor was added. The only floor is QP 0, which is the codec's own minimum. Adding a
  quality floor above QP 0 would deliberately undershoot, so it was not done.
- Moving content and mandelbrot stay within +/-5% at all three targets except mandelbrot at
  500 kb/s (+5.5%), which sits just outside that bound. This is an overshoot at a rate the
  content is too complex to hit (QP is already at 51, the ceiling).

Separate finding, not fixed here: the noisy near-black clip overshoots at 500 kb/s (+14%) and
2000 kb/s (+8%). Its QP hunts over a wide range (5..47 at 500 kb/s, 5..29 at 2000 kb/s). That
is an overshoot, not the reported undershoot, and the 5% bound is not claimed for it. It is a
candidate for a follow-up lane.

## Diff

- `crates/ec-h265/examples/hevc_rc.rs`: the encode summary prints `qp_min` and `qp_max`.
- `crates/ec-h265/tests/rate_control.rs`: new test
  `flat_near_black_descends_to_the_qp_floor`. A 320x240 flat clip of 60 pictures at 2000 kbps
  must code every predicted picture from picture 10 on at QP 0. Helpers `flat_clip` and
  `encode_pictures` are added. It runs in the same fast tier as the existing tests.
- `crates/ec-h265/src/enc_rc.rs`: unchanged. No bug was found in the model.

## Results

Run on the server (host c), `cargo clippy -p ec-h265 --all-targets -- -D warnings` passed. Then
`cargo test -p ec-h265 --release` passed: all 18 test binaries report `ok` with 0 failures
(ec_h265 unit tests 89, conformance 15, conformance_intra 2, cu_syntax 4, dec_shell 13, encode 4,
encode_threads 3, perf 3, rate_control 3 including the new test, and the rest). The
`rate_control` binary alone was also run and passed (new test plus both existing tests).
The `perf` benchmarks ran for about 5 minutes on the loaded host; they pass.

Scratch measurements were run from `$TMPDIR` on the server and were removed from the lane
afterwards with `remote-run --clean`.
