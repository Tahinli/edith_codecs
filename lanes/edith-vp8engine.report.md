# edith lane vp8engine: software VP8 decode through ec-vp8

Goal: VP8 decodes in edith's engine directly through `ec-vp8`, as a software span like VP9's
(`lanes/edith-vp9sw.report.md`). Before: VP8 was refused without the VA-API plugin, whose own
`ec-vp8` arm was the only decoder.

Base: codecs-integration `bfcd98c`. Branch `lane-vp8engine`, commit `a1adc45`.

## What changed
- `crates/engine/src/decode/vp8.rs` (new): `run_vp8`, the software span, modelled on `run_vp9`.
  Shown frames go to `ec_vp8::decode::Decoder::decode` output as 8-bit I420 (no narrowing).
  Seeks start at the sync sample at or before the target; pictures before the target are decoded
  but not sent. Speed and lateness rules are the VP9/AV1 ones.
- Hidden frames (alt-ref or golden updates not shown): `Decoder::decode` returns `None`. Unlike
  VP9 (hidden frames live inside superframes), a hidden VP8 frame is its own WebM/mp4 block, so it
  keeps its container slot and sends nothing. Seek indices and `frame_count` stay on the container's
  timeline. Consequence: a stream with alt-ref frames has a one-slot hole where each hidden frame
  sits (see "left").
- `crates/engine/src/decode.rs`: VP8 joins H.264/AV1/VP9 in the non-fatal hardware probe and the
  software fallback. Hardware (the plugin, whose VP8 arm is the same `ec-vp8`) still goes first when
  present. The worker dispatch gets a `Codec::Vp8` arm. The gap unit test now uses HEVC (VP8 is no
  longer a no-seat codec there).
- `crates/engine/src/demux.rs`: the VP8 refusal no longer says its decoder is "inside the plugin";
  it reads "playback decodes it in software, the software export path does not", as AV1 and VP9 do.
  Export still refuses VP8 without the plugin (export's software seat is H.264-only). The
  "no software decoder" wording now only reaches HEVC.
- `crates/engine/Cargo.toml` + `Cargo.lock`: `ec-vp8` path dependency.
- `crates/engine/tests/vp8.rs`:
  - `the_software_path_refuses_vp8_by_name` is now `the_software_path_opens_vp8` (premise changed,
    no test deleted or ignored).
  - New `ec_vp8_matches_libvpx_picture_for_picture`: ffmpeg `-c:v libvpx` encodes three clips at test
    time; every picture of `ec-vp8` is compared plane-for-plane with ffmpeg `-c:v libvpx` decode.
  - New `the_software_seat_decodes_and_seeks_vp8`: the edith span on each clip delivers every shown
    frame in order at the clip's size, and seeks (key frames, neighbours, the frame after each hidden
    slot) equal the linear decode's BGRA.
  - Clips: `ve_vp8_plain` 352x288 (key frame every 8); `ve_vp8_altref` 352x288, two-pass with
    `-auto-alt-ref 1 -lag-in-frames 16` (one-pass through ffmpeg never emits a hidden frame, verified:
    0 hidden in 24 packets); `ve_vp8_odd` 99x67. ffmpeg's libvpx refuses to encode odd sizes (asked
    for 99x67 it writes 98x66), so the odd clip is the 98x66 stream re-labelled at the bitstream level:
    key frame dimension fields and the IVF header are set to 99x67. 98/66 and 99/67 round to the
    same 7x5 macroblock grid, so the macroblock data is unchanged and libvpx decodes it as its own.

## Verify
Commands (from `/home/user/Documents/Code/Rust/edith_codecs`; all on the server via remote-run, host a
after waiting on busy slots):

1. `EDITH_TREE=~/.cache/wt/edith-vp8engine scripts/remote-run.sh edith-vp8engine --with-edith -- cargo test -p engine --release --test vp8 -- --nocapture`
   Result: 4 passed, 0 failed, 2 ignored (the two plugin twins, need `libengine_hw.so`).
   Picture counts: `ve_vp8_plain` 24 pictures (24 access units), `ve_vp8_altref` 24 pictures (25 access
   units, one hidden), `ve_vp8_odd` 24 pictures (24 access units); all agree with libvpx sample for sample.
2. `full cargo test -p engine --release --no-fail-fast` (same wrapper): 847 passed, 11 failed, 56 ignored
   across all targets. The failures:
   - 9 in `video_copy`: "HEVC needs the VA-API plugin (libengine_hw.so) — there is no software HEVC
     decoder". Known on codecs-integration; untouched here.
   - `playback::import_refuses_what_does_not_match`: "audio into a silent timeline: 1". Known.
   - `subtitles::a_project_naming_many_tracks_of_one_file_walks_it_once_and_keeps_its_order`: a
     byte-count assertion ("979582 bytes read against 709") that reads a process-wide counter, so it
     races with the other tests in the same binary under the parallel run. Rerun alone with
     `--test subtitles`: 22 passed, 0 failed. Not a VP8 effect; I did not change the test.
3. `clippy -p engine --all-targets` vs base `bfcd98c` (same wrapper, detached base worktree): exit 0 both.
   Warning messages located in `crates/engine/` are identical (51 in the lib, same messages
   everywhere). The new VP8 files add none.
4. `rustfmt --check` on `decode/vp8.rs`, `tests/vp8.rs`, and `decode.rs`: clean. `demux.rs` has the
   same pre-existing diffs as base (15 before and after; none from this change).

## Environment notes (not code)
- A first attempt hit a background `pkill -f` that matched its own shell. Not a code issue.
- The lane vp8 run queued for 10+ minutes behind other lanes' slots before falling through.
- The local edith-integ worktree had moved to `4b39db2` (newer than this lane's base), so a clippy
  comparison against it differs; the comparison above uses a detached worktree at `bfcd98c`.

## Not done / left
- Golden-frame refresh is not asserted directly. The alt-ref clip covers the hidden-frame path;
  I did not inspect refresh flags in the header.
- Hidden-frame timeline: a VP8 alt-ref stream plays with a one-slot hole where each hidden frame
  sits. The container's own count (`frame_count`) includes that slot, so seeks stay exact. Whether
  playback should repeat the previous picture into the hole is a playback decision left open here.
- Export and proxy keep their VP8 refusal without the plugin (software export is H.264-only).
- No timing check for 720p or 1080p at realtime. `ec-vp8` has no threading.
