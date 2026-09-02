# lane-frame80 r1 HANDOFF — the rect intra `tx_depth` context used the key frame's grid inside inter frames

## Stream / repro
- Cut: `~/.cache/frame80-tmp/t120.obu` = `~/.cache/hg-0-10s.obu` (10-bit 3840x1608 10 s head)
  truncated to the first **120 frame-carrying OBUs** with
  `python3 <scratchpad>/census4/trunc.py <in> 120 t120.obu` (445416 B,
  sha256 `045d81a9691540220292663cecbc9a2eeb38db6624a714d3d0150544590a9128`, hashed twice, equal).
  120 OBUs = DECODE-order frames 0..80 (the OBU count is NOT the decode index: `show_existing_frame`
  headers count as frame OBUs; n=81 only reaches decode frame ~57).
- Every probe on this stream needs `EC_INTRA16X4_DECODE=1 EC_INTRA128_IN_INTER=1`; without them the
  full 10 s cut stops at decode frame 62 on the 16x4-in-inter refusal (main 243f125 re-added it),
  which is NOT the golomb wall.

## New instrument (committed)
- `decode.rs` `set_coeff_trace_frame` / `coeff_trace_on`: `EC_TRACE_COEFF_FRAME=<decode idx>` narrows
  every `EC_TRACE_COEFF` rung to one DECODE-order frame; unset = every frame (old behaviour).
  Also prints `EC_COEFF_FRAME decode_idx=N` per frame under `EC_TRACE_COEFF`, which is what makes the
  our-side trace splittable. Wired from `stream.rs` (`decode::set_coeff_trace_frame(pictures_decoded)`).
- Cross-decoder coefficient ladder recipe (`~/.cache/frame80-tmp/lad3.py`): aomdec's and our tag order
  differ inside a TU (ours prints `br` before `base_eob`, aomdec after; ours splits `sign`/`sign_rect`;
  aomdec prints `tag=tx_type` unconditionally for luma even when no symbol is read). Ladder ONLY on
  tags `{all_zero, eob, after_bases}` + rng. On that key our frames 1..62 align element-exactly inside
  aomdec's whole-stream trace by substring search of the frame's first 30 elements.

## First diverging element (this is NOT in frame 80 — the charter premise was stale)
DECODE frame **62** (not 80), frame-local ladder element 3981:
- Block **mi_row=108 mi_col=528**, an INTRA block inside an inter frame, luma 32x8 (BLOCK_32X8),
  luma TU TX_32X8 (aomdec `tx_size=16`), chroma TX_16X4; its above neighbour mi(106,528) is an INTER
  32x8 block whose var-tx tree split (txfm ctx 16 px wide).
- Symbol: the block's `tx_depth` (`read_selected_tx_size`, cat=2).
  aomdec `EC_ISTEP mi_row=108 mi_col=528 name=tx_depth val=0 ctx=2 cat=2 rng=60142`;
  ours computed **ctx=1** (`EC_TXCTX ... w=32 h=8 above_px=16 left_px=64 ctx=1`) and left the range at
  43516 where aomdec has 60142; the next luma `all_zero` then reads ctx=5/rng=41484 vs aomdec ctx=0/rng=60138.
- Entry ranges agree (both 49400 at the end of the previous block's chroma TU), so the divergence is
  exactly this one CDF-row choice.

## Root cause (fixed, committed)
`get_tx_size_context` (libaom `av1/common/pred_common.h:342`) overrides the `TXFM_CONTEXT` bands when a
neighbour is an INTER block: `above = block_size_wide[above_mbmi->bsize] >= max_tx_wide`
(mirror for left/height). Our `tx_size_context_rect` (the KEY frame's deblock-grid approximation) has no
such override and was being used for rect intra strips inside INTER frames; `tx_size_context_txfm_rect`
(which has the override) was reachable only from `decode_intra_rect_in_inter`.
Fix, at the two single points every caller routes through (`crates/ec-av1/src/decode.rs`):
- `tx_size_context_rect` → delegates to `tx_size_context_txfm_rect` when `TX_SELECT_INTER` is set
  (covers `decode_block_rect`, `decode_leaf_rect`, `decode_block_rect4`, `decode_rect4_16_strip`).
- `tx_size_context` (square, used by `read_tx_size(.., None)`) → delegates to `tx_size_context_txfm`.
- `decode_key_frame_tile_with_cdfs` now clears `TX_SELECT_INTER` (it was set by the inter tile decoder
  only, so a key frame after an inter frame on the same thread inherited it).

## State after the fix (measured)
`EC_INTRA16X4_DECODE=1 EC_INTRA128_IN_INTER=1 EC_FRAMES=1 EC_GOLOMB=1 decode_probe ~/.cache/hg-0-10s.obu`
→ decode-order frames 0..**84** decode (was 0..79); the wall moves to decode frame 85, still the
"a Golomb tail longer than this decoder reads" refusal (`EC_GOLOMB_LONG w=32 h=32 pos=353 base=15 length=21`),
i.e. still our own desync, now 5 frames later.

## Exact next step
1. Re-cut for decode frame 85 (bump the OBU count until `EC_FRAME_OK` shows 85 frames, ~n=127) and
   re-run the ladder: `python3 ~/.cache/frame80-tmp/lad3.py <aom_start_idx> <frame>` after locating the
   frame's start with the first-30-elements substring search (see lad.py/lad2.py in the same dir).
   Sweep frames 63..85 the same way — several of them failed head-alignment before the fix; re-check
   which still do.
2. NOT DONE (owner: next round): pixel compare of the fixed frames vs ffmpeg with
   `<scratchpad>/census7/streamcmp.py`; frame 57's two tx_depth divergences; the witness fixture
   `crates/ec-av1/fixtures/hg_head_frame80_witness.obu` + its gate; `cargo test -p ec-av1 --lib` suite.
   The fix is entropy-verified on the ladder and by the 5-frame wall move only.
