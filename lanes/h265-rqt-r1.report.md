# lane-h265-rqt r1 — residual quadtree, one split level

## What landed

`max_transform_hierarchy_depth_intra = 1`: a 2N×2N intra coding unit may split its
luma transform once into four half-size children, chosen by `J = SSD + lambda * bits`
against the unsplit block. Chroma splits with the tree while its own blocks stay 4×4
or larger (log2 > 3); at an 8×8 luma tree chroma stays at the parent size and is
coded with the last child, per 7.3.8.8. The parent luma cbf is the OR of the
children's. Default: on.

## Gates

| gate | result |
| --- | --- |
| `cargo test --release -p ec-h265` | 14/14 conformance, whole suite green |
| `rqt_decodes_bit_exactly` | ffmpeg reconstruction sample-exact |
| `transform_skip_decodes_bit_exactly` | sample-exact with rqt on (see defect below) |
| clippy `--all-targets` | 0 warnings |

## Rate-distortion, four-point ladder against x265

BD-PSNR of ours against x265 (higher = closer to x265; both arms same ladder):

| clip class | rqt off | rqt on | delta |
| --- | --- | --- | --- |
| film | +0.036 | +0.046 | **+0.010** |
| screen capture | −0.043 | +0.037 | **+0.080** |

Ladder QP shift: −1 on film, 0 on screen. RQT wins on both content classes, so it
is on by default — the ffmpeg/x265 default for intra depth is likewise ≥1.

## Defect found and fixed in-lane

Flipping the default to on broke `transform_skip_decodes_bit_exactly`:
`2955 samples differ -- Y 1938, Cb 510, Cr 507, first at (8, 32)`, everything from
luma row 32 down diverging — a CABAC desync, not a reconstruction difference.

Cause: `transform_skip_flag` is read by the decoder for **every** 4×4 residual block
once the picture parameter set enables skip, and the encoder decided it from the
coding unit's size (`transform_skip && n == 8`, i.e. "chroma is 4×4 only under an
8×8 unit"). With a transform tree, a 16×16 unit splits chroma to 4×4 as well; those
blocks were coded with a real transform and no flag written, while the decoder read
one. The decision now travels by the block's own size — `code_chroma` and
`encode_residual` each already apply it only at 4×4.

Class: **decision carried at the wrong granularity** — a syntax element that belongs
to the transform block was decided from the coding unit. Swept: the only other
size-conditioned decisions (`ctu.rs:1268` skip, `intra.rs:122/146` filtering and
strong smoothing) all take the transform block's own size. Clean.

## Not in this lane

- Depth > 1 (`max_transform_hierarchy_depth_intra = 2`), and the inter tree.
- NxN partitions above 8×8 (still 2Nx2N only outside the minimum unit).
- 64×64 coding units — the remaining lever for the screen-capture gap.
