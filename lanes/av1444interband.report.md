# lane-av1-444interband: the inter 1:4 strip's 4:4:4 chroma band stamping

## Ticket

Fix the 4:4:4 lossless 16x4 inter strip band stamping (t6 next-site from
av1sub8rect444's measurement: post-fix desync at the 16x4 INTER strip
mi(2,8), U unit (2,8) — ours `txb_skip` ctx 3 vs oracle ctx 11 with raw
cells `above=[0,] left=[23,]`), or stop with the site narrowed further.
Worktree at f41d649b, branch lane-av1-444interband.

## Named site, re-measured (the report's attribution needed one correction)

Fixture `/tmp/llkf/t6.obu`, sha256
`1e6dd4e05038c8d51e2c8d29f8c337130c4fea0aed629415535e6e53af9a2c72`,
32487 bytes (scratch, regenerable per av1llkf.report.md's recipe;
testsrc2 128x96, 1 key + 5 inter frames, `--profile=1 --lossless=1
--enable-palette=0 --enable-intrabc=0`, default partition, sb128).
Oracle: the shared patched `$HOME/.cache/aom-oracle/build`, fresh
`aomdec --rawvideo` on the same bytes.

- Pre-change at f41d649b, our decoder refuses mid-frame-1
  (`a Golomb tail longer than this decoder reads`) — fail-before,
  measured before the edit on this tree.
- The oracle's cell dump (`EC_ECDUMP`) walks `left[2][1]` (U plane, mi
  row 2) across frame 1: the last writer before the failing read is the
  16x16-INTRA block (0,4)'s U unit bc=3/br=2 (event #2379), which
  stamps cul_level 23 (mag 7, dc set); nothing else touches the cell
  until the (2,8) strip's U bc=0 reads it as 23. The skipped inter
  strips (0,8)/(1,8) log no coefficients and their
  `av1_reset_entropy_context` (decodeframe.c:1180) zero only their OWN
  cells — row 2's chroma left cell is not theirs to clear.
- Correction to the inherited attribution: our ctx 3 on the failing
  read is a `TxbSet::Chroma4` TABLE ROW (base 0 in the +3 offset-10
  layout), not a luma `skip_contexts` value — the read is the strip's
  U bc=0 (`side=4` = the 4x4 chroma tables), so the report's "U unit
  (2,8), ctx 3 vs 11" stands; the detour through "luma bc=3" was a
  rung-semantics misread (our `EC_ECDUMP_IN` logs the raw `skip_ctx`
  argument, which for chroma callers is the pre-offset base/row).

## Root cause

`decode_inter_block`'s strip tail ran lane-inter16ab r2's `pair_chroma`
rewrite unconditionally. The rewrite stamps the PAIR-form chroma span —
16x8 (HORZ) / 8x16 (VERT) at `pair_mi`, planes 1/2 — which is libaom's
single `av1_set_entropy_contexts` call for the 4:2:0 pair's 8x4 (4x8)
chroma unit. At ss 0/0 there is no pair: `pair_mi` == the strip's own
mi, every strip is its own chroma reference, and the strip's own
`record_rect_mi` (zero grids on the skip arm) already stamps exactly
its own 16x4 (4x16) chroma span — the `av1_set_entropy_contexts` /
`av1_reset_entropy_context` behaviour. The rewrite therefore smeared
the strip's state one extra row (col) too far: skipped strip (1,8)
wrote zero over `left[2][1..2]`, erasing the (0,4) intra block's U/V
stamps, and (2,8)'s U walk then read base 0 where aom reads base 1.
Counter evidence: `INTER16_CHROMA_PAIR_HITS` fired 31x on this
pairless 4:4:4 stream (its documented meaning is "odd strips that
close a 4:2:0 chroma pair").

## Change (decode.rs, one filter + one trace rung)

1. `pair_chroma` now carries `.filter(|_| ss_x(fctx) == 1 &&
   ss_y(fctx) == 1)`: the pair-span rewrite exists only at 4:2:0, where
   it is byte-preserved (the added condition is false-only-at-ss-0).
2. `record_mi_chroma` gained the chroma twin of the existing
   `EC_ECPUB` rung: `EC_CHROMA_PUB plane= mi=() wh=() lvl= dc=`,
   env-gated eprintln (the instrument that localized this site; kept
   for the next band hunt).

Sibling ownership clean: the edit is in `decode_inter_block`'s tail
(lane-inter16ab's rewrite region), not `sub8_leaf_chroma444` nor
`decode_intra_sub8_leaf`.

## Verification (measured, this tree @ d5325172)

- Entropy/pixel witness, t6.obu vs fresh `aomdec --rawvideo`
  ($HOME/.cache/aom-oracle/build): all 6 frames decode (pre-change the
  decode refused mid-frame-1) and frames 0..5 are **byte-exact, 0
  differing bytes** per frame (36864 B/frame compared). Frame exactness
  is claimed on exactly that basis.
- Non-vacuity: `inter16_1to4` counters on the full t6 decode:
  `horz4=20 vert4=28 chroma_pairs=0` (was `chroma_pairs=31` while the
  strips decoded); the strips themselves still decode and the frames
  above prove it.
- Gates: `cargo test -p ec-av1 --lib a_lossless` → 7 passed, 0 failed
  (both 4:2:0 lossless libaom gates, the 444 min-partition-64 and
  min-partition-8 inter gates, the sb128 pair, the 16x4 pair). The
  4:2:0 inter 16x4 aomenc sweep
  (`a_real_aomenc_inter_sequence_with_intra_16x4_strips_...`) is
  yuv420p-only and the change is no-op at 4:2:0 by construction (the
  filter's condition is satisfied there); not re-run this round.
- `cargo check -p ec-av1 --all-targets` (target dir
  `$HOME/.cache/cargo-target-av1444interband`, ec-av1 cleaned first):
  0 warnings, 0 errors. `rustfmt --check` reports no hunks inside the
  edited ranges (the crate's pre-existing let-chains drift is
  untouched).
- Commit d5325172 on lane-av1-444interband. Not pushed.

### Addendum (review P2): the t6 pin is now an in-tree gate

Main's review follow-up: commit
`ll444_defaultp_inter_strip.obu` (force-added fixture, 32487 bytes,
sha256 `1e6dd4e05038c8d51e2c8d29f8c337130c4fea0aed629415535e6e53af9a2c72`,
FNV1a64 `52789f0fce2d1e52` per the crate's pin convention) and gate
`a_lossless_444_defaultp_inter_strip_stream_decodes_byte_exact`
(stream.rs, av1llpredgate2's pinned-fixture pattern): len + FNV pin,
the 1:4 counter delta pinned to `horz4=20 vert4=28 chroma_pairs=0`,
and the env-conditional aomdec arm requiring all 6 frames sample-exact.
Mutation-proven load-bearing: with the ss-0 filter reverted the gate
fails at the pre-fix refusal; restored, it is green (8 passed in the
a_lossless battery).

## Named, not chased (spoken)

1. `Neighbour::level` clamps `cul_level` to 7 while aom stores up to 15
   (`COEFF_CONTEXT_MASK` = 15, dc sign in bit 4). Invisible to every
   context derived from the cells (the `txb_skip` categories only
   distinguish {0} / {1..3} / {>=4}, and dc sign rides its own field),
   so it is representation drift, not a defect — noted so nobody
   "fixes" the stored value into a desync.
2. Oracle-side f2 cross-reference: lane-av1lrflush's 444sb witness f2
   diverges at PREFILT entering at column 128 (inter 64x128 at
   (128,0)), plausibly this same skipped-inter-strip class one size up
   (their filter-pipeline/LR exoneration is consistent with our
   entropy-domain finding). Main owns the 444sb gate extension
   (take(2) -> take(3)) once this lands. deferred(that check, unblock:
   their f2 witness bytes against this tree).
3. Whole-stream read-pairing via `EC_ECDUMP_IN`/`EC_ECDUMP` (dif,rng)
   triples is NOT a reliable event alignment across the two
   implementations: the internal daala-EC representations legitimately
   diverge while decoding identically (frame 0 is pixel-exact yet its
   pre-read triples fail 1:1 pairing). The robust comparators are
   pixels, refusal points, and ctx/cell values. Recorded so the next
   band lane doesn't burn a round on the same lesson.
