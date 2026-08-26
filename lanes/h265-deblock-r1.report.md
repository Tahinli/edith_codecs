# lane-h265-deblock r1 — HEVC deblocking filter, on by default

## What landed

`crates/ec-h265/src/deblock.rs`: the spec 8.7.2 in-loop deblocking filter,
written from the text, and the encoder wiring for it.

An intra-only stream makes the filter unusually simple:

* Intra prediction reads *unfiltered* reconstruction (8.4.4.2.1), so for a
  stream with no inter pictures the filter is a post-pass over the finished
  picture. Reconstruction stays bit-identical to a decoder's output, which is
  what the picture-hash SEI promises.
* Every edge of an intra picture has `bS = 2`, so the whole boundary-strength
  derivation collapses to one question: where are the transform-block
  boundaries?

`TuMap` answers that with one byte per 4x4 unit — `log2` of the transform block
covering it. Transform blocks of side S are aligned to multiples of S, so an
edge at `x` is a boundary exactly when `x % S == 0`. No edge list is stored.
Each wavefront worker publishes its band's map; the encoder stitches the bands
and runs every vertical edge in the picture, then every horizontal one over the
vertically-filtered samples.

Luma is the per-4-line-segment decision (`d < beta`), strong or weak filter,
with `filter_p1`/`filter_q1` gated on `dp`/`dq`. Chroma filters p0/q0
unconditionally with `delta = Clip3(-tC, tC, ((((q0-p0)<<2) + p1 - q1 + 4) >> 3))`
on the 8-chroma-sample edge grid. `beta` is indexed by `Clip3(0,51,qp)`, `tC`
by `Clip3(0,53,qp+2)`; chroma `tC` uses `chroma_qp(qp,0)+2`.

## Conformance

`ffmpeg_decodes_bit_exactly_at_every_shape` passes with the filter on: six
picture shapes, both CTB sizes, zero samples differ against ffmpeg's decode.
Bit-exactness against a reference decoder is the whole gate here — the filter
is normative, so "close" is a bug.

## Bug this uncovered — class `trial-map-not-restored`

First run: `shape-64x64-ctbdefault: 28 samples differ -- Y 0, Cb 16, Cr 12`.
Luma exact, chroma not, which pointed at a real edge disagreement rather than
at the filter arithmetic: chroma filters unconditionally, so a phantom edge
shows there while luma's `d < beta` test swallows it.

Root cause was in the encoder, not the filter. The RQT split trial marks four
child transform blocks in the TU map; when the *nosplit* trial won, nothing
rewrote those cells, so the map reported a 16x16 boundary inside a 32x32
transform. Reconstruction and coefficients were snapshot/restored between
trials — the side map was not.

Fixed by marking the accepted transform in the nosplit branch, and by adding
`tu_log2` to the third byte of `save_meta`/`load_meta` so the CU-depth trials
restore it too.

Class swept across `ctu.rs`: every `mark_*` write inside a trial body was
checked against the winner path. `mark_coded` is rewritten by the parent after
the decision; `modes`/`depths`/`tu_log2` all ride the meta snapshot. No second
instance.

## Measurement — his files

BD-PSNR against x265 at matched features, **x265 deblocking in every row**, so
the two rows differ only in whether our filter runs. 24 pictures after 1500,
ladders auto-aligned, 0% extrapolation both sides.

| clip | ours off | ours on | delta luma | delta YUV |
|---|---|---|---|---|
| synthetic 352x288 | +8.781 | +8.796 | +0.015 | +0.039 |
| film 3840x1608 | -0.197 | **+0.063** | **+0.260** | +0.244 |
| screen capture 2560x1440 | +0.606 | +0.693 | +0.087 | +0.104 |
| web clip 640x1138 | +0.567 | +0.686 | +0.119 | +0.140 |
| phone clip 1920x1080 | +0.113 | +0.162 | +0.049 | +0.074 |

Every clip improves, luma and YUV, so the default is **on** (`EncoderConfig::
deblock: true`, `deblocking_filter_disabled = !deblock` in the PPS). The film
crosses from behind x265 to ahead of it.

The absolute numbers in this table are not comparable to the ones in the gate's
doc comment: those were measured against an x265 with `no-deblock`, which is a
different anchor. `EC_H265_X265_DEBLOCK=1` selects the anchor used here.

## Knobs

* `EC_H265_DEBLOCK=0` — our filter off.
* `EC_H265_X265_DEBLOCK=1` — x265 deblocks (drops `no-deblock`), for the
  matched-feature anchor above.

## Next

SAO is the remaining in-loop tool (`sao_enabled: false`). Screen capture is
still the largest content gap.
