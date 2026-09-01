# lane-palette2 r9 — GREEN: both palette gates pass after the main merge + one real defect fix

VERDICT: GREEN. Both gates decode-and-pixel-compare every successful attempt, 0 mismatches.

## STEP 1 — merge main f31a2c5 (as chartered)
`git merge f31a2c5` into lane-palette2 (commit `4f30677`). Two conflicts:
- `crates/ec-av1/src/decode.rs` `reconstruct_rect` (~5517): kept this lane's `PALETTE_PRED`
  override branch, and took main's rect filter-intra call signature (`bw, bh` — lane-rectsplit
  widened `predict_filter_intra` to a rectangle). Keeping only one side would have either
  dropped palette prediction on rect strips or failed to compile.
- `crates/ec-av1/src/refusal_inventory.rs` (~51): union of both sides, then the two strings the
  merge made unreachable were deleted (the inventory test named them):
  `"a HORZ/VERT intra strip in a screen-content frame (palette syntax is consumed for square
  blocks only)"` (lifted by this lane) and `"a HORZ/VERT intra strip with a split transform
  (per-unit rect prediction is not ported)"` (lifted by main's lane-rectsplit).

The r8 blocker — the chroma-only U mismatch on `rgbtestsrc 128x128 cq=55` (30 px, max |d| 2,
luma+V exact) — is GONE after the merge, exactly as the charter predicted: main `9b1297a`
(deblocker chroma tx size = block span/2, not luma_tx/2). That attempt now decodes pixel-exact.

## STEP 2 — the merge created a new defect, root-caused and fixed
The merge did NOT leave the gates green: they failed on a *different*, larger mismatch
(`smptebars 192x192 cq=35`), which main alone refuses outright
(`decode_probe` on f31a2c5: "REFUSED: ... a HORZ/VERT intra strip in a screen-content frame"),
so the case only exists in the merged tree.

Measured shape: luma x0..63, y0..127 entirely wrong (8192 px, max |d| 52), the rest of the frame
exact; ours flat 128 there = DC prediction with no edges. `EC_TRACE_MODE_STEP` showed mi(0,0) =
32x16 strip, `mode=0`/`uv_mode=0` (the palette signature) with a split transform.

ROOT CAUSE: `decode_rect_split` (`crates/ec-av1/src/decode.rs:3780`, from main's lane-rectsplit)
knows nothing about palette. The palette wiring at the call site
(`decode_block_rect`, decode.rs ~4113) sets `PALETTE_PRED` *after* the `depth != 0` early return,
so every transform unit of a split-transform palette strip predicted DC. The coefficients are
still read, so the entropy stream stayed in sync and only those pixels were wrong — class
[[parsed-then-discarded]] applied to the *prediction* rather than to a header field.

FIX (decode.rs):
- `RectStripModes` (~3747) gains `palette_y: Option<Vec<u16>>` and
  `palette_uv: Option<(Vec<u16>, Vec<u16>)>` — the block's colour-mapped maps.
- `decode_rect_split` luma TU loop (~3857): before each unit's `read_plane`/`reconstruct`, sets
  `PALETTE_PRED` to that unit's `tx`x`tx` window of the block map. Palette prediction reads no
  edge pixels, so a unit's window on the whole-block map IS its prediction — no ordering subtlety.
- `decode_rect_split` chroma (~3905/~3960): sets `PALETTE_PRED` per plane before each
  `reconstruct_rect` (skip and coded arms); the chroma transform is never split by luma's depth.
- call site (~4130): builds the maps into `RectStripModes` and bumps `PALETTE_SPLIT_TX_HITS`, the
  counter the screen-content gate already HARD-asserts (`split_palette_matched > 0`,
  stream.rs:2655), so this new path is gated, not merely exercised.

## Gate results (EC_AV1_REQUIRE_AOMENC=1, --test-threads=1)
```
cd /home/tahinli/Documents/Code/Rust/edith_codecs-palette2
export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette2 EC_AV1_REQUIRE_AOMENC=1
cargo test -p ec-av1 --lib -j3 -- --test-threads=1 --nocapture \
  a_real_aomenc_stream_with_rect_palette_decodes_pixel_exact \
  a_real_aomenc_stream_with_rect_screen_content_decodes_pixel_exact
```
- `a_real_aomenc_stream_with_rect_palette_decodes_pixel_exact`: 17/70 matched pixel-exact,
  21 decoded-and-pixel-exact but uncounted, 32 named refusals, `palette_rect_hits=92`.
- `a_real_aomenc_stream_with_rect_screen_content_decodes_pixel_exact`: 19/70 matched
  (14 of them through a split-transform palette block), 19 decoded-and-pixel-exact but uncounted,
  32 named refusals.
- 38 successful decodes per gate, ALL pixel-compared (r8's integrity fix stands), 0 mismatches.
- `refusal_inventory` (3) + `gate_coverage` (2): 5 passed, 0 failed.

## EVIDENCE
EVIDENCE: /tmp/claude-1000/-home-tahinli-Documents-Code-Rust-edith-codecs/b6d8a07f-96a4-4bbb-b378-af9ae25cf7c9/scratchpad/r9-mismatch.obu (196 B, sha256 1f9bcc87138c39bbca9a38f95e5db09de0f84afdefeeaab2a7288e22b9c91068) | decode_probe dump vs `ffmpeg -f obu -f rawvideo -pix_fmt yuv420p`, before and after the fix | before: Y 8192 px wrong (x0..63, y0..127, max |d| 52), U 640 px, V 640 px; after: Y exact, U exact, V exact
EVIDENCE: git worktree at f31a2c5 (main), `cargo run -p ec-av1 --example decode_probe -- r9-mismatch.obu` | same stream on unmerged main | "REFUSED: unsupported: AV1 tile (a HORZ/VERT intra strip in a screen-content frame ...)" — the defect is a merge-created combination, not a main regression
EVIDENCE: cargo test -p ec-av1 --lib -j3 (EC_AV1_REQUIRE_AOMENC=1), two palette gates | full 70-attempt sweep each | 2 passed, 0 failed; counters above
EVIDENCE: cargo test -p ec-av1 --lib -j3 (EC_AV1_REQUIRE_AOMENC=1) | whole ec-av1 lib on 4f30677 | 277 passed, 0 failed, 23 ignored, 1748.98s

## Refusals
Lifted this round: none newly lifted by this lane. Two strings LEFT the inventory as an
arithmetic consequence of the merge (each was already lifted with its own gate on one of the two
sides): "a HORZ/VERT intra strip in a screen-content frame ..." (this lane's, gated by the two
palette gates above) and "a HORZ/VERT intra strip with a split transform ..." (main's
lane-rectsplit, gated there). The palette-on-split-tx path added here is gated by the existing
HARD assert `split_palette_matched > 0` (stream.rs:2655), now 14.

## Residue
- deferred(this batch's merge, then one mechanical sweep): the 8 sibling `stream.rs` sites listed
  in r8 that still `continue` past the pixel compare when their hit counter did not move
  (~2662, ~3781, ~3959, ~4159, ~4354, ~4530, ~7222, ~7495). Untouched here for the same reason as
  r8 — concurrent lanes own those gates and each needs its own aomenc sweep.
- accepted: the 16px palette-neighbour grid corner-cut (`record_palette_y_rect`), ceiling named in
  the code; libaom keeps `palette_size` per 4px mi cell.
- accepted: `PALETTE_SPLIT_TX_HITS` is bumped by both the square split path (decode.rs ~6159) and
  the new rect-strip path, so the gate's assert does not distinguish them; the gate's own recipe
  (`--min-partition-size=16`, rect strips present) plus this round's before/after pixel evidence
  covers the rect case.
