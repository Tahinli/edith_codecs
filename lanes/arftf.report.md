# lane-arftf — the ARF temporal filter's strength, window and level

Worktree `edith_codecs-arftf`, branch `lane-arftf` off main `8d9d056b`.
Deciding gate `encode::tests::bd_rate_film_long_gop` (48 pictures, both film
rows, BD vs libaom `cpu-used 6` / rav1e `speed 6`, lower is better).
**Control RUN, not quoted: film A +22.7 / −7.9, film B +79.0 / +2.7 — the
charter's numbers to the digit.**

lane-arfpred shipped the filter at strength 3 with only that one point on the
gate; its two-point probe mispredicted the SIGN of this lever, so every arm
here is a gate arm.

## 1. Strength sweep — the shipped point moves 3 → 2

| strength | film A | film B |
|---|---|---|
| 1 | +22.6 / −8.1 | +81.6 / +4.8 |
| 2 **(ships)** | **+22.2 / −8.2** | **+78.6 / +2.7** |
| 3 (was) | +22.7 / −7.9 | +79.0 / +2.7 |
| 5 | +23.9 / −7.4 | +83.6 / +5.1 |

Single-peaked and BRACKETED on both sides. 2 is down on all four columns
against the shipped 3 (film A −0.5 / −0.3, film B −0.4 / 0.0), which clears
the keep rule outright. Both neighbours are worse on the film B libaom
column by 2.6 (s1) and 5.0 (s5), so this peak is not a noise artefact.

## 2. The ±2 window — the future half is NOT buffered, past-only ±2 loses

`Av1Encoder::drain_pending` codes the group's top ARF as the group's LAST
picture (`group.last()`), and `encode_frames` pushes into `pending` and drains
the whole group before the next picture arrives, so **when the top ARF is
coded no picture after it exists in the encoder at all**. `tpl_window(pos,
true)` hands it `group[..pos]` reversed — display-PAST neighbours only, up to
`tpl_depth() − 1` of them (7 at the default preset). A symmetric ±2 needs a
one-group lookahead this encoder does not have; that is the blocker, and it
is a picture-buffer change, not a filter change.

What IS available is more past, so the charter's fallback was measured:
`speed::ARF_TF_WIN` (new, `EC_AV1_ARF_TF_WIN=<n>`) widens the neighbour count
from the literal 2 to any n.

| window | film A | film B |
|---|---|---|
| 2 past (ships) | +22.2 / −8.2 | +78.6 / +2.7 |
| 4 past (±2 one-sided) | +22.8 / −7.8 | +78.9 / +2.8 |

REFUTED on three of the four columns. `arf_pred_census`'s 14.3% SAD removal
was measured with two neighbours on EACH side; four pictures of one-sided past
is a different window, and the anchor drifts away from the leaves that have to
predict from it.

## 3. The mid ARF — filtering it a second level down does not pay

`speed::ARF_TF_MID` (new, `EC_AV1_ARF_TF_MID=<strength>`) runs the same filter
on the group's second hidden frame, whose own window is `tpl_window(at,
false)` — the group's LATER leaves, i.e. it filters forward where the top ARF
filters backward, and those pictures ARE buffered.

| arm | film A | film B |
|---|---|---|
| top ARF only, s = 2 | +22.2 / −8.2 | +78.6 / +2.7 |
| top + mid, both s = 2 | +22.2 / −8.3 | +78.9 / +2.6 |

Inside the noise on three columns and 0.3 WORSE on film B's libaom one, so it
ships OFF (`ARF_TF_MID = 0`). Deviation from the charter, named: the mid arm
was run at strength 2, not 3, because the sweep had already put 2 above 3 at
the top level and a 3 there would have confounded the level with the strength.

Per-block weight refinement was left alone (charter: out of scope unless
one line — it is not).

## 4. Guard rows — `bd_rate_screen_native`, 12 pictures, all five rows

| clip | control (s = 3) | s = 2 | Δ |
|---|---|---|---|
| bars 1080p | −2.9 / −18.6 | −3.2 / −18.8 | −0.3 / −0.2 |
| bars 2160p | +9.1 / −13.3 | +9.1 / −13.4 | 0.0 / −0.1 |
| film A | +18.3 / −6.0 | +18.5 / −5.9 | +0.2 / +0.1 |
| film B | +24.6 / −2.1 | +24.6 / −2.0 | 0.0 / +0.1 |
| screen capture | +14.6 / −33.2 | +14.6 / −33.2 | byte-identical |

Control reproduces the charter's numbers exactly. Nothing moves by more than
0.3; the two bars rows actually improve, and the screen sequence codes no
pyramid ARF at all so the filter never runs there.

## 5. Invariants

* Byte pins re-taken (the filtered anchors' pixels move by construction):
  `(150, 8218 → 8307)`, and `(60, 33017)` keeps its LENGTH with a different
  hash. Green at the default AND at `EC_AV1_SPEED=6`.
* `encoder::tests::bitrate_target_lands_within_5_percent_over_48_frames` is
  green in the split suite below (it sits near its bound at the rate loop's
  quantizer floor; `ARF_TF_QMIN = 16` still keeps the filter off there).
* Split suite on the shipped default: `--skip stream::` **343 passed / 0
  failed**, `stream:: --skip 10bit` **202 passed / 0 failed**, `10bit`
  **42 passed / 0 failed** (587 in all).
* `--ignored --exact
  encoder::tests::every_speed_preset_decodes_sample_exact_through_both_decoders`
  1 passed.
* `cargo check --workspace --all-targets -j4`: 0 errors, 0 `ec-av1` warnings
  (the 25 warnings are `ec-opus` / `ec-vorbis`, pre-existing).

## 6. What this lane did NOT do

* `deferred: the symmetric ±2 window — the census's best filter shape needs
  the two DISPLAY-FUTURE neighbours of the top ARF, which the encoder does
  not hold when it codes it — unblocked by a lane that gives
  Av1Encoder::drain_pending one group of lookahead (buffer group N+1's
  sources before coding group N's ARF).`
* `deferred: per-block weight refinement (libaom weights each 16x16 block by
  its own error against each neighbour separately, and uses up to 7 frames) —
  out of the charter's scope and not a one-liner — unblocked by a lane that
  ports libaom's temporal_filter weighting.`
* `deferred: a mid-ARF strength of its own (only 2 was measured there, and it
  was flat-to-worse) — unblocked by a lane with spare long-GOP arms; the knob
  ships wired (EC_AV1_ARF_TF_MID) so the arm is env-only.`
