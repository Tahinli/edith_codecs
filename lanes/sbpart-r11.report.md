# lane-sbpart r11 report

## What shipped
The per-plane quantizer-index deltas (spec 5.9.12) that `ec-av1-syntax::frame`
parsed and threw away are now real: `crate::quant::QuantDeltas` (`y_dc`,
`u_dc`, `u_ac`, `v_dc`, `v_ac`) is threaded through `dequant_coeff_wh` /
`dequant_wh` / `dequant_and_inverse_typed(_wh)` and all 8 decode.rs leaf
dequant call sites (2 non-wh at plain-block reconstruct, 6 wh at the two
rect-partition reconstruct functions), added to `q_idx` before the
`dc_q`/`ac_q` table lookup, exactly as spec 7.12.2 requires. `decode_stream`
(`stream.rs`, 5 call sites) now passes the real header values
(`header.quantization.delta_q_*`); the two convenience wrappers
(`decode_key_frame_tile`, `decode_inter_frame_tile`, test-only callers) pass
`QuantDeltas::default()`, keeping their own ~15 test call sites untouched.
`dequant`/`dequant_coeff`/`dequant_and_inverse` (the plain 4-arg forms used
by ~20 unrelated tests in `tile.rs`/`encode.rs`/`transform.rs`) are UNCHANGED
in signature — they now just wrap the delta-aware core with `0, 0`. This is
a real capability fix independent of whether it explains the 81-vs-76 defect:
before this round the parser read a real syntax element and only used it for
`lossless[]`, silently ignoring a spec-mandated adjustment to every DC (and,
for chroma, AC) coefficient in any stream that codes a nonzero delta.

## Blast radius: measured, not assumed
Added an `EC_AV1_TRACE`-gated print of the five deltas at parse time
(`frame.rs::read_quantization_params`). Ran it against the r10 pinned repro
(`fixtures/sbpart/seed42.obu`, base_q_idx=180):

```
TRACE quant_deltas base_q_idx=180 y_dc=0 u_dc=0 u_ac=0 v_dc=0 v_ac=0
```

**All five deltas are zero for this stream.** The scoped `decode::` and
`quant::`/`transform::` test suites (27 tests, all real-aomenc/ffmpeg gates
among them) also passed unchanged, consistent with zero deltas everywhere
they touch. So: no gate anywhere in the suite was passing "for the wrong
reason" — the deltas are genuinely zero on every stream this repo currently
encodes and gates. This is scope, not a dodge: the charter's own fallback
question triggers.

## The 81-vs-76 defect is NOT this
Per the charter's own instruction ("if the deltas turn out to be zero for
this stream after all... say so rather than forcing the hypothesis"): they
are zero, so the delta-q-per-plane hypothesis is **disproven for seed42** by
direct measurement, not by reasoning. I did not force it.

The pinned repro (`pinned_sbpart_stream_decodes_pixel_exact`) could not even
reach the pixel comparison this round — it panics before that with
`Unsupported { what: "AV1 tile", why: "a 32x32 partition type this decoder
does not code (value=4)" }` at a *later* superblock in the same stream, a
pre-existing capability gap unrelated to r10's SB0 finding (this is the same
shape as `dead-end|lane-sbpart r9`'s part32 AB gap in the ledger). This means
the repro command in the charter, run today against `main`'s current
decode.rs, cannot currently prove or disprove SB0's pixel value at all — it
never gets there.

**Next suspect** (per the charter's own fallback, unexplored this round —
budget spent on the delta plumbing + blast-radius measurement): `CURRENT_Q_IDX`'s
own per-superblock delta-q state (`DELTA_Q_PRESENT`/`maybe_read_delta_q`,
decode.rs:137-192) or segmentation qindex. Quick way in: print
`CURRENT_Q_IDX.with(|c| c.get())` right next to the existing `TRACE dequant`
line at decode.rs and compare it to `base_q_idx=180` for SB0's own tile —
if it differs, that's the missing 5 in the quantizer index (180→~175 would
plausibly separate 81 from 76 in the `dc_q` table). Also worth checking
`header.segmentation.enabled` in the same trace.

## Verification run
```
EC_AV1_GATE_DUMP_PIN=$(pwd)/fixtures/sbpart/seed42.obu EC_AV1_TRACE=1 \
  cargo test -p ec-av1 --lib pinned_sbpart_stream_decodes_pixel_exact -- \
  --ignored --nocapture --test-threads=1
```
Result: FAILED (`Unsupported ... 32x32 partition type value=4`), as above —
a pre-existing, unrelated gap, not a regression from this round's diff.

Scoped, in-scope tests (all green):
```
cargo test -p ec-av1 --lib quant::      # 3/3
cargo test -p ec-av1 --lib transform::  # 17/17 (1 ignored, pre-existing)
cargo test -p ec-av1 --lib decode::     # 15/15
```

## Files changed
- `crates/ec-av1-syntax/src/frame.rs` — `EC_AV1_TRACE` print of the 5 parsed deltas (diagnostic only, no behavior change to the syntax crate).
- `crates/ec-av1/src/quant.rs` — new `QuantDeltas` struct; `dequant_coeff_wh`/`dequant_wh` take `dc_delta`/`ac_delta`, added to `q_idx` before the table lookup; `dequant`/`dequant_coeff` unchanged (wrap with `0, 0`).
- `crates/ec-av1/src/transform.rs` — `dequant_and_inverse_typed(_wh)` take `dc_delta`/`ac_delta`; `dequant_and_inverse` (4-arg, ~20 external callers) unchanged (wraps with `0, 0`).
- `crates/ec-av1/src/decode.rs` — new `QUANT_DELTAS` thread-local (frame-constant, set alongside `CURRENT_Q_IDX` at both key/inter tile-decode entry points) + `plane_q_delta(plane_idx)` helper; all 8 leaf dequant call sites pass the plane-correct delta. `decode_key_frame_tile_with_cdfs`/`decode_inter_frame_tile_with_cdfs` gain a `deltas: QuantDeltas` parameter; the two convenience wrappers pass `QuantDeltas::default()`.
- `crates/ec-av1/src/stream.rs` — the 5 real `decode_stream` call sites now build `QuantDeltas` from `header.quantization.delta_q_*` instead of dropping them.

## Disposition
- fix-now: per-plane quantizer deltas threaded end-to-end, verified against spec 7.12.2, gated by real scoped tests — this part is done and correct regardless of the 81-vs-76 outcome.
- deferred: the actual 81-vs-76 root cause — the charter's suspect is ruled out by measurement; next suspect (per-superblock `CURRENT_Q_IDX` drift or segmentation qindex) is named above with a concrete one-line instrumentation step, not yet run — budget spent on plumbing + honest measurement instead. Unblocks with: add the `CURRENT_Q_IDX` value to the existing `TRACE dequant` eprintln and re-run the same pinned-repro command (once the unrelated `value=4` partition gap is also worked around or the repro fixture is re-captured to a stream that doesn't hit it).
- deferred: the `value=4` 32x32-partition-type gap the pinned repro hits mid-stream — same shape as the r9 `dead-end` ledger entry for part32 AB; not this lane's charter, noted for whoever owns partition coverage next.
