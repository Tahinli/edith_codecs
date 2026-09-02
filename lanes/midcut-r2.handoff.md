# lane-midcut r2 handoff (coordinator turn cap)

Tip: lane-midcut, merged main (243f125) cleanly at STEP 0 (decode.rs/stream.rs auto-merged, `cargo check -p ec-av1` clean).

## r1's named defect was STALE on both counts
1. `find_mv_stack` call sites decode.rs:20959 and :23046 ALREADY pass
   `bw4 = write_w/4`, `bh4 = write_h/4` (lane-rect r2 did that). No `side, side`
   remains at any single-ref/compound mv-stack site; the sub-8 leaf sites pass
   the literal `1,1` / `2,2` libaom uses. Nothing to fix there.
2. The mv stack at mi(112,288) is IDENTICAL to aomdec's, entry for entry:
   i0 (-24,-352) w=700 | i1 (-32,-350) w=664 | i2 (-40,-340) w=644 | i3 (-20,-360) w=4
   (both decoders, both stack=4). Only the DRL index differed, because the
   entropy state at that block already differed (aomdec EC_MODE rng=56215 vs
   ours 41892) -- r1's "entropy in sync" premise was wrong. It was a downstream
   symptom, never a defect.

## Root cause found and FIXED (commit cde8893)
Reproducing this cut now needs `EC_INTRA16X4_DECODE=1` (main re-added the
16x4-in-inter refusal); without it the probe refuses and every trace truncates.

First real divergence in frame 1 (2-frame cut, decode order): the 32x8 INTRA
strip at mi(118,280), inside an inter frame, reading its `tx_depth` symbol.
  aomdec: `EC_ISTEP mi_row=118 mi_col=280 name=tx_depth val=0 ctx=2 cat=2 rng=63652`
  ours  : `EC_TXCTX  mi_row=118 mi_col=280 w=32 h=8 above_px=16 left_px=16 ctx=1`
Both entered at rng 42394 (the `use_filter_intra` rung matches exactly), so the
ROW of `tx_size_cat2` is the whole defect (class wrong-alphabet-same-value: the
value is 0 in both, only the range moves).

`tx_size_context_rect` is the KEY FRAME approximation of libaom
`get_tx_size_context` (`pred_common.h:342`): it reads the deblock tx grid and
never applies "an INTER neighbour contributes its BLOCK size, not its transform
size". The above neighbour here is the inter 32x8 at mi(116,280) whose var-tx
tree had split to 16-wide TUs, so libaom sees 32 >= 32 (above=1) while we saw
16 >= 32 (above=0). The 1:4 16x4 strip already branched to
`tx_size_context_txfm_rect` at its own call site (decode.rs:9116); the other six
rect intra readers did not. Fix = one branch inside `tx_size_context_rect`
itself (decode.rs:6737) keyed on `INTRA_IN_INTER_MODE`, so every call site
inherits it.

### Measured effect (EVIDENCE lines)
EVIDENCE: ~/.cache/midcut-tmp/{m_am.txt,m_o2.txt} | greedy in-order match of every
`EC_MODE mi_row= mi_col= rng=` line, aomdec vs ours, 2-frame cut | first mismatch
moved from aom idx 1998 (mi 120,272) to ALL MATCH 6871/6871 inter blocks.
EVIDENCE: ~/.cache/midcut-tmp/d/{our,aom}.f{0,1} (EC_AV1_PREFILT_DUMP16, both
decoders, regenerated this round) | numpy plane compare | f0 EXACT; f1 luma
4386319 -> 115 wrong samples, U 342, V 860, every delta |1|, luma bbox rows
552..1552 cols 488..969.

## Also added (compiles, NOT yet gated -- next agent's first job)
`decode::intra_rect_in_inter_txctx_override_hits()` (decode.rs, next to
`intra_rect_in_inter_split_tx_hits`): counts ONLY the rect-intra-in-inter blocks
whose ctx actually MOVED under the override, i.e. the blocks the bug mis-read --
a plain "rect intra in inter" tally is green on the buggy code too
(class gate-blind-to-feature).

## Exact next steps
1. Print the new counter in `crates/ec-av1/examples/decode_probe.rs` (model:
   the `interintra_rect:` line at :46) and hard-assert a positive delta in
   `intra_rect_in_inter_split_tx_gate` / `intra_rect_in_inter_gate`
   (stream.rs:5639 / :29785, both bit depths). If neither arm fires the counter,
   add an aomenc arm with rect partitions + tx-size search on (the override only
   moves the row once a neighbour's var-tx tree splits).
2. Chase the 115/342/860 |1| residue in frame 1 (entropy is now provably exact,
   so it is pure reconstruction: MC rounding or an intra edge, luma bbox above).
3. Re-run the 24-offset decode-order table (`~/.cache/midcut-tmp/cen/`, r1's
   scripts; the r1 table `cen/after.tsv` is now stale) and report how many
   offsets have frame 1 exact.
4. Then the named gates (mv_stack, interintra, rect, hidden_arf,
   refusal_inventory, gate_coverage) and the suite unit. None was run this round.
5. No fixture pinned: frame 1 is not exact yet (residue above).
6. No refusal lifted or moved; refusal_inventory/gate_coverage untouched.
