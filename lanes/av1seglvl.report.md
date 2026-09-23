# lane-av1-seglvl — the segmentation mode-override refusal: census, the map-inheritance fix, and the reaching stream

Base: `main` **73f3ac64** (2026-09-24). Worktree `edith_codecs-av1seg`, branch
`lane-av1-seglvl`, lane commit **5a96bff9**,
`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1seg` (private). No push.
Region: `decode.rs`'s segment-id/map block (`inter_segment_id`), the
`stream.rs` segmentation refusal + census gates, and the inventory entry that
pins them. Sibling lanes' block-decoder regions untouched.

## TL;DR

- **Census (re-scoped):** over 11 real-aomenc recipes — 1-pass, alt-ref-lag
  (`--lag-in-frames=16 --auto-alt-ref=1`), and a **genuine two-pass** encode,
  `--aq-mode=1/2/3`, cq 10/40/45/63, screen (`testsrc2`) and film
  (`mandelbrot`) sources — **every frame header parsed**: aomenc enables
  `SEG_LVL_ALT_Q` and nothing else. 162 seg-enabled frames in the lag/two-pass
  arms alone, zero instances of any other `SEG_LVL` feature. The refusal
  **stays, by name, unchanged** (`a frame whose segmentation enables
  SEG_LVL_REF_FRAME/SKIP/GLOBALMV ...`).
- **The "reachability is nil" claim survived the census but its old PROOF did
  not:** the stale comment cited only "CLI exposes no ROI/active-map flag".
  The full answer has three legs (§1), the load-bearing one being that
  `configure_static_seg_features` (`encoder_utils.c:315`) — the ONLY setter of
  `SEG_LVL_ALT_LF_*`/`REF_FRAME`/`SKIP` in the tree — is gated on
  `hl_sf.static_segmentation`, which libaom's own `speed_features.h` documents
  as **"Always set to 0"**, AND on the second pass (`encoder_utils.c:738`).
- **One real defect found and fixed** on the way: `update_map == 0` frames
  inherited the previous segment map through the MINIFIED block id, flattening
  heterogeneous footprints. libaom (`copy_segment_id`, `decodemv.c:342`)
  copies the previous map **verbatim** and hands the block only the min. Fixed
  bug-for-bug; witness stream now byte-exact (§2/§3).

Inventory delta: none (strings unchanged); the PROVEN entry's comment now
carries the corrected evidence and the new witness gate name.

## 1. Census: which SEG_LVL features does aomenc actually emit?

Instrument: not the refusal — the refusal is blind to a pure-`ALT_LF` stream,
which would decode silently past it. `segmentation_feature_census` (new,
`stream.rs`) parses every frame OBU and tallies `feature_enabled` per
segment-feature instance, alongside the shape counters the premise asserts on
(`update_map == 1` map-coding frames, `update_map == 0` map-inheriting
frames, `ALT_Q`-carrying frames).

Arms (`a_segmentation_census_over_real_aq_streams_finds_no_mode_overriding_feature`,
re-scoped in place):

- r27's six 1-pass no-lag recipes (kept verbatim: baseline, `--aq-mode=1/2/3`,
  `--aq-mode=3 cq10`, `--deltaq-mode=1` over mandelbrot 192x128);
- four alt-ref-lag arms: `--aq-mode=1 cq45` over mandelbrot AND testsrc2
  256x192, `--aq-mode=1 cq63` (the `high_q = avg_q > 48` regime of the
  static-seg branch), `--aq-mode=2 cq45` — 40 frames each;
- one **two-pass** arm (`--passes=2 --pass=1/--pass=2 --fpf=...`,
  `--aq-mode=1 cq45 lag16`): `configure_static_seg_features` runs ONLY at
  `is_stat_consumption_stage_twopass`, so this arm covers the encoder surface
  that would expose the branch if its speed feature were ever revived.

Verdict: `SEG_LVL_ALT_Q` only — e.g. `aq-mode=1 lag16 mandelbrot`: 40/40
frames seg-enabled, 7 map-coding, 33 inheriting, 12 with `ALT_Q` tables,
per-feature `[96, 0, 0, 0, 0, 0, 0, 0]`; twopass arm `[80, 0, ...]`. All
non-`ALT_Q` counts asserted zero.

Source-side proof that the other seven features have no CLI path
(`stream.rs` refusal comment + inventory, all sites read in the oracle tree
`~/.cache/aom-oracle/src`, libaom v3.13.3):

1. AQ modules clear the table then set `SEG_LVL_ALT_Q` only
   (`aq_variance.c:61` clear + `:89` enable, `aq_complexity.c:74` + `:118`,
   `aq_cyclicrefresh.c:616` + `:630`).
2. `configure_static_seg_features` (`encoder_utils.c:315`) — sets seg-1
   `ALT_Q + ALT_LF_*(-2)` on arfs and `REF_FRAME(ALTREF)` (+`SKIP` when
   `high_q`) on `is_src_frame_alt_ref` overlays — runs only under
   `is_stat_consumption_stage_twopass(cpi) && cpi->sf.hl_sf.static_segmentation`
   (`encoder_utils.c:738-740`); `static_segmentation` has **zero assignments
   in the tree** and is documented "Always set to 0" (`speed_features.h:436`).
   Dead code in v3.13.3, upstream-confirmed.
3. `av1_apply_roi_map` / `av1_apply_active_map` (the `REF_FRAME`/`SKIP`
   library paths) are control-API only; `aomenc.c` exposes no flag for either
   (checked this round).

`SEG_LVL_GLOBALMV` additionally has **no setter anywhere in the encoder** —
it stays refused with no caveat.

## 2. The defect: map inheritance flattened mixed footprints

The census's richest arm (`--aq-mode=1 lag16`) decoded 2 luma bytes wrong on
film content: mandelbrot 192x128, 40 frames, **frame 35, Y px (75,81) and
(74,86)** — one 8x8 block at mi (18..19, 20..21), everything else exact vs
ffmpeg. Stage bisect (`EC_AV1_PREFILT_DUMP`): the divergence exists at
RECONSTRUCTION — prediction/dequant, not a filter decision. The paired
`EC_TRACE_SEG` ladders (ours and the oracle's instrumented aomdec, 386 coded
ids) were **line-identical** — every CODED id matched. The divergence had to
be in the ids nobody codes: the `update_map == 0` inheritance.

libaom (`decodemv.c:386-390`), on such a block:

- `copy_segment_id` (`:342`) **memcpys** the previous map over the block's own
  footprint — mixed ids intact;
- the id the BLOCK carries is `get_predicted_segment_id` = **min** over that
  footprint (`dec_get_segment_id`, `:307`).

Our port did one step: `write_segment_id(min)`, stamping the min over the
footprint. Any block wider than 1 mi sitting on a heterogeneous patch of the
previous map (exactly what an arf full of 1:4 splits leaves) flattened the
saved map; every later inheriting frame or temporal prediction that read that
position got the wrong segment id → wrong `SEG_LVL_ALT_Q` delta → wrong
dequant. 2 pixels, frames away from the cause (class
`simplified-inheritance`).

Fix (`decode.rs`, lane commit 5a96bff9): `copy_segment_ids` — the verbatim
per-cell copy — plus the min only as the block's own id; map writes on the
coded/temporal paths are untouched (`set_segment_id` semantics, already
correct there). `av1_get_qindex` parity re-verified while there
(`quant_common.c:222`: `clamp(base + data, 0, MAXQ)` — our `block_q_idx`
identical; libaom applies it per block in `parse_decode_block`, values equal
at every dequant site).

## 3. Proof

- **Fail-pre-fix (single mutation, scratch archive of 5a96bff9, only the
  `copy_segment_ids` call reverted to the min-stamp):**
  `a_real_aomenc_segmentation_stream_with_map_inheritance_decodes_pixel_exact`
  FAILS with `film mandelbrot: decode-order frame 35 of 40 (40 shown, 0
  hidden) differs from the oracle at byte 15627 (ours 132 vs 43), 2 bytes
  differ` — the gate reproduces the exact defect unaided.
- **Post-fix:** same gate green on both arms — screen testsrc2: 41 frames
  decode-order byte-exact vs the instrumented oracle (1 hidden), 40 shown
  frames pixel-exact vs ffmpeg, q span 48..189, 308 segment symbols; film
  mandelbrot: 40/40 exact, q span 60..202, 386 symbols. Premise asserts:
  `enabled > 0 && umap > 0`, `inherited > 0`, `alt_q > 0`,
  `segment_id_hits > 0`, `block_q_span.hi > lo`.
- Determinism: the lag stream re-encodes byte-identical across runs
  (`mand_r1.obu == mand_r2.obu`, sha256 `76ece5d2...`); screen arm sha256
  `2b0548e9...`.

## 4. Gates

- **NEW witness:** `a_real_aomenc_segmentation_stream_with_map_inheritance_decodes_pixel_exact`
  (the reaching stream: segmentation on every frame + real alt-ref groups +
  33-34 inheriting frames per arm; every decode-order frame vs oracle,
  every shown frame vs ffmpeg).
- **RE-SCOPED census:** `a_segmentation_census_over_real_aq_streams_finds_no_mode_overriding_feature`
  — r27's six arms kept, four lag arms + one two-pass arm added, instrument
  upgraded from refusal-fires to header-parse tally, verdict asserted per
  feature.
- **KEPT unchanged:** `a_frame_whose_segmentation_overrides_a_block_mode_is_refused_by_name`
  (the hand-written header sweep over all three features x segments 0/3/7 —
  still the proof the refusal fires on conformant streams).
- **Regression arms run green** post-fix: `a_real_libaom_monochrome_key_frame_decodes_pixel_exact`
  (mono), `a_10bit_key_frame_with_skipped_8x8_intra_leaves_that_split_their_transform_decodes_luma_exact`
  + `a_real_film_key_frame_with_a_skipped_cfl_block_decodes_pixel_exact`
  (hg_*/troy 10-bit fixtures), `a_non_420_subsampled_sequence_header_is_refused_by_name`
  (444 refused), `a_frame_using_quantisation_matrices_is_refused_by_name`
  (qmatrix refused), `a_real_aomenc_stream_with_cdf_update_disabled_decodes_pixel_exact`
  + `a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact`
  (cdf exact), `a_coded_rect_intrabc_block_reconstructs_in_both_orientations`
  + `an_sb128_screen_stream_with_intrabc_decodes_pixel_exact` (the cq50/cq45
  screen families), `a_real_aomenc_mixed_lossless_segment_frame_is_refused_by_name`
  (aq-mode cq0 refusal, still fires).

## 5. Suite + deferred

- Full suite (`cargo test -p ec-av1 --lib -- --test-threads=1`,
  EC_AV1_REQUIRE_AOMENC/FFMPEG=1) runs on the VPS per the batch standing
  order, staged by Main from this commit (5a96bff9, tree clean except the
  gitignored `assets` symlink); the literal final line is recorded in the
  merge ticket. The batch's targeted regression arms (§4) all ran green
  locally, as did the three lane gates and the fail-pre-fix run (§3).
- `deferred(aomenc has no recipe; unblock = an encoder that emits seg SKIP/REF on intra-only frames — needs a per-segment skip guard on the intra-frame path, where libaom's own read has a stale-segment-id wrinkle worth a look first)`: SEG_LVL_SKIP/REF on INTRA-only frames is not applied on our intra path. aomenc never emits it (features are cleared on key frames, `configure_static_seg_features:334-344`; ROI/active-map gate `!frame_is_intra_only`/`frame_is_intra_only`), and the stream-level refusal is being lifted for none of it — GLOBALMV frames still refuse wholesale. No stream in the suite can reach the shape.
- `deferred(no decoder-side effect found; unblock = a census arm that catches configure_static_seg_features going live — the two-pass arm + parse tally in the re-scoped census is that tripwire)`: the arf `ALT_LF(-2)` tables and overlay `REF_FRAME/SKIP` tables cannot reach a stream while `static_segmentation` stays 0. The deblocker's per-segment `ALT_LF` port (`decode.rs`, lane-seg) is in place but stays unwitnessed by a real stream for the same reason.
