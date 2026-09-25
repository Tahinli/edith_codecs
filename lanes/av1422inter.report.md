# lane-av1-422inter — the frame-9 `read_coeffs_rect` scratch panic fixed at the
# source (the 4:2:2 intra-in-inter side-64 chroma is TWO square TX_32X32 units,
# not one 32x64 rect unit); the inter-frame divergence remeasured with the
# stage ladder and STOPPED at the sub-8x8 inter chroma class (`decode_inter_block8`)

## Outcome

1. REMEASURED first (pinned fixture `/tmp/t422.obu` sha256
   `d3fa4beeb1309033e4a03f5ae5c431c8fa8386ba1ada088684710423c7938129`,
   256x144 / chroma 128x144, 16 coded frames, local `EC_AV1_ALLOW_422_PROBE`
   bypass applied and reverted byte-identical after — `git diff` after the
   revert carries only the decode.rs fix): reproduced the frame-9 panic
   exactly as the r lane named it — `rect coeff scratch w=32 h=64 n=2448`
   at [`read_coeffs_rect`]'s guard (decode.rs:7195), backtrace
   `decode_inter_block` -> `read_rect_chroma_unit`; 9 frames decoded before
   the abort.
2. ROOT CAUSE, and why the scratch itself stays untouched: the panic site is
   `decode_inter_block`'s intra-in-inter 4:2:2 chroma arm (lane-av1-422k's
   `chroma_w != chroma_h` dispatch), whose `_` side-match handed the WHOLE
   uv plane block (32x64) to [`read_rect_chroma_unit`] as ONE rect unit. But
   `av1_get_max_uv_txsize(BLOCK_64X64)` at ss (1,0) is
   `av1_get_adjusted_tx_size(max_txsize_rect_lookup[BLOCK_32X64])` =
   `av1_get_adjusted_tx_size(TX_32X64)` = **TX_32X32** — libaom codes TWO
   stacked SQUARE 32x32 units per plane, plane-major (U r0, U r1, then V
   r0, V r1), never a TX_32X64 coefficient read (no such scan exists in
   this decoder and libaom writes none: a 64-axis codes its low-32 corner).
   Lifting the scratch bound to 36x68 (the r lane's tentative "heap
   fallback" sketch) would have read a full 32x64 scan and desynced on the
   very first such unit. The KEY-FRAME path has carried the correct model
   since lane-av1-422f (`decode_block`'s 4:2:2 chroma_tx table: "64 and
   128: TX_32X64 / TX_64X64 adjust down to TX_32X32, square units, a 1x2 /
   2x4 grid") — proven byte-exact on this fixture's frame 0.
3. FIX (the sibling 444 per-unit-walk pattern): the `side == 64` arm at
   `decode_inter_block`'s intra-in-inter chroma now walks the two stacked
   32x32 square units per plane through [`read_plane`] (`TxbSet::Chroma32`,
   `SCAN_32`, txb_skip ctx = above+left+3 — the offset-10 rows, the plane
   block being bigger than the unit), each with its own per-chroma-cell
   context read (`around_mi_422_chroma` at the unit's luma span), its own
   `tu_reach_rect`, immediate `record_mi_chroma`, assembled into the
   chroma_side-strided block grid, with `mu_chroma = true` so the tail's
   existing `mu_chroma_units` enumeration replays the per-unit entropy
   state after the whole-block record (the lane-dpm1 mechanism; its
   (rows, cols, ur, uc) walk already derives exactly these two units per
   plane at ss (1,0)). The 4/8/16/32 rect arms and the square
   `chroma_w == chroma_h` arm are untouched — 4:2:0 and 4:4:4 are
   byte-identical by construction (ss-keyed). `read_coeffs_rect` itself is
   untouched: with the adjusted-tx model no caller can deliver a unit
   bigger than 32x32's 36x36 levels frame, which is the invariant the
   scratch sizing always stated.
4. MEASURED after the fix (16 frames decode, no panic; the probe's
   `chroma422_square` counter rose 24 -> 87 across the stream, the touched
   class provably firing on inter frames): frame 0 is **byte-exact vs the
   instrumented aomdec at PREFILT, POSTDEBLOCK, POSTCDEF and FINAL on Y, U
   and V** (0 diffs everywhere — the parent lanes' frame-0 exactness
   survives). The inter frames stay divergent: per-frame stage-ladder
   counts (ours padded-cropped vs oracle, both vs the same instrumented
   `$HOME/.cache/aom-oracle/build` aomdec):
   - frame 1: PREFILT Y 30825 / U 12624 / V 12796, first Y (0,8) 76 vs 80,
     first U (0,0) 231 vs 90, first V (0,0) 164 vs 241;
   - frames 1..16 all diverge from (0,0) with ~28-35k Y / ~12-17k U /
     ~11-17k V per frame at PREFILT, and POSTDEBLOCK / POSTCDEF / FINAL
     track the same first samples 1:1 (the loop filters introduce no new
     first-diff site) — a pre-filter reconstruction class, filters
     exonerated.
5. ATTRIBUTION (2-frame cut, `EC_TRACE_MODE_STEP` + `EC_TRACE_COEFF`
   pairing against the oracle's own rungs): every commonly-traced mode /
   skip / angle / uv_mode read of frames 0+1 agrees rng-for-rng (1326
   paired lines, 0 mismatches), and the first 873 coefficient reads
   (frame 0 in full) agree — the entropy walk is in sync up to frame 1's
   first coded block. The FIRST divergence is that block's own tail: at
   mi (0,0) (8x8 inter leaf, NEARESTMV (0,0), coded), after the leaf's
   luma TX_8X8 txb_skip bit (agreed, rng 55168) the oracle reads the
   leaf's **U TX_4X8 rect chroma unit** (ctx 7 row, coded) then V — while
   OURS reads a TX_4X4-SQUARE-shaped unit (`TxbSet::Chroma4`,
   ctx = above+left, no offset-7 row). `decode_inter_block8`'s chroma half
   models the 4:2:2 leaf chroma plane block — `ss_size_lookup
   [BLOCK_8X8][1][0]` = BLOCK_4X8, one TX_4X8 rect unit per plane
   (`av1_get_max_uv_txsize`) — as a `chroma_side = SIDE >> ss_x` = 4
   square: wrong coefficient set/scan/ctx rows, and the same 4x4 square
   shape flows into the leaf's MC prediction, skip push and interintra
   blends. Pixel witness: frame-1 U (0,0) ours=231 ~= frame-0's 225 (the
   zero-mv prediction with only a square-read residue applied) vs the
   oracle's 90 (the coded TX_4X8 residual applied).
6. That defect class is not a one-site edit: the leaf's whole chroma half
   must move to (4,8) shapes — prediction build, skip/interintra/OBMC
   pushes, the rect coefficient read (`TxbSet::ChromaRect8x4` /
   `SCAN_4X8`, offset-7 ctx rows, inherited luma tx type via
   [`read_inter_plane_rect`]) — and the sub8 split-group's own chroma read
   (`decode_inter_sub8_split4`'s `TxbSet::Chroma4` group unit) repeats the
   same square model. Per charter step 3: STOPPED and named.

## Decision

- The header refusal STAYS unconditional in `stream.rs` — delete condition
  unmet (not pixel-exact from frame 1 on). The bypass was local-only, one
  `&& std::env::var(...).is_err()` clause, reverted byte-identical before
  the commit; the refusal gate re-verified firing (see Verification).
- `read_coeffs_rect` untouched (no scratch growth, no new panic path);
  4:2:0/4:4:4 arms of the touched dispatch untouched; `sub8_leaf_chroma422`
  (parent lane) untouched; the 4:2:0 group-tail SKIP arm untouched.
- Next stop named for the owning lane: **`decode_inter_block8`'s 4:2:2
  chroma half** (leaf chroma plane block (4,8): one rect TX_4X8 unit per
  plane through the inter rect reader, (4,8)-shaped MC prediction and skip
  pushes) plus `decode_inter_sub8_split4`'s group chroma read; after that
  round the stage ladder of Outcome 4 should simply be re-run — frame 0
  and the side-64 class are now proven clean, so the residual measurement
  is directly attributable.

## Verification

- `cargo check -p ec-av1` (target `~/.cache/cargo-target-av1422inter`):
  0 warnings, 0 errors on a fresh `Checking` of the edited tree.
- Pre-fix panic reproduced on the pinned fixture (backtrace named above);
  post-fix the same stream decodes 16/16 frames, frame 9 included.
- Frame-0 stage dumps vs the instrumented aomdec: PREFILT / POSTDEBLOCK /
  POSTCDEF / FINAL all Y=0 U=0 V=0 (comparator: ours padded 256x160/128x160
  u8 cropped, probe FINAL u16 planes vs oracle u8 — values equal).
- `stream::tests::a_non_420_subsampled_sequence_header_is_refused_by_name`:
  1 passed, 0 failed (refusal fires again after the bypass revert).
- `stream::tests::a_real_aomenc_stream_with_a_skipped_8x8_intra_leaf_
  whose_tx_split_decodes_pixel_exact` under `EC_AV1_REQUIRE_AOMENC=1`
  (with `EC_AV1_AOMENC` pointed at the oracle build): 1 passed, 0 failed —
  the 4:2:0 inter path is unchanged.
- `git status` clean except the decode.rs fix; the report is the only other
  file in the commit.

## State

- Commit on lane-av1-422inter (no push): the side-64 per-unit chroma fix in
  `decode_inter_block` + this report.
- The stage-ladder instruments live in `/tmp/i422/` (our/oracle per-stage
  dumps for all 17 oracle frames, comparator + EC_ISTEP pairing scripts) —
  scratch, not committed; the oracle build's rungs already carry everything
  needed to regenerate them.
