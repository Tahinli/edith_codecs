# av1knobshold — who holds `speed::KNOBS`, and for how long

Base `main` = `7737e75d`. Lane `lane/av1knobshold`. Measured 2026-10-03 on the local PC: 12 cores, load average 0.92 at the start of the run, so no competing build inflated these times.

The lock is untouched. This lane measures; it edits nothing. `speed.rs` unchanged, no test shortened, no `#[ignore]` added.

## Command

```bash
git worktree add <path> -b lane/av1knobshold 7737e75d
cd <path>
RUSTC_WRAPPER= TMPDIR=$HOME/.cache/tmp CARGO_TARGET_DIR=$HOME/.cache/ct-knobshold \
  cargo test -p ec-av1 --no-run

# once per holder test, one named test per invocation:
timeout 420 $CARGO_TARGET_DIR/debug/deps/ec_av1-79bef263d07a873f \
  --exact encoder::tests::<test> --test-threads=1 --quiet
```

`RUSTC_WRAPPER=` is load-bearing on this box: with sccache on, the build dies `Disk quota exceeded (os error 122)` because the `/tmp` tmpfs is at 80% (13 G of 16 G). Skill `local-edquot-rust-build-tests` has the full workaround.

One named test per invocation, so a slow test is never cut short while another is being measured. Per-test ceiling 420 s, global cap 1500 s. **No test reached either ceiling** — all 28 running holders produced a wall time, so nothing is recorded as `>cap`.

**Total across the 28 running holders: 1634 s (27 min).** The run overran the 25-minute cap overall (1634 s) because the cap gates *starting* a new test, not finishing one: the last test began at 1406 s and ran 228 s to completion. The alternative — killing it mid-measurement — would have produced a number that means nothing.

A note on paths: measurement ran from a worktree under `/tmp`; the report itself was committed from `$HOME/.cache/wt-knobshold` after `/tmp` hit its user quota and refused new writes. Same commit `7737e75d`, same test binary hash, so the numbers stand.

## 1. Every `KNOBS` holder

`knob_write()` — **53 tests** take the exclusive lock, one `let _knobs = crate::speed::knob_write();` statement each, held for the whole test body: **28 run** in a default suite, **25 are `#[ignore]`d** and contribute nothing to a default run.

`lock_gate_counters()` — a different lock (`GATE_COUNTER_LOCK`, a plain `Mutex`), so it does not convoy with `KNOBS` on its own. **19 `encoder::tests` take it**; **11 of the running `KNOBS` holders take BOTH locks at once** (section 4). In `stream.rs` there are 196 `lock_gate_counters()` sites resolving to **181 real tests plus 15 helper-fn sites** (`read_pin`, `decode_all_frames_vs_oracle`, the bit-depth and counter helpers), so some stream tests hold it transitively.

## 2. Ranked by wall time — the 28 running `knob_write` holders

| # | seconds | >10s | test |
|---|---|---|---|
| 1 | 418.11 | **YES** | `encoder::tests::the_future_tpl_window_still_codes_every_picture_and_moves_the_stream` |
| 2 | 228.46 | **YES** | `encoder::tests::filter_stage_bytes_do_not_depend_on_the_thread_count` |
| 3 | 212.31 | **YES** | `encoder::tests::the_lookahead_holds_one_group_and_still_codes_every_picture` |
| 4 | 156.76 | **YES** | `encoder::tests::tile_bytes_do_not_depend_on_the_thread_count` |
| 5 | 109.62 | **YES** | `encoder::tests::a_moving_detail_clip_codes_two_delta_q_levels_both_decoders_read_exactly` |
| 6 | 84.05 | **YES** | `encode::tests::the_probe_arms_the_gates_source` |
| 7 | 77.54 | **YES** | `encoder::tests::the_arf_leaf_offer_set_fires_moves_the_stream_and_decodes_sample_exact` |
| 8 | 50.27 | **YES** | `encoder::tests::an_inter_clip_codes_both_inter_set_tx_types_both_decoders_read_exactly` |
| 9 | 45.16 | **YES** | `encode::tests::the_final_filter_replay_matches_the_capture_decode_at_odd_sizes` |
| 10 | 41.73 | **YES** | `encoder::tests::a_crossfade_clip_codes_a_compound_64x64_root_and_decodes_sample_exact` |
| 11 | 26.18 | **YES** | `encoder::tests::a_1080p_shaped_clip_straddles_at_every_block_size_and_decodes_sample_exact` |
| 12 | 21.43 | **YES** | `encoder::tests::a_leaf_predicts_off_last2_when_the_picture_two_back_matches` |
| 13 | 19.26 | **YES** | `encoder::tests::a_pyramid_stream_decodes_in_display_order_through_both_decoders` |
| 14 | 18.47 | **YES** | `encoder::tests::a_high_frequency_clip_splits_the_64x64_roots_luma_and_decodes_sample_exact` |
| 15 | 18.25 | **YES** | `encoder::tests::a_three_level_pyramid_stream_decodes_in_display_order` |
| 16 | 17.25 | **YES** | `encode::tests::a_repeated_pattern_key_frame_codes_intrabc_blocks_ffmpeg_decodes_exactly` |
| 17 | 15.94 | **YES** | `encoder::tests::a_four_level_pyramid_stream_decodes_in_display_order` |
| 18 | 15.86 | **YES** | `encoder::tests::an_edge_clip_codes_every_reduced_set_tx_type_both_decoders_read_exactly` |
| 19 | 10.15 | **YES** | `encode::tests::each_frame_interpolation_filter_codes_its_own_stream_ffmpeg_decodes_exactly` |
| 20 | 10.02 | **YES** | `encode::tests::every_mode_decodes_to_what_the_encoder_predicted` |
| 21 | 9.69 |  | `encoder::tests::a_dc_stepped_clip_codes_a_64x64_residual_and_decodes_sample_exact` |
| 22 | 7.24 |  | `encoder::tests::a_non_superblock_aligned_clip_codes_edge_64x64_roots_and_decodes_sample_exact` |
| 23 | 7.02 |  | `encode::tests::a_high_precision_frame_codes_eighth_pel_motion_vectors_both_decoders_read` |
| 24 | 6.58 |  | `encode::tests::the_search_picks_the_diagonal_the_picture_runs` |
| 25 | 6.15 |  | `encoder::tests::a_static_clip_codes_64x64_roots_and_decodes_sample_exact` |
| 26 | 0.15 |  | `encode::tests::the_search_picks_the_direction_the_picture_runs` |
| 27 | 0.13 |  | `encode::tests::a_nearestmv_block_with_one_nonzero_coefficient_decodes_clean` |
| 28 | 0.05 |  | `encode::tests::the_encoders_own_streams_are_byte_identical_to_their_pins` |

**20 of 28 are over 10 s.** The top four (1016 s) are 62% of all KNOBS-held time. The previous lane saw only the ~50 s holder; it is rank 8, not the worst.

### Named, over 10 s

- **418.1s** `encoder::tests::the_future_tpl_window_still_codes_every_picture_and_moves_the_stream`
- **228.5s** `encoder::tests::filter_stage_bytes_do_not_depend_on_the_thread_count`
- **212.3s** `encoder::tests::the_lookahead_holds_one_group_and_still_codes_every_picture`
- **156.8s** `encoder::tests::tile_bytes_do_not_depend_on_the_thread_count`
- **109.6s** `encoder::tests::a_moving_detail_clip_codes_two_delta_q_levels_both_decoders_read_exactly`
- **84.0s** `encode::tests::the_probe_arms_the_gates_source`
- **77.5s** `encoder::tests::the_arf_leaf_offer_set_fires_moves_the_stream_and_decodes_sample_exact`
- **50.3s** `encoder::tests::an_inter_clip_codes_both_inter_set_tx_types_both_decoders_read_exactly`
- **45.2s** `encode::tests::the_final_filter_replay_matches_the_capture_decode_at_odd_sizes`
- **41.7s** `encoder::tests::a_crossfade_clip_codes_a_compound_64x64_root_and_decodes_sample_exact`
- **26.2s** `encoder::tests::a_1080p_shaped_clip_straddles_at_every_block_size_and_decodes_sample_exact`
- **21.4s** `encoder::tests::a_leaf_predicts_off_last2_when_the_picture_two_back_matches`
- **19.3s** `encoder::tests::a_pyramid_stream_decodes_in_display_order_through_both_decoders`
- **18.5s** `encoder::tests::a_high_frequency_clip_splits_the_64x64_roots_luma_and_decodes_sample_exact`
- **18.2s** `encoder::tests::a_three_level_pyramid_stream_decodes_in_display_order`
- **17.2s** `encode::tests::a_repeated_pattern_key_frame_codes_intrabc_blocks_ffmpeg_decodes_exactly`
- **15.9s** `encoder::tests::a_four_level_pyramid_stream_decodes_in_display_order`
- **15.9s** `encoder::tests::an_edge_clip_codes_every_reduced_set_tx_type_both_decoders_read_exactly`
- **10.2s** `encode::tests::each_frame_interpolation_filter_codes_its_own_stream_ffmpeg_decodes_exactly`
- **10.0s** `encode::tests::every_mode_decodes_to_what_the_encoder_predicted`

`an_inter_clip_codes_both_inter_set_tx_types_both_decoders_read_exactly` measured **50.27 s**, which confirms the ~50 s figure in `lanes/av1intertxwedge.report.md` to two decimals.

## 3. Reading the ranking

- The convoy is real and larger than the report that triggered it: ~27 minutes of exclusive `KNOBS` time per default suite, concentrated in 4 tests.
- `filter_stage_bytes_do_not_depend_on_the_thread_count` (228 s) and `tile_bytes_do_not_depend_on_the_thread_count` (157 s) are throughput probes — measuring elapsed time is their purpose. They are not shortenable without changing what they prove.
- `the_future_tpl_window_...` (418 s) and `the_lookahead_holds_one_group_...` (212 s) are the two real holds: both pin speed 0 over multi-picture fixtures, which is the encode itself, not a probe.
- 25 holders are already `#[ignore]`d — the whole `bd_rate_*`, `*_census`, `probe_*` and `*_wall_*` families. A suite that runs with `--ignored` pays for all of them; one that does not pays only the 28 above.

## 4. The overlap: tests holding both locks

| seconds | test |
|---|---|
| 228.46 | `encoder::tests::filter_stage_bytes_do_not_depend_on_the_thread_count` |
| 156.76 | `encoder::tests::tile_bytes_do_not_depend_on_the_thread_count` |
| 109.62 | `encoder::tests::a_moving_detail_clip_codes_two_delta_q_levels_both_decoders_read_exactly` |
| 50.27 | `encoder::tests::an_inter_clip_codes_both_inter_set_tx_types_both_decoders_read_exactly` |
| 41.73 | `encoder::tests::a_crossfade_clip_codes_a_compound_64x64_root_and_decodes_sample_exact` |
| 26.18 | `encoder::tests::a_1080p_shaped_clip_straddles_at_every_block_size_and_decodes_sample_exact` |
| 18.47 | `encoder::tests::a_high_frequency_clip_splits_the_64x64_roots_luma_and_decodes_sample_exact` |
| 15.86 | `encoder::tests::an_edge_clip_codes_every_reduced_set_tx_type_both_decoders_read_exactly` |
| 9.69 | `encoder::tests::a_dc_stepped_clip_codes_a_64x64_residual_and_decodes_sample_exact` |
| 7.24 | `encoder::tests::a_non_superblock_aligned_clip_codes_edge_64x64_roots_and_decodes_sample_exact` |
| 6.15 | `encoder::tests::a_static_clip_codes_64x64_roots_and_decodes_sample_exact` |

Five more hold both but are `#[ignore]`d: `a_non_screen_inter_clip_codes_a_one_d_type_both_decoders_read_every_plane_exactly`, `a_static_clip_codes_128x128_superblock_roots_and_decodes_sample_exact`, `a_wide_tx_set_clip_codes_the_new_alphabets_both_decoders_read_exactly`, `every_speed_preset_decodes_sample_exact_through_both_decoders`, `tile_wall_table_at_1080p`.

## 5. Recommendation (not implemented here)

Do not change the lock's scope. If contention needs to come down, the lever is a per-arm replacement for `knob_write` — a setter that touches one knob without moving the streams every other test pins — which is its own lane with its own proof obligation. Nothing in this measurement justifies a timeout, an `#[ignore]`, or a narrowed fixture.

## Appendix — the 25 `#[ignore]`d `knob_write` holders

Not measured: they do not run in a default suite. Listed so the census of all 53 holders is complete.

- `encode::tests::a_128_root_ab_shapes_decode_exact_through_both_decoders` · `encode::tests::a_128_root_block_with_a_real_residual_decodes_exact_through_both_decoders` · `encode::tests::a_128_root_compound_block_decodes_exact_through_both_decoders` · `encode::tests::a_128_root_horz_and_vert_halves_decode_exact_through_both_decoders`
- `encode::tests::a_128_root_residual_block_under_a_per_unit_cdef_list_decodes_exact` · `encode::tests::a_128_superblock_clip_whose_root_search_codes_128x128_blocks_decodes_exact` · `encode::tests::a_rect128_half_with_a_real_residual_decodes_exact_through_both_decoders` · `encode::tests::a_rect128_residual_half_under_a_per_unit_cdef_list_decodes_exact`
- `encode::tests::an_ab128_piece_with_a_real_residual_decodes_exact_through_both_decoders` · `encode::tests::bd_rate_film_long_gop` · `encode::tests::bd_rate_vs_libaom_and_rav1e` · `encode::tests::pricer_error_census_on_clips`
- `encode::tests::probe_directional` · `encode::tests::probe_intrabc_key_frame` · `encode::tests::probe_parity_point` · `encoder::tests::a_non_screen_inter_clip_codes_a_one_d_type_both_decoders_read_every_plane_exactly`
- `encoder::tests::a_static_clip_codes_128x128_superblock_roots_and_decodes_sample_exact` · `encoder::tests::a_wide_tx_set_clip_codes_the_new_alphabets_both_decoders_read_exactly` · `encoder::tests::every_speed_preset_decodes_sample_exact_through_both_decoders` · `encoder::tests::filter_stage_wall_1080p`
- `encoder::tests::filter_stage_wall_4k` · `encoder::tests::filter_stage_wall_film` · `encoder::tests::tile_search_wall_1080p` · `encoder::tests::tile_search_wall_4k`
- `encoder::tests::tile_wall_table_at_1080p`
