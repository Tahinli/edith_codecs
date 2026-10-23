# lane/hwav1grain: AV1 film grain on the ec-hw hardware decode path

Base: `main` at `bc48e903`. Branch `lane/hwav1grain`.

## Goal

ec-hw's AV1 hardware decode never applied film grain: `apply_grain` was hard-set to 0
(`crates/ec-hw/src/dec/av1.rs`, `picture_parameters`), so edith's
`av1::a_film_grain_av1_file_decodes_whole_and_keeps_its_grain` saw a clean picture on
the hardware path. Grain has to reach the picture the decoder hands out.

## Option chosen: (A) VA-API grain

Option (A) is the driver's own grain pass, and radeonsi runs it bit-exactly on this
machine (Mesa 26.2.4, gfx1200). Option (B) was rejected for three reasons:

1. `ec_av1::film_grain::apply_grain` takes a `&FrameCtx` (decoder-internal state for
   bit depth and subsampling), so a public entry would have to expose or rebuild that
   context. More invasive than the VA path.
2. Software grain on the read-back frame would leave the zero-copy DRM PRIME export
   (`Frame::export_prime`) carrying the clean picture unless a second grained copy were
   made, which is a CPU pass plus an upload per frame.
3. The VA path keeps the grained picture in a GPU surface, so `export_prime` carries it
   without any copy.

## What changed

- `crates/ec-hw/src/dec/av1.rs`
  - Every frame that is shown, or showable, with `apply_grain` gets a second pool
    surface. The driver writes the grained picture there through
    `current_display_picture` and `film_grain_info`. The decoded reconstruction stays
    grain-free in `current_frame`, so it remains the reference (spec 7.20 and 7.18.3).
  - Each reference slot keeps a grained copy next to its clean surface
    (`grained: [Option<Arc<PooledSurface>>; NUM_REF_FRAMES]`). A `show_existing_frame`
    outputs that copy, because spec 7.21 reloads the grain parameters of the frame it
    shows. The grain pass therefore runs once, at decode time, and is reused.
  - A `show_existing_frame` of a key frame refreshes the grained copies with the clean
    ones (same rule as the clean references).
  - A stream with `film_grain_params_present` allocates `NUM_REF_FRAMES + 2` more pool
    surfaces (24 instead of 14). Streams without grain are unchanged.
  - `wants_grain` and `film_grain_struct` are new helpers. The latter maps
    `ec_av1_syntax::FilmGrainParams` (including the ref-loaded params when
    `update_grain = 0`, already done by the parser) to `VAFilmGrainStructAV1`.
- `crates/ec-hw/src/params/av1.rs`: doc comment on `FilmGrainStructAV1` updated (it was
  "edith leaves apply_grain clear").
- `crates/ec-hw/tests/film_grain.rs` (new): decodes `fixtures/bitstreams/av1-grain.ivf`
  on the hardware, compares every frame with ec-av1 software decode and with ffmpeg
  (libdav1d, grain applied), and asserts both are bit-exact. It follows the
  `EC_REQUIRE_FIXTURES` convention, so an absent fixture fails when that variable is set.

Note on the earlier premise: the `hwfixes` report row "AV1 film-grain display surface"
recorded this as missing by design. It is now implemented.

## Fixture

The grain fixture is edith's `assets/test_av1_grain.mkv` (1280x720, 8-bit 4:2:0, 90
frames). It was extracted to IVF with (no re-encode):

    ffmpeg -i .../edith/assets/test_av1_grain.mkv -map 0:v:0 -c:v copy \
      -bsf:v av1_metadata=td=insert -f ivf fixtures/bitstreams/av1-grain.ivf

The file lives in the gitignored `fixtures/` directory and is not committed.

## Diagnosis that led to the fix

The first version (grain only on the decoded frames) was still wrong on half of the
output frames. Per-frame dumps showed the cause: 90 output frames, but only 48 are decoded
shown frames. The other 42 are `show_existing_frame` outputs, which the old code returned
as the clean reference. The driver grained all 48 decoded shown frames exactly, and the
grained frames matched the software output bit-exactly. The residual mismatch was entirely
the `show_existing_frame` path, which the fix above covers. I also checked and ruled out
readback sync (an explicit `vaSyncSurface` changed nothing), in-place grain (it corrupts
references, as expected), and a ping-pong pair of reserved grain surfaces (same result).

## Results

Measured per frame, 90 of 90 frames, 1280x720, 8-bit:

| comparison | max abs sample diff | PSNR |
|---|---|---|
| hardware with grain vs ec-av1 software (grain applied) | 0 (all 90 frames) | inf |
| hardware with grain vs ffmpeg/libdav1d (grain applied) | 0 (all 90 frames) | inf |
| hardware WITHOUT grain (old behaviour) vs ec-av1 software | 17 to 18 | about 35.2 to 35.5 dB |

The old behaviour is what the edith test was guarding against: the grain is worth about
17 to 18 sample levels and about 35 dB of PSNR. The grained hardware output is
bit-exact with both software decoders. That is what the spec's determinism gives, and it
means no driver-grain differences were observed. No option-(B) comparison was needed.

## Verify commands and results

    export CARGO_TARGET_DIR=~/.cache/tgt-hwav1grain TMPDIR=~/.cache/tmp-hwav1grain

1. Clippy, ec-hw and ec-va only (the ec-av1 lib lints block the full set; they were
   pre-existing and not added to):
       cargo clippy --no-deps -p ec-hw -p ec-va --lib --tests -j4 -- -D warnings
   Result: exit 0.
   Full set `-p ec-hw -p ec-va -p ec-av1` fails only on ec-av1's pre-existing lints
   (`empty line after doc comment`, `unnecessary parentheses`, `variable does not need to
   be mutable`), in `decode.rs`, `encode.rs`, `mc.rs`, `restoration.rs`, `warp.rs`,
   `tile.rs`. None are in files this lane touches.

2. New grain test:
       EC_REQUIRE_FIXTURES=1 cargo test -p ec-hw --release --test film_grain -j4 -- --nocapture
   Result: 1 passed (worst max abs diff 0 vs ec-av1, 0 vs ffmpeg).

3. Gate:
       EC_REQUIRE_FIXTURES=1 cargo test -p ec-hw --release -j4 -- --skip av1_encode_agrees \
         --skip av1_10bit --skip bd_row --skip av1_encode_speed --skip real_library --skip long_decode
   Result: exit 0. Suites: 17 lib, 1 film_grain, 33, 3, 2, and 1 passed, 0 failed.

## Not verified, and what is left

- Zero-copy export: `Frame::export_prime` exports `Frame::surface()`, which is now the
  grained surface. This follows by construction. A DRM PRIME import test was not run, so
  the export is not measured.
- edith's own test `av1::a_film_grain_av1_file_decodes_whole_and_keeps_its_grain` was not
  built or run here (it lives in the edith repo and needs `libengine_hw.so`). The
  condition it checks (grain reaches the picture) is what the bit-exact comparison above
  shows.
- Pool sizing: grain streams now allocate 24 surfaces instead of 14. At 4K NV12 (about
  12.4 MB each) that is about 300 MB of GPU memory, versus about 175 MB before. Whether that is acceptable on the
  target hardware is a call for the orchestrator.
- Grained copies are produced for every showable frame, including those that are never
  shown. This costs one grain pass per showable frame, and it avoids a later pass per
  `show_existing_frame`.
