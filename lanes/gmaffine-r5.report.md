# lane-gmaffine r5 — the 8x8-leaf chroma residue was the NARROW-BLOCK SHARP interp filter

Commit on `lane-gmaffine`, on top of r4 (`f5e1851`). Suite (before the new unit test,
after the fix): `EC_AV1_REQUIRE_AOMENC=1 cargo test --release -p ec-av1 --lib -j3` →
**273 passed, 0 failed, 24 ignored** (r4: 271/2/24 — the two 8x8 motion gates are the
delta, both now green at 8-bit AND 10-bit).

## Root cause (one line, one table entry)
`mc.rs:187` `InterpFilterKind::Sharp => (&SUBPEL_FILTERS_SHARP, &SUBPEL_FILTERS_SHARP)`.
Spec 7.11.3.4's `filterIdx`: a prediction block 4 or fewer samples wide/high reads
`Subpel_Filters[4]` — the REGULAR narrow kernel — for BOTH `EIGHTTAP` and
`EIGHTTAP_SHARP`; libaom agrees literally (`filter.h:239-245`,
`av1_interp_4tap[MULTITAP_SHARP] == av1_sub_pel_filters_4`). We used the 8-tap sharp
kernel on the narrow axis, and the old doc comment above `SUBPEL_FILTERS_SHARP` asserted
as fact that "spec 7.11.3.4 never swaps this for a narrow-block table" — false.
Invisible above 4 samples, so it could only ever bite the 4x4 chroma of an 8x8 (or
smaller) luma leaf: exactly the blocks r4 left red. Fixed to
`(&SUBPEL_FILTERS_SHARP, &SUBPEL_FILTERS_4)`; all three MC paths
(`predict_with_filters`, `predict_scaled`, `predict_compound_intermediate`) resolve
their kernels through `InterpFilterKind::tables()`, so this one entry is the whole
sweep — grep of `SUBPEL_FILTERS_SHARP|SUBPEL_FILTERS_4|SUBPEL_FILTERS_SMOOTH_4` finds
no other consumer.

## The two charter experiments, both answered NEGATIVE before the real cause
Experiment 1 (chroma inverse forced to `DCT_DCT`): pixel error went from 3 chroma
blocks to 25 in frame 1 (u/v byte diffs 40/45 → 270/259, whole-clip 3585 → 27819) —
the inherited 1D types are right and used right.
Then the decisive instrument: `~/.cache/gmaffine-r5/inv.c`, a C harness linking the
oracle's `libaom.a` and calling `av1_inv_txfm2d_add_4x4_c` directly (needs
`av1_rtcd()`/`aom_dsp_rtcd()` first, else the txfm func pointers are NULL and it
segfaults). Every distinct 4x4 dequantized grid our decoder produced on the gate
stream — 1476 blocks, 16 distinct tx types incl. 246 `H_ADST` / 248 `H_DCT` /
59 `IDTX` — reproduces libaom's residual BIT-EXACT. The 4x4 inverse basis, the
identity scaling, the 1D-class scan and the nz ctx are therefore all exonerated;
so is Experiment 2 (the `ext_tx_used` clamp: this stream is `reduced_tx_set == 0`,
so the chroma 4x4 inter set is `EXT_TX_SET_ALL16`, which contains `H_ADST` — the
clamp is inert here; `side >= 32` stand-in left as-is, see residue).
With the transform exonerated the residual is right, so the error had to be the
chroma PREDICTION — one 4-sample-wide MC block.

EVIDENCE: `/home/tahinli/.cache/gmaffine-r5/{gm1_cq32.obu,ours.yuv,ref.yuv,f2.yuv,diff.py}`
| `gen.sh 32 1` reproduced the gate's own 8-bit cq32 stream twice, sha256
`22d09738baab612910c83106cb4fefe860ed20de1b2a01b36c5d3c717d5b18bc` both times (r4's
hash); `EC_PROBE_DUMP=… decode_probe gm1_cq32.obu` vs `ffmpeg -i … -pix_fmt yuv420p`
| before: 3585 differing bytes over 24 frames (luma 0, U/V only, first block chroma
(8,12) frame 1); after the one-entry fix: **0 differing bytes, all 24 frames, all 3
planes**.

EVIDENCE: `/home/tahinli/.cache/gmaffine-r5-suite.log` | `EC_AV1_REQUIRE_AOMENC=1
cargo test --release -p ec-av1 --lib -j3` | `test result: ok. 273 passed; 0 failed;
24 ignored` in 545s.

EVIDENCE: same log + `cargo test … -- 8x8_leaf` | both lane gates run at 8-bit and
10-bit | `a_real_globalmv_8x8_leaf_stream_decodes_pixel_exact ... ok`,
`a_real_warped_causal_8x8_leaf_stream_decodes_pixel_exact ... ok`,
sibling `a_real_aomenc_inter_sequence_with_an_8x8_leaf_split_decodes_pixel_exact ... ok`
(its 10-bit twin stays `#[ignore]`d from lane-inter8 r2, untouched by this round).

## Refusals
The GLOBALMV / WARPED_CAUSAL 8x8-leaf refusals r1 removed are now PROVEN by two green
real-aomenc gates that hard-assert `globalmv_hits_8` / `warp_hits_8` at both depths —
the r4 deviation (not restoring them) resolves in favour of the lift. No further
`refusal_inventory.rs` / `gate_coverage.rs` edit was needed this round; both tests are
green (`refusal_inventory::tests::*` 5 passed).

## Runnable check left
`mc.rs::tests::a_narrow_block_reads_the_regular_four_tap_kernel_under_every_sharpness`
pins the narrow kernel of all four `InterpFilterKind`s against spec 7.11.3.4.
Run: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-gmaffine EC_NOMEMGUARD=1 cargo test
--release -p ec-av1 --lib -- mc::tests` → 8 passed, 0 failed.

## Residue
- `read_plane`'s `side >= 32` stand-in for `av1_get_ext_tx_set_type`'s
  `av1_ext_tx_used` membership — accepted for now: it is exact for every set this
  decoder currently reaches (`reduced_tx_set == 0` inter/intra at ≤16 is ALL16/INTRA_1,
  both of which contain every type the CDF can emit), and no gate can distinguish it
  until a `reduced_tx_set == 1` inter stream is gated. deferred(a `--reduced-tx-type-set=1`
  aomenc gate) — not a defect this round could measure.
- Instrumentation from this round was removed before commit (the `EC_EXP1`/`EC_EXP_DUMP`
  hooks in `decode.rs` and `EC_TX_DUMP` in `transform.rs`); the C oracle harness lives
  outside the repo at `~/.cache/gmaffine-r5/inv.c` and is worth keeping — it is the
  only instrument that can settle "is our inverse transform the defect?" in one run.
