VERDICT: PARTIAL -- r5's decode.rs work verified green, gate residue localized to a specific
deblock edge, root cause not yet found (defer to r7).

## Step 1: verify r5's uncommitted work

`cargo check` clean, `cargo test -p ec-av1` (EC_AV1_REQUIRE_AOMENC=1, 600s timeout): 237 passed,
1 failed (the known `a_real_aomenc_superres_key_frame_sequence_decodes_pixel_exact`, the gate
this lane exists to close), 17 ignored. Nothing else regressed. No fix was needed -- working
tree was clean at a69e1bd/3462632, so no commit was made for step 1 itself.

## Step 2: use the rungs

Reproduced the gate's exact failing stream (added `EC_SUPERRES_STREAM_DUMP` to the test, one
run, one seed -- avoids the seeded-fixture-not-reproducible dead-end since it's now a captured
file, not a re-rendered lavfi seed) and ran both oracle rungs plus our own matching decode.rs
dumps (`EC_AV1_PREFILT_WIDE_DUMP` exists on both sides already; `EC_AV1_MARGIN_DUMP` is our own
post-deblock+cdef dump, CDEF is off in this stream so it's a valid post-deblock comparison
point) against frame 2 (the failing frame):

- **Pre-deblock (`ours-prefilt.f2` vs `lib-prefilt.f2`): 0 diffs, byte-exact.** Reconstruction
  (predict+residual, all of coefficient decode) is proven correct for this frame including the
  whole margin region -- rules out mode/tx/coefficient decode as the cause.
- **Post-deblock (`ours-margin.f2` Y vs `lib-postdeblock.f2` Y): 37 diffs, ALL in columns 44-47**
  (the coded frame's rightmost mi column, mi_c=11 of mi_cols=12, true_width=48) across rows
  9-38ish. Nowhere else in the frame differs.

This localizes the bug to `deblock_plane`'s horizontal-edge pass at the rightmost mi column.
Traced the specific edges firing there (`EC_AV1_DEBLOCK_TRACE=1`): three horizontal edges per
frame at x0=44, y0∈{16,32,48}, `len=14 level=5 sharpness=0`. Hand-reimplemented `filter14`'s
flat-branch formula in Python against the bit-exact pre-deblock input at columns 44-47: it
**exactly reproduces our own Rust decoder's output** (op2..oq2 match `ours-margin.f2` to the
byte) -- our filter math is spec-correct and self-consistent. But **libaom's real post-deblock
output at these exact samples equals the unfiltered pre-deblock input** -- libaom did not filter
this edge at all, at 3 of the 4 columns in the group (col 44 happened to be a no-op already).

## What's not yet found

Read `av1_loopfilter.c`'s two candidate skip conditions (`curr_skipped = skip_txfm &&
is_inter_block`) -- both are INTER-only gates; this is a key frame (`--kf-max-dist=0`), so intra
blocks never hit that suppression, and by the source's own logic this edge (nonzero level on an
intra PU boundary, `pu_edge = mi_prev != mbmi` true) should still filter. To get ground truth
instead of re-deriving the C by eye, patched two candidate real call sites
(`set_lpf_parameters` and `set_one_param_for_line_luma`, in
`~/.cache/aom-oracle/src/av1/common/av1_loopfilter.c`, rebuilt via `ninja -C
~/.cache/aom-oracle/build aomdec`) with `EC_LF_TRACE`/`EC_LF_TRACE2` eprintln diagnostics gated
on `mi_col==11` and `!is_vert` -- **neither ever fired** for this stream, meaning the real
horizontal-edge loop-filter path for this exact column takes a THIRD, unlocated code path in
libaom (a SIMD/quad-batch variant most likely, or a per-line-luma variant reached from a
different fast path than the two I found via `grep set_lpf_parameters`). That third path is
where the actual "why libaom doesn't filter here" answer lives -- I ran out of turn budget
finding it. The libaom source patches are throwaway (in `~/.cache/aom-oracle/src`, not part of
this repo, not committed) and can be re-applied/removed freely by r7; `scripts/build-aom-oracle.sh`
+ `scripts/instrument-aom-oracle.sh` rebuild from clean if you want a pristine tree first.

## Left in the repo (committed, `8ef70ff`)

- `EC_SUPERRES_STREAM_DUMP` (stream.rs, the superres key-frame gate test): dumps the exact OBU
  bytes fed to aomenc/decode_stream to a file when set -- makes the gate's stream reproducible
  for oracle diffing without relying on lavfi seed determinism.
- `EC_AV1_DEBLOCK_TRACE` (decode.rs, `deblock_plane`'s horizontal-edge loop): eprintln's
  `x0/y0/len/level/sharpness` when `x0==44` and `plane_idx==0` -- narrow and hardcoded to this
  investigation; r7 should widen or remove it once the real cause is found.

Both are env-gated, off by default, zero behavior change to any existing test.

## Step 3: inter-frame superres (spec 7.11.3.3)

Not started -- all turn budget went to step 2's localization. Deferred to r7/next lane.

## Refusal strings

None added, renamed, or removed this round.

## Merge

Not attempted -- no time budget left to verify a `git merge main` cleanly after the above; left
for r7 to decide, per charter's "consider `git merge main` here first" being conditional on
budget.

## Disposition

- deferred: the actual libaom code path for the last-column horizontal edge -- unblocks with
  either `perf`/`gdb break set_lpf_parameters_for_line_luma` on the real aomdec binary while it
  decodes this exact stream (fastest: confirms which function body genuinely executes), or
  grep `av1_filter_block_plane_horz`/AVX2 variants under `av1/common/x86/av1_loopfilter_sse2.c`
  for a scalar-bypassing fast path.
- deferred: stage 4 (inter-frame superres, scaled-reference MC) -- unstarted, whole stage.
