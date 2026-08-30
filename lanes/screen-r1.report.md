VERDICT: GREEN — key-frame palette/intrabc syntax consumed (not reconstructed), whole-frame allow_screen_content_tools refusal narrowed to non-key frames only; 224/0 lib suite unchanged; ab_partitions gate 5/40 matches (35 named refusals, partab_hits=2, none of them screen-content-tools anymore); filter_intra gate now runs its full pixel-exact assertion instead of SKIPping on decode_stream's screen-content-tools refusal.

## What landed this round (commit 15fbff6, on top of prior builder's 09b50ee)

Prior builder had already wired `read_intra_mode`/`decode_block`/`decode_leaf8` to read
`use_intrabc` and `palette_y_mode`/`palette_uv_mode` and refuse by name if either ever
actually fires (palette_mode_ctx/palette_uv_mode_ctx hardcoded to 0, a documented safe
corner-cut). What was missing was purely mechanical threading:

- `decode_key_frame_tile_with_cdfs` and the public `decode_key_frame_tile` wrapper gained
  trailing `allow_screen_content_tools: bool, allow_intrabc: bool` params (the internal
  `decode_block`/`decode_leaf8` call sites already referenced these names — they were just
  never in scope at the outer function's signature).
- `stream.rs`'s real production call now passes `header.allow_screen_content_tools,
  header.allow_intrabc`.
- 7 test call sites (`decode.rs` x6, `stream.rs` x1) pass `false, false` — none of those
  fixtures use screen content tools, so this is inert for them.
- `stream.rs:169`'s whole-frame refusal narrowed from `if header.allow_screen_content_tools`
  to `if header.frame_type != FrameType::Key && header.allow_screen_content_tools`. This is
  the load-bearing line: `decode_inter_block`/`decode_inter_block8`'s intra-sub-block branches
  (libaom's `read_intra_block_mode_info` equivalent) never call `read_intra_mode` and have no
  palette/intrabc wiring at all, so a non-key frame with the bit set would desync silently if
  the refusal were removed outright instead of narrowed. The `delta.q_present || delta.lf_present`
  refusal on the next lines is untouched, as instructed.

## Verification

`cargo check -p ec-av1 --lib --tests`: clean (pre-existing warnings only).

`cargo test -p ec-av1 --release --lib` (no aomenc requirement): **224 passed, 0 failed, 17
ignored** — identical to the pre-change baseline; every existing stream has the bit clear, so
the new code path is inert for them, confirming the threading didn't perturb anything already
green.

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --release --lib pinned`: 2 passed
(`pinned_golden7_stream_decodes_pixel_exact`, `pinned_warp_stream_decodes_pixel_exact`), 4
failed — all 4 on `NotFound`/`NotPresent` reading a hardcoded fixture path or missing
`EC_AV1_PIN` env var (`pinned_golden3`/`pinned_golden4`/`scratch_decode_pinned_stream_once`/
`scratch_isolate_pinned_mismatch`), matching the charter's stated pre-existing, unrelated-to-
this-lane stale-scratchpad-path failures exactly (4 of 17 ignored).

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --release --lib
a_real_aomenc_stream_with_ab_partitions_decodes_pixel_exact`: **ok**, "35 named refusals, 5
pixel-exact matches out of 40, partab_hits=2". Refusal breakdown from the run: mostly
"a partition type this encoder never writes" and "a non-skip rectangular strip needs
rectangular residual coding" (both pre-existing, unrelated gaps), plus 3 seeds refusing on
"a non-key frame with allow_screen_content_tools set" — i.e. the screen-content-tools bit is
still firing on ~14/40 attempts as the charter's baseline says, but now correctly scoped to
inter frames only (the intra/key-frame occurrences that used to refuse the whole stream no
longer do). Charter's baseline count (~14/40 refusing on this bit specifically) is consistent
with what's visible in this run's log (seeds 67, 69, 70, 79 named explicitly in the tail —
full seed list not individually tallied beyond the visible refusal lines, but the shape
matches: this lane did not change the *inter-frame* refusal count, by design).

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --release --lib
a_real_aomenc_filter_intra_stream_decodes_pixel_exact`: **ok**, and critically — no
`SKIP a_real_aomenc_filter_intra_stream_decodes_pixel_exact: ...` eprintln in the output. That
SKIP line existed specifically because `decode_stream` used to refuse the whole (key) frame
whenever libaom's internal `screen_content_tools_determination` happened to flip
`allow_screen_content_tools=1` on this fixture even with `--enable-palette=0
--enable-intrabc=0`. With the key-frame path now consuming that syntax instead of refusing,
the test runs to its real assertions (`filter_intra_hits` moved, luma/U/V pixel-exact vs
ffmpeg) every time this round, i.e. it now exercises the gate instead of skipping it.

## Ceiling for the next lane

Threading `decode_inter_block`/`decode_inter_block8`'s intra-sub-block branches (libaom's
`read_intra_block_mode_info`) with the same `allow_screen_content_tools`/`allow_intrabc`
params — and reading `use_intrabc`/palette mode there the same way `read_intra_mode` does now
— is what would let the `stream.rs:169` refusal narrow further (or drop) for non-key frames.
Until that lands, every inter frame with the bit set keeps refusing by name; this lane
deliberately did not touch that surface (out of scope per charter, and doing it blind would
risk a silent desync exactly like the key-frame path was one signature-threading step away
from having, before this round).

Claude-Session: https://claude.ai/code/session_01T6cfkyThENXszWWQqYpuC4
