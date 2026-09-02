# lane-rect64port r1 — TX_64X32/TX_32X64 luma corner for a split intra strip

Base: main 78d8ff7. Commit: 35f33c9 (branch lane-rect64port).

## What changed
- `crates/ec-av1/src/decode.rs` — `decode_rect_split`: new `(64, 32) | (32, 64)` arm of the
  `luma_rect` table (guarded by `EC_RECT64_SPLIT`) plus the `luma_64corner` TU arm inside the
  transform-unit loop: `read_coeffs` on `TxbSet::Luma64` with `default_scan(TX32)`,
  txb_skip ctx 0, `Some((tx_w, tx_h))` for `av1_nz_map_ctx_offset`, corner copied into a
  `tx_w*tx_h` grid, `dequant_and_inverse_typed_wh` + `reconstruct_rect` +
  `record_mi_luma_rect`. Hand-ported from lane-intra64split 5ffbaf3 (its only decode hunk).
- `crates/ec-av1/src/decode.rs` — `RECT64_CORNER_TU_HITS` thread-local + `rect64_corner_tu_hits(orient)`
  (0 = 64x32, 1 = 32x64).
- `crates/ec-av1/src/stream.rs` — `pub fn rect64_corner_tu_hits`.
- `crates/ec-av1/examples/decode_probe.rs` — prints `rect64_corner_tu: 64x32=.. 32x64=..`.
- `refusal_inventory.rs` / `gate_coverage.rs`: UNCHANGED — the refusal is not lifted (see below).

## Witness attempt — FAILED to produce a pixel comparison
The arm fires on both 10-bit 3840x1608 cuts, and the frontier moves, but the frame that
contains the shape never completes: it stops on a DIFFERENT refusal owned by the inter side,
`"a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding"`
(decode.rs:19156, `reject_residual && !skip && !rect_inter_residual_supported(..)`).
Truncating cut 0 to its 33 decoded frames and decoding gives `rect64_corner_tu: 0 0` — every
hit is inside the first REFUSED frame. So there is no frame to compare against ffmpeg, no gate
can be written, and the refusal stays.

EVIDENCE: ~/.cache/rect64port-tmp/hg300.log, ~/.cache/rect64port-tmp/full.log | decode_probe (release) on the two pinned film cuts, default vs EC_RECT64_SPLIT=1 | cut A: 33 frames dumped both ways, refusal "…transform unit is 32x64…" -> "…needs rectangular residual coding", counter 64x32=16 32x64=2; cut B: 1 frame dumped, same refusal swap, counter 64x32=0 32x64=2
EVIDENCE: ~/.cache/rect64port-tmp/t33.log + out.raw (389007360 B = 21 frames) | trunc.py cut A to 33 frame OBUs, decode with EC_PROBE_OUT16 | OK 21 frames 3840x1608, rect64_corner_tu 64x32=0 32x64=0 — the shape is confined to the refusing frame

## Residue
- fix-now(next lane): the 64-level split intra strip needs the INTER rect-residual refusal at
  decode.rs:19151 lifted first; only then does a film frame carrying a 64x32/32x64 split intra
  strip finish and a pixel-exact witness gate become writable. Unblocked by: a lane that ports
  rectangular residual coding for non-skip HORZ/VERT/HORZ_B inter strips.
- deferred: the witness fixture `hg_rect64_witness.obu` and the gate
  `a_10bit_film_inter_frame_with_a_64x32_split_intra_strip_decodes_pixel_exact` — nothing to pin
  while every candidate frame refuses.

## Suite
`cargo test -p ec-av1 --lib -j3` (systemd unit, EC_AV1_REQUIRE_AOMENC=1):
417 passed, 3 failed, 37 ignored — identical totals and identical failing names to base 78d8ff7
(a_frame_edge_straddling_band, a_real_aomenc_inter_sequence_with_16x16_level_1to4_partitions,
real_aomenc_1to4_streams_..._rect_vartx_leaves_fire). No regression; refusal_inventory,
gate_coverage, split_transform_intra_strip and intra_1to4 tests are among the 417 passed.
EVIDENCE: $HOME/.cache/rect64port-suite-r1.log | full lib suite on 35f33c9 | 417/3/37, the 3 known reds
