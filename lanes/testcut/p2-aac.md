# testcut Phase 2: ec-aac heavy-test report

Result: **no deletions**. All 14 ec-aac heavy candidates (>= 20 s in `rest.tsv`) are kept. No candidate has the coverage and mutation kill evidence the protocol requires, and several runs skip under the lane's environment, so their evidence is invalid.

Seconds saved: 0.

Timed pass on host c (plain nextest, `EC_REQUIRE_FIXTURES=1`, repo `.cache/heaac-fixtures` present). Wall time is compared with the `rest.tsv` baseline. A run under 30% of baseline counts as a skip.

| crate | test | seconds (baseline / run) | class | |U(T)| | mutants tried | |S| | |K| | |K'| | decision |
|---|---|---|---|---|---|---|---|---|---|---|
| ec-aac | chunk_a::sbr_real_library_matches_reference | 27.9 / 26 | candidate (asserts vs ffmpeg reference) | n/a (no valid instrumented coverage) | 0 | n/a | n/a | n/a | keep (no mutation evidence yet) |
| ec-aac | chunk_a::synthetic_heaac_matrix | 151 / 142 | candidate (asserts) | n/a | 0 | n/a | n/a | n/a | keep (no mutation evidence yet) |
| ec-aac | chunk_c::coupled_cpe_channel_swap_probe | 194 / 135 | candidate (asserts) | n/a | 0 | n/a | n/a | n/a | keep (no mutation evidence yet) |
| ec-aac | chunk_b::sbr_header_feature_table | 42.5 / 23 | pseudo (no assertion on output) | n/a | 0 | n/a | n/a | n/a | keep (no mutation evidence yet) |
| ec-aac | chunk_a::dump_sideinfo_kx | 28 / 0 | pseudo | n/a | 0 | n/a | n/a | n/a | keep (skips: no valid evidence) |
| ec-aac | chunk_a::wide_lag_search_full_chain | 145 / 2 | pseudo | n/a | 0 | n/a | n/a | n/a | keep (skips: no valid evidence) |
| ec-aac | chunk_b::per_au_low_band_divergence | 145 / 8 | pseudo | n/a | 0 | n/a | n/a | n/a | keep (skips: no valid evidence) |
| ec-aac | chunk_b::probe_explicit_config_and_wide_lag | 148 / 4 | pseudo | n/a | 0 | n/a | n/a | n/a | keep (skips: ReelP media absent) |
| ec-aac | chunk_b::sbr_actual_noise_fraction | 24.7 / 0 | candidate (env-gated, `EC_AAC_SBR_NOISE_ANCHOR`) | n/a | 0 | n/a | n/a | n/a | keep (skips: needs personal media) |
| ec-aac | chunk_b::sbr_instrument_anchor_controls | 22 / 0 | pseudo (env-gated) | n/a | 0 | n/a | n/a | n/a | keep (skips: needs personal media) |
| ec-aac | chunk_c::reelq_multichannel_lc_matches_reference | 194 / 0 | candidate (asserts) | n/a | 0 | n/a | n/a | n/a | keep (skips: needs personal media) |
| ec-aac | chunk_c::sbr441_family_sample_drift_probe | 201 / 29 (14%) | pseudo | n/a | 0 | n/a | n/a | n/a | keep (skips) |
| ec-aac | chunk_c::sbr_hf_window_band_probe | 217 / 19 (9%) | pseudo | n/a | 0 | n/a | n/a | n/a | keep (skips) |
| ec-aac | chunk_c::sbr_residual_locator | 43 / 2 | pseudo | n/a | 0 | n/a | n/a | n/a | keep (skips) |

Notes:
- Fast-suite gate on host c (`--profile fast`, `EC_REQUIRE_FIXTURES=1`): rc=0, no failures.
- Instrumented (llvm-cov) runs failed with exit 100 for every candidate because they ran with the wrong fixture path or without fixtures. The coverage they produced is invalid, so no U(T) is reported.
- The protected-test rule did not apply to any candidate (none are named in DONE.md or charters, and none is a conformance vector).
- `five_one_decodes_faster_than_realtime` is a wall-clock tripwire. It is protected and was not evaluated.
- Remaining work to delete anything: valid instrumented coverage for the four valid candidates, mutation kill runs for the same, and the same for the pseudo-tests once they run on a host with the media they need.
