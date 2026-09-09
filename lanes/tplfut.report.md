# lane-tplfut: the top ARF's temporal lambda window

## 1. What the tpl window MEANS on each path

`encode_inter_frame`'s `lookahead` argument (encode.rs:12333) is turned into
`frames = [current, w0, w1, ...]` and `tpl_lambda_factors` (encode.rs:11785)
runs one coarse motion pass per ADJACENT pair, `tpl_coarse_pass(frames[f-1],
frames[f])`, then propagates `mc_dep` from the far end back to `frames[0]`.
So the window is ordered NEAREST-FIRST and the propagation assumes each entry
is one display step further from the current frame along a continuous chain.

* Flat path (encode.rs:13783): `rest[i+1..].take(tpl_depth()-1)` -- strictly
  display-FUTURE, nearest first. Forward in time, like libaom.
* Pyramid leaves and the mid/quarter ARFs (`code_group`'s `tpl_window(pos,
  false)`): the group's own display-order successors. Also forward.
* The TOP ARF: `tpl_window(group.len()-1, true)` = `group[..pos].iter().rev()`
  -- the group's own sources, which are all display-PAST of it (the top ARF is
  its group's LAST picture). Nearest-first is still an adjacent chain, so the
  propagation is well-formed, but it runs BACKWARD in time: the window is
  TIME-REVERSED relative to the flat path and to libaom's tpl at an ARF, which
  looks forward over the next group. That is the premise this lane tested, and
  it is confirmed in code.

## 2. The arm

`speed::TPL_FUT` / `EC_AV1_TPL_FUT=<n>`, read in `Av1Encoder::code_group`
(encoder.rs), applying to the TOP ARF only; falls back to mode 0's window
whenever `future` is empty (lookahead off, end of stream). The ARF source
filter window (`arf_tf_window_future`) is untouched.

* 0 -- the display-past window, the shipped behaviour.
* 1 -- future-only, nearest first, up to `tpl_depth()-1`.
* 2 -- past then future concatenated nearest-first, half the budget each side
  (`EC_AV1_TPL_FUT_HALF=<n>` moves the split; default 3 past + 4 future at
  `tpl_depth()=8`). The chain has ONE discontinuity in the middle.
* 3 -- future-only over the WHOLE buffered next group, ignoring `tpl_depth`.

## 3. Deciding gate: `encode::tests::bd_rate_film_long_gop` (48 pictures)

BD-rate vs libaom / vs rav1e, lower better; wall is ours.

| arm | film A | film B | wall A | wall B |
|---|---|---|---|---|
| 0, past (control, reproduced) | +21.3% / -8.8% | +72.1% / -1.2% | 671.1s | 461.6s |
| 1, future only | +22.0% / -8.6% | +73.1% / -1.1% | 629.6s | 583.1s |
| **2, 3 past + 4 future (SHIPS)** | **+21.2% / -8.8%** | **+71.6% / -1.5%** | 692.3s | 462.5s |
| 3, whole next group | +21.9% / -8.7% | +72.7% / -1.4% | 573.1s | 455.7s |

The control reproduces the standing table (film A +21.2/-8.8, film B
+72.1/-1.2 at 152996c8), so the arms are read against a live baseline.

A future-ONLY window LOSES on every column of both films (modes 1 and 3): the
nearest display-past neighbours are what carries the map's propagation for a
frame the whole group predicts from. Mode 2 is film B -0.5 / -0.3 and film A
-0.1 / 0.0, i.e. one row >=0.5 down and the other flat within +-0.3 -- the
lane's keep rule -- at +3.2% (film A) and +0.2% (film B) wall, well under the
+15% ceiling. `speed::TPL_FUT` defaults to 2.

Honest ceiling: mode 2's win is 0.5 point on a +72 number and the split
(3+4) was not swept. `EC_AV1_TPL_FUT_HALF` exists for that sweep.

## 4. 12-picture native guard (`bd_rate_screen_native`, all three clips on)

STRUCTURALLY BLIND to this lever: `group_target` absorbs the 12-picture tail
into ONE group of 11, so no next group is ever buffered and mode 2 falls back
to the past window on every ARF. Run only as a no-regression guard:

| clip | BD vs libaom | BD vs rav1e |
|---|---|---|
| bars 1080p | -3.2% | -18.8% |
| bars 2160p | +8.4% | -13.9% |
| film A | +17.9% | -6.3% |
| film B | +22.4% | -3.9% |
| screen capture | +14.4% | -33.2% |

## 5. Witness

`encoder::tests::the_future_tpl_window_still_codes_every_picture_and_moves_the_stream`
(sibling of the lane-lookahead one), coded at q=60 -- NOT 120, where this
fixture's ARF is almost pure skip and re-quantises to identical bytes. At GOP
32 and 8, run lengths 1/7/8/9/17, every mode: coding order and levels
unchanged, every picture coded, decoded exact through our decoder AND ffmpeg.
Modes 1 and 3 MOVE the stream against mode 0 at 17 pictures.

Mode 2 does NOT move it on that 128x128 fixture at its default split: it keeps
the three nearest past neighbours, which dominate the propagation, and no RD
decision flips. Measured, not assumed -- the same run with
`EC_AV1_TPL_FUT_HALF=1` (one past, six future) DOES move the stream, and the
long-GOP gate above measures mode 2 moving on real content. The witness names
this instead of asserting it.

## 6. Other gates

* Pins `encode::tests::the_encoders_own_streams_are_byte_identical_to_their_pins`:
  PASS at the default AND at `EC_AV1_SPEED=6`, unchanged (its 4-picture
  fixture has no next group, so the fallback keeps the past window).
* `encoder::tests::every_speed_preset_decodes_sample_exact_through_both_decoders`
  (--ignored): PASS.
* `encoder::tests::bitrate_target_lands_within_5_percent_over_48_frames`:
  PASS. Pyramid arm -1.9% is flat: 384k +1.5%, 768k +0.0%, 1536k +0.0%,
  2000k +0.0%; the flat arm -1.9% / -2.6% / -2.8% / -1.7%.
* Suite in three lanes and `cargo check --workspace --all-targets`: see the
  suite line at the end of this file.

## Deviations

* The charter's sub-arm (b) "past (+) future concatenated nearest-first" does
  not define the split; half-and-half with an env override is what shipped,
  and the split is unswept (named above).
* `EC_AV1_TPL_FUT_HALF` is an extra knob the charter did not ask for; it is
  what makes the witness's mode-2 inertness a measurement rather than a story.

## Suite line

`ec-av1` lib suite, three detached lanes off one release binary: 347 passed /
0 failed (`--skip stream::`), 202 / 0 (`stream:: --skip 10bit`), 42 / 0
(`10bit`) -- 591 passed, 0 failed, 55 ignored.
`cargo check --workspace --all-targets -j4`: 0 errors, 0 ec-av1 warnings (the
21 ec-opus missing-doc warnings and the ec-vorbis oracle one are pre-existing
and untouched).
