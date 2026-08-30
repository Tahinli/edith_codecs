# lane-palette r7 report

VERDICT: DONE -- the fix r6 pinned is implemented, the gate is pixel-exact
against ffmpeg, the palette-Y refusal is removed, `refusal_inventory.rs` is
updated, main is merged in, and the lane arrives mergeable.

## The fix (`crates/ec-av1/src/decode.rs`, `read_intra_mode`)

r6's range trace pinned the cause exactly: `decode_color_index_map` was
called inline right after the Y colours, but libaom's `av1_visit_palette`
(`decoder.c:234`) only runs `decode_color_map_tokens` from
`parse_decode_block` (`decodeframe.c:1135`) **after** the whole mode-info
read, including UV palette mode_info (`read_palette_mode_info`'s own
second half, `decodemv.c:588`) and `filter_intra`.

Two changes, both inside `read_intra_mode`:

1. **UV palette mode_info read added.** Right after the Y colours are read
   (or Y palette doesn't fire), read `palette_uv_mode` at
   `ctx = usize::from(use_palette_y)` whenever `uv_mode == UV_DC_PRED` --
   unconditionally, not gated on whether Y fired (that was the actual bug:
   the old code returned early inside the `use_palette_y` branch, before
   this read was ever reached). No `read_palette_colors_uv` port was
   needed: UV palette reconstruction stays out of scope, so the moment the
   mode symbol fires this decoder refuses by name immediately and the
   whole decode aborts -- no further bits from that stream matter, so
   `palette_uv_size`/the UV colour reader never need to exist. `is_chroma_ref`
   didn't need threading in either: this decoder's scope is squares only
   and `decode_block` always reconstructs chroma alongside luma, so it is
   unconditionally true here (matches `av1_visit_palette`'s own `plane == 0
   || xd->is_chroma_ref` when `is_chroma_ref` is always true).
2. **Y's `decode_color_index_map` call moved.** The Y palette branch now
   only reads mode/size/colours and stashes them in a local
   `palette_y_pending: Option<(usize, [u16; 8])>`; the actual
   `decode_color_index_map` call (and the `PALETTE_HITS` counter bump) now
   runs after `filter_intra` is read, matching
   `decode_mbmi_block` → `av1_visit_palette` order exactly.

The old "index map decodes but pixels don't match" refusal
(`decode.rs`'s r3/r4 comment block) is gone -- replaced by an r7 doc
comment explaining the reorder and citing `compare-range-not-tell`.

## Gate: refuses-by-name -> decodes-pixel-exact

`crates/ec-av1/src/stream.rs`'s
`a_real_aomenc_stream_with_palette_y_refuses_by_name` renamed to
`a_real_aomenc_stream_with_palette_y_decodes_pixel_exact`: same fixture
(`smptebars=size=64x64:rate=25` + `hue=s=0`, `--enable-palette=1
--tune-content=screen`, every non-palette intra tool forced off so a
mismatch can only come from palette), but now asserts `decode_stream`
succeeds, `palette_hits() > before` (proves the block actually fired, not
a vacuous pass), and `frames[0].{y,u,v}` match `ffmpeg_decode_sequence`'s
decode of the same bytes exactly.

```
EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -- \
  a_real_aomenc_stream_with_palette_y_decodes_pixel_exact
```
-> `test stream::tests::a_real_aomenc_stream_with_palette_y_decodes_pixel_exact ... ok`

## refusal_inventory.rs

Dropped `"a block that actually uses a palette (Y) -- the index map
decodes but the reconstructed pixels do not match libaom yet (lane-palette
r3/r4)"` from `REFUSALS` (the string no longer exists in the source, so the
pinned-inventory test would otherwise fail). `"a block that actually uses a
palette (Y) -- reconstruction is out of scope"` (the excluded-call-site
refusal, unaffected by this round) and `"...UV... -- reconstruction is out
of scope"` both stay -- neither string changed.

`gate_coverage.rs` has no `enable-palette` entry to remove: the gate
already passed `--enable-palette=1` before this round, so this milestone's
"drops off the list" note in the charter doesn't apply here.

## Oracle hygiene

No patch to `~/.cache/aom-oracle` was needed this round -- the fix was
derived by reading libaom source directly (`av1_visit_palette`,
`read_palette_mode_info`, `read_palette_colors_uv`) and verified purely
against this repo's own `decode_stream` vs `ffmpeg_decode_sequence`, no
`aomdec EC_TRACE`/`decodemv.c` re-instrumentation this time. The checkout
is untouched by this lane (its working tree shows pre-existing sibling-lane
modifications to `restoration.c`/`decodeframe.c`/`decodemv.c`/`decodetxb.c`/
`detokenize.c` that were already there before this round started -- not
mine to revert or rebuild). No new env-gated rung added to
`scripts/instrument-aom-oracle.sh` since none was needed.

## Checks

- `cargo build -p ec-av1 --tests -j4`: clean (pre-existing doc-lint
  warnings only, no new ones).
- `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j4` (before merging
  main, timeout 600000ms, ~147s): **244 passed, 0 failed, 17 ignored**
  (r6's 244-test baseline, palette gate now green instead of "refuses").
- After `git merge main` (main brought in 5 more tests, tile-rows work):
  `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j4` (~175s):
  **249 passed, 0 failed, 17 ignored**.

## Commits (this branch, on top of bba4d3c)

- `dd399d4` -- the fix + gate flip + refusal_inventory update, all one
  commit per charter.
- `406e0f2` -- `git merge main` (auto-merged cleanly: `decode.rs`,
  `refusal_inventory.rs`, `stream.rs` all resolved without conflicts,
  main's `bit depth != 8` refusal and tile-rows work landed alongside
  this round's palette changes).

Branch left unpushed, unmerged to main, per charter ("Never push, never
merge this branch into main").

## Handoff for the next round

r6's own next-milestone list (`palette_bsize_ctx` rect-strip refusal,
`enable-intrabc`) is still fully unstarted -- this round's entire scope was
the reorder fix the charter named, and it's done. UV palette
*reconstruction* (as opposed to the mode_info read, now correct) also
remains a named refusal and unstarted; it was never in this round's scope.

`deferred: palette_bsize_ctx rect-strip refusal, intrabc, UV palette
reconstruction -- all unstarted, out of this round's charter scope.`
