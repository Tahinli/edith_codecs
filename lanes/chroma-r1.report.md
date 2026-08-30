# lane-chroma r1 report — smooth/paeth chroma refusal

VERDICT: NOT LANDED — root cause found and localized, refusal kept (reverted
to HEAD). This is the charter's documented fallback outcome: "the report
explains precisely what a chroma block needs that is missing."

## What was established (charter step 1-2)

The predictor itself (`crate::intra::predict`, `SMOOTH_PRED`/`SMOOTH_V_PRED`/
`SMOOTH_H_PRED`/`PAETH_PRED` branches) IS shared between luma and chroma,
and its formula matches spec 7.11.2.6 / libaom `intrapred.c` exactly (weight
tables, `round2` shifts, no-neighbour `Edges::build` fallback of 127/129/128
which is the spec's own default for a corner block). The chroma call path
(`PlaneBuf::reconstruct`) is the SAME function luma uses — there is no
missing chroma-specific machinery for the predictor math itself.

`git log -S "smooth or paeth chroma"` shows the refusal was added in the SAME
commit (4f93fb2, "chroma tx_type derivation…") that landed the predictor
plumbing — its own comment already named the reason: a corner block with no
above/left neighbour, fed `SMOOTH_PRED`, produced a wrong pixel (worst delta
78 vs ffmpeg), traced 2026-08-27. So the earlier round deferred this because
it was HARD (a real bug found), not because it was merely out of scope.

## What this round found (charter step 3, attempted, then reverted)

Removed both refusals (`decode.rs` ~1970 and ~2458, the two `read_intra_mode`
variants), added a `SMOOTH_UV_HITS` counter, and wrote
`a_real_aomenc_stream_with_smooth_paeth_chroma_decodes_pixel_exact` (same
structure as the existing `..._with_directional_chroma_...` gate, but
`--enable-smooth-intra=1 --enable-paeth-intra=1`). First attempt (seed 42)
FAILED: luma mismatched ffmpeg, not chroma.

Grepped every other real-aomenc gate in `stream.rs` (23 call sites): every
single one passes `--enable-smooth-intra=0 --enable-paeth-intra=0`. So
smooth/paeth has NEVER been exercised end-to-end against a real encoder
stream before this lane, for EITHER plane — the charter's premise "already
exercises them for LUMA" holds only in `intra.rs`'s own unit tests, not
against a real bitstream. This is a real, previously-unverified defect, not
a chroma-specific gap.

Root-caused with `EC_AV1_TRACE=1` (mode dump) + a throwaway pixel-diff test
against the pinned stream, with and without the loop filter
(`--loopfilter-control=0` reproduces it too, ruling out deblocking):

- First mismatch (no-LF stream): `row=16 col=48`, inside luma block
  `px=48 py=16 side=16 mode=5` (`D113_PRED`, a directional mode).
- Its ABOVE neighbour is `px=48 py=0 side=16 mode=11` (`SMOOTH_H_PRED`).

This is exactly the corner-cut already named in the code, verbatim, at the
`reconstruct()` call site's own doc comment:

> `smooth_neighbor` (spec `get_intra_edge_filter_type`) is left `false`:
> exact for chroma here (a smooth `uv_mode` is refused before decode reaches
> this call) and a corner-cut for LUMA -- ceiling is one wrong filter-strength
> bucket on a block whose real neighbour predicted smooth; upgrade path is
> threading `Neighbours::above_mode`/`left_mode`'s `SMOOTH_PRED..=SMOOTH_H_PRED`
> membership down instead of `false`.

Enabling `--enable-smooth-intra=1` makes the RD search reachable to actually
land SMOOTH_H on a block, which is exactly the case the corner-cut's own
comment predicted would break: `intra_edge_filter_strength`'s
`smooth_neighbor` threshold bucket is picked wrong for the directional
neighbour below it, producing the observed off-by-one-ish deltas
(`intra_edge_filter_strength` returns the wrong strength index -> different
filter kernel -> pixels shift by 1-2).

## Why it wasn't landed this round

The fix is well-scoped (`Neighbours::above_mode`/`left_mode` already track
per-16x16-cell LUMA mode; threading `SMOOTH_PRED..=SMOOTH_H_PRED.contains()`
into every `reconstruct`/`reconstruct_rect` call site's `smooth_neighbor`
argument is mechanical) but touches ~9 call sites across 3 planes x 3 partition
shapes (`decode.rs:2161,2174,2186,2279,2317,2354,2928,3038,3050,3061,3337,
3349,3360,7850,7862,7873,8923,8934,8945`), and the CHROMA side of the same
question needs a NEW piece of state this decoder does not track at all: a
per-cell UV-mode neighbour array (today's `Neighbours` only has `above_mode`/
`left_mode` for LUMA). Landing chroma smooth/paeth correctly needs BOTH the
luma-side threading (fixes the observed defect) AND new UV-mode tracking
(so a chroma block's own `smooth_neighbor` reflects its chroma neighbour, not
its luma one) — verified but not measured against a gate this round, out of
budget. Shipping only the luma-side half and leaving chroma's `smooth_neighbor`
hardcoded `false` would silently reintroduce the same bug on the chroma side
the moment two chroma smooth/directional blocks sit adjacent, which is exactly
the kind of silently-wrong decode this lane's own rules forbid landing.

Given the choice between (a) landing a two-line refusal removal with a KNOWN,
now-quantified pixel defect gated by a passing test that happened not to
exercise the bad case, and (b) reverting to the safe refusal with the root
cause fully documented, this round takes (b). The attempted diff (refusal
removal + counter + gate + throwaway diagnostic test) is saved at
`/tmp/claude-1000/-home-tahinli-Documents-Code-Rust-edith-codecs/baaa03f8-c4ff-4469-8ebb-83100429b150/scratchpad/chroma-r1-attempt.diff`
for whichever round lands the `smooth_neighbor` threading.

## Gate firing count

None — the gate (`a_real_aomenc_stream_with_smooth_paeth_chroma_decodes_pixel_exact`)
was written and fired real smooth/paeth chroma symbols on its very first
attempt (seed 42), but FAILED on luma pixel mismatch before reaching a
chroma pixel-exact assertion; it was reverted along with the refusal removal
rather than left in the tree failing or disabled. `smooth_uv_hits` counter:
1 firing observed (chroma `uv_mode=11` on block `px=32 py=16`), never
verified pixel-exact.

## The directional-chroma sibling ("a directional chroma mode (round 2)")

Not touched this round; it lives at a DIFFERENT, unrelated code path
(`decode.rs:7786`, inside the intra-block-within-an-inter-frame reader) than
the two smooth/paeth refusals (`~1970`/`~2458`, the key-frame `read_intra_mode`
variants). That inter-frame intra-block path never computes `angle_delta_uv`
at all and refuses ANY non-DC/non-CFL `uv_mode` outright — i.e. directional
chroma is refused there unconditionally, unlike the key-frame path (`~2458`),
which ALREADY decodes directional chroma correctly today (see the existing,
passing `a_real_aomenc_stream_with_directional_chroma_decodes_pixel_exact`
gate). Landing the `~7786` sibling needs angle-delta-for-chroma support added
to that separate reader, plus its own gate; not attempted this round —
smaller in shape than the smooth/paeth work (no known predictor bug blocking
it, just missing plumbing) but independent scope, deferred to a future round.

## Disposition

deferred: thread `smooth_neighbor` through luma + add chroma UV-mode
neighbour tracking, then re-attempt this refusal removal — unblocks on
someone picking up the saved attempt diff and the ~9 call-site list above.
deferred: the `~7786` "directional chroma mode (round 2)" sibling in the
inter-frame intra-block path — unblocks independently, no known bug, just
unwritten angle-delta-for-chroma plumbing in that reader.
