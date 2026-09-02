# lane-sqdrift r2 -- the square-only silent drift is the no-CFL `uv_mode` alphabet

## Verdict: GREEN. Root cause confirmed by ablation, gated with a new real-aomenc gate.

Tip `2b750e6` on `lane-sqdrift` (base main `85887c7`).

## 1. What the divergence was

r1 had localised it to one 64x64 `PARTITION_NONE` INTRA block inside an INTER frame
(frame 4, mi(16,32) = the last superblock), entered bit-exactly in sync (`pre_rng=33730`
== aomdec's `rng=33730`), with TX_64X64 dequant+inverse already exonerated against real
libaom. The open question was "which symbol inside that block".

Answer: `uv_mode`. libaom's `is_cfl_allowed` (`blockd.h`: `block_size_wide[bsize] <= 32 &&
block_size_high[bsize] <= 32`, spec 5.11.5) excludes a 64-axis block from CFL, so its
`uv_mode` comes off the **13-symbol `uv_mode_no_cfl`** alphabet. We read the 14-symbol
`uv_mode_cfl` one: the same `DC_PRED` VALUE, a different interval narrowing, and the tile
desynced from that block on (class `wrong-alphabet-same-value`).

The fix was found independently by lane-sbrect10 (`9f1f108`) and is merged here, not
rewritten: commit `cbcbf13` merges `lane-sbrect10` into this branch. `decode.rs` gets the
`cfl_allowed = write_w.max(write_h) <= 32` test at the intra-in-inter square arm plus the
same `is_cfl_allowed` bound enforced at the two funnels `read_intra_mode` /
`read_intra_mode_rect`, and the `nocfl_uv_mode_hits()` witness counter. No decoder line in
this round is mine; my round's contributions are the confirmation, the ablation, and the
square-only gate.

## 2. The decisive before/after, same stream, same tree

Pinned stream `/home/tahinli/.cache/sqdrift/keep-s.obu`, sha256
`c6af4fb4ebdc3d74dcfa0c945c0ef2d5e1e3a0902891d9e0a97a5608776b5d55` (r1's `gen.sh`,
hashed twice from two independent encodes), compared PRE-LOOP-FILTER, per decode-order
frame, byte for byte against instrumented aomdec's own `EC_AV1_PREFILT_DUMP`:

| | f0 | f1 | f2 | f3 | f4 | f5 | f6 | f7 | outcome |
|---|---|---|---|---|---|---|---|---|---|
| before (`cfl_allowed = true` ablation on THIS tree) | 0 | 0 | 0 | 0 | **3540** | - | - | - | REFUSED after f4: "an intra-coded 1:4 (or other non-2:1) rect strip on the inter block path" |
| after (merged fix) | 0 | 0 | 0 | 0 | **0** | 0 | 0 | 0 | 8 frames decoded, all byte-exact |

The "before" row is not r1's memory: it is the one-line ablation
`decode.rs:18548 let cfl_allowed = true;` rebuilt and re-run in this round, then reverted
(`git diff --stat` after revert shows stream.rs only). It also re-confirms r1's finding
that the refusal main stops at on this stream is a PHANTOM of the desync -- aomdec's
partition histogram over the stream contains no rect partition at all (class
`refusal-from-own-desync`).

## 3. The gate (new)

`crates/ec-av1/src/stream.rs:6900`
`a_real_aomenc_square_only_inter_sequence_with_a_64x64_intra_superblock_decodes_pixel_exact`

The SQUARE-ONLY twin of lane-sbrect10's gate, pinning r1's recipe: real aomenc,
192x128 mandelbrot (`start_scale` 4.76/3.20), cq 55/45, `--cpu-used=1 --sb-size=64
--enable-rect-partitions=0 --enable-ab-partitions=0 --enable-1to4-partitions=0
--min-partition-size=16 --max-partition-size=64 --enable-tx-size-search=0`, 8 decode-order
frames, **both bit depths**. Rect OFF is what makes the 64x64 `PARTITION_NONE` a WHOLE
superblock (`bsize == sb_size`), a shape the sibling gate's rect-on recipe does not pin;
8 frames of drift means a desync that survives one frame still fails here.

Asserts: every decode-order frame's Y/U/V compared against ffmpeg (a mismatch of ANY shape
is a failure, never a SKIP -- so out-of-scope mismatches are structurally 0); a decode
error must be a named `unsupported` refusal; and `nocfl_uv_mode_hits` delta per attempt
must be > 0 on at least one pixel-exact attempt per depth.

Result (`--exact --nocapture --test-threads=1`), 10.4 s:

* 8-bit: 0 named refusals, **4/4 pixel-exact attempts**, 6 no-CFL `uv_mode` reads, 0 carried none
* 10-bit: 0 named refusals, **4/4 pixel-exact attempts**, 6 no-CFL `uv_mode` reads, 0 carried none

No refusal string is removed in this round: the refusal this stream used to hit was never
about this shape.

## 4. Suite

`cargo test -p ec-av1 --lib` under `systemd-run --user --unit=sqdrift-suite-r2-...`,
log `$HOME/.cache/sqdrift-suite-r2.log`: **379 passed, 0 failed, 33 ignored** in 958.68 s (r1 baseline on the unfixed tree never completed).

## EVIDENCE

EVIDENCE: /home/tahinli/.cache/sqdrift/np.f0..f7 vs aom.f0..f7 | decode_probe with EC_AV1_PREFILT_DUMP on the pinned stream (sha256 c6af4fb4...) after merging lane-sbrect10, cmp -l per decode-order frame Y+U+V | 0 differing bytes on all 8 frames (r1: f4 3540 wrong luma samples, then refusal)
EVIDENCE: /home/tahinli/.cache/sqdrift/ab.f0..f4 | same binary with decode.rs:18548 ablated to `cfl_allowed = true`, rebuilt, re-run, then reverted | f0-f3 0, f4 3540 differing bytes, decode then REFUSES "an intra-coded 1:4 ... rect strip on the inter block path" -- the phantom refusal is produced by this desync alone
EVIDENCE: cargo test -p ec-av1 --lib -- --exact --nocapture a_real_aomenc_square_only_inter_sequence_with_a_64x64_intra_superblock_decodes_pixel_exact | 4 attempts x 2 bit depths of real aomenc, rect partitions off, every decode-order frame vs ffmpeg | ok in 10.38s; 8-bit 4 exact/6 hits, 10-bit 4 exact/6 hits, 0 refusals, 0 attempts carried none

## Residue

* deferred(lane-intra14 re-runs its own gate): `lanes/intra14-r2.report.md` records its
  blocker as "decode-order frame 3/4 luma ~3.7-4.5k samples at max |d| 6..17, first diff
  around x=128..141, y=58..59, then frames 5-7 drift to ~24k at max |d| ~220" on the same
  192x128 mandelbrot source -- the identical signature to this lane's, at the identical
  coordinates. Very likely the same defect, so its two `#[ignore]`d 1:4-strip arms should
  be re-measured on a tree carrying this fix. Not measured here: its recipe (1:4 on) is
  another lane's surface.
* accepted: r1's `EC_AV1_TRACE pre_rng` and `EC_IIS` rungs stay as debug instrumentation.
