# lane-av112bitc — the 12-bit compound lift

Base 0899977a (lane-av112bit's merged tree), worktree `edith_codecs-av112bc`,
branch `lane-av1-12bit-comp`. The charter: lift the named 12-bit compound
refusal the parent left ("a compound inter block at 12 bits ...") exactly as
far as a real `aomenc --bit-depth=12` compound witness proves byte-exactness
against ffmpeg, and keep warp, grain and screen content refused by name.

## Reaching the path (encode 1 of the 8 allowed)

The parent had already measured a compound-reaching recipe: the lane's own
sub-pel source, six frames, warp pinned OFF — with six frames LAST diverges
from GOLDEN from frame 2 on and aomenc codes compound inter blocks. Pre-fix
proof: the unmodified tree's gate
`a_12bit_compound_inter_stream_is_refused_by_name` encoded that stream live
and refused it at the compound mode read (decode.rs, both
`read_inter_compound_mode` callers). Reaching + non-vacuity in one
measurement, and the same recipe then became the witness.

## What changed

- **decode.rs** — both 12-bit compound refusal sites deleted (the block-level
  arm and the 8x8-leaf arm). The `bit_depth(fctx) == 12` guard sat AFTER the
  compound mode symbol read and BEFORE any prediction, so the lift changes
  nothing for 8/10-bit: the parse simply proceeds into `assign_compound_mv`
  and the prediction path the parent already parameterised.
- **mc.rs** — NO arithmetic change. The parent's parameterisation already
  covered every combine: `combine_compound` rounds by
  `INTER_POST_ROUND - round_delta` (CONV_BUF gain 16x -> 4x at 12-bit,
  `round_0` 5, compound `round_1` 7), `diffwtd_mask` by
  `INTER_POST_ROUND + (bd-8) - round_delta` (reconinter.c:307),
  `blend_masked_compound` by `INTER_POST_ROUND - round_delta`. Only the
  stale doc sentence on `combine_compound` ("decode.rs still refuses those
  by name") was corrected.
- **decode.rs** header comment over the 12-bit family updated: the family is
  now warp-only; compound joined the witnessed set.
- **`mc_subpel_hits`** gained the repo-standard
  `#[allow(dead_code)] // reader is test-only` (its only reader is the
  parent's 12-bit inter gate; non-test builds warned — this tree now checks
  `cargo check -p ec-av1 --all-targets` warning-free, which the charter
  demands).
- **refusal_inventory.rs** — the compound string left both the inventory and
  the gate-coverage mapping; inventory 38 -> 37, all still proven, every
  proven refusal still names an existing test.
- **stream.rs** — the compound refusal gate was REPLACED by the witness
  `a_real_aomenc_12bit_compound_inter_sequence_decodes_pixel_exact` (below).

## The witnesses

Source: the parent's `y4m_12bit_subpel_source(6)` (160x128 C420p12,
gradient + texture, a 24x20 box moving 1.25 px/frame horizontally and
0.75 px/frame vertically), encoded live in-gate by
`~/.cache/aom-oracle/build/aomenc` (libaom src `92d4c37fbdd08944a0e721bbaeb13318f10aebb0`),
recipe = the parent's `encode_12bit` base (`--profile=2 --bit-depth=12
--passes=1 --end-usage=q --cq-level=20 --cpu-used=0 --threads=1 --row-mt=0
--sb-size=64 --lag-in-frames=0 --auto-alt-ref=0 --kf-max-dist=1000
--enable-cdef=1 --enable-restoration=1 --obu --limit=6`) plus, spelled exactly
once, the arm flags. Two arms:

1. **`dist-wtd`** — `--enable-warped-motion=0` (dist-weighted average
   combines live): census compound blocks=15, masked=1, diffwtd=1, wedge=0.
2. **`plain-masked`** — `--enable-warped-motion=0 --enable-dist-wtd-comp=0`
   (pushes compound and the masked choice harder): census compound blocks=18,
   masked=6, diffwtd=6, wedge=0.

Both arms byte-exact against ffmpeg 8.1.2 (`yuv420p12le` rawvideo) on every
plane of every frame, with hard asserts `compound_mode_hits > 0`,
`masked_compound_hits > 0`, `diffwtd_hits > 0` (all deterministic: aomenc
`--threads=1 --passes=1` on pinned bytes).

Committed fixtures (same live recipes, determinism proven by `cmp` of a
re-encode against the committed bytes):

- `crates/ec-av1/fixtures/av112bit-compound.obu` — 7486 bytes, sha256
  `a839cff534b89d5cd52681dd43a7dedc10131d8dd7725bdfa5502d6bc4421740`
- `crates/ec-av1/fixtures/av112bit-compound-masked.obu` — 7470 bytes, sha256
  `83625de2ed347d457eb67f9781eed3fea598ef035f29bbc311d7025fe178c691`

Both fixtures additionally verified standalone: `dump_yuv` (the crate's own
decoder) vs `ffmpeg -pix_fmt yuv420p12le -f rawvideo`, whole-stream `cmp`
clean for all 6 frames.

## Regression

- Parent 12-bit key witness `a_real_aomenc_12bit_stream_decodes_pixel_exact`
  — byte-exact, CDEF + restoration asserts green.
- Parent 12-bit single-ref inter witness
  `a_real_aomenc_12bit_inter_sequence_decodes_pixel_exact` — byte-exact;
  its `compound_mode_hits` UNCHANGED premise still holds on the 2-frame
  recipe (doc wording updated: compound no longer "stays refused", the
  2-frame recipe simply never reads a compound symbol).
- `a_12bit_warped_motion_stream_is_refused_by_name` — still refuses by name
  (warp stays unwitnessed; the compound lift does not reach it first).
- 10-bit film gates: `a_real_aomenc_10bit_film_grain_stream_decodes_pixel_exact`
  (pixel-exact, grain_hits=1) and
  `a_10bit_128sb_film_frames_with_warp_cdef_and_interintra_decode_pixel_exact`
  (15 shown frames pixel-exact on every plane) — green.
- `refusal_inventory` module green (37 refusals + 1 capability claim, all
  proven); `gate_coverage::print_never_on_per_bit_depth` green.
- `cargo check -p ec-av1 --all-targets`: **0 warnings**.

## Open halves (recorded, not silently dropped)

1. **Wedge-masked compound at 12 bits** — unwitnessed. Two recipes on the
   sub-pel corpus never made aomenc pick COMPOUND_WEDGE (`wedge_hits == 0`
   in both arms); the 8-bit wedge gate only reaches wedge on hard-edge
   content with attempt loops, which this lane's charter forbids
   (deterministic single-recipe encodes). The lift is still honest per the
   parent's own composition rule: the wedge arm and the diffwtd arm run the
   SAME `blend_masked_compound` (witnessed byte-exact at 12 bits via six
   diffwtd blends), differing only in the u8 mask bytes, and the wedge
   codebook is bit-depth independent and 8-bit-witnessed (lane-wedge r3).
   Disposition: `accepted` — a future wedge witness needs only an
   aomenc recipe that picks wedge at 12 bits.
2. Warp, film grain synthesis, screen content (palette/intrabc) at 12 bits
   — unchanged, refused by name exactly as the parent left them.

## Process note

One fixture encode initially landed in the MAIN checkout's
`crates/ec-av1/fixtures/` (a shell call whose cwd did not apply); both files
were moved into this worktree unmodified (sha256s above taken after the
move) and `git status --short` on the main checkout shows nothing from this
lane. The parent worktree was never touched.
