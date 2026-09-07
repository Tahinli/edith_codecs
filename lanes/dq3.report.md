# lane-dq3 -- per-superblock delta_q shipped at presets 5 and 6, gated off screen

## Change

* `crates/ec-av1/src/encode.rs` (`encode_inter_frame`, the `deltaq_res` binding):
  `.filter(|_| !screen)` -- a frame that set `allow_screen_content_tools` codes
  no delta syntax at all, the same content gate `b64_residual` / `b64_compound`
  take. This is what makes the capture row byte-identical, at any preset and
  even under `EC_AV1_DELTAQ=2`.
* `crates/ec-av1/src/speed.rs`: `DELTAQ_RES = [4,4,4,4,4,2,2,4,4,4,4]` --
  res 4 (`delta_q_res` log2 = 2) at presets 5 and 6, off everywhere else.

## Measurement -- 12-frame `bd_rate_screen_native`, BD vs libaom / vs rav1e

Control = same binary with `EC_AV1_DELTAQ=0`; arm = delta_q on.

| preset | film A off -> on | film B off -> on | verdict |
|---|---|---|---|
| 6 | +27.2/-0.2 -> +26.6/-0.4 | +35.2/+6.6 -> +34.7/+6.4 | SHIP (both films, both columns) |
| 5 | +26.6/-0.7 -> +25.8/-1.1 | +33.3/+4.9 -> +32.8/+4.6 | SHIP (both films, both columns) |
| 4 | +23.7/-2.9 -> +23.1/-3.1 | +29.9/+2.0 -> +30.2/+2.6 | REJECT (film B worse on both) |
| 3 | (lane-dq2) | film B worse on both | REJECT |
| 0 | (lane-deltaq) neutral | | stays off |

Preset 6 reproduces lane-dq2's numbers to the digit on both films.

### Screen capture, preset 6 -- BYTE-IDENTICAL

| arm | ours PSNR/bytes per point | vs libaom | vs rav1e |
|---|---|---|---|
| control | 45.40/37996, 48.11/46966, 50.55/58908, 52.93/74047 | +26.0% | -27.2% |
| shipped | 45.40/37996, 48.11/46966, 50.55/58908, 52.93/74047 | +26.0% | -27.2% |

All four q points code the identical byte count; the row prints
`screen frames on=48 off=0`, so the gate is what is doing it.

The two `testsrc2` bars rows are fixtures, not decisions (they move: 1080p
+68.4 -> +67.0, 2160p +62.7 -> +64.1).

## Invariants (release binary)

* `EC_COMP_MISMATCH=1 --include-ignored identically_with_one_and_four`: 3 passed
  (decode / recon / filter thread determinism).
* `EC_COMP_MISMATCH=1 the_facade_codes_the_same_bytes_as_encode_sequence`: 1 passed.
* `--ignored every_speed_preset_decodes_sample_exact_through_both_decoders`: 1 passed.
* Bitrate pins unchanged (8562 / 33357), preset 0 untouched -- green inside the suite.
* `cargo test -p ec-av1 --release`: **568 passed, 0 failed, 45 ignored**.
* `cargo check --workspace --all-targets -j4`: 0 errors, 0 ec-av1 warnings.

## Logs

`$HOME/.cache/dq3/{p6-ship,p6-ctl,p5-on,p5-ctl,p4-on,p4-ctl,inv,suite}.log`.
