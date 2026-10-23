# lane fuzz1haudio: DONE.md shared gate 2 for the P0/P1 audio crates

Status: the one-hour run is complete for all 14 targets (ec-aac, ec-opus, ec-vorbis, ec-ac3, ec-truehd,
ec-flac). One crash was found (ec-opus `decode_packet`), fixed, replayed and rerun for a full hour with
no crash. Base `e51e3a34` (main), branch `lane/fuzz1haudio`.

Note on the run flags: the task asked for `-max_total_time=3600 -rss_limit_mb=2048 -timeout=10`. Every
run used exactly these flags. Log files are on the host as `out/<crate>-<target>.log`. The crate prefix
keeps `decode_stream` (ec-ac3 and ec-truehd) and `decode` names apart.

## Targets and runs (final code)

exec/s = total runs / 3601 s, averaged over the hour. `host` is where the run executed; `local` means
the remote-run fallback (no free server slot in 10 minutes).

| target | host | run | exec/s | result |
|---|---|---|---|---|
| ec-aac decode_adts | local | 668,182 | 186 | 1 h, EXIT 0, no crash |
| ec-aac decode_raw | local | 384,441 | 107 | 1 h, EXIT 0, no crash |
| ec-opus decode_packet | c | 598,606 | 166 | 1 h on the fixed code, EXIT 0, no crash (the pre-fix run crashed, see below) |
| ec-opus parse_toc | local | 1,139,827,568 | 316,502 | 1 h, EXIT 0, no crash |
| ec-vorbis decode (capped) | b | 117,703 | 33 | 1 h, EXIT 0, no crash |
| ec-vorbis setup | c | 1,461,429 | 406 | 1 h, no crash, no EXIT line in the log |
| ec-ac3 decode_stream | a | 1,370,296 | 381 | 1 h, no crash, no EXIT line in the log |
| ec-ac3 parse_header | c | 230,658,709 | 64,055 | 1 h, EXIT 0, no crash |
| ec-truehd decode_stream | b | 559,795 | 155 | 1 h, no crash, no EXIT line in the log |
| ec-truehd parse_sync | b | 654,187,150 | 181,654 | 1 h, EXIT 0, no crash |
| ec-flac decode_file | a | 336,228 | 93 | 1 h, EXIT 0, no crash |
| ec-flac decode_frame (capped) | c | 7,202,998 | 2,000 | 1 h, EXIT 0, no crash |
| ec-flac encode_roundtrip | b | 4,466,470 | 1,240 | 1 h, EXIT 0, no crash |
| ec-flac parse_metadata | b | 5,351,945 | 1,486 | 1 h, EXIT 0, no crash |

"No EXIT line" means libFuzzer printed its final `Done N runs in 3601 second(s)` line with no crash
output, but the wrapper's `EXIT` echo is missing from the log. I did not find out why (see "Left").
The ec-ac3 parse_header log holds two `Done` lines (239,671,575 and 230,658,709 runs); the table uses
the second, which is followed by `EXIT 0`.

Runs that were started and then discarded (not counted above): an uncapped-seed ec-flac run that was
interrupted when the flac seed cap was added, several runs killed by SIGTERM on shared hosts, and local
runs that raced the sync and were stopped. The 60 s probes are listed under "Probes".

## Probes (60 s, before the caps were settled)

| target | exec/s | note |
|---|---|---|
| ec-aac decode_adts | 63 | close to the 50 threshold, not capped |
| ec-aac decode_raw | 60 | close to the 50 threshold, not capped |
| ec-opus decode_packet | 182 | |
| ec-opus parse_toc | 295,000 | |
| ec-ac3 decode_stream | 543 | |
| ec-ac3 parse_header | 227,000 | |
| ec-vorbis decode, uncapped | 16 | capped, see below |
| ec-vorbis decode, capped | 66 | |
| ec-vorbis setup | 1,314 | |
| ec-truehd decode_stream | 117 | |
| ec-truehd parse_sync | 383,000 | |
| ec-flac decode_file, uncapped | 59 | |
| ec-flac decode_frame, uncapped | 31 | capped, see below |
| ec-flac decode_frame, capped | 280 | |
| ec-flac encode_roundtrip | 6,083 | after the corpus-dir fix |
| ec-flac parse_metadata | 1,044 | |

## Crash found and fixed

1. ec-opus `decode_packet`, panic `attempt to subtract with overflow` at `crates/ec-opus/src/range.rs`
   (`RangeDecoder::tell_frac`). The path is `decode_frame` (lib.rs:368), then CELT `decode` (celt.rs:1516),
   right after the silence path calls `skip_to_end` on a frame of only a few bytes. The bit count then
   drops below `ilog(rng)`, and the unsigned `nbits_total * 8 - lg` underflows. Release builds wrap instead.
   - Fix: `RangeDecoder::tell_frac` and `RangeEncoder::tell_frac` use `saturating_mul` and `saturating_sub`
     and clamp at 0. Comment in the code explains the choice.
   - Not `Error::Corrupt`: `tell_frac` returns a plain `u32` with no error path. Turning it into a `Result`
     touches every CELT bit-allocation call. The clamp is a judgement call, flagged for review.
   - Input: `crates/ec-opus/tests/data/fuzz/decode_packet-tell-frac-underflow.bin` (268 bytes, from the
     crash file `crash-53816579...`).
   - Replay test: `crates/ec-opus/tests/fuzz_regressions.rs::silent_celt_frame_tell_frac_does_not_underflow`.
     It runs the harness framing on the input. It fails in debug builds with the clamp reverted and passes
     with the clamp (verified on host c, see below). In release builds the unclamped code wraps silently,
     so the test is a debug-build detector only, same as the ec-h264 replay in fuzz1hvideo.

No other crash, OOM, or timeout appeared in any final run. No crash artifacts exist on any host except
the pre-fix opus input.

## Harness changes (fuzz targets and seeds only, no decoder behaviour)

- `crates/ec-flac/fuzz/seed.sh`: seeds above 1 MiB are skipped. The xiph testbench has 87 MB and 42 MB files,
  which made each input run at a few exec/s and dominated the RSS budget.
- `crates/ec-flac/fuzz/fuzz_targets/decode_frame.rs`: input capped at 16 KiB of frame data (was 31 exec/s uncapped, 280 capped in a probe).
- `crates/ec-vorbis/fuzz/fuzz_targets/decode.rs`: input capped at 4 KiB, at most 8 packets per input (was 16 exec/s
  uncapped). Capped: 66 exec/s in a probe, 33 exec/s averaged over the hour-long run on host b.
  Below the 50 threshold in the full run; the cap is kept but is not enough on its own on a loaded host.
- `ec-flac encode_roundtrip`: no code change. Its `corpus/encode_roundtrip` directory did not exist, so libFuzzer
  exited with "No such file or directory" after about 159k runs. The fix was to create the directory locally
  (rsync `--delete` removed the empty one created on the host).

## Other changes

- `crates/ec-opus/tests/conformance.rs`: two `.map(&fmt_fd)` changed to `.map(fmt_fd)`. These were pre-existing
  clippy `needless_borrows_for_generic_args` errors that stopped `cargo clippy -p ec-opus --all-targets -D warnings`.
  No behaviour change.

## Verify (run on server hosts through `scripts/remote-run.sh fuzz1haudio`)

| command | host | result |
|---|---|---|
| `cargo test -p ec-opus --test fuzz_regressions` (debug), clamp in place | c | 1 passed |
| same, clamp reverted to `nbits_total * 8 - lg` | c | FAILED, panic `attempt to subtract with overflow` at range.rs:262 |
| `cargo clippy -p ec-opus --all-targets -- -D warnings` | c | exit 0 |
| `cargo test --release -p ec-opus` | c | unit tests 42 passed; `conformance`: 26 passed, 2 FAILED (`decode_speed`, `encode_speed`) |
| `cargo test --release -p ec-opus --test conformance -- decode_speed` (speed rerun, clamp in place) | c | 110x realtime for 5.1 vs a 104x floor; stereo 196x and 216x vs a 135x floor (pass) |
| `cargo test --release -p ec-opus --test conformance -- encode_speed` (alone) | c | ok |
| `EC_REQUIRE_FIXTURES=1 cargo test --release -p ec-flac` | c | 16 + 2 + 4 passed, exit 0 |
| `cargo clippy -p ec-flac --all-targets -- -D warnings` | c | exit 0 |
| `EC_REQUIRE_FIXTURES=1 cargo test --release -p ec-vorbis` | c | 14 + 8 passed (3 ignored), exit 0 |
| `cargo clippy -p ec-vorbis --all-targets -- -D warnings` | c | exit 0 |

The two `conformance` failures in the full release run are timing floors (`decode_speed` and `encode_speed`
assert a realtime multiple). The full run happened while host c had load average 4 to 6 from other lanes. The
decode 5.1 figure was 100x against a 104x floor in that run, and 110x in the isolated rerun. The speed
check I ran with the clamp reverted gave 74x and 101x for stereo, which points to host load rather than the
clamp, but I did not get a quiet host to prove it. I did not change the floors.

No crate in the lane's scope has an unrelated `#[ignore]` added or any test removed. `ec-aac`, `ec-ac3`,
`ec-truehd` have no source or harness changes, so they have no crate verify step in this lane.
`scripts/remote-run.sh` was not modified.

## Infrastructure notes

- The three servers were saturated by other lanes (fuzz1hvideo, fuzz1hcontainers, h264encbd) for most of the lane.
  Many runs waited for slots or fell back to local. Local fallback runs write their corpus into the worktree
  while the next remote-run syncs it, which makes rsync fail with code 24 ("file vanished") and marks the host
  unreachable. This cost a lot of time.
- Some remote runs ended without an EXIT echo (see the table). Remote logs are on each host under
  `~/lanes/fuzz1haudio/out/`.
- Crash artifacts stay in `crates/<crate>/fuzz/artifacts/` on the host. `remote-run` syncs with `--delete`, so a
  crash file on a remote host can be removed by the next sync of this lane. Copy any crash out before the next
  run.

## Left

- Find why the wrapper's EXIT echo is missing from the ec-ac3 decode_stream, ec-vorbis setup and
  ec-truehd decode_stream logs.
- The ec-vorbis decode average (33 exec/s) is below the 50 threshold. Cap it further, or accept it on a loaded host.
- The ec-opus conformance speed floors (`decode_speed` 5.1 at 104x, `encode_speed`) are load-sensitive. Rerun on a quiet host.
- Decide whether the `tell_frac` clamp is right, or whether the CELT silence path should return `Error::Corrupt` for
  frames shorter than the skip requires.
