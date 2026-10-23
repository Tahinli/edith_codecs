# lane hevcrefuse: refuse an HEVC stream whose parameter sets do not parse

Branch `lane-hevcrefuse` from `codecs-integration` (base fbf0f85). Commit a4bc0f4.

## Change
- `crates/engine/src/demux.rs`: new `Demuxer::hevc_parameter_sets_parse()`. It reads the
  VPS/SPS/PPS blob the demuxer already keeps (`hvcC` for mp4, `CodecPrivate` for mkv), splits
  it with `ec_h265_syntax::split_annex_b`, and parses each VPS/SPS/PPS with the matching
  `ec-h265-syntax` parser. It is `false` when any of them fails, or when no SPS parses. If the
  container carried no sets, it reads the first access unit instead and seeks back, as
  `fill_sei_light` does. Non-HEVC returns `true`. Parameter sets only, no picture decode.
- `crates/engine/src/decode.rs`: `DecodeSession::open_worker` (the synchronous opener behind
  `open`, `open_at`, `open_range`) calls it right after `Demuxer::open`, before a worker or
  decoder is chosen. A refusal returns `Err(Codec::Hevc.undecodable())`, so both hardware and
  software are covered.

## Gates
- `cargo test -p engine --release --test hw_decode -- --include-ignored`: 13 passed, 0 failed,
  including `an_undecodable_hevc_stream_is_refused_by_name`.
- `cargo test -p engine --release --no-fail-fast`: 875 passed, 4 failed. Failures:
  - `import_refuses_what_does_not_match`, `a_project_naming_many_tracks_of_one_file_walks_it_once_and_keeps_its_order`: on the allowed list.
  - `an_mp4_copy_of_an_anamorphic_source_refuses_in_words` (anamorphic) and
    `the_software_seat_decodes_and_seeks_vp8` (vp8): not on the allowed list. Both fail in
    isolation, and both also fail on the baseline (fbf0f85) with my change stashed, so they are
    not caused by this lane. They reproduce only when `libengine_hw.so` is present next to the
    test binary (see below).
- Clippy (`cargo clippy -p engine --all-targets`): no warnings on the lines touched. The only
  hit in `decode.rs` is the pre-existing `hw_decodes(&PathBuf)` at line 3015.

## Environment notes
- The hw_decode tests need `libengine_hw.so` beside the test binary. That lookup is
  `current_exe().parent()` and then the bare name. I built `-p engine-hw --release` and copied
  the library to `~/.cache/tgt-edith-hevcrefuse/release/` and `.../release/deps/`. I also built
  `hw-encode-child` (`-p engine --bin hw-encode-child`). Without them, all 8 hw tests fail with
  "no hardware decode plugin/driver available".
- `assets` is a symlink to `/home/user/Documents/Code/Rust/edith/assets` in the worktree and
  is not committed.

## Not done
- `open_worker_deferred` (the no-sync-error opener) is not checked. Its refusal would be a
  dropped sender, not an `Err`, and the task only asked for the synchronous open path.
