# lane-av1-ibcskip2: skipped-intrabc sub8 chroma predicts the DV frame copy (r1)

## Charter

Close `av1ibc444rect` blocked_by item 2: `sub8_leaf_chroma444`'s skip arm
predicted the intra mode (DC for an intrabc leaf) where libaom predicts the
frame copy at the DV — `av1_build_inter_predictors_sb` runs for an intrabc
block regardless of `skip`; only the residual read is dropped. Work in
`edith_codecs-av1ibcskip2` @ b6d4b528 (`f23115aa` + cherry-picked
`9dbeafab`, the leaf8 chroma-extent fix that unblocked this fixture). The
RESERVED 4:2:0 group-tail chroma SKIP arm is untouched (see collision
statement).

## Defect (measured on this tree, fail-before)

The fixture decodes to completion at this HEAD (the parent lane needed a
throwaway leaf8 patch to measure; b6d4b528 landed it). Native reproduction,
oracle `aomdec --rawvideo` vs our decode:

- luma: 0 differing samples,
- U: 2794 differing samples, V: 2830, across 180 distinct 4x4 cells —
  175 of them EXACTLY the frame's skipped intrabc leaves (150 leaves
  covering 175 4x4 cells), the other 5 their immediate neighbours
  (poisoned edge state),
- a throwaway cell trace confirmed every diff cell sits on a
  `(intrabc=1, skip=1)` leaf or touches one.

Class: `skipped-intrabc-predicts-intra`. The 4:2:0 group-tail twin (flat DC
at the tails) is the RESERVED arm — not touched, not fixed here.

## Fix

New first arm in `sub8_leaf_chroma444` (`if let Some(dv) =
intrabc_dv.filter(|_| skip)`): bilinear MC of the CURRENT frame's U/V at the
DV (the unskipped arm's own `mv_to_q4` route), armed through
`set_palette_pred` and pushed with `ZERO_RESIDUAL` via
`push_intra`/`push_intra_rect` — the same armed-palette-slot route the two
luma skip arms (`decode_leaf_split4`, `decode_leaf_rect8`) already use; the
callers flushed the recon queue before their luma DV copy, so the source
samples are current. The inherited chroma `tx_type` slot is disarmed
(consumed by nothing — a skipped leaf reads no coefficient), keeping
`SKIPPED_INTRABC_CHROMA_ARM_HITS` semantics. Engagement counter
`SKIPPED_INTRABC_DV_COPY_HITS` + accessors pin the route for the gate.

## Stale gate flip (forced by the cherry-pick)

`a_lossless_444_intrabc_rect_leaf_walks_per_4x4_units` was RED at HEAD as
committed: it pinned the leaf8 `palette_window` panic ((3, 3) walk hits)
which b6d4b528 removed — the walk now fires its full census (339, 339).
Flipped per its own r1 doc to the full-frame exact `aomdec` comparison:
fixture len/FNV pins kept, (339, 339) census assert, `SKIPPED_INTRABC_
DV_COPY_HITS == 150` route assert, `aomdec --rawvideo` 0-diff on all three
planes (arm skipped with a notice only when the oracle binary is absent).

## Verification (measured)

- Mutation check: the arm mutated to count-but-predict-flat (grey 128
  prediction, flow intact, counter armed) → gate RED at "aomdec plane U:
  2503 samples differ". The pixel comparison is load-bearing; the ==150
  counter assert catches an unreachable arm.
- Gate green (post-fix, mutation reverted): "frame sample-exact vs oracle
  aomdec; 339+339 rect walks, 150 skipped-intrabc DV copies". Whole-fixture
  0 differing samples on Y, U and V — also re-measured out-of-gate via a
  throwaway plane dump + byte diff before the gate existed (0/0/0).
- Battery (superset of the parent's 16): every `intrabc`-named gate 17
  passed, every `lossless`-named gate 12 passed, every `sb128`-named gate 9
  passed (1 ignored diagnostic each where present), including the parent's
  full list, the leaf8oob gate from HEAD's cherry-pick, and this flipped
  gate. `cargo check -p ec-av1 --all-targets`: 0 warnings, 0 errors.

## 4:2:0 / reserved-arm collision statement

The diff lands only in `sub8_leaf_chroma444` (444-only: all six call sites
sit inside `if chroma_444` blocks; 4:2:0 chroma rides the group tails),
the `SKIPPED_INTRABC_DV_COPY_HITS` counter block in decode.rs, and the
flipped gate in stream.rs. The 4:2:0 group-tail chroma SKIP arm and the
`last_intrabc` tails in `decode_leaf_split4`/`decode_leaf_rect8` — the
RESERVED territory — are byte-for-byte untouched, and none of their call
sites pass `intrabc_dv = Some`, so 4:2:0 control flow through the edited
function is identical; the 420-heavy intrabc/lossless/sb128 battery above
confirms bit-identical behaviour.

## Not claimed

Exactness is claimed for the pinned fixture (whole frame, all planes) and
whatever the battery gates already pinned — not a general "every 4:4:4
intrabc stream decodes exact" statement. The `444_leaf8_oob.obu`
divergence noted in `lanes/av1leaf8oob.report.md` is that lane's deferral,
not re-measured here.

## Deferred

Nothing. The charter's deliverable (skip arm consults the DV, fixture gate,
4:2:0 identical, gates green) is complete on this branch.
