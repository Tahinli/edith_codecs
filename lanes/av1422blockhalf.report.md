# lane-av1-422blockhalf — `decode_inter_block`'s 4:2:2 chroma half ported to the
# per-axis plane-block model (per-axis pred buffers + real-subsampling masked
# compound blend), the `EC_RECTCHROMA_GAP` 16x8 / 8x32 chroma shapes coded as
# the (4, 8) unit tiling the Release oracle's OOB `max_txsize_rect_lookup[
# BLOCK_INVALID]` byte produces, and `decode_inter_sub8_rect2`'s has_chroma
# gating moved from `i == 1` to the per-piece `is_chroma_reference` rule;
# frame 0 stays byte-exact at PREFILT/POSTDEB/POSTCDEF, all 16 frames now
# decode (the frame-11 `(4, 32)` refusal is gone), and the inter-frame
# divergence is REMEASURED and REATTRIBUTED to a partition-dispatch defect
# upstream of the chroma half (ours dispatches SPLIT4 where the oracle codes
# HORZ at mi(8,10)) — frame exactness NOT claimed for frames 1+

## Outcome

1. `decode_inter_block`'s chroma PREDICTION buffers (compound and
   single-ref arms, OBMC plan, interintra stride, skip pushes, su/sv Pred
   strides, the `read_inter_rect_chroma`/`read_inter_chroma_lossless` pred
   strides) are keyed to the PLANE BLOCK's own per-axis shape at ss (1, 0):
   `(chroma_stride, chroma_buf_h) = (write_chroma_w, write_chroma_h)`
   instead of the enclosing square `chroma_side`. The two masked-compound
   blend sites now pass the plane's REAL subsampling shifts — the
   stop-note's pre-port direct luma-resolution mask read is gone
   (aom_dsp/blend-a64-mask.c's `(1, 0)` branch at 4:2:2). Every replaced
   expression reduces to the parent values at 4:2:0/4:4:4
   (`chroma_stride == chroma_buf_h == chroma_side` there), byte-identical
   by construction.
2. `read_inter_rect_chroma` now takes the per-axis stride correctly: its
   assembled block grid is `stride x plane-height` (`assembled_h`), the
   single-unit embed included — the old square `chroma_side * chroma_side`
   allocation panicked (`range end index 20 out of range for slice of
   length 16`) the moment the per-axis stride reached it.
3. `decode_rect_split`'s 4:2:2 BLOCK_INVALID chroma planes: the tall/wide
   1:2 and 1:4 strips (`8x16, 16x32, 32x64, 8x32, 16x64`, + `64x128` for
   128-SB streams) have NO chroma plane block
   (`av1_ss_size_lookup[...][1][0] == BLOCK_INVALID`); the plane is still
   coded, tiled into (4, 8) rect units — the Release oracle's
   out-of-bounds `max_txsize_rect_lookup[BLOCK_INVALID]` byte is TX_4X8
   (lane-av1-422m's measurement), and `decode_token_recon_block` steps the
   plane in that unit shape. Units read `TxbSet::ChromaRect8x4`/`SCAN_4X8`
   with `around_mi_422_chroma` ctx, mode-indexed intra tx type (the
   `< 32` square-up rule, pre-existing rect units unchanged at DctDct),
   per-unit stamps + units replay. SKIP strips predict per (4, 8) unit as
   well (`decode_token_recon_block`'s per-TU plane walk runs before the
   skip guard) — the old single whole-plane zero push asked the intra
   predictor for a 1:8 rect (`dc_rect_multiplier` panic, bw=8 bh=64).
   `EC_RECTCHROMA_GAP` now prints only truly-unhandled shapes (it used to
   misfire on every tiled square — the parent lane's `chroma=8x8` rows);
   it prints ZERO times on the fixture now. `chroma422_chunk` = 344 on the
   full fixture (new units provably fire).
4. `decode_inter_sub8_rect2`'s has_chroma gating: per-piece
   `is_chroma_reference` — 4:4:4 every piece, 4:2:2 both horz pieces
   (`!(bw & 1)`) / the odd-column vert piece (`mi_col & 1`), 4:2:0 keeps
   `i == 1` verbatim. The inter pieces take a per-piece inline chroma arm
   at 4:2:2 (U then V TX_4X4 at the floored cell, `around_mi_422_chroma`
   ctx, inherited piece tx type; horz predicts whole-from-own mv, vert
   predicts halves `build_inter_predictors_sub8x8`-style, left half from
   the in-group left piece or the decoded grid neighbour when the group
   starts on an odd column — filters unavailable on the grid, so an
   outside neighbour borrows this piece's filters). The 4:2:0 group tail
   (and the group DC stamp) are gated off at 4:2:2.
5. MEASURED (17-frame probe, per-OBU cut harness `/tmp/i422/cut_frames.py`,
   comparator ours-padded-u8-cropped vs instrumented
   `$HOME/.cache/aom-oracle/build` aomdec-cropped-u8; fixture `/tmp/t422.obu`
   sha256 `d3fa4beeb1309033e4a03f5ae5c431c8fa8386ba1ada088684710423c7938129`):
   - frame 0: byte-exact at PREFILT, POSTDEB and POSTCDEF on Y, U and V —
     0 diffs everywhere (FINAL not re-run this round; frame 0's final state
     is bracketed by POSTCDEF on a key frame).
   - all 16 frames decode, exit 0 — the parent lane's frame-11 refusal
     (`a coded HORZ/VERT strip whose chroma transform has no rect
     coefficient tables here`, the `luma=8x32 tx=8x8 chroma=4x32` GAP) is
     gone.
   - frames 1..15 still diverge (~24-36k Y, ~13-18k U/V per stage; ladder
     in `/tmp/i422/ladder/`). **Frame exactness is NOT claimed.**
6. REATTRIBUTION (the round's real finding): the frames-1+ divergence is
   NOT the chroma half. Strict stream-order EC_ISTEP pairing on the
   frames-0-1 cut puts the first entropy divergence at index 167 — the
   8x8 group at mi(8,10): the ORACLE codes PARTITION_HORZ (pieces (8,10)
   and (9,10) only, both intra with uv; oracle partition trace
   `EC_PART mi_row=8 mi_col=10 bsize=3 ctx=0 value=1`) while OURS
   dispatches decode_inter_sub8_split4 (EC_SUB8INTRA: `shape=4x4 sub=0
   has_chroma=false mi=8,10` ... four pieces, one extra uv_mode
   consumption on the (9,11) odd piece). Same bits, different partition
   outcome at the 8x8-leaf partition read (`partition_w8` ctx/value —
   ours `ctx=2` at the (8,8) sibling). Every downstream diff in the
   ladder is walk-shift noise from that point. This is a pre-existing
   defect upstream of the named chroma surfaces, newly visible now that
   the chroma shapes no longer refuse.

## Decision

- The header refusal STAYS unconditional in `stream.rs`; the bypass was
  local-only, applied and reverted byte-identical (0 occurrences left;
  `stream::tests::a_non_420_subsampled_sequence_header_is_refused_by_name`
  green after the revert).
- 4:2:0/4:4:4 untouched by construction (ss-keyed arms; the 4:2:0 sub8
  group tail and `i == 1` rule verbatim).
- `read_coeffs_rect` untouched; the 4:2:0 group-tail SKIP arm untouched.

## Deferred

(a) The (8,10)-class partition-dispatch defect (ours SPLIT4 vs oracle
    HORZ) — the frames-1+ exactness blocker. Unblock: capture the 8x8
    leaf partition context both sides (ours `EC_TRACE_PART`, oracle's
    shipped partition probe) and compare `partition_ctx_mi` vs libaom's
    `partition_context` gather at that leaf; the chroma half itself is
    now shape-honest and rides on the fix.
(b) 4-wide strips (4x16, 4x8 luma) at 4:2:2: `decode_rect_split` still
    refuses `(2, 16)` chroma by name (no pair-anchor machinery there; the
    4:2:0 `(4, 16)` single-unit arm would be wrong at ss (1, 0)).
    Unblock: the split4-style floored-pair anchor ported to the 1-mi-wide
    strip callers, then the OOB (4, 8) tiling.
(c) 128-root 4:2:2 chroma chunking (`decode_inter_block`'s side > 64
    mu-chunk chroma) still passes the square `chroma_side` geometry —
    unreachable on <=64-superblock streams (this fixture is 64-SB);
    flagged for whoever lands 4:2:2 128-SB support.

## Verification

- `cargo check -p ec-av1` (target `~/.cache/cargo-target-av1422blockhalf`):
  0 warnings, 0 errors.
- Refusal gate green post-revert (1 passed).
- Probe: `OK: 16 frames decoded`; 0 `EC_RECTCHROMA_GAP` prints;
  `chroma422_chunk=344`, `chroma422_square=197` on the full fixture.
- Ladder: frame 0 = 0 diffs at PREFILT/POSTDEB/POSTCDEF (Y=U=V); frames
  1..15 divergent (see 5/6 — attributed, not claimed).

## State

- Commit on lane-av1-422blockhalf (no push): the per-axis decode_inter_block
  chroma half, the read_inter_rect_chroma stride fix, the decode_rect_split
  (4, 8) chroma tiling incl. per-unit skip prediction, the rect2 per-piece
  has_chroma gating, and this report.
- Ladder dumps under `/tmp/i422/ladder/` (scratch, not committed).
