# lane-tiny r4 report

Rebased onto main `3808cf8` first (r3 was already merged there); branch head `9b1297a`.

## Residue closed: 32x16 seeds 42 and 47, chroma-only

### Fixture determinism (charter step 1)
The sweep's "seed" is not an encoder seed -- it is `attempt`, which shifts
`mandelbrot` start_x/start_y. Regenerated both failing 32x16 fixtures twice
each and hashed:

    attempt 0 (seed 42): c72a6a1b...5659 both runs, 152 bytes
    attempt 5 (seed 47): fcbf0894...c388 both runs, 151 bytes

So the sweep is genuinely reproducible and no binary fixture needed pinning
(per `seeded-fixture-not-reproducible.md`, checked rather than assumed).

### Bisection (charter step 2)
Not an entropy defect -- the range ladder was never needed. Stage dumps
settled it in one step:

| stage | Y | U | V |
|---|---|---|---|
| pre-filter recon (`EC_AV1_PREFILT_DUMP`, ours vs aomdec) | 0 diff | **0 diff** | 0 diff |
| post-deblock (`EC_AV1_POSTDEBLOCK_DUMP`, ours vs aomdec) | 0 diff | **5 diff** | 0 diff |

Reconstruction (and therefore the whole entropy/prediction path) is exact;
the deblocker introduces the mismatch. Printing the U plane rows 0..7 shows
the oracle leaves the horizontal edge at chroma row 4 **completely
unfiltered**, while we filter it and move rows 3/4 at columns 1, 2, 15.

## Root cause

`av1_get_max_uv_txsize`: a block's chroma transform is the largest transform
covering the **subsampled BLOCK**, capped at 32x32. It does **not** follow
luma's `tx_depth` split. `tx_px_at`/`tx_h_px_at` (decode.rs) computed it as
`tx_grid[...] / 2` -- half the *resolved luma* transform. A 16x16 block whose
luma transform split to 8x8 therefore read a 4-pixel chroma transform, and the
deblocker filtered a chroma transform edge that does not exist in the stream.

Reconstruction was never wrong because it derives chroma from the block side
(`chroma_side = side / 2`, decode.rs 4313/4850/8881) -- which is exactly why
the symptom was luma-bit-exact, chroma-only, +-1, and seed-dependent (the
phantom edge only changes pixels on the seeds where the filter masks pass).
V was clean on both seeds for the same reason: its masks did not pass.

**Fix** (decode.rs): `Neighbours::uv_tx_grid` / `uv_tx_h_grid`, written per-mi
from the block's own mi span inside `fill_lf_grid_rect` -- the single writer
every `fill_lf_grid*` caller routes through -- as
`((span_mi * MI / 2).max(4).min(32))`; `tx_px_at`/`tx_h_px_at` read those
grids for chroma instead of halving luma's.

**Class sweep**: grepped every other chroma-from-luma derivation.
`tx_px_at`/`tx_h_px_at` were the only sites deriving chroma tx from the luma
*transform*; 4313/4850/8881 derive from the block *side* and are correct.
No sibling instances.

## Gate (charter step 4)

`probe_tiny_frame_size_boundary` -> `stream.rs`
`a_real_aomenc_tiny_frame_size_sweep_decodes_pixel_exact`, no `#[ignore]`:
- wrong pixels at any size/seed = panic (was: an eprintln)
- a decode error that is not a NAMED `unsupported:` refusal = panic
- floor assert `total_exact >= 70` so refusals can never make it vacuous
  (per `gate-skips-on-its-own-failure.md` / `refusal-lifted-without-a-gate.md`)
- `EC_AV1_REQUIRE_AOMENC=1` makes an absent oracle fail, not skip.

`gate_coverage.rs` needed no change: it derives the tool set from `stream.rs`
source and this gate's `--enable-*` flags are unchanged (it was already in the
derived gate set while `#[ignore]`d). `gate_coverage` + `refusal_inventory`
are green.

| size | r3 | r4 |
|---|---|---|
| 8x8 | refused 10/10 | refused 10/10 (named: 16x16 block whose true edge cuts both axes needs a rect transform) |
| 16x16, 32x32, 16x32, 48x48, 64x64, 24x24 | 10/10 | 10/10 |
| **32x16** | **8/10** | **10/10** |

No refusal string was lifted this round (the 8x8 rect-transform refusal stays;
it is a genuine missing capability, now pinned by the gate).

## Instruments added
- `EC_AV1_POSTDEBLOCK_DUMP` / `EC_AV1_POSTCDEF_DUMP` (decode.rs `dump_stage`),
  mirroring aomdec's own `EC_AV1_POSTDEBLOCK_DUMP` (decodeframe.c ~5404) -- with
  the existing pre-filter dump these bisect which filter stage broke a frame.
- `decode_probe <stream.obu> [out.yuv]` now writes the decoded planes as raw
  yuv420p, so a pixel diff against `ffmpeg -f rawvideo` needs no test harness.

## Verification

- `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib`: **268 passed, 0 failed,
  22 ignored** (362s).
- New gate alone: 9.5s.

EVIDENCE: $SCRATCH/pre0.bin.f0 vs oracle0.bin.f0, pd0.bin.f0 vs opd0.bin.f0 | EC_AV1_PREFILT_DUMP + EC_AV1_POSTDEBLOCK_DUMP on ours and instrumented aomdec, same 32x16 seed-42 stream | pre-filter U diff 0, post-deblock U diff 5 -> defect is in deblock, not recon
EVIDENCE: $SCRATCH/ours0.yuv vs ff0.yuv, ours5.yuv vs ff5.yuv | decode_probe a{0,5}.obu out.yuv, cmp against ffmpeg rawvideo | both byte-identical after the fix (before: U 4 and 5 differing samples)
EVIDENCE: cargo test -p ec-av1 --lib a_real_aomenc_tiny_frame_size_sweep -- --nocapture | 8 sizes x 10 seeds, real aomenc -> ours -> ffmpeg | 32x16 8/10 -> 10/10; 70 pixel-exact + 10 named 8x8 refusals; gate green

## Open

- 8x8 frames: `deferred(a real rectangular TX_4X8/TX_8X4 coefficient path)` --
  refused by name, now pinned by the gate; the same square-only-transform
  ceiling `square-only-transform-ceiling.md` already tracks.
- Repo-wide `cargo fmt --check` drift exists in `cdf.rs` and elsewhere and
  predates this lane: `accepted`, not touched (formatting the crate would bury
  this diff).
