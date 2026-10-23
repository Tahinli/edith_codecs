# Lane av1svc: AOM conformance 242/242 (spatial SVC: scaled references and output policy)

## Goal

Base: `lane/av1conf` at `a19bc09b`. Make `av1-1-b8-22-svc-L2T1-2.ivf` and
`av1-1-b8-22-svc-L2T2-2.ivf` bit-exact. Both are two spatial layers (640x360 base, 1280x720 top).
Result: **242/242 exact**. The two vectors were in `KNOWN_FAILING`; the list is now empty.

## Result

- `aom_conformance`: `aom vectors: 242/242 exact (611 shown frames), 0 refused by name, 0 known failing, 0 failed`.
  Run on host a (`EC_AV1_THREADS` default) and host b.
- Each shown frame of the two SVC vectors matches the libaom `.md5` (8 frames each, 1280x720).
- ffmpeg (libdav1d) cross-check: `ffmpeg -c:v libdav1d -i <v> -f framemd5` gives 8 frames per vector, and every
  frame hash equals the matching line of the libaom `.md5` (both vectors). So the first frames (frame 0 `d059f1db...`,
  1280x720) agree with dav1d.

## Changes

### 1. Scaled-reference prediction (`crates/ec-av1/src/mc.rs`)

- New `RefScale { x, y }` (spec 7.11.3.3 `xScale`/`yScale`, Q14): `RefScale::new(ref_w, ref_h, cur_w, cur_h)`,
  `is_unscaled()`, `UNSCALED`, and `out_of_limits()` (2x down / 16x up, per axis).
- The scaled entry points (`predict_scaled`, `predict_scaled_kern`, `predict_maybe_scaled`,
  `predict_compound_intermediate[_kern]`) take `RefScale` instead of an x-only `i64`.
- When `yScale != 1<<14`, the new `scaled_vertical_sums` runs the spec 7.11.3.4 two-pass walk: the
  intermediate has `((h-1)*yStep + 1023 >> 10) + 8` rows from `startY >> 10`, and each output row takes its own
  phase `((startY & 1023) + yStep*r) >> 6 & 15`. Positions are `startY = Round2Signed((y_q4+8)*yScale - (8<<14), 8) + 32`.
  The x-only path (superres) and the unscaled path are unchanged.
- Test `scaled_spec_tests::scaled_prediction_matches_the_spec_transcription`: 800 random cases (8 and 10 bit,
  both axes and one axis scaled, all three filter kinds, luma and compound) against a literal transcription of the
  spec steps. Passes.

### 2. Decoder plumbing (`decode/*.rs`, `encode/split.rs`, `encode/inter_search.rs`)

- Removed `RefPix::compatible`, the rule that dropped any non-LAST reference whose size differed from LAST.
  That rule was not in the spec, and it hid the base layer's references from the top layer.
- `frame_tile.rs`: the height refusal ("a reference picture whose height does not match...") is gone. A reference
  outside the 2x down / 16x up limits is refused by name: "a reference picture outside the 2x down / 16x up scaling limits".
- `frame_size: (usize, usize)` is threaded explicitly through `decode_inter_block`, `decode_inter_block8`,
  `decode_inter_sub8_split4/rect2`, `obmc_plan` and `obmc_prediction`. Scale is `scale_of(dims, frame_size)`.
  A first attempt stored the height in `FrameCtx`; recon workers use a copy without it and divided by zero.
- Warp gating (`ref_is_scaled`, compound `ref_unscaled`, global warp) now uses `!is_unscaled()`, which is
  libaom's `av1_is_scaled` on both axes. Before, only the width counted.
- OBMC neighbours carry `RefScale` (`ObmcNb.scale`).

### 3. Output policy (`stream/mod.rs`)

- New `LayerOutput { LastShownPerTemporalUnit (default), AllShown }` and `decode_stream_with_layers`.
  `decode_stream_with` keeps its signature and uses the default (libaom `output_all_layers = 0`).
- The plan `last_shown_per_temporal_unit` reads only OBU headers and the `show_existing_frame` / `show_frame`
  bits of frame OBUs (spec 5.9.2). The policy applies only when some OBU carries `spatial_id > 0` and the stream
  has temporal delimiters. Otherwise every shown frame is forwarded, as before. Hidden frames are always forwarded.
- Unit tests in `stream/tests/layers.rs` (4): last shown per TU, no spatial layers gives no plan, no TD gives no plan,
  `show_existing_frame` counts as shown.

### 4. Test

- `tests/aom_conformance.rs`: `KNOWN_FAILING` is `&[]`. The test asserts 242/242 with the same per-frame md5 logic.

## edith check (read-only)

edith calls `ec_av1::stream::decode_stream_with(&stream, |picture, _, shown| ...)` in
`engine/src/decode.rs`, `engine/src/export.rs` and `engine/tests/av1_software_export.rs`, and
`decode_stream` in `engine/tests/av1.rs`. The signatures are unchanged, so nothing breaks. Behaviour changes only for
streams with spatial layers: they now deliver one frame per temporal unit (the top layer), as libaom and dav1d do.
Use `decode_stream_with_layers(.., LayerOutput::AllShown, ..)` to get every shown frame.

## Verify commands and results

All run with `CARGO_TARGET_DIR=~/.cache/tgt-av1svc TMPDIR=~/.cache/tmp-av1svc`, through `scripts/remote-run.sh av1svc`.

1. `EC_AV1_AOM_DIR=$HOME/fixtures/av1-aom EC_REQUIRE_FIXTURES=1 EC_MEMGUARD_MAX=6G cargo test --release -p ec-av1 --test aom_conformance -- --nocapture --test-threads=1`
   on host a and host b: `242/242 exact (611 shown frames)`, `test result: ok`.
2. `cargo test --release -p ec-av1 --lib -- layers scaled_spec` on host b: 5 passed.
3. `cargo clippy -p ec-av1 --all-targets -- -D warnings` on host b: exit 0.
4. `cargo fmt`: rustfmt is not installed on the server. Changed files were formatted with the local rustfmt,
   and only the lines this lane wrote were touched.
5. Full suite: `EC_MEMGUARD_MAX=6G cargo test --release -p ec-av1 -- --test-threads=4` on host a, run on the
   merged tree before the last `main` merge (log `~/tmp/av1svc/full-suite2.log`): lib `816 passed; 0 failed;
   49 ignored`, all other test binaries ok. An earlier run (before the refusal-inventory and t01 fix) had 3 failures
   from the removed height refusal; those are fixed.
6. After merging `main` `6a56fb9e` (av1brokengop, one 10-line hunk in `stream/mod.rs`, clean merge): `aom_conformance`
   on host a: `242/242 exact (611 shown frames)`, `test result: ok`. `clippy -p ec-av1 --all-targets -D warnings`
   on host a: exit 0.

## Found, not fixed

- **Worker panic hangs the threaded decode.** When a recon worker panicked (my divide-by-zero, now fixed), the
  decode did not return: the test binary sat at 0% CPU with no result. The panic is reported on the worker, but the
  parse thread waits on a slot promise that never resolves. This is a robustness gap in the threaded path, which
  predates this lane.
- **Reference-size check covers all seven references.** The refusal fires for any ref slot outside the scaling limits,
  even one no block uses. libaom errors only when a block uses it. This is stricter than libaom and never hit by the
  240 vectors.
- **Coverage.** The two SVC vectors are the only streams with scaled references. The transcription test pins the
  kernel, but it does not show which block paths (OBMC over a scaled neighbour, sub-8x8 scaled chroma, compound
  scaled) those two vectors exercise.
- Superres combined with a scaled reference is not exercised.

## Not met / left

- Nothing blocking for 242/242. The items above are follow-ups.
