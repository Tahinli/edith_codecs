# lane-tiles r2 — actually decode the tiles

Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-tiles`, branch
`lane-tiles`, at 23f412f.

## Read first, then write code
- `lanes/tiles.report.md` — r1's report. Its "next lever" section names exact
  line ranges and the staging order. Do NOT redo r1's recon (class
  `worker-cap-spent-reading`).
- `lanes/tiles.charter.md` — the original charter: the three hard parts
  (entropy reset per tile, tile-clipped neighbour availability,
  `context_update_tile_id`), the gate rules, the hard rules. Still binding.

## What r1 landed (1231cd5, suite green 232/0, currently a no-op)
`Neighbours` gained `tile_row0_mi` / `tile_col0_mi` and `start_tile()`, and every
literal-`0` availability check — `skip_txfm_ctx`, `tx_size_context`,
`tx_size_context_rect`, and the has_above / has_left in `decode_inter_block` and
`decode_inter_block8` — now compares against the tile origin instead of 0.
Nothing calls `start_tile()` yet, so the origin stays 0 and behaviour is
unchanged.

## Still missing (r1's list, verbatim)
- entropy-per-tile restructuring in `decode_key_frame_tile_with_cdfs` /
  `decode_inter_frame_tile_with_cdfs`
- `PlaneBuf` tile-pixel-origin clipping — not started
- `mvstack.rs` neighbour scan still frame-relative
- multi-OBU tile-group collection — not started
- the refusal at `stream.rs:294-296` is unchanged and still fires

## Order — COMMIT AFTER EVERY GREEN STEP
1. Restructure `decode_key_frame_tile_with_cdfs` to loop tiles: fresh msac over
   each tile's byte range, fresh copy of the frame's initial CDFs per tile, and
   only `context_update_tile_id`'s end-of-tile CDFs kept. Wire `start_tile()`
   and the new `PlaneBuf` tile-pixel-origin fields. Prove it on an intra-only
   two-tile column split, pixel-exact. COMMIT.
2. Tile rows, then non-uniform spacing. COMMIT.
3. Inter frames — thread the tile origin into `mvstack.rs`'s neighbour scan.
   This is where it will break. COMMIT.
4. Several tile-group OBUs per frame. COMMIT.
5. `context_update_tile_id` proven with a stream where it is not tile 0, the
   FOLLOWING frame decoding pixel-exact off those inherited CDFs. COMMIT.

## Gate (mandatory)
Same rules as r1's charter: `EC_AV1_REQUIRE_AOMENC=1` on every run, `-t <seconds>`
on every ffmpeg generate, fixtures through `gradients_source(seed, w, h, tail)`,
aomenc `--threads=1 --row-mt=0` plus tile-columns/tile-rows log2, a frame large
enough to be splittable, and a HARD-asserted thread-local `Cell<usize>` counter
proving more than one tile was decoded.

## Hard rules
- `export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-tiles`. Foreground builds,
  `nice -n 19 cargo ... -j4`; give every `cargo test` a timeout of at least
  600000 ms (the suite runs ~3-4 min; a 120 s default kills it mid-run).
- Sibling worktrees (edith_codecs, -chroma, -realworld, -lr, -superres) have
  live agents. Never build in or edit them.
- Baseline 232 passed / 0 failed on this tree; main is at 234 with two new
  guard tests you inherit at merge.
- NEVER push, never merge, never touch `main`. Commit on `lane-tiles` only.
- 75-turn cap: commit at every green step. Stage 1 alone committed and green is
  a good outcome; a perfect uncommitted tree is not.
- End with `lanes/tiles-r2.report.md`, VERDICT on the first line.
