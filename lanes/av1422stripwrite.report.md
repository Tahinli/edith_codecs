# lane-av1-422stripwrite — the 4:2:2 residual was NOT the rect-strip
# chroma write/anchor: a reference picture's chroma plane claimed
# `h / 2` rows, so every chroma MC sourced at or past the frame's
# midpoint was clamped; the pinned 16-frame t422 stream now decodes
# byte-exact (first cut FAILed on review for a floor half-width where
# the producer ceils — corrected, ladder re-run, still 48/48 + 16/16)

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
`round_ss(w, 1) * h`, which matches neither, so it fell into the 4:2:0
branch and claimed `h / 2` rows. The stored data was never wrong (the frame
is stored through `round_ss(fw, ss_x) x round_ss(fh, ss_y)`,
`decode.rs:25806`, which is 4:2:2-correct) — only the *declared* shape was,
and `whole_plane` copies it into `width`/`height`/`true_*`, which is what
every chroma MC and every edge clamp reads.

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
    let half_w = round_ss(width, 1);
    if len == width * height { (width, height) }        // 4:4:4
    else if len == half_w * height { (half_w, height) } // 4:2:2
    else { (width / 2, height / 2) }                    // 4:2:0
}
```

The half width comes from `round_ss`, **not** a bare `width / 2`, because
that is what the producer used: a frame is stored through
`round_ss(fw, ss_x) x round_ss(fh, ss_y)` (`decode.rs:25806`) and
`round_ss(dim, 1) == (dim + 1) >> 1` is a **ceil**. A floor test here
(this lane's first cut, corrected on review) silently misses every
odd-width 4:2:2 reference and drops it into the 4:2:0 arm — the same
defect one dimension over. The 4:2:0 arm is left at the old
`(width / 2, height / 2)` on purpose, so 4:2:0 keeps the exact shape it
always had.

`Picture`'s `u`/`v` doc comment (`encode.rs`) was corrected to state the
real contract (half width, the frame's own chroma height) — the inference
above depends on it.

**Untouched:** the current frame's chroma planes were already
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
- **4:2:0/4:4:4 identity, measured three ways.**
  *By construction:* the inference's 4:4:4 arm is the old first test
  unchanged, and the 4:2:0 arm is the old fallback `(w / 2, h / 2)`
  verbatim — the only thing between them is the inserted 4:2:2 test, which
  can only fire on a count a 4:2:0 or 4:4:4 plane does not have.
  *Brute-forced:* against the producer's own `round_ss` crops
  (`decode.rs:25806`) over every `w, h` in `1..=128`, the 4:4:4 and 4:2:2
  counts are distinct from each other and from the 4:2:0 count and both
  route correctly for **every `w >= 2 && h >= 2`**; nothing a decodable
  frame produces falls outside that (the pinned fixture's chroma is
  128x144).
  *4:2:0 at odd dimensions — an open, pre-existing gap, not a clean bill.*
  The floor arm is what the parent produced, but it is the wrong shape for
  an odd-dimension 4:2:0 reference: the producer stored
  `round_ss(w, 1) x round_ss(h, 1)`, so the floor under-declares. Running
  every 4:2:0 count over the `1..=128` square through the shipping helper:
  **12160 of the 16384 pairs** come back smaller than the plane holds —
  **12033** of them with `w >= 2` and **127** in the `w == 1` column (at
  `1x1` the 4:4:4 arm catches the count and `(1, 1)` is exact).
  Under-declaration at `h == 1` is **zero**: there the 4:2:0 count equals
  the 4:2:2 count, routes into the 4:2:2 arm and lands on the correct
  `(round_ss(w, 1), 1)`, which is also the one place the new arm departs
  from the parent's floor — for the 127 pairs with `w >= 2`, and there
  only by *correcting* the parent, which claimed zero rows. The `w == 1,
  h >= 2` residue is a shape error, not a misroute: the floor fallback
  simply cannot express `(1, round_ss(h, 1))`. A ceil fallback would close
  the whole gap, at the cost of departing from the parent on every
  odd-dimension 4:2:0 shape with no in-tree coverage to justify it.
  Recorded, not taken.
  *Gated:* `cargo test -p ec-av1 --lib -- lossless decodes_sample_exact
  444 reference_chroma` in its own target dir
  `~/.cache/cargo-target-av1422stripwrite-gate` — see below for what that
  run is and is not evidence of.
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
- **A gate run that was thrown away, and why.** An earlier identity-gate
  run on this branch was launched against the ceil form and then
  **invalidated by my own concurrent edits**: while it was in flight I added
  the two routing tests below and deliberately mutated `half_w` back to the
  floor form to prove the test non-vacuous. Its result was never delivered,
  and whatever it compiled was not the shipping source (`decode.rs`
  md5 `999348d786ca0a8e930010ee46184d87` is the tree the accepted run
  covers). It is recorded here as **no evidence** rather than quietly
  dropped, and the accepted run is a clean re-run in a dedicated target
  dir with nothing else of mine touching cargo. Not a target-dir lock: the
  other suites on the host each use their own
  `$HOME/.cache/av1-local-suite/target-*` dir.
- `cargo check -p ec-av1 --all-targets` (target dir
  `~/.cache/cargo-target-av1422stripwrite`): **0 warnings, 0 errors**.
- **Regression tests, and proof they are not vacuous.** Two unit tests in
  `decode.rs`'s test module, the only 4:2:2-reachable gate while the header
  refusal stands (no stream can reach the code path otherwise):
  `a_reference_chroma_sample_count_routes_back_to_its_own_format` drives
  all three formats off the producer's own `round_ss` crops over odd AND
  even widths (2,3,4,5,8,9,16,17,64,65,128,129) and heights
  (2,3,4,5,8,9,64,65,144,145), and
  `a_422_reference_claims_the_full_chroma_height` pins the t422 shape
  (256x144 -> chroma 128x144, explicitly not the 72 rows the bug claimed).
  **Mutation proof:** with `half_w` reverted to `width / 2` — the exact
  floor form this lane's first cut shipped and the review caught — the first
  test FAILS with `4:2:2 reference of 3x2 (chroma 4 samples) must route
  back to its own shape`, and passes again on the restored ceil form. So
  the test pins the defect, not the implementation.
- `git status` names `decode.rs`, `encode.rs` (doc comment) and this
  report. Every temporary rung (`EC_SW422` in the 4:2:2 per-piece arm and
  in `build_c`; `SUB48`/`SUB48D`/`SUB48P` in the oracle's
  `reconinter_template.inc`) was removed before the commit.

## State

- Commit on `lane-av1-422stripwrite` (no push): the `ref_chroma_shape`
  helper + its two call sites in `crates/ec-av1/src/decode.rs`, its two
  routing regression tests, the `Picture` chroma-shape doc correction in
  `crates/ec-av1/src/encode.rs`, and this report. The first cut was FAILed
  on review for using a floor half-width where the producer ceils; the
  corrected helper, the ladder re-run (48/48 + 16/16 again) and the
  identity re-proof are all in this commit.
- **Merge note.** The sibling lane's `RefPix` three-way inference
  (`39f45c31`, lane-av1-422bigblock) edits the same two call sites. Both
  now agree on the arithmetic — the half width is `round_ss(w, 1)`, i.e.
  ceil, because that is what `decode.rs:25806` stores — so the union is a
  union, not a floor-vs-ceil fight. If that lane threads an explicit
  subsampling instead of inferring one from the length, prefer it and drop
  this helper; the 48/48 result is a property of the *shape* being right,
  not of how it is derived, so it survives either way. Re-run the 16-frame
  ladder after resolving.
- **No residual to hand on.** The 4:2:2 stream is pixel-exact end to end
  on this fixture, so the 16-frame ladder is no longer a frontier. What
  remains for 4:2:2 is coverage, not correctness: the 4:2:2 header
  refusal is still unconditional, and every claim here rests on one
  256x144 fixture (`aomenc` 4:2:2, `--limit` cut) whose inter content is
  skip-heavy. A second 4:2:2 fixture with real residuals, compound and
  warped motion above the vertical midpoint is what would justify lifting
  the refusal — not more work on this stream.
