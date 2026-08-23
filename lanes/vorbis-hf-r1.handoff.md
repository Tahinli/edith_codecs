## HANDOFF — Tool cap still blocking all tool calls

Three consecutive sessions have hit the tool-call cap before I could make a single edit. No new work was possible.

### The one blocking action (must be first tool call next session)

Read `crates/ec-vorbis/src/encode.rs:950-960` to confirm current state, then revert lines 954–956 from:
```rust
let scale = if self.config.bitrate_bps < 96_000 { 0.93 } else { RATE_TARGET_SCALE };
let target = f64::from(self.config.bitrate_bps) * scale * delta as f64
    / f64::from(self.config.sample_rate);
```
to:
```rust
let target = f64::from(self.config.bitrate_bps) * RATE_TARGET_SCALE * delta as f64
    / f64::from(self.config.sample_rate);
```

### Full remaining sequence (from prior handoffs, unchanged)

1. Revert scale (above).
2. Make co_mask HF cap (line 1056) steady-only: pass `steady` bool from `encode_block` (line 715) to `fit_floor` (call site ~line 746). Only apply `co_mask - 30.0` when `steady == true`. Transient blocks keep full 50 dB → finer HF floor → fewer drops.
3. `cargo check -p ec-vorbis`
4. `SWEEP_ONLY=sadie,hein` sweep — target: sadie@96 ≤3% rate, 0 drops; hein@96 0 drops.
5. If sadie@96 rate still >3%: try `RATE_TARGET_SCALE = 0.95` globally.
6. Full 14-row sweep — 14/14 PASS, no regression >.001 gap.
7. Write `lanes/vorbis-hf-r1.report.md` — verdict line first.
8. Commit on `lane-vorbis-hf`. No merge, no push.

### Evidence (from completed sweep runs)

co_mask HF cap at bark > 19, −30 dB (line 1056, KEEP):

| Row | Baseline | With cap | Verdict |
|---|---|---|---|
| sadie@96 | +7.07%, .0093, 9 drops | +3.74%, .0032, 3 drops | FAIL (rate, drops) |
| sadie@128 | +2.31%, .0060, 2 drops | −0.13%, .0014, 0 drops | **PASS** |
| hein@96 | +4.26%, .0086, 3 drops | +1.18%, .0024, 1 drop | FAIL (drops) |
| hein@128 | +0.51%, .0050, 0 drops | −0.26%, .0000, 0 drops | **PASS** |

### Todos

Mark complete: "Map fit_floor/co_mask/tilt interaction" (done), "Try CO_MASK_RANGE HF cap" (done, active lever). Still open: tilt HF extension, ATH floor lift, full sweep, report, commit.
