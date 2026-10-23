# lane hwregress: local cros/gbm fixes ported to ec-hw / ec-va, each with a regression test

Base: `main` at `c0347b7f`. Branch `lane/hwregress`. Local GPU (radeonsi, `/dev/dri/renderD128`), `-j4`,
`CARGO_TARGET_DIR=~/.cache/tgt-hwregress`, `TMPDIR=~/.cache/tmp-hwregress`. VA-API tests ran locally (server has no libva).
Edith was read only.

## 1. Inventory of the local fixes

Sources: edith `vendor/cros-codecs` (its git history: about 15 commits touch the path, the last is `d404a6b` which removes it),
`vendor/cros-libva` (no fix commits; only the vendor import and removal), `gbm` (no vendor dir, no patch entry; the
`[patch.crates-io]` table holds only `proc-macro-error2`, which is not a GPU fix). The lane reports
`hwgaps`, `hwfixes`, `edith-enginehw`, `edith-hwcleanup`, `edith-hwplugfails` were read.

Commits found in the cros-codecs history that `hwfixes` did not list are marked **(new)**. `44839d1` (4K wavefront,
33 entry points) and `c12f93c` (tiled HEVC, entry points past u8) are the two that matter for the decoder.

| # | Fix (edith location / commit) | ec-hw / ec-va equivalent (file:line) | Regression test |
|---|---|---|---|
| a | H.264 open-GOP `frame_num` gap fill (`cros h264.rs` ~L954, `ea312d3`) | `ec-hw/src/dec/dpb264.rs:314` `frame_num_gap`, `:329` `fill_frame_num_gap` | `dpb264::tests::frame_num_gap_fill_ignores_the_sps_flag_and_wraps` (new in hwfixes); GPU `h264_frame_num_gaps_are_synthesised`, `h264_seek_matches_linear_open_gop` |
| b | HEVC Main 10 decode context RT format (`backend/vaapi/decoder.rs` `create_config`, always YUV420 upstream; commit not identified in the history) | `ec-hw/src/dec/mod.rs:72` `rt_format_for_bit_depth` | `dec::tests::decode_context_rt_format_follows_the_bit_depth`; GPU `hevc_10bit_decodes_to_p010` |
| c | radeonsi probe needs >= 64x64 placeholder (`decoder.rs` `VaapiBackend::new`) | `ec-va/src/caps.rs:229` `probe_entries` (no placeholder; minimum from `Config::surface_caps`, `config.rs:126`) | GPU `caps_report_driver_legal_surface_sizes` (min 64x64, 16x16 refused) |
| d | HEVC encoder, parameter sets, conformance window (`357033c`, `3c52487`, `497c70b`, `b0d483f`) | `ec-hw/src/enc/hevc.rs`, `params/hevc.rs`, `enc/headers.rs` | GPU `hevc_encode_round_trips_through_ffmpeg`, `hevc_encode_honours_the_conformance_window`; `idr_counting::hevc_poc_lsb_restarts_at_every_idr` |
| e | one CLFLUSH per cache line, not per byte (`generic_dma_video_frame.rs`, `7c90393`) | `ec-va/src/cacheline.rs` `flush_offsets` (no CPU DMA-BUF map path in ec-hw yet) | `cacheline::tests::{empty_buffer_has_nothing_to_flush, one_flush_per_line_not_per_byte, every_byte_is_covered_for_every_base_alignment}` |
| f | munmap leak in `DmaMapping::drop` (`7c90393`) | `ec-va/src/surface.rs:595` `MappedImage`, `:490` `Image::map`, `picture.rs:382` `MappedBuffer`; RAII drops | GPU `surface_pool_create_and_destroy_is_leak_free`, `buffers_map_unmap_and_destroy`, `prime_export_owns_and_releases_its_descriptors` |
| g | `cabac_zero_word` trailing zeros ends the feed loop (engine-hw `has_start_code`, `bb3462e`) | `ec-h264-syntax/src/nal.rs:128-170` `AnnexBIter`; ec-hw decodes whole access units (`dec/mod.rs:110`) | `ec-h264-syntax nal::tests::cabac_zero_words_stay_inside_their_slice_nal`. Lives in the syntax crate, which is where the splitter is. |
| g' | radeonsi writes `frame_num`/POC as 0; level hard-coded (`slice_fix.rs`, `7c8f894`) | `ec-hw/src/enc/h264.rs:144` writes `frame_num`/POC; level from Table A-1 (`:42`) | GPU `idr_counting::h264_frame_num_and_idr_pic_id_restart_at_every_idr` (covers the enginehw "frame_num not reset at IDR" gap, which is closed) |
| h | film grain synthesized into its own surface (`fb02732`) | `ec-hw/src/dec/av1.rs` (grain not applied by ec-hw; output matches software) | GPU `film_grain::hardware_grain_matches_software_decoders` |
| i | HEVC 4K wavefront: 33 entry points, not 32 (`44839d1`) **(new)** | `ec-hw/src/dec/hevc.rs` `entry_point_count` (count from the stream); parser `ec-h265-syntax/src/slice.rs` (Vec, not a fixed array) | **NEW** `ec-hw dec::hevc::tests::entry_points_are_counted_past_32_and_past_u8`; **NEW** `ec-h265-syntax slice::tests::entry_point_counts_past_32_and_past_u8_round_trip` (33 and 440) |
| j | tiled HEVC entry points counted past u8 (`c12f93c`) **(new)** | count is `usize`/`u16` in ec-hw; CTB-product bound in u64 at `ec-h265-syntax/src/slice.rs:519` | same tests as (i) (440 entries); the old u8 product is not present in the port |
| k | tile count caps 20 x 22 and long-term/sub-layer caps refuse by name (`44839d1`, `c12f93c`) **(new)** | `ec-h265-syntax/src/ps.rs:1227` (tiles), `ps.rs:490` (SPS sub-layers), `slice.rs:381` (long-term 32) | existing: `ps::tests::pps_tile_count_is_capped_for_uniform_spacing_too`, `seven_sub_layers_is_refused_by_name_and_six_is_not`, `slice` long-term bound |
| l | H.264 level_idc from Table A-1 instead of a fixed level (`slice_fix.rs` level helpers; engine-side `0c07a89` on the software seat) | `ec-hw/src/enc/h264.rs:42` level table and `H264Profile` (`hwgaps`) | GPU `h264_profile_round_trip` cases; `enc::headers::h264_profile_tests` |

Fixes that are seat policy, not ec-hw, and so have no ec-hw test: `7d2a453` (two pictures in flight only on the
measured seat), `3f66139` (pay debts: audio fallbacks, proxy, .part sweep), and the engine-hw VP8 pump hidden-frame fix
(`hwplugfails`, `6843aff`). Those belong to edith's engine-hw seat, which this lane does not own.

Open items found by the reports (not carried fixes, not ported here): the hardware AV1 path drops film grain at the
engine level (`hwplugfails`: the ec-hw film-grain test passes, so the drop is above ec-hw); the VP9 8-bit profile-0 into
P010 and the tiled decode DMA layout (`enginehw` items 3-4); the `an_undecodable_hevc_stream_is_refused_by_name` test
(now covered by row k: the sub-layer refusal is in place, so re-running the engine test is the next check, not this lane).

## 2. Changes in this lane

- `crates/ec-hw/src/dec/hevc.rs`: `picture_parameters` now takes the current surface id and POC, the RPS, and plain
  `DpbSlot`s instead of `Current` and `StoredPicture`. No behaviour change (the HEVC GPU suite passes). This lets the
  builder run without a display. Added `entry_point_count`, `fuzz_build_parameters` (`#[doc(hidden)]`, re-exported as
  `ec_hw::dec::hevc_fuzz_build_parameters`), and the two tests in row i and j.
- `crates/ec-hw/src/dec/mod.rs`: the re-export.
- `crates/ec-h265-syntax/src/slice.rs`: test `entry_point_counts_past_32_and_past_u8_round_trip`.
- `crates/ec-hw/fuzz/` (new): cargo-fuzz crate, target `build_parameters`, its own `[workspace]`, `.gitignore` for
  corpus, artifacts and target. Seeds (not committed, gitignored): 6 x 8-bit and 1 x 10-bit HEVC streams from
  `ffmpeg -f lavfi -i testsrc2 -c:v libx265` with `keyint`, `bframes`, `tiles=2x2`, `tiles=3x2:wpp=0`,
  `scaling-list=default`, `frame-threads=1`, and `-pix_fmt yuv420p10le`. Each seed reaches the builder (7 of 7 build slices).

## 3. Fuzz decision

The VA-API parameter builders take untrusted input. `picture_parameters`, `write_weights` and `iq_matrix` copy
SPS, PPS and slice header fields from the stream into `VAPictureParameterBufferHEVC` and friends. A fuzz target was
added (`crates/ec-hw/fuzz/fuzz_targets/build_parameters.rs`). Run locally, no GPU:
`cargo +nightly fuzz run build_parameters -- -max_total_time=600 -rss_limit_mb=4096`:
**2,910,819 runs in 601 s, no crash, no sanitizer report.** The DPB and RPS are synthetic in the target, since the RPS is
driven by the decoder's DPB; the header fields are all real parse output.
H.264, VP9 and AV1 builders were not given a target in this lane (see the gap below).

Gap: the H.264, VP9 and AV1 parameter builders are not fuzzed. Their syntax crates have fuzz tests
(`ec-h265-syntax/tests/fuzz.rs`, `ec-av1/tests/fuzz_regressions.rs`), but the ec-hw builders are not covered.

## 4. Gates (local, release)

| Command | Result |
|---|---|
| `cargo test --release -p ec-hw -p ec-va` (log `~/.cache/tmp-hwregress/gate-final.log`) | pass, exit 0. ec-hw lib 19 (was 17), ec-hw gpu 41, ec-va gpu 14, plus idr_counting 3, film_grain 1, intra_rate 2 |
| `cargo clippy --release -p ec-hw -p ec-va --all-targets -- -D warnings` | exit 0 |
| `cargo test --release -p ec-h265-syntax` and clippy `-D warnings` | pass, exit 0 |
| fuzz `build_parameters`, 10 min | 2,910,819 runs, no crash |

Notes: the ec-va GPU tests have no skip path. `Display::open().expect(...)` fails without a driver, by design
(documented in `crates/ec-va/tests/gpu.rs:3`). The lane's new tests are pure and need no GPU. The ec-hw GPU tests
keep their existing `skipped:` messages.

## 5. Verdict

DONE.md criterion "the local fixes edith carried to cros/gbm are each ported with a regression test": **MET** for the
fixes listed above. Each row has a test that exists in the tree at this commit. Rows i, j, and k are new in this lane
or were missed by `hwfixes`; rows i and j added tests, and row k already had tests. The gateaudit row ("partly ported")
can move to "ported with tests", with the caveat in section 3 on the H.264, VP9 and AV1 builders.
