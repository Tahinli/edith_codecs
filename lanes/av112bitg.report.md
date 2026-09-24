# lane-av112bitg — 12-bit film grain joins the witnessed set

Base 786c15b6 (lane-av112bitc), worktree `edith_codecs-av112bg`. The charter:
lift the `film grain synthesis at 12 bits` refusal exactly as far as a real
`aomenc --bit-depth=12` grain stream proves byte-exactness against ffmpeg, and
leave warp and screen content refused. Sibling lane av112bitw owns warp;
`film_grain.rs` needed no arithmetic change, so the two lanes share no code
except the refusal inventory rows (grain rows only here).

## Emission (charter gate 1: passed on try 1)

`--film-grain-test=N` maps to libaom's `film_grain_test_vectors[N-1]`
(`av1/encoder/grain_test_vectors.h`); `av1_update_film_grain_parameters`
overwrites the vector's static `bit_depth: 8` with the sequence's
(`encoder_utils.c:792`), and `av1_update_film_grain_parameters_seq` turns on
`film_grain_params_present`. Probed all of vectors 1..8 on the lane's 12-bit
sub-pel fixture: every one emits `apply_grain=1` on BOTH the key and the inter
frame header (vectors 1..8, 2-frame streams, all parsed and printed by a
scratch gate before the lift). No try failed, so the "8 tries" stop condition
never came close.

## The bit-depth audit (no assumptions)

Every bit-depth-sensitive site of `film_grain.rs` re-diffed against the oracle
`~/.cache/aom-oracle/src/av1/decoder/grain_synthesis.c` (v3.13.3 @ 92d4c37):

| site | oracle | ours |
|---|---|---|
| `grain_min/max = ±(128 << (bd-8))` AR + blend clamp | `:1043-1045` | `grain_range` |
| `gauss_sec_shift = 12 - bd + grain_scale_shift` | `:468`, `:503` | both `generate_*_grain_block`s |
| `scale_LUT` shift-4 interpolation, `x == 255` short-circuit | `:616-626` | `scale_lut` |
| expanded LUT (4096 entries at 12-bit) | per-call `scale_LUT` | `expand_scaling_lut` (same function tabulated) |
| chroma index combine `((avg*luma_mult + mult*chroma) >> 6) + offset`, clamp `0..(256 << (bd-8)) - 1` | `:815-819`, `:830-834` | `chroma_noise_row_scalar` + ChromaNoise |
| `cb/cr_offset = (x << (bd-8)) - (1 << bd)` | `:752`, `:757` | `add_noise_to_block` |
| legal-range clamps `16/235/240 << (bd-8)`, `mc_identity` chroma quirk | `:781-795` | `add_noise_to_block` |
| 4:2:0 chroma luma average `(l0 + l1 + 1) >> 1` (horizontal PAIR, not a quad) | `:801-806` | `chroma_noise_row_scalar` |
| ver/hor overlap blends clamped to `grain_min/grain_max` | `:912-970` | `ver_overlap_inplace`/`hor_overlap_inplace` |

The AVX2 kernels are parameter-shaped (`index_max`, min/max, LUT length all
inputs; the `vpmaddwd` luma pair sum stays exact at 12-bit since
`2 * 4095 < 2^15`) and this machine's decode path runs them (chroma blocks are
15 wide -> 8+4+3 tails; luma 30 wide -> 8+8+8+4+2), so the witness exercised
SIMD and scalar alike. `apply_grain` call sites already thread the parsed
sequence `bit_depth` (the `map_or(8, ...)` site included).

## The witness

`stream.rs`: `grain_refusal` and its four call sites deleted; the refusal gate
became `a_real_aomenc_12bit_film_grain_stream_decodes_pixel_exact`:

- `aomenc --profile=2 --bit-depth=12 --film-grain-test=5
  --enable-warped-motion=0 --enable-global-motion=0`, 2 frames over the lane's
  160x128 C420p12 sub-pel fixture. Vector 5 carries 2 luma + 9 cb + 9 cr
  scaling points, `overlap_flag=1`, `clip_to_restricted_range=1`; the inter
  frame codes `update_grain=0` (params inherited from the ref slot, new seed
  only), so the gate also exercises the spec 5.9.30 persistence path and grain
  on a non-key output.
- Warp is pinned OFF by name: the unpinned first run tripped the (sibling's,
  intact) `warped motion at 12 bits` refusal — this lane lifts GRAIN only, so
  the arm stays inside the witnessed set. Each flag spelled exactly once
  (aomenc LAST-occurrence precedence).
- Non-vacuity: parsed-header asserts demand `apply_grain`, all three planes'
  scaling points, overlap and restricted-range clipping on BOTH frames;
  `film_grain::grain_hits()` must advance; ffmpeg decodes with grain ON by
  default and must produce exactly 2 yuv420p12le frames.
- Result: **byte-exact on every plane of both frames** (grain_hits=2). ffmpeg
  synthesized the identical grain from the identical reconstruction.

## Inventory

`refusal_inventory.rs`: grain REFUSALS string and PROVEN row deleted (37 -> 36
refusals), lane-av112bitg notes added next to the lane-av112bitc ones.
`the_decode_path_refuses_exactly_the_listed_cases` and
`every_proven_refusal_names_a_test_that_exists` both green.

## Regression (all green, same run)

- `a_real_aomenc_12bit_stream_decodes_pixel_exact` — parent 12-bit KEY fixture, grain off, byte-exact.
- `a_real_aomenc_12bit_inter_sequence_decodes_pixel_exact`,
  `a_real_aomenc_12bit_compound_inter_sequence_decodes_pixel_exact` — 12-bit inter/compound witnesses.
- `a_real_aomenc_10bit_film_grain_stream_decodes_pixel_exact` + the other four `10bit_film` gates.
- `a_12bit_warped_motion_stream_is_refused_by_name`,
  `a_12bit_screen_content_stream_is_refused_by_name` — sibling refusals intact.
- Full `refusal_inventory` module. 26/26 tests, 0 warnings on
  `cargo check --all-targets -p ec-av1`.

## Open

None for grain. Warp (`a_12bit_warped_motion_stream_is_refused_by_name`,
sibling lane av112bitw) and screen content tools stay refused by name.
