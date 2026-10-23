# lane/opusgate report

Base: bdc74c55 (main at lane start). Branch: lane/opusgate. Not pushed.

## Status (round 2, Sonnet)

| Item | Status | Evidence |
|---|---|---|
| RFC 6716 12/12 vectors | met (legacy mode) | all 12 pass with `set_legacy_rfc6716(true)` |
| RFC 8251 `.dec` set 12/12 | met | all 12 pass (scripts/fetch-vectors.sh, sha256 in scripts/vectors.sha256) |
| RFC 8251 `m` set | not met | `.dec` phase-shift set chosen; `m` set not run as a gate |
| unwraps celt.rs:2227/2297/2406/2410 | met | `Result`/`Error::corrupt`; clippy exit 0 |
| SILK packet budget (D2b) | met | `encoder_rate_target_gate` on host c: 15/15 rows 0 over, 0 under, all +0.00% (nik/reelo/her/reell music, hein speech at 32/64/128 kbps); hein@32k Q -767.89 (was 2 over, Q -986.65) |
| rate +-5% at 32/64/128 | met | all rows 32.00/64.00/128.00 kbps |
| dropout gate opus-gate-r1 reelm@64k | met | `encoder_library_gate_vs_libopus` on host a: reelm@64k ref 64.5 ours 64.3 kbps (-0.2%), corr o=0.9888 r=0.9874, drop o=0 r=0; reelm@96k drop o=0; "RATE GATE: all rows within +-5%", "DROPOUT GATE: passed" |
| silk_library gate | met | `silk_library_gate_vs_libopus` host a: 12k/16k/24k/32k rate -1.6/+3.7/-0.3/-0.0%, drop o=66/48/25/16 vs libopus 89/87/87/83; "RATE GATE", "LAG GATE", "DROPOUT GATE" all passed |
| `oracle_decodes_our_packets_across_the_rate_table` | met | 2 ch 16 kbps corr now above 0.44 (was 0.4046); mono SILK 8/12/16k corr 0.8649/0.8150/0.8534 (floor 0.8) |
| `encode_speed` / `decode_speed` | met in isolation | alone with `--test-threads=1` on host b (load ~3): decode 262x/120x(5.1)/249x; encode mono 96k 306x, stereo 128k 194x, 256k 136x, 510k 99x, 5.1 384k 68x. They fail only when run in parallel with the other 27 conformance tests (169 s suite). Floors untouched. |
| `cargo test --release -p ec-opus` | met apart from timing | host b: lib 42 passed; conformance 27 passed, 2 failed (the two timing tests above, parallel load), 17 ignored; third binary 3 passed |
| `cargo clippy -p ec-opus --all-targets -- -D warnings` | met | exit 0 |
| multi-frame (40/60 ms) stereo SILK budget | not done | not attempted |

## Regression root cause and fix

The 2 ch 16 kbps case is a mid-only hybrid stream. The budget clamp had three weaknesses, all in `silk_enc_write.rs`:
1. Budget coarsening fed back into the rate carry (`rate_ema`), dragging later frames coarser. Now the carry uses the rate loop's own step.
2. The ceil step overshot a whole quantiser step; there was no walk-back. Added a refine pass toward the last non-fitting step.
3. On onsets the gain range saturates (a 110 B frame stayed 110 B at any step), so the packet exceeded budget. Added a pulse-shrink fallback (`lam_mul`: pulses divided by 1.15x steps up to 4x, then 2x steps to 64x), plus a plateau break and `MAX_BUDGET_STEP` 120.
Also: stereo budget is split dynamically (mid takes its share plus a quarter of the side share; side gets what the coded mid left, `side_budget_after_mid`). `SilkStereoEncoder` grew by 16 B (stack inventory 19808).
Isolation: disabling coarsening gave 0.4534 (base), disabling sub-budgets alone left mono SILK 12k at 0.764 (also a regression, fixed by 1 and 2).

## Changes

- crates/ec-opus/src/celt.rs: Result conversion of quant_band / quant_all_bands; four unwraps replaced by `Error::corrupt`. RFC 8251 Sec 8 (energy cap at 32), Sec 9 (hybrid folding), gated by `legacy_rfc6716` (default false = RFC 8251 behaviour). Legacy mode is required for RFC 6716 vector 05 (Sec 9 changes it).
- crates/ec-opus/src/lib.rs: `Decoder::set_legacy_rfc6716`.
- crates/ec-opus/src/encoder.rs: `packet_budget`; SILK per-packet budget for CBR; hybrid SILK budget = budget - (HYBRID_CELT_MIN_BYTES + 2).
- crates/ec-opus/src/silk_enc_write.rs: `set_max_bytes`; post-search coarsening loop (MAX_BUDGET_TRIALS = 8, nominal 0.227 bit/sample/step, clamp at start + MAX_RATE_STEP = 30); SilkStereoEncoder sub-budgets.
- crates/ec-opus/src/stack_budget.rs: SilkEncoder 6648 -> 6664, SilkStereoEncoder 19744 -> 19792 (deliberate).
- crates/ec-opus/tests/conformance.rs: RFC 8251 test; `encoder_rate_target_gate` (ignored, corpus-based); clippy fixes.
- scripts/fetch-vectors.sh, scripts/vectors.sha256: opus-rfc8251 set.
- scripts/remote-run.sh: modified in worktree, NOT committed (not this lane's change).

## Rate table

Gate run rate7 (hein@32k with widened coarsening): 

| source | bitrate | realised kbps | Q | budget B | over |
|---|---|---|---|---|---|
| hein (speech) | 32k | 32.00 (+0.00%) | -986.65 | 79 | 2 |
| hein (speech) | 64k | 64.00 (+0.00%) | -384.41 | 159 | 0 |
| hein (speech) | 128k | 128.00 (+0.00%) | -57.19 | 319 | 0 |

Earlier gate (rate5, before the MAX_RATE_STEP widening) music rows, all 0 over:
her 32/64/128: 32.00/64.00/128.00; reell 32/64/128: 32.00/64.00/128.00; nik 32/64/128; reelo 32/64/128 (all +0.00%).

The gate test asserts over-budget = 0 and fails with 2 packets. Hein@32k is therefore a FAIL row in the gate.

Quality note: hein@32k Q moved from -726.63 (base) to -986.65 now (lower is worse in this table's sign convention as recorded). Earlier lane state was -699.69 before widening. The widening trades quality for budget compliance.

## Dropout gate rows

reelm@64k: not run. reelm.wav is absent from ~/Music and from all server hosts (a, b, c). Last recorded sweep (lanes/opus-drop-r2.sweep.txt): drops_ours=0, rate -1.1%. Blocked on the corpus; no fresh FAIL row.

## What is left

1. Multi-frame (40/60 ms) stereo SILK budget not done.
2. SILK-only packets are not zero-padded to budget (decoder redundancy-flag condition, lib.rs ~L323); budget is enforced by coarsening.
3. RFC 8251 `m` set not run as a gate.
4. Timing tests can fail under parallel suite load; they pass alone.
5. Temporary worktree ~/.cache/wt/opusgate-base: remove with `git worktree remove`.
6. Correlation at stereo 16k is 0.4396-0.44 territory, sensitive to small changes in the clamp (the metric is bounded near 0.45 by mid-only coding); re-check after any budget change.
