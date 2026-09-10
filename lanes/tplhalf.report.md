# lane-tplhalf: the top ARF's tpl split, then its budget

Claude Code died mid-lane after the split commit (`0c8d5778`) and after
the WIN sweep logs landed; this report closes that lane.

## 1. Premise

`TPL_FUT=2` (lane-tplfut) concatenates past then future, nearest-first.
The split was `(tpl_depth()-1)/2` = 3 past + 4 future at depth 8, unswept.
The future half is taken from the budget's edge, so the budget itself had
to be swept apart from `EC_AV1_TPL_D` (which deepens every frame's lambda
pass, not the ARF window). Class: instrument-at-bound.

## 2. Split sweep (`EC_AV1_TPL_FUT_HALF`)

Deciding gate: `encode::tests::bd_rate_film_long_gop` (48 pictures), one
thread, native crops. BD vs libaom cpu-used 6 / rav1e speed 6, lower
better. Logs: `~/.cache/tplhalf/{control,h1,h2,h5}.log`. Load before each
arm 2.8-4.7, after 2.9-4.4.

| past half | film A | film B | wall A | wall B | log |
|---|---|---|---|---|---|
| 1 (6 future) | +21.1 / -8.9 | +71.2 / -1.9 | 548.5s | 429.1s | h1.log |
| **2 (5 future, SHIPS)** | **+20.9 / -9.0** | **+71.0 / -1.8** | 551.3s | 442.3s | h2.log |
| 3 (4 future, old default) | +21.1 / -8.9 | +71.5 / -1.5 | 547.0s | 423.0s | control.log |
| 5 (2 future) | +21.1 / -8.9 | +71.5 / -1.6 | 561.8s | 438.6s | h5.log |

Bracketed: 2 is down on all four columns against 3 (film A -0.2/-0.1,
film B -0.5/-0.3), and 1 and 5 are both worse than it on the deciding
film B libaom column. Wall of 2 is +0.8% / +4.6% vs 3, under the +15%
ceiling. `speed::TPL_FUT_HALF` = 2 at presets 0..=2; presets 3..=6 keep 1
(`TPL_DEPTH=4`, unswept).

## 3. Budget sweep (`EC_AV1_TPL_FUT_WIN`) at half=2

Same gate. `0` means `tpl_depth()-1` = 7. Logs: `h2.log`, `w9.log`,
`w11.log`, `h2d10.log`, `w11h3.log`.

| window | film A | film B | wall A | wall B | log |
|---|---|---|---|---|---|
| 7 (`depth-1`) | +20.9 / -9.0 | +71.0 / -1.8 | 551.3s | 442.3s | h2.log |
| 9 | +21.0 / -9.0 | +70.7 / -2.0 | 545.3s | 430.2s | w9.log |
| **11 (SHIPS at presets 0..=2)** | **+20.9 / -9.0** | **+70.4 / -2.2** | 578.6s | 431.3s | w11.log |
| 11, half=3 (split re-check) | +20.9 / -9.0 | +70.9 / -1.9 | 554.8s | 437.7s | w11h3.log |
| `TPL_D=10` at half=2 | +21.0 / -9.0 | +70.7 / -2.0 | 543.7s | 435.8s | h2d10.log |

11 vs 7: film B -0.6 / -0.4, film A 0.0 / 0.0 -- keep rule (one row
>=0.5 down, the other flat). Wall A +5.0%, wall B -2.5%. 11 saturates
the one-group lookahead (2 past + 8 future available). Half=3 at the
larger budget loses to half=2, so the split does not flip.

`TPL_D=10` at half=2 is byte-identical to WIN=9 (same PSNR/bytes on both
films): extra depth on non-ARF frames is unused because a pyramid leaf's
window is the group's own successors. That is why WIN is its own knob.

`speed::TPL_FUT_WIN` = 11 at presets 0..=2; presets 3..=6 keep 0
(`depth-1` = 3 at `TPL_DEPTH=4`), unswept.

## 4. 12-picture native guard

STRUCTURALLY BLIND: `group_target` absorbs a 12-picture tail into one
group, so no next group is buffered and mode 2 falls back to the past
window. Not re-run; the standing table's 12-frame rows are the guard.

## 5. Witness

`encoder::tests::the_future_tpl_window_still_codes_every_picture_and_moves_the_stream`
already names that mode 2 at the old 3-past split is byte-identical on
the 128x128 fixture and that `EC_AV1_TPL_FUT_HALF=1` moves it. No new
witness: the long-GOP gate is the measurement.

## 6. Other gates

* Pins `encode::tests::the_encoders_own_streams_are_byte_identical_to_their_pins`:
  PASS at the default and at `EC_AV1_SPEED=6`, unmoved (8291 / 33227). The
  4-picture fixture has no next group, so the fallback keeps the past window.
* `encoder::tests::the_future_tpl_window_still_codes_every_picture_and_moves_the_stream`:
  PASS.
* `encoder::tests::every_speed_preset_decodes_sample_exact_through_both_decoders`
  (--ignored): PASS.
* `encoder::tests::the_lookahead_holds_one_group_and_still_codes_every_picture`
  went red under WIN=11 (17 pictures at gop 32: 5375 vs 5335 B). The identity
  arm now pins `TPL_FUT=0` so it tests drain, not the lambda window
  (`9f81a05e`). PASS after that pin.
* `cargo check --workspace --all-targets -j4`: 0 errors, 0 ec-av1 warnings
  (21 ec-opus missing-doc warnings and the ec-vorbis oracle one are
  pre-existing).

## Deviations

* Claude subscription died after the split commit and after the WIN
  logs; this close is from those logs plus the WIN default they keep.
* Presets 3..=6 split and budget left unswept (named in the tables).
* 12-frame rows not re-measured (structurally blind, accepted).
* The lookahead drain witness had to pin `TPL_FUT=0`; that is a test
  isolation fix, not a WIN revert.

## Suite line

`ec-av1` lib suite, three detached lanes off one release binary at
`9f81a05e`: 348 passed / 0 failed (`--skip stream::`), 202 / 0
(`stream:: --skip 10bit`), 42 / 0 (`10bit`) -- 592 passed, 0 failed,
56 ignored. Log `~/.cache/tplhalf/suite2.log`.
`cargo check --workspace --all-targets -j4`: 0 errors, 0 ec-av1 warnings.
