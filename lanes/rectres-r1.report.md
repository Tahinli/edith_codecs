# lane-rectres r1 — the 32x32-level 1:4 inter strip's rectangular residual

## Step 1 MEASURE — the shape histogram (this is the whole finding)

An env-gated one-liner at the refusal site (`crates/ec-av1/src/decode.rs`, in the
`reject_residual` block, `EC_RECTRES=1`) prints `(w, h, side, skip, is_inter, mi_row, mi_col)`.
Six probes, `EC_RECT64_SPLIT=1 EC_RECTRES=1`, each under `systemd-run --scope -p MemoryMax=6G`:

| stream | refusing shape | skip | is_inter | at |
|---|---|---|---|---|
| 10-bit 3840x1608 cut 0 | **32x8** | 0 | 1 | mi 16,744 |
| 10-bit 3840x1608 cut 300 | **32x8** | 0 | 1 | mi 0,64 |
| 10-bit 1920x792 @900 | **8x32** | 0 | 1 | mi 24,148 |
| 10-bit 1920x792 @5400 | **32x8** | 0 | 1 | mi 20,448 |
| 10-bit 1920x792 @6300 | **8x32** | 0 | 1 | mi 0,56 |
| 10-bit 1920x792 @8100 | **32x8** | 0 | 1 | mi 10,184 |

6 of 6 are the 32x32-level 1:4 strip. No 64-axis shape, no AB footprint, nothing else.
The charter's "most likely 32x64/64x32 and 64x16/16x64" premise is WRONG for these films —
those shapes are already supported and the films never stop on them.

EVIDENCE: shell probe, 6 streams | `EC_RECT64_SPLIT=1 EC_RECTRES=1 decode_probe <cut>` | histogram above, 32x8 x4 / 8x32 x2, 0 other shapes

## Step 2 IMPLEMENT — one shape family

`max_txsize_rect_lookup[BLOCK_32X8] == TX_32X8`, all 256 positions coded.

* `crates/ec-av1/src/cdf_state.rs` — new `TxbSet::LumaRect32x8Inter`: the existing INTRA
  `LumaRect32x8` tables (`get_txsize_entropy_ctx(TX_32X8)` = TX_16X16 -> luma-16 coefficient
  tables; `txsize_log2_minus4` = 4 -> 256-position `eob_pt`) **plus** a `tx_type` table the
  intra twin has none of. `av1_get_ext_tx_set_type` at `tx_size_sqr_up == TX_32X32` is
  `EXT_TX_SET_DCT_IDTX` for inter with *or without* `reduced_tx_set` (no branch), read at the
  `txsize_sqr_map[TX_32X8] == TX_8X8` row -> `inter_tx_type_8`. Class
  `table-indexed-by-raw-size`: the coefficient tables follow the ADJUSTED size and the
  `tx_type` row the SQUARE one, and here the two disagree.
* `crates/ec-av1/src/decode.rs` `rect_inter_residual_supported` — `+ (32, 8) | (8, 32)`.
* `crates/ec-av1/src/decode.rs` `rect_inter_luma_set` — `(32, 8) | (8, 32) => LumaRect32x8Inter`.
* `crates/ec-av1/src/decode.rs` `rect_inter_chroma_set` — `(16, 4) | (4, 16) => Chroma8`
  (`ss_size_lookup[BLOCK_32X8]` = BLOCK_16X4; TX_16X4 entropy ctx = TX_8X8, 64-position `eob_pt`).
* Counter `RECT32X8_INTER_TU_HITS` `[32x8, 8x32]` + `stream::rect32x8_inter_tu_hits()`, and
  `decode_probe` prints `rect32x8_inter_tu:`.

Nothing else was needed: `SCAN_32X8`/`SCAN_8X32` already exist (the intra strip path uses them),
`sub_tx_size_map(32,8) == (16,8)` is an already-supported rect var-tx leaf, and
`read_inter_plane_rect`/`read_coeffs_rect` are shape-general.

## Step 3 GATE

`a_real_aomenc_inter_sequence_with_32x32_level_1to4_strips_codes_their_rect_residual`
(`crates/ec-av1/src/stream.rs`) — real aomenc, `--enable-1to4-partitions=1
--enable-rect-partitions=1 --enable-tx-size-search=1 --min-partition-size=8
--max-partition-size=32` (per-arm overrides last: aomenc keeps the last `--enable-*`),
16 attempts x {8-bit, 10-bit}, both orientations, every decode-order frame compared on Y/U/V
against ffmpeg, refusals counted never SKIPped, `oos_mismatch == 0` asserted, and one
hard assert per lifted shape on the PIXEL-COMPARED counter (a refused stream's hits are
tallied separately, class `counter-from-refused-stream`).

Recipe found by a 72-run sweep (192x128..256x192, cq 24..60, bands 8/16/32/64): only
**32-pixel** motion bands at **cq >= 48** make aomenc code a NON-SKIP 32x32-level 1:4 strip.
Narrower bands give the same strips with `skip` set — they prove the partition symbol and
nothing about the residual (class `gate-blind-to-feature`).

```
cargo test -p ec-av1 --lib a_real_aomenc_inter_sequence_with_32x32_level_1to4_strips -- --nocapture
8-bit : 0 named refusals, 5 pixel-exact attempts carrying the arm, 11 carried none (0 mismatched)
10-bit: 0 named refusals, 4 pixel-exact attempts carrying the arm, 12 carried none (0 mismatched)
pixel-compared units 32x8=7 8x32=8 (decoded anywhere 32x8=7 8x32=8)
test result: ok. 1 passed; 0 failed
```

EVIDENCE: /home/tahinli/.cache/rectres-gate2.log | aomenc 32 streams (8+10 bit) -> decode -> ffmpeg Y/U/V compare | 15 pixel-compared 32x8/8x32 residual TUs, 0 mismatches

`refusal_inventory.rs`: the string stays (sub-8x8 and AB footprints still have no rect
residual path) with a comment recording the narrowing. `gate_coverage.rs` needs no change —
it derives from `--enable-*` flags and this gate turns no tool off that others turn on.

## Film frontier — before / after

hg cut 0 (10-bit 3840x1608, 73 frame OBUs): **33 shown frames before, 33 after.** The gain is
INSIDE frame 34, not across frames: at prefix 50 that frame now decodes a 32x8 residual unit
(`rect32x8_inter_tu: 32x8=1`) and stops on a *different* wall —
`an intra 16x4/4x16 strip inside an inter 16x16-level 1:4 partition`. All six cuts moved to
that same next refusal. So this lane removed one of at least two refusals in the films'
first blocking frame; the shown-frame count cannot move until the other lane lands.

EVIDENCE: /home/tahinli/.cache/rectres-bisect.log + probe on the 50-OBU prefix | trunc.py bisect over frame-OBU prefixes, decode_probe each | max OK prefix 49 OBUs = 33 shown frames; prefix 50 refuses on the intra-1:4 wall after coding one 32x8 residual TU

## rect64 refusal — NOT lifted

deferred: removing the `EC_RECT64_SPLIT` bypass — no witness exists. The 49-OBU prefix that
decodes clean reports `rect64_corner_tu: 64x32=0 32x64=0`; every rect64 hit (22 + 4) lives
inside the frame that still refuses on the intra 16x4/4x16 wall. Confirms the ledger dead-end
`lane-rect64port r1`. Unblocked by: the intra-16x4/4x16-in-inter-1:4 lane.

## Suite

See the r1 handoff/report tail for `cargo test -p ec-av1 --lib` totals.
