VERDICT: DONE -- UV palette reconstruction green and committed (0b79412)

## What happened

r1's uncommitted WIP (committed verbatim at d2ea8e7, "unverified") was much
further along than the charter's own description suggested: `read_palette_colors_uv`,
`PaletteUv` reconstruction into both chroma planes (`palette_uv_bufs` ->
`PALETTE_PRED` for `u.reconstruct`/`v.reconstruct`, mirroring the palette-Y
path), and `record_palette_uv` neighbour tracking were all already written
and, on inspection, correct. The tree did not compile for one reason: the
8x8-leaf call site of `read_intra_mode` (crates/ec-av1/src/decode.rs:4394)
was still destructuring the pre-UV 8-tuple and passing the pre-UV 13-arg
call, one edit behind the widened `read_intra_mode` signature (9-tuple
return, 14-arg call with the new `palette_uv_cache: &[u16]` parameter) that
the rest of the file had already moved to.

Fixed by adding `_palette_uv` to the destructured tuple and `&[]` for
`palette_uv_cache` at that one call site -- the same "read+refuse the
symbol, never reconstruct" corner-cut that site already used for palette_y
(`None` for the `palette` param), extended to UV. That site is the excluded
8x8-leaf path (`prev_leaf`/rect handling), not the intra square-block path
this round's gate exercises, so no new refusal or behaviour needed there.

## Gate

`a_real_aomenc_stream_with_palette_uv_decodes_pixel_exact`
(crates/ec-av1/src/stream.rs, new): `testsrc2` (the multicoloured AV1 test
pattern -- flat, few-colour, repetitive chroma per region, unlike
`smptebars=hue=s=0` in the sibling palette-Y gate, which deliberately
flattened chroma to sidestep this exact case) through the same square-only
recipe as the palette-Y gate (`--enable-rect-partitions=0`/`-ab-`/`-1to4-`,
fixed 32x32 partition, `--enable-palette=1`, `--sb-size=64 --threads=1
--row-mt=0`). Hard-asserts `decode::palette_uv_hits()` moved before
comparing pixel output, per [[gate-blind-to-feature]] -- a pixel match on a
stream that never used a UV palette would prove nothing. Live run: 2 UV
palette blocks fire, both frames' Y/U/V match ffmpeg's independent decode
of the same bytes exactly.

## refusal_inventory.rs

No edit needed. "a block that actually uses a palette (UV) -- reconstruction
is out of scope" is still genuinely present in decode.rs at two other call
sites -- the HORZ/VERT rect-strip path (decode.rs ~9263) and the inter-frame
8x8-leaf path (decode.rs ~10359) -- both out of this round's scope (charter
steps 2/3, plus inter-frame palette which the charter never named). Those
stay refused; the inventory's scan is string-presence-based, so it already
passes with no change. Confirmed: `refusal_inventory::tests::the_decode_path_refuses_exactly_the_listed_cases`
and `capability_claims_are_declared_not_scattered` both green.

## Checks run

- `cargo check -p ec-av1` -- clean (was 2 errors before the fix).
- `cargo test -p ec-av1 --lib` (EC_AV1_REQUIRE_AOMENC=1) -- 263 passed, 18
  ignored, 0 failed, in 133.5s. Includes the new UV palette gate, the
  existing palette-Y gate, refusal_inventory, and gate_coverage, all green.
- Command: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette2
  EC_AV1_REQUIRE_AOMENC=1 nice -n 19 cargo test -p ec-av1 --lib -j4`

## Scope discipline

Did not touch steps 2 (HORZ/VERT rect-strip palette) or 3 (split-transform
palette) -- both stay refused, per charter instruction to wait until UV is
green and committed first. Turn budget: finished well under the ~55-turn
soft cap (compile fix + one gate + verification), so no further step was
started this round per charter ("do not start them until UV is green and
committed" -- deferring to the next round rather than scope-creeping into
step 2 with remaining budget).

## Oracle / sibling worktrees

Used the shared `~/.cache/aom-oracle/build/aomenc` read-only (rung 12 was
not needed -- no change to `scripts/instrument-aom-oracle.sh`). No edits
outside this worktree. Nothing pushed, nothing merged into main.
