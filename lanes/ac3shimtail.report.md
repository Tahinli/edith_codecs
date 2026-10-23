# Lane ac3shimtail: multi-syncframe packets in the AC-3 shim

Branch `lane/ac3shimtail`, base `8fb9fb6c` (main).

## Goal

`cargo test --release` in `shims/oxideav-ac3` failed
`edith_surface::the_replicas_open_path_lands_on_audible_pcm` with
"a second syncframe in the tail of the packet". Find the root cause, and make
multi-syncframe packets decode if that is the right fix.

## Root cause

Both halves were involved, and the test is where the packet came from.

- The test (`shims/oxideav-ac3/tests/edith_surface.rs`, the "second frame" step)
  passes `data[size..]` as one packet. That is the whole rest of the fixture,
  about 100 syncframes, not one frame. Its own comment says "a second frame".
- The decoder refused any packet whose tail begins a syncframe
  (`reject_tail` in `crates/ec-ac3/src/decoder.rs`), by design: "one packet is one
  syncframe".
- edith does not send multi-frame packets. It calls `ec_ac3::Ac3Decoder::decode_frame`
  once per MKV block or MP4 sample (`crates/engine/src/audio.rs`, `run_ac3` and
  `run_mkv_ac3`), so one syncframe per packet is its real shape. The failing
  packet came from this test only.

## Changes

1. `crates/ec-ac3/src/decoder.rs`
   - New `Ac3Decoder::decode_packet`: walks every syncframe in the packet against
     one decoder state and concatenates the interleaved PCM. Padding after the
     last syncframe is ignored. A following syncframe must share the first one's
     channel layout, or it is refused by name. An E-AC-3 independent substream
     with `substreamid` 1 and up (another program) is refused by name. A truncated
     trailing syncframe is `Error::Corrupt`, not `NeedMore`.
   - `Decoder::send_packet` now calls `decode_packet`. The shim's registry
     decoder goes through this path, so it handles multi-syncframe packets.
   - `decode_frame` keeps its one-syncframe contract. Existing tests pin the
     refusal there: `decode_matrix::a_concatenated_tail_is_refused_by_name_not_dropped`,
     `dependent::a_second_independent_substream_in_the_packet_is_still_refused`.
     edith calls `decode_frame` directly, so its behaviour is unchanged.
   - Refactor: `decode_syncframe` (decode one frame, returns bytes consumed),
     `audio_frame` (builds the `AudioFrame` and updates params), `begins_syncframe`
     (the test `reject_tail` used, now shared).
2. `crates/ec-ac3/tests/packet_walk.rs` (new, 6 tests): AC-3 5.1 and E-AC-3 5.1
   packets of 2 and 3 syncframes, and E-AC-3 7.1 (independent + dependent)
   packets of 2 and 3, each compared bit-exact with the same frames decoded one at
   a time. Also padding after 3 frames, a 5.1 + stereo packet refused by name, and
   `decode_frame` still refusing a 2-frame packet.
3. `shims/oxideav-ac3/tests/edith_surface.rs`: new test
   `a_packet_of_two_and_three_syncframes_decodes_to_its_frames` (AC-3 and E-AC-3,
   through the registry path with the stereo fold). The `chunks_exact` in
   `decode_ac3` is now `as_chunks`, for clippy.
4. `shims/oxideav-ac3/src/lib.rs`: `chunks_exact` replaced by `as_chunks` in
   `s16le`, the S16 readback and the round-trip test. These were clippy
   `-D warnings` failures already on main (`chunks_exact_to_as_chunks`).

The existing shim test `the_replicas_open_path_lands_on_audible_pcm` is unchanged.
It now passes because its remainder packet is walked. I left the test body as it
was rather than narrowing it to one frame.

## Verify commands and results (server, `scripts/remote-run.sh ac3shimtail --host auto`, host c)

Run with `EC_REQUIRE_FIXTURES=1`, `CARGO_TARGET_DIR=~/.cache/tgt-ac3shimtail`,
`TMPDIR=~/.cache/tmp-ac3shimtail`.

- Before the change: `cd shims/oxideav-ac3 && cargo test --release` -> the
  `edith_surface` test FAILED with the "second syncframe" error (reproduced).
- `cargo test --release -p ec-ac3`: lib 48 passed; decode_matrix 8; dependent 7;
  encode_matrix 6; packet_walk 6; real_library 1. All passed, 0 failed.
- `cd shims/oxideav-ac3 && cargo test --release`: lib 3 passed; edith_surface 2
  passed (`the_replicas_open_path_lands_on_audible_pcm` and the new packet test).
- `cargo clippy -p ec-ac3 --all-targets -- -D warnings`: clean.
- `cd shims/oxideav-ac3 && cargo clippy --all-targets -- -D warnings`: clean
  (exit 0). Before, it failed on 4 pre-existing `chunks_exact` lints.

## Parity

The new tests check packet-walk output against per-frame decode bit-exact. The
per-frame decoder is already held to ffmpeg by the existing matrix and
real-library tests, so the multi-frame output inherits that tolerance. I did not
add a new ffmpeg comparison for multi-frame packets.

## Left

- edith still calls `decode_frame` one sample at a time, so the decoder change
  does not reach edith's path. To use it, edith would switch to `decode_packet`
  or `send_packet`. Edith is read-only in this lane.
- E-AC-3 programs 2 and up in a packet are still refused by name, and no fixture
  exercises that refusal.
- Not pushed. Commit is local on `lane/ac3shimtail`.
