# lane-av1-sub32 r1 — 32x32 quadrants may split into four 16x16 blocks

## What landed

`encode_key_frame` now trials each 32x32 quadrant twice and keeps the cheaper
coding:

- `cost_whole = luma_cost + sum_chroma(sse + lambda * bits) + lambda * partition_bits(BLOCK, false)`
- `cost_split = sum_i(sub costs) + lambda * (partition_bits(BLOCK, true) + 4 * partition_bits(SUB, false))`

The reconstruction is snapshotted before the whole trial, restored before the
split trial, and the winner's reconstruction is put back, so prediction below and
to the right reads exactly what the decoder will. Mode bookkeeping moved to the
16x16 grid (`above_mode[cols * 2]`, `left_mode[rows * 2]`), which is the
granularity the spec's mode context reads at.

The ablation is a parameter, not an env knob: `encode_key_frame_inner(..., split_blocks: bool)`,
with `SPLIT_BLOCKS` as the shipped default and `probe_split` calling both ways.

## Measurement — `probe_split`, three-point ladder (q 110/90/70), BD-rate split vs whole

| picture | BD-rate | coded blocks / quadrants @ q90 |
| --- | --- | --- |
| test card (synthetic) | -0.72% | 15 / 15 |
| stripes (synthetic) | +0.00% | 15 / 15 |
| diagonal (synthetic) | -22.45% | 33 / 15 |
| Troy 1080p film @20:00 | **-9.15%** | 415 / 220 |
| OBS screen capture @10:00 | **-27.20%** | 604 / 220 |
| he_is_not_the_only_one.mp4 @5s | **-9.05%** | 316 / 220 |

Negative is better. `stripes` never splits (every quadrant is one texture), which
is the shape a correct RD decision should have. Default set to `SPLIT_BLOCKS = true`.

## Defect found, fixed and gated — class: context read from one cell

Turning splits on broke `ffmpeg_decodes_exactly_what_the_encoder_reconstructed` at
96x64: 979/6144 samples off by ~1, starting at the first split quadrant. Bisected
by forcing DC on sub-blocks (drift stayed, so not the mode context) and by adding a
permanent 16x16 inverse-transform-vs-ffmpeg gate (passed, so not the transform or
dequant).

Cause: `write_block` read the chroma `txb_skip` context and the DC-sign context
from a single neighbour cell. The decoder gathers over every 4x4 unit the block
spans (`get_txb_skip_ctx`, spec 5.11.39; `Dc_Sign_Contexts`, spec 8.3.2). While
every block is 32x32 the one cell *is* the span, so the bug is invisible; the
moment a 16x16 sits beside a 32x32 the arithmetic decoder desynchronises — as
sample drift, never a crash.

Fix: `Neighbours::around((r, c), side) -> [Around; 3]`, OR-ing `coded` and summing
the DC vote across `side / SUB` cells on both edges.

Sibling single-cell reads swept, each correct by spec and left alone:
`above_mode`/`left_mode` (the spec reads exactly the mi above/left of the top-left),
`partition_ctx` (one neighbour's size), luma `txb_skip` ctx = 0 (the transform
covers the whole block at every size the writer codes).

## Gates added

- `the_inverse_transform_predicts_what_ffmpeg_decodes_for_a_16x16_block` — a split
  quadrant carrying eight coefficients including `(15, 15, 4)`, every sample
  compared to `dequant_and_inverse(&levels, TX16, 8, Q_IDX)`.
- `a_block_gathers_the_cells_its_neighbours_cover` — two 16x16 neighbours with
  different chroma state; asserts the gathered read sees both and that the
  single-cell read misses it.

## State

`cargo clippy --release -p ec-av1 --all-targets` zero warnings; suite 85 passed,
0 failed, 4 ignored.

## Next in this layer

Transform types beyond DCT_DCT and tx-size selection; splits below 16x16; pictures
that are not a multiple of 32; interleaving the search with the writer so rates
price against adapted tables.
