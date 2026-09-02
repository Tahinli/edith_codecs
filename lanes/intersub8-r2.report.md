# lane-intersub8 r2 — GREEN gate, one root cause, 8x4/4x8 deferred

## 1. Merges
* `c048e29` main (8e9ba81) into lane-intersub8 — no conflicts, `cdf.rs` untouched.
* `e8bb7e7` lane-r14 (b86eb38) — `read_var_tx_size` is now `(tx_w,tx_h)`-general with the
  full `sub_tx_size_map`, plus `read_inter_plane_rect`. `b86eb38` was NOT on main
  (`git merge-base --is-ancestor b86eb38 main` → false; main only has the earlier
  `6699ce6` merge of `a2e2e29`).

## 2. Root cause of the frame-5 red (fixed, `96eff0c`)
`crates/ec-av1/src/decode.rs` (8x8 inter leaf, compound arm, ~19940): the branch
**never called `resolve_interp_filter`**. It hard-coded `Regular` for both taps and left
`leaf_filter_syms` at the `[3, 3]` "no filter" sentinel that `Neighbours::record_inter*`
then stored. libaom reads `read_mb_interp_filter` for every inter block right after
`read_compound_type` (`decodemv.c` `read_inter_block_mode_info`), and when
`av1_is_interp_needed` is 0 (`skip_mode`, `WARPED_CAUSAL`, non-translational GLOBALMV)
`set_default_interp_filters` stores **EIGHTTAP_REGULAR (0)**, not a sentinel. Any later
block whose `ref_frame[0]` matches that leaf's ref0 **or ref1** then computes
`av1_get_pred_context_switchable_interp` from 3 instead of 0 → wrong `switchable_interp`
CDF row → tile desync.

Bisection (class *compare range not tell*): on `a1_8.obu` (192x128, 8-bit, cq 12, sp 3)
the 4x4 sub-block at mi(14,46) matched the oracle at both of aomdec's own print points —
entry `rng=39095`, post-`assign_mv` `rng=60392` — and diverged only across the interp
filter read: ours `51740` vs aomdec `57815`. `IFDBG4` showed `left_ref=1 left_ref1=Some(4)
ref=4 left_filt=[3,3]`: the left neighbour was a **skip_mode compound 8x8 leaf** (its own
`EC_MODE` → `EC_MODE_VAL` range never moved, i.e. zero symbols).

Fix reads the filter properly (`skip_mode` → `force_regular`, ctx from the same ref-match
rule the other leaves use), feeds `h_filter`/`v_filter` to all six `predict_compound_*`
calls of that branch and records the real symbols. This also removes the long-standing
`lane-av1comp` corner-cut: a **non-skip** compound 8x8 leaf used to skip the symbol
entirely, which was a latent desync for any stream that codes one.

## 3. Out-of-scope defect found and named (NOT hidden)
Same recipe at **cq 8 / cq 10**: three luma samples of `|d| = 1` on decode-order frames 3
and 4 (x=189 resp. 186, y=116/120/124), with **zero** sub-8x8 inter split groups in those
frames. Re-encoding the identical recipe with `--enable-cdef=0` decodes all six frames
pixel-exact ⇒ a **CDEF** defect of another shape.
`deferred: CDEF ±1 luma at cq 8/10 on the 192x128 min-partition-4 recipe — belongs to a
CDEF lane, not to sub-8x8 partitions — unblocked by a CDEF-focused gate`.
The gate does not switch CDEF off (that changes aomenc's RD and kills every firing
attempt); it moves to cq {12,14}, where a full sweep shows no mismatch at all.

## 4. Gate
`cargo test -p ec-av1 --lib sub8x8_inter_split` →
`test result: ok. 1 passed; 0 failed` (414 filtered out).
Schedule measured, not guessed: sweep of cq {8,10,12,14,16,18,20} × sp {3,6,9,12} × 8/10-bit
(`decode_probe`'s new `sub8_inter_split:` counter + an ffmpeg rawvideo compare).
Firing attempts: 8-bit cq12/sp3 = 1 group, 10-bit cq12/sp6 = 3 groups, both pixel-exact,
every decode-order frame compared Y/U/V; `out_of_scope_mismatch == 0`.

EVIDENCE: ~/.cache/intersub8-tmp/a1_8.obu + ours.if.log + oracle.mode.log | aomenc 192x128 8-bit cq12 min-partition-size=4, our EC_MODE/EC_MODE_MV rungs vs aomdec EC_TRACE_MODE range ladder at mi(14,46), then a full-frame ffmpeg compare | ladder diverged at the interp filter (51740 vs 57815); after the fix all 6 frames byte-identical (`cmp.py` frames 0..5 exact)
EVIDENCE: ~/.cache/intersub8-tmp/sweep.sh output (56 encodes) | cq×sp×depth sweep, decode_probe counter + ffmpeg compare | 2 firing attempts both EXACT, all cq≥12 attempts EXACT or named-refusal, cq 8/10 carry the CDEF ±1
EVIDENCE: fixtures/realworld/hunger-games.obu | decode_probe on the merged tree | now stops at "an inter 8x8 HORZ/VERT partition (8x4/4x8 inter leaves...)" — the sub-8x8 SPLIT refusal no longer blocks it

## 4b. Suite
`intersub8-suite-1788328909.service`, log `$HOME/.cache/intersub8-suite-r2.log`:
`test result: ok. 382 passed; 0 failed; 33 ignored; 0 measured; 0 filtered out; finished in 1018.88s`.
This is the sibling sweep the interp-filter fix owes: it touches every compound 8x8 inter leaf.

## 5. Not done this round
`deferred: 8x4/4x8 inter leaves (charter step 3) — the turn budget went to the root-cause
hunt and the gate schedule — unblocked by a fresh round on this branch; r14's general
read_var_tx_size is now merged here, so the remaining pieces are the LumaRect8x4Inter /
LumaRect4x8Inter TxbSets (EXT_TX_SET_ALL16, sqr_up TX_8X8) and the 2-sub-block chroma`.
`deferred: a --tile-columns=1 arm on this gate (COMMON neighbour-map rule) — same budget —
unblocked by the next round`.
The `an inter 8x8 HORZ/VERT partition ...` refusal therefore STAYS; no refusal string was
removed this round, and `refusal_inventory`/`gate_coverage` are unchanged.
