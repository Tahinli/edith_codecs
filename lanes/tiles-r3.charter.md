# lane-tiles r3 — verify r2's per-tile loop, then finish it

Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-tiles`, branch
`lane-tiles`, at d1c2071.

## State
- `1231cd5` (r1) — tile-origin groundwork in `Neighbours` (`tile_row0_mi`,
  `tile_col0_mi`, `start_tile()`), every literal-`0` availability check keyed on
  the tile origin. A no-op until something calls `start_tile`.
- `d1c2071` (r2, cap-rescued by the orchestrator) — `decode.rs` +159,
  `stream.rs` +3, staged but never committed by r2 and **never seen to compile
  or pass anything**. That is your first job: find out what it is.
- `lanes/tiles-r2.charter.md` and `lanes/tiles.charter.md` still govern the
  staging, the three hard parts and the gate rules. `lanes/tiles.report.md` has
  r1's exact line ranges.

## Order
1. `export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-tiles`, then
   `cargo check -p ec-av1 --lib` and, if it compiles,
   `EC_AV1_REQUIRE_AOMENC=1 nice -n 19 cargo test -p ec-av1 --lib -j4`
   (timeout >= 600000 ms). Report the count against 232/0 for this tree. Getting
   d1c2071 green is the whole job until it is green. COMMIT once green.
2. Stage 1 from the r2 charter: the key-frame tile decode as a per-tile loop —
   fresh msac over each tile's byte range, a fresh copy of the frame's initial
   CDFs per tile, only `context_update_tile_id`'s end-of-tile CDFs kept, and
   `start_tile()` wired along with the `PlaneBuf` tile-pixel-origin fields.
   Proven on an intra-only two-tile column split, pixel-exact, with a
   hard-asserted thread-local count of tiles decoded `> 1`. COMMIT.
3. Then tile rows and non-uniform spacing; then inter frames (thread the tile
   origin into `mvstack.rs`'s neighbour scan — this is where it will break);
   then several tile-group OBUs per frame; then `context_update_tile_id` proven
   with a stream where it is not tile 0. One commit each.

## Note for the merge
Main carries two guards you do not have: `gate_coverage.rs` (pins the aomenc
tools no gate exercises) and `refusal_inventory.rs` (pins every decode-path
refusal string, so adding, renaming or removing one — including the multi-tile
refusal you are here to delete — fails until the list is updated). Report the
refusal strings you change, verbatim.

## Hard rules
Foreground builds `nice -n 19 cargo ... -j4`, own `CARGO_TARGET_DIR` as above,
every `cargo test` a timeout >= 600000 ms. Sibling worktrees (edith_codecs,
-chroma, -realworld, -lr, -superres, -palette) have live agents — never build in
or edit them. Never push, never merge, never touch `main`. 75-turn cap, does not
reset: COMMIT AT EVERY GREEN STEP — both rounds on this lane so far ended with
work uncommitted. Stage 1 committed and green is a good round on its own. End
with `lanes/tiles-r3.report.md`, VERDICT on line 1.
