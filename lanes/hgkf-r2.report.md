# lane-hgkf r2 — a skipped intra block predicts per TRANSFORM UNIT

Branch `lane-hgkf`, on top of r1 (`4939695`). Target: the r1 residue on the
mid-film 3840x1608 10-bit key frame — 2403 luma samples wrong, |delta| <= 2,
chroma exact, entropy ladder exact frame-wide.

## Stage split (the charter's step 1)

Both decoders' 8-bit stage rungs truncate 10-bit samples, so a |delta| <= 2
defect is invisible in them. Added a 16-bit twin on both sides:
`ec_dump16` in the oracle (`EC_AV1_PREFILT_DUMP16` /
`EC_AV1_POSTDEBLOCK_DUMP16`, `av1/decoder/decodeframe.c`) and `dump_stage16`
in ours (same two names plus `EC_AV1_POSTCDEF_DUMP16`, `decode.rs`).

EVIDENCE: /home/tahinli/.cache/hgkf-work/{o,a}_pre.f0, {o,a}_pdb.f0, o_fin.f0.yuv vs ref_kf_4500.yuv | ours vs instrumented aomdec, 16-bit cropped planes at PREFILT / POSTDEBLOCK / FINAL | PREFILT Y 2372 wrong (maxabs 3, first row 416 col 3170, ours 210 vs 209), POSTDEBLOCK identical set, FINAL 2403 -> **the defect is already in reconstruction**; deblock/CDEF/LR only smear it

## Root cause (class `decision-at-wrong-granularity`)

An intra block's `tx_depth` is read whether or not the block is skipped (only
an INTER skip forces the largest transform), and spec 7.11.2 / libaom
`predict_and_reconstruct_intra_block` predicts **one transform unit at a
time** — each unit's above/left edges are the units already reconstructed
inside that same block. Our skip branches predicted the whole block in one
call:

* `crates/ec-av1/src/decode.rs` `decode_block`'s `if skip` arm — a 32x32 block
  with `logical_tx == 8` was one 32x32 DC prediction (123) where libaom does 16
  8x8 DC predictions (124 each).
* `crates/ec-av1/src/decode.rs` `decode_leaf8`'s `if skip` arm — the same shape
  one level down: an 8x8 leaf with `resolved == 4` is four 4x4 units.

Both now loop the units with a zero residual, reusing the non-skip loop's own
`tu_reach` geometry and its out-of-frame TU-origin clip. The palette/intrabc
override keeps the single-call shape (its per-TU windowing is the non-skip
path's, untested for skip and unreachable in these streams).

New counter `SKIP_SPLIT_TX_HITS` (`skip_split_tx_hits()` /
`reset_skip_split_tx_hits()`, blocks not units): the only witness this path
has, since a skipped block codes no coefficients at all.

## Evidence

EVIDENCE: /home/tahinli/.cache/hgkf-work/aom_pred.txt, our_pred.txt | new oracle rung `EC_PREDND` (non-directional high-bitdepth prediction; the pre-existing `EC_PRED` rung sat AFTER the `is_hbd` early return and printed nothing on a 10-bit stream) vs our new `OUR_PRED` rung | at px(1088,1216) libaom prints 16 8x8 DC predictions of 124, ours one 32x32 of 123 — sum 7936 x16 vs 125952
EVIDENCE: /home/tahinli/.cache/hgkf-work/fx_kf_4500.f0.yuv vs ref_kf_4500.yuv | `dump_yuv kf_4500.obu` vs `ffmpeg -pix_fmt yuv420p10le`, 16-bit compare | 2403 wrong -> **0, all three planes bit-exact**
EVIDENCE: /home/tahinli/.cache/hgkf-work/fx_kf_0.f0.yuv vs ref_kf_0.yuv | same command pair | **0, still bit-exact** (r1's frame did not regress)

Scratch pin (not a repo fixture, scratch-only as the charter requires):
`.../scratchpad/census2/kf_4500.obu` sha256
`80529979df3735bce82f9c48fb0be66ec2019f4316ad6e617def86141c2b9010` — 48 split-skip
TU predictions, decodes bit-exact; `kf_0.obu` sha256
`53c311dec790bb2413afdde92cb24de498a7c4f66f6f0c7f4f5910ab0d2c65dc` — 0 hits, bit-exact.

## Gate: NOT LANDED — no encoder recipe reached the path

`deferred: an in-repo gate for split-tx skipped intra prediction — no
reproducible encoder recipe found in this round's budget — unblocked by either
an aomenc flag combination that keeps a split `tx_depth` on a block it then
skips, or by lifting the 1:4-partition refusal so an SVT-AV1 stream can be
decoded whole.`

Swept, all 10-bit, all with the decode's success checked (a refused decode's
counter is a `counter-from-refused-stream` artefact, so every arm below reports
both):

* aomenc `--enable-tx-size-search=1`, mandelbrot/testsrc2/low-amplitude noise,
  256x192..1280x720, cq 12..55, cpu-used 0..8, aq-mode 0/1, 8- and 10-bit:
  **0 hits on every arm that decoded**. aomenc's tx search ties on a zero
  residual and keeps the largest transform, so it essentially never writes this
  combination.
* ffmpeg `libsvtav1` (v3.1.2) preset 4, 1280x720 10-bit: **reaches the path**
  (4..24 units at crf 25..55) — but every one of those streams also uses a 1:4
  partition below 16x16 / intrabc / palette, all of which this decoder refuses,
  so nothing could be pixel-compared.
* `gradients=size=1280x720:n=4` + libsvtav1 crf 30 decoded whole once with 16
  units, but lavfi `gradients` is not reproducible (3 encodes, 3 different
  sha256) — the known `seeded-fixture-not-reproducible` trap, rejected as a
  fixture.

## Same-shape sweep

Every intra skip-prediction site in `decode.rs`: `decode_block` (fixed),
`decode_leaf8` (fixed), the 4x8/8x4 leaf at `decode.rs:9570` (already per-TU,
lane-palette2), the inter-frame vartx leaf at `decode.rs:16450` (already
per-TU), the 4x4 leaf (no split exists). Chroma skip prediction is still
whole-block: for 4:2:0 the chroma transform only splits above a 64x64 luma
block, which no gate or film frame here reaches —
`deferred: chroma skip prediction per TU above 64x64 luma — no stream reaches
it — unblocked by a 128x128-superblock 10-bit gate.`

Suite: `cargo test -p ec-av1 --lib` (user unit, log `$HOME/.cache/hgkf-suite-r2.log`) -- **356 passed / 0 failed / 29 ignored**, 416s.
