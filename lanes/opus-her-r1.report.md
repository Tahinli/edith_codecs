# opus-her r1 — her@96k opus_compare error map

**Verdict: no fix. The charter's premise (her@96 err_ratio 10.126, target < 5) is
superseded — `706596b` (lane-opus-64, transient_analysis port) already took her@96
from 10.1 → 1.05 and her@64 from 4.65 → 1.10.** This lane delivers the requested
error map (`lanes/opus-her-r1.map.txt`, test `her_err_map_96k`) and the certified
current 14-row baseline. No `src` change: the gate target is met and a change would
be tuning a passing row.

## What the map is

`her_err_map_96k` (`#[ignore]`, `crates/ec-opus/tests/conformance.rs`, 30 s): the
gate's exact encode path for both encoders (ffmpeg libopus ref → realised ref_kbps
→ ours CVBR at that rate), `opus_compare_err_parts` per-2.5 ms-hop ef² bucketed
into 1 s windows. Prints top-10 windows by ours/ref ratio and by ours share, with
our encoder flags, both encoders' coded decisions (ref packets decoded with our
decoder, the naz-r2 method), per-band Δlog2E on the worst hop, and all 50 frames of
the worst window. `HER_KBPS` overrides the rate.

## Map findings (her@96, ref 101.5 / ours 101.0 kbps, err_ratio 1.052)

- 93 % of our remaining error sits in **t=88 s**, the track's biggest attack burst.
  Ref's error there is 0.005 % of its total (window ratio 3.5 M).
- Frame dump f4410–f4415 (transient burst): both encoders agree on trans/sb8/ac1.
  Divergence: **ours fires intra coarse-energy 3 frames running (f4413–15) where
  libopus stays inter**, and **underspends the burst 2016–3024 bits/frame vs ref's
  3112–3536 with the reservoir pegged (~16100)**. `delayed_intra` (celt_enc.rs:708)
  is the libopus heuristic; once rate loops diverge it fires differently.
- Startup window t=0 s: intra1 both (first frame), ref spends 3496 vs our 2848
  bits, ref codes cb21/dual on the transient — startup transient underspend, same
  shape as naz-r2's tf_res finding, now small (0.0 % of total error).
- Other windows (t=76 s, t=83 s, t=89 s): flag-level agreement, ±1 alloc_trim,
  Δlog2E ≤ 3 dB on isolated bands — normal two-encoder divergence, no mechanism.

## Named residual (candidate for a future lane, not this one)

`deferred: transient-burst VBR underspend + spurious delayed-intra at attacks
(t=88 s evidence above)` — would chase the last 1.05 → ~1.0. Gate is green at
1.052; wrong to tune a passing row.

## Gates (this lane's certification, no `src` change so no regression risk)

- `cargo test -p ec-opus --release` — **34 + 27 passed, 0 failed** (14 ignored as
  designed).
- 14-row gate `encoder_library_gate_vs_libopus -- --ignored` — **RATE within ±5 %,
  DROPOUT passed**, all rows:

| row | ref/ours kbps | corr o / r | err_ratio |
|---|---|---|---|
| nik@64k | 69.2 / 69.1 | .9887 / .9864 | 0.708 |
| nik@96k | 101.0 / 100.9 | .9947 / .9940 | 0.982 |
| zaur@64k | 63.6 / 63.6 | .9893 / .9866 | 1.510 |
| zaur@96k | 93.9 / 93.4 | .9947 / .9937 | 0.940 |
| her@64k | 67.9 / 67.7 | .9834 / .9794 | **1.102** |
| her@96k | 101.5 / 101.0 | .9914 / .9902 | **1.052** |
| naz@64k | 71.3 / 71.3 | .9908 / .9880 | 2.697 |
| naz@96k | 105.4 / 105.3 | .9956 / .9948 | **4.499 (new worst)** |
| sadie@64k | 63.3 / 63.1 | .9866 / .9859 | 0.623 |
| sadie@96k | 84.8 / 84.7 | .9935 / .9920 | 0.660 |
| dl8a@64k | 65.8 / 65.7 | .9889 / .9865 | 0.551 |
| dl8a@96k | 96.8 / 96.5 | .9942 / .9934 | 0.374 |
| hein@64k | 64.9 / 64.8 | .9888 / .9874 | 0.855 |
| hein@96k | 86.9 / 86.8 | .9946 / .9930 | 0.528 |

Worst row is now **naz@96 at 4.499** — the next gate lane's premise.

## Files

- `crates/ec-opus/tests/conformance.rs` — `her_err_map_96k` (commit 233d47e).
- `lanes/opus-her-r1.map.txt` — the map (commit 233d47e).
- `lanes/` gate artifacts restored via `git checkout --` after the runs.
