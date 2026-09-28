# lane-av1-rect128recon — the 444_sb128rect witness f2 is byte-exact vs aomdec AND ffmpeg; root cause: the inter-frame intra tail's chroma walk coded ONE whole-plane unit where 4:4:4 codes four TX_32X32 units; gate extended take(2)→take(3) (r1)

## Outcome

1. REPRODUCED at HEAD 99232190 (fresh target dir `cargo-target-av1rect128recon`,
   fixture sha256 `27825e14633a4b23e37d48a2acdc7f5d814ca4fadd0b8797d72a1ffb34061e80`
   re-verified, oracle aomdec `~/.cache/aom-oracle/build` regenerated this
   session): frame 2 diverges at PREFILT Y 6952 / U 9568 / V 13496, first diffs
   Y (col 128, row 98), U (col 128, row 64), V (col 128, row 0); f0/f1 exact.
   Numbers match av1444sbf2 r1 / av1lrflush r3 sample-for-sample.
2. BLOCK-SHAPE CORRECTION (ladder evidence, not inference): the oracle's f2
   top-right superblock root is a gathered-bit SPLIT, and its right column is
   INTRA blocks — 64x64 at (128,0), 64x32 at (128,64), 64x32 at (128,96),
   64x64 at (128,128) — not "the inter 64x128 at (128,0)" the earlier lane
   reports name. Evidence: oracle `EC_PART`/`EC_PART_VAL` (ctx 14, values
   NONE/HORZ at mi (0,32)/(16,32)), `EC_PREDOUT8` per-unit sums, and
   `EC_DQCOEFF`. The earlier reports' pixel numbers are all reproduced here;
   only the block-shape naming was wrong.
3. FIRST DIVERGENT SYMBOL, pinned by consecutive-TU rng alignment on both
   ladders (`EC_COEFF_STEP`, TU-boundary values 60168/55581/51541 matching
   line-for-line): the 64x64 intra block's FIRST chroma TX_32X32 unit at
   mi (0,32). Ours decoded `txb_skip` off the offset-7 context row (printed
   ctx 0); aom reads the offset-10 row (true ctx 10 — the uv plane block
   64x64 is larger than the TX_32X32 unit). Same decoded value (skip), but
   the CDF rows differ, so the arithmetic coders consumed different bits and
   the rest of frame 2 decodes off a drifting state — bounded, mostly-skip
   garbage that still corrupted the block's own V plane (uniform −1: the
   missing V residual) and the bottom superblock row.
4. ROOT CAUSE (one site): `decode_inter_block`'s intra tail read the block's
   chroma as ONE whole-plane unit (`read_plane` with `chroma_side`/`chroma_tx`,
   block-level `around`, no +10 offset) — a 4:2:0-era remnant. At 4:4:4 a
   64x64 plane block is FOUR plane-major TX_32X32 units, each with its own
   `around_mi` context and the offset-10 rows. The keyframe path
   (`decode_block`'s `cn > 1` loop, lane-av1-444) and the inter tail's own
   ss-0/0 arm already did this correctly; the intra-in-inter tail was the one
   remaining copy of the old shape. The misnaming in (2) hid this: the
   "inter 64x128" framing pointed at Levent4's strip/inter territory, while
   the real site is the intra tail.
5. FIX (decode.rs, intra tail chroma chain): new `ss_x==0 && ss_y==0 &&
   chroma_side > chroma_tx` arm — four plane-major TX_32X32 units, per-unit
   `around_mi(cu_mi, 32)`, `Some(3)` skip-ctx offset (+10 convention on our
   chroma-32 table), immediate per-unit `record_mi_chroma`, per-unit
   `tu_reach`, assembled 64x64 plane grids, `mu_chroma = true` so the
   block-tail unit replay re-stamps after the whole-block record (class
   `override-slot-on-one-arm`). 4:2:0 is identical by construction: the arm
   is ss-gated and `chroma_side == chroma_tx` there still takes the old
   single-read path. Class sweep: the only other intra-in-inter chroma arms
   are the side>64 mu-chunk walk (already per-unit, correct) and the
   rect-strip readers (separate paths, sibling-owned, untouched).
6. RESULT: f2 PREFILT byte-exact; FINAL (full deblock/CDEF/LR pipeline)
   byte-exact on ALL THREE frames against BOTH oracles — ours ==
   `ffmpeg -f rawvideo -pix_fmt yuv444p` byte-for-byte (276480 bytes), and
   frame-equal to aomdec's Y4M output (its 12 interleaved `FRAME` markers
   stripped).

## Verification

- `a_444_sb128_root_rect_stream_with_restoration_decodes_pixel_exact`
  (extended to `.take(3)` in both oracle arms + rewritten scoping comment,
  same commit): PASS.
- `cargo check -p ec-av1`: 0 warnings (target `$HOME/.cache/cargo-target-av1rect128recon`).
- sb128 family (`cargo test -p ec-av1 --lib sb128`): 10 passed, 1 ignored.
- 444 family (`--lib 444`): 4 passed (includes the lossless 444
  min-partition-64 inter stream).
- Restoration battery (`EC_AV1_REQUIRE_AOMENC=1 --lib restoration`):
  10 passed, 0 failed.
- chroma family (`--lib chroma`): 28 passed, 0 failed.
- Stage evidence from env-gated rungs on both decoders (EC_PART/EC_PART_VAL,
  EC_COEFF_STEP, EC_ECDUMP_IN, EC_PREDOUT8, EC_IMODE, AOMMB, EC_DQCOEFF);
  throwaway /tmp scratch only, nothing left in the tree.

## Decision

- Gate extended and the stale "LR never restores RU row 0" scoping comment
  replaced (that attribution was refuted in lane-av1lrflush r3 as a
  stage-ladder mixup) — the deferred same-commit pairing is now discharged.
- Sibling ownership respected: `decode_rect4_16_strip`'s 16x4/strip-tail
  territory and the sub-8 leaf paths untouched; the fix lands in
  `decode_inter_block`'s intra tail only.
- Named for the record: any future block-shape work must sweep BOTH copies
  of the chroma unit walk — `decode_block` (keyframe) and
  `decode_inter_block`'s intra tail — plus the inter tail; this defect was
  the third copy drifting from the first two.

## State

- Commit on lane-av1-rect128recon (no push): decode.rs fix + stream.rs gate
  extension/comment + this report.
