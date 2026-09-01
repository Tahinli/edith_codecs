# lane-hbdgates r1 — 10-bit gates for the seven 8-bit-only tools

His two films are yuv420p10le, so an 8-bit-only gate proves nothing about the streams that
matter. lane-covbd r1 named seven tools positively enabled by an 8-bit gate and by no 10-bit
gate. Six are now closed with real-aomenc 10-bit gates; the seventh (restoration) has its
gate written and it FAILS on a real 10-bit defect.

## Result table

| tool | gate | result | first diff |
|---|---|---|---|
| filter-intra | `a_real_aomenc_10bit_intra_tools_stream_decodes_pixel_exact` | PASS, 6/6 attempts pixel-exact, `filter_intra_hits=2` | — |
| smooth-intra | same gate | PASS, `smooth_pred_hits=16` | — |
| paeth-intra | same gate | PASS, `paeth_pred_hits=3` | — |
| intra-edge-filter | same gate | PASS, `intra_edge_filter_hits=22` | — |
| rect-partitions | `a_real_aomenc_10bit_rect_and_ab_partitions_decode_pixel_exact` | PASS, 12/16 attempts pixel-exact (4 named refusals), `rect_partition_hits=32` | — |
| ab-partitions | same gate | PASS, `partab_hits=4` | — |
| restoration | `a_real_aomenc_10bit_restoration_stream_decodes_pixel_exact` | **FAIL — `#[ignore]`d, real 10-bit LR defect** | seed 42, frame 0, plane Y, index 122 (row 0, col 122): ours **351** vs ffmpeg **350**, on a stream where `lr_hits=2` |

## What changed
- `crates/ec-av1/src/intra.rs`
  - `smooth_pred_hits()` / `paeth_pred_hits()` (counted at the top of `predict`) and
    `intra_edge_filter_hits()` (counted inside `filter_intra_edge`, strength != 0).
    Separate counters per tool on purpose: the pre-existing `smooth_uv_hits` lumps
    SMOOTH..=PAETH together, so a paeth-only stream would have "proved" smooth intra
    ([[gate-blind-to-feature]]).
- `crates/ec-av1/src/stream.rs`
  - `ten_bit_tool_gate(name, w, h, frames, extra, seeds, counters)`: drives one 10-bit
    aomenc recipe over a seed sweep, decodes each attempt, pixel-compares EVERY shown
    frame against ffmpeg's own 10-bit decode, and sums each tool counter **per attempt,
    only over attempts that decoded and compared** (class `counter-from-refused-stream`).
    Named decode refusals are tolerated and reported; a pixel mismatch panics with
    plane + first-diff index + both values + the per-attempt firing deltas; no SKIP path.
  - `encode_10bit_gradients_seed(...)`: the existing helper with the fixture seed exposed;
    `encode_10bit_gradients` now delegates with seed 42 (all three prior call sites
    unchanged). A caller-supplied `--cq-level` now REPLACES the built-in `--cq-level=30`
    instead of being appended after it (two `--cq-level` on one aomenc line is a coin flip).
  - the three gates above.
- `crates/ec-av1/src/gate_coverage.rs`
  - `gate_bodies()` now (a) drops `#[ignore]`d gates — an ignored gate exercises nothing and
    must not close a hole — and (b) recognises gates that spell `--passes=1` inside the
    shared 10-bit helpers (`ten_bit_tool_gate` / `encode_10bit_gradients`); `is_ten_bit`
    likewise. 45 -> 49 gates parsed, 4 -> 7 of them 10-bit.
  - six entries deleted from `NEVER_EXERCISED_10BIT` (filter-intra, smooth-intra,
    paeth-intra, intra-edge-filter, rect-partitions, ab-partitions); `enable-restoration`
    kept with the failing-gate reason inline. 19 -> 13 holes.

## Gate commands + results
- `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -- --nocapture --test-threads=1 10bit_intra_tools`
  -> `6/6 attempts pixel-exact, filter_intra_hits=2 smooth_pred_hits=16 paeth_pred_hits=3 intra_edge_filter_hits=22`
- `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -- --nocapture --test-threads=1 rect_and_ab`
  -> `12/16 attempts pixel-exact, rect_partition_hits=32 partab_hits=4`
- `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -- --nocapture --test-threads=1 10bit_restoration`
  (run with the `#[ignore]` overridden) -> FAIL, message above.
- `cargo test -p ec-av1 --lib gate_coverage -- --nocapture` -> 5 passed, 0 failed;
  `gate_coverage: 49 real-aomenc gates, 7 of them 10-bit`, `NEVER_EXERCISED_10BIT (13 of 26)`.

EVIDENCE: crates/ec-av1/src/stream.rs (3 new gates) | EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib | intra-tools 6/6 pixel-exact (4 counters non-zero), rect+ab 12/16 pixel-exact (rect=32 partab=4), 10-bit LR gate fails Y[122] 351 vs 350 with lr_hits=2

## Refusals lifted
None — this lane authors gates only; no decode behaviour changed. The rect/ab gate's 4
non-matching attempts are named refusals reported by the gate, not silenced.

## Residue
- **fix-now (follow-on lane): the 10-bit loop-restoration defect.** Pinned recipe:
  `encode_10bit_gradients_seed(name, 42, 192, 128, 1, ...)` with `--cq-level=15
  --enable-restoration=1 --enable-cdef=0` (full list in the gate). `lr_hits=2` on the
  failing stream, so the LR filters ran; the mismatch is one LSB at 10 bits on the very
  first row, the shape of an unscaled box sum / an 8-bit clamp bound. Un-ignore the gate
  with the fix; do NOT weaken it. lane-hbdinter reports fixes of that shape on its own
  branch — if that merges first, re-run this gate before assuming the defect is still open.
- accepted: `enable-angle-delta`, `enable-cfl-intra`, `enable-1to4-partitions`,
  `enable-rect-tx`, `enable-tx64`, `enable-flip-idtx`, `enable-global-motion`,
  `enable-intrabc`, `enable-dist-wtd-comp`, `enable-dual-filter`, `enable-ref-frame-mvs`,
  `enable-superres` remain 10-bit holes — they are holes at 8 bits too (or flag-spelling
  aliases), so they belong to the lanes that gate them at all, not to this one.
