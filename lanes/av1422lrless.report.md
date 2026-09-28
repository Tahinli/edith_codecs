# lane-av1422lrless — the LR-OFF 4:2:2 coverage witness

Base `d3e9a616` (lane-av1422warp's head — the 4:2:2 frontier; this stacks
on it), worktree `~/.cache/wt/av1422lrless`, branch `lane-av1422lrless`, no
push. Target dir `$HOME/.cache/cargo-target-av1422lrless`.

## Verdict

**Exact, no new boundary exposed, and the stronger of the pair on the leg
that was weakest.** `d3e9a616` already decodes LR-off 4:2:2 correctly,
because the corners fix `af3285d5` shipped is upstream of the per-plane
`RESTORE_NONE` skip. The fixture is pinned, the gate is committed, and the
loop-restoration-off state is **proved by counters reading zero**, not
inferred from the encoder flag.

| | LR-on (`422_residual_compound_warp_16f.obu`) | LR-off (this lane) |
|---|---|---|
| frames pixel-exact vs `aomdec` | 16/16 | **16/16** |
| entropy pairing | 246735 / 246735 | **246480 / 246480** |
| `lr_wiener` | 22 | **0** |
| `lr_sgrproj` | 10 | **0** |
| `lr_stripe0` / `lr_last_stripe` | 26 / 28 | **0 / 0** |
| top-half compound | 3 | **14** |
| top-half warp | 3 | **10** |
| compound_warp (global) | 25 | 42 |
| compound_warp_8 (global) | 18 | 24 |
| rotzoom_gm_warp (global) | 84 | 76 |
| cdef_idx / part128_split | 53 / 95 | 39 / 95 |

## The controlled pair

The bug `af3285d5` fixed lives in `read_lr`'s
`av1_loop_restoration_corners_in_sb` port, and that corners code is the
**only** thing that differs between an LR-on and an LR-off 4:2:2 decode:
with LR off every plane's `frame_restoration_type` is `RESTORE_NONE`, so
`read_lr` skips each plane and never computes a range.

So this is the sibling's source content and encoder knobs with exactly one
flag changed, `--enable-restoration=0`. That makes the comparison controlled
rather than a second data point, and it is why the differing read counts
matter: **246480 vs 246735**, the per-plane skip showing up in the symbol
stream. The two streams really are on different paths, and both are exact.

```
ffmpeg -f lavfi -i "mandelbrot=size=256x288:rate=24:maxiter=220:start_scale=3:end_scale=0.35:end_pts=300,rotate=a=0.10*t:c=none:ow=256:oh=288" \
       -frames:v 24 -pix_fmt yuv422p -f yuv4mpegpipe src422.y4m
aomenc --codec=av1 --profile=2 --input-bit-depth=8 --limit=16 --width=256 --height=288 \
       --lag-in-frames=25 --auto-alt-ref=1 --enable-global-motion=1 --enable-restoration=0 \
       --cq-level=24 --cpu-used=0 --threads=4 --kf-min-dist=0 --kf-max-dist=999999 \
       src422.y4m -o a2.webm
ffmpeg -i a2.webm -c:v copy -f obu a2.obu
```

- source `src422.y4m` sha256 `4d35eaf65d1a5541b3177e1183644c163b3868d8f141bed0ce9fdf833280ba9f`
  (the same file the LR-on sibling used)
- pinned `422_residual_compound_warp_nolr_16f.obu` 38701 B, sha256
  `205146b85d93a6bf586f7d3cf8778fe1a62887a2e2b265068c563ce9462e0479`, fnv1a64
  `0xf331fb2ed6b79efa`
- decoded raw sha256 `03ee237c5c5824c8917fca77e8464c29ab14dd96cbbfd02f91f57d99842c8f6e`,
  2359296 bytes, identical on ours and `aomdec --rawvideo`

## What this adds to the lift argument, and what it does not

It **discharges the third ground** lane-av1422warp listed for keeping the
refusal: "LR-off 4:2:2 is untested, and it is a different range
computation again". It is now tested, and it is exact.

It also **replaces the weakest leg rather than adding to it.** That leg was
engagement: the LR-on stream carried only 3 top-half compound and 3
top-half warp blocks once the census was corrected to libaom's real
`is_global_mv_block` predicate. This stream carries **14 and 10** — and it
does so on the arm where `read_lr` does nothing, so the engagement is not
an artefact of the code under test. Two independent recipes at cq-level 24
also now stand behind the coverage instead of one.

**The refusal still stays**, on the two grounds this lane does not touch:

1. **One encoder family.** Both streams are `aomenc` at cq-level 24 on
   mandelbrot, differing by one flag. That is a controlled pair, which is
   exactly what it was built to be, and it is not the same as two
   independent recipes.
2. **The refusal has never been the problem; coverage breadth is.** A lift
   decision wants a chroma-format sweep (4:2:0 odd dimensions, 4:4:4
   high-bit-depth, tile columns) before 4:2:2 joins them, and that is a
   bigger question than this lane's charter.

**Recommendation: keep the refusal.** The lift argument now has two exact
4:2:2 streams and a real engagement figure; it is short of breadth, not of
exactness.

## Evidence

| check | result |
|---|---|
| 16 frames vs `aomdec --rawvideo` | pixel-exact, 2359296 B, sha256 `03ee237c…c8f6e` both sides |
| entropy pairing vs instrumented oracle | **246480 / 246480, no divergence** |
| LR-off proof | all five LR counters 0 (sibling: 22 / 10 / 0 / 26 / 28) |
| 4:2:0 control | byte-exact |
| 4:4:4 LR witness | byte-identical to its pre-lane decode |
| all three previously pinned 4:2:2 witnesses | pixel-exact |
| 4:2:2 gate family | 5 passed, 0 failed |
| 422 + obmc + LR families | 25 passed, 0 failed, 1 ignored |
| wide battery 420/444/inter/superres/cdef | 118 passed, 0 failed, 7 ignored |

Gate: `the_pinned_422_lr_off_witness_is_present_and_refuses_by_name` — pin
plus refuse-by-name, the established 4:2:2 pattern, since with the header
refusal standing no committed test *can* decode a 4:2:2 stream. Mutation-
proven both ways: a flipped fixture byte panics `bytes drifted`; a wrong
refusal string panics `must refuse by name`.

## State

Two commits on `lane-av1422lrless`, no push. Worktree clean; the
`EC_AV1_ALLOW_422_PROBE` bypass is reverted and in no commit. The
measurement shim used to read the `pub(crate)` counters from the probe
example was removed before the commit. The 4:2:2 sequence-header refusal is
**unchanged and unconditional**.
