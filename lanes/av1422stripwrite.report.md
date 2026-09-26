# lane-av1-422stripwrite — the 4:2:2 residual was NOT the rect-strip
# chroma write/anchor: a reference picture's chroma plane claimed
# `h / 2` rows, so every chroma MC sourced at or past the frame's
# midpoint was clamped; the pinned 16-frame t422 stream now decodes
# byte-exact

## Outcome

The parent lane's named site is **fixed, and it was not where the parent
pointed**. Frame-1 prefilt chroma no longer first-diffs at U/V (88, 104):
after the fix **all 16 frames of the pinned t422 stream are byte-exact on
every plane at prefilt, post-deblock AND post-CDEF, and the final
(post-loop-restoration) output is byte-identical to the oracle for all 16
frames** — 0 diffs, measured. The 4:2:2 header refusal stays.

## Localization: the write and the anchor were both already right

The parent read the site as "our written rows 88..91 are not the oracle's
MC output" and suspected the strip write/anchor. Measured, in order:

1. **The block is not what the parent said.** The oracle's own dispatch
   trace (`SUB48D`, added to `build_inter_predictors` in
   `reconinter_template.inc`) puts the site on **two `BLOCK_8X4` blocks at
   luma (88, 208) and (88, 216)** — `bsize=2`, `mi=(208,88)` / `mi=(216,88)`,
   one per mi column, i.e. the top strips of the 8x8 groups at mi (11,26)
   and (11,27), each coded its own chroma `TX_4x4` at chroma (88,104) and
   (88,108) (`is_chroma_reference` at ss (1,0): `mi_size_wide[BLOCK_8X4] == 2`
   in libaom's 4x4-mi units, so the column clause `!(bw & 1)` is true and
   BOTH are chroma references — `av1_common_int.h:1454`). The parent's
   "mi(22,52)" is this decoder's own 4x4-mi coordinate for the same block
   (`const MI: usize = 4`, `decode.rs:6052`), not libaom's 8x8 mi, and its
   "chroma cols 104..111, rows 88..95" spans TWO mi columns and two 8x8
   groups. There is no strip-2 chroma MC because the strips below
   (luma rows 92..95) are INTRA, not because the pair merged.
2. **The oracle's prediction is a pure reference copy** (`AOMIN48`/
   `AOMOUT48`, both inert without the env var): `row=88 col=104 bw=4 bh=4
   sx=0 sy=0 srcoff=17000 stride=192` → source = frame 0's final U at
   (88, 104) exactly, output `r0 = 202 204 204 167` = the source bytes.
   The oracle's final chroma there is the same value, so the strip is a
   zero-residual copy.
3. **Our MC inputs were identical and correct.** A temporary rung in
   `decode_inter_sub8_rect2`'s 4:2:2 per-piece arm dumped the raw
   reference samples at the unit before the MC:
   `unit=(104,88) stride=128 tw=128 th=72 xq4=1664 yq4=1408
   raw=[202, 204, 204, 167, 185, 190, 190, 165, 158, 164, 163, 148,
   145, 150, 148, 138]` — byte-identical to the oracle's `src0`. So the
   reference *contents*, the position, the mv, the filters and the scale
   were all right, and the 4x4 write rect `(unit_x, unit_y, 4, 4)` was
   right. What was wrong was that the MC turned those correct inputs into
   `206 204 202 200` x4 rows.
4. **The MC output was the tell.** `th=72` on a 144-tall frame. The
   reference plane the MC read declared **72 chroma rows** while holding
   144. Rows at/past 72 are out of the plane's valid area, so
   `predict_with_filters` clamped them — the four output rows came from a
   clamped row, which is why they were identical to each other and why
   they matched nothing in the reference.

## Root cause, class `ref-chroma-shape-420-folds-422`

`RefPix::planes` (`decode.rs:30311`) builds each reference's chroma
`PlaneBuf` from a **two-way** shape test:

```rust
let full = g.u.len() == g.width * g.height;
let (cw, ch) = if full { (g.width, g.height) } else { (g.width / 2, g.height / 2) };
```

A stored reference carries no subsampling field, so the code inferred it
from the sample count — but the inference only knew 4:4:4 (luma-sized) and
4:2:0 (a quarter). A 4:2:2 reference is **half-width, full-height**:
`(w/2) * h`, which matches neither, so it fell into the 4:2:0 branch and
claimed `h / 2` rows. The stored data was never wrong (the frame is stored
through `round_ss(fw, ss_x) x round_ss(fh, ss_y)`, `decode.rs:25806`, which
is 4:2:2-correct) — only the *declared* shape was, and
`whole_plane` copies it into `width`/`height`/`true_*`, which is what every
chroma MC and every edge clamp reads.

Consequence: at 4:2:2, **every** inter chroma prediction whose source row
was at or past the frame's vertical midpoint read clamped garbage, on every
subsampling-independent path (square blocks, rect strips, sub-8 pieces,
compound, OBMC). On the pinned fixture the same rung sweep found four
`mv = 0` per-piece units wrong, all with `unit_y >= 72`: (104,88),
(108,88) [the named site] and (80,120), (80,124) — the parent's residual
count was one symptom, not the site.

## Fix

One helper, both call sites (`decode.rs`):

```rust
fn ref_chroma_shape(len: usize, width: usize, height: usize) -> (usize, usize) {
    if len == width * height { (width, height) }            // 4:4:4
    else if len == (width / 2) * height { (width / 2, height) }  // 4:2:2
    else { (width / 2, height / 2) }                        // 4:2:0
}
```

The sample count is exact and unambiguous for all three chroma formats, so
no new field has to be threaded. `Picture`'s `u`/`v` doc comment
(`encode.rs`) was corrected to state the real contract (half width, the
frame's own chroma height) — the inference above depends on it.

**Untouched, provably:** the current frame's chroma planes were already
`(width >> ss_x) x (height >> ss_y)` (`decode.rs:27313`, `41692`); the
`4:2:0` group-tail chroma SKIP arm (its owning lane); `stream.rs`'s 4:2:2
header refusal (byte-identical, unconditional — the local
`EC_AV1_ALLOW_422_PROBE` bypass was applied for the probe runs and
REVERTED).

## Verification

- **Before/after on the named fixture** (`/tmp/i422/t422_2f.obu`, sha256
  `d166ebf3c1bde8c32152cd9d077a317b4c7205b6538f0a7481e09fb6333ffdf4`;
  decode_probe + `EC_AV1_{PREFILT,POSTDEBLOCK,POSTCDEF}_DUMP` vs the
  oracle's): frame 0 exact before and after; frame 1 before: Y 0, U 2231
  (first (88,104) ours 206 / oracle 202), V 2197 (first (88,104) ours 212 /
  oracle 231) — byte-identical to the parent lane's reported counts, so
  the reproduction is sound; frame 1 after: **0 / 0 / 0**.
- **16-frame ladder** (`/tmp/i422/t422_16f.obu`, sha256
  `dda96300d2d36ba9dc958febb033e2334a2caee137bcb286c0276f259e57d5b6`):
  **0 diffs on every plane of every frame 0..15 at prefilt, post-deblock
  and post-CDEF** (48 stage-frame-plane groups), and the final
  post-loop-restoration dump is **byte-identical (73728 B) for all 16
  frames**. Re-measured after the probe-bypass revert/re-apply round trip:
  48 groups, 0 diffs, 16/16 final. The oracle ladder was regenerated with
  the current build and is byte-identical to the parent lane's on every
  frame where both exist.
- **4:2:0/4:4:4 identity, measured:** `ref_chroma_shape` takes the *same*
  branch as the old two-way test for both other formats — 4:4:4 hits
  `len == width * height` first, and a 4:2:0 plane's `(w/2) * (h/2)` can
  only equal `(w/2) * h` at `h == 0` — so they are value-identical by
  construction, and the gates confirm it: `cargo test -p ec-av1 --lib --
  lossless decodes_sample_exact 444` (target dir
  `~/.cache/cargo-target-av1422stripwrite`) is **16 passed, 0 failed, 3
  ignored** — `a_lossless_libaom_key_frame_decodes_sample_exact`,
  `a_lossless_libaom_inter_frame_decodes_sample_exact`,
  `a_real_aomenc_mixed_lossless_segment_frame_decodes_sample_exact`,
  `a_lossless_sb128_rect_intra_block_decodes_sample_exact`,
  `a_lossless_sb128_square_frame_clips_overhanging_chroma_tus`,
  `a_lossless_16x4_chroma_pair_repairs_the_measured_site`,
  `a_skipped_lossless_intrabc_rect_strip_zeroes_its_entropy_bands`, the
  WHT round-trip, the tile-layout dual-decoder sweep and the six
  64x64-root encode/decode gates.
- **Oracle instrumentation (measurement, not decode).** The oracle's
  `EC_AV1_POSTCDEF_DUMP` rung sat inside `if (!optimized_loop_restoration)`,
  so a frame with no CDEF and no superres produced **no** post-CDEF dump —
  frame 15 got none, which is why the parent lane's "f15 postcdef Y=5059"
  had no oracle counterpart to compare against (the file it names,
  `/tmp/i422/av1422mi24/postcdef.f15`, is 81920 B — this decoder's own
  padded dump, not an oracle dump). The rung is now a shared
  `ec_dump_postcdef()` called from both branches; the oracle confirms
  frame 15's post-CDEF equals its post-deblock (no CDEF on that frame),
  and our frame 15 post-CDEF is byte-identical to it.
- `cargo check -p ec-av1 --all-targets` (target dir
  `~/.cache/cargo-target-av1422stripwrite`): **0 warnings, 0 errors**.
- `git status` names `decode.rs`, `encode.rs` (doc comment) and this
  report. Every temporary rung (`EC_SW422` in the 4:2:2 per-piece arm and
  in `build_c`; `SUB48`/`SUB48D`/`SUB48P` in the oracle's
  `reconinter_template.inc`) was removed before the commit.

## State

- Commit on `lane-av1-422stripwrite` (no push): the `ref_chroma_shape`
  helper + its two call sites in `crates/ec-av1/src/decode.rs`, the
  `Picture` chroma-shape doc correction in `crates/ec-av1/src/encode.rs`,
  and this report.
- **No residual to hand on.** The 4:2:2 stream is pixel-exact end to end
  on this fixture, so the 16-frame ladder is no longer a frontier. What
  remains for 4:2:2 is coverage, not correctness: the 4:2:2 header
  refusal is still unconditional, and every claim here rests on one
  256x144 fixture (`aomenc` 4:2:2, `--limit` cut) whose inter content is
  skip-heavy. A second 4:2:2 fixture with real residuals, compound and
  warped motion above the vertical midpoint is what would justify lifting
  the refusal — not more work on this stream.
