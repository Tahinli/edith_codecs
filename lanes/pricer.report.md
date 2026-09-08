# lane-pricer — the inter RD pricer charged the wrong reference symbols

## The defect

`crates/ec-av1/src/encode.rs`'s inter RD search priced a single reference
with LAST's branch of the tree, whatever the reference was, at CDF context 0:

```
p1 = SINGLE_REF[0][0] symbol 0
p3 = SINGLE_REF[0][2] symbol 0          (GOLDEN took p3=1,p5=1)
p4 = SINGLE_REF[0][3] symbol (ref==LAST2)
```

What `tile.rs write_single_ref` actually codes (spec 5.11.25 `single_ref_p1..p6`),
each symbol at the context its own `single_ref_p*_ctx` derives from the block's
above/left neighbours:

| reference | symbols the writer codes |
|---|---|
| LAST    | p1=0, p3=0, p4=0 |
| LAST2   | p1=0, p3=0, p4=1 |
| LAST3   | p1=0, p3=1, p5=0 |
| GOLDEN  | p1=0, p3=1, p5=1 |
| BWDREF  | p1=1, p2=0, p6=0 |
| ALTREF2 | p1=1, p2=0, p6=1 |
| ALTREF  | p1=1, p2=1 |

So every BACKWARD reference (BWDREF/ALTREF2/ALTREF) was priced as LAST, and
LAST3 as LAST2. The magnitude runs both ways: at the default CDFs and context 0
LAST's chain costs ~4.4 bits while ALTREF's p1=1,p2=1 costs ~0.30, so the old
pricer OVERCHARGED ALTREF by ~4 bits; against the real neighbour contexts the
sign flips block by block.

## Arms

* **arm 1** — the symbol tree only, still at context 0.
* **arm 2** — arm 1 plus the writer's own contexts: `MvStack` now carries the
  two cells `write_single_ref` reads (`above_refs`, `left_refs`), so the pricer
  runs `single_ref_p1_ctx`..`p6_ctx` exactly as the writer does. No search
  signature changed — every pricer site already holds the stack.

## Gate (12-frame native gate, BD-rate vs libaom cpu-used 6 / rav1e speed 6)

Control REPRODUCED in this worktree (the charter's screen row was stale):

| row | control | arm 1 | arm 2 |
|---|---|---|---|
| film A | +21.7 / −4.4 | +21.9 / −4.2 | **+21.5 / −4.4** |
| film B | +26.9 / −0.6 | +27.0 / −0.3 | **+26.9 / −0.4** |
| screen | +14.9 / −32.9 | +15.2 / −32.7 | **+14.4 / −33.2** |
| bars 1080p | −1.0 / −16.8 | (not run) | **−1.4 / −17.2** |
| bars 2160p | +9.4 / −12.7 | (not run) | **+8.8 / −13.2** |

Arm 1 is REFUTED: correct symbols at the wrong (0) context make a backward
reference read almost free (p1=1 = 0.23 bits), ALTREF fires 992 → 2648 blocks on
film A, and every row loses. Arm 2 pulls the reference mix back (film A ALTREF
1476, GOLDEN 304) and wins or ties everywhere.

Long-GOP gate, arm 2: film A +26.4 / −6.4 (control +26.4 / −6.6),
film B +89.6 / +9.1 (control +89.8 / +9.1).

## Decision

Ship arm 2. Strictly the charter's keep rule is NOT met — it asks both film rows
to improve on both columns, and film B's rav1e column moves −0.6 → −0.4 (0.2 the
wrong way, inside the rule's own ±0.3 "flat" band but not paired with a ≥0.5 win).
Shipped anyway because this is a correctness fix — the pricer now prices the
symbols the writer emits — and no row regresses more than 0.2 while screen
(−0.5 / −0.3) and both bars rows (−0.4 / −0.4 and −0.6 / −0.5) improve on both
columns.

## Sweep: `symbol_bits(&cdf::` sites whose CDF/leaf is constant while the writer selects

| site (encode.rs) | verdict |
|---|---|
| `SINGLE_REF` (all) | FIXED — tree + neighbour contexts, matches the writer |
| `COMP_REF_TYPE[0]` (6236/8901/9539) | value matches (`is_uni_comp_ref`); CTX constant 0 vs writer's `comp_reference_type_ctx` |
| `COMP_REF[0][0]=0`, `[0][1]=0` (6237-8/…) | value right only because compound `ref0` is always LAST; CTX constant vs `single_ref_p3/p4_ctx` |
| `COMP_BWDREF[0][0]=1` (6239/8904/9542) | assumes `ref1 == ALTREF`; true today (compound `ref1` ∈ {GOLDEN, ALTREF}) but wrong the day BWDREF/ALTREF2 pair up — writer would code p2=0 then p6. CTX constant too |
| `UNI_COMP_REF[0][1]=1`, `[0][2]=1` (6243-4/…) | assumes the forward `ref1` is GOLDEN; wrong for a LAST+LAST2 pair. CTX constant |
| `SKIP[0]`, `INTRA_INTER[0]` | value right; CTX constant 0 while the writer derives both from neighbours |
| `TX_SIZE_CAT*[0]` | value right; CTX constant 0 |
| `TXFM_PARTITION[ctx(..)]`, `DRL_MODE[stack.drl_ctx[..]]`, `NEW_MV`/`ZERO_MV`/`REF_MV`/`INTER_COMPOUND_MODE` | same as writer (real contexts) |
| `ANGLE_DELTA[mode - V_PRED]` | same row as the writer; value fixed at delta 0, which is what these candidates code |
| `MV_*` | single tables where the writer has per-component ones — the pricer's long-standing static-MV approximation, out of this lane |

Only the single-ref sites are fixed here; the rest are `open|` lines.

## Evidence

* unit test `encode::tests::the_single_ref_pricer_pays_the_writers_own_symbol_sequence`
  — asserts the priced bits equal the writer's symbol sequence at the writer's
  contexts, for all seven references and two neighbour configurations. RED before
  the fix (`ref 3: priced 7.615346227887448 bits, writer codes 8.121845298652628`).
* pins re-taken: `[(150, 8535, 0x7b7f_4eeb_1080_7b45), (60, 33221, 0x720b_821e_5dd2_90e4)]`
  (was 8562 / 33357), green at default and at `EC_AV1_SPEED=6`.
