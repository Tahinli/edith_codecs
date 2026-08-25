# lane-av1-directional r1 — the six directional intra modes

Adds AV1's six directional intra predictors (D45, D67, D113, D135, D157, D203)
to `ec-av1`, with the availability derivation the decoder actually uses, and
puts the cost of *naming* a mode into the mode search — which was the honest
finding left open by `av1-predict-r1`.

## What landed

- `crates/ec-av1/src/intra.rs`: `directional()` per spec 7.11.2.4, all three
  zones (`pAngle < 90` walks the above row, `> 180` walks the left column,
  `90..180` walks the above row while `base >= -1` and falls to the left
  column below that), `Dr_Intra_Derivative` and `Mode_To_Angle` tabulated.
  Edges now carry the spec's index −1 (the corner) at slot 0 and are extended
  to `2 * side` by repeating the last sample. `enable_intra_edge_filter` is
  false in our sequence header, so no filtering and no upsampling apply.
- `crates/ec-av1/src/encode.rs`: `Reach` — which of the above-right and
  below-left samples a block may read, derived from `clear_block_decoded_flags`
  (spec 7.4). For the four 32x32 quadrants of a 64x64 superblock: above-right
  available for quadrants 0, 1, 2 (not 3), below-left only for quadrant 0.
- `mode_bits()` — what the tile writer spends to name each of the thirteen
  modes, scored through the writer's own CDFs: the KF_Y_MODE symbol under the
  context the two neighbouring modes pick, the ANGLE_DELTA zero symbol a
  directional mode carries, and the UV_MODE_CFL DC symbol, whose row the luma
  mode indexes. The encoder tracks neighbour modes exactly as the writer does.
- `LAMBDA_SCALE` re-swept with the mode cost in: 0.05 → 0.1.

## Gates

| Gate | Result |
|---|---|
| `ffmpeg` decodes our stream to the encoder's own prediction, all 13 modes, 128x96 and 160x96 | bit-exact, 0 differing samples |
| the search picks the diagonal the picture runs (D135 / D45 test cards) | pass |
| directional modes cost no more than 1% rate on a picture that does not want them | +0.41% |
| full suite `cargo test --release -p ec-av1` | 75 passed, 0 failed, 2 ignored |
| `cargo clippy --release -p ec-av1 --all-targets` | 0 warnings |

The 160x96 width is in the shape gate on purpose: it makes the last superblock
of a row partial, which is where a wrong above-right derivation shows up.

## Mutations killed

| Mutation | Caught by |
|---|---|
| above-right always available | ffmpeg mismatch, mode 3, first at (63, 64) |
| below-left always available | ffmpeg mismatch, mode 7 |
| 45° derivative off by one (`1023` → `1022`) | ffmpeg mismatch, mode 3 |
| zone 2 reads the left column instead of the above row | ffmpeg mismatch, modes 4/5/6 |

Restore run after the four: 12 passed, 0 failed.

## His data — this branch against `main`, same frame each row

One key frame per clip, 640x352 crop, `base_q_idx` 100. The skip is recorded
here because `av1-predict-r1` did not record its own, and its screen-capture
row (4808 B / 46.06 dB) is a *different frame* of the same file, not a
regression — that clip is 6.5 s long and its frames differ by 8 dB across it.

| clip | skip | main bytes | main PSNR | this branch | PSNR | Δ rate |
|---|---|---|---|---|---|---|
| Troy (1080p film) | 1920 s | 19125 | 40.61 dB | 19134 | 40.59 dB | +0.05% |
| OBS screen capture | 5 s | 9144 | 43.52 dB | 9112 | 43.52 dB | −0.35% |
| he_is_not_the_only_one | 5 s | 7535 | 44.76 dB | 7518 | 44.76 dB | −0.23% |

On synthetic pictures that genuinely run diagonally the same code is worth
−53.0% rate. The difference is block size: at 32x32, directional modes fire on
8 of 220 blocks in the film frame and 5 of 220 in the screen capture.

## Dispositions

- **accepted** — directional modes move real 640x352 frames by ≤0.35%. They
  are correct, bit-exact and default-on; the win they are capable of waits on
  block sizes below 32x32 and on the partition decision, which is its own lane.
- **accepted** — Troy is +0.05% rate / −0.02 dB, inside the noise of a single
  frame and the price of the lambda move that pays for the other two clips.
- **fix-now, done in this lane** — the search's blindness to mode signalling
  cost, left open by `av1-predict-r1`. Costed through the writer's CDFs above.

## Not in this lane

Angle deltas other than zero; block sizes other than 32x32 and the partition
decision; a coefficient rate term scored through the writer's CDFs (the mode
half is now done, the level half is still the magnitude estimate); transform
types beyond DCT_DCT; CDF adaptation; inter frames; pictures whose dimensions
are not a multiple of 32.
