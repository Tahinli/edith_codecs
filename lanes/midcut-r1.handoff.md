# lane-midcut r1 handoff (turn cap)

## Verdict: (C) real reconstruction desync. NOT (A) alignment, NOT (B) film grain.

Reproducer, hashed twice, 325 KB: `~/.cache/midcut-tmp/h2400.t2.obu`
= `ffmpeg -ss 2400 -t 2 -c:v copy -f obu` on the 10-bit 3840x1608 stream
(sha256 2f62dd1f74bffe10973494404ee4d1a695d2d327b88fb1891c63452b5d47c895 for the
full 2 s cut) truncated to its first 2 frame OBUs (census7 `trunc.py`).
It holds the whole defect: key frame + first hidden alt-ref.

### Measurements that separated A/B/C
1. (A) ruled out. ffmpeg decodes 261 frames from the 2 s cut; our OUT16 frames
   are in display order and monotone in `order_hint` (HDR dump: 114 key, then
   115,116(show_existing 7),117,118(se 6),119,120,121...). An 8x8 cross-compare
   of our 8 shown frames vs ffmpeg's 8 (grain off both sides) has NO near-zero
   off-diagonal: every ours[i] is 13-16 MB from every ffmpeg[j]. Frames are
   aligned; the census tool was measuring the truth.
2. (B) ruled out. `EC_AV1_NO_GRAIN=1` (added this round, `stream.rs`) vs
   `ffmpeg -c:v libdav1d -filmgrain 0`: first_diff_frame=1, 13450726 bytes --
   the same mismatch as grain-on/grain-on (13458423). Grain is not it.
3. (C) located. Decode-order compare of `EC_AV1_PREFILT_DUMP16` ours vs the
   instrumented aomdec: f0 (key) EXACT, f1 (hidden ARF) 4386319 luma px wrong,
   confined to superblocks (0,17) and (0,58) at first, then spreading.
   Pre-deblock and post-deblock diffs are identical -> reconstruction, not filters.

### Root cause found and FIXED (commit 43e9909)
First bad pixel (17,1088) sat in the block aomdec reports as
`EC_MODE_VAL mi_row=4 mi_col=272 mode=13 ref0=1 ref1=0` -- ref1=0 is INTRA_FRAME,
i.e. an INTERINTRA block, and its footprint is 32x16.
`decode.rs interintra_blend` took ONE `side` and used it for the edge fetch, the
intra predictor's (bw,bh), `ii_size_scales` and the blend bounds. On a rect block
`side` is the LONGER dimension, so a 32x16 block got a 32x32 SMOOTH/DC predictor
built off 32 rows of left edge (16 of them not yet decoded). The error signature
matches exactly: rank-1 (sv 90 vs 18), exactly 0 on the block's first row, growing
downward -- the `sm_weights_h` divergence between height-32 and height-16 SMOOTH_PRED.
Fix: `interintra_blend(plane, x, y, bw, bh, stride, ..)` -- `edges_rect`/`predict`
at the true footprint, mask scale `128 / max(bw,bh)`, loop `bh x bw`, `pred`
indexed at the buffer stride. New `interintra_rect_hits()` counter + `decode_probe`
line + a delta-counted hard assert in
`a_real_aomenc_stream_with_interintra_decodes_pixel_exact` (NOT yet run: turn cap).
Result: superblock (0,17) is now exact; f1's first mismatch moves to (448,1152).

### The NEXT defect (this is the exact next step)
Same 2-frame reproducer, frame 1, block mi_row=112 mi_col=288, a 32x16 NEARMV
block (`TRACE_RECT_SPLIT mi_row=108 mi_col=288 bw=32 bh=16`):
  aomdec `EC_MODE_VAL mi_row=112 mi_col=288 mode=14 ref0=1 ref1=-1 mv0=(-32,-350) stack=4`
  ours   `EC_TRACE   mi_row=112 mi_col=288 mv=(-40,-340) is_new_mv=false bsize=32`
Entropy stays in sync (later blocks' rng match; the frame does not refuse), so it
is the mv STACK content/order, not a symbol. Suspect the same class as the fix
above: an mv-stack scan taking the square `side` instead of the block's true
`write_w`/`write_h` (`find_mv_stack` call sites decode.rs:18005 and :30091 --
check the bw4/bh4 arguments there first). Ladder: aomdec `EC_TRACE_MODE=1`
(`EC_MODE_MV` lines) vs ours for that mi; artifacts already on disk:
`~/.cache/midcut-tmp/am.txt` (aomdec, 2-frame cut) and `tr2.txt` (ours).

### Re-measured table (24 offsets, 2 s cut truncated to 4 frame OBUs,
### decode-order bytes vs instrumented aomdec, AFTER the fix)
`~/.cache/midcut-tmp/cen/after.tsv`. f0 is exact at EVERY offset. ss=0 is fully
exact (0,0,0,0) -- and is the ONLY offset with `interintra_rect: 0`, which is why
the head cut never saw this bug. Every other offset still diverges at f1 or f2
because of the NEARMV defect above; rect-interintra blocks per 4-frame prefix run
1..645. Sample: 0 -> 0,0,0,0 | 300 -> 0,2362684,5576522,10902022 |
2400 -> 0,11010633,7316581,15127483 | 4200 -> 0,607935,837,586 |
4800 -> 0,537,5724744,14232841 | 6000 -> 0,806,6192253,13728776.

### Owed / not done (turn cap)
- deferred: run `a_real_aomenc_stream_with_interintra_decodes_pixel_exact` with the
  new rect assert (`cargo test -p ec-av1 --lib interintra`) -- unblocked by a free
  turn; `cargo check -p ec-av1 --lib` is CLEAN on this tree.
- deferred: full suite. A run was started under unit `midcut-suite-r1-1788371174`
  on commit 43e9909 (log `$HOME/.cache/midcut-suite.log`) and had not reached
  `test result` at the cap.
- deferred: no fixture pinned. The film prefix is NOT pixel-exact yet (the NEARMV
  defect), so a hard-counter gate on it would have to assert a mismatch; pin it
  once the mv-stack defect is fixed.
- no refusal was lifted or moved; refusal_inventory/gate_coverage untouched.
