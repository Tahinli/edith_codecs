# lane-hpmv — eighth-pel motion vectors: the capability, and why it does not ship on

Worktree `edith_codecs-hpmv`, branch `lane-hpmv` off main 12c99b84. Every BD
row is the release lib-test binary (`bd_rate_screen_native`, 12 pictures,
`gop 12`, four quantizers, native `gate_crop`), each row detached under its own
`systemd-run` unit and read off the log's own clip line.

## 1. What was missing

`allow_high_precision_mv` was off in every frame this writer coded, so
`tile::write_mv_component` REFUSED any residual whose eighth-pel bit is zero
and `encode::round_to_valid_mv` rounded the search's winner down to an even
1/8-pel count first — the finest vector the encoder could commit to was a
quarter pel. The DECODER already read `mv_hp`/`mv_class0_hp` (it decodes
libaom's low-q streams exactly), so this lane is writer-side only.

Built (commit `a2cc1120`):

* the header bit, from libaom's own rule — `av1_pick_and_set_high_precision_mv`
  (`mv_prec.c`): `use_hp = qindex < HIGH_PRECISION_MV_QTHRESH` (128). Ours is
  `encode::HP_MV_QTHRESH`, swept through `EC_AV1_HP_MV`
  (`0`/`off`, `all`, or a threshold).
* `write_mv_component`: the `mv_class0_hp` / `mv_hp` symbol per non-zero
  component in a high-precision frame, nothing coded otherwise (spec 5.11.32).
* `encode::mv_component_bits`: the same symbol priced, and no `None` refusal
  in a high-precision frame; `round_to_valid_mv` returns `mv` untouched there.
* `motion::search`: a FOURTH refinement stage at `EIGHTH_PEL_Q3`, run only in
  a high-precision frame.
* the frame's bit armed per worker beside the order hints
  (`tile::arm_high_precision_mv`), because the SEARCH reads it before the
  writer does.

Not needed: `lower_mv_precision`. The spec applies it to the GLOBAL and
TEMPORAL (tpl) candidates only; this encoder writes `use_ref_frame_mvs: false`
and identity global motion, and `mvstack`'s tpl entry points already take the
flag. Spatial stack candidates keep their own precision either way.

## 2. Witness (red before, green after)

`encode::tests::a_high_precision_frame_codes_eighth_pel_motion_vectors_both_decoders_read`
— three frames of a panned test card, encoded twice with
`set_high_precision_mv(Some(false|true))`:

```
hp witness: off 0 hp symbols / 862 B, on 6 hp symbols (5 eighth-pel) / 867 B
test ... ok
```

Both arms decode sample-exact (Y, U, V, every frame) through ffmpeg AND
`crate::stream::decode_stream`. RED BEFORE by construction: before this commit
no frame could carry the bit at all (`allow_high_precision_mv: false`, no
setter, and `write_mv_component` erroring on the eighth-pel residual), so the
`on` arm could not be encoded — the count could only ever be 0. The
"5 of 6 hp symbols are ZERO" assertion is the signal, not the symbol: a zero
hp bit names a vector a non-hp frame cannot code at all.

## 3. The policy sweep — film B, 12 frames (the deciding row)

| arm | vs libaom | vs rav1e | hp symbols | of them eighth-pel |
|---|---|---|---|---|
| **control (off)** | **+24.6** | **-2.0** | 0 | — |
| `qindex < 96` | +25.0 | -1.8 | 58019 | 52.6% |
| `qindex < 128` (libaom's rule) | +25.1 | -1.8 | — | — |
| always on | +26.6 | -0.3 | 108001 | 53.9% |

Monotone worse in how much high precision is armed, on BOTH columns. The
capability fires: over half the hp symbols coded are ZERO bits, i.e. vectors
the control cannot name.

NEWMV census, same clip and quantizers, control -> always-on:

| side | won% | mv bits | mv==nearest |
|---|---|---|---|
| 8 | 31.5 -> 29.4 | 8.29 -> 9.56 | 36.7 -> 32.4 |
| 16 | 41.1 -> 40.7 | 7.99 -> 9.48 | 30.3 -> 26.0 |
| 32 | 24.3 -> 24.9 | 7.07 -> 8.38 | 35.5 -> 32.4 |

Search evals/call 81.3 -> 93.6 (the fourth stage). So the finer vector is
FOUND and CODED; it just costs 1.2-1.5 more mv bits per coded `NEWMV` than the
extra prediction accuracy earns back on this content, and the NEWMV share
falls with it. This is not a flat/inert result — it is a priced loss.

## 4. Film A, 12 frames (best arm only)

| arm | vs libaom | vs rav1e |
|---|---|---|
| control | +20.3 | -5.0 |
| `qindex < 128` | +20.3 | -4.9 |

Flat on libaom, 0.1 worse on rav1e. With film B 0.4-0.5 worse on libaom at
every threshold, the keep rule ("both film rows improve on both columns, or
one >=0.5 down and the other flat +-0.3") cannot be met by any arm, so the
screen row and the long-GOP arm were not run — no candidate could have shipped
on their result.

## 5. What ships

The CAPABILITY, default OFF: `HP_MV_QTHRESH = 0`. `EC_AV1_HP_MV` sweeps it
without a rebuild, `set_high_precision_mv` arms it from a test. No default
stream moved, so no byte pin moved:
`the_encoders_own_streams_are_byte_identical_to_their_pins` is green UNCHANGED
(`(150, 8252, ...), (60, 33053, ...)` still the pinned bytes).

Worth re-testing when the block-level mv pricing changes: the loss is entirely
a rate loss on the mv syntax, so a lane that moves `MV_LAMBDA`/the DRL pricing
or ships a smarter `NEWMV` share may flip its sign. libaom's own rule reads
`qindex < 128` on OUR ladder as "the two lowest-q points", i.e. it is the
low-q end that pays for it there.
