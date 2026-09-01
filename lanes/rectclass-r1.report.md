# lane-rectclass r1 — rect transform readers × tx class

Branch `lane-rectclass` off main `df5d630`. Worktree
`/home/tahinli/Documents/Code/Rust/edith_codecs-rectclass`.

## Verdict on the charter's premise

The verifier finding ("`read_coeffs_rect` hardcodes `TxClass::TwoD` and is the reader
for LUMA rect TUs whose ext-tx set yields `TX_CLASS_HORIZ`/`VERT`") is **false on this
tree, by construction**, and the measurement gate below now pins that.

`read_coeffs_rect` is the ONLY rectangular coefficient reader in the decoder, and every
one of its six call sites is an INTRA path whose rect TU has a 2D type by spec:

| # | reader (decode.rs:line) | call site | shape(s) | `TxbSet` | tx_type source | classes reachable | scan | eob row | nz ctx | br ctx |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | `read_coeffs_rect` 2327 | 4191 | chroma 16x8/8x16/32x16/16x32 of a split-tx HORZ/VERT strip (`chroma_rect_tables` 4052-4055) | `ChromaRect16x8` / `ChromaRect32x16` (`tx_type: None`) | `default_intra_tx_type(uv_mode)`, forced `DCT_DCT` at >=32 (`EXT_TX_SET_DCTONLY`) | **2D only** — every `Intra_Mode_To_Tx_Type` entry is 2D | caller's `SCAN_*` (2D) | `TxClass::TwoD` row | `base_ctx_rect` (2D) | `br_ctx_rect` (2D) |
| 2 | `read_coeffs_rect` 2327 | 4487 | **luma** 32x16/16x32 intra strip | `LumaRect32x16` (`tx_type: None`) | `DCT_DCT` — `txsize_sqr_up(TX_32X16) == TX_32X32` => `EXT_TX_SET_DCTONLY` for intra, no symbol exists | **2D only** | `SCAN_32X16`/`SCAN_16X32` | `TwoD` | 2D | 2D |
| 3 | `read_coeffs_rect` 2327 | 4528 / 4568 | chroma 16x8/8x16 of the same strip | `ChromaRect16x8` | `default_intra_tx_type(uv_mode)` | **2D only** | `SCAN_16X8`/`SCAN_8X16` | `TwoD` | 2D | 2D |
| 4 | `read_coeffs_rect` 2327 | 5021 / 5075 | chroma 32x16/16x32 of a superblock-level strip | `ChromaRect32x16` | `DCT_DCT` (chroma sqr_up >= 32) | **2D only** | `SCAN_32X16`/`SCAN_16X32` | `TwoD` | 2D | 2D |
| 5 | `read_coeffs` 2124 with `rect_shape: Some((bw,bh))` | 4946 | luma 64x32/32x64 SB strip, coded as its 32x32 corner | `Luma64` (`tx_type: None`) | `DCT_DCT` (sqr_up > TX_32X32) | **2D only** | `default_scan(32)`, class scan available | class-aware (`eob_pt_class1: None` on `Luma64`, never taken) | `base_ctx` full class support, rect position table | `br_ctx` full class support |
| 6 | `read_plane` 5814 / `read_inter_plane` 9910 -> `read_coeffs` | many | SQUARE only (`side x side`) | all `Luma*`/`Chroma*` | coded symbol or inherited/derived default | all three classes | `class_scan_table(side)` | class-1 row per `TxbSet` | class-aware | class-aware |
| 7 | `decode_leaf_rect8` (lane-tx4x8) | — | — | — | — | **not on this base** (`git grep decode_leaf_rect8` = nothing) | — | — | — | — |

Every rect shape whose ext-tx set DOES contain `V_DCT`/`H_DCT`/`V_ADST`/... —
i.e. `txsize_sqr_up <= TX_16X16` rect (16x8, 8x16, 8x4, 4x8, 16x4, 4x16) and every
INTER rect leaf — is refused by name *before* any reader:
`refusal_inventory.rs` "a coded (non-skip) HORZ/VERT rect strip below 16x16",
"a coded (non-skip) HORZ_B/VERT_B rect strip below 16x16",
"a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding",
"an inter partition below 16x16 other than SPLIT". Those refusals belong to lane-rect16 /
lane-tx4x8 / the inter lanes; the 1D-on-rect work is *their* prerequisite, and this lane
did NOT lift them (COMMON: a refusal is lifted only together with a gate that fires, and
no gate can make a 1D class reach a rect reader while those refusals stand).

So no class-correctness bug exists to fix here today. What was missing was the
**measurement** — nothing proved the 2D-only claim against a real encoder — and a trap:
if lane-rect16 lifts its refusal, `read_coeffs_rect` silently becomes wrong. Both are now
addressed by a counted, hard-asserting gate.

Corrected premise (ledger `constraint|merge lane-gmaffine into main: ... Luma4Inter/
Luma4InterSet1 ... need eob_pt_16_luma_class1`): **not needed**. `Luma4Inter`/`Luma8Inter`
read `INTER_TX_TYPE_SET3_*`, a two-symbol set whose only members are `IDTX` and `DCT_DCT`
(`cdf.rs:1447` doc + `TxType::from_symbol`, transform.rs:364) — both `TX_CLASS_2D`, so
`eob_pt_class1: None` is correct there, not a gap. The `*Set1` (ALL16) twins, which can
be 1D, do carry the class-1 row.

## What changed

- `crates/ec-av1/src/decode.rs:869-895` — new `RECT_COEFF_TU_HITS` / `RECT_CLASS1_REFUSALS`
  thread-local counters + `rect_coeff_tu_hits()` / `rect_class1_refusals()` accessors.
- `crates/ec-av1/src/decode.rs` `read_coeffs_rect` — counts a rect TU that actually coded
  coefficients (`all_zero == 0`), and counts each of the reader's two 2D-only refusals
  before returning them. No behaviour change.
- `crates/ec-av1/src/stream.rs` — new gate
  `a_real_aomenc_stream_with_a_1d_tx_class_on_a_rect_transform_decodes_pixel_exact`.
- `crates/ec-av1/src/gate_coverage.rs` — `enable-flip-idtx` off `NEVER_EXERCISED_10BIT`,
  `enable-tx-size-search` off `NEVER_ON_10BIT`: the new gate spells both `=1` at 10 bits
  and pixel-compares four 10-bit decodes.
- `crates/ec-av1/src/refusal_inventory.rs` — the two rect capability claims now name the
  gate that is their evidence and why they hold by construction.

## Gate

    cd /home/tahinli/Documents/Code/Rust/edith_codecs-rectclass
    export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-rectclass EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1
    nice -n 10 cargo test -p ec-av1 --lib -j3 a_real_aomenc_stream_with_a_1d_tx_class -- --nocapture

Recipe: lane overrides FIRST (class `aomenc-first-flag-wins`) —
`--enable-rect-partitions=1 --enable-tx-size-search=1 --enable-flip-idtx=1
--min-partition-size=8 --max-partition-size=32`, then the base recipe; 12 attempts,
seeds 42..47, alternating 8-bit / 10-bit (`--input-bit-depth=10 --bit-depth=10`, y4m
needs `-strict -1`), 192x128 gradients. Every successful decode is pixel-compared against
ffmpeg (luma+U+V); a decode error is only tolerated if it contains "unsupported"; the
rect-TU counter is folded in PER ATTEMPT after that attempt compared; the class-1 refusal
counter is summed over ALL attempts including refused ones (that is exactly where a 1D
class on a rect TU would surface). Asserts: `matched > 0`, `matched_10bit > 0`,
`compared_rect_tus > 0`, `class1_refusals == 0`.

EVIDENCE: $HOME/.cache/rectclass-suite.log + gate stdout | 12 real aomenc encodes (6 seeds x 8-bit/10-bit) decoded and pixel-compared vs ffmpeg | 8 pixel-exact decodes (4 of them 10-bit), 4 named refusals, 10 rect coefficient TUs on compared attempts, 1D-class-on-rect refusals: 0

Measurement recorded per the charter: aomenc at these settings never puts a 1D tx class
on a rect TU that reaches a reader — not because RD avoids it, but because the shapes
whose sets contain 1D types are refused earlier. Widening the recipe (`--tune-content=screen`,
cq sweep) cannot change that while those refusals stand, so it was not run.

## Test totals

`cargo test -p ec-av1 --lib` (EC_AV1_REQUIRE_AOMENC=1): see
`$HOME/.cache/rectclass-suite.log` — result line at the tail.

## Residue

- deferred(lane-rect16 / lane-tx4x8 lifting their sub-16x16 rect refusals): the 1D path in
  `read_coeffs_rect` (mrow/mcol rect scan, class-1 `eob_pt` row for `ChromaRect16x8` &
  friends, 1D `base_ctx_rect`/`br_ctx_rect` arms). Building it now would be dead code that
  no gate can reach, and lifting the refusal without a firing gate is forbidden by COMMON.
  The gate's `class1_refusals == 0` assert is the tripwire that fails the moment that lane
  lands, with the exact TODO in its message.
