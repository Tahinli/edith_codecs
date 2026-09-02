# lane-golomb r4 — merge with main, and the 32x32-frame-edge cross-product isolated (NOT fixed)

## STEP 1 — merge (DONE, commit 17fc2a7 `Merge branch 'main' into lane-golomb`)

`git merge main` (main b51719f) conflicted in 4 files; 14 hunks in `decode.rs`.
Resolution (all by hand, no `cargo fmt`):

* `crates/ec-av1/src/cdf.rs`, `crates/ec-av1/src/cdf_state.rs` — main's, whole.
* `crates/ec-av1/src/decode.rs`
  * fi32x8 rows `(32,8)=>10 / (8,32)=>11` in `filter_intra_size_class`: main's.
  * `decode_rect_split`'s TU loop: main's rect `tx_w`/`tx_h` form, with golomb r3's
    TU-ORIGIN CLIP re-applied per axis inside it
    (`if tu_px >= y.true_width || tu_py >= y.true_height { continue; }`).
  * oddh's 16-level edge HORZ/VERT strip (replaces golomb's refusal): main's.
  * golomb's 6 edge-bit counter sites (`bump_edge32` / `bump_edge32_bit`) and the
    64-level + 32-level SECOND-HALF guards: kept, re-applied on top of inter4's
    MI-unit `decode_inter_block` signature (`sub16_to_mi(...)`) at both inter
    SB-level HORZ (`edge32` slot 2) and VERT (slot 3) arms.
* `lanes/golomb-r1.report.md` — lane's.

`cargo check -p ec-av1` + `cargo test -p ec-av1 --lib --no-run`: clean.

## STEP 2 — the cross-product: ISOLATED to one block, root cause NOT yet found

Gate `a_real_aomenc_stream_with_a_32x32_frame_edge_rect_partition_decodes_pixel_exact`
still FAILS on the merged tree, identically to merge-E's report:
`192x80 cq59 frames=5 8-bit frame 1 plane Y: 90 pixels differ, first at row 61 col 174
(ours 175 vs ffmpeg 174) [edge32=[1, 35, 1, 0, 0, 18, 0, 0]]`.

Stream reproduced standalone (md5 `48e1d78f31a4eb6a6ef5a67df9bd36fe`, 971 B):
`ffmpeg -f lavfi -i "testsrc2=size=192x80:duration=0.2:rate=25,noise=alls=6:allf=t+u:all_seed=59"
 -pix_fmt yuv420p ... | aomenc <the gate's flags> --cq-level=59` (script in the handoff).

### What the facts say (each with its command)

1. **The first divergent picture is DECODE-order picture 2, not shown-frame 1.**
   `EC_AV1_PREFILT_DUMP` on both decoders, luma compared per picture:
   f0 0 diffs, f1 0, **f2 130**, f3 84, f4 110. The shown-frame-1 report is the
   propagated copy. (Class `gate-blind-to-hidden-frames` applies to the READING of
   the gate message, not to the gate.)
2. **The wrong pixels are exactly one 16x16 block**: picture 2, rows 64..79 x
   cols 160..175 = mi(16,40), max |delta| 36 — a prediction-scale error, not rounding.
3. **libaom decodes mi(16,40) as SPLIT into two 16x16 leaves in every picture**
   (`EC_TRACE=1 aomdec | grep 'mi_row=16 mi_col=40 bsize=9'` → `value=3` x6);
   **we decode a 32-wide HORZ strip there in picture 2 only**
   (`EC_AV1_COMPIDX_DUMP=1` → `mi_row=16 mi_col=40 bsize=32 mode=0 mv0=(-30,-20)
   mv1=(16,0) ref0=1 ref1=7`, where libaom has `mode=18 (NEAR_NEARMV) mv0=(-16,0)`).
   That is inter4's SB/32-level inter rect arm consuming golomb's frame-edge bit —
   a path neither parent could reach.
4. **The edge bit is NOT the defect.** Instrumented pre-read state at the 32-level
   gathered read (temporary `DBG32PRE`, reverted): ctx = 2 (= libaom's 10 = bsl 2 * 4 + 2),
   CDF row and 2-entry gather identical in all 6 pictures, and the pre-read RANGE
   matches aomdec's `EC_PART ... rng=` in 11 of 12 reads. The one mismatch is
   picture 2 at mi(16,40): ours 39967, libaom 64226 — i.e. **we arrive at the bit
   already desynced**, and the wrong HORZ is the consequence.
5. **The desync starts inside the 16x16 INTER block at mi(16,32), picture 2.**
   Range ladder (ours `EC_AV1_TELL`/`EC_TRACE_MODE` vs aomdec `EC_TRACE_MODE`):
   * mi(16,32) entry / post_is_inter: ours 52732 == libaom `EC_MODE ... rng=52732`.
   * libaom then reads compound NEAR_NEARMV `ref0=1 ref1=7 mv0=(0,0) stack=3`
     (`EC_MODE_MV rng=39839`, `EC_MODE_VAL rng=56231`) and leaves the block at 62618.
   * ours leaves the block at 35347 (`TELL ... label=block_end tell=232 range=35347`),
     having consumed ~1 bit: no `EC_MODE_VAL` (single-ref print) and no `EC_COMPIDX`
     line for that block at all.
   So we read FEWER symbols than libaom for a `skip=true`, `is_inter=true` compound
   block at the bottom frame edge. First divergent element: the symbol(s) after
   `intra_inter` in `decode_inter_block` at mi(16,32) (ref-frame / compound-mode
   group), picture 2.
6. Coefficient ladders are otherwise identical (6426 aligned
   `EC_COEFF_STEP tag=base|sign|all_zero|eob|base_eob|after_bases|post_golomb` rng
   values, `diff` empty), so this is a LOCAL symbol-count divergence in one block
   that re-converges, not a whole-tile desync.

EVIDENCE: /tmp/.../scratchpad/{ours,aom}.f0..f5 | `EC_AV1_PREFILT_DUMP` both decoders on g4.obu | first divergent picture = 2, diff box rows 64..79 x cols 160..175 (130 px, max 36)
EVIDENCE: /tmp/.../scratchpad/{o,a}-cf.txt | `EC_TRACE_COEFF=1` both decoders, aligned tags | 6426 == 6426 lines, `diff` empty (entropy re-converges)
EVIDENCE: /tmp/.../scratchpad/{ours,aom}-mode.txt + tell.txt | `EC_TRACE_MODE` / `EC_AV1_TELL` ours vs aomdec | mi(16,32) p2 entry 52732 == 52732, exit ours 35347 vs libaom 62618

## Status / disposition

* merge: DONE and committed.
* the 32x32-frame-edge cross-product: **OPEN — fix-now, next round**, isolated to
  the symbol group read after `intra_inter` for the compound block at mi(16,32) in
  picture 2. Not a golomb-side defect in the edge bit (fact 4).
* verifier's r3 item (no `max_blocks_wide/high` clip on the INTER var-tx leaf loops
  at `decode.rs` ~14135 / ~14970, plus a 192x68 / 68x192 gate arm with a
  straddling-TU counter): **deferred — turn budget spent on the cross-product
  isolation above — unblocked by a fresh round; nothing about it is contradicted
  by this round's findings.**

## Suite

`cargo test -p ec-av1 --lib` (systemd unit, log `$HOME/.cache/golomb-suite-r4.log`): **364 passed, 2 failed, 31 ignored** in 547 s.
Failure 1 = the stale golomb refusal string `"a 16x16 block at the true frame edge coded as a rect strip rather than SPLIT"` still listed in `refusal_inventory.rs` after the merge took oddh_s lifted strip -- deleted in the follow-up commit (refusal_inventory 3/3 green after).
Failure 2 = the open cross-product above.
