# lane-palette2 r8 — verifier findings closed; both gates now RED on a real defect they used to hide

VERDICT: RED (honest). The two gate-integrity fixes the verifier asked for turned 2 previously
"green" gates red: 16/70 attempts per gate decoded successfully and were never pixel-compared,
and one of them (`rgbtestsrc=size=128x128:rate=25 cq=55`) mismatches ffmpeg on chroma. That
mismatch is PRE-EXISTING, not caused by this round's decoder change (proved by stashing the
decode.rs diff and re-running: same panic, same attempt, same plane).

## What changed
- `crates/ec-av1/src/stream.rs` ~2170-2210 (`a_real_aomenc_stream_with_rect_palette_decodes_pixel_exact`)
  and ~2335-2375 (`a_real_aomenc_stream_with_rect_screen_content_decodes_pixel_exact`) — the
  hit-counter check no longer `continue`s past the pixel comparison. Every successfully decoded
  attempt is compared and a mismatch panics; the counter now only decides whether the attempt
  counts toward the hit-asserting total (`matched` vs new `uncounted_exact`). Class
  [[gate-skips-on-its-own-failure]].
- `crates/ec-av1/src/decode.rs` ~1986-2010 / ~2084-2105 (`record_palette_y_rect`,
  `record_palette_uv_rect`) — `for cell in 0..(w / SUB).max(1)` (both axes). The plain
  `w / SUB` loop was a NO-OP for every sub-16px span, so a sub-16 leaf could not clear the
  above/left palette state at all. Corner-cut comment names the ceiling: our grid is 16px
  where libaom keeps `palette_size` per 4px mi cell; safe today because every sub-16 leaf
  path refuses palette outright, so it can only ever stamp `size == 0`. Upgrade path:
  widen the palette arrays to mi granularity.
- `crates/ec-av1/src/decode.rs` ~3803-3810 (`decode_leaf_rect`, before `Ok(mode)`) and
  ~5703/~5721 (`decode_leaf8`, both exits) — each sub-16 leaf now records its own palette
  state (size 0, since both paths refuse palette) over its own cell, so the next block's
  `palette_y_mode` ctx and colour cache no longer read a previous block's palette. Class
  [[cdf-row-held-constant]].
- `crates/ec-av1/src/decode.rs` (tests) — new unit test
  `a_sub16_leaf_after_a_palette_block_yields_palette_ctx_zero`: a 16x16 palette block gives
  ctx 2 + cache [7,9]; an 8x8 non-palette leaf in the same cell must take it to ctx 0 and an
  empty cache (Y and UV halves). Fails before this round's fix, passes after.
- `lanes/palette2-r7.report.md` line 50 — CORRECTED. r7 claimed the aomenc line carried
  `--tune-content=screen --enable-rect-partitions=1`; the code passes NEITHER. Real argv:
  `--codec=av1 --passes=1 --end-usage=q --cq-level=<cq> --cpu-used=4 --threads=1 --row-mt=0
  --sb-size=64 --enable-palette=1 --min-partition-size=16 --max-partition-size=64 --obu -o - -`.
  The recipe itself is unchanged.

## Gate results (EC_AV1_REQUIRE_AOMENC=1)
```
cd <worktree>; export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette2 EC_AV1_REQUIRE_AOMENC=1
cargo test -p ec-av1 --lib -j3 -- --test-threads=1 --nocapture \
  a_real_aomenc_stream_with_rect_palette_decodes_pixel_exact \
  a_real_aomenc_stream_with_rect_screen_content_decodes_pixel_exact
```
Buckets over the same 70-attempt sweep (5 sizes x 7 cq x smptebars/rgbtestsrc), per gate:
- named refusal: 54/70 (32x32 partition value=4..7, filter-intra-on-strip, split-transform strip)
- decoded, counter fired, pixel-compared: was 14/70 in r7 — not re-reached this round, the run
  aborts at the first mismatch, which is an uncounted attempt earlier in the sweep.
- decoded, counter did NOT fire, now pixel-compared (r7 skipped these entirely): 2 reached
  before the abort — `rgbtestsrc 128x64 cq=63` (exact) and `rgbtestsrc 128x128 cq=55` (MISMATCH).
`refusal_inventory` (3 tests) + `gate_coverage` (2 tests): 5 passed, 0 failed.

## The defect the integrity fix exposed
`rgbtestsrc=size=128x128:rate=25 cq=55`, single key frame: luma bit-exact, V exact, **U plane
30 pixels wrong by +-1 / +-2**, all inside chroma rows 38-50 x cols 28-50 (luma 76-100 x 56-100).
Shape matches the open residual chroma-only defect already noted on main (`1a89ed9` "residual
chroma-only defect open") and memory `[[lane-tiny r4]]`: chroma-only +-1 with luma exact is a
post-recon (filter) defect — dump prefilt/postdeblock stages first, do NOT open a symbol trace.
Pinned stream (118 bytes, sha256
`807e5d434b035e626548d0ec3dc166b959622ed64b59dfa049a16d0e5a35c297`):
`/tmp/claude-1000/-home-tahinli-Documents-Code-Rust-edith-codecs/b6d8a07f-96a4-4bbb-b378-af9ae25cf7c9/scratchpad/palette2-r8-mismatch.obu`
(not committed: `crates/ec-av1/fixtures` is under `.gitignore:2 fixtures`; regenerate with
`EC_AV1_GATE_DUMP=<path>` on either gate).

## EVIDENCE
EVIDENCE: /tmp/claude-1000/-home-tahinli-Documents-Code-Rust-edith-codecs/b6d8a07f-96a4-4bbb-b378-af9ae25cf7c9/scratchpad/palette2-r8-mismatch.obu | both palette gates re-run under EC_AV1_REQUIRE_AOMENC=1 with the counter-`continue` removed | 2 previously-unchecked decoded attempts now compared; 1 exact, 1 mismatching (U plane, 30 px, max |d| = 2)
EVIDENCE: stash-and-rerun of the decode.rs diff (`git stash push crates/ec-av1/src/decode.rs`, gate re-run, `git stash pop`) | same gate, r8 stream.rs only | identical panic at stream.rs:2200, same attempt, same plane -- the chroma defect is pre-existing, not this round's regression
EVIDENCE: cargo test -p ec-av1 --lib -j3 (EC_AV1_REQUIRE_AOMENC=1) | whole ec-av1 lib | 268 passed, 2 failed (the two palette gates above), 23 ignored, 514.71s
EVIDENCE: cargo test -p ec-av1 --lib -j3 a_sub16_leaf_after_a_palette_block | new unit test | 1 passed

## Residue
- fix-now(next round, blocks merge): the chroma-only U mismatch above. Both gates stay RED until
  it is fixed or its stream is proven to hit a legal named refusal instead.
- deferred(concurrent lanes own these gates): 8 sibling sites in stream.rs carry the SAME shape --
  a successful decode whose hit counter did not move `continue`s/`never_fired += 1` before any
  pixel compare: lines ~2662 (`tx_depth_hits`), ~3781 (`non_last_ref_hits`), ~3959
  (`ref_hits(target_ref)`), ~4159 (`comp_mode_hits`), ~4354 (`skip_mode_hits`), ~4530
  (`obmc_hits`), ~7222 (`tmv_hits`), ~7495 (`tx_class1_hits`). Same one-line transform applies
  (compare pixels, then decide whether to count). NOT changed here: they belong to lanes running
  concurrently in this batch on the same file (merge-conflict cost), and each needs its own
  aomenc sweep to validate. Unblocked by: this batch's merge, then one mechanical sweep round.
- accepted: the 16px palette-neighbour grid (corner-cut comment in `record_palette_y_rect`).
