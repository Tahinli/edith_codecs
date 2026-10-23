# lane hwplugfails: the two engine tests that fail only with libengine_hw.so

Branch `lane-hwplugfails` (worktree `~/.cache/wt/edith-hwplugfails`), from `codecs-integration` (9f13f8e).
Commits: `6843aff` (plugin fix), `90e7ce5` (test determinism). Not pushed.

## Causes
1. `the_software_seat_decodes_and_seeks_vp8` (real bug, plugin code).
   The software linear decode matched, but a seek with the plugin on landed on the wrong bytes
   (`seek to 7 differs ... first byte at Some(320)`). The clip is `ve_vp8_altref`, which has a hidden
   alt-ref frame. `engine-hw`'s `VpxSession::pump` counted a container slot only when the decoder
   produced a picture, so a hidden frame was not counted, and the skip after a seek dropped one picture
   too few. Every fed unit is now one slot (shown or hidden), matching `decode/vp8.rs`.
   Seat selection was not the cause: VP8 has been plugin-first since `eab0ccb`. The enginehw and
   hwcleanup merges did not change VP8 routing (`git log -p -S"Codec::Vp8 => Self::Vpx"`).
2. `an_mp4_copy_of_an_anamorphic_source_refuses_in_words` (test bug, not a code bug).
   `wait()` calls `ExportHandle::result()`, which takes the outcome. The test then called `result()`
   again and panicked "a finished export has an outcome". The export refusal itself is correct
   (`pixel aspect`, no file written). Without the plugin the test returns early, so it never hit this.
   Fixed the test to read the error from `wait()` once. The test's claim is unchanged.

## Determinism (item 3)
- `VE_SW` was set and removed per test, so a parallel software-seat test could decode on the plugin
  while a neighbour had just cleared the pin. This is the race behind
  `a_broken_gop_is_refused_in_words_and_never_a_panic`.
- New `crates/engine/tests/support/seat.rs`: `software()` and `hardware()` return a guard that holds a
  process-wide mutex and sets/removes `VE_SW` for its life.
- Converted to it: `av1.rs`, `hevc.rs`, `hevc_hvc1.rs`, `vp8.rs`, `vp9.rs`. Software-meaning tests take
  `software()`; the `#[ignore]`d plugin twins take `hardware()`. The `software_frames` helpers no longer
  touch the env; their callers hold the seat. The vp8 seat test now pins software for its seek half
  too, so it checks the software seat as its name says.
- `export.rs`: the `Once`-based pins were order-dependent (the first call won and the hardware twins
  could run in software). `pin_software()` now sets the pin on each call under a small mutex, and
  `pin_hardware()` takes `SEAT` exclusively and clears it.
- `src/decode.rs` unit-test `SwPin`: now serialised by a static mutex.
- Left as they were (set once per binary, never removed, so already deterministic): `anamorphic`,
  `decode_failure`, `color`, `exif_orientation`, `export_subs`, `images`, `lanes`, and the other files
  that set `VE_SW` only once. `hevc_sw.rs` already had its own `SW` lock and was not changed.

## Gates
- `cargo test -p engine --release --no-fail-fast`, plugin present: 878 passed, 1 failed
  (`import_refuses_what_does_not_match`, the allowed one).
- Same with the plugin absent (`libengine_hw.so` moved aside, then restored): 878 passed, 1 failed (same).
- `--include-ignored` with the plugin present: `hw_decode` 13 passed, `hw_encode` 6 passed. `hevc`, `vp8`,
  `vp9`, `hevc_hvc1`, `export`, `anamorphic`, `av1` all pass except the one below.
- Repeated 3 times: `av1`, `vp8`, `vp9`, `hevc` pass each time.
- Clippy (`-p engine-hw -p engine --all-targets`): no warnings on the lines I touched. The warnings in
  `engine-hw/src/lib.rs` (438, 902, 985) and in the test files I did not touch predate this lane.
- rustfmt: my lines are formatted. The existing diffs in these files (for example `vp9.rs` 117, `av1.rs` 158,
  `anamorphic.rs` 24/30) predate this lane and are left alone.

## Found, not fixed (outside this lane)
- `av1::a_film_grain_av1_file_decodes_whole_and_keeps_its_grain` (`#[ignore]`, needs the plugin) fails with
  the plugin present: "the grain never reached the picture: flattest 8x8 block deviation 0.000". It also fails
  on `codecs-integration` (checked with the prebuilt `tgt-edith-integ` av1 test binary and this build's
  `libengine_hw.so`). It passes with the plugin absent (software AV1 grain). So the hardware AV1 path drops
  film grain. Probably from the enginehw move of AV1 decode onto `ec-hw`. It is not in the gates; needs an
  `ec-hw`/driver look.
- `an_undecodable_hevc_stream_is_refused_by_name` and the ec-hw items in the earlier reports are unchanged.

## Files
- `crates/engine-hw/src/lib.rs` (`VpxSession::pump`)
- `crates/engine/tests/support/seat.rs` (new), `av1.rs`, `hevc.rs`, `hevc_hvc1.rs`, `vp8.rs`, `vp9.rs`, `export.rs`, `anamorphic.rs`
- `crates/engine/src/decode.rs` (unit-test `SwPin` only)
- Logs: `~/.cache/tmp-edith-hwplugfails/final-on.log`, `final-off.log`
- The `assets` symlink in the worktree is git-ignored and was not committed.
