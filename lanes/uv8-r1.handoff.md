# lane-uv8 r1 — HANDOFF (RED: implemented, NOT gated)

## What is implemented (`crates/ec-av1/src/decode.rs`, `decode_inter_block8`'s intra arm)
Mirrors libaom `read_intra_block_mode_info` (`av1/decoder/decodemv.c:1178-1220`, read from
`~/.cache/aom-oracle/src`) for an INTRA 8x8 leaf inside an INTER frame:

- `uv_mode` over the CFL-allowed alphabet (`cdfs.uv_mode_cfl[mode]`; BLOCK_8X8 is inside
  `is_cfl_allowed`'s 32x32 bound), then `read_cfl_alphas` when `UV_CFL_PRED`, then
  `angle_delta_uv` off `angle_delta[uv_mode - V_PRED]` (`get_uv_mode` is the identity over
  `V_PRED..=D67_PRED`) — the exact order libaom reads them in.
- `uv_predict_mode = get_uv_mode(uv_mode)`; chroma prediction, chroma `default_tx_type`
  (`read_plane`'s `predict_mode` arg → `default_intra_tx_type`, DCT for CfL) and the CfL AC
  (`cfl_ac_q3(y, px, py, 8)` over the leaf's 4x4 chroma) all take it.
- `smooth_neighbor_uv` from `Neighbours::smooth_uv_neighbour(leaf_mi.0, leaf_mi.1, r, c)`
  (chroma edge filter reads the CHROMA neighbour's uv_mode — reconintra.c:974), and the leaf's
  LUMA `smooth_neighbor` from `modes_above_left_mi` (it passed a hardcoded `false` before).
- `palette_uv_mode` is now read only when `uv_mode == DC_PRED` (`read_palette_mode_info`);
  it was read unconditionally, harmless only because every non-DC uv_mode was refused.
- Tail: the leaf now stamps its mode bands (`above_uv_mode[c]`/`left_uv_mode[r]`,
  `record_mode_mi`, `record_uv_mode_mi`, 2x2 mi) — it stamped none at all, so a neighbour read
  a stale coarse SUB slot. An inter leaf stamps `DC_PRED`, what libaom stores.
- New per-leaf counters `decode::intra_in_inter8_uv_hits() -> (dir, smooth/paeth, cfl)`.

## Refusal state
`"a non-DC chroma mode on an 8x8 inter-frame leaf (this encoder never writes one)"` is REMOVED
from decode.rs and from `refusal_inventory.rs::CAPABILITY_CLAIMS`. **This lift is NOT gated** —
see below. If the next round cannot gate it, restoring both lines is the honest move.

## Gate (written, never observed a firing attempt)
`stream.rs::uv_mode_in_inter8_gate(bit_depth, tile_columns)` + 3 tests (8-bit, 10-bit, 2-tile):
192x128, 8 frames, mandelbrot zoom + hard cut at frame 4 + seeded `noise`, chroma NOT
desaturated, 30 attempts, cq 12..40, `--min-partition-size=8 --max-partition-size=32
--sb-size=64 --enable-cfl-intra=1 --enable-smooth-intra=1 --enable-paeth-intra=1
--enable-directional-intra=1`, per-attempt counter deltas, every decode-order frame compared,
no SKIP on decode error. The y4m goes through a temp FILE: feeding aomenc on stdin while its
~100 KB OBU fills the stdout pipe DEADLOCKS (measured, 3 min into attempt 0).

RESULT: 8-bit arm ran, 30/30 attempts REFUSED, 0 compared → the gate fails as designed.

## The blocker (pre-existing, NOT this round's change)
Every `--min-partition-size=8` inter stream refuses. `--min-partition-size=16` on the same
recipe decodes 8/8 frames. The refusals name tools the encoder was explicitly told to disable:
`--enable-rect-partitions=0 --enable-ab-partitions=0 --enable-1to4-partitions=0` yet we refuse
"an inter 16x16-level AB or 1:4 partition" / "a non-skip rectangular strip"; and
`--enable-angle-delta=0` yet we refuse "a nonzero angle delta on an 8x8 intra leaf". Class
[[refusal-from-own-desync]]: those are our own desync, not encoder output.
Ablation: the same refusals appear with `--enable-cfl-intra=0 --enable-smooth-intra=0
--enable-paeth-intra=0 --enable-directional-intra=0`, where this round's code is inert →
the desync is INDEPENDENT of this round.
The KEY frame of the same recipe (`--limit=1`, min8) decodes PIXEL-EXACT vs aomdec
(md5 860b50e830271fa57fb60e4a76eb83ec both) — so the 8x8 leaf's key-frame path is fine and the
divergence is in an INTER frame.

## Exact next step
Bisect the first inter-frame divergence with the oracle mode ladder. Note the trace semantics
(cost this round an hour): oracle `EC_IMODE` = KEY-frame blocks only, oracle `EC_MODE` =
INTER blocks only (an intra block inside an inter frame prints NEITHER), while ours prints
`EC_MODE` for EVERY block of an inter frame. So the oracle's `EC_MODE` (mi_row, mi_col)
sequence must be an ordered SUBSEQUENCE of ours; the first ref entry that ours never produces
is the first structurally divergent block. On `~/.cache/uv8-tmp/o.obu` (recipe above, cq 26,
cpu-used 0) that subsequence match stops after 79 of the ref's frame-1 entries, with ref next
at mi (12,8) — start there with a range ladder (compare RANGE, never tell()).
Scripts: `~/.cache/uv8-tmp/{in.y4m,sweep.sh,sweep2.sh,sweep3.sh}`; probe
`$HOME/.cache/cargo-target-uv8/debug/examples/decode_probe <obu> [out.yuv]`.
