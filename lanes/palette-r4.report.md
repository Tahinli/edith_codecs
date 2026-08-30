# lane-palette r4 report

VERDICT: PARTIAL -- Job 1 (the SKIP-on-wrong-pixels shape) is fixed and
committed, green. Job 2 (locate the desync) is not closed this round: the
oracle-side range-ladder rung for the palette colour-index map is built and
compiles, but no bisect run against our own decoder's ranges was completed
within budget.

## Job 1 -- refuse by name instead of SKIP on wrong pixels (committed 882c2d7)

`crates/ec-av1/src/decode.rs`, the `palette_y_mode` arm (~line 2782): after
`read_palette_colors_y` + `decode_color_index_map` finish and
`PALETTE_HITS` increments, the arm now returns
`Err(unsupported("a block that actually uses a palette (Y) -- the index
map decodes but the reconstructed pixels do not match libaom yet
(lane-palette r3/r4)"))` instead of building `palette_y = Some(...)` and
falling through to reconstruction. All the landed reconstruction code
(`PALETTE_PRED`, the `py.colors[idx]` buffer build, `record_palette_y`,
etc.) is untouched and still compiles -- it is simply unreachable until this
refusal is removed once the desync is fixed.

`crates/ec-av1/src/stream.rs`: the gate is renamed
`a_real_aomenc_stream_with_palette_y_refuses_by_name` (was
`..._decodes_pixel_exact`) and rewritten to assert `decode_stream` returns
`Err` containing `"reconstructed pixels do not match libaom"`, with
`palette_hits()` still hard-asserted to have moved (proves the refusal
isn't vacuous -- it's reached only after a real palette block's full
syntax decodes). No more pixel comparison, no more SKIP path.

**Refusal strings changed:** one message text changed (same call site, no
new site): `"a block that actually uses a palette (Y) -- reconstruction is
out of scope"` (only fired when `palette.is_none()`, a different call-site
condition, unchanged) is now ALSO reachable unconditionally after a full Y
palette decode, with message `"a block that actually uses a palette (Y) --
the index map decodes but the reconstructed pixels do not match libaom yet
(lane-palette r3/r4)"`. No refusal strings removed. `gate_coverage.rs`'s
`NEVER_EXERCISED` list is untouched by this round (r3 already removed
`enable-palette` correctly, per its own report).

**Check:** `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j4`: 235
passed, 0 failed, 17 ignored (was 234/0/17 before this round's new test;
+1 test, all still green). Scoped run of the renamed gate alone also green.

## Job 2 -- desync location: rung built, bisect not run

Per charter, r3 already cleared `read_palette_colors_y`, `read_uniform`,
`palette_color_index_context` and the `PALETTE_Y_COLOR_INDEX` CDF table
against the oracle line-for-line, and the charter forbids re-reading them.
Added a new `EC_TRACE_PALETTE=1` rung to
`scripts/instrument-aom-oracle.sh` (rungs 6/6b), targeting
`av1/decoder/detokenize.c`'s `decode_color_map_tokens`:
- `EC_PAL row=.. col=.. ctx=.. n=.. rng=..` before every colour-index
  symbol in the wavefront (and a `row=0 col=0 ctx=-1` line before the
  `map[0] = av1_read_uniform(...)` read, which the loop doesn't cover),
  `EC_PAL_VAL row=.. col=.. color_idx=.. rng=..` after.
- Idempotent, env-gated, wrapper-around-impl shape matching the existing
  rungs; ran `bash scripts/instrument-aom-oracle.sh` (reports "palette map
  instrumented" / "palette map[0] instrumented", no-op on the five
  pre-existing rungs) then `ninja -C ~/.cache/aom-oracle/build aomdec`
  clean.

**Not done this round:** our own `decode_color_index_map` (decode.rs
~2182) has no matching `EC_AV1_TRACE`-gated per-pixel range print yet, and
no bisect run comparing `aomdec EC_TRACE_PALETTE=1` against our decoder's
own ranges was executed against the r3 fixture stream. That is squarely
the charter's job 2 and is the very next step -- the oracle side is ready,
the decoder side isn't instrumented, and no stream was regenerated this
round to feed either.

## Deferred

- `deferred: Job 2 bisect itself (compare EC_TRACE_PALETTE against our own
  decode_color_index_map's ranges) -- ran out of budget after building the
  oracle rung -- next round: add the matching EC_AV1_TRACE print in
  decode_color_index_map (row, col, ctx, n, rng before; color_idx, rng
  after -- same shape as the oracle's), regenerate the r3 fixture stream,
  diff the two traces per compare-range-not-tell/equal-range-means-unread.`
- `deferred: "Then" milestones (palette UV refusal accuracy check, the
  rect-strip palette_bsize_ctx refusal, intrabc) -- never started, all
  budget went to job 1 + the job 2 rung.`

## Handoff for the next round

1. Instrument `decode_color_index_map` in `crates/ec-av1/src/decode.rs`
   with an `EC_AV1_TRACE`-style per-pixel print (there's already a `trace`
   local + `EC_AV1_TRACE` eprintln right after this function's call site at
   ~2792, so the flag plumbing exists -- just add the print inside the
   function itself, matching `EC_PAL row=.. col=.. ctx=.. n=.. rng=..` /
   `EC_PAL_VAL ...` from the oracle rung field-for-field). Our `SymbolDecoder`
   needs a `range()`/similar accessor mirroring `SymbolEncoder::rng()` (msac.rs
   ~83) -- check whether the decoder side already exposes `range` (msac.rs
   struct field `range: u32` on `SymbolDecoder`, ~260) before adding one.
2. Regenerate the r3 gate's exact fixture (`smptebars=size=64x64` +
   `hue=s=0`, the aomenc args in the (now renamed) gate test) once, save the
   stream bytes to a file, and run both `aomdec EC_TRACE_PALETTE=1` and our
   own decoder (`EC_AV1_TRACE=1`, once instrumented) against the identical
   bytes.
3. Per the charter's own steer: if the ranges match to the end of the
   block, the reads are right and the bug is in how the palette map is
   applied (per-TU slicing, `PALETTE_PRED`'s lifetime, or chroma/luma plane
   mixing) -- decode.rs ~3347's `PALETTE_PRED.with(...)` set/take pairing
   and the `buf` construction (`py.map.iter().map(|&idx| py.colors[idx as
   usize] as u8)`) are the first places to look, not the symbol reads
   again.
