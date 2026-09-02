# lane-oddh r1 — frames whose height/width is 8 mod 16

Branch `lane-oddh` off main `b1d8457`. Target: the user's Hunger Games film shape
(3840x1608, `1608 % 16 == 8`), where every inter attempt derailed.

## What the round established (measured, not reasoned)

1. **The charter's premise is stale in its blame, right in its shape.** The refusal
   `"a non-DC chroma mode on an 8x8 inter-frame leaf"` is NOT what odd-height frames
   stop at once lane-golomb's edge-partition fix is present. Cherry-picking
   `eacd7fd` (clean, no conflicts) moves 192x136 to the same refusal its
   *even-height control* 192x128 hits, i.e. that one is width/content-related, not
   odd-height (see 4).

2. **Minimal repro is 64x72, not 192x136.** With the standard inter-gate recipe
   (`--enable-rect-partitions=0 --min/max-partition-size=8/16`, mandelbrot, cq 40):
   - `64x64` decodes 4/4 frames pixel-exact (control),
   - `64x72` (bottom band 8 px) stopped at the key-frame 16-level edge refusal,
   - `72x64` (right band 8 px) stops in the inter tile path.

3. **The real blocker underneath is a first-symbol desync of the KEY tile at
   height 72.** Not an edge-band defect at all: the *first* partition symbol of the
   frame diverges.
   `EVIDENCE: /tmp/.../scratchpad/k-64x72.obu (667 B, key frame only, aomenc cq40) | EC_AV1_TRACE=1 decode_probe vs EC_TRACE=1 aomdec | first element mi=(0,0) bsize=BLOCK_64X64 ctx=0: aomdec value=3 (PARTITION_SPLIT, tell=1 rng=32768), ours value=9`
   `EVIDENCE: /tmp/.../scratchpad/k-64x72.{ours,aom,ref}.y | aomdec --rawvideo + ffmpeg rawvideo vs our Y dump | aomdec == ffmpeg byte-for-byte; ours differs in 4583 of 4608 Y samples (whole frame), while the 64x64 control differs in 0`
   Our tile payload starts at file offset 20 in both the 64x64 and the 64x72 stream
   (`key_tile_bytes len=543`/`647`, file sizes 563/667), and `base_q_idx=160`,
   `mi_cols=16`, `mi_rows=18` all parse correctly, so the header field values are
   right; the divergence is in the very first symbol read off those bytes.
   **Not diagnosed further — this is the handoff item.** Ruled out by reading the
   source: `sb_rows`/`sb_cols` (`sequence.rs:409-422`, 18 -> 2 correct) and the
   uniform-spacing tile-info loops (`frame.rs:1296-1334`, both increment loops match
   spec 5.9.15, so the extra `increment_tile_rows_log2` bit that height 72 introduces
   IS consumed).

4. Pre-existing, unrelated to odd height: `192x128` and `192x136` both stop at
   `"a 1:4 partition below 16x16"` with `--enable-1to4-partitions=0` on the command
   line — a same-shaped refusal-from-own-desync at width 192, present on the control.

## Code changed (all in this worktree, unproven end to end)

- cherry-pick `eacd7fd` (lane-golomb) — the frame-edge partition bit at 6 sites in
  both tile paths, plus its film key-frame fixture/gate. Clean merge.
- `crates/ec-av1/src/decode.rs:11371` — the key-frame 16-level frame-edge
  `PARTITION_HORZ`/`VERT` now decodes its single visible 16x8 / 8x16 strip through
  `decode_leaf_rect` instead of refusing (removes that refusal string; entry dropped
  from `refusal_inventory.rs`).
- `crates/ec-av1/src/decode.rs` (inter tile path, superblock level and 16-level) —
  the same edge half-strip as one 64x32 / 32x64 resp. 16x8 / 8x16 inter block
  (`write_w`/`write_h`, square CDF/size-class corner-cut, exactly the accepted
  corner-cut of the 32-level rect arm at `decode.rs:18448`). Interior rect
  partitions still refuse by name.
- `decode.rs` — new counter `edge_rect_strip_hits()` (blocks decoded inside the 8px
  edge band), for the gate a successor must write.

`corner-cut:` the two inter edge arms read square CDFs / size classes for a
rectangular block (ceiling: coefficient and mode syntax for TX_64X32/TX_16X8 inter is
not modelled; upgrade path = the same treatment the 32-level rect arm eventually gets).

## Verification status — honest

- NO gate was added and NO refusal claim is proven: every stream that reaches the new
  arms is desynced before them (finding 3), so a pixel compare would measure the
  desync, not the arm. Writing
  `a_real_aomenc_inter_sequence_with_an_8px_tall_edge_row_decodes_pixel_exact` on top
  of a first-symbol desync would have produced a vacuous or a permanently-red gate.
- Suite: see `$HOME/.cache/oddh-suite.log` (run as a systemd unit, result quoted in
  the handoff message).

## Disposition

- fix-now (next round, r2): the 64x72 key-tile first-symbol desync. It is the root
  blocker; the edge arms above are dead code until it is fixed.
- deferred(the desync above): the 8px-edge-band gate + the refusal lift claim.
- deferred(width-192 defect, separate lane): the `"1:4 partition below 16x16"` stop
  that both 192x128 and 192x136 share.
