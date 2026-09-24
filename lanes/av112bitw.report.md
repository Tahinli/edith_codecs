# lane-av112bitw — the 12-bit warp lift

Base 786c15b6 (lane-av112bitc's merged tree), worktree `edith_codecs-av112bw`,
branch `lane-av1-12bit-warp`. The charter: lift the named 12-bit warp refusal
("warped motion at 12 bits (warp's reduce bits inherit the 12-bit round_0 and
no 12-bit warp witness exists)") exactly as far as real `aomenc --bit-depth=12`
warp witnesses prove byte-exactness against ffmpeg, and keep grain and screen
content refused by name.

## Reaching the path (encode budget: 3 probes + 6 gate encodes pre-lift + 6 post-lift)

1. **Recipe 1 (the parent's refusal recipe, re-proven on this base)**: the
   12-bit sub-pel source (160x128 C420p12, gradient + texture, travelling
   box), six frames, `--enable-warped-motion=1` (global motion NOT spelled).
   The unmodified tree's gate `a_12bit_warped_motion_stream_is_refused_by_name`
   encoded it live and refused by name — warp emits, refusal fires. Post-lift
   the same recipe is the single-ref witness.
2. **Recipe 2 (negative result, reported per charter)**: same source, warp ON
   **and** `--enable-global-motion=1`. It decodes byte-exact post-lift but
   `compound_warp_hits` delta is **0**: pan/box content only fits TRANSLATION
   global models, and the compound-warp site needs model > TRANSLATION per
   slot (`is_global_mv_block`, blockd.h:421-429, pinned by lane-cwarp r1).
   NOT a compound-warp witness; the throwaway probe gate was deleted again.
3. **Recipe 3 (the lever)**: the lane-cwarp gate's rotating-mandelbrot recipe
   (`mandelbrot` + `rotate=a=rate*n`, 128x128 -> 64x64, 24 frames, 3 cq x 2
   rate) already hard-asserts `compound_warp_hits > 0` at 8/10 bits. Its
   12-bit arm refused pre-lift at the compound-warp site on the FIRST attempt
   (`refused on warp (cq 32)` — reachability + non-vacuity in one run), and
   post-lift it is the compound-warp witness.

## What changed

- **warp.rs** — the hard `REDUCE_BITS_HORIZ: i32 = 3` became
  `warp_round_0(bd)` = `ROUND0_BITS + max(bd + FILTER_BITS - ROUND0_BITS -
  14, 0)`, the exact `get_conv_params_no_round` bump (convolve.h:
  `intbufrange = bd + FILTER_BITS - round_0 + 2`; overflows 16 bits from
  bd 12 on, so `round_0` 3 -> 5). 3 at 8/10-bit (identical to before), 5 at
  12-bit. Everything else derives from it exactly as warped_motion.c:295-301
  reads: `reduce_bits_horiz` IS `conv_params->round_0`; the non-compound
  `reduce_bits_vert = 2*FILTER_BITS - round_0` (11 -> 9 at 12 bits, filter
  gain constant); `offset_bits_horiz`/`offset_bits_vert` keep their formulas.
  `warp_affine_compound`'s CONV_BUF blend bias is now derived
  (`(1 << (offset_bits - round_1)) + (1 << (offset_bits - round_1 - 1))`) —
  `(1 << (bd+4)) + (1 << (bd+3))` at 8/10 bits (identical to before), one
  octave down at 12 bits; compound `reduce_bits_vert` stays
  `COMPOUND_ROUND1_BITS = 7` (the bump never moves compound round_1).
- **decode.rs** — all three 12-bit warp refusal sites deleted: the
  compound-warp arm (`COMPOUND_WARP_HITS`, 16x16+ path), the 16x16+
  single-ref site and the 8x8-leaf site (both guarded
  `bit_depth == 12 && warp_params.is_some()`, covering the local
  WARPED_CAUSAL arm AND the single-ref global-warp arm — the filter is the
  same `warp_affine`, and the model source is bit-depth independent and
  10-bit-pinned). Header comment over the 12-bit family updated: the family
  is now grain + screen content only.
- **Found during the lift (recorded, closed by it)**: the 8x8-leaf
  compound-warp arm (`decode_inter_block8`'s `COMPOUND_WARP_HITS_8` site)
  had NO 12-bit refusal at all — a 12-bit stream whose first warp block was
  an 8x8 compound leaf would have decoded silently through the wrong
  shifts. The refusal map was incomplete, not just unlifted; post-lift the
  site runs the witnessed parameterised path like every other warp site.
- **refusal_inventory.rs** — the warp string left `REFUSALS` (38 -> 37) and
  its pair left `PROVEN`; both slots carry lane-av112bitw notes pointing at
  the witnesses. `the_decode_path_refuses_exactly_the_listed_cases` and
  `every_proven_refusal_names_a_test_that_exists` both green.
- **stream.rs** — the refusal gate was REPLACED by the single-ref witness
  (below), and `run_compound_global_warp_gate` was generalised
  `ten_bit: bool` -> `bd: u32` with a third caller. The 8/10-bit arms are
  byte-identical recipes (8-bit still passes NO depth flags).

## The witnesses

Encoder: the local aom oracle build (`~/.cache/aom-oracle/build/aomenc`,
libaom src `92d4c37fbdd08944a0e721bbaeb13318f10aebb0`); reference decode
ffmpeg 8.1.2 (`yuv420p12le` rawvideo). Gates encode live; the committed
fixtures are untouched (no new fixture needed).

1. **Single-ref** `a_12bit_warped_motion_stream_decodes_pixel_exact` — the
   exact recipe that refused pre-lift, 6 frames, byte-exact vs ffmpeg on
   every plane of every frame. Engagement: `warp_selected_hits` 7 (16x16+
   WARPED_CAUSAL), `warp_hits_8` 1 (usable 8x8 local model),
   `affine_gm_hits` 0 (content fits no >TRANSLATION global model — the
   single-ref global-warp arm rides on the same filter + the composition
   argument, not its own stream). Hard counter assert so a warpless stream
   cannot pass vacuously.
2. **Compound** `a_real_compound_global_warp_12bit_stream_decodes_pixel_exact`
   — the cwarp rotating-mandelbrot recipe at `--bit-depth=12`: **6/6
   attempts pixel-exact, 0 named refusals, compound_warp_hits = 28**, with
   the gate's own hard asserts (`hits > 0`, `matched > 0`, no refusal may
   contain "warp"). Pre-lift the same gate FAILED with the warp refusal on
   attempt 1 — non-vacuity both directions.

## Results

- 12-bit battery: key, inter, compound (parent fixtures) all still
  byte-exact; `a_12bit_screen_content_stream_is_refused_by_name` and
  `a_12bit_film_grain_stream_is_refused_by_name` still refuse by name.
- Warp family: 8-bit and 10-bit cwarp gates green (the parameterisation is
  a no-op at bd <= 10, proven not just claimed), both fimv
  force-integer-mv warp-alphabet gates green, both 10-bit film fixture
  gates with warp green (`a_10bit_128sb_film_frames_with_warp_cdef_and_interintra...`,
  `a_10bit_film_frames_with_small_side_globalmv_and_rect_warp_reach...`).
- `refusal_inventory` + `gate_coverage` suites green.
- `cargo check -p ec-av1 --all-targets`: 0 warnings.

## Open halves (recorded, not silently dropped)

1. **Single-ref GLOBAL-warp at 12 bits has no stream of its own** (engagement
   0 on this content): lifted on the composition argument — identical
   `warp_affine` filter, bit-depth-independent model source, both pieces
   witnessed. A stream whose single-ref blocks warp under a ROTZOOM global
   model would close it outright; the mandelbrot content produces those
   models only on compound blocks here.
2. **Grain and screen content stay refused by name** (untouched, per
   charter): `film grain synthesis at 12 bits ...` and
   `a 12-bit frame with screen content tools ...` with their real-stream
   refusal gates.

## Create-list audit

Exactly one new file: this report. Touched: `warp.rs` (the parameterisation),
`decode.rs` (three refusal deletions + header comment), `stream.rs` (witness
gate swap + gate generalisation), `refusal_inventory.rs` (two row removals +
notes). No fixture, no junk file.
