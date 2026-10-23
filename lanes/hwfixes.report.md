# lane/hwfixes: inventory of the edith hardware patches, and their ec-hw / ec-va regression tests

Base: `main` at `0bf279bd`. Branch `lane/hwfixes`.

## Scope and what changed

- Inventory of the local fixes carried in edith `vendor/cros-codecs` (cros-codecs 0.0.6 upstream, diffed against the
  extracted registry crate), `vendor/cros-libva` (0.0.12 upstream), and the `engine-hw` seat code that owns the
  `slice_fix.rs` and trailing-zero work. The `gbm` crate is upstream and unpatched (edith's `Cargo.lock` pins
  `gbm 0.15.0`, and `vendor/` has no gbm); it is an allocator dependency, not a patch.
- Corrections to the brief: the `cabac_zero_word` fix is not in `slice_fix.rs`. It is the trailing-zero feed guard in
  `crates/engine-hw/src/lib.rs` (`has_start_code`, commit `bb3462e`). `slice_fix.rs` (commit `7c8f894`) is the
  H.264 `frame_num` / `pic_order_cnt_lsb` rewrite plus level selection.
- Code changes (all in this branch):
  1. `ec-hw/src/dec/dpb264.rs`: the frame_num gap range moved into a pure helper `gap_frame_nums`, used by
     `fill_frame_num_gap`; new test `frame_num_gap_fill_ignores_the_sps_flag_and_wraps`.
  2. `ec-hw/src/dec/mod.rs`: the decode-context RT format choice moved into `rt_format_for_bit_depth`, used by
     `Decoder::new`; new test `decode_context_rt_format_follows_the_bit_depth`.
  3. `ec-va/src/cacheline.rs` (new, `pub mod cacheline`): `flush_offsets(len)` is the one-flush-per-cache-line
     schedule with the last-byte straddle flush; three tests.
  4. `ec-h264-syntax/src/nal.rs`: new test `cabac_zero_words_stay_inside_their_slice_nal`.
  5. `ec-hw/src/enc/mod.rs`: clippy fix (`chunks_exact_mut` to `as_chunks_mut`), needed for the `-D warnings` gate on
     the toolchain here (rustc/clippy 1.99). No behaviour change.

## Fix table

| # | Fix (edith location) | Bug and fix | ec-hw / ec-va equivalent | Test(s) |
|---|---|---|---|---|
| a | open-GOP H.264 `frame_num` gap fill: `vendor/cros-codecs/src/decoder/stateless/h264.rs` ~L954 (commit `ea312d3`) | The decoder refused any `frame_num` gap unless the SPS set `gaps_in_frame_num_value_allowed_flag`. An editor's seek into an open GOP always makes such a gap, so 17 of 20 seeks fell back to software. Fix: fill the 8.2.5.2 non-existing frames whatever the flag says. | Present. `ec-hw/src/dec/dpb264.rs:314` `frame_num_gap` (no flag check), `:329` `fill_frame_num_gap`, called from `ec-hw/src/dec/h264.rs:264`. | NEW pure `dpb264::tests::frame_num_gap_fill_ignores_the_sps_flag_and_wraps`. GPU: `h264_frame_num_gaps_are_synthesised` (20 frames, 3 inferred), `h264_seek_matches_linear_open_gop`. |
| b | HEVC Main 10 decode contexts: `vendor/cros-codecs/src/backend/vaapi/decoder.rs` `create_config` (always `VA_RT_FORMAT_YUV420`; the upstream TODO) | A decode context was 8-bit 4:2:0 only, so no Main 10 stream could decode into its P010 surfaces. Fix: take the RT format from the stream. | Present. `ec-hw/src/dec/mod.rs:72` `rt_format_for_bit_depth` (new helper, used at `:247`); P010 surface spec in the same constructor. | NEW pure `dec::tests::decode_context_rt_format_follows_the_bit_depth`. GPU: `hevc_10bit_decodes_to_p010` (24/24 identical to ffmpeg, both 10-bit fixtures). |
| c | radeonsi probe size: `backend/vaapi/decoder.rs` `VaapiBackend::new` placeholder context | Upstream probes with a 16x16 context; radeonsi returns `UNIMPLEMENTED` for < 64x64 and the `expect` aborts. Fix: 64x64 placeholder. | Present by design. `ec-va/src/caps.rs:229` `probe_entries` creates each `Config` with no context and no placeholder surface; the minimum comes from the driver via `Config::surface_caps` (`ec-va/src/config.rs:126`, `SurfaceCaps` doc at `:33`). | No new test: ec-va GPU `caps_report_driver_legal_surface_sizes` already asserts min 64x64 and that 16x16 is refused. Passes. |
| d | HEVC encode: `vendor/cros-codecs/src/codec/h265/{nalu_writer,synthesizer,parser}.rs`, `encoder/stateless/h265/{,predictor,vaapi}.rs`, conformance window (commits `357033c`, `3c52487`, `497c70b`, `b0d483f`) | Upstream had no HEVC encoder in 0.0.6. The vendored one is large (about 2.5k lines). | Present. `ec-hw/src/enc/hevc.rs` (202 lines), `params/hevc.rs`, `enc/headers.rs`, conformance crop written by ec-hw. Not re-implemented (inventory-only, per the brief). | No new test. GPU: `hevc_encode_round_trips_through_ffmpeg` (53.7 dB and 55.0 dB worst), `hevc_encode_honours_the_conformance_window`. Both pass. |
| e | per-cache-line flush: `vendor/cros-codecs/src/video_frame/generic_dma_video_frame.rs` (commit `7c90393`) | Unmap issued one `CLFLUSH` per byte (64 per line; 6.2 ms of the 8.4 ms per-frame export). Fix: one per 64-byte line, plus the last byte for an unaligned base. | MISSING as a CPU path: ec-hw never maps a DMA-BUF (reads go through VA images: `ec-va/src/surface.rs:204` `Surface::read_into`, `ec-hw/src/frame.rs:135` `to_i420`). IMPLEMENTED the schedule as a pure helper, `ec-va/src/cacheline.rs` `flush_offsets`. It has no caller yet; it is for the day a DMA-BUF map path lands. | NEW `cacheline::tests::{empty_buffer_has_nothing_to_flush, one_flush_per_line_not_per_byte, every_byte_is_covered_for_every_base_alignment}` (the last covers 64 base alignments x 0..=300 bytes). |
| f | `munmap` leak: same file, `DmaMapping::drop` (commit `7c90393`) | The unmap was a lazy `.map()` and never ran; each encode frame leaked about 3 MB of address space (2.2 GB over 719 frames). Fix: keep `(ptr, len)` from `mmap` and `munmap` each. | Present, by RAII. `ec-va/src/surface.rs:595` `MappedImage` drop unmaps; `Image::map` at `:490` cannot leak a mapping; `ec-va/src/picture.rs:382` `MappedBuffer` drop; `Surface` drop `:310`, `Image` drop `:524`. | GPU (existing): `surface_pool_create_and_destroy_is_leak_free`, `buffers_map_unmap_and_destroy`, `prime_export_owns_and_releases_its_descriptors` (ec-va `tests/gpu.rs`), all pass. |
| g | `cabac_zero_word` (edith `engine-hw/src/lib.rs` `has_start_code` and feed loop, commit `bb3462e`; NOT `slice_fix.rs`) | Cros-codecs consumed a NAL and left the trailing `cabac_zero_word`s in the buffer; `Nalu::next` then failed "No NAL found" on phone-camera MP4s whose every sample ends in zero words. Fix: an access unit with no start code left is consumed. | Present. `ec-h264-syntax/src/nal.rs:128-170` `AnnexBIter` keeps `00 00 03` cabac words inside their slice NAL and trims only zero bytes. `ec-hw` decodes whole access units (`ec-hw/src/dec/mod.rs:110` `decode(data)`), so there is no feed loop to get wrong. | NEW `nal::tests::cabac_zero_words_stay_inside_their_slice_nal`. Pure witness (the splitter was already right). |
| g' | `slice_fix.rs` (commit `7c8f894`): radeonsi writes `frame_num` and POC as 0 in the slice header; level was hard-coded L4 | ec-hw does not depend on the driver writing headers: `ec-hw/src/enc/h264.rs:144` writes the slice header via `headers::h264_slice_header(.., frame_num, ..)`; level from Table A-1 at `ec-hw/src/enc/h264.rs:42` (`level_idc`, fn near `:151`). | Present by construction. No test added. |

Note on (a): the H.264 `frame_num` gap check that edith's vendored patch removed does not exist in ec-hw at all, so the
pure test checks that the predicate and the fill range ignore the SPS flag, and that the range wraps at `MaxFrameNum`.

## API map: engine-hw to ec-hw / ec-va

Symbols collected from `edith/crates/engine-hw/src/lib.rs` and `slice_fix.rs`.

| engine-hw uses (cros-codecs / cros-libva / gbm) | ec-hw / ec-va equivalent | Status |
|---|---|---|
| `cros_libva::Display` | `ec_va::Display::open`, `open_path` (`display.rs:46`, `:68`) | present |
| `libva::VAProfile`, `VAEntrypoint`, `VAImageFormat` | `ec_va::caps::{Profile, Entrypoint}`, `sys::VAImageFormat` | present |
| `libva::Surface` | `ec_va::Surface::create`, `create_pool` (`surface.rs:73`, `:82`) | present |
| `VA_FOURCC_NV12`, `VA_FOURCC_P010` | `SurfaceSpec::nv12`, `SurfaceSpec::p010` (`surface.rs:28`, `:43`) | present |
| `libva::Image::create_from` (vaGetImage read-back, `lib.rs:722`) | `ec_va::Surface::read_into` (`surface.rs:204`), `Image::create` (`:433`) | present |
| CPU write of NV12 | `ec_va::Surface::write_from` (`surface.rs:241`) | present |
| `Config` / RT-format attributes | `ec_va::Config::new` (`config.rs:83`), `ConfigAttrib::rt_format` (`:22`) | present |
| `Picture` protocol, `Context`, `Buffer` | `ec_va::Picture<New>` begin/render/end/sync (`picture.rs:457`-`:527`), `Context` (`:86`), `Buffer` (`:329` drop) | present |
| `VAProfile` for VP9 and AV1 decode | `ec_hw::Codec::{H264, Hevc, Vp9, Av1}` (`dec/mod.rs:43`) | present |
| `Dec::<H264>::new_vaapi(display, BlockingMode)` | `ec_hw::Decoder::new(display, Codec)` (`dec/mod.rs:87`), synchronous, no `BlockingMode` | present |
| `StatelessDecoder::decode`, `DecoderEvent::{FrameReady, FormatChanged}`, `DecodedHandle::sync` | `Decoder::decode` (`:110`), `next_frame` (`:120`), `flush` (`:130`), `stream_info` (`dec/mod.rs:149`); sync internal | present (events folded into `next_frame`) |
| `StreamInfo` (format change, min surfaces) | `ec_hw::dec::StreamInfo` (`dec/mod.rs:56`) | present |
| `FramePool<GenericDmaVideoFrame>` / `PooledVideoFrame` | `ec_hw::SurfacePool::new` (`pool.rs:35`), `acquire` (`:75`), `PooledSurface` (`:92`) | present |
| `gbm::Device::new(file)`, `create_buffer_object(R8, LINEAR)`, `bo.fd()`, `bo.stride()` (allocation of NV12/P010 DMA-BUFs) | VA surfaces from `SurfacePool` (no gbm). The buffer's DMA-BUF is `Surface::export_prime` (`surface.rs:164`) through `ec_hw::Frame::export_prime` (`frame.rs:123`). | present, gbm-free |
| `GenericDmaVideoFrame::new(files, FrameLayout)` (wrapping a client DMA-BUF as a frame) | none. ec-va has no `VASurfaceAttribExternalBuffers` import path (no "EXTERNAL" in `ec-va/src`) | MISSING (import). Needed only for zero-copy encode input from foreign fds. |
| `GenericDmaVideoFrame` DMA-BUF export (`to_native_handle`, `FrameLayout` with modifier) | `Frame::export_prime` (`frame.rs:123`), `PrimeSurface` / `PrimeLayer` (`surface.rs:322`-`:355`) | present (export only) |
| `FrameLayout`, `PlaneLayout`, `Fourcc`, `Resolution` | `ec_hw::Frame` metadata (`coded_size`, `display_size`, `bit_depth`, `frame.rs:40`) and `SurfaceSpec` | present by design (no layout type) |
| `image_processing::nv12_to_i420` | `ec_hw::frame::nv12_to_i420` (`frame.rs:338`), `i420_to_nv12` (`:375`) | present |
| P010 export to 8-bit (`to_i420` path) | `Frame::to_i420` (`frame.rs:135`), `to_i420_16` (`:209`) | present |
| `codec::h264::parser::{SpsBuilder, Level, Profile}`, `codec::h265::parser::{Level, Profile}`, `codec::av1::parser::Profile` | `ec_h264_syntax`, `ec_h265_syntax`, `ec_av1_syntax` crates; `ec_hw::params::{h264, hevc, av1}` | present |
| `encoder::stateless::{h264,h265,av1}::StatelessEncoder`, `encoder::{h264,h265,av1}::EncoderConfig` | `ec_hw::Encoder::new(display, EncoderConfig)` (`enc/mod.rs:259`), `EncoderConfig::new` (`:177`), `ten_bit` (`:193`), `colour` (`:199`) | present |
| `encoder::h265::ColourDescription` | `EncoderConfig::colour` (`enc/mod.rs:199`), `Colour` (`:164`) | present |
| `encode_frame` / encode from a DMA frame | `Encoder::encode` (`:389`), `encode_16` (`:415`), `encode_frame` (`:472`) | present |
| `backend::vaapi::encoder::VaapiBackend` | internal to `enc/mod.rs` | present |
| `Tunings` (rate control) | `ec_hw::Tunings` (`enc/mod.rs:113`), `Encoder::tune` (`:383`) | present |
| VPP (pixel conversion, P010 to NV12) | `ec_va::Vpp` (`vpp.rs`), used by `vpp_into_pool` in `enc/mod.rs` | present |
| AV1 film-grain display surface (`VaapiPicture::set_display_frame`, `display_surface`) | none. ec-hw never sets `apply_grain` (`ec-hw/src/dec/av1.rs:400-401`), so no grain target is needed | MISSING by design. Needed if grain is ever applied. |
| `vaGetImage` on `display_surface()` (`lib.rs:656`, `:710`) | `ec_va::Surface::read_into` (above) | present |

## Verify runs

Environment: `CARGO_TARGET_DIR=~/.cache/tgt-hwfixes`, `TMPDIR=~/.cache/tmp-hwfixes`, `-j4`, local (GPU: radeonsi,
`/dev/dri/renderD128`, `vainfo` OK). `EC_REQUIRE_FIXTURES=1` with fixtures linked.

| Command | Result |
|---|---|
| `cargo clippy --no-deps -p ec-hw -p ec-va -p ec-h264-syntax --lib -- -D warnings` | pass (exit 0) |
| `cargo clippy -p ec-hw -p ec-va -p ec-h264-syntax --all-targets -- -D warnings` | NOT clean, for a reason outside this lane: `ec-hw`'s dev-dependency `ec-av1` (a workspace member) fails under this toolchain's clippy (about 194 lints in `crates/ec-av1/src/*.rs`, none touched here). `ec-hw` itself: the one lint in `enc/mod.rs:553` is fixed. |
| `cargo test --release -p ec-hw -p ec-va -p ec-h264-syntax --lib` | pass: ec-hw 14, ec-va 8, ec-h264-syntax 14 |
| `cargo test --release -p ec-va --test gpu -- --include-ignored --test-threads=1` | pass: 13/13 |
| `cargo test --release -p ec-hw --test gpu -- --include-ignored --exact` for `h264_frame_num_gaps_are_synthesised`, `h264_seek_matches_linear_open_gop`, `hevc_10bit_decodes_to_p010`, `hevc_8bit_matches_ffmpeg`, `hevc_encode_round_trips_through_ffmpeg`, `hevc_encode_honours_the_conformance_window` | all pass |
| full `ec-hw --test gpu` | NOT completed. The AV1 tests (ffmpeg with librav1e, `speed=6`) took more than 40 minutes on this box and the run was stopped. The AV1 and VP9 GPU tests are unverified in this lane. |
| `scripts/remote-run.sh hwfixes -- cargo test -p ec-hw -p ec-va` | not usable: the server has no `libva` (link fails on `-lva`). All builds and tests ran locally. |

## Left for later

- Import of client DMA-BUFs as VA surfaces (`VASurfaceAttribExternalBuffers`): needed for zero-copy encode input. Missing.
- A DMA-BUF CPU map path: when it lands, it must use `ec_va::cacheline::flush_offsets`, and `munmap` each mapping
  (`ec-va` mappings are RAII already).
- AV1 film-grain target surface: only if grain synthesis is switched on.
- Clippy `--all-targets` on the touched crates needs the `ec-av1` dev-dependency lints fixed first (not in this lane).
- The full `ec-hw --test gpu` run (AV1/VP9 encode-vs-ffmpeg) needs a faster schedule or a longer window.
