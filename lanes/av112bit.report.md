# lane-av112bit — the 12-bit decode gate

Base 73f3ac64, worktree `edith_codecs-av112b`. The charter: lift the blanket
`bit_depth == 12` refusal in `stream.rs` (`lanes/av1hdr.report.md` §4.2 named
the sites) exactly as far as real `aomenc --bit-depth=12` witnesses prove
byte-exactness against ffmpeg, and keep every unwitnessed 12-bit path refused
by name.

## The libaom rule (what actually moves at 12 bits)

`av1/common/convolve.h:68..100` (`get_conv_params_no_round`) and
`:107..118` (`get_conv_params_wiener`):

```c
const int intbufrange = bd + FILTER_BITS - round_0 + 2;
if (intbufrange > 16) { round_0 += intbufrange - 16; if (!is_compound) round_1 -= intbufrange - 16; }
```

i.e. `delta = bd - 10` (zero at 8/10-bit; 2 at 12-bit). Horizontal `round_0`
3 -> 5; non-compound vertical `round_1` 11 -> 9 (product, the filter gain,
unchanged); compound `round_1` stays 7, so the CONV_BUF gain drops from 16x
to 4x and every combine round absorbs the delta. Warp inherits the same
`round_0` (`warped_motion.c:295`, with the assert at `:310` pinning
`bd + FILTER_BITS + 2 - round_0 <= 16`) — §4.2's "`round_0 + max(...)` (5 at
bd 12)" and the retracted-reviewer note stand, expressed through the conv
params.

## What changed (witnessed paths, lifted)

- **`mc.rs`** — `round_delta`/`round_pair` implement the convolve.h rule.
  The shift is threaded through `hpass_row`/`hpass_contig`/`hpass_rows`/
  `hpass_avx2`/`hpass_sse2`, `vpass_row_u16` + AVX2/SSE4.1 kernels and
  scalars (the `srai` immediates became vector-count `sra`), the identity-tap
  fast paths (`gain = 128 >> round_0`, `t * (128 >> round_0)`,
  `round2(a, round_1 - 7)`), `horizontal_scaled_pass`, and
  `predict_compound_intermediate_kern` (which now takes `fctx`). The compound
  combines absorb the CONV_BUF gain drop:
  `combine_compound`/`blend_masked_compound` round by
  `INTER_POST_ROUND - delta`, `diffwtd_mask` by
  `INTER_POST_ROUND + (bd - 8) - delta` (the `reconinter.c:307` formula).
  `horizontal_intermediate_fits_i16` now pins the 12-bit bound
  (`round2(348 * 4095, 5) = 27976 < 2^15`); the SIMD tests already spanned
  12-bit intermediates.
- **`restoration.rs`** — the Wiener stripe runs its horizontal pass at the
  frame's `round_0` and vertical at `round_1` (9 at 12-bit). SGR needed
  nothing: `compute_ab`/`ab_point` were already `bd_shift = bd - 8`
  parameterised (the lane-hbdinter fix), and the oracle's
  `calculate_intermediate_result` matches it exactly.
- Everything else 12-bit-relevant was already bit-depth generic and
  10-bit-proven: the 12-bit dequant tables (`quant.rs`), deblock
  (`sclamp`/threshold `<< bd - 8`), CDEF (`coeff_shift = bd - 8`,
  `damping += coeff_shift`, exactly `av1_cdef_filter_fb`), film grain
  (`grain_center`/`scale_lut`/LUT expansion, lane-hbd10), superres upscale
  clamp, palette colour reads, `sample_max` clamps.

## What stayed refused (unwitnessed, by name — real-stream gates)

| refusal | site | gate (real aomenc stream) |
|---|---|---|
| `warped motion at 12 bits ...` | decode.rs warp decision sites (compound, 16x16+, 8x8 leaf), parse-time | `a_12bit_warped_motion_stream_is_refused_by_name` |
| `a compound inter block at 12 bits ...` | both leaves' `read_inter_compound_mode` callers | `a_12bit_compound_inter_stream_is_refused_by_name` |
| `film grain synthesis at 12 bits ...` | all four `apply_grain` output sites (`grain_refusal`) | `a_12bit_film_grain_stream_is_refused_by_name` |
| `a 12-bit frame with screen content tools ...` | stream.rs frame gate (`allow_screen_content_tools`) | `a_12bit_screen_content_stream_is_refused_by_name` |

The screen-content gate covers palette **and** intrabc with one honest frame
level refusal: both hang off `allow_screen_content_tools`, neither has a
12-bit witness, and refusing the frame keeps both code paths 12-bit
unreachable instead of unwitnessed-and-trusted.

OBMC, interintra, palette reads, intrabc copies and superres-scaled MC are
NOT separately gated: their arithmetic is bit-depth independent and their
bd-sensitive inputs (MC predictions, samples) are witnessed at 12 bits —
the composition of two proven pieces. The scaled-MC walk shares the exact
parameterised round path; the 10-bit superres gate pins the walk itself.

## The witnesses

Source (Rust `y4m_12bit_subpel_source`, byte-identical to the manual
generator): 160x128 C420p12, gradient + fine texture, a 24x20 box travelling
1.25 px/frame horizontally and 0.75 px/frame vertically with linear edge
coverage — sub-pixel true motion, so the inter frame cannot be coded
whole-pel-only (proven: `MC_SUBPEL_HITS > 0`, a new counter).

Recipe (both witnesses): `aomenc --codec=av1 --profile=2 --bit-depth=12
--input-bit-depth=12 --passes=1 --end-usage=q --cq-level=20 --cpu-used=0
--threads=1 --row-mt=0 --sb-size=64 --lag-in-frames=0 --auto-alt-ref=0
--kf-max-dist=1000 --enable-cdef=1 --enable-restoration=1
--enable-warped-motion=0 --enable-global-motion=0 --obu --limit=<N>`,
N=1 (key) / N=2 (key + one P frame).

- `crates/ec-av1/fixtures/av112bit-key.obu` — 923 bytes,
  sha256 `c74f62b57f71f7c36c49ea1feeb482786a2650e5c7f66610949b5f65fbbed8d1`
- `crates/ec-av1/fixtures/av112bit-inter.obu` — 4533 bytes,
  sha256 `e679d577e61b4a8bc99685355cca8c0473cfb637284b07786a6c53b1755553d9`

Encoder: the local aom oracle build (`~/.cache/aom-oracle/build/aomenc`,
libaom src `92d4c37fbdd08944a0e721bbaeb13318f10aebb0`); reference decode
ffmpeg 8.1.2 (`yuv420p12le` rawvideo). The gates encode live from the
in-test generator and pixel-compare every plane of every frame against
ffmpeg's decode of the identical bytes; the committed fixtures are the same
recipe's output for provenance (the inter fixture byte-equals the manual
lane run, `cmp` clean).

Gate asserts beyond the pixel match, so none can pass vacuously:
key — `cdef_idx_hits > 0` (CDEF index literals read) and Wiener+SGR unit
count advanced; inter — `MC_SUBPEL_HITS > 0` (the parameterised round pair
actually ran) and `compound_mode_hits` UNCHANGED (the single-reference
premise the compound refusal leans on).

## Results

- 12-bit battery (6 gates) green; the two witnesses byte-exact vs ffmpeg.
- 10-bit regression: `a_real_aomenc_10bit_stream_decodes_pixel_exact`,
  `a_real_aomenc_10bit_inter_sequence_decodes_pixel_exact`,
  `a_real_aomenc_10bit_film_grain_stream_decodes_pixel_exact`, and the three
  film fixture gates (`a_10bit_128sb_film_frames_with_warp_cdef_and_interintra...`,
  `a_10bit_film_frames_with_rect64_corner_tus...`,
  `a_10bit_film_frames_with_small_side_globalmv_and_rect_warp_reach...`) all
  green — the two charter-named film gates included.
- 8-bit spot regression (monochrome, lossless, real-aomenc arms) green.
- `refusal_inventory` + `gate_coverage` tests green (inventory diff = the
  capability delta above).

## Open halves (recorded, not silently dropped)

1. **Wiener intermediate clamp at 12 bits.** A first attempt replaced the
   corpus-proven constant clamp with the C clamp's exact per-sample
   transform (add_src bias surviving `Round2(_, round_0)`) — it MOVED
   `a_10bit_128sb_film_frames...` frame 1 luma: libaom's 8-tap-loop
   pointer-alignment trick means the C horizontal intermediate is not
   `ours + bias` wherever the clamp actually binds, so the exact transform
   is not derivable from the convolve source alone. The shipped code keeps
   the 8/10-bit-proven constant structure (`[-2^(bd+3), 2^(bd+5)-1-2^(bd+3)]`,
   i16-safe; at 12-bit the widest i16 window, which strictly contains the
   kernel's mathematical range and so never binds — the same property the
   8/10-bit bounds have on the proven corpus). The 12-bit key witness is
   byte-exact with it; a stream whose intermediates would trip C's real
   clamp is unproven either way. Disposition: `accepted` (deferred to a
   lane that instruments the oracle's wiener path directly).
2. **Compound, warp, grain, palette, intrabc at 12 bits** — refused by name
   with real-stream gates; lifting each needs its own byte-exact witness
   (compound of every kind, warp at the 12-bit `round_0`, grain synthesis,
   screen content).
