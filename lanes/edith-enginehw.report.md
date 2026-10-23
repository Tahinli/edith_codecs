# lane enginehw: engine-hw onto ec-hw / ec-va (round 2, after hwgaps)

Branch `lane-enginehw` (worktree `~/.cache/wt/edith-enginehw`). Not pushed.
Merged `codecs-integration` (depguard, oxideav-core removal) and the hwgaps API from edith_codecs main.

## Commits
- `8c92471` decode seat onto `ec_hw::Decoder`, `ec_va::CapReport` (round 1)
- `134ac93` HEVC and AV1 encode seats onto `ec_hw::Encoder` (round 1)
- `bcf0063` merge of codecs-integration into lane-enginehw
- `d404a6b` H.264 encode onto ec-hw (Main profile), DMA-BUF import for every seat, cros-codecs and gbm removed, vendor dirs and PENDING_WAVE2 cleared
- `ac1a451` clippy fixes

## Steps
1. Merge: done (`bcf0063`).
2. H.264 on ec-hw, `H264Profile::Main`: done. The seat tests stay pinned to Main (SPS profile_idc 77 checked).
3. Foreign DMA-BUF encode: done for H.264, HEVC and AV1. `VhDma` fds are duplicated into an `ec_va::PrimeSurface`, imported with `Surface::import_prime`, and encoded with `Encoder::encode_surface`. `vh_enc_dma_geometry` answers for every seat. The CPU upload path remains only for pixels the caller holds. Cost of the DMA path itself not measured (no CPU copy in it).
4. HEVC PSNR at matched achieved bitrate (8 s, 640x480, testsrc2, 240 frames, ffmpeg psnr against source):

| path | target | achieved | mean PSNR |
|---|---|---|---|
| ec-hw HEVC (new) | 2.0 Mb/s | 3,828 kb/s | 50.74 dB |
| cros HEVC (old, 8c92471) | 2.1 Mb/s | 3,996 kb/s | 50.58 dB |

The old encoder's achieved rate stayed near 4.0 Mb/s for targets 2.1 to 3.8 Mb/s (3.8 gave 5.06 Mb/s, 2.9 gave 4.46, 1.0 gave 2.76), so 3.83 Mb/s was not reachable exactly. The new seat is 4.2% lower in bits and 0.16 dB higher in PSNR.

**Finding: the new intra seat overshoots its target.** A 2.0 Mb/s request gives 3.83 Mb/s (+91%). The hwgaps CBR figure (within ±2% at 8 s) was measured with GOP 30. The HEVC and proxy seats use gop_size 1 (every picture an IDR), which that fix does not cover. Needs an ec-hw rate-control fix for intra-only streams.

5. `a_broken_gop_is_refused_in_words_and_never_a_panic`: passes at default threads (av1 binary, 2/2 runs, 15 passed).
6. Stage 5: done. `cros-codecs`, `cros-libva` and `gbm` are gone from `cargo tree -p engine-hw -e normal` (the only match is `crossbeam`, a substring). `vendor/cros-codecs` and `vendor/cros-libva` are deleted, with their `[patch.crates-io]` and `exclude` entries. `Cargo.lock` regenerated. `PENDING_WAVE2` is `[&str; 0]` and `dependency_guard` passes.
7. Gates below.

## Test changes (please review)
- `crates/engine/tests/hw_encode.rs` `slice_headers_carry_frame_num_and_poc`: the parser accepts `pic_order_cnt_type` 2 (what ec-hw writes; legal without B-frames, no lsb field), and the frame_num modulus comes from the SPS (ec-hw declares 16, the test hardcoded 32). Assertions on frame_num and the IDR positions are unchanged.
- `crates/engine-hw/src/lib.rs` `an_intra_seat_writes_a_sequence_header_a_decoder_can_read`: rewritten to encode one picture on the GPU and parse the SPS (same name, same claim). It returns early with no VA-API, as before. The cros SpsBuilder assertions went with the cros crate; the `gop_limit` values stay in `an_intra_seat_asks_for_a_group_of_one_picture`.
- `slice_fix.rs`: level helpers return level IDC numbers (no cros enums); the test expectations are the same levels as numbers.

## Gap found: ec-hw frame_num across IDR
ec-hw keeps counting `frame_num` across an IDR instead of restarting at 1 after it (measured: after the IDR at picture 60 with a 16 modulus, picture 61 has frame_num 13). A decoder reads that as a frame-num gap. This is the same defect the old cros path needed `slice_fix` for. Engine-hw now applies the repair (`slice_fix::SliceCounters::fix_au`) to every H.264 access unit. Needs an ec-hw fix; the repair can then go.

## Gates
- engine-hw unit tests: 12 passed.
- `dependency_guard`: passed.
- GPU hw_encode: 6 passed (including `a_decoded_buffer_is_encoded_without_ever_being_read_back`).
- GPU hw_decode: 12 passed, 1 failed (`an_undecodable_hevc_stream_is_refused_by_name`, failing at baseline too).
- `cargo test -p engine --release --no-fail-fast`: failing: `an_mp4_copy_of_an_anamorphic_source_refuses_in_words` and `the_software_seat_decodes_and_seeks_vp8` (both at baseline), plus the allowed `import_refuses_what_does_not_match` and `a_project_naming_many_tracks_of_one_file_walks_it_once_and_keeps_its_order`. No new failures. The AV1 decode test and the seek test that failed at baseline pass.
- `cargo build -p edith` (dev): ok.
- Clippy `--all-targets` on engine-hw: no warnings in `enc_ec.rs`. The remaining warnings in `lib.rs` and `slice_fix.rs` predate this lane.

## Gaps for edith_codecs
1. ec-hw `frame_num` not reset at IDR (above).
2. ec-hw rate control for intra-only streams (gop 1): +91% at 2 Mb/s for HEVC.
3. ec-hw VP9 8-bit profile-0 decodes into P010 (carried over from round 1; pixels still match).
4. The decode DMA layout is tiled, the old one linear (carried over; now handled by import_prime).

## Files
- `crates/engine-hw/src/enc_ec.rs` (all encode seats), `crates/engine-hw/src/lib.rs`, `crates/engine-hw/src/slice_fix.rs`, `crates/engine-hw/Cargo.toml`, `Cargo.toml`, `Cargo.lock`
- `crates/engine/tests/dependency_guard.rs`, `crates/engine/tests/hw_encode.rs`
- Scratch and logs: `~/.cache/tmp-edith-enginehw/` (s5-engine-full.log, s5-enc.log, s5-dec.log, probe/)
