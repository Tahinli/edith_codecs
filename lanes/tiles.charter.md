# lane-tiles — multi-tile decode

Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-tiles`, branch
`lane-tiles`, off main (48a6e29 or later).

## Goal
Remove the refusal at `crates/ec-av1/src/stream.rs:293-298`
("a frame with more than one tile (this decoder only ever decodes tile 0)")
by decoding every tile of every tile group, in order, into one picture.

## What is already there
- `crates/ec-av1-syntax/src/frame.rs:115` `TileInfo` already carries everything
  the walk needs: `cols`, `rows`, `cols_log2`, `rows_log2`, `mi_col_starts`
  (`TileCols + 1` entries, 4x4 units), `mi_row_starts`, `context_update_tile_id`,
  `tile_size_bytes`. The uniform/non-uniform spacing derivation is at
  `frame.rs:1292` onward and is already implemented — READ it, do not re-derive.
- The tile-group OBU parse already produces per-tile `offset`/`size`; today
  `stream.rs` takes `tiles.first()` and drops the rest. `Tile::offset` is
  relative to the OBU buffer, not to `data` — see the comment at stream.rs:298.
- The superblock walk lives in `crates/ec-av1/src/decode.rs` (two sites, around
  lines 4547 and 9275: `for sb_r in 0..sb_rows { for sb_c ... }`). Today those
  bounds are the whole frame; per tile they become the tile's own sb range
  derived from `mi_row_starts[t]..mi_row_starts[t+1]`.

## The three things that make tiles more than a loop
1. **Entropy reset per tile.** Each tile starts a FRESH msac decoder over its
   own byte range AND a fresh copy of the frame's initial CDFs (spec 5.11.2
   `decode_tile`: `init_symbol` + `clear_above_context`). Only the tile named by
   `context_update_tile_id` has its END-of-tile adapted CDFs saved as the
   frame's output tables (spec `exit_symbol`) — every other tile's adaptation is
   discarded. This is exactly what the current refusal comment warns about.
2. **Context reset at tile boundaries.** Above-context arrays are cleared at the
   start of each tile ROW of superblocks within the tile; left-context at each
   superblock row. Neighbour availability MUST be clipped to the tile: a block
   at the tile's left edge has NO left neighbour even though a decoded block
   sits there in the picture. Same for above. Grep libaom
   `av1/decoder/decodeframe.c` `av1_tile_init`/`av1_tile_set_col`/
   `av1_tile_set_row` and `av1/common/tile_common.c`; the availability
   predicates are `xd->left_available` / `xd->up_available`, set in
   `set_mi_row_col` (av1/common/blockd.h). Deblocking and CDEF, by contrast,
   run ACROSS tile boundaries unless `loop_filter_across_tiles` says otherwise —
   check the actual v3.13.3 source, AV1 has no such flag in the final spec but
   verify rather than assume.
   Also check the mv-stack / ref-mv scan (`crates/ec-av1/src/mvstack.rs`) and
   the CDEF/deblock neighbour reads for the same tile clipping.
3. **Tile ordering across tile groups.** A frame may carry several tile-group
   OBUs; tiles are numbered `tg_start..tg_end` globally.

## Method
CLASS `compare-range-not-tell`: compare the msac RANGE against the oracle after
each element, never `tell()`. The instrumented oracle is at `~/.cache/aom-oracle`
with env-gated rungs (`EC_TRACE=1` partitions, `EC_TRACE_COEFF=1`,
`EC_TRACE_MODE=1`); `scripts/instrument-aom-oracle.sh` + `scripts/build-aom-oracle.sh`
add and rebuild rungs. If you need a per-tile rung (tile index at each
`decode_tile` entry), add one in the existing shape: env-gated, silent when
unset, idempotent, wrapper-around-impl.
CLASS `equal-range-means-unread`: reference range unchanged where ours moves =
we read a symbol it never wrote; theirs moves and ours does not = we skipped one.

## Staging — COMMIT AFTER EVERY GREEN MILESTONE (75-turn cap; builders have lost whole implementations here)
1. Two tiles, one column split, all-intra key frame. COMMIT.
2. Tile rows as well as columns; non-uniform spacing. COMMIT.
3. Inter frames (mv-stack + neighbour clipping is where this will break). COMMIT.
4. Several tile-group OBUs per frame. COMMIT.
5. `context_update_tile_id` proven: a stream where it is NOT tile 0, decoding
   pixel-exact across the following frame (which inherits those CDFs). COMMIT.

## Gate (mandatory, in `crates/ec-av1/src/stream.rs` beside the existing gates)
- Copy the existing gate shape. Tests must be run with `EC_AV1_REQUIRE_AOMENC=1`
  so a missing oracle FAILS instead of SKIPping.
- Bound every ffmpeg `generate` with `-t <seconds>` (an unbounded source once
  hung a gate for an hour).
- Build fixtures through the existing `gradients_source(seed, w, h, tail)`
  helper — ffmpeg's `gradients` ignores its own seed, so a hand-written
  `gradients=size=` string makes the gate non-reproducible.
- aomenc: `--threads=1 --row-mt=0` plus `--tile-columns=1 --tile-rows=1`
  (log2 values in aomenc) on a frame large enough that the tiles are legal —
  a 64x64 frame cannot be split, so size the fixture accordingly.
- Hard-assert a firing count: a thread-local `Cell<usize>` counter in decode.rs
  matching the existing `*_HITS` (thread-local, NOT atomics), incremented per
  tile decoded, asserted `> 1`. CLASS `gate-blind-to-feature`: a gate that
  cannot prove its feature fired is vacuous.
- Refusals inside the gate are FORBIDDEN once the stage removing them lands.

## Hard rules
- `export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-tiles`. Foreground builds
  only, `nice -n 19 cargo ... -j4`. Never build in or edit another worktree:
  edith_codecs, -chroma, -realworld, -lr, -superres all have agents in them NOW.
- Suite: `nice -n 19 cargo test -p ec-av1 --lib`, baseline 232 passed / 0 failed
  (~67 s). It stays green.
- NEVER push, never merge, never touch `main`. Commit on `lane-tiles` only.
- Refuse-by-name rather than desync; never write a refusal string claiming the
  encoder cannot emit a case unless you proved it (class
  `refusal-claim-disproved-by-its-own-gate`).
- End with `lanes/tiles.report.md`: what landed, gate name + firing count,
  remaining refusal strings verbatim, next lever.
