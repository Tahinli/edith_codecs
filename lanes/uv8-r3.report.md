# lane-uv8 r3 — GREEN (root cause: wrong tx_type ALPHABET on the 8x8 intra-in-inter leaf)

## Root cause
`decode_inter_block8`'s INTRA arm passed a hardcoded `TxbSet::Luma8` to `read_plane` for the
leaf's 8x8 luma transform. `TxbSet::Luma8` is the `reduced_tx_set == 1` set
(`EXT_TX_SET_DTT4_IDTX`, FIVE types); with `reduced_tx_set == 0` libaom's
`av1_get_ext_tx_set_type` (`av1/common/blockd.h`) returns, for `is_inter == 0` and
`tx_size_sqr == TX_8X8`, `EXT_TX_SET_DTT4_IDTX_1DDCT` — SEVEN types
(`TxbSet::Luma8Set1`). Same symbol, different alphabet: the CDF narrowing differed, so the
msac range diverged at the leaf's first `tx_type` and every following block desynced. The
desync manufactured the 30/30 "named refusals" the r1/r2 gate logs showed
(class [[refusal-from-own-desync]] + [[wrong-alphabet-same-value]]).

Charter suspect (1) (the `fimode_to_intradir` row) was already correct: `fi_tx_row` is applied
at every luma `txb` call site including this one, and `fimode_to_intradir[0] == DC_PRED`
equals this block's `y_mode` anyway, so that row was inert here.

## Change
- `crates/ec-av1/src/decode.rs:22057` — `TxbSet::Luma8` -> `txbset_for(8, reduced_tx_set)`
  (`reduced_tx_set` was already a parameter of `decode_inter_block8`).
- `crates/ec-av1/src/stream.rs:5897` — gate `counted` predicate tightened from
  "sum nonzero" to "all three uv classes fired on the same pixel-compared attempt".

## Sweep (class: hardcoded reduced-set TxbSet)
`grep -n 'TxbSet::Luma\(4\|8\|16\|32\),' decode.rs` — the only reduced-sensitive hardcodes are
sizes 8 and 4; after this fix there are ZERO outside `txbset_for`/`txbset_for_inter`. Every
`Luma16`/`Luma32` literal is alphabet-invariant to `reduced_tx_set` (`txbset_for` maps 16 and
32 identically for both values). The rect-strip and >=16 square intra-in-inter arms already go
through `txbset_for`/`txbset_for_inter` (decode.rs:6053, 7167, 7777, 7857, 9594, 11075).

## EVIDENCE
EVIDENCE: ~/.cache/uv8-tmp/ref.tu + ours3.tu | aomdec EC_TRACE_COEFF vs our EC_TRACE_COEFF on ~/.cache/uv8-tmp/a0.obu, per-TU (all_zero,eob) msac range ladder | before: first divergence at TU-line 3392 (ref eob rng=32776, ours 34568; the preceding tx_type read was ref rng=33532 vs ours 35240 at plane=0 tx_size=1, the mi(2,34) filter-intra leaf of decode-order frame 2) — after: 13986/13986 lines identical, whole stream, decode_probe rc=0 with no refusal
EVIDENCE: $HOME/.cache/uv8-gate-r3.log | `cargo test -p ec-av1 --lib -- --test-threads=1 --nocapture non_dc_chroma_8x8_intra_leaf` | 3 passed / 0 failed; every arm exact on attempt 0 with all three classes firing — 8bit/1tile dir 159 smooth-paeth 172 cfl 211; 10bit/1tile 112/126/120; 8bit/2tile 83/66/80; buckets counted-exact=1 uncounted-exact=0 named-refusals=0 on each

## Residue
- deferred: the 8x8 intra-in-inter leaf whose `tx_depth` splits to four TX_4X4 units is still
  refused by name (decode.rs ~21996). Unblocked by an arm with `--enable-tx-size-search=1`
  plus a 4x4-TU prediction loop in this leaf path — out of this lane's charter, which fixed the
  depth-0 alphabet. Its refusal string stays in refusal_inventory.rs.
- accepted: `frames` from `decode_stream` is decode order; the gate recipe has
  `--lag-in-frames=0 --auto-alt-ref=0`, so decode order == display order and all 8 frames are
  compared (class [[gate-blind-to-hidden-frames]] does not apply).
