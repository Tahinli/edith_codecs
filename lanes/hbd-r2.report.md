VERDICT: Done. `cargo check -p ec-av1 --lib --tests` reports 0 errors (42 -> 0),
and `cargo test -p ec-av1` (`EC_AV1_REQUIRE_AOMENC=1`) is 263 passed, 0 failed,
18 ignored -- 8-bit output is bit-exact. Real commit 213eeac on top of two
`wip(hbd):` checkpoints (177aa1c at 42->5, 6e6dbbb at --lib clean/17 test
errors left).

## What actually needed doing

r1's saved diff had already widened `PlaneBuf::data`, intra prediction, and
all six of `mc.rs`'s prediction/blend functions to `u16` -- the charter's own
"mc.rs last, it unlocks ~24 call sites" order was already moot; `mc.rs` was
done. The 42 errors were all downstream call sites that hadn't caught up:

- `warp.rs`'s `warp_affine` (`reference`/`dst` still `&[u8]`/`&mut [u8]`,
  only caller is 3 `decode.rs` sites) -- widened.
- `decode.rs`'s OBMC chain (`obmc_neighbour_pred`, `obmc_blend_v`,
  `obmc_blend_h`, `obmc_blend`) and interintra chain (`interintra_blend`,
  its local `intra` scratch buffer) -- both fed by the same `pred_y`/
  `pred_u`/`pred_v` buffers `read_inter_plane`'s call sites already needed
  widened, so one `vec![0u8; ...]` -> `vec![0u16; ...]` sweep across those
  buffer declarations (12 sites) plus the two function chains closed 37 of
  the 42 lib errors.
- `read_inter_plane`'s own `prediction: &[u8]` parameter -> `&[u16]`.
- `encode.rs` (3 call sites into `intra::predict`) and `motion.rs` (1 call
  site into `mc::predict`) -- see decision below.

## The encoder decision (charter asked for one)

The encoder's own buffers (`Plane::source`, `Plane::reconstruction`, the
motion-search `reference`/`source`) stay `u8` -- this round does not widen
the encoder, and nothing forces it to: the encoder is 8-bit-only by design
(`Picture`'s `y`/`u`/`v` are still `Vec<u8>`, unchanged this round), so there
is no wider content for it to ever hold. Widening those buffers to `u16`
would have added real diff for zero behavioural change and speculative
10-bit-encode plumbing nobody asked for.

Instead, each of the 4 sites gets a local `u8<->u16` round-trip box right at
the call into the now-widened shared `intra::predict`/`mc::predict`:
- `encode.rs`: `intra_predict_u8` (new free fn, `crates/ec-av1/src/encode.rs`
  near `pad_plane`) wraps `intra::predict`, used at all 4 of encode.rs's
  `crate::intra::predict` call sites (`trial`, `intra_scores`,
  `search_block`, and a `#[cfg(test)]` micro-bench).
- `encode.rs::mc_trial`: inline conversion around its one `mc::predict` call.
- `motion.rs::search_traced_from_step`: `reference` converted once, outside
  the per-MV-candidate closure (not per-call), so the search's inner loop
  cost is unchanged.

A block-sized `u8`->`u16`->`u8` round-trip is exact for 8-bit content (every
value fits both ways with no rounding), so none of this changes encoder
output -- and the RD-trial/motion-search test suite (`encoder::tests::*`,
`motion` module) stayed green with no changes needed.

## Test-only fixes (not caught by `--lib`)

`--lib` hit 0 errors after the above; `--tests` still had 17, all fixture
buffers in `mc.rs`'s and `decode.rs`'s own unit tests declared `Vec<u8>`/
`vec![0u8; ...]` and calling the now-`u16` `predict`/`predict_with_filter`/
`combine_compound`. Mechanical widen of the fixture types (one hand-written
`u8` window array in `mc.rs`'s
`smooth_filter_matches_aomdec_chroma_inter_block` stays `u8` as the aomdec
dump's own literal shape, copied element-wise into a `u16` reference plane
instead of `copy_from_slice`). No test assertion changed shape, only types.

## Files touched

- `crates/ec-av1/src/warp.rs` -- `warp_affine` signature.
- `crates/ec-av1/src/decode.rs` -- `read_inter_plane` signature; OBMC chain;
  interintra chain; 12 `pred_y`/`pred_u`/`pred_v` buffer declarations across
  4 near-identical inter-block-decode sites; one unit test's fixture types.
- `crates/ec-av1/src/mc.rs` -- unit test fixture types only (functions were
  already widened).
- `crates/ec-av1/src/encode.rs` -- new `intra_predict_u8` helper + 4 call
  sites; `mc_trial`'s inline conversion.
- `crates/ec-av1/src/motion.rs` -- `search_traced_from_step`'s inline
  conversion.

## Checks run

- `cargo check -p ec-av1 --lib -j4` -- 0 errors (was 42 at round start).
- `cargo check -p ec-av1 --tests -j4` -- 0 errors.
- `cargo check -p ec-hw --lib -j4` -- 0 errors (only other in-tree crate
  depending on ec-av1; no `PlaneBuf`/`mc`/`intra` internals are exported
  across that boundary, confirmed by a clean check with no `ec-av1` diff
  visible to it).
- `cargo test -p ec-av1 -j4` (`EC_AV1_REQUIRE_AOMENC=1`): **263 passed, 0
  failed, 18 ignored**, 177.58s. Every real-aomenc gate (obmc, masked
  compound, reference-select, restoration, temporal MVs, warp, superres)
  still fires and still matches pixel-exact -- 8-bit decode/encode output is
  unchanged.

## Scope respected

`bit_depth != 8` refusal untouched (not searched for/touched this round);
no 10-bit gate, no second `ffmpeg_decode_sequence` helper -- both explicitly
next round's job per the charter.

## Budget

Landed well inside the 75-turn budget (checkpoint-then-converge discipline
wasn't needed this round; the crate reached 0 errors and a green suite in
one pass). Two `wip(hbd):` checkpoints exist on the branch per the round's
policy even though the whole thing closed in one sitting.
