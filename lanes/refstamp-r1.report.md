# lane-refstamp r1 — `ref_stamp` vs forward key frames, `av1_copy_frame_mvs` clear, scripted MFMV rungs

Base: lane-inter4 `49af875` (contains lane-interbis's motion-field fix). Commits `34ae093`, `b03a4d2`.

## What changed

- `crates/ec-av1/src/motion_field.rs:270` — `MotionField` gains `is_intra` (the DPB slot's
  `frame_type`); `motion_field_projection` now opens with libaom's own first `return 0`
  (`start_frame_buf->frame_type == KEY_FRAME || INTRA_ONLY_FRAME`) before the frame-size test.
  Previously a forward reference holding a key frame was "projected" (empty field) and reported
  success, so it consumed the `ref_stamp` slot that ALTREF/LAST2 should have had.
- `crates/ec-av1/src/motion_field.rs:335` — `setup_motion_field` ports
  `if (!order_hint_info->enable_order_hint) return;` (empty `TplField` when `order_hint_bits == 0`)
  and counts the frames that take the intra-forward-ref `ref_stamp` path
  (`refstamp_intra_frames() -> (>=1, >=2)`).
- `crates/ec-av1/src/motion_field.rs:296` — `MotionField::set` now takes `Option<SavedMv>`;
  `crates/ec-av1/src/decode.rs:1772` `build_motion_field` ports `av1_copy_frame_mvs`'s leading
  CLEAR (`mv->ref_frame = NONE_FRAME; mv->mv.as_int = 0;`), which runs for every block of an
  inter frame — intra blocks, and inter blocks whose every slot is other-side/over-REFMVS_LIMIT,
  now wipe their 8x8 cells instead of `continue`ing. Latent until sub-8x8 leaves (only they share
  a cell), ported now.
- `crates/ec-av1/src/stream.rs:614` — the key-frame DPB slot is constructed with `is_intra: true`.
- `scripts/instrument-aom-oracle.sh` — new **rung 14**: `EC_TRACE_TPL` (`EC_TPL ... mfmv0=(..,..)
  rfo=..` / `INVALID` in `add_tpl_ref_mv`) and the per-entry `EC_STACK` dump under rung 4's
  `EC_TRACE_MODE`. Both existed only as hand edits in `~/.cache/aom-oracle/src` (lane-interbis)
  and would have vanished on the next oracle rebuild.

## Gate

`a_real_aomenc_inter_sequence_with_forward_keyframes_and_temporal_mvs_decodes_pixel_exact`
(`crates/ec-av1/src/stream.rs:4900`): 192x128, 24 frames of translating structure,
`--enable-fwd-kf=1 --fwd-kf-dist=8 --lag-in-frames=16 --auto-alt-ref=1 --arnr-maxframes=0
--enable-ref-frame-mvs=1 --enable-order-hint=1` (overrides last, aomenc keeps the last flag),
2 attempts (cq 34 / 45), 8-bit AND 10-bit, every decoded frame — hidden alt-refs included —
compared to the oracle IN DECODE ORDER via `decode_all_frames_vs_oracle`. A decode error is only
tolerated as a named `unsupported` refusal and is counted, never SKIPped; three hard asserts
(`compared > 0`, temporal candidates `> 0`, key/intra-forward-ref frames `> 0`).

Run: `EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-refstamp
cargo test -p ec-av1 --lib forward_keyframes -- --nocapture`

EVIDENCE: /home/tahinli/.cache/refstamp-suite.log | aomenc fwd-kf recipe, 2 attempts x 2 depths, decode-order compare incl. hidden frames | 8-bit: 26 frames (2 hidden) pixel-exact, temporal candidates=524, key/intra-forward-ref frames=6; 10-bit: 26 frames (2 hidden) pixel-exact, candidates=514, frames=6; attempt 0 (cq 34) refuses by name ("an intra-coded HORZ/VERT strip needs rectangular intra prediction")

MUTATION: removing the `if start.is_intra { return false; }` early return (pre-fix behaviour) makes
the gate FAIL, not skip:
EVIDENCE: /tmp/ec-av1-a_real_aomenc_inter_sequence_with_forward_keyframes_and_temporal_mvs_decodes_pixel_exact-8bit-attempt1-703199/in.obu | mutate motion_field.rs early return to a no-op, rerun the gate | "decode-order frame 15 of 26 (24 shown, 2 hidden) differs from the oracle at byte 6686 (ours 179 vs 178), 160 bytes differ"

Rung 14 verification: applied to a scratch tree (pristine `mvref_common.c` from the oracle's git
HEAD + a rung-13-state `decodemv.c`) and byte-compared to the live hand-patched oracle sources.
EVIDENCE: scratchpad/rung14/av1/{common/mvref_common.c,decoder/decodemv.c} | `SRC=<scratch> bash rung14.sh`, then `diff` vs ~/.cache/aom-oracle/src | both files IDENTICAL to the live oracle; re-running on the live oracle prints "already applied (no-op)" twice (idempotent); `bash -n` clean

## Refusals

None lifted this round (no refusal guarded this path); `refusal_inventory` / `gate_coverage`
unchanged and green in the suite below.

## Suite

SUITE_PLACEHOLDER

## Residue

- The gate's frames all have exactly ONE key/intra forward reference (`two or more: 0`) — the
  charter's ">=2 forward refs are key/intra" phrasing is not what `--enable-fwd-kf` produces at
  `--fwd-kf-dist=8`; one is already sufficient to move `ref_stamp` (it changes whether LAST2 and
  ALTREF are projected), and the mutation proves the gate is sensitive to exactly that.
  accepted.
- `--enable-fwd-kf=1` at cq 34 still stops at the intra HORZ/VERT rect-strip refusal (attempt 0);
  cq 45 decodes fully. deferred(lane owning rectangular intra prediction) — not this lane's path.
- The `av1_copy_frame_mvs` clear is unexercised by any gate today (needs a sub-8x8 inter leaf,
  which still refuses by name). deferred(the sub8 lane's 4x4 inter leaves) — ported now because
  the same block that lifts that refusal would otherwise silently inherit the old behaviour.
