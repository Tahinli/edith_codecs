# lane-realworld r5 report

VERDICT: Job 1 (get dbfd67c green) DONE, committed (70372d1, c185755).
Job 2 (delta_q/delta_lf) both halves now read and applied, committed
(70372d1 for delta_q's compile-completion, a830d87 for delta_lf's new
deblocker path). Job 2's gate (charter step 4, a real aomenc stream with
`--deltaq-mode=`/`--aq-mode` proving live firing) is UNSTARTED -- next
lever, see below.

## Job 1 -- dbfd67c green (70372d1, c185755)
r4 had wired `delta: DeltaParams` all the way through
`decode_key_frame_tile_with_cdfs` (including its 2 stream.rs call sites,
its own tile-loop `DELTA_Q_PRESENT`/`DELTA_Q_RES`/`CURRENT_Q_IDX` reset)
but never touched `decode_inter_frame_tile_with_cdfs`'s own signature --
its one public wrapper (`decode_inter_frame_tile`) was already passing
`DeltaParams::default()` as an extra, unaccepted argument. Added the
`delta: DeltaParams` param to `decode_inter_frame_tile_with_cdfs`, the
matching `DELTA_Q_PRESENT`/`DELTA_Q_RES`/`CURRENT_Q_IDX` reset at its own
single-tile entry point (mirroring the key-frame per-tile reset), and
`header.delta` at stream.rs's inter-frame call site plus its test-only
key-frame call site (stream.rs:6179, a `#[cfg(test)]` path r4 missed).
Narrowed the whole-frame refusal from `q_present || lf_present` to
`lf_present` alone (delta_q was already genuinely read on both frame
types once this compiled), and updated `refusal_inventory.rs`'s
`REFUSALS` list to the narrowed string. `EC_AV1_REQUIRE_AOMENC=1 cargo
test -p ec-av1 --lib -j4`: **242 passed, 0 failed, 17 ignored** (r3's
234/0/17 plus lane-tiles/lane-realworld merges already on `main` before
this worktree branched).

## Job 2 -- delta_lf's new deblocker path (a830d87)
Exactly the piece r3 flagged as needing a real design pass, not a narrow
substitution:

- `CURRENT_DELTA_LF: Cell<[i32; 4]>` (spec `DeltaLF[FRAME_LF_COUNT]`),
  reset to `[0; 4]` at the top of every tile in both
  `decode_key_frame_tile_with_cdfs` and `decode_inter_frame_tile_with_cdfs`
  (mirroring `CURRENT_Q_IDX`'s own per-tile reset), plus
  `DELTA_LF_PRESENT`/`DELTA_LF_RES`/`DELTA_LF_MULTI` set from
  `header.delta` the same way `DELTA_Q_PRESENT`/`DELTA_Q_RES` already were.
- `maybe_read_delta_lf` (decode.rs, next to `maybe_read_delta_q`): same
  4-symbol-CDF-then-Golomb-tail-then-sign shape, called right after
  `maybe_read_delta_q` at all 4 existing block-decode call sites (spec
  order `delta_q -> delta_lf`). Loops over 4 planes/directions when
  `delta_lf_multi` (reading `cdfs.delta_lf_multi[i]`, r4's already-wired
  CDF), or reads once into index 0 against `cdfs.delta_lf` otherwise.
  `DELTA_LF_HITS`/`delta_lf_hits()` mirror `DELTA_Q_HITS` for a future
  gate's firing count.
- New `Neighbours::delta_lf_grid: Vec<[i8; 4]>` field, snapshotted per
  4x4-mi block in `fill_lf_grid_rect` (the one function every existing
  `fill_lf_grid`/`fill_lf_grid_rect` call site already routes through, so
  no call-site changes needed) -- `!delta_lf_multi` mode broadcasts
  `DeltaLF[0]` into all 4 slots at write time, so `lf_level`'s reader
  never needs to know which mode produced the value, matching
  `ref_grid`'s existing snapshot pattern exactly.
- `lf_level` takes a new `delta_lf: i32` param, applied to the base level
  (`clamp(0, 63)`) *before* the existing `ref_deltas`/`mode_deltas`
  scaling and regardless of `lf.delta_enabled` (spec 7.14.4: that flag
  only gates the ref/mode terms, not `DeltaLF`). `edge_params` looks up
  `delta_lf_grid` at both the current and previous mi position via a new
  `plane_idx/dir -> FRAME_LF_COUNT` index map (`0`/`1` for Y
  vertical/horizontal, `2`/`3` for U/V), the same two-position lookup
  pattern it already uses for `ref_grid`.
- Removed the stream.rs `delta_lf_present` refusal entirely (both halves
  of Job 2 are now read and applied). `refusal_inventory.rs`'s `REFUSALS`
  list: removed `"a frame with delta_lf_present set (this decoder never
  applies per-superblock loop-filter deltas)"`, no replacement string.

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j4`: **242 passed, 0
failed, 17 ignored** -- unchanged from Job 1's number, because no
existing fixture sets `delta_lf_present`. This means the new deblocker
path is **code-verified against the spec/libaom read shape**, not yet
gate-proven against a real aomenc stream that actually exercises it.

## Refusal strings changed this lane (verbatim)
- Removed: `"a frame with delta_q_present or delta_lf_present set (this
  decoder never reads the per-superblock delta symbols)"`.
- Added then removed within this same lane (never landed on `main`):
  `"a frame with delta_lf_present set (this decoder never applies
  per-superblock loop-filter deltas)"` -- present after commit 70372d1,
  removed by commit a830d87. Net effect for merge purposes: the old
  combined refusal is gone, no new refusal string replaces it.

## Next lever -- charter step 4, the gate
Not started this round (turn budget spent on Jobs 1-2's design/wiring).
Needed: a real aomenc stream with `--deltaq-mode=1` or `--aq-mode=1`
(reliably sets `delta_q_present`) plus a recipe that also sets
`delta_lf_present`/`delta_lf_multi` (aomenc's `--deltaq-mode` alone may
not set `lf_present` -- needs checking against aomenc's own CLI, not
assumed), gradients source through `gradients_source`, `--threads=1
--row-mt=0 --sb-size=64`, MORE THAN ONE SUPERBLOCK (128x64+), and a
hard-asserted `delta_q_hits()`/`delta_lf_hits()` firing count in an
attempt loop (never a printed SKIP on decode error, per the charter's
class reminder). `DELTA_Q_HITS`/`DELTA_LF_HITS`/`delta_q_hits()`/
`delta_lf_hits()` are already in place from this round's work, ready for
that gate to call.

## Merge note
`gate_coverage.rs`/`refusal_inventory.rs` guards this worktree doesn't
carry: `refusal_inventory.rs`'s own `REFUSALS` list was already updated
in this worktree (main-tracked file), so the merge just needs the diff
as-is -- no separate main-only edit required for the refusal list itself.
`gate_coverage.rs` (main-only) will need the eventual gate test's name
added when Job 2's gate lands.
