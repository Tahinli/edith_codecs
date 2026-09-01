# lane-hidden r1 — a direct hidden-frame (alt-ref) pixel oracle + gate

## Premise, re-measured
Charter premise (ledger `constraint|ec-av1 decode_stream pushes a picture only under
header.show_frame`) confirmed at HEAD dc5553f: `crates/ec-av1/src/stream.rs` pushes into
`pictures` only inside `if header.show_frame`, and every pixel gate compares against
`ffmpeg -f obu ... rawvideo` / `aomdec -o y4m`, both shown-frames-only. So no hidden
(`show_frame == 0`) alt-ref frame's pixels were compared anywhere in the repo.

## What changed
- `scripts/instrument-aom-oracle.sh` (rung 12, appended): `EC_AV1_FINAL_DUMP=<prefix>` in the
  instrumented `aomdec` → `<prefix>.f<N>`, one file per DECODED frame (decode order, hidden
  frames included), written in `decode_frame` after CDEF + superres + loop restoration and
  before the `!pbi->dcb.corrupted` block — i.e. the frame exactly as stored into the reference
  buffer. Y then U then V, crop-sized rows (post-superres `y_crop_width`), 8-bit as u8 and
  high bitdepth as u16 LE via `CONVERT_TO_SHORTPTR` (bit-depth correct; the other dumps in
  this repo narrow u16→u8 and silently truncate 10-bit).
- `crates/ec-av1/src/stream.rs`: matching `EC_AV1_FINAL_DUMP` at the ref-slot store site
  (immediately before `pictures_decoded += 1;`, so post-deblock/CDEF/LR/superres, decode
  order, hidden frames included), same byte layout, `bit_depth == 8` → u8 else u16 LE. The
  prefix comes from a thread-local (`set_final_dump_prefix`) falling back to the env var, so
  a gate's dump cannot be polluted by a sibling test decoding on another thread.
- `crates/ec-av1/src/stream.rs` tests: `aomdec_path()`, helper
  `decode_all_frames_vs_oracle(stream, name) -> (frames, hidden)` (runs both dumps, asserts
  equal frame counts, compares every decode-order frame byte for byte, reports first
  differing byte + count and pins the stream on failure), and the gate
  `a_real_aomenc_altref_sequence_hidden_frames_decode_pixel_exact`.

## Gate
`a_real_aomenc_altref_sequence_hidden_frames_decode_pixel_exact` — 64x64 testsrc2, 40 frames,
`--auto-alt-ref=1 --lag-in-frames=25 --arnr-maxframes=0 --kf-max-dist=1000` plus the tool set
the sibling inter gates pin; run for 8-bit and 10-bit. Hard `assert!(hidden >= 2)` (no skip),
hard per-frame byte equality incl. hidden frames, `decode_stream` error = failure not skip.

`--arnr-maxframes=0` is load-bearing and was measured, not assumed: with libaom's temporal
alt-ref filter at its default the same recipe emits **0 hidden frames** (the gate would be
vacuous); with it off, 3.

    cd /home/tahinli/Documents/Code/Rust/edith_codecs-hidden
    export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-hidden
    EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib \
      a_real_aomenc_altref_sequence_hidden -- --nocapture

EVIDENCE: /tmp/.../t_arnr.f* + t10.f* (oracle dumps), gate stdout | aomenc --auto-alt-ref=1 --lag-in-frames=25 --arnr-maxframes=0, aomdec EC_AV1_FINAL_DUMP, decode_stream EC_AV1_FINAL_DUMP, byte compare | 8-bit: 43 frames decoded, 3 hidden, all pixel-exact; 10-bit: 43 frames decoded, 3 hidden, all pixel-exact
EVIDENCE: oracle rung proof | `EC_AV1_FINAL_DUMP=$PWD/fin aomdec -o out.y4m alt.obu` on a default-settings aomenc stream | dumps=31 vs shown=30 → the rung emits the hidden frame the y4m output does not; 8-bit f0 = 6144 B = 64x64x1.5, 10-bit f0 = 12288 B = 2 bytes/sample
EVIDENCE: recipe-vacuity measurement | same recipe with/without --arnr-maxframes=0 | hidden 0 → 3

## Refusals
None lifted (`refusal_inventory.rs` / `gate_coverage.rs` untouched) — this lane adds an
instrument and a gate, it does not widen decode capability.

## (d) Which existing gates carry hidden frames
Static scan of the 52 encoder-driven gates in stream.rs: **15 pre-existing gates pass
`--auto-alt-ref=1`** (1 with `--lag-in-frames=0`, so no alt-ref is possible there):
reference_select, compound_references, obmc, obmc_8x8, warped_motion, interintra,
interintra_wedge, free_partitions, ab_partitions, masked_compound, cdef,
an_altref_reference, delta_q_and_delta_lf, gradients_with_cdef (lag=25), and
a_golden_reference (lag=0 → none). Measured proxy for that recipe shape (the reference-select
arg list, 20 frames at lag=16, testsrc2 rather than each gate's own `gradients_source` seed):
**1 hidden frame per stream, decoded by us and compared by nothing**. The 40-frame variant of
the same recipe produced 0 (libaom's temporal filter absorbs the candidates), so the count is
content- and length-dependent — a per-gate sweep with each gate's own fixture is the honest
way to size it.

- deferred: sweeping `decode_all_frames_vs_oracle` over the 14 alt-ref-capable existing gate
  recipes — cheap per gate but 14 recipe transcriptions and 14 encodes; unblocked by an
  orchestrator decision (the helper it needs now exists and is one call per gate).
- accepted: the gate's fixture is `testsrc2` (deterministic lavfi source, no seed) rather
  than `gradients_source` (whose seed lavfi ignores — memory `seeded-fixture-not-reproducible`).

## Suite
`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib` (scoped, per charter): **273 passed, 0 failed, 22 ignored, 0 filtered**, 1060.84s (branch lane-hidden @ 25b7222).
