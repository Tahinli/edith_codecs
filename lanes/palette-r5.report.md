# lane-palette r5 report

VERDICT: PARTIAL -- Job 1 (merge main, resolve, suite green, commit) is
done and committed (7666b7a). Job 2 (the range-ladder bisect) was not
started this round: the turn-budget hook fired at 55/75 right after job 1
landed green, and starting a fresh instrumentation+bisect with 20 calls
left risked leaving it half-done and uncommitted, which is worse than
deferring it cleanly.

## Job 1 -- merge main, resolved, committed (7666b7a)

`git merge main` (main at 53f5358) conflicted in six places in
`crates/ec-av1/src/decode.rs` and one in `crates/ec-av1/src/gate_coverage.rs`,
exactly as the charter predicted:

- **`Neighbours` struct + its constructor** (2 conflicts): both sides added
  new fields to the same struct -- this lane's `above_palette_size` /
  `left_palette_size` / `above_palette_colors` / `left_palette_colors`, and
  main's `tile_row0_mi` / `tile_col0_mi`. Kept both (no overlap in meaning).
  One conflict marker (`>>>>>>> main`) was a stray leftover spanning past the
  constructor's closing brace into the unconflicted `start_tile` body --
  removed the stray marker line, left `start_tile`'s body (all main's,
  unconflicted) intact.
- **`read_intra_mode`'s signature** (1 conflict + 2 call-site conflicts):
  this lane added a `palette: Option<(usize, &[u16])>` param, main added
  `mi_r: usize, mi_c: usize` (used by `maybe_read_cdef_idx` inside the
  function body, which the merge kept). Both params are real and used --
  kept both, `palette` before `mi_r, mi_c`, and updated both call sites
  (~3548 rect/square path, ~3900 the 8x8-leaf path) to match the new
  4-arg tail in the same order.
- **`PlaneBuf::reconstruct`** (1 conflict): this lane added the
  `PALETTE_PRED` thread-local override + `filter_intra` branch inside the
  `else`; main added real `smooth_neighbor` threading (was hardcoded
  `false`) plus dropped this lane's now-stale doc comment about the
  hardcoding being "exact for chroma". Kept this lane's palette-override +
  filter_intra structure, swapped the hardcoded `false` in the `predict()`
  call for main's `smooth_neighbor` parameter and its shorter comment
  (main's chroma lane made the old caveat comment no longer true).
- **`gate_coverage.rs`'s `NEVER_EXERCISED`**: this lane's side (via an
  already-merged local chroma lane) listed `enable-paeth-intra` /
  `enable-smooth-intra` (and, outside the conflict hunk, `enable-cdef`);
  main's side listed `enable-palette` (plus `enable-intrabc`, unconflicted).
  Per charter: kept `enable-intrabc` only. `enable-palette` drops because
  this lane's own real palette gate now exercises it; `enable-cdef` /
  `enable-paeth-intra` / `enable-smooth-intra` drop because main's
  CDEF-index and chroma-smooth/paeth lanes, now merged in, exercise those
  too -- none of the three belonged on this lane's list once main's fixes
  are actually present in the tree.

**`refusal_inventory.rs`** (added cleanly by the merge, no conflict, but
incomplete): the suite's `the_decode_path_refuses_exactly_the_listed_cases`
test caught two refusal strings this lane's own history introduced that
weren't yet in the pinned `REFUSALS` list -- the r4 palette-Y pixel-mismatch
refusal ("...the index map decodes but the reconstructed pixels do not
match libaom yet (lane-palette r3/r4)") and a pre-existing
"a palette block with a split luma transform (round 1)" string. Added
both. No refusal strings removed or reworded by this merge; main's own
three reworded "encoder never writes" strings (2be815f, already on `main`
before this merge) carried through untouched.

**Check:** `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j4`
(timeout 600000ms): first run failed 1/244 on the missing refusal-inventory
entries (caught and fixed above); rerun: **244 passed, 0 failed, 17
ignored**. Scoped `refusal_inventory` run alone: 3/3 green. `cargo check -p
ec-av1`: clean (only pre-existing doc-warning noise).

## Job 2 -- not started, deferred

`deferred: the EC_TRACE_PALETTE range-ladder bisect (per-pixel range
compare between aomdec's decode_color_map_tokens rung, already built by r4
in scripts/instrument-aom-oracle.sh rungs 6/6b, and a matching
EC_AV1_TRACE print this lane still needs to add inside
decode_color_index_map in decode.rs ~2182/2792) -- ran out of turn budget
right after job 1 landed green; r4's handoff (lanes/palette-r4.report.md,
"Handoff for the next round") is still accurate and unconsumed: (1) add
the per-pixel EC_PAL row/col/ctx/n/rng / EC_PAL_VAL trace inside
decode_color_index_map, checking whether SymbolDecoder already exposes a
range field/accessor (msac.rs ~260) before adding one; (2) regenerate the
r3/r4 gate's exact fixture stream once (smptebars=size=64x64 + hue=s=0)
and save the bytes; (3) run aomdec EC_TRACE_PALETTE=1 and our own decoder
EC_AV1_TRACE=1 against the identical bytes and diff per
compare-range-not-tell / equal-range-means-unread; (4) if ranges match to
the end of the block, the bug is in palette application (PALETTE_PRED's
set/take pairing or the buf = colors[idx] build at decode.rs ~3347), not
the symbol reads.`

`deferred: the "Then" milestones (palette UV, palette_bsize_ctx rect-strip
refusal, intrabc) -- unstarted, all budget went to job 1; unchanged from
r4's own deferral.`

## Handoff for the next round

Start straight from job 2 step (1) above -- the merge is done, the suite
is green, `main`'s CDEF/chroma/tile landings are now in this branch's tree
so no further merge debt exists. Next round should NOT need to touch
`decode.rs`'s struct/signature layout again; the palette index-map
function itself (`decode_color_index_map`, ~2182) is the only untouched
surface job 2 needs.
