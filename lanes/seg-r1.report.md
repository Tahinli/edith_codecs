# lane-seg r1 — per-block `segment_id` and its consumers

Branch `lane-seg`, off main `3808cf8` (WIP snapshot `51581d3` continued, not restarted).

## What changed

- `crates/ec-av1/src/cdf.rs:564` — `SEGMENT_ID` (`default_spatial_pred_seg_tree_cdf`, 3 ctx x 8 symbols)
  and `SEGMENT_PRED` (`seg_id_predicted`, 3 ctx) default CDFs.
- `crates/ec-av1/src/cdf_state.rs:467` — both tables in `Cdfs`, in `reset_counters` (class
  `cdf-counter-not-reset`) and in the default-table constructor, so they adapt and reset like every
  other per-tile table.
- `crates/ec-av1/src/decode.rs:319-655` — the reader: `set_segmentation` (per-frame install +
  `load_previous_segment_ids`), `CurrentSegmentIds`/`PrevSegmentIds` maps, `AboveSegPredContext`/
  `LeftSegPredContext`, `av1_neg_deinterleave`, `av1_get_spatial_seg_pred` (ctx + prediction),
  `read_segment_id`, `intra_segment_id` (spec 5.11.7) and `inter_segment_id` (5.11.9, both
  pre-skip and post-skip positions, `seg_id_predicted` under `temporal_update`, inherited map when
  `update_map == 0`), `set_segment_id` stamping the block's whole mi footprint.
- Call sites: `read_intra_mode` (decode.rs:4375) and `read_intra_mode_rect` (3332) read
  `intra_segment_id` before `skip` when `SegIdPreSkip`, after it otherwise;
  `decode_inter_block` (9250/9268) and `decode_inter_block8` (11106/11116) read both spec positions.
- Consumers: `block_q_idx()` (decode.rs:400, spec 7.12.2 `get_qindex` with `SEG_LVL_ALT_Q`) replaces
  the raw `CURRENT_Q_IDX` read at all 10 dequant sites; `lf_level` (decode.rs:6168) takes the block's
  segment id and applies `SEG_LVL_ALT_LF_Y_V/H`/`_U`/`_V` (spec 7.14.4) between the `DeltaLF` term and
  the ref/mode deltas; `edge_params` passes the per-side segment ids.
- `crates/ec-av1/src/stream.rs:266-291` — frame-level: narrowed refusal (below), spec
  `load_previous_segment_ids` from the primary reference's slot, `set_segmentation` per frame;
  `stream.rs:736` stores the decoded map into every slot `refresh_frame_flags` names.
- `crates/ec-av1/src/refusal_inventory.rs:59` — refusal replaced.

## Refusal

LIFTED: `"a frame with segmentation enabled (this decoder never reads a per-block segment_id symbol)"`.
REPLACED BY (narrower): `"a frame whose segmentation enables SEG_LVL_REF_FRAME/SKIP/GLOBALMV (this
decoder reads segment_id but never lets a segment override a block's reference, skip or mode)"` —
those three features rewrite mode/reference decisions and suppress symbols `decode_inter_block` reads
unconditionally; aomenc's `--aq-mode` only ever sets `SEG_LVL_ALT_Q`, so no gate here can exercise
them. `gate_coverage.rs` needed no edit: it derives coverage from `--enable-*` flags and segmentation
is driven by `--aq-mode`, not an `--enable-*` flag; its two tests stay green.

## Gates (both hard-assert; no decode error or mismatch becomes SKIP)

`stream.rs:8698 run_segmentation_gate` — mandelbrot fixture (varying local contrast so AQ has
something to segment), `--aq-mode=1 --deltaq-mode=0`, 10 attempts, pixel-exact vs ffmpeg over every
decoded frame including hidden alt-refs (`--auto-alt-ref=1 --lag-in-frames=16`), asserting
`segment_id_hits > 0`, `segment_ids_seen >= 2`, and — for multi-frame runs — `segment_pred_hits > 0`
(the `temporal_update` path).

Command: `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib aq_segmentation -- --nocapture`

EVIDENCE: crates/ec-av1/src/stream.rs:8908 a_real_aomenc_stream_with_variance_aq_segmentation_decodes_pixel_exact | aomenc --aq-mode=1 --deltaq-mode=0, 16 frames 128x64 8-bit, 10 seeds, decode_stream vs ffmpeg per plane | 10/10 pixel-exact, segment_id_hits=223, distinct segment ids=6, seg_id_predicted symbols=35
EVIDENCE: crates/ec-av1/src/stream.rs:8941 a_real_aomenc_10bit_stream_with_aq_segmentation_decodes_pixel_exact | same recipe, yuv420p10le, key frame only, 10 seeds | 10/10 pixel-exact, segment_id_hits=80, distinct segment ids=5
EVIDENCE: cargo test -p ec-av1 --lib (EC_AV1_REQUIRE_AOMENC=1) | full ec-av1 lib suite | 269 passed, 0 failed, 23 ignored, 494s
EVIDENCE: scratchpad troy.obu / hg.obu (ffmpeg -t 0.4 -c:v copy -f obu) | cargo run -p ec-av1 --example decode_probe | Troy stops at "a 32x32 partition type this decoder does not code (value=4)", Hunger Games at "a partition below 8x8" — neither stops on segmentation

## Residue

- deferred(sub-16x16 rect/AB partition lanes): an `--aq-mode=2` (complexity AQ) twin. libaom only
  enables segmentation for complexity AQ under `--end-usage=vbr` (`aq_complexity.c:is_sb_aq_enabled`,
  `sb64_target_rate >= 256`), and every VBR recipe tried makes aomenc pick partition shapes other
  lanes still refuse (AB below 16x16, SB-level HORZ/VERT, part32 value 4/6) — 10/10 attempts refuse
  before a pixel is compared. Coverage, not capability: complexity AQ sets exactly the same
  `SEG_LVL_ALT_Q`/`update_map` syntax variance AQ does. Comment at stream.rs:8919.
- deferred(a 10-bit inter-frame decode defect unrelated to this lane): the 10-bit gate is key-frame
  only. Measured: the same recipe with `--aq-mode=0` (segmentation entirely off) also mismatches at
  frame 2, so a 16-frame 10-bit variant would gate a pre-existing gap. Comment at stream.rs:8933.
- accepted: `SEG_LVL_REF_FRAME`/`SKIP`/`GLOBALMV` stay refused by name (see above) — no aomenc
  `--aq-mode` path writes them, so a gate cannot be built from this oracle today.
