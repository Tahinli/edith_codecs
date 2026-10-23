# testcut Phase 2: misc crates (lane testcut-p2-misc)

Scope: heavy rows (exec_time_s >= 20) in `lanes/testcut/rest.tsv` for ec-image, ec-opus, ec-mp3, ec-dnxhd, ec-mp4, ec-vorbis, ec-amr, ec-dv, ec-flac, ec-matroska, ec-pcm-ext, ec-vp9. ec-aac, ec-h265 and ec-h264 are owned by other lanes.

Counts: 24 heavy rows in scope. 5 are protected, leaving 19 candidates. The orchestrator's brief said 28 candidates and about 1,100 s. Summed from the TSV, the 19 candidates total 718.9 s.

## Decision for all candidates

**Deferred.** The candidates are already excluded from the `fast` profile (`.config/nextest.toml`). Deleting them would save only the full-gate cost, about 719 s. That is below the cost of mutation evidence on contended server slots. No mutation runs were done, so no test is deleted and no per-test commit exists for this lane.

## Table

| crate | test | seconds | class | \|U(T)\| | mutants tried | \|S\| | \|K\| | \|K'\| | decision | seconds saved |
|---|---|---|---|---|---|---|---|---|---|---|---|
| ec-mp3 | random_frames_match_ffmpeg | 107.8 | candidate | - | - | - | - | - | deferred | 0 |
| ec-dv | mutated_streams_match_ffmpeg | 60.4 | candidate | - | - | - | - | - | deferred | 0 |
| ec-mp4 | every_fixture_remuxes_into_a_file_ffprobe_and_ffmpeg_agree_with | 57.2 | candidate | - | - | - | - | - | deferred | 0 |
| ec-mp4 | every_fixture_demuxes_into_the_streams_ffprobe_reports | 48.7 | candidate | - | - | - | - | - | deferred | 0 |
| ec-vorbis | onset_after_silence_has_no_pre_echo | 40.4 | candidate | - | - | - | - | - | deferred | 0 |
| ec-image | a_gif_with_many_tiny_frames_over_a_huge_canvas_is_refused_by_the_total_budget | 37.6 | candidate | - | - | - | - | - | deferred | 0 |
| ec-image | a_gif_with_a_few_tiny_frames_over_a_huge_canvas_still_decodes | 36.5 | candidate | - | - | - | - | - | deferred | 0 |
| ec-image | a_compressed_tiff_survives_mutation | 36.3 | candidate | - | - | - | - | - | deferred | 0 |
| ec-image | a_gif_animation_survives_mutation | 35.6 | candidate | - | - | - | - | - | deferred | 0 |
| ec-mp3 | decodes_every_fixture_like_ffmpeg | 34.7 | candidate | - | - | - | - | - | deferred | 0 |
| ec-dnxhd | every_dnxhd_cid_ffmpeg_can_encode | 30.3 | candidate | - | - | - | - | - | deferred | 0 |
| ec-dnxhd | dnxhr_profiles_at_odd_sizes | 25.1 | candidate | - | - | - | - | - | deferred | 0 |
| ec-image | an_animated_webp_survives_mutation | 26.5 | candidate | - | - | - | - | - | deferred | 0 |
| ec-image | arbitrary_bytes_behind_a_signature_are_refused_not_believed | 25.4 | candidate | - | - | - | - | - | deferred | 0 |
| ec-mp3 | encodes_above_the_incumbent_bar | 26.3 | candidate | - | - | - | - | - | deferred | 0 |
| ec-amr | all_modes_bit_exact_vs_libopencore | 26.6 | candidate | - | - | - | - | - | deferred | 0 |
| ec-opus | oracle_decodes_our_packets_across_the_rate_table | 20.1 | candidate | 158 | 0 | - | - | - | keep: coverage unique (11086 covered regions, 158 unique vs fast) | 0 |
| ec-matroska | every_fixture_remuxes_into_a_file_ffprobe_and_ffmpeg_agree_with | 22.8 | candidate | - | - | - | - | - | deferred | 0 |
| ec-pcm-ext | every_format_is_bit_exact_against_ffmpeg | 20.6 | candidate | - | - | - | - | - | deferred | 0 |
| ec-opus | rfc8251_test_vectors | 176.3 | protected (RFC vectors) | - | - | - | - | - | protected | 0 |
| ec-opus | rfc6716_test_vectors | 90.5 | protected (RFC vectors) | - | - | - | - | - | protected | 0 |
| ec-vp9 | libvpx_vectors_match_per_frame_md5 | 92.7 | protected (libvpx vectors) | - | - | - | - | - | protected | 0 |
| ec-vorbis | xiph_vectors_decode_like_ffmpeg | 35.5 | protected (xiph vectors) | - | - | - | - | - | protected | 0 |
| ec-flac | subset_vectors_decode_bit_exact_against_ffmpeg | 20.5 | protected (conformance vectors) | - | - | - | - | - | protected | 0 |

## ec-opus oracle coverage

`oracle_decodes_our_packets_across_the_rate_table` covers 11086 regions. 158 of them are not covered by the fast suite. It is therefore not coverage-redundant, and it is kept.

The fast-suite baseline reports two wall-clock tests failing under the instrumented build (`decode_speed`, `encode_speed`). These are timing tripwires, not correctness failures. They were tolerated with `--ignore-run-fail`.

## Tools

The helper scripts are in `lanes/testcut/p2-misc-tools/` and are committed with this report: `cov.sh`, `regions.py`, `pick.py`, `killed.py`, `retry.sh`. `killed.py` is not validated against real cargo-mutants output.
