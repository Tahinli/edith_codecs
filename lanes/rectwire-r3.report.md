# lane-rectwire round 3

VERDICT: reverted r2's real (non-skip) rect coefficient decode to refuse by
name -- the pixel-exact gate found a genuine desync (not a subtle context
bug), and this round's budget was exhausted before isolating its exact
symbol. No desync landed. Free-partition gate green, full lib suite
231/0.

## What was confirmed a defect, and where it isn't

`/tmp/claude-1000/rectwire-flake-1.obu` (seed 55, 64x64x24) reproduces
byte-identically from the pinned bytes across two runs (`scratch_decode_
pinned_stream_once`, `OK: 24 frames` both times) -- this is a real decode
bug, not attempt-selection flake.

`scratch_isolate_pinned_mismatch` on that stream: frame 0 luma has 1085/4096
mismatches. The *first* one (row 28, col 31, delta 1) sits inside the
top-left 32x32 `PARTITION_NONE` intra block -- **not** a rect strip at all
(charter's step-3 question answered: no). But a quadrant breakdown shows
1015 of the 1085 luma mismatches (93.5%) land inside the one rect quadrant
this frame has: `mi(8,8)` reads `PARTITION_HORZ` (`TRACE partition_w32
mi=(8,8) ctx=0 value=1`), splitting the bottom-right 32x32 into two true
32x16 strips. Sub-bucketing that quadrant: strip 0 (rows 32..48) is 503/512
wrong, strip 1 (rows 48..64) is 512/512 wrong, both starting at their very
first pixel (strip 1's first pixel is 105 vs 72 -- a 33-level jump, not a
rounding-scale error). That magnitude and near-total coverage is a real
desync inside the rect coefficient path, not a context-table nudge; the
handful of TL/BL-quadrant mismatches are plausibly a small halo from
whatever downstream state the corrupted BR quadrant leaves (not itself
implicated).

## Three suspects r2 named -- all checked and cleared, cross-referenced
against real libaom source this round, not memory:

1. **`base_ctx_rect`'s 5-neighbour `TwoD` offset array.** Checked against
   `get_nz_map_ctx_from_stats` / the `av1_nz_map_ctx_offset` comment in
   `~/.cache/aom-oracle/src/av1/common/txb_common.h:189-224`: the 5 offsets
   `(1,0),(0,1),(1,1),(2,0),(0,2)`, the `w<h`/`row<2`→`+11` and
   `w>h`/`col<2`→`+16` boundary branches, and the tx-size-independent
   `row+col` fallback (verified `cdf::NZ_MAP_CTX_OFFSET_32` already encodes
   exactly `row+col<2→1, row+col<4→6, else→21`) all match libaom
   byte-for-byte. Not the bug.
2. **`u_skip_ctx`/`v_skip_ctx`'s `above + left` formula.** libaom's real
   chroma rule (`get_txb_ctx_general`, same file, plane != 0 branch) is
   `get_entropy_context(tx_size,a,l) + ctx_offset` where `ctx_offset` is 7
   for a lone TU (`plane_bsize == tx bsize`, our case) or 10 otherwise, and
   `get_entropy_context` returns exactly `(above coded?)+(left coded?)` --
   i.e. the *same* 0..2 range our code computes. Cross-checked against the
   **existing, tested** square `Chroma16` path (`read_plane`,
   `decode.rs:2707`): it uses the identical `usize::from(around.0) +
   usize::from(around.1)` formula with no `+7`, meaning the `txb_skip_
   chroma_16` default-CDF table was already transcribed with the `+7`
   pre-baked into which 3 rows were picked -- consistent, not the bug.
3. **`dc_sign_ctx` reading the wrong `around` index.** `decode_block_rect`
   indexes `around[0]` for luma, `around[1]` for u, `around[2]` for v --
   same convention the square path's `chroma_around[1]`/`[2]` uses. Not the
   bug.

Also checked `around_rect`/`record_rect`'s luma-mi-granularity span for
chroma (looked like a subsampling bug at first glance): both the write side
(`record_mi_rect`) and read side (`around_mi_rect`) use the same un-halved
`w_mi`/`h_mi` for all three planes, matching the square path's own
`around`/`record` convention exactly. Not a bug either.

## Where the range-ladder bisection stalled

Built the requested instrument check first: `~/.cache/aom-oracle/build/
aomdec` with `EC_TRACE_COEFF=1`/`EC_TRACE_MODE=1` emits real per-block
`rng` -- confirmed live (`EC_PART`/`EC_COEFF`/`EC_MODE` all firing on this
stream). But the intra key-frame path in libaom's `decodeframe.c` has no
traced symbol between the partition read and the first `EC_COEFF` (mode/
skip/tx_depth reads for an *intra* block are only traced by the new
`EC_TRACE_MODE` patch on the *inter* decodemv.c path, not here), so
`aomdec`'s post-partition `rng` and our own decoder's `rng` at coefficient
entry aren't directly comparable -- several untraced symbol reads (skip,
y_mode, uv_mode, tx_size_cat2 in `read_intra_mode_rect`) sit between them on
our side with no anchor on aomdec's. Extending `EC_TRACE_MODE`-style tracing
into libaom's intra mode-info reader (a second oracle patch) is the next
lane's first move, not attempted this round (budget).

## The revert

`decode.rs::decode_block_rect`'s non-skip branch now returns `Err(unsupported(
"a HORZ/VERT intra strip with real (non-skip) coefficients ..."))` before
any of r2's real-decode code runs (that code stays in the file, dead behind
the early return, so the next round doesn't re-derive it). The free-partition
gate's `RECT_COEFF_HITS` hard assert (r2) is downgraded back to a SOFT-NOTE
(`rect_coeff_hits` is now always 0 by construction) -- matches r1's original
posture.

## Before/after refusal + firing counts

This round, one gate run (`EC_AV1_REQUIRE_AOMENC=1`, n=40, seed 42+i):
- 29 named refusals, 11 pixel-exact matches, 0 mismatches (gate green).
- `rect_partition_hits=9` (real HORZ/VERT strips reached), `rect_coeff_hits=0`
  (by construction now), `extended_partition_hits=0`, `partab_hits=2`.
- 3 of the 29 refusals are the new named one: "a HORZ/VERT intra strip with
  real (non-skip) coefficients (lane-rectwire r3: confirmed desync, not yet
  isolated)" (seeds 63, 67, 81) -- these are exactly the runs that would
  previously have risked the r2 desync; they refuse cleanly now instead.

Full `cargo test -p ec-av1 --release --lib`: 231 passed, 0 failed, 17 ignored.

## Next round's first move

Patch `~/.cache/aom-oracle`'s `decodeframe.c` intra mode-info reader
(`read_intra_frame_mode_info` or equivalent) with the same `rng`-emitting
pattern `EC_TRACE_MODE` already added for inter, so a same-length range
ladder can walk from the `mi(8,8)` `PARTITION_HORZ` read through skip/
y_mode/uv_mode/tx_depth into the first `EC_COEFF` and find the exact
diverging symbol, rather than guessing between the coefficient-context
math (now cleared) and the prediction/transform math (untested this round).
