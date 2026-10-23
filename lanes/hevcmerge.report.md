# lane/hevcmerge report

## Goal

Merge `lane/hevcintra` (pixel pipeline, base e2168158) with `main` (dccd4503: hevcmvpred motion derivation, hevcmc, hevcpwtfix) into one branch `lane/hevcmerge`.

## Base

- hevcintra: e2168158
- main: dccd4503
- branch `lane/hevcmerge` created from e2168158, then `main` merged in.

## Resolution rules applied

- Kept hevcintra's pixel pipeline: PicState, recon, deblock, SAO, u16 planes, all_intra.
- Kept main's real motion: `SliceMotion` built via `motion_field::collocated` and `slice_motion`. No `ZeroMotion` on real slices.
- The per-picture `MotionField` is stored on the DPB picture (`dpb.set_motion` with the compressed `ColField`).
- All module declarations kept from both sides (`motion_field`, `pic`, `sao`, `weighted`, and the rest).
- Inter pixels remain placeholder. There is no MC wiring in this lane.

## Changes

- `crates/ec-h265/src/dec/inter.rs`: took hevcintra's version. Main's `decode_inter_slice` removed (only the old test used it).
- `crates/ec-h265/src/dec/mod.rs`:
  - Imports: `cu_syntax::{parse_slice, parse_slice_into}`, `motion_field::{ColField, MotionField, SliceMotion, SliceRefs}`, `pic::PicState`.
  - `CurPicture` carries `pic`, `all_intra`, and `motion: Option<MotionField>`. Main's `pixels` field dropped.
  - `decode_slice` builds the real `SliceMotion` once and passes it to both the syntax-check path (`parse_slice`) and the pixel path (`parse_slice_into`).
  - `slice_motion` takes its fields separately to avoid borrow conflicts.
  - `finish_picture` sets `pic.motion = field.deblock_units()` before deblocking, then stores the compressed `ColField` in the DPB and updates `motion_checksum`.
- Bug fix (bS identity): hevcintra's `cu_syntax::store_motion` wrote `ref_pic = ref_idx`, which is not a picture identity. Removed `store_motion` and its two calls in `cu_syntax.rs`. Added `MotionField::deblock_units()` in `motion_field.rs`, which sets ref identity to POC through the slice's `SliceRefs`. Intra and undecoded blocks get defaults. The bS motion source is now the MotionField only.
- `crates/ec-h265/src/pic.rs`: doc comments updated (`PuMotion.ref_pic` is POC; motion filled from the motion field before deblocking).
- `crates/ec-h265/tests/cu_syntax.rs`: resolved to main's let-chain style. It calls `cu_syntax::parse_slice` with `ZeroMotion` for a standalone sidecar parse, because that test has no DPB to derive real motion from. The doc comment and print message were updated ("inter syntax: ... P/B slices parsed").

## Verify

1. `cargo clippy -p ec-h265 -p ec-h265-syntax --all-targets -- -D warnings`: PASS (exit 0).
2. `cargo test -p ec-h265 -p ec-h265-syntax --release`: PASS, all suites ok (exit 0). Suites: ec-h265 unittests 79/79; conformance 15/15; conformance_intra 1/1; cu_syntax 4/4; dec_meta 4/4; dec_shell 13/13; decode 5/5; encode 4/4; perf 3/3; rdoq_cache_hash 3/3; scaling_lists 0 run, 1 ignored; ec-h265-syntax unittests 29/29, fuzz 2/2, sei_sweep 1/1.
   - conformance_intra exact-intra count: **269 intra pictures checked, 269 exact, 100/147 streams accepted** (`--nocapture` run, exit 0).
   - hevcmvpred determinism over conformance streams: **147 streams, 0 slice failures**, `motion_derivation_is_deterministic_over_conformance_streams` ok (`--nocapture` run in package ec-h265, exit 0). The first attempt used `-p ec-h265-syntax` and failed to find the target; the test is in package ec-h265.
3. `cargo check --workspace --all-targets`: see the result line below.

## What is left

- Inter MC wiring: inter pixels are still placeholder. Wiring hevcmc into the pixel path is the next lane.
- The `cu_syntax` standalone test still uses `ZeroMotion`. Switching it to real motion needs a DPB-backed harness.
