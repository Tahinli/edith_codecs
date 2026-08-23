## HANDOFF — sadie@64k dropout second

### Goal
Kill the sadie@64k dropout second in ec-opus encoder. Baseline: sadie@64 ours min-second corr .8987 (1 dropout second), ref .9100. Done = `drop_ours==0` AND full 14-row sweep still passes (no new dropout row, rate ±5%, no row corr≥ref regressed >.001).

### What's done (uncommitted, on branch `lane-opus-drop`, worktree `/home/tahinli/Documents/Code/Rust/ec-wt-opus-drop`)

**Instrumentation plumbing added — NOT yet compile-checked, NOT yet exercised.** All edits to two source files:

1. `crates/ec-opus/src/celt_enc.rs`
   - New `pub struct CeltFrameDiag` (after line ~275, before `CeltEncoder`): fields `is_transient, short_blocks, intra, silence, lm, start, coded_bands, intensity, dual_stereo, alloc_trim, vbr_reservoir, nb_compressed, band_log_e: [f32; 2*NB_BANDS], pulses: [i32; NB_BANDS], fine_quant: [i32; NB_BANDS]`. `#[derive(Clone, Debug, Default)]`.
   - New field `last_diag: CeltFrameDiag` on `CeltEncoder` (after `urow`), initialized `CeltFrameDiag::default()` in `new` (after `urow: vec![0;1280]`).
   - New getter `pub fn last_diag(&self) -> &CeltFrameDiag` (after `channels()`).
   - Populate `self.last_diag` at end of `encode`, just before `Ok(nb_compressed)` (after the `enc.error()` check block). Copies `self.band_log_e` into a `[f32; 2*NB_BANDS]` array; copies `self.pulses` and `self.fine_quant` arrays.

2. `crates/ec-opus/src/lib.rs`
   - `pub use celt_enc::CeltFrameDiag;` (after `CeltEncoder` re-export).

3. `crates/ec-opus/src/encoder.rs`
   - New `pub fn last_celt_diag(&self) -> &crate::celt_enc::CeltFrameDiag` on `Encoder` (after `final_range()`), delegating to `self.celt.last_diag()`.

**WARNING — one edit left a possible cosmetic issue:** after the last `edit` on the encode tail, the region read back as:
```
913:        Ok(nb_compressed)
914:    }
915:    // -- Analysis ------------------------------------------------------------
```
No blank line between `}` and the `// -- Analysis` comment. Cosmetic only; syntax probe passed on that edit. **The first compile gate (`cargo check -p ec-opus`) has NOT been run yet** — that is the very first next step. If `band_log_e` local name clashes (there's already a `self.band_log_e` field but the local shadow is fine) or `NB_BANDS`/`CeltFrameDiag` visibility errors surface, fix them. Likely clean.

### What remains (exact next steps)

1. **Compile gate:** `export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-opus-drop && cargo check -p ec-opus` (cheap, no emit — per the GLM compile-discipline rule, run `check` not `build`/`test` first). Fix any errors in the three files above.

2. **Write the instrumentation test.** Add a new `#[ignore]` test `sadie64_dropout_instrumentation` in `crates/ec-opus/tests/conformance.rs` (near the gate test, ~line 3013). Add `CeltFrameDiag` to the existing `use ec_opus::{...}` at line 784. The test should:
   - `ffmpeg_decode_pcm(sadie, 120.0)` → `source_pcm` (2ch, 48k).
   - Ref: `ffmpeg_encode_libopus(sadie, 64, &scratch, 120.0)`; `decode_ogg(&scratch)` → `ref_dec`; also call `ogg_packets(&fs::read(scratch))` to get per-packet byte sizes for the reference (skip packet[0] = OpusHead). Compute `ref_kbps` from file size like the gate does.
   - Ours: `Encoder::new(48000,2,Audio)`, `set_bitrate((ref_kbps*1000.0).round() as u32)`, `set_vbr_constrained(true)`. Loop frame-by-frame (960 = 20ms): `enc.encode_float(block,960,&mut out)` → record `len` (packet bytes) AND `enc.last_celt_diag()` snapshot; decode each via own `Decoder`; accumulate decoded. 50 frames = 1 second.
   - Align ours and ref to source via `align_to_source(..., MAX_LAG=2000)`.
   - `per_second_corr` for ours and ref → find the second(s) where ours < 0.9.
   - Write `lanes/opus-drop-r1.second.txt`: per-second corr table (ours vs ref), then for the failing second dump all 50 frames: `frame_idx, t_ms, ours_bytes, ours_bits, ref_bytes, ref_bits`, and diag fields `is_transient, short_blocks, intra, silence, lm, start, coded_bands, intensity, dual_stereo, alloc_trim, vbr_reservoir`, plus per-frame decoded RMS (ch0,ch1) for ours and ref. Also dump `band_log_e[0..21]` (ch0) and `pulses[0..21]` per frame.
   - Run: `SWEEP_ONLY=sadie cargo test -p ec-opus --release --test conformance sadie64_dropout_instrumentation -- --ignored --nocapture` (or just the test name without SWEEP_ONLY since it's hardcoded to sadie).

3. **Name the cause** from the dump. Working hypothesis from static read (NOT yet confirmed by data): at 64k stereo, `effective_bytes≈158`, `effective_rate=2*((8*158-80)>>3)/5≈58` → **intensity=16** (bands ≥16 collapse to mono above ~band 16). Combined with the constrained-VBR reservoir draining on transient frames (`target=7*target/4` at line ~660), a transient burst starves the next ~1s of frames, and anti-collapse reservation (line ~731) needs `5<<BITRES` bits at lm=3 — if bit-starved it's skipped → transient recovery fails → one second drops below 0.9. The dump will confirm which: look for the failing second to (a) start right after a `is_transient=true` frame with `vbr_reservoir` going negative, (b) show `ours_bytes << ref_bytes` for several frames, (c) `coded_bands < 16` or `intensity` stuck high, (d) `pulses` near-zero on upper bands.

4. **Levers, one at a time** (measure each with `SWEEP_ONLY=sadie` gate run, keep winner, `git checkout` revert losers, record numbers):
   - **L1 transient bit reservoir:** raise the reservoir floor or reduce the `7*target/4` transient boost (celt_enc.rs ~660) so a transient burst can't drain >1 frame of buffer.
   - **L2 disallow bandwidth drop at 64k:** clamp `coded_bands` / `end` so fullband is always coded (prevent `coded_bands` collapsing on starved frames).
   - **L3 stereo width floor:** lower the intensity threshold so intensity stays <16 at 64k stereo (line ~710-724), keeping mid/side above band 16.
   - **L4 tf_select:** allow non-zero `tf_select` for transient frames (line ~545) — reference's tf search may pick better time resolution.
   - Gate command per lever: `SWEEP_ONLY=sadie cargo test -p ec-opus --release --test conformance encoder_library_gate_vs_libopus -- --ignored --nocapture`. Winner = `drop_ours==0` AND `minsec_ours` improved without trashing `corr_ours`.

5. **Full 14-row sweep on winner:** `cargo test -p ec-opus --release --test conformance encoder_library_gate_vs_libopus -- --ignored --nocapture` (no SWEEP_ONLY). Verify: no new dropout row, rate ±5%, no row where `corr_ours≥corr_ref` regressed >.001 vs baseline in `lanes/opus-gate-r1.sweep.txt` (12/14 rows corr_ours≥corr_ref; sadie@64 + hein@64 trail).

6. **Report:** write `lanes/opus-drop-r1.report.md`, verdict line first: `sadie@64 minsec a→b, drops n→m; 14-row: X rows corr≥ref; worst regression row <tag>@<kbps> gap +d.dddd`. Include cause line (named mechanism with frame evidence from the dump) and refuted levers with their measured numbers.

7. **Commit** on branch `lane-opus-drop` (no merge, no push): the diag plumbing + the winning lever change + the report. **Do NOT commit the instrumentation test** unless it's still wanted — it's `#[ignore]`d scaffolding; recommend keeping it (ignored, cheap) or removing before commit. User's call, but unattended-batch default = keep ignored scaffolding out of the shipped tree → remove the test before commit, keep the `CeltFrameDiag`/`last_diag` plumbing only if a follow-up gate wants it; otherwise revert plumbing too. **Recommendation: revert the plumbing + test together once the cause is named and the lever lands, so the final diff is only the encoder fix** — unless the user wants persistent introspection.

### Key file/line references for the next session
- `crates/ec-opus/src/celt_enc.rs`: `encode` (439-914), VBR reservoir (652-700, was 652-700 pre-edit, now ~720-770 after diag additions — re-read), stereo intensity (702-726 → ~770-794), anti-collapse (731-736 → ~799-804), `last_diag` populate (~893-912).
- `crates/ec-opus/src/encoder.rs`: `encode_frame` (725-752), `last_celt_diag` (221-229).
- `crates/ec-opus/tests/conformance.rs`: gate test (2862-3013), `roundtrip_own` (810-843), `per_second_corr` (2831-2860), `align_to_source` (2761-2806), `decode_ogg` (537), `ogg_packets` (~440), `ffmpeg_decode_pcm` (2690), `ffmpeg_encode_libopus` (2724), `shellexpand` (2680), import line 784.

### Evidence / verification status
- Baseline dropout reproduced (prior session): `sadie@64k: ... minsec o=0.8987 r=0.9100, drop o=1 r=0` → DROPOUT GATE FAILED. sadie@96k passes.
- **No compile or run verification of this session's edits yet.** The `CeltFrameDiag` plumbing is code-written only. First action next session = `cargo check -p ec-opus`. Do not claim the instrumentation works until that test runs and produces `lanes/opus-drop-r1.second.txt`.

### Constraints reminder
- `export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-opus-drop` before any cargo command.
- Codecs deterministic → every claim is a numeric test, never ears.
- ≤60 tool calls then handoff (this session hit the cap mid-instrumentation).
- Commit on branch; no merge, no push.
- Repo is NOT under `~/.claude` or `~/.config` → no auto push obligation, but commit is still required per task.
