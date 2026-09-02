# lane-intra14 r3 -- merge sqdrift, the r2 blocker dissolves, the real blocker is the recipe

Branch `lane-intra14`, base main `48b35ab` + `lane-sqdrift 7d498c0`. **RED on the
gate, but the r2 blocker is closed**: the "pre-existing inter-frame pixel defect"
r2 was blocked on was lane-sqdrift's CFL-alphabet defect, and with it merged the
gate source decodes **pixel-exact on all 40 attempts**. What is left is a RECIPE
gap, not a decoder gap: no aomenc source found this round codes an INTRA block on
a 1:4 partition inside an INTER frame. Both gate arms stay `#[ignore]`d, on the
new MEASURED reason.

## 1. Merges (f4b5419, 67a9bb3)

* `git merge --no-commit main` (48b35ab) -- clean auto-merge, no conflict.
* `git merge --no-commit lane-sqdrift` (7d498c0, carries lane-sbrect10 9f1f108)
  -- one conflict, `read_intra_mode_rect` (decode.rs:5558). Resolved keeping
  **both**: sbrect10's `is_cfl_allowed` funnel first (`cfl && bw.max(bh) > 32`
  -> no-CFL alphabet, bumps `NOCFL_UV_MODE_HITS`), then this lane's
  intra-in-inter `skip` passthrough. This lane's own 1:4-in-inter reader
  (`decode_intra_rect_in_inter`, decode.rs:6234) reaches chroma through the same
  rule at decode.rs:6373 (`let cfl_allowed = bw.max(bh) <= 32`), never a private
  CFL read -- checked by grepping every `cdfs.uv_mode_cfl` site in decode.rs
  (4 sites: 5618 rect funnel, 6373 this lane, 8707 square funnel, 10572 sub8
  where every block is <= 8 so CFL is always allowed).

md5 of files this lane does not own, merged vs lane-sqdrift: cdf.rs `d3de223b`,
cdf_state.rs `d0644312`, encode.rs `6c9f9f66`, mvstack.rs `9e6e3f59`,
motion_field.rs `fc33f264` -- all byte-identical.

## 2. The r2 blocker was our own desync (c49c93a)

Gate un-ignored and run on the merged tree, 8-bit, unchanged r2 recipe:

| | r2 | r3 |
|---|---|---|
| attempts decoding whole | 3 / 40 | **40 / 40** |
| pixel-compared attempts mismatching | 3 / 3 | **0 / 40** |
| differing samples (40 streams x 8 frames x 3 planes) | ~4k then ~24k drift | **0** |
| `intra_rect4_strip_in_inter_hits` seed 52 | 64x16=4 16x64=3 | 0 0 0 0 |

So r2's hit counter was produced by our own **desynced** decode of a mismatching
stream (class counter-from-refused-stream): decoded correctly, that same stream
contains no intra 1:4 strip at all. The r2 report's "pre-existing inter-frame
defect owned by no lane" is closed -- it was lane-sqdrift's 64x64 intra block in
an inter frame reading `uv_mode` off the CFL alphabet.

## 3. Recipe hunt (this is the remaining blocker)

A per-attempt counter probe was added to the gate (`intra14 probe` line: raw
`sb_rect4_*`, `rect4_32_*`, `intra_rect_strip_in_inter_hits`), so "no hit" can be
told apart from "no partition". Five 40-attempt sweeps, both counters read:

| recipe | decodes exact | 1:4 partitions of ANY kind | intra 1:4 in inter |
|---|---|---|---|
| mandelbrot zoom, min-part 32, cpu 1..4 (r2 recipe) | 40/40 | `sb_rect4 0/0`, `rect4_32 0/0` | 0 |
| + every intra tool on (smooth/paeth/directional/angle-delta/**cfl-intra=1**) | 40/40 | 0 | 0 |
| + `--min-partition-size=16` | 40/40 | 0 | 0 |
| + `--sb-size=64 --cpu-used=0/1` | 40/40 | 0 | 0 |
| `gradients` (source of the repo's proven 1:4 gates), transposed on odd attempts | 21/40 (19 named refusals) | **`sb_rect4 h=4 v=16`** | 0 (all coded inter) |
| mandelbrot x gradients `blend=all_mode=average` | 30/40 | 0 | 0 |

The mandelbrot source is excellent at intra-in-inter (589 2:1 intra strips over
40 streams, class `intra_rect_strip_in_inter_hits[1]`) and produces **zero** 1:4
partitions -- independently confirming sqdrift r1's aomdec partition histogram on
the same source. `gradients` is the reverse. The gate recipe is left on
`gradients` (the only arm reaching a 1:4 partition at all) with the full measured
table in the source above `intra_rect4_in_inter_gate`.

Two lookups made this round: lavfi `gradients` `speed` is capped to `[0, 1]`
(ffmpeg rejected 12), and its animation steps per FRAME not per second -- a
rate=2 render is byte-identical to rate=25, so the sequence cannot be made
jumpier that way.

The 19 gradients-recipe refusals are other lanes' surfaces: split intra strip
with a 64x32 TU (11), with a 32x64 TU (5), inter SB-level AB (2), HORZ/VERT intra
strip in a screen-content frame (1).

EVIDENCE: $HOME/.cache/intra14-r3-8bit.log, -wide.log, -min16.log, -cpu0.log, -grad.log, -blend.log | gate un-ignored, 6 x 40-attempt aomenc sweeps under systemd-run scopes with a per-attempt counter probe | 40/40 attempts pixel-exact (0 differing samples) vs r2's 3/40 all-mismatching; 1:4 partitions 0 on every mandelbrot arm, sb_rect4 h=4 v=16 on gradients, intra-1:4-in-inter 0 everywhere

## 4. `--enable-cfl-intra=1` arm (charter step 3)

Ran, in the "every intra tool on" sweep above: 40/40 attempts decode pixel-exact
with `--enable-cfl-intra=1` (plus smooth/paeth/directional/angle-delta all 1).
That flag is now in the gate's recipe permanently. It fires no counter this lane
owns, so it is reported as a measurement, not as coverage.

## 5. Film check

`decode_probe` on a fresh 0.4 s extract at 4500 s of the 3840x1608 yuv420p10le
film (156 frame headers; r2's extract was reaped from /tmp, class
oracle-in-reaped-dir, so the two are not the same bytes):

* stop string UNCHANGED: `a split intra strip whose transform unit is 64x32
  (no luma coefficient tables for that shape here)`
* `intra_rect4_in_inter: 64x16=0 16x64=4 32x8=0 8x32=0` (4 intra 16x64 strips in
  inter frames decoded before the stop), `rect4_32: horz=40 vert=36 coded=43`
* `EC_AV1_FINAL_DUMP` file count = **1** (r2 measured 0 on its extract)

EVIDENCE: $HOME/.cache/intra14-r3-film.log | ffmpeg -ss 4500 -t 0.4 -c:v copy -f obu, then decode_probe under a 6G scope with EC_AV1_FINAL_DUMP | stop string "transform unit is 64x32", 4 intra 16x64 strips in inter frames, 1 dumped frame

## 6. Suite

`cargo test -p ec-av1 --lib` as a user systemd unit (MemoryMax=10G):
**382 passed; 0 failed; 35 ignored**; 1017.06s (r2: 370/0/34 -- the merges add
12 gates, the 35th ignored is unchanged: this lane's two arms plus the pre-existing
ignored set).

EVIDENCE: $HOME/.cache/intra14-suite-r3.log | systemd-run --user --unit=intra14-suite-1788330813 -p MemoryMax=10G, EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1, nice -n 10 -j3 | test result: ok. 382 passed; 0 failed; 35 ignored; 1017.06s

## Residue

* fix-now (successor, this lane): find a source that codes an INTRA block on a
  1:4 partition inside an INTER frame. Everything else is in place -- the reader,
  the counters and the pixel compare are all proven on the surrounding shapes.
  Two untried levers: (a) `--enable-1to4-partitions=1` with `--sb-size=64` on a
  gradients sequence containing hard CUTS (concat of per-frame gradients: the
  lavfi `concat` graph tried this round returns only 3 of 8 frames -- build the
  y4m by concatenating 8 single-frame renders in Rust instead); (b) drive the
  encoder with `--enable-global-motion=0 --lag-in-frames=0` at cq 25 on the
  gradients source so RD prices intra lower.
* accepted: the gate stays `#[ignore]`d. A refusal is lifted only with a gate
  that fires its counter; this one does not fire.
* accepted: the mandelbrot arm's 40/40 pixel-exact result is not turned into a
  new pin here -- the sibling 2:1-strip gate already covers that source.
