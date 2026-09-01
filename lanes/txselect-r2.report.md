# lane-txselect r2 — the frame-2 divergence: a missing eob_pt class dimension

Branch `lane-txselect`, on top of `fb6b74f` (r1's report commit, off main `3808cf8`).

## Root cause

`read_eob` picks between the 2D and the `TX_CLASS_HORIZ`/`TX_CLASS_VERT`
eob_pt CDF (`decode.rs:1384`, libaom `decodetxb.c:200`
`eob_multi_ctx = (tx_class == TX_CLASS_2D) ? 0 : 1`). Only the 4x4 and 8x8
LUMA sets ever carried a class-1 sibling (`eob_pt_16_luma_class1`,
`eob_pt_64_luma_class1`, added by lane-av1tx4 r5); every other `TxbSet` had
`eob_pt_class1: None`, so a 1D-class TU silently read (and adapted) the
class-0 table. Instance of [[cdf-row-held-constant]] / the class dimension
half of [[table-indexed-by-raw-size]].

`TxMode::Select` is what made it reachable: a 32x32 inter block's var-tx
leaf is a **16x16** luma TU, whose inter tx set is
`EXT_TX_SET_DTT9_IDTX_1DDCT` — the only 16x16 set holding `V_DCT`/`H_DCT`.
(16x16 intra reads `TX_SET_INTRA_2`, all-2D; 32x32 and up read
`DCT_IDTX`/`DCTONLY`, also all-2D — so 16x16 inter is exactly the hole.)

Sweep of the same shape: every reachable size now carries its sibling —
`eob_pt_256_luma_class1` plus the three chroma ones
(`eob_pt_256/64/16_chroma_class1`; an *inter* block's chroma TU inherits its
luma unit's `tx_type`, so 1D classes reach chroma too). 32-point and larger
need none, by their tx sets.

## What changed

- `crates/ec-av1/src/cdf.rs` (EOF): `EOB_PT_256_LUMA_CLASS1*`,
  `EOB_PT_256_CHROMA_CLASS1*`, `EOB_PT_64_CHROMA_CLASS1*`,
  `EOB_PT_16_CHROMA_CLASS1*` — `av1_default_eob_multi{256,64,16}_cdfs[q][plane][1]`,
  `token_cdfs.h:773/811/851`, all four q-contexts.
- `crates/ec-av1/src/cdf_state.rs`: the four fields, their `pick(q_ctx, …)`
  defaults, their lines in the per-frame counter reset (class
  [[cdf-counter-not-reset]]), and `eob_pt_class1: Some(…)` on
  `Luma16`/`Luma16Inter`/`Luma16InterSet1`/`Chroma16`/`Chroma8`/`Chroma4`.
- `crates/ec-av1/src/stream.rs`: the gate body becomes
  `tx_select_inter_gate(bit_depth)` with two `#[test]` arms — the existing
  8-bit one and a new `…_decodes_pixel_exact_10bit` (`-b 10
  --input-bit-depth=10`, `yuv420p10le` fixture under `-strict -1`, compared
  through `ffmpeg_decode_sequence_10bit`).

No refusal string changed this round (r1 lifted the blanket
`TxMode::Select` one; it is now actually proven).

## Method (what found it, in order)

1. Pinned the r1 failing stream: seed-43 recipe → `s43.obu`,
   sha256 `d0ba79bbdd34317fa3864c824026e0b905dea0d9add540e646281a1ae23e3d57`.
2. `EC_AV1_PREFILT_DUMP` ours vs instrumented aomdec: frame 2 already
   differed BEFORE the loop filter (1721 luma samples, chroma clean) — so
   not the deblock tx grid, which was r1's leading suspect.
3. `EC_TRACE_MODE_STEP` matched everywhere (it only instruments the intra
   path); `EC_TRACE_COEFF` on both, compared by msac RANGE: the first
   divergent element is the **`eob` of the 16x16 luma var-tx leaf at
   mi (4,8)** of the 32x32 block at mi (0,8) — entry `rng=42600`
   (`all_zero`) and `rng=33264` (`tx_type`) identical, then ref
   `eob=248 rng=65032` vs ours `eob=247 rng=56584`.
4. `EC_EOBPT_DUMP` on aomdec gave `tx_size=2 eob_multi_size=4 txs_ctx=2`;
   ours read `tx_type value=1 len=13` = `V_DCT` (class VERT) yet took the
   class-0 CDF. `eob_pt_class1: None` on `Luma16InterSet1` was the line.

## Gate — GREEN, both arms

```
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-txselect EC_AV1_REQUIRE_AOMENC=1 \
  cargo test -p ec-av1 --lib a_real_aomenc_inter_sequence_with_tx_select -- --nocapture
```

EVIDENCE: cargo test stderr | 4-frame 64x64 mandelbrot inter sequence, aomenc
default tx-mode (`--enable-tx-size-search` deliberately absent), decode_stream
vs ffmpeg per frame, 8-bit and 10-bit arms | `txselect gate 8-bit seed=42:
txfm_split reads=22 splits taken=3`, `txselect gate 10-bit seed=42:
txfm_split reads=25 splits taken=6`, `test result: ok. 2 passed; 0 failed`.

EVIDENCE: /tmp/…/scratchpad/{ref,ours}.f0..f3 | `EC_AV1_PREFILT_DUMP` on
instrumented aomdec and on `decode_probe s43.obu` (r1's own failing seed-43
stream) | `cmp -l` 0 differing bytes on all four frames' Y+U+V pre-filter
planes (was 0 / 0 / 1721 / 3647 before the fix).

10-bit note: the 10-bit arm passes, so this lane owes lane-hbdinter nothing.

## Suite

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib` (full, this branch):
**269 passed / 0 failed / 23 ignored**, 877.86s. The earlier run of the same
command before the 10-bit arm was added was
**268 passed / 0 failed / 23 ignored** (r1: 267/1/23, the one failure being
this lane's gate).

## Film probe (0.4 s, `-c:v copy -f obu`)

- Troy: `REFUSED: unsupported: AV1 tile (a 32x32 partition type this decoder
  does not code (value=4))`
- Hunger Games: `REFUSED: unsupported: AV1 tile (a partition below 8x8 (this
  decoder codes no leaf smaller than 8x8))`

Both are other lanes' refusals, reached in the first tile's partition tree —
strictly upstream of anything this round touched, so the "before" value is
identical by construction (not separately rebuilt: `deferred(nothing)` —
measuring it would mean a second full build of `fb6b74f` for a string that
cannot move).

## Residue

- accepted (documented corner-cut, unchanged from r1, `decode.rs:11447`): a
  var-tx block's **chroma** deblock edges follow `tx_h_grid / 2`, i.e. the
  luma leaves, though the block's uv transform is not split. Ceiling: a
  chroma deblock edge at a luma-only transform seam. Not observable in this
  gate (`--enable-cdef=0`, and both arms are chroma-exact through 4 frames);
  upgrade path = a per-plane tx grid, not a scalar.
- accepted: the two narrow refusals r1 added stay (a var-tx leaf >32x32; an
  intra block in an inter frame whose `tx_depth` splits).
