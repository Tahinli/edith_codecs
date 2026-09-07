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

(filled in below as each gate lands)
