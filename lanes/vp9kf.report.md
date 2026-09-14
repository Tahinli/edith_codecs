# lane-vp9-kf report — first VP9 software-decode lane (`ec-vp9`)

Status: **HANDOFF — decode pipeline complete end-to-end, byte-exactness not yet achieved.**
Charter file: `lanes/vp9kf.charter.md`. Branch `lane-vp9-kf` (base 29f35b77). NOT merged, NOT pushed.

## What works (verified by running code)

- Crate `crates/ec-vp9` compiles (`cargo check -p ec-vp9` clean of errors; a handful of
  unused-variable warnings remain). `deny(unsafe_code)`, deps = `ec-core` + `ec-vp9-syntax` only.
- `bool.rs` — VP9 range coder (spec 8.3.2) with carry-propagating reference-encoder
  roundtrip tests: **7/7 lib tests pass**.
- Full keyframe pipeline runs end-to-end on `fixtures/vp9/key-320.ivf`
  (320x240, 2 keyframes, generated with the charter's ffmpeg command): superframe split →
  `Vp9Parser` uncompressed header → compressed header (tx_mode, tx/coef/skip diff-updates
  with `inv_remap_prob`/`decode_term_subexp` verbatim from libvpx v1.15.0) → tiles
  (4-byte size prefixes) → partition recursion → intra mode info → tokens →
  intra prediction → transforms → residual add → loop filter → cropped `Picture`.
- `profile1_444_is_named_unsupported` PASSES (refuses `vp9 profile 1` via
  `fixtures/bitstreams/vp9-profile1-444.ivf`).
- Constant tables (`src/tables/gen_tables.rs`) extracted mechanically from libvpx v1.15.0
  (coefficient probs incl. band-0 padding, pareto8 model, scans + neighbors, kf/uv mode
  probs, partition/tx/skip probs, common_data lookups, subsize/ss_size lookups).

## What fails

- `keyframes_match_ffmpeg` — our reconstruction diverges from
  `ffmpeg -v error -i <ivf> -frames:v 1 -f rawvideo -pix_fmt yuv420p -` at pixel (0,0)
  (ours 133 vs 72); ~90% of pixels are the plane-fill value, i.e. the entropy decode or
  reconstruction is systematically wrong from the first transform block.
- `inter_is_named_unsupported` — refused frames do error, but the panic-vs-Error path was
  being reworked when the budget ran out; needs a re-run after the entropy fix.

## HANDOFF — where to look next (in order of suspicion)

1. **Entropy desync AT THE UV-MODE READ of the first block** (latest probe): a Python
   replica of the bool decode over the same tile bytes agrees with our Rust through
   partition(0)/skip(0)/y-mode(2=H), but the uv-mode read diverges (replica: 7=D207 vs
   Rust: 4=D135). Check `kf_uv_probs`/`KF_UV_MODE_PROB` row indexing (y*9) and the
   `UV_MODE_TREE` leaf mapping (DC,V,H,D45,D135,D117,D153,D207,D63,TM order) against the
   replica in the lane transcript. The first tx block then EOBs immediately (eob bit 0),
   i.e. the correct (0,0) is pure prediction (129 for H at the frame corner) + later
   blocks' overlap; our residual decode never even starts.
2. **Entropy desync at/inside the first transform block.** Evidence: first-block probs
   verified correct (`8x8 band0 ctx0 = [125,34,187]`), compressed header consumed 82 bytes
   with 0 overreads, defaults untouched by updates — so the divergence starts inside token
   reading or right after it. Dump the first 64x64's full read sequence with `EC_VP9_TRACE=1`
   and hand-check against the spec 8.5.2 token flow.
2. **Verify the "no partition symbol at 4x4 level" decision** (`decode.rs`,
   `if n4 == 1 { 0 }`): our stream traced 3 extra NONE reads then SPLIT(=subsize 255)
   before the removal, strongly indicating the spec behavior (partition only >= 8x8) — but
   double-check `has_rows/has_cols` edge handling at 4x4 against `read_partition` in
   `~/.cache/vp9-ref/vp9_decodeframe.c:1195`.
3. **Sub-8x8 chroma gating**: implemented as "chroma tokens/prediction once per 8x8, on the
   TL sub-block" (`decode.rs`, `sub && plane != 0` gate). Verify against libvpx
   (`predict_and_reconstruct_intra_block` runs chroma per sub-block; the real gating lives in
   `read_intra_frame_mode_info`/`n4_w` semantics — confirm).
4. **Dequant**: `dq=[38,44]` for q=130 — sanity-check `dc_q(8,130)` against the spec 8.6.1
   table (the syntax crate's `dc_q`/`ac_q` are trusted, but the wiring
   `segment_dequant(seg)[luma_dc|luma_ac]` should be printed once per frame).
5. **Loop filter edge rules** (`loopfilter.rs`): per-edge re-derivation of libvpx's
   `build_masks` semantics (documented in the module head); the 32x32/64x64 border
   promotions and the z-(0,0)-slot chroma rule for 8x8 regions need one careful re-read
   against `vp9_loopfilter.c:652-790` once pixels match pre-filter.
6. Debug traces still present: `EC_VP9_DBG` eprintlns in `header.rs`, `tokens.rs`,
   `loopfilter.rs` — remove when done. `tests/dump_probe.rs` is a scratch harness; delete.

## Verification commands

```
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9 cargo check -p ec-vp9
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9 cargo test -p ec-vp9 --lib
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9 cargo test -p ec-vp9 --test keyframe_exact -- --nocapture
```

## Provenance notes

- libvpx v1.15.0 reference sources cached at `~/.cache/vp9-ref/` (test oracle; never linked).
- Table extractor: brace-matching + macro-resolving Python over the C initializers
  (comments stripped, all arrays flattened, ragged band-0 padded).
- No edits to `ec-vp8`, `ec-hw`, `edith`, or `ec-vp9-syntax`. Workspace `Cargo.toml` untouched
  (members glob covers the new crate, same as `ec-vp8`).
