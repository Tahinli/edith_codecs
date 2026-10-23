# testcut Phase 2: ec-av1 encoder tests (batched kill matrix)

Branch: testcut-p2-av1e (from main). Scope: the 38 heavy candidates in `lanes/testcut/p2-av1enc-tools/local/cands.tsv` (ec-av1 encode::/encoder:: and related heavy tests).

## Summary

- Tests deleted: **0**. Seconds saved: **0**.
- Mutants: 24 sampled (seed 20260123, stratified over 20 files, from 147,833 eligible mutants covered by heavy tests). 3 unviable (rustc E0277, `PaletteUv: Default`), 21 viable.
- Heavy kill matrix: 14 of 21 viable mutants killed by heavy tests; 7 survived all heavy tests.
- Fast-suite pass on the 14 heavy-killed mutants: 13 killed by the fast suite, **1 survivor** (`speed.rs:196`, replacement `1`), which is the only mutant with kill evidence against the candidates. The TIMEOUT rule affected **0** mutants (no fast-suite result was a TIMEOUT).
- Kill attribution for the survivor: `the_arf_leaf_offer_set_fires_moves_the_stream_and_decodes_sample_exact` and `encode::more_tests::the_encoders_own_streams_are_byte_identical_to_their_pins`. Greedy cover keeps one of them; 37 candidates have unique line coverage over fast + kept, so none is deletable.
- Gate `cargo test --release -p ec-av1` (fresh target dir): pass, rc=0 (817 lib tests, plus integration suites). `cargo clippy --release -p ec-av1 --tests -- -D warnings`: clean. `#![forbid(unsafe_code)]` present at `crates/ec-av1/src/lib.rs:18`.

## Method and deviations

- Coverage: per-test line-level regions (38 candidates + fast profile, 683 fast tests). Every candidate has coverage. The earlier FAST report failure was fixed in `cov-shard.sh` (report step now logs its errors); the 11 missing tests were redone.
- Sample: 24, not 200. Heavy-first runs cost about 40k serial test-seconds for the seed-20260123 sample (other seeds 59-60k), which the ~10 host-hour budget would not cover at 200 mutants.
- Mutant selection: `cargo mutants --re` (cargo-mutants 27.1.0) does not select one mutant here (106 matches for a single-mutant regex, JSON listing agrees). The runner is `matrix/mutrun.sh`, which applies each mutant with `scripts/mutate.py` in a scratch copy, runs the heavy covering tests (`--no-fail-fast`, 1200 s timeout) or the fast profile, and reverts with a cmp check.
- Validity checks: each run logs `mutate.py` applied diff, nextest result counts and rustc error counts (`*.evidence`). Unviable results are real rustc errors (see `matrix/summary/heavy/*.evidence`). Fails are real assertion panics: no fail log contains `library clip is absent`, `No such file` or runner errors.
- Baseline (unmutated, same scratch layout): heavy union 36/36 pass and fast profile 683/683 pass, so no test needed exclusion from kill attribution (`matrix/summary/base/`).
- Bugs found and fixed along the way (all in the lane, none in the tree): scratch copy dropped the top-level `fixtures/` symlink (tests panicked with a missing clip, so the first heavy results were invalid); a shared target dir reused a test binary built in another scratch path (`CARGO_MANIFEST_DIR` baked in); the first gate run used that stale target and failed with `AV tile` refusals, and passed in a fresh target dir.
- Coverage granularity: line-level, not column-level. U(T) and the subset check are therefore coarser than the protocol asks; this only makes the keep decision more conservative.

## Report rows

| crate | test | seconds | class | |U(T)| vs fast | |U(T)| vs fast+kept | |K'| | decision |
|---|---|---|---|---|---|---|---|
| ec-av1 | encoder::tests::bitrate_target_lands_within_5_percent_over_48_frames | 764.1 | candidate | 859 | 62 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::filter_stage_bytes_do_not_depend_on_the_thread_count | 679.9 | candidate | 981 | 231 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::geometry_mismatch_is_refused | 647.8 | candidate | 9 | 7 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::gop_cadence_is_honored | 609.1 | candidate | 1064 | 310 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::every_tile_layout_decodes_sample_exact_through_both_decoders | 180.4 | candidate | 1007 | 269 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::the_rate_loop_prices_the_frames_the_pyramid_codes | 169.5 | candidate | 1010 | 238 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::the_facade_codes_the_same_bytes_as_encode_sequence | 169.2 | candidate | 853 | 108 | 0 | keep (unique coverage) |
| ec-av1 | yuv422_matches_dav1d_byte_exact | 158.4 | candidate | 916 | 550 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::thirty_pictures_at_gop_fifteen_decode_to_two_key_frames | 135.6 | candidate | 1024 | 262 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders | 127.5 | candidate | 910 | 166 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::the_content_gate_keeps_screen_streams_flat | 114.9 | candidate | 1035 | 311 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::the_lookahead_holds_one_group_and_still_codes_every_picture | 95.7 | candidate | 988 | 205 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::bytes_per_frame_target_settles_within_20_percent | 93.7 | candidate | 875 | 73 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::the_future_tpl_window_still_codes_every_picture_and_moves_the_stream | 89.6 | candidate | 981 | 201 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::a_three_level_pyramid_stream_decodes_in_display_order | 83.3 | candidate | 1021 | 236 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::a_static_clip_codes_64x64_roots_and_decodes_sample_exact | 80.3 | candidate | 944 | 418 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::a_pyramid_stream_decodes_in_display_order_through_both_decoders | 79.6 | candidate | 1027 | 236 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::the_key_q_offset_reaches_the_key_frame | 78.8 | candidate | 521 | 163 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::tile_bytes_do_not_depend_on_the_thread_count | 68.9 | candidate | 982 | 228 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::bytes_per_frame_controller_never_oscillates_past_its_clamp | 67.8 | candidate | 886 | 75 | 0 | keep (unique coverage) |
| ec-av1 | encode::tests::the_search_beats_dc_alone | 63.4 | candidate | 493 | 139 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::filter_replay_final_matches_the_capture_decode | 47.5 | candidate | 870 | 141 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::one_in_one_out_delayed_by_the_lookahead_depth | 46.9 | candidate | 1087 | 350 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::filter_replay_codes_the_same_stream_as_a_full_decode | 42.2 | candidate | 968 | 232 | 0 | keep (unique coverage) |
| ec-av1 | encode::tests::a_640x360_half_straddle_round_trips_through_ffmpeg_across_three_frames | 38.1 | candidate | 490 | 115 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::the_arf_leaf_offer_set_fires_moves_the_stream_and_decodes_sample_exact | 35.3 | candidate | 949 | 205 | 0 | keep (unique coverage) |
| ec-av1 | encode::more_tests::the_probe_arms_the_gates_source | 33.4 | candidate | 870 | 205 | 0 | keep (unique coverage) |
| ec-av1 | encode::tests::a_640x360_half_straddle_sequence_round_trips_through_ffmpeg | 33.2 | candidate | 967 | 179 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::zero_gop_is_refused | 31.8 | candidate | 2 | 2 | 0 | keep (unique coverage) |
| ec-av1 | encode::more_tests::the_encoders_own_streams_are_byte_identical_to_their_pins | 28.9 | kept(cover) | 875 | 0 | 0 | keep |
| ec-av1 | decode::tests::odd_sizes_round_trip_bit_exact_against_the_encoder_reconstruction | 27.2 | candidate | 519 | 187 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::a_moving_detail_clip_codes_two_delta_q_levels_both_decoders_read_exactly | 25.2 | candidate | 1010 | 253 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::quality_target_is_monotone_in_bytes_and_psnr | 22.9 | candidate | 871 | 68 | 0 | keep (unique coverage) |
| ec-av1 | encode::more_tests::real_clip_encodes_within_its_quality_and_size_budget | 22.8 | candidate | 884 | 45 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::a_non_superblock_aligned_clip_codes_edge_64x64_roots_and_decodes_sample_exact | 22.3 | candidate | 1021 | 481 | 0 | keep (unique coverage) |
| ec-av1 | encode::tests::a_nearestmv_block_with_one_nonzero_coefficient_decodes_clean | 20.8 | candidate | 746 | 402 | 0 | keep (unique coverage) |
| ec-av1 | encoder::tests::an_inter_clip_codes_both_inter_set_tx_types_both_decoders_read_exactly | 20.8 | candidate | 1028 | 442 | 0 | keep (unique coverage) |
| ec-av1 | encode::tests::a_frame_round_trips_at_its_own_size | 20.6 | candidate | 1110 | 544 | 0 | keep (unique coverage) |

All 38 candidates are `candidate` class (none protected by the name rules), and none is deleted. `yuv422_matches_dav1d_byte_exact` is a single dav1d comparison, not a vector suite, so it was not treated as protected.

## Unresolved

- Lane budget: sample (24) and survivor count (1) are too small to clear any heavy test. A larger sample would need roughly 4-5 more host-hours of heavy-first runs. I did not start it.
- `cargo clippy` ran on the earlier (stale-target) build directory. It checks source only, so this is a minor caveat.
- Evidence files too large for git (per-test coverage TSVs, full test logs) are under `~/.cache/testcut/testcut-p2-av1e/` (`evidence/`, `outputs/`).
