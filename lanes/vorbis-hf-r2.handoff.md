## HANDOFF — vorbis-hf r2 at tool cap

### Done

**BlockLog instrumentation wired and verified** — `cargo check -p ec-vorbis --tests` passes (only doc warnings, now fixed).

Files changed (uncommitted on `lane-vorbis-hf`):
- `crates/ec-vorbis/src/encode.rs`: `BlockLog` pub struct (line 234), `block_log`/`enable_block_log` fields on `VorbisEncoder`, `enable_block_log()`/`take_block_log()` methods (line 456), `update_rate` now returns `f64` (the target), `encode_block` pushes a `BlockLog` entry after `update_rate` when enabled (line 965).
- `crates/ec-vorbis/src/lib.rs`: re-exports `BlockLog`.
- `crates/ec-vorbis/tests/oracle.rs`: `encode_to_file_with_log()` (line 357), sweep uses it always, `DROPS_DUMP=1` env writes per-block rate-loop state for each dropout second to `lanes/vorbis-hf-r2.drops.txt`.

**Instrumented sweep run** (`SWEEP_ONLY=sadie,hein DROPS_DUMP=1`), dump at `lanes/vorbis-hf-r2.drops.txt` (2265 lines).

### Ground-truth sweep results (DIFFERENT from r1 handoff — r1 never ran the sweep, its table was stale)

| Row | rate% | gap | drops | Verdict |
|-----|-------|-----|-------|---------|
| sadie@96 | +0.93 | .0054 | 5 | FAIL (gap, drops) |
| sadie@128 | −3.32 | .0030 | 0 | FAIL (rate) |
| hein@96 | −1.75 | .0045 | 2 | FAIL (drops) |
| hein@128 | −4.36 | .0017 | 0 | FAIL (rate) |

**All 4 rows FAIL.** The 128k rows that r1 reported as PASS now fail on rate undershoot.

### Diagnosis (from drops dump)

**Two distinct failure classes:**

1. **128k rate undershoot (−3 to −4%)**: `RATE_TARGET_SCALE = 0.97` (line 84) makes the loop target 97% of nominal. At 128k the loop approximately hits its target, producing ~3-4% under libvorbis (which lands near nominal). Direct cause.

2. **96k reservoir windup → dropouts**: `reservoir_debt` grows to **~831,000 bits** (sadie@96, ≈8.7s of audio) and **~74,000-86,000** (hein@96). Each transient run (16-18 short blocks) adds ~10-18k bits. Repayment is clamped to ±25% of per-block target (~393 bits), so repaying 831k takes ~2,115 steady blocks ≈ 91 seconds — the reservoir never clears before the next transient. The rate loop suppresses headroom (~20-28 dB) for the entire repayment window. In quiet passages during suppression, bits drop to **~500** (vs target 1572) → corr dips below 0.9. The **0.93 scale at <96k** (line 1006) worsens this: lower target → larger bits-vs-target gap → faster reservoir growth → deeper headroom suppression.

**Dropout seconds**: sadie@96 at secs 237 (and 4 others in the 235-237s cluster, all following transient runs); hein@96 at sec 85 (and 1 other, same pattern). All dropouts are post-transient quality dips in quiet passages, not silence.

### Remaining (exact next steps)

**Lever 1 — revert 0.93 → RATE_TARGET_SCALE for <96k** (line 1006):
```rust
let scale = RATE_TARGET_SCALE;  // was: if bitrate < 96k { 0.93 } else { RATE_TARGET_SCALE }
```
This raises the 96k target from 0.93× to 0.97× nominal → less reservoir windup, less headroom suppression, fewer dropouts. 96k rate may rise from +0.93% toward +3% (still within ±3%). 128k unaffected. Run `SWEEP_ONLY=sadie,hein` sweep.

**Lever 2 — raise RATE_TARGET_SCALE 0.97 → 1.0** (line 84): Fixes 128k undershoot directly. May raise 96k rate further. Run sweep.

**Lever 3 — cap reservoir_debt**: After `self.reservoir_debt += bits - target` in the `!steady` branch (line 1010), clamp: `self.reservoir_debt = self.reservoir_debt.clamp(-target * 64.0, target * 64.0)` (≈2s of audio at 32 blocks/s). Prevents the 831k windup, shortens the suppression window from 91s to ~2s. Run sweep.

**Lever 4 — minimum residue bit floor**: In `encode_block`, if steady and bits < target * 0.5, the floor is starving; consider raising headroom floor or ensuring minimum partition coverage. Only if levers 1-3 don't clear the gap.

**Order**: 1 → sweep → 2 → sweep → 3 → sweep → 4 if needed. One lever per sweep run.

**Then**: full 14-row sweep (`SWEEP_ONLY` unset), `cargo test -p ec-vorbis --release` green, rewrite constant doc comments citing sweep evidence, write `lanes/vorbis-hf-r2.report.md` (verdict first, refuted levers with numbers), commit on `lane-vorbis-hf`.

**Note on no-regression constraint**: The r1 "PASS" rows (sadie@128, hein@128) are already FAILing — there are no PASS rows to regress in the current state. The 0.93 hack and RATE_TARGET_SCALE=0.97 are the regressors. Reverting them IS the fix.
