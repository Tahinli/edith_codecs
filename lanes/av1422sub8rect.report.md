# lane-av1-422sub8rect — the sub-8x8 inter 4:2:2 chroma ported to its real
# shapes (`decode_inter_block8`'s whole chroma half to the BLOCK_4X8 rect;
# `decode_inter_sub8_split4`'s group unit replaced by the per-piece
# odd-column chroma-reference model the oracle traces actually code); frame
# 0 stays byte-exact at all four stages, the 4:2:0/4:4:4 identity holds by
# ss-keying, and the inter-frame divergence is REMEASURED (reduced ~15% on
# frames 1..10, exactness NOT claimed -- other 4:2:2 classes remain)

## Outcome

1. ROOT-CAUSE CONFIRMATION FIRST (pinned fixture `/tmp/t422.obu` sha256
   `d3fa4beeb1309033e4a03f5ae5c431c8fa8386ba1ada088684710423c7938129`,
   parent-lane instruments in `/tmp/i422/`, local `EC_AV1_ALLOW_422_PROBE`
   bypass applied and reverted byte-identical after — `git diff` carries
   only decode.rs/mc.rs/encode.rs changes): the oracle's frame-1 first
   block (`AOMMB mi=(0,0) ... skip=0`) reads luma TX_8X8 then
   `plane=1 tx_size=5 mi_row=0 mi_col=0 rng=55168` — tx_size 5 IS TX_4X8,
   confirming the charter: the 8x8 inter leaf's chroma plane block is
   BLOCK_4X8, one TX_4X8 rect unit per plane (`ss_size_lookup[BLOCK_8X8][1][0]`),
   while ours read a TX_4X4 square.
2. SPLIT4 MODEL CORRECTED AGAINST THE ORACLE, NOT THE CHARTER'S GUESS: the
   parent report assumed the split4 group codes ONE rect group unit; the
   traces prove otherwise. At ss (1,0) `is_chroma_reference` for a BLOCK_4X4
   piece is `(mi_col & 0x01)` — ODD columns only (av1_common_int.h:1459) —
   and every non-skipped odd-column piece codes its OWN U then V TX_4X4
   square unit right after its luma, anchored at the FLOORED chroma cell
   (`(mi_col & !1) * MI >> ss_x`, the lane-av1-422kf2 rule) with
   `around_mi_422_chroma`-style per-chroma-cell ctx (measured ctx 8 =
   above+left+7 at frame 1's mi(12,3)):
   - frame 0 key-frame groups (18,54) and (30,42): U+V read on BOTH
     odd-column leaves ((18,55)+(19,55), (30,43)+(31,43));
   - frame 1 inter group at mi(12,2): U+V on leaf (12,3) ALONE — its
     bottom-right twin was skipped, so it codes nothing.
   The 4:2:0 shared group unit simply never exists at 4:2:2.
3. PORT (all keyed on ss, 4:2:0/4:4:4 byte-identical by construction):
   - `decode_inter_block8` chroma half to per-axis shapes: `chroma_w/chroma_h`,
     `(TxbSet::ChromaRect8x4, SCAN_4X8)` at 4:2:2; inter coefficient reads
     through `read_inter_plane_rect` (offset-7 ctx — the unit IS the plane
     block — inherited luma tx type) with `around_mi_422_chroma`; MC
     prediction (single-ref, scaled, warp, compound-intermediate), the
     wedge-masked blend, interintra blend, OBMC pushes and skip pushes all
     (4, 8)-shaped; the intra-in-inter arm reads the key-frame arm's own
     rect reader (`read_rect_chroma_unit`, ctx offset None, CfL carried);
     palette UV map dims per axis.
   - `decode_inter_sub8_split4`: per-piece arm added after each inter
     piece's luma (odd column, ss 1,0): prediction as
     `build_inter_predictors_sub8x8` — left 2x4 half from the LEFT piece's
     own ref/mv/filters, right 2x4 half from this piece's; a mixed group
     (left piece intra/off-frame, `is_sub8x8_inter` false) predicts the
     whole 4x4 from this piece; skip pieces still predict but record zeros.
     Coefficients via square `TxbSet::Chroma4` reads at the floored anchor,
     per-unit `record_mi_chroma` (8x4 luma span). Intra pieces gate
     `has_chroma` on the piece's own column parity and route through
     [`sub8_leaf_chroma422`]; even-column intra pieces publish DC to the
     uv-mode map. The 4:2:0 group tail (build, read, band stamps, DC map
     stamp) is gated OFF at 4:2:2.
   - Shared helpers made ss-exact (4:2:0/4:4:4 reduce to the old behaviour):
     `blend_masked_compound`'s `subsampled: bool` → per-axis `(subw, subh)`
     mirroring aom_dsp/blend-a64-mask.c's four branches; `interintra_blend`'s
     wedge mask read likewise; `obmc_plan`/`obmc_run` take the plane ss —
     chroma extents are the luma rules `>> ss` (`av1_skip_u4x4_pred_in_obmc`
     keyed on the plane block, above-pass skip and left-pass blend extents),
     `TxParams::strided` sized `stride * h` (identical when h == stride, as
     every pre-existing caller has). The encoder's `obmc_plan` call passes
     the fctx ss.
   - STOP-NOTE (owning parent-lane surface, named not fixed):
     `decode_inter_block`'s 4:2:2 chroma is still the square-cut model; its
     chroma masked-blend keeps the pre-port direct mask read there (guarded
     by `ss_x == ss_y`), because its `chroma_side`-square buffers cannot be
     indexed by the subsampled-axis walks. Known-divergent, unchanged
     behaviour, named for the owning lane.
4. MEASURED after the port (17-frame probe run under the reverted bypass;
   comparator: ours padded 256x160/128x160 u8 cropped vs the instrumented
   `$HOME/.cache/aom-oracle/build` aomdec):
   - frame 0: **byte-exact at PREFILT, POSTCDEF (and FINAL by the u16
     comparator) on Y, U and V — 0 diffs everywhere**, so the rect read,
     the split4 per-piece model and every shared-helper change are exact on
     everything frame 0 exercises (including the split groups (18,54),
     (30,42) that motivated the per-piece model).
   - inter frames 1..10 decode and were measured: PREFILT diffs dropped
     ~15% vs the parent lane's ladder (e.g. frame 1: 30825/12624/12796 →
     22248/11733/11974; frame 8: 35014/11020/12043 → 32855/11020/12043;
     per-frame values in `/tmp/i422/sub8rect/`). **Frame exactness is NOT
     claimed** — frames 1..10 remain divergent: the named sub-8 sites are
     fixed, but the stream carries further 4:2:2 classes the decoder still
     square-cuts or gaps (probe trace: `EC_RECTCHROMA_GAP luma=16x8
     tx=16x8 chroma=8x8`, `luma=8x32 ... chroma=4x32`, i.e. `decode_inter_block`'s
     own 4:2:2 chroma half), so the entropy walk desyncs again after the
     first uncoded-for block.
   - frames 11..16 were not reached by the probe's frame accounting this
     round (11 dumped frames, decode exits 0 — the dump-index vs
     show-existing accounting needs the parent-lane harness's
     1-frame-per-OBU walk); the ladder for 1..16 is the owning lane's
     re-measurement once the next class lands.
5. Deferred items: (a) frames 11..16 ladder entries — unblock: rerun the
   parent-lane per-OBU cut harness (`/tmp/i422/cut_frames.py`) after the
   next class; (b) `decode_inter_sub8_rect2`'s 4:2:2 has_chroma gating (its
   inter strips pass `i == 1` where BLOCK_4X8 needs odd-column parity) —
   not one of this charter's named sites, unmeasured on the fixture so far.

## Decision

- The header refusal STAYS unconditional in `stream.rs`; the bypass was
  local-only, applied and reverted byte-identical (0 occurrences left;
  `stream::tests` green after the revert).
- 4:2:0/4:4:4 untouched by construction (ss-keyed arms); the 4:2:0
  group-tail SKIP arm untouched; `read_coeffs_rect` untouched.

## Verification

- `cargo check -p ec-av1` (target `~/.cache/cargo-target-av1422sub8rect`):
  0 warnings, 0 errors.
- `stream::tests::a_non_420_subsampled_sequence_header_is_refused_by_name`:
  1 passed — the refusal fires after the bypass revert.
- `stream::tests::a_real_aomenc_stream_with_a_skipped_8x8_intra_leaf_
  whose_tx_split_decodes_pixel_exact` under `EC_AV1_REQUIRE_AOMENC=1`
  (oracle aomenc): 1 passed — the 4:2:0 inter path, including the
  shared-helper ss-ifications, stays pixel-exact.
- Frame-0 stage dumps vs the instrumented aomdec: PREFILT/POSTCDEF
  Y=U=V=0 (POSTDEB comparator run this round used the wrong dump-env name;
  the post-deblock state is bracketed by the two exact stages).
- Hit counter added: `CHROMA422_INTER_SUB8_HITS` (per-piece inter 4:2:2
  chroma units) — reads non-zero on the fixture's frames 1+, so the new arm
  provably fires.

## State

- Commit on lane-av1-422sub8rect (no push): the sub-8 rect port +
  `TxParams::strided` sizing fix + shared ss-exact helpers + this report.
- Probe instruments regenerated under `/tmp/i422/sub8rect/` (scratch, not
  committed); the oracle build's rungs already cover everything needed to
  re-measure after the next class.
