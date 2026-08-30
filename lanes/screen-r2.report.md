VERDICT: GREEN — decode_inter_block/decode_inter_block8's intra-sub-block branches now consume PALETTE_Y_MODE/PALETTE_UV_MODE the same way read_intra_mode does for key frames; stream.rs:169's `frame_type != Key && allow_screen_content_tools` whole-frame refusal is removed entirely; 231/0 lib suite unchanged.

## What landed (commit daa6934, on top of merged lane-screen r1 at 42af470)

libaom's inter-frame intra-sub-block reader is `read_intra_block_mode_info`
(decodemv.c:1065-1107) — confirmed by reading it directly, NOT the
key-frame `read_intra_frame_mode_info` the charter warned might not
transfer. Two findings that changed scope from what the charter assumed:

- Its `mbmi->mode` read already uses `ec_ctx->y_mode_cdf[size_group_lookup[bsize]]`,
  never `kf_y_mode_cdf` — and this decoder's `decode_inter_block`/
  `decode_inter_block8` intra branches already read `cdfs.y_mode[size_group]`/
  `cdfs.y_mode[SIZE_GROUP_8]` respectively (pre-existing code, unrelated to
  this lane). No CDF-selection bug to fix here.
- `read_intra_block_mode_info` has **no `use_intrabc` call at all**.
  `av1_read_mode_info` dispatches `use_intrabc = 0` then branches on
  `frame_is_intra_only(cm)`: only that branch calls `read_intra_frame_mode_info`
  (which reads `use_intrabc`); the inter-frame branch calls
  `read_inter_frame_mode_info` → `read_intra_block_mode_info` for an intra
  sub-block, with no intrabc symbol at all — it's an intra-frame-only bit by
  spec construction, not something this decoder was ever missing here. So
  this round's whole job collapsed to palette only.

What was added, mirroring `read_intra_mode`'s existing r1 wiring exactly
(same `av1_allow_palette` gate reused via the existing `palette_bsize_ctx`
helper, same `palette_mode_ctx`/`palette_uv_mode_ctx` hardcoded-0
corner-cut — still safe, unchanged reasoning):

- `decode_inter_block`'s square-intra branch: after `uv_mode`/`alpha` are
  read (same point `read_palette_mode_info` sits at in libaom, right after
  `xd->cfl.store_y`), reads `palette_y_mode[bsize_ctx][0]` when
  `mode == DC_PRED` and `palette_uv_mode[0]` when `uv_mode == DC_PRED`,
  refusing by name on either firing nonzero.
- `decode_inter_block8`'s 8x8-leaf intra branch: same two reads, `bsize_ctx`
  fixed at 0 (`BLOCK_8X8` is always `av1_allow_palette`-eligible; the leaf's
  own existing refusal already forces `uv_mode == DC_PRED`, so the UV read
  is unconditional there, matching libaom's own always-true gate for this
  shape).
- Both functions gained a trailing `allow_screen_content_tools: bool` param;
  `decode_inter_frame_tile_with_cdfs` gained the same param, threaded to all
  19 `decode_inter_block` and 1 `decode_inter_block8` call sites (the public
  `decode_inter_frame_tile` wrapper and one test call site pass `false`,
  matching the existing key-frame wrapper convention).
- `stream.rs`'s production call passes `header.allow_screen_content_tools`.
- `stream.rs:169`'s remaining `frame_type != Key && allow_screen_content_tools`
  refusal is deleted (comment left explaining why no whole-frame refusal is
  needed anymore — genuine palette use still refuses deeper in the block
  readers, same as key frames). The `delta.q_present || delta.lf_present`
  refusal on the lines below is untouched, per charter.

No new CDF tables — `palette_y_mode`/`palette_uv_mode`/`palette_y_size`/
`palette_uv_size` already exist and were already 4-site-wired in r1
(`reset2`/`reset3` length-generic, save/restore whole-struct, defaults
checked then). This round only reused them from a second call site.

Rect intra strips inside a screen-content frame still refuse by name
("a HORZ/VERT intra strip in a screen-content frame (palette syntax is
consumed for square blocks only)") — untouched, per charter.

## Verification

`cargo check -p ec-av1 --lib --tests`: clean (pre-existing warnings only).

`cargo test -p ec-av1 --release --lib`: **231 passed, 0 failed, 17 ignored**
— identical to the pre-change baseline; the bit is clear on every existing
stream, so the new palette reads are unreached (inert) for all of them.

`allow_screen_content_tools` refusal count, before (commit 42af470) vs after
(daa6934), same seed range, same test:

- `a_real_aomenc_stream_with_ab_partitions_decodes_pixel_exact`: **before 6/40**
  refusing specifically on `"a non-key frame with allow_screen_content_tools set"`
  (34 named refusals total, 6 pixel-exact matches, partab_hits=3) → **after
  0/40** on that string (31 named refusals — same partition/rect-transform/
  rect-screen-content-strip reasons as before minus the 6 whole-frame ones,
  9 pixel-exact matches, partab_hits=5).
- `a_real_aomenc_stream_with_interintra_wedge_decodes_pixel_exact`: 0/40 on
  that string both before and after in this fixed-seed run — every refusal
  here is `"more than one concurrently active ROTZOOM/AFFINE global-motion
  ref slot"` (unrelated, pre-existing gap), 20/40 pixel-exact both times
  (wii_hits 8-9, consistent with the documented aomenc-RD-nondeterminism
  attempt-selection class). The charter's "9 of 40" estimate for this gate
  did not reproduce in this measurement — noted rather than silently
  dropped; the AB-partitions gate is the one that actually carried this
  refusal in both rounds.

## What still refuses

- Genuine palette use (nonzero `palette_size`) or genuine intrabc use, on
  any frame type — reconstruction is out of scope, by design (spec-correct
  named refusal, not a desync).
- A HORZ/VERT (rect) intra strip in a screen-content frame — the square-only
  `palette_bsize_ctx` doesn't cover rect strips; unchanged from r1.
- `SMOOTH_PRED`/`PAETH_PRED` chroma, directional chroma on the general
  intra-in-inter path, non-angle-zero deltas, `mode >= 13` — all pre-existing,
  unrelated round-2 gaps this lane didn't touch.

Claude-Session: https://claude.ai/code/session_01T6cfkyThENXszWWQqYpuC4
