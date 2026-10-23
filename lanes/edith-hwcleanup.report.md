# lane hwcleanup: drop the H.264 frame_num repair, shared-display check, gates

Branch `lane-hwcleanup` (worktree `~/.cache/wt/edith-hwcleanup`), from `codecs-integration` (706ec4c).
Commit `8f234db` (not pushed). ec-hw is the edith_codecs main checkout at 70ce9f7c.

## 1. H.264 frame_num repair removed
- Deleted `crates/engine-hw/src/slice_fix.rs` entirely. Its functions: the frame_num/POC rewrite (`SliceCounters::fix_au`,
  now emitted correctly by ec-hw), the level helpers `h264_level`/`hevc_level` (test-only; ec-hw computes level_idc itself,
  `enc/h264.rs:176`), `sps_log2_max_frame_num`/`fix_slice_nal_headers` (test-only, see below). Nothing else used them.
- `enc_ec.rs`: `h264_counters` field and `queue()` removed; access units go straight to `ready`.
- `lib.rs` tests: `an_intra_seat_writes_a_sequence_header_a_decoder_can_read` now parses log2_max_frame_num_minus4 with a
  small inline SPS reader (still expects 0). `only_a_unit_carrying_parameter_sets_is_marked_idr` (tested the deleted
  `fix_slice_nal_headers`) is replaced by `every_intra_only_access_unit_is_an_idr_with_parameter_sets`, a GPU test on the
  real seat (two pictures, each with SPS + IDR and no non-IDR slice). It returns early without VA-API, as the old one did.
- Proof with the repair gone:
  - `hw_encode` with `--ignored` (LD_LIBRARY_PATH to the built libengine_hw.so): 6 passed, including
    `slice_headers_carry_frame_num_and_poc`.
  - H.264 HW export tests (`export` hardware set, `hevc_export`, `hevc_hvc1`, `vp8`/`vp9` as_h264): all pass.
  - ffmpeg decode of a 300-picture 1080p hardware H.264 stream with 5 IDRs (`~/.cache/tmp-edith-hwcleanup/export-300.h264`):
    `ffmpeg -v error` prints nothing, 300 frames, 5 keyframes, no "gap" in `-v debug`.
- `cargo build -p engine-hw --release` and `cargo test -p engine-hw --release`: 7 passed.

## 2. Shared Display
- Edith does not share one `ec_va::Display`. `Display::open()` is called once per `EcEncSession::open` (`enc_ec.rs:103`),
  and the decode session has its own. Nothing in edith caches a Display or an encoder (only the plugin library handle is a
  static, `engine/src/hw.rs`). No change made.
- Per the orchestrator note (hwrc2 section 3, revised): the carry-over is process-wide and a fresh Display does not isolate it,
  so no restructuring was done for it.
- Achieved bitrate of the HEVC intra seat (`EcEncSession`, `intra_only`, GOP 1, CBR, 640x480, 240 pictures at 30 fps,
  synthetic moving content, fresh process, HEVC first in it): request 2000 kb/s, delivered **2019.7 kb/s (+1.0%)**, 240 AUs.
  Before this change the same seat delivered 3.83 Mb/s (+91%). Within the +/-5% gate. (The probe was temporary and is not committed.
  H.264 in the same process gave 2007.7 kb/s, +0.4%.)

## 3. Gates
- `cargo test -p engine-hw --release`: 7 passed. `cargo build -p edith`: ok (dev).
- `dependency_guard`: passed.
- GPU `hw_decode --ignored`: 12 passed, 1 failed: `an_undecodable_hevc_stream_is_refused_by_name` (the allowed one).
- GPU `hw_encode --ignored`: 6 passed.
- `cargo test -p engine --release --no-fail-fast` (log `~/.cache/tmp-edith-hwcleanup/engine-full.log`): 874 passed, 5 failed:
  - `an_mp4_copy_of_an_anamorphic_source_refuses_in_words` (baseline failure)
  - `the_software_seat_decodes_and_seeks_vp8` (baseline failure)
  - `import_refuses_what_does_not_match` (allowed)
  - `a_project_naming_many_tracks_of_one_file_walks_it_once_and_keeps_its_order` (allowed, load-flaky)
  - `a_broken_gop_is_refused_in_words_and_never_a_panic` (av1): **not on the allowed list.** It is a pre-existing race: the test
    sets and removes `VE_SW` in the process env (its own comment says the suite needs `--test-threads=1`). It appears in
    the baseline failure list (`~/.cache/wt/base-failed.txt`). It passes alone (2/2) and with `--test-threads=1` (15 passed).
- Clippy `-p engine-hw --all-targets`: no warnings in `enc_ec.rs`, and none on lines touched in `lib.rs`. The three remaining
  warnings (`vh_caps`, `OpenSession`, `CAP_TABLE`) are on untouched lines.

## 4. an_undecodable_hevc_stream_is_refused_by_name: needs an ec-h265-syntax change (not edited)
- What the test does: takes the 4K wavefront fixture, sets `sps_max_sub_layers_minus1 = 7` in every SPS (0x42 0x01 NAL header,
  `|= 0x0e`), and expects `DecodeSession::open` to refuse with `Codec::Hevc.undecodable()`.
- What happens now: open succeeds. The plugin's first `next_frame()` returns a real picture, so `hw_decodes` (`engine/src/decode.rs`)
  passes. The fixture's slice data is intact; the VA decoder ignores the bad sub-layer count.
- Why: the comment in the test says "the parser now refuses", but no code in edith_codecs does. `ec-h265-syntax/src/ps.rs`
  reads `max_sub_layers_minus1` (3 bits) at `Sps::parse` (~line 475) and `Vps::parse` (~line 211) and accepts 7. Searching
  the history finds no sub-layer check on main or on the lane commits.
- Exact change needed in edith_codecs `crates/ec-h265-syntax/src/ps.rs`, in both `Vps::parse` and `Sps::parse`, right after
  `let max_sub_layers_minus1 = r.read_bits(3)?;`:
  ```rust
  if max_sub_layers_minus1 > 6 {
      return Err(Error::corrupt(format!(
          "HEVC SPS: sps_max_sub_layers_minus1 = {max_sub_layers_minus1}"
      )));
  }
  ```
  (7.4.3.1 and 7.4.3.2.1 limit it to 0..6.) The ec-hw decoder already propagates a parse error with `?` (`ec-hw/src/dec/hevc.rs:138,145`),
  so the plugin should yield no picture and edith's probe refuses. I did not verify the plugin-side outcome, because that needs
  the change in edith_codecs, which I was told not to edit. Re-run `hw_decode --ignored` after it lands.
- Nothing to fix in edith for this test.

## Files
- `crates/engine-hw/src/enc_ec.rs`, `crates/engine-hw/src/lib.rs`, `crates/engine-hw/src/slice_fix.rs` (deleted)
- `~/.cache/tmp-edith-hwcleanup/export-300.h264` (scratch stream), `~/.cache/tmp-edith-hwcleanup/engine-full.log`
- The `assets` symlink in the worktree is git-ignored and was not committed.
