VERDICT: GREEN

## Step 1 -- verify HEAD
`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j4`: **234 passed; 0 failed; 17
ignored** (vs the charter's 232/0 baseline -- 2 extra tests are r3's own new
smooth-intra/smooth-paeth-chroma gates, already in HEAD). Tree was clean at
start (nothing to commit for this step); r3's cap-rescued work is genuinely
green, contrary to the "no suite run has ever been seen" warning.

## Step 2 -- chroma UV-mode neighbour tracking + smooth/paeth chroma refusal
Already done in HEAD (r3's commit `47da0be`): `Neighbours::above_uv_mode`/
`left_uv_mode` exist, `decode.rs`'s two `read_intra_mode`/`read_intra_mode_rect`
sites (keyframe callers) removed the `9..=12` uv_mode refusal, and
`lanes/chroma-r1-attempt.diff`'s gate
(`a_real_aomenc_stream_with_smooth_paeth_chroma_decodes_pixel_exact`) is
present in `stream.rs` and passes on its own
(`cargo test ... smooth_paeth_chroma` -- 1 passed). No new commit needed.

## Step 3 -- gate_coverage.rs
`crates/ec-av1/src/gate_coverage.rs` **does not exist in this tree**
(confirmed via `find`). Per the charter, the merge must delete the
`enable-smooth-intra`/`enable-paeth-intra` entries from `NEVER_EXERCISED`
on `main`'s copy.

## Step 4 -- directional-chroma sibling (turns remained)
Charter's line number (`decode.rs:7786`) had drifted from r3's +99 lines;
the real sibling was `decode_inter_block`'s intra-in-inter branch (the
`else` arm handling an intra block coded inside an inter frame), which read
`uv_mode` and refused outright with `"a directional chroma mode (round 2)"`
for anything past DC/CFL, and never read `angle_delta_uv` at all.

Fixed by mirroring `read_intra_mode`'s already-proven pattern in that one
function (`crates/ec-av1/src/decode.rs`, `decode_inter_block`):
- `uv_predict_mode` (CFL predicts as DC for the reconstruct call, same as
  the keyframe path) threaded through both `u`/`v.reconstruct` calls (skip
  path) and both chroma `read_plane` calls (non-skip path), replacing the
  hardcoded `DC_PRED`.
- `angle_delta_uv` computed via `read_angle_delta` when `uv_mode` is
  directional (`V_PRED..=D67_PRED`), replacing the hardcoded `0`.
- `smooth_neighbor_uv` computed from the chroma neighbour's own `uv_mode`
  (`is_smooth_mode(above_uv_mode/left_uv_mode)`), replacing the hardcoded
  `false`, threaded into the same reconstruct/read_plane calls.
- `SMOOTH_UV_HITS`/`DIRECTIONAL_UV_HITS` counted here too (shared
  thread-locals with the keyframe path).
- `neighbours.record_rect`'s uv-mode arg (previously hardcoded `DC_PRED`)
  now carries the real `uv_predict_mode` so a following block's own
  smooth-neighbour check sees it.
- The luma-mode side of this branch (`mode`, its own angle_delta refusal at
  the top) is **untouched** -- out of this charter step's chroma-only scope.

`cargo check -p ec-av1 --lib`: clean (no new warnings/errors). Full lib
suite re-run after the change: still **234 passed; 0 failed; 17 ignored** --
no regression.

**Gap, disclosed per "refusal strings are claims":** no dedicated gate
proves a real aomenc stream ever actually exercises this specific path (an
intra block, directional/smooth/paeth `uv_mode`, coded *inside an inter
frame*) end to end against ffmpeg. The change is a direct structural mirror
of the already-gated keyframe fix (identical CDF tables, identical
`get_uv_mode`/angle-delta semantics per spec 9.3), and the full suite stays
green, but it is **code-verified by mirroring, not gate-verified by firing**.
`deferred: dedicated a_real_aomenc_stream_with_directional_chroma_in_inter_frame_intra gate -- next round, no seat left this round -- unblocks by writing a stream.rs gate that forces an inter-frame key/delta pair whose delta frame codes an intra 8x8+ leaf with directional/smooth/paeth uv_mode (drop --enable-tx-size-search=0 constraint if it starves the recipe) and asserts on decode::smooth_uv_hits()/directional_uv_hits() firing plus pixel-exact vs ffmpeg`.

## Commit
`c806bd6` -- directional/smooth/paeth chroma in inter-frame intra blocks.
Nothing else uncommitted; `git status` clean.
