# lane-uv8 r3 — HANDOFF (GREEN gate; suite still running at the turn cap)

## ROOT CAUSE (fixed, committed)
`decode_inter_block8`'s INTRA arm passed a hardcoded `TxbSet::Luma8` to `read_plane` for the
8x8 leaf's LUMA transform. `TxbSet::Luma8` is the `reduced_tx_set == 1` alphabet
(`EXT_TX_SET_DTT4_IDTX`, FIVE types). libaom `av1_get_ext_tx_set_type` (`av1/common/blockd.h`)
returns `EXT_TX_SET_DTT4_IDTX_1DDCT` (SEVEN types, `TxbSet::Luma8Set1`) for `is_inter == 0`,
`tx_size_sqr == TX_8X8`, `use_reduced_set == 0` — and an INTRA leaf inside an INTER frame is
`is_inter == 0`. Same symbol value, different narrowing (class [[wrong-alphabet-same-value]]);
the msac range diverged at the leaf's first `tx_type` and desynced the rest of the tile, which
manufactured the 30/30 "named refusals" of r1/r2 (class [[refusal-from-own-desync]]).
FIX: `crates/ec-av1/src/decode.rs:22057` -> `txbset_for(8, reduced_tx_set)` (the parameter was
already in scope). Charter suspects (1) `fimode_to_intradir` and (2) `tx_depth` were BOTH already
correct: `fi_tx_row` is applied at every luma txb call site, `fimode_to_intradir[0] == DC_PRED`
equals this block's y_mode anyway, and `read_block_tx_size` returns depth 0 here.

## GATE AFTER THE FIX — GREEN (unit uv8-gate-r3, log $HOME/.cache/uv8-gate-r3.log)
`cargo test -p ec-av1 --lib -- --test-threads=1 --nocapture non_dc_chroma_8x8_intra_leaf`
-> 3 passed / 0 failed, 5.53s. Every arm exact on ATTEMPT 0 with all three chroma classes
firing (directional / smooth-paeth / CfL):
  8-bit 1-tile  seed42 cq12: 159 / 172 / 211
  10-bit 1-tile seed42 cq12: 112 / 126 / 120
  8-bit 2-tile  seed42 cq12:  83 /  66 /  80
buckets on each arm: counted-exact=1 uncounted-exact=0 named-refusals=0 (attempts 1).
All 8 frames of each stream pixel-compared vs ffmpeg (Y/U/V), no mismatch.
`stream.rs:5897` tightened: `counted` now requires ALL THREE classes on the same compared
attempt (was sum-nonzero) — this stricter predicate is what the green run above satisfies.

## REFUSAL STATE
The lifted non-DC-uv_mode refusal STAYS LIFTED (gate green). Still refused by name and NOT part
of this lane: "an 8x8 intra leaf in an inter frame whose tx_depth splits it into 4x4 transform
units" (decode.rs ~21996) and "a nonzero angle delta on an 8x8 intra leaf in an inter frame".

## SWEEP (class: hardcoded reduced-set TxbSet)
`grep -n 'TxbSet::Luma\(4\|8\|16\|32\),' crates/ec-av1/src/decode.rs` — after the fix there are
ZERO reduced-sensitive hardcodes outside `txbset_for`/`txbset_for_inter`; every remaining
`Luma16`/`Luma32` literal is alphabet-invariant to `reduced_tx_set`. The rect-strip and >=16
square intra-in-inter arms already route through `txbset_for` (decode.rs 6053, 7167, 7777,
7857, 9594, 11075).

## SUITE — RUNNING, NOT VERIFIED
Unit `uv8-gate-r3.service` (still active at the turn cap) is now in its suite phase:
log `$HOME/.cache/uv8-suite-r3.log`. At hand-off: 233 tests `ok`, 0 FAILED, 0 `failures:` lines
(of ~425). NO green suite claim.

## EXACT NEXT STEP
1. `grep -E "^test result|FAILED|^failures:" $HOME/.cache/uv8-suite-r3.log` once the unit goes
   inactive; expect N/0. If a gate reddens, it is a SIBLING of this alphabet fix — re-run it
   before touching the fix.
2. Then the lane is mergeable at its tip.

## EVIDENCE
EVIDENCE: ~/.cache/uv8-tmp/ref.tu + ours3.tu | instrumented aomdec EC_TRACE_COEFF vs our EC_TRACE_COEFF on ~/.cache/uv8-tmp/a0.obu, per-TU (all_zero,eob) msac RANGE ladder | before: first divergence at TU-line 3392 (ref eob rng=32776 vs ours 34568; the preceding tx_type read ref rng=33532 vs ours 35240 at plane=0 tx_size=1, the mi(2,34) filter-intra leaf of decode-order frame 2) — after: 13986/13986 identical over the whole stream, decode_probe rc=0, no refusal
EVIDENCE: $HOME/.cache/uv8-gate-r3.log | the three-arm gate command above | 3 passed / 0 failed, counted-exact=1 named-refusals=0 per arm, counts as listed
