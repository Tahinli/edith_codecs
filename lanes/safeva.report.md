# lane/safeva: ec-va and ec-hw without unsafe

## Result

- `ec-va` is rebuilt on `cros-libva` (git rev `59384456ac2ae78c0c3e5515f41ef1efd9b802cf`, BSD-3). It contains no `unsafe`, `sys.rs` is deleted, and it carries `#![forbid(unsafe_code)]`.
- `ec-hw` has `#![forbid(unsafe_code)]`. H.264 decode is ported to typed cros-libva buffers. HEVC, VP9 and AV1 decode and all encoders return `Error::Unsupported`.
- `cargo clippy -p ec-va -p ec-hw --all-targets -- -D warnings` is clean.
- GPU tests ran locally (AMD radeonsi, VA-API 1.23.0).

## Test counts (`cargo test --release -p ec-va -p ec-hw`)

| Binary | Baseline (main) | Now |
|---|---|---|
| ec-hw unit | 27 pass | 20 pass |
| ec-hw tests/film_grain | 1 pass | 1 pass |
| ec-hw tests/gpu | 41 pass | 24 pass, 17 ignored |
| ec-hw tests/idr_counting | 3 pass | 3 ignored |
| ec-hw tests/intra_rate | 2 pass | 2 ignored |
| ec-hw tests/reset | 1 pass | 1 ignored |
| ec-hw doctest | 1 pass | 1 pass |
| ec-va unit | 8 pass | 6 pass |
| ec-va tests/gpu | 14 pass | 14 pass |
| ec-va doctests | 6 pass | 1 pass |
| **Total** | **104 pass** | **67 pass, 23 ignored** |

The 14 fewer passing tests are removed unit tests from the deleted HEVC/VP9/AV1 modules, the sys.rs tests, and the picture.rs compile-fail doctests (the picture.rs API was rewritten).

Every ignored test has an `#[ignore = "safeva: ..."]` reason. The reason is either "needs an encoder, not ported to cros-libva yet" or "needs an HEVC decoder, not ported to cros-libva yet".

## H.264 decode

Decode-only H.264 tests that pass: `h264_matches_ffmpeg`, `h264_seek_matches_linear_open_gop`, `h264_frame_num_gaps_are_synthesised`, `sdr_h264_colour_never_panics`, `hardware_grain_matches_software_decoders`, `long_decode_does_not_grow_the_resident_set`, plus the H.264 unit tests in `dec::h264`. These compare against ffmpeg reference output, so they pass against the same references as on main. I did not run a byte-level diff of decoder output against the baseline worktree.

H.264 tests that are ignored: `h264_high_profile_still_decodes`, `h264_main_profile_has_no_high_syntax_and_decodes`, `h264_constrained_baseline_encodes_and_decodes`, `h264_slice_headers_count_frames_for_stateful_decoders`, `h264_frame_num_and_idr_pic_id_restart_at_every_idr`, `h264_intra_only_stream_is_one_idr_per_picture`, `our_encoder_and_our_decoder_agree`, `cbr_lands_within_five_percent_*`, `one_encode_in_its_own_process`, and the reset test. They build their input stream with the H.264 encoder, so they cannot run until the encoder is ported.

## TODO per codec

- H.264 encode: `ec-hw/src/enc/` is a stub (`Encoder` is uninhabited, `new` returns `Unsupported`). Needs typed `EncMiscParameter`, `EncSequenceParameter`, `EncPictureParameter`, `EncSliceParameter`, and coded buffer readback via `MappedCodedBuffer`.
- HEVC encode: same as H.264 encode, plus the HEVC typed parameter buffers.
- AV1 encode: same as above. Its 10-bit path depends on the VPP port.
- HEVC decode: typed `PictureParameter::HEVC`, `SliceParameter::HEVC`, and the 10-bit P010 context. Removed module: `dec/hevc.rs`, `params/hevc.rs`.
- VP9 decode: `dec/vp9.rs`, `params/vp9.rs` removed; typed `PictureParameter::VP9` and `SliceParameter::VP9` needed.
- AV1 decode: `dec/av1.rs`, `params/av1.rs` removed; typed `PictureParameter::AV1`, `SliceParameter::AV1`, and film grain params needed.
- Tests to re-enable after the ports: the 23 `#[ignore]` tests above.

## API changes (ec-va)

- `Display` is `cros_libva::Display` (re-exported as `ec_va::Display`). `open()` returns `Result<Rc<Display>>`. `open_path` is kept.
- `Surface` is `cros_libva::Surface<()>`. `Surface::sync` is blocking. `SurfaceExt::sync_timeout(Duration)` emulates the old `vaSyncSurface2` timeout by polling `query_status()` with backoff (50 us doubling to 2 ms) until the surface is neither Rendering nor Displaying. On deadline it returns `Error::Va { status: VA_STATUS_ERROR_TIMEDOUT }`.
- `create_pool`, `SurfaceSpec`, `PrimeSurface`/`PrimeObject`/`PrimeLayer` (DRM PRIME export), `import_prime` (ExternalBufferDescriptor), `image_format`, `create_image` (`Image::create_from`).
- `picture::{create_context, buffer, submit}`: `submit` runs new/add_buffer/begin/render/end on cros typed `BufferType`s.
- `Config` wraps cros `Config`; `caps_of` queries surface attributes with `query_surface_attributes_by_type`.
- `Vpp` keeps `convert` and `convert_region` on `ProcPipelineParameterBuffer`. It has no render targets.
- `Error` is `Va { op, status: i32, message }`, `Drm`, `NoDevice`, `Protocol`.
- `Rc` replaces `Arc` for `Display`, `Context`, `Surface` and the pool. `Frame` and `Surface` are `!Send`. The old code was effectively single-threaded per session, so this does not change any caller in the workspace.

## Unmappable fields and behaviour changes

- `Display::version`, `device_path`, `drm_fd`, `is_valid`: cros-libva does not expose them. `vaQueryVersion` is not called.
- `Vpp::filters` (`vaQueryVideoProcFilters`): not exposed. The VPP filter probe is now "try the pipeline; fall back on error".
- `vaPutImage` (CPU-to-surface upload): not exposed. The P010 readback test now calls the fill helpers on synthetic planes directly. The encoder test that imported a DMA-BUF through an upload (`encode_surface_of_an_imported_dma_buf_matches_the_cpu_path`) is ignored.
- `vaSyncSurface2`: emulated by `sync_timeout` (see above).
- Raw `repr(C)` parameter structs and `unsafe trait VaParam`/`from_param`: replaced by plain Rust structs with `to_buffer()` returning `BufferType`.
- Frame readback: each `to_i420`/`to_i420_16` call creates a fresh `Image::create_from` because the cros `Image` borrows its surface and cannot be cached per stream. This costs extra time per frame and needs measuring against the realtime tests before the encoder path is ported.
- `ImageCache` in `frame.rs` removed for the same reason.
- `ec-hw/src/params/mod.rs` keeps only `INVALID_SURFACE`; the `VaParam` machinery is gone.

## lane/va-hevc: HEVC decode on typed cros-libva buffers

- `ec-hw/src/params/hevc.rs` is rewritten as plain Rust structs (`PictureParameterBufferHEVC`, `SliceParameterBufferHEVC`, `IQMatrixBufferHEVC`, `VAPictureHEVC`, plus `PicFields`, `SliceParsingFields`, `LongSliceFlags`). Each has a `to_buffer()` that calls cros-libva's typed `HevcPicFields`/`HevcSliceParsingFields`/`HevcLongSliceFlags` constructors. This is the same pattern as `params/h264.rs`.
- `ec-hw/src/dec/hevc.rs` is restored from main with the FFI-coupled parts ported: `Arc` becomes `Rc`, `sys::VASurfaceID` becomes `u32`, bitfield builder chains become struct literals, `param_buffer` becomes `ec_va::picture::buffer(.., x.to_buffer())`, and the slice data is `BufferType::SliceData`. The decode logic is unchanged.
- `dec/mod.rs`: `Codec::H265` now dispatches to `HevcDecoder` (`Inner::H265`). `ReadyFrames::len` is restored for the debug trace. `params/mod.rs` exports `hevc`.
- Profiles: `ensure_session` already chose the profile from chroma format and bit depth. Main (8-bit) and Main10 (P010 context) both go through `Session::new`. This was not changed.
- Not ported, because main did not use them: `PictureParameterBufferHEVCRext`/`...Scc`, `SliceParameterBufferHEVCRext` and the SCC/RExt flag structs. Main's HEVC decoder never built them, so RExt and SCC streams decode the same as on main (that is, without those extension buffers).
- The ABI assertions (`abi-probe.c` offsets) that main carried in `params/hevc.rs` are gone. cros-libva's bindgen layout owns them now.
- Tests un-ignored (they pass locally, GPU radeonsi, against the same ffmpeg references as main): `hevc_8bit_matches_ffmpeg`, `hevc_10bit_decodes_to_p010`, `ten_bit_4k_chroma_is_not_grey`, `ten_bit_4k_frame_path_is_realtime`, `hdr_film_light_level_sei_is_exposed`, plus the two unit tests in `dec::hevc::tests`.
- `encode_frame_accepts_p010_via_vpp` and `p010_zero_copy_encode_is_realtime` were labelled "needs an HEVC decoder" but use the encoder. Their reason is corrected to "needs an encoder". They stay ignored.
- Still ignored and encoder-dependent: `hevc_slice_headers_count_pocs_for_stateful_decoders`, `hevc_poc_lsb_restarts_at_every_idr`, and the H.264 encode tests.
- `real_library_spot_check` is not verified for its HEVC entries. It panics at its first AV1 entry (`Unsupported`), which is the va-vp9av1 lane's job.
- `fixtures/` in this worktree is a symlink to the main repo's fixtures, added to `.git/info/exclude`. It is not committed.

Test run (`cargo test --release --no-fail-fast -p ec-va -p ec-hw`, local GPU):

| Binary | Pass | Fail | Ignored |
|---|---|---|---|
| ec-hw unit | 22 | 0 | 0 |
| ec-hw tests/film_grain | 0 | 1 | 0 |
| ec-hw tests/gpu | 19 | 8 | 14 |
| ec-hw tests/idr_counting, intra_rate, reset | 0 | 0 | 3, 2, 1 |
| ec-hw doctest | 1 | 0 | 0 |
| ec-va unit | 6 | 0 | 0 |
| ec-va tests/gpu | 14 | 0 | 0 |
| ec-va doctest | 1 | 0 | 0 |
| **Total** | **63** | **9** | **20** |

The 9 failures are all `Unsupported` from decoders this lane does not own: AV1 decode (`av1_matches_ffmpeg`, `av1_10bit_matches_ffmpeg`, `a_broken_gop_is_refused_on_every_codec`, `real_library_spot_check`, `film_grain`), VP9 decode (`vp9_matches_ffmpeg`, `vp9_10bit_matches_ffmpeg`, `unsupported_profiles_are_refused_with_a_reason`) and H.264 zero-copy encode (`encode_frame_zero_copy_matches_the_cpu_path`). These tests are not ignored on the base branch either, so they are not caused by this change. Main passes 104 across these binaries. The base lane/safeva report claimed 24 gpu passes, which does not match this run.

## lane/va-vp9av1: VP9 and AV1 decode on typed cros-libva buffers

- VP9: `dec/vp9.rs` and `params/vp9.rs` rebuilt on `PictureParameter::VP9` and `SliceParameter::VP9`. Commits `1b4ce723` and `133c289f` (AV1), then the merge of `lane/safeva`.
- AV1: `dec/av1.rs` and `params/av1.rs` rebuilt on `PictureParameter::AV1` and `SliceParameter::AV1`. Film grain goes through the same `current_display_picture` path as before. All tiles of a frame go out as one slice-parameter buffer (cros-libva sends AV1 slices as one array), not one buffer per tile.
- `units()` is back in `dec/mod.rs` for the fuzz harness. `vp9_fuzz_build_parameters` and `av1_fuzz_build_parameters` are still exported.
- `dec/mod.rs` now has the Codec::Vp9 and Codec::Av1 arms next to H265. The unreachable catch-all `other =>` arm in `Decoder::new` is gone.
- `clippy -p ec-va -p ec-hw --all-targets -- -D warnings` is clean. No `unsafe` in ec-hw or ec-va.

Tests (`cargo test --release -p ec-va -p ec-hw`): 66 pass, 0 fail, 27 ignored.
- `vp9_matches_ffmpeg`, `vp9_10bit_matches_ffmpeg`: 24/24 identical on all fixtures.
- `av1_matches_ffmpeg`, `av1_10bit_matches_ffmpeg`: 24/24 identical on all fixtures.
- `hardware_grain_matches_software_decoders` (film_grain.rs): pass, 24/24 identical against ec-av1 and ffmpeg.
- `unsupported_profiles_are_refused_with_a_reason`: pass, VP9 and AV1 profile 1 refused with a typed error.
- Ignored with `safeva: needs an encoder`: the six `av1_encode_*` and `av1_10bit_*` tests. They build their input with the AV1 encoder. They printed "skipped" and passed vacuously before, so they are now ignored instead.
- Also ignored: `encode_frame_zero_copy_matches_the_cpu_path`. It has no ignore on lane/safeva and failed there with "H264 encode not ported". This is an encoder test.
- Fixtures: `fixtures/` is a symlink in the worktree to `/home/user/Documents/Code/Rust/edith_codecs/fixtures`. It is not tracked.

Unmapped or changed fields (AV1):
- `anchor_frames_list` / `anchor_frames_num`: cros-libva takes a Vec. Always passed empty, so the count is 0. Large scale tile is out of scope, as before.
- `output_frame_width/height_in_tiles_minus_1`, `large_scale_tile`, `chroma_sample_position`, `anchor_frame_idx`, `tg_start`, `tg_end`: zero, as before.
- `wmtype` is carried as `u32` (the libva transformation enum value).

Not done in this lane: `cargo fmt --check` still flags `dec/mod.rs` and `params/mod.rs`. The diffs come from the hevc and vp9 `pub use`/`mod` ordering after the merge, and from `dec/hevc.rs` and `dec/mod.rs` lines that belong to lane/va-hevc. This lane's own files are formatted.

## lane/va-enc: encoders on cros-libva

- H.264 encode: `enc/h264.rs`. Sequence, picture and slice parameter buffers use cros typed constructors; rate control, frame rate and HRD are `EncMiscParameter`; packed headers are `EncPackedHeaderParameter` + data; coded output is read with `MappedCodedBuffer`. Upload writes through a cros `Image` (vaPutImage on drop). All H.264 encode tests pass (round trip through ffmpeg, constrained baseline, main, high, slice headers, frame_num/IDR counting, reset, zero-copy, DMA-BUF).
- HEVC encode: `enc/hevc.rs`. Same structure with the HEVC typed constructors. The HEVC encode, colour, conformance-window, round-trip, POC and `our_encoder_and_our_decoder_agree` tests pass.
- `encode_surface` and the sync path are generic over the surface memory descriptor, so PRIME-imported surfaces encode. `encode_surface_of_an_imported_dma_buf_matches_the_cpu_path` passes.
- `ec_va::error::va_ok` is now public (the encoder maps VA results through it).
- AV1 encode: NOT ported. `Encoder::new` returns `Unsupported`. Still ignored and needing it: `av1_encode_agrees_between_two_decoders`, `av1_10bit_encode_agrees_between_two_decoders`, `av1_encode_rate_control_lands`, `av1_10bit_bd_row_vs_software`, `av1_encode_bd_row_vs_software`, `av1_encode_speed`, `encode_frame_accepts_p010_via_vpp`, `p010_zero_copy_encode_is_realtime`, `cbr_lands_within_five_percent_of_the_request` (also covers AV1), and `intra_rate.rs` (both tests, covering AV1).
- Not yet un-ignored: the two HEVC POC tests are now un-ignored and passing. The `cbr_*` test stays ignored because it includes AV1.
- `cargo clippy -p ec-va -p ec-hw --all-targets -- -D warnings` clean. No `unsafe` in ec-va or ec-hw code (only comments mention it).

## lane/va-enc: AV1 encode, and test reconciliation against main

AV1 encode: `enc/av1.rs`. Sequence, picture and tile-group buffers use cros typed constructors. The sequence and frame header OBUs come from `enc/headers.rs`, the driver back-annotates offsets outside CQP, and a temporal delimiter (`0x12 0x00`) is prepended to each AV1 access unit. 10-bit AV1 uses P010 uploads, P010 black fill and `encode_16`. All nine AV1-dependent gpu tests and both intra_rate tests are un-ignored and pass. No test is ignored.

`cargo test --release -p ec-va -p ec-hw`: 97 passed, 0 failed, 0 ignored (branch). Main: 105 passed.

Name-level reconciliation (`cargo test -- --list` on both trees, then diffed):

| Binary | Main | Branch | Status |
|---|---|---|---|
| ec-hw lib | 27 | 26 | `params::tests::bit_fields_pack_least_significant_first` and `params::tests::setters_overwrite_rather_than_or` removed: they tested the `VaParam` bit-field `Probe` macro, which is gone (cros-libva's bit-field constructors replace it). `enc::tests::default_profile_is_high` added. |
| ec-hw tests/film_grain | 1 | 1 | same |
| ec-hw tests/gpu | 41 | 41 | same |
| ec-hw tests/idr_counting | 3 | 3 | same |
| ec-hw tests/intra_rate | 2 | 2 | same |
| ec-hw tests/reset | 1 | 1 | same |
| ec-hw doctest | 1 | 1 | same |
| ec-va lib | 8 | 6 | `error::tests::check_maps_success_and_failure` and `error::tests::status_strings_come_from_libva` removed: they tested the old `check`/`status_str` helpers over `sys::` constants, replaced by cros-libva's `VaError` and `va_ok`. |
| ec-va tests/gpu | 14 | 14 | same |
| ec-va doctests | 6 | 2 | five `compile_fail` doctests in `picture.rs` and `lib.rs (line 28)` removed: they checked ordering rules of the old `Picture` API, which was rewritten on cros-libva's typestate. `lib.rs (line 25)` added for the new module. |
| **Total** | **105** | **97** | every removal is listed above; nothing that passed on main is failing or ignored. |
