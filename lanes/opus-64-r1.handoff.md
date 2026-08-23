## HANDOFF

### Done
- **Compile gate passes**: `cargo check -p ec-opus` clean (0.22s)
- **Diagnostic test written and run**: `sadie64_persecond_diag` in `conformance.rs:2862` — compiles, passes, dumps to `lanes/opus-64-r1.seconds.txt`
- **Fresh per-second data collected** on current (post-r2) code for sadie@64k:
  - **Avg corr: ours=0.9823, ref=0.9854, gap=+0.0031** (per-second avg; gate test overall corr gap is +0.0043)
  - **alloc_trim histogram: [(0, 2517), (1, 2745), (2, 738)]** — never above 2
  - **intensity: always 16** (correct for 64k stereo)
  - **dual_stereo: 0/6000** (correct for 64k)
  - **avg bytes/frame: ours=157.2, ref=156.6** — nearly identical total rate (NOT a rate problem)
  - **realised kbps: ours=63.3, ref=62.6**
  - **transient frames: 1431/6000** (24%)
  - **Worst 10 seconds**: 88 (+0.0241), 117 (+0.0169), 35 (+0.0165), 96 (+0.0161), 79 (+0.0142), 20 (+0.0144), 32 (+0.0133), 80 (+0.0127), 98 (+0.0092), 26 (+0.0086)
  - Per-frame diag for worst seconds includes: B_ours vs B_ref, trans, short, intra, trim, reservoir, pulses[0..5], fine[0..5]

### Key Finding
**The gap is an allocation-shape problem, not a rate problem.** Bytes/frame are nearly identical. The gap is distributed across ~30+ seconds at +0.005 to +0.024 per second. Three hardcoded/stand-in decisions remain:

1. **`tf_select = 0` hardcoded** (`celt_enc.rs:613`) — libopus runs `tf_analysis()` to pick tf_select; we always use 0. For lm=3 (20ms), tf_select=1 gives `[3,0,1,-1]` vs tf_select=0's `[0,-2,0,-3]`.
2. **`SPREAD_NORMAL` fixed** (`celt_enc.rs:648`) — libopus uses `spread_analysis` considering tonality.
3. **VBR tf boost stand-in** (`celt_enc.rs:774-778`) — `0.012*target` for steady, `7*target/4` for transient, vs libopus's `2*(tf_estimate-0.044)*target` from real tonality analysis.

### Incomplete — `tf_res` initialization not yet found
The last grep (which hit the cap) was searching for where `self.tf_res[i]` is SET before `tf_encode` is called. It's likely set in the transient analysis or energy computation section of `encode()` (around lines 600-650). This is needed to understand whether tf_select=1 would have any effect (if tf_res is always 0, tf_select is moot).

### Remaining Steps
1. **Find `tf_res` initialization** — grep `tf_res` in `celt_enc.rs` (the blocked call was `grep "tf_res" celt_enc.rs`). Determine if tf_res is ever non-zero for steady frames. If always 0, tf_select has no effect and lever #1 is dead.
2. **Try lever — tf_select=1 hardcoded** (quickest test): change line 613 `let tf_select = 0usize` to `1usize`. Run `SWEEP_ONLY=sadie,hein cargo test -p ec-opus --release --test conformance encoder_library_gate_vs_libopus -- --ignored --nocapture`. Keep only if both gaps shrink AND minsec doesn't drop.
3. **If tf_select doesn't help, try spread**: change `SPREAD_NORMAL` to `SPREAD_AGGRESSIVE` at line 648. Same gate test.
4. **If neither helps, try VBR shaping**: increase the steady-frame tf boost from `0.012*target` to `0.025*target` (corresponding to tf_estimate≈0.057). Same gate test.
5. **Full 14-row sweep**: `cargo test -p ec-opus --release --test conformance encoder_library_gate_vs_libopus -- --ignored --nocapture`. Verify no row corr −0.001 vs baseline, no dropouts, rate ±5%.
6. **Remove diagnostic test** before commit. Commit lever + report + sweep on branch `lane-opus-64`.
7. **Write `lanes/opus-64-r1.report.md`** (verdict first) and `lanes/opus-64-r1.sweep.txt`.

### Files Modified (uncommitted)
- `crates/ec-opus/tests/conformance.rs` — added `sadie64_persecond_diag` test at lines 2862-3056 (MUST be removed before commit)
- `lanes/opus-64-r1.seconds.txt` — diagnostic output (keep for analysis, remove before commit)

### Commands
```bash
export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-opus-drop
# Scoped sweep (sadie+hein only)
SWEEP_ONLY=sadie,hein cargo test -p ec-opus --release --test conformance encoder_library_gate_vs_libopus -- --ignored --nocapture
# Full 14-row sweep
cargo test -p ec-opus --release --test conformance encoder_library_gate_vs_libopus -- --ignored --nocapture
# Compile gate
cargo check -p ec-opus
```

### Baseline (from `lanes/opus-drop-r2.sweep.txt`, must not regress)
```
sadie 64: ours .9816 ref .9859 gap +.0043 minsec .9148/.9100 drops 0/0
hein  64: ours .9854 ref .9874 gap +.0020 minsec .9210/.9148 drops 0/0
All other 12 rows: ours ≥ ref
```
