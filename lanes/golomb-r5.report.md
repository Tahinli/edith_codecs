# lane-golomb r5 — the 32x32-frame-edge cross-product ROOT-CAUSED and fixed (luma exact); gate still RED on a newly-exposed chroma defect

## What changed

* `crates/ec-av1/src/mvstack.rs:1663-1703` (commit `13aebbe`) — `find_mv_stack_compound`
  now runs libaom's three `tpl_sample_pos` EXTENSION samples
  (`mvref_common.c:563-600`, `allow_extension` + `check_sb_border` at
  `mvref_common.c:316`, mask hardcoded `mi_size_wide[BLOCK_64X64]`=16). The
  single-ref twin (`mvstack.rs:863-891`) already carried them; the compound
  path dropped them. Same hunk as lane-sb128 `d6cca7d` (independent lane, same
  root cause found from the other end).

Nothing else. The r4 debug ladder used to find this was removed before commit.

## Root cause (H3 of the charter; H1 skip_mode and H2 comp-ref ctx both exonerated)

Charter's isolated element: `192x80 cq59 frames=5 8-bit`, DECODE-order picture 2,
16x16 inter block at mi(16,32), entry range 52732 == libaom, exit ours 35347 vs
libaom 62618.

A temporary compound range ladder (`EC_LAD` after `read_comp_mode`, after
`read_compound_ref_frames`, after `read_inter_compound_mode`, after
`assign_compound_mv`) showed:

| element | ours (before) | libaom |
|---|---|---|
| block entry (post `intra_inter`) | 52732 | 52732 |
| `is_compound` | true | true |
| ref pair | `ref0=1 ref1=7` | `ref0=1 ref1=7` |
| **compound stack** | **2 entries** `[(-16,0)/(14,0) w=672, (0,0)/(0,0) w=672]` | **3 entries**, same two + `(-10,0)/(10,0) w=2` |
| compound mode | 1 = `NEAR_NEARMV` | 18 = `NEAR_NEARMV` |
| post-mv | 50080 | 39839 |

So `skip_mode` and the whole reference-frame group were already right (H1/H2 dead).
With `ref_mv_count == 2` our `NEAR_NEARMV` drl loop condition
(`count > idx + 1`, idx=1) is false, so we never read the `drl_mode` symbol
libaom reads at `ref_mv_count == 3` — **one bit short**. Every later symbol in the
tile then read from the wrong place, and the 32-level gathered edge bit at
mi(16,40) resolved HORZ where libaom split (the r4 "edge bit" symptom was the
consequence, not the cause — matching r4 fact 4).

The missing candidate has weight 2: `add_tpl_ref_mv`'s temporal vote
(`ref_mv_weight[idx] = 2 * weight_unit`), from extension sample
`(voffset, hoffset) = (4,4)` at mi(16,32) (`16&15=0`, `32&15=0`, both in range).

After the fix the ladder is exact through the block: stack 3 == 3 with identical
entries and weights, post-mv 39839 == libaom `EC_MODE_MV rng=39839`, next block
entry 62618 == 62618, and mi(16,36) stack weight `w=10` == 10 (was 8, i.e. the
same missing weight-2 vote folded into an existing entry).

EVIDENCE: /tmp/.../scratchpad/gl-r5-ours.txt (before) + gl-r5-ours2.txt (after) vs aom-mode.txt | `EC_TRACE_MODE` ours (temporary EC_LAD compound ladder) vs instrumented aomdec on g4.obu md5 48e1d78f31a4eb6a6ef5a67df9bd36fe | mi(16,32) p2 stack 2->3, post-mv 50080->39839 == libaom, next-block entry 39296->62618 == libaom

EVIDENCE: /tmp/.../scratchpad/g4-ours.yuv vs g4-aom.yuv | `decode_probe EC_PROBE_OUT` vs `aomdec --rawvideo` on g4.obu | plane Y ALL 5 FRAMES EXACT (was f1 90, f2 143, f3 106 wrong pixels); chroma residue below

## Gate: still RED, on a DIFFERENT defect

`cargo test -p ec-av1 --lib -- a_real_aomenc_stream_with_a_32x32_frame_edge_rect_partition`

* baseline (`fbc427c`): `192x80 cq59 frames=5 8-bit frame 1 plane Y: 90 pixels differ, first at row 61 col 174 [edge32=[1,35,1,0,0,18,0,0]]` (log `$HOME/.cache/golomb-gate-r5-base.log`)
* with the fix: `192x80 cq35 frames=5 8-bit frame 1 plane U: 335 pixels differ, first at row 0 col 56 [edge32=[0,36,0,0,0,18,0,0]]` (log `$HOME/.cache/golomb-gate-r5.log`)

The gate now fails EARLIER in the cq sweep (35 < 59) because **cq35 was never
compared before**: at baseline that stream hit a false named refusal
(`"an inter 16x16-level AB or 1:4 partition"`) produced by our own desync, so the
gate `continue`d past it. With the stack fixed it decodes end to end. Class
`refusal-from-own-desync` + `refusal-hides-a-defect`, both already in memory.

The residual defect is **chroma-only and pre-existing**: on both streams luma is
bit-exact in every frame while U/V carry scattered ±1..±3 across the whole plane
(g4 baseline already had f1 U 75 / V 185 wrong under the luma failure). The key
frame (f0) is exact, so it is inter chroma only, and V is consistently ~2.5x worse
than U.

EVIDENCE: /tmp/.../scratchpad/g35-ours.yuv vs g35-aom.yuv | decode_probe vs aomdec on g35.obu (md5 9037f5b21db95d35e71f91f040fc33e1, recipe scratchpad/gl-gen.sh, hashed twice identical) | f0 exact, f1..f4 luma exact, U 335/324/1183/691 and V 408/412/1158/746 wrong, deltas -4..+5
EVIDENCE: /tmp/.../scratchpad/g35-ours.txt vs g35-aom.txt | `EC_TRACE_MODE` block-entry ranges, ours vs aomdec | all 48 of our `EC_MODE` ranges match libaom's in order — entropy fully in sync, so this is a pure chroma reconstruction defect, not a symbol defect

## The r3 verifier item (`max_blocks_wide/high` clip on the INTER var-tx leaf loops)

**Already implemented, at the shared entry point** — no code needed
(`decode.rs:10317`):

```
fn read_var_tx_size(..., (max_w_mi, max_h_mi), ...) {
    if blk_row >= max_h_mi || blk_col >= max_w_mi { return; }
```

`max_w_mi`/`max_h_mi` are computed as `mi_cols.saturating_sub(at_mi.1)` /
`mi_rows.saturating_sub(at_mi.0)` at BOTH inter callers
(`decode.rs:10402-10403` square, `decode.rs:10630-10631` rect), i.e. libaom's
`max_blocks_wide/high` in the same 4x4 units. A leaf whose top-left mi is outside
the frame is therefore never emitted into `vartx_leaves`, and the two reconstruct
loops (`decode.rs:15852`, `decode.rs:16830`) cannot see one. Adding a second
`tu_px >= y.true_width` guard there would be dead code.

## Status / disposition

* the mi(16,32) compound-stack desync: **FIXED**, luma bit-exact on the charter's stream.
* the chroma ±1 residue: **fix-now, next round** — first block to bisect is chroma
  plane U/V of `g35.obu` frame 1 (entropy is proven in sync, MVs proven right by
  exact luma, so it is chroma prediction/rounding or the per-plane chroma
  quantizer; V-worse-than-U points at a per-plane term, cf. memory
  `parsed-then-discarded`). Unblocked by a fresh round; nothing here contradicts it.
* r3 verifier item (var-tx clip): **accepted as already-satisfied**, cited above.
  Its 192x68 / 68x192 gate arm: **deferred — the gate it would join is RED, so a
  new arm would only add noise — unblocked by the chroma fix above.**

## Suite

`cargo test -p ec-av1 --lib` under a user systemd unit, log `$HOME/.cache/golomb-suite-r5.log`: re-armed as unit `golomb-suite-r5` after restoring the `#[ignore]` attribute on `a_32x32_frame_edge_rect_partition_with_a_flat_band_decodes_pixel_exact` (the r2 open-defect twin; the attribute was lost in the r4 merge and the test ran, and failed, in the mid-round run). Expected failures: the one open gate above. The coordinator reads the log.
