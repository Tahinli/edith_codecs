# lane-deltaq — per-superblock `delta_qindex` from the tpl map

Base `36b573b9` (no RDOQ on this head). Feature commit `c121e991`.

## What the commit holds

* **Frame header** — `frame.rs::write_delta_q_params` already wrote the spec's
  syntax; `encode_inter_frame` now SETS `header.delta.q_present` /
  `delta.q_res` whenever the frame has a tpl map and the lever is on.
* **Tile syntax** — `tile.rs::write_delta_q`, the write side of decode.rs
  `maybe_read_delta_q`, called right after `write_cdef_idx` at each of the
  four inter block writers (`Whole64`, the 32x32 quadrant, the 16x16 leaf,
  the 8x8 leaf), i.e. the spec's `skip -> cdef -> delta_q` order. Fires only
  at the superblock's top-left mode-info position, and (as the reader does)
  never when that block IS the superblock and is skipped. `arm_delta_q`
  carries the per-SB plan the way `arm_cdef_idx` carries the CDEF one and is
  cleared by the same `CdefIdxGuard`, so `CurrentQIndex` resets at each tile
  exactly as the spec's `decode_tile` does. The `delta_q` CDF was already in
  `cdf_state` and already in the per-frame counter reset.
* **Per-superblock quantizer in the search** — every trial reaches the
  quantiser through `Search::base_q_idx` (luma and chroma alike), so the
  search sets that field per superblock and both the quantisation and the
  dequantisation that reconstructs the block move with it. `qmatrix` is off.
* **Mapping** — `encode::deltaq_for_factor`, derived, not fitted (below).
* **Levers** — `EC_AV1_DELTAQ` (the `delta_q_res` LOG2; `0` = off, stream
  byte-identical to the pre-lane one), `EC_AV1_DELTAQ_K`,
  `EC_AV1_DELTAQ_CLAMP`, and `speed::DELTAQ_RES` (preset 0..10).
* **Fire counter** — `encode::take_deltaq_levels`, the number of DISTINCT
  qindex levels the last frame's grid carried; a flat grid makes every
  exactness assert below vacuous (class `gate-blind-to-feature`).
* **One decoder-side fix** (deviation from "do not touch the decoder", and the
  reason the feature could not work without it): `decode_inter_frame_tiles_lr`
  hard-coded `DeltaParams::default()`, the exact stale-header class the three
  parameters above it in that signature document. The encoder's own trial
  decode therefore never read the syntax the writer wrote and desynced at the
  first superblock, surfacing as a reference-slot refusal ("refusal from own
  desync"). It now takes the frame's own `delta` and forwards it; the two
  in-crate gates that decode a raw tile take `frame.delta` from the new
  `Encoded::delta` field.

## The mapping derivation

The tpl map hands out a factor `f` on LAMBDA. RD lambda goes as the square of
the quantiser step (`encode_inter_frame`: `lambda = .. * step * step`, `step =
ac_q(base)/8`), so a superblock the map prices at `f` wants a step of
`sqrt(f)` times the frame's. `ac_q` is not linear in the index, so the step
ratio is inverted THROUGH THE TABLE — the index whose `ac_q` is closest to
`ac_q(base) * sqrt(f)` — rather than through a fitted `K`:

    delta = argmin_q | ac_q(8, q) - ac_q(8, base) * sqrt(f) |  -  base

`EC_AV1_DELTAQ_K` then scales that step (1.0 = the whole derivation, the
sweep's `x0.7` / `x1.3`), the result is snapped onto the `delta_q_res` grid
BEFORE the +-clamp (a clamp applied after would hand back a qindex that is not
congruent to `base`, and the coded symbol would not be a whole number of
steps), and the clamp defaults to +-16 (libaom clamps its objective deltaq
around a quarter of the base qindex).

Self-consistency: the superblock's RD lambda is then recomputed FROM the
qindex it is coded at (`sb_q_search`: `lambda * (ac_q(q)/ac_q(base))^2`) and
replaces the raw tpl lambda factor, so nothing is counted twice. At `K = 1`
unclamped the two are the same number.

## Results

### The 12-frame gate, `bd_rate_screen_native` (BD vs libaom / vs rav1e)

CONTROL FIRST (`EC_AV1_DELTAQ=0`, `lanes/dq-ctrl.log`). The charter's control
numbers are STALE for this head on the two film rows: the screen row lands on
+33.4/-23.8 to the digit, but film A reads +34.8/+5.0 (charter +37.1/+7.3) and
film B +47.0/+16.5 (charter +52.5/+22.3). Every delta below is against THIS
control, same binary, same box.

| row | control | res 4, K 1 |
|---|---|---|
| bars 1080p | +1.8 / -14.4 | +1.5 / -14.9 |
| bars 2160p | +12.6 / -9.9 | +12.5 / -9.8 |
| film A 1920x768 | +34.8 / +5.0 | +34.7 / +5.2 |
| film B 1920x1024 | +47.0 / +16.5 | +46.7 / +16.6 |
| screen capture | +33.4 / -23.8 | +34.1 / -23.6 |

Logs `lanes/dq-ctrl.log`, `lanes/dq-r2k1.log`.

Screen is NOT byte-identical, which the charter expected: screen frames still
get a tpl map (the screen detector gates palette/intrabc, not the map), so
they carry the syntax too -- and they LOSE 0.7 points against libaom, past the
keep rule's 0.3 bound.

### The mapping sweep (12-frame gate, all five rows)

| arm | bars 1080p | bars 2160p | film A | film B | screen |
|---|---|---|---|---|---|
| control (`DELTAQ=0`) | +1.8 / -14.4 | +12.6 / -9.9 | +34.8 / +5.0 | +47.0 / +16.5 | +33.4 / -23.8 |
| res 4, K 1.0 | +1.5 / -14.9 | +12.5 / -9.8 | +34.7 / +5.2 | +46.7 / +16.6 | +34.1 / -23.6 |
| res 4, K 1.3 | +1.7 / -14.8 | +12.6 / -9.8 | +34.7 / +5.1 | +46.8 / +16.6 | +34.3 / -23.5 |
| res 8, K 1.0 | +1.2 / -15.3 | +12.7 / -9.8 | +35.2 / +5.3 | +47.8 / +17.1 | +35.0 / -23.4 |

Logs `lanes/dq-{ctrl,r2k1,r2k13,r3k1}.log`, all `RC=0`. Clamp +-16 throughout.

K 0.7 was not run: K 1.3 is K 1.0 within noise on every row and K 0.7 moves
strictly TOWARD the control, so it cannot clear a keep rule that K 1.0 misses
by 0.4 points.

## Decision: SHIPPED OFF (`speed::DELTAQ_RES = [4; 11]`)

The keep rule wanted both film rows down on both columns, or one column >= 0.5
down with the other flat. The best arm (res 4, K 1.0) gives film A -0.1/+0.2
and film B -0.3/+0.1 -- inside noise, nowhere near 0.5 -- and costs the screen
row 0.7 against libaom, past the 0.3 bound. Coarser steps (res 8) lose 0.4-0.8
on both films. So the syntax, the search plumbing and the mapping ship, and
the lever ships off: `EC_AV1_DELTAQ=2` (or 3) turns it on, every stream is
otherwise byte-identical to the pre-lane one.

Why the ceiling is where it is: the map's LAMBDA arm was already measured at
+53.2 -> +53.2 on film A (the `speed` lever table), i.e. BD-neutral. Turning
that same factor into a quantizer is the same information through a different
knob, so the same near-zero answer is what the derivation predicts. Moving
this lever needs a BETTER MAP, not a better mapping -- the tpl pass's own
coarse motion/intra costs are the upstream bound (class
`search-bounded-by-its-heuristic`).

Screen: NOT byte-identical with the lever on (the charter expected it to be).
Screen frames get a tpl map like any other -- the screen detector gates
palette/intrabc, not the map -- so they carry the syntax and lose 0.7. Had the
films paid, this row would have needed the same `!screen` content gate
`b64_residual`/`b64_compound` take.

## Invariants

* three-way witness `a_moving_detail_clip_codes_two_delta_q_levels_both_decoders_read_exactly`:
  3 distinct quantizer levels, 29358 bytes, every frame sample-exact through
  `decode_stream` AND ffmpeg. Its fire count is preset-0's alone --
  `speed::TPL_DEPTH` cuts the lookahead to one picture above preset 0, so
  there is no map, and delta_q is inert at every preset but 0.
* `EC_COMP_MISMATCH=1`: 0 lines at presets 0 and 6.
* `tile_bytes_do_not_depend_on_the_thread_count --include-ignored`,
  `the_facade_codes_the_same_bytes_as_encode_sequence`,
  `predicted_coeff_bits_track_the_tile_the_writer_wrote`: pass at presets 0
  and 6.
* `bitrate_target_lands_within_5_percent_over_48_frames`: passes, landed
  -0.2 / +3.1 / -1.6 / -0.0 / -2.8 / +0.0 / -2.8 / -4.4 %.
* byte pins: 9808 / 35791 HOLD unchanged (the lever ships off). With
  `EC_AV1_DELTAQ=2` the q=150 pin clip codes 9898 bytes.
* `cargo check --workspace --all-targets`: 0 errors, 0 ec-av1 warnings.

## Deferred

* `deferred: the long-GOP arm (bd_rate_film_long_gop) --` the shipped
  configuration is the lever OFF, which is byte-identical to the base on every
  stream, so the shipped long-GOP row IS the base's row and there is nothing
  to measure. Whether 48 frames of propagation would flip the 12-frame verdict
  is open; it needs a control + one arm, ~30 minutes.
* `deferred: separate leaf/ARF clamps --` the sweep never separated them; with
  the best single-clamp arm 0.4 short of the keep rule, a per-level clamp is
  unlikely to close that alone.
* `deferred: the per-level delta symbol census --` `syntax_census` reports the
  header's `delta_q` bit but counts no `delta_qindex` symbols; the fire counter
  `encode::take_deltaq_levels` (distinct qindex levels per frame) is what this
  lane used instead.
