# lane hevcrefusals: ec-h265 refuses out-of-scope HEVC by name

Base: `main` at `927fb3db`. Branch `lane/hevcrefusals`.

## Goal
DONE.md (ec-h265 decode, Done when): out-of-scope streams (RExt, SCC, multi-layer) refuse by
name with `Error::Unsupported`, never with a panic, a `Corrupt` error, or wrong pixels.

## What changed
- `crates/ec-h265/src/dec/mod.rs`: `check_sps` and `check_pps` name the feature.
  - Chroma: `chroma format 4:0:0 (monochrome)`, `4:2:2`, `4:4:4` (and `4:4:4 with
    separate_colour_plane_flag`). Before: `chroma_format_idc 2`, which did not name 4:2:2.
  - Bit depth: `bit depth 12 luma|chroma` (any depth above 10). Before: `bit depth above 10`.
  - Extensions: one name per flag via `refused_extension`: `range extension (RExt tools)`,
    `multi-layer extension (MV-HEVC, SHVC)`, `3D extension (3D-HEVC)`, `SCC extension (screen
    content coding)`. Before: one message, `range, multilayer, 3D or SCC extension`, which did
    not name `multi-layer` or `RExt` by name.
  - The module doc now lists 4:0:0, 4:2:2, 4:4:4 and 3D.
- `crates/ec-h265-syntax/src/ps.rs`: `Sps::to_rbsp` and `Pps::to_rbsp` write the
  `*_extension_present_flag` block from the stored `extension_flags`, so a fixture can carry a
  flag. The extension bodies are not written (the decoder never reads them). Output is unchanged
  when the flags are zero.
- `crates/ec-h265/tests/refusals.rs` (new, 15 tests) and `crates/ec-h265/tests/data/refusals/`
  (7 streams, 2.8 to 3.3 KB each, 64x64, 4 frames).

## Policy decisions
- **Multi-layer.** NAL units with `nuh_layer_id > 0` are skipped, so the base layer of an
  MV-HEVC/SHVC stream decodes. The test `layer_one_nal_units_are_skipped_and_base_layer_decodes`
  adds layer-1 slices and a layer-1 SPS with the multilayer flag, and checks the base-layer
  pictures are bit-identical to the plain stream. A base-layer SPS or PPS that itself carries the
  multilayer flag is refused by name.
- **VPS extension.** The VPS is parsed and its extension data is ignored, not refused. The base
  layer does not depend on it.
- **RExt.** Any `sps_range_extension_flag` or `pps_range_extension_flag` is refused, even when
  every RExt tool inside is off. Parsing the range-extension bodies would only pass a subset of
  streams that the spec allows; refusing the flag is the conservative choice.
- **3D.** Refused by name when the SPS or PPS carries the 3D flag. A base layer without it decodes.

## Stream sources
All streams are made locally with ffmpeg (libx265 build) and are committed under
`crates/ec-h265/tests/data/refusals/`:
- `main420_8.265`: `-pix_fmt yuv420p`, control.
- `main10_420.265`: `-pix_fmt yuv420p10le`, control.
- `yuv422_10.265`: `-pix_fmt yuv422p10le`.
- `yuv444_8.265`: `-pix_fmt yuv444p`.
- `main444_8.265`: `-pix_fmt yuv444p -profile:v main444-8`.
- `yuv420_12.265`: `-pix_fmt yuv420p12le`.
- `gray_400.265`: `-pix_fmt gray`.
- SCC, RExt, multi-layer and 3D: synthetic, from `main420_8.265` with one flag rewritten by
  `Sps::to_rbsp` / `Pps::to_rbsp`. x265 cannot write SCC, RExt tools, MV-HEVC, SHVC or 3D-HEVC.
- The JCT-VC conformance set in `fixtures/vectors/hevc-jctvc` (147 streams) is HEVC_v1 only.
  It has no RExt, SCC or MV-HEVC streams, and `scripts/fetch-vectors.sh` does not fetch them
  (the script says they live in other directories). Nothing further was downloaded.

## Before and after
"Before" is `main` at `927fb3db` for the decoder, with the new test file and the writer change.
Result before: 6 passed, 9 failed. Each failure was `Unsupported`, with no panic and no `Corrupt`
error. The failures are only about the name.

| Feature | Stream source | Before (what field) | After (what field) |
|---|---|---|---|
| 4:2:2 10-bit | ffmpeg libx265, yuv422p10le | `HEVC decode: chroma_format_idc 2` (FAIL: no "4:2:2") | `HEVC decode: chroma format 4:2:2` |
| 4:4:4 8-bit | ffmpeg libx265, yuv444p | `chroma_format_idc 3` (FAIL) | `chroma format 4:4:4` |
| 4:4:4 Rext profile | ffmpeg libx265, `-profile:v main444-8` | `chroma_format_idc 3` (FAIL) | `chroma format 4:4:4` |
| 4:0:0 monochrome | ffmpeg libx265, gray | `chroma_format_idc 0` (FAIL) | `chroma format 4:0:0 (monochrome)` |
| 12-bit 4:2:0 | ffmpeg libx265, yuv420p12le | `bit depth above 10` (FAIL: no "bit depth 12") | `bit depth 12 luma` |
| SPS RExt flag | synthetic (flag 0x80) | `SPS range, multilayer, 3D or SCC extension` (FAIL: no "RExt") | `SPS range extension (RExt tools)` |
| PPS RExt flag | synthetic (flag 0x80) | `PPS range, multilayer, 3D or SCC extension` (FAIL) | `PPS range extension (RExt tools)` |
| SCC, SPS | synthetic (flag 0x10) | refused, names "SCC" (pass) | `SPS SCC extension (screen content coding)` |
| SCC, PPS | synthetic (flag 0x10) | refused, names "SCC" (pass) | `PPS SCC extension (screen content coding)` |
| Multi-layer, SPS | synthetic (flag 0x40) | refused as "multilayer" (FAIL: no "multi-layer") | `SPS multi-layer extension (MV-HEVC, SHVC)` |
| Multi-layer, PPS | synthetic (flag 0x40) | refused as "multilayer" (FAIL) | `PPS multi-layer extension (MV-HEVC, SHVC)` |
| 3D, SPS | synthetic (flag 0x20) | refused, names "3D" (pass) | `SPS 3D extension (3D-HEVC)` |
| 3D, PPS | synthetic (flag 0x20) | refused, names "3D" (pass) | `PPS 3D extension (3D-HEVC)` |
| Layer-1 NALs (SPS and slices) | MAIN8 with layer-1 copies | base decodes (pass) | base decodes, identical pixels (pass) |
| Control: Main, Main 10 | ffmpeg libx265 | decodes (pass) | decodes (pass) |

No case panicked or returned `Corrupt` before the change. No case decoded wrong pixels.

## Verify
Run on host `c` through `scripts/remote-run.sh hevcrefusals --host auto`, with `EC_REQUIRE_FIXTURES=1`.
- `cargo test --release -p ec-h265 --test refusals`: 15 passed, 0 failed (after).
- `cargo test --release -p ec-h265 -p ec-h265-syntax -- --nocapture` with
  `EC_H265_REF_YUV_DIR="$HOME/refyuv/hevcinter"`: exit 0, all binaries pass
  (ec_h265 lib 86, conformance 15, conformance_intra 2, cu_syntax 4, dec_shell 13, decode 5,
  encode 4, encode_threads 3, fuzz_regressions 7, perf 3, rdoq_cache_hash 3, refusals 15,
  ec_h265_syntax lib 39, fuzz 2, fuzz_regressions 7, sei_sweep 1; 213 passed, 0 failed).
  The conformance line reads `total: 9203 pictures checked, 9203 exact, 147/147 streams accepted`.
- `cargo clippy -p ec-h265 -p ec-h265-syntax --all-targets -- -D warnings`: no warnings, no errors.
- `rustfmt --check` passes on the new test file. The edited blocks in `dec/mod.rs` and `ps.rs`
  follow the surrounding style. Pre-existing formatting differences in other parts of those
  files were left alone.

## Left
- The RExt tools themselves (4:2:2, 4:4:4, 12-bit decode) are not implemented; they are refused.
- No real MV-HEVC, SHVC, 3D-HEVC or SCC conformance streams were available to test against. The
  multi-layer and 3D cases are synthetic flag rewrites of a Main stream.
