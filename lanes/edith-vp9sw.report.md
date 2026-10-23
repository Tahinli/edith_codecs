# edith lane vp9sw: software VP9 decode through ec-vp9

Goal (edith_codecs DONE.md, "ec-vp9 decode"): edith refused VP9 without VA-API with
"there is no software VP9 decoder". ec-vp9 is byte-exact against libvpx, so the refusal was stale.

Base: edith main `b7d8ed0`. Branch `lane-vp9sw`, commit `d63d9ff`.

## What changed
- `crates/engine/Cargo.toml`: `ec-vp9` path dependency, next to `ec-av1`. `Cargo.lock` adds ec-vp9, ec-vp9-syntax.
- `crates/engine/src/decode/vp9.rs` (new): `run_vp9`, the software span, modelled on `run_av1`.
  Each demuxed block goes straight into `ec_vp9::decode::Decoder::decode` (no chunking). Hidden frames
  return None and take no display slot; shown frames (incl. show_existing_frame) do. Seeks start at the
  sync sample at or before the target; pictures before the target are decoded but not sent. Speed and
  lateness rules are the AV1 ones. Planes narrow to 8-bit as AV1's do (10-bit shifts down by 2).
  Anything but 4:2:0 at 8/10 bits fails the span by name.
- `crates/engine/src/decode.rs`: VP9 joins H.264/AV1 in the non-fatal hardware probe and the
  software fallback; the worker dispatch gets a `Codec::Vp9` arm. Hardware still goes first when the
  plugin is present, exactly as for AV1. Stale comments updated.
- `crates/engine/src/demux.rs`: `needs_plugin` drops the VP9 "no software decoder" wording
  (the remaining VP9 refusal is the software *export*, which is H.264-only, and says so). `vp9_bit_depth`
  refuses profile 1 and 3 at open, by name, next to the existing 12-bit refusal. Dead profile-3
  reserved-bit read removed.
- `crates/engine/src/playback.rs`, `export.rs`: comments only (VP9 no longer "black frames without a plugin").
- `crates/engine/tests/vp9.rs`:
  - New: `ec_vp9_matches_libvpx_sample_for_sample` (ffmpeg libvpx-vp9 encodes profile 0 and profile 2
    WebMs at test time, 24 frames 352x288, keyframe every 8; every picture of ec-vp9's decode is compared
    plane-for-plane with `ffmpeg -c:v libvpx-vp9` output, 8-bit and 16-bit LE).
  - New: `the_software_seat_decodes_and_seeks_vp9` (edith's software span, both profiles: every frame
    0..n in order at 352x288, non-flat; seeks to 0, 1, 7, 8, 9, 23 equal the linear decode's BGRA).
  - New: `a_profile_1_vp9_file_is_refused_by_name` (ffmpeg yuv444p profile 1; refused by demuxer and by
    software open, message names VP9 and profile 1).
  - Changed (premise was "no software VP9"): `the_software_path_refuses_vp9_by_name` is now
    `the_software_path_opens_vp9`; `a_timeline_takes_the_other_codec...` refuses HEVC (no software seat)
    and joins VP9; `a_refused_codec_at_a_span...` swaps in HEVC bytes as the refused span. No test deleted or ignored.

## Verify
Command (run from edith_codecs):
`EDITH_TREE=~/.cache/wt/edith-vp9sw scripts/remote-run.sh edith-vp9sw --with-edith -- cargo test -p engine --release --no-fail-fast`
Host a (final run, before two comment-only edits). Result: 825 passed, 10 failed, 56 ignored.
- `tests/vp9`: 7 passed (the 3 new tests above, plus the 4 existing software/container tests); 2 ignored
  (hardware, need libengine_hw.so).
- The 10 failures are the same 10 that fail on clean main at `b7d8ed0` with the same command for
  `--test playback --test video_copy` (that baseline ran locally: the server slots were busy):
  9 in `video_copy` ("HEVC needs the VA-API plugin ... no software HEVC decoder": no plugin here and HEVC
  has no software seat), and `playback::import_refuses_what_does_not_match` (the audio refusal does not
  fire in this environment; not investigated further). Neither set touches VP9 or code changed here.
  Main also shows two timing-flaky playback tests in its run that passed on the branch.

Clippy: `cargo clippy -p engine --all-targets` on host a exits 0. Warnings located in `crates/engine/`
are identical to main's (116 locations each). The VP9 files add none: `run_vp9` carries
`#[allow(clippy::too_many_arguments)]` with its reason; the two `chunks_exact` lints in my tests are
written as `as_chunks`. Remaining warnings are pre-existing, and `ec-av1` warnings differ only because
edith_codecs main moved between the two syncs.

Fixtures: the ffmpeg here and on the server both have libvpx-vp9, so the ffmpeg-backed tests really ran
(they skip where it is missing).

## Environment notes (not code)
- remote-run's sync copies the worktree's `assets` symlink, since `--exclude=/assets/` does not match a
  symlink. The server lanes ended up with dangling links. Fixed by putting real copies of the edith assets in
  `~/lanes/edith-vp9sw/edith/assets` on both hosts. Worth fixing in remote-run.
- Local fallback needs `~/.cache/wt/edith_codecs` (the lane docs say `~/.cache/edith_codecs`, one level too
  high for a worktree at `~/.cache/wt/edith-<name>`). I created the link locally.
- Commit trailer is `Co-Authored-By: Claude Haiku 5.5`, per the harness attribution rule, not the Opus trailer
  in ORCHESTRATION-EDITH.md.

## Not done / left
- VP9 export and proxy stay on their refusals (export's software seat is H.264-only, as for AV1). Playback only.
- No timing check for 1080p30 at realtime. ec-vp9 has no threading yet (DONE.md's threading item is open).
- Pixel-exactness is proven at the plane level (ec-vp9 vs libvpx, all frames). The edith-level test checks
  frame count, order, size, and seek-vs-linear equality, not BGRA against an external reference, since
  Render's grade/placement/colour pipeline has no reference here.
- DONE.md's dead-code removal (`modes.rs:131`, `tables/mod.rs:16`, `gen_tables.rs` allow) and the stale
  reports (`vp9inter`, `vp9hbd`) are not in this lane's ask and are untouched.
