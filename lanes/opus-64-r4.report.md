# lane-opus-64 r4 — tf_analysis port pasted, gated OFF

## What was pasted

All six edits from `lanes/opus-64-r3.handoff.md` applied to
`crates/ec-opus/src/celt_enc.rs`, in order:

1. **`INV_TABLE` constant** (128-entry `u8` table) — after `CACHE_BITS`.
2. **`l1_metric` free fn** — `L1 * (1 + LM*bias)`, float-mode identity macros.
3. **`transient_analysis` replaced** — libopus float-mode algorithm returning
   `(is_transient, tf_estimate, tf_chan)` (high-pass → forward/backward decay →
   `INV_TABLE` harmonic-mean unmask → `mask_metric > 200`; `tf_estimate` from
   `tf_max`). Replaces the old bin-consecutive peak-decay detector that
   returned `bool`. **This replacement is unconditional** (not behind the flag).
4. **`tf_analysis` method added** — libopus Viterbi tf search: per-band Haar
   `l1_metric` level sweep → `metric[]`, `tf_select` cost search, forward
   Viterbi with `path0/path1`, backward trace. Writes `self.tf_res`, returns
   `tf_select`. Borrows `self.hadamard_tmp` via `split_at_mut`, `self.x` by ref.
5. **Transient call site** — unpacks the tuple into `is_transient`/
   `tf_estimate`/`tf_chan`.
6. **tf_res wiring** — `const TF_ANALYSIS: bool` gates the Viterbi call:
   `effective_bytes >= 15*c` → `tf_analysis(...)`; else the no-analysis default
   `self.tf_res = [i32::from(is_transient); NB_BANDS]` + `tf_select = 0`.

Module doc comment (`//!`) updated to describe the gated port.

One handoff correction: the handoff's Edit 6 `else` branch used
`self.tf_res = [0; NB_BANDS]`; main since changed the default to
`[i32::from(is_transient); NB_BANDS]`, so the port keeps that (per the task's
"when off, keep the current default"). Also fixed a loop-step typo during
pasting (handoff `i += 4` was the intended fixed 1/4-sample step).

## Gate: 2 rows (sadie, hein @64k), `SWEEP_ONLY=sadie,hein`

Baselines from `lanes/opus-naz-r2.sweep.txt` (old transient_analysis, no
tf_analysis):

| row | baseline gap | baseline minsec |
|-----|-------------|-----------------|
| sadie@64 | +0.0043 | 0.9151 |
| hein@64  | +0.0020 | 0.9209 |

### TF_ANALYSIS = true (ON)

| row | gap | minsec | Δgap vs base | Δminsec vs base |
|-----|-----|--------|--------------|-----------------|
| sadie@64 | +0.0046 | 0.9085 | +0.0003 (grew) | −0.0066 (dropped) |
| hein@64  | +0.0030 | 0.9103 | +0.0010 (grew) | −0.0106 (dropped) |

### TF_ANALYSIS = false (OFF) — committed state

| row | gap | minsec | Δgap vs base | Δminsec vs base |
|-----|-----|--------|--------------|-----------------|
| sadie@64 | +0.0046 | 0.9085 | +0.0003 (grew) | −0.0066 (dropped) |
| hein@64  | +0.0031 | 0.9113 | +0.0011 (grew) | −0.0096 (dropped) |

## Decision: OFF

Gate rule: keep ON only if **both** gaps shrink **and** minsec does not drop.

Both conditions fail on both rows, for ON **and** OFF. ON ≈ OFF (tf_analysis
itself is neutral on these two rows — the Viterbi search barely moves the
needle once the new transient detector is in place). The divergence from the
old baseline comes from the **Edit 3 `transient_analysis` replacement, which is
ungated**: the libopus float-mode detector (`mask_metric > 200`) flags
transients differently from the old bin-consecutive detector, changing
`short_blocks` and therefore the whole frame encoding.

So `const TF_ANALYSIS: bool = false`. The tf_analysis Viterbi search is present
in source but never executed; `tf_res` uses the no-analysis default.

### Note for review
OFF ≠ old baseline because `transient_analysis` (Edit 3) is replaced
unconditionally. The 2 gate rows regress vs `opus-naz-r2` under the new
transient detector alone. If a follow-up wants OFF to exactly reproduce the
pre-port baseline, Edit 3 must also be gated (keep the old `bool` detector
behind the flag). Not done here because the task scoped the flag to the tf_res
wiring (Edit 6) and instructed applying handoff edits 1..N as-is. The full
14-row sweep (step 4) was skipped — it is only required when ON.

## Verification

- `cargo check -p ec-opus` — clean (one pre-existing `missing_docs` warning in
  `celt.rs:1113`, unrelated).
- `cargo test -p ec-opus --release` — 34 + 27 passed, 0 failed (8 ignored).
- Gate `SWEEP_ONLY=sadie,hein cargo test -p ec-opus --release --test
  conformance encoder_library_gate_vs_libopus -- --ignored --nocapture` —
  RATE GATE passed, DROPOUT GATE passed, both for ON and OFF runs.

## Files

- `crates/ec-opus/src/celt_enc.rs` — INV_TABLE, l1_metric, transient_analysis
  (replaced), tf_analysis (added), call-site + tf_res wiring, module doc.
- `lanes/opus-64-r4.sweep.txt` — the OFF 2-row gate sweep (sadie/hein @64/96k).
- `lanes/opus-64-r4.report.md` — this file.
