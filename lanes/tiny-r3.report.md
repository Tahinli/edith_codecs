# lane-tiny r3 report

## Coefficient range ladder result

Added `EC_TRACE_COEFF` per-symbol range tracing to `read_coeffs` (decode.rs,
tags `all_zero`, `tx_type`, `eob`, `base_eob`, `after_bases`, `sign`,
`post_golomb`) matching the oracle's existing `decodetxb.c` instrumentation
(same var name, same tags -- oracle needed two more trace points added,
`all_zero`/`tx_type` entry ranges, rebuilt via `ninja aomdec` in
`~/.cache/aom-oracle/build`).

Ran both against the pinned seed-45 32x32 fixture
(`$SCRATCHPAD/seed45.obu`, CFL + filter_intra DC_PRED block,
`filter_intra_mode=3`).

**`all_zero` matched exactly** (ctx=1, all_zero=0, rng=52448 both sides) --
the very first symbol of the coefficient layer was clean.

**Divergence: the very next symbol, `tx_type`.** Ours exited at rng=37340,
oracle at rng=42464. Direction: both consumed a symbol (range narrowed from
the same 52448 entry state) but landed on different final ranges -> a wrong
CDF *row* was read, not a missing/extra read (both sides read exactly one
tx_type symbol here).

## Root cause

`av1_read_tx_type` (libaom `decodemv.c:646`): for an **intra** block whose
`filter_intra_mode_info.use_filter_intra` is set, the `intra_ext_tx_cdf` row
index is

```c
const PREDICTION_MODE intra_mode =
    mbmi->filter_intra_mode_info.use_filter_intra
        ? fimode_to_intradir[mbmi->filter_intra_mode_info.filter_intra_mode]
        : mbmi->mode;
```

`fimode_to_intradir = { DC_PRED, V_PRED, H_PRED, D157_PRED, DC_PRED }`
(`blockd.h:180`) -- **not** the block's ordinary `mode`, which is always
`DC_PRED` whenever filter-intra is active (spec: filter-intra blocks always
carry `mode == DC_PRED`, the interesting information is in
`filter_intra_mode`). Every `read_plane` call site in `decode.rs` passed the
block's plain `mode` (here `DC_PRED == 0`) as `tx_mode` regardless of
filter_intra, so the CDF lookup always used the DC_PRED row instead of
`D157_PRED` (index 6) for `filter_intra_mode=3`. Wrong row -> wrong symbol ->
correctly-consumed bits but wrong resulting range and wrong `tx_type`, and
everything downstream (scan order, base/br contexts) reads off the wrong
axis from the very first coefficient TU on.

## What shipped

**Fix**, `crates/ec-av1/src/decode.rs`, `read_plane` (the single shared
chroma/luma coefficient entry point every multi-TU and single-TU luma call
routes through): when `plane_idx == 0` and `filter_intra` is `Some(mode)`,
remap `tx_mode` through `FIMODE_TO_INTRADIR = [DC_PRED, V_PRED, H_PRED,
D157_PRED, DC_PRED]` before the `cdfs.txb(set, tx_mode)` lookup. Added
`H_PRED`/`D157_PRED` consts (matched libaom's numbering: DC=0, V=1, H=2,
D157=6, verified against `crates/ec-av1/src/intra.rs`'s existing constants).

One fix point covers every caller (chroma is unaffected by construction --
`plane_idx != 0` never touches the new branch, and chroma has no
filter-intra in AV1 to begin with).

**Gate**: the existing `probe_tiny_frame_size_boundary` diagnostic sweep
(size x seed matrix, real aomenc + ffmpeg oracle) now shows:

| size  | before | after |
|-------|--------|-------|
| 32x32 | 3/10   | **10/10** |
| 24x24 | 3/10   | **10/10** |
| 16x32 | 2/10   | **10/10** |
| 32x16 | 2/10   | 8/10 (residual, see Open) |
| 16x16, 48x48, 64x64 | 10/10 | 10/10 (unaffected, already clean) |

This is a `#[ignore]`d diagnostic, not a hard-asserting gate (matches its
pre-existing shape from r1/r2; converting it into a hard-asserting
`stream.rs` gate in the `a_real_aomenc_*` style is the natural next step but
did not fit this round's budget -- flagged in Open below).

## Investigated and REJECTED as a fix

Chased a second lead from the same trace session: the chroma `txb_skip_ctx`
computation (`usize::from(around.0) + usize::from(around.1)`, range 0..2)
looked wrong against libaom's `get_txb_ctx`'s chroma branch, which is
`get_entropy_context(...) + ctx_offset` where `ctx_offset` is 7 (chroma
plane_bsize == tx size, i.e. always true here since chroma is never split
into multiple TUs in this decoder). Oracle traced `ctx=7` where ours prints
`ctx=0` for the identical read -- **but this is not a bug**: our
`txb_skip_chroma_8`/`_16`/`_32`/`_4` CDF tables (`cdf_state.rs`) are already
pre-sliced 3-row extracts of the full 13-row spec table taken at the correct
7/10 offset (`TXB_SKIP_CHROMA_8` etc., confirmed by grep at their
initializer). Index 0 in our local table *is* spec index 7 -- the two
printed "ctx" numbers are in different local bases for the same CDF row,
which is exactly why the traced range matched exactly in that instance.
Applying a `+ 7` there panicked immediately (`index out of bounds: len 3,
index 7`) on the very first refused-8x8-then-16x16 case, confirmed the
tables are chroma-relative already, and was reverted in full before commit
(`git diff` verified clean, no residue in the shipped commit).

## Open (not shipped this round)

- **32x16 residual, 2/10 seeds**: `first_diff=None, ndiff_luma=0/512` --
  luma is bit-exact, only chroma differs, for exactly 2 of 10 seeds at one
  specific rectangular aspect (32x16, not its transpose 16x32 which is now
  10/10). Not characterized further this round (budget). Not gated or
  refused -- this is a real "wrong pixels returned as success" gap per the
  charter's own standard, scoped narrower than what shipped fixed. Next
  round: re-run the same `EC_TRACE_COEFF`/`EC_TRACE_MODE_STEP` ladder on one
  of the two failing 32x16 seed-47/42 fixtures (pin them first, aomenc
  output is seeded but re-verify determinism per
  `seeded-fixture-not-reproducible.md`) to find the actual desync point --
  it is chroma-only, so start with the chroma predict path / CFL /
  uv_predict_mode rather than the luma tx_type fix's territory.
- `probe_tiny_frame_size_boundary` is still `#[ignore]`d and non-asserting.
  Promoting it to a hard-asserting gate under `stream.rs` (model:
  `a_real_aomenc_*_decodes_pixel_exact`, `EC_AV1_REQUIRE_AOMENC=1`,
  hard-assert a hit counter, no printed SKIP on decode error) is the
  natural follow-up once the 32x16 residual is closed too -- gating it now
  would either need to accept 32x16 as a known-flaky size (against the
  charter's own "never mismatch" rule) or hard-code a size exclusion, both
  of which are worse than finishing the fix.

## Verification

- `cargo test -p ec-av1 --lib`: **266 passed, 0 failed, 22 ignored** (221s).
- `cargo check --workspace --all-targets`: clean, 0 errors.
- HEAD: `1a89ed9` (branch `lane-tiny`).

EVIDENCE: `cargo test -p ec-av1 --lib -- --ignored --nocapture probe_tiny_frame_size_boundary` | 32x32/24x24/16x32 3-or-2-of-10 -> 10/10, 32x16 2/10 -> 8/10, all other sizes unchanged at 10/10 | before/after table above
EVIDENCE: EC_TRACE_COEFF range ladder, seed45.obu, oracle vs ours | all_zero ctx=1 all_zero=0 rng=52448 matches both sides; tx_type exit rng 37340 (ours) vs 42464 (oracle) is the first divergent element | fixed by filter-intra tx_type CDF row remap, commit 1a89ed9
