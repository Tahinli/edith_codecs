# lane fuzz1hcontainers: DONE.md shared gate 2 for ec-mp4, ec-ogg, ec-riff, ec-inflate, ec-image, ec-subs, ec-ass, ec-pgs

Status: 36 of 37 targets closed (one full 3,600 s run each on the final code, no crash, OOM or timeout).
`ec-image decode` is OPEN: its runs stop on libFuzzer timeouts (see below); the decoder is not changed.
Base `ca5230f2` (main at worktree creation). Branch `lane/fuzz1hcontainers`.

## Run table (final code; exec/s = runs / 3,601 s)

Code column: `ca5230f2` means no change to the target's crate since the base. `64896167` is the ec-mp4 fix.

| target | host | runs | exec/s | result | code |
|---|---|---|---|---|---|
| ec-mp4 demux_mp4 | a | 73,466,050 | 20,405 | clean (rerun after fix; pre-fix crash) | 64896167 |
| ec-mp4 esds_parse | local | 1,553,319,646 | 431,000 | clean | ca5230f2 |
| ec-mp4 is_mp4 | local | 7,346,048,995 | 2,041,000 | clean | ca5230f2 |
| ec-ogg demux | local | 43,852 | 12 | clean (slow, rss ~1.0 GB) | ca5230f2 |
| ec-ogg demux_raw | a | 187,954 | 52 | clean | ca5230f2 |
| ec-ogg mapping | local | 4,540,303,284 | 1,261,000 | clean | ca5230f2 |
| ec-ogg page_parse | b | 608,421,095 | 169,000 | clean | ca5230f2 |
| ec-ogg roundtrip | local | 8,297,921 | 2,304 | clean | ca5230f2 |
| ec-riff avi_read | b | 202,412,982 | 56,000 | clean | ca5230f2 |
| ec-riff avi_seek | local | 504,658,238 | 140,143 | clean | ca5230f2 |
| ec-riff wav_read | local | 395,975,401 | 109,962 | clean | ca5230f2 |
| ec-inflate inflate_diff | local | 5,637,232 | 1,565 | clean | ca5230f2 |
| ec-inflate inflate_into | a | 31,727,900 | 8,811 | clean | ca5230f2 |
| ec-inflate inflate_raw | a | 72,305,285 | 20,081 | clean | ca5230f2 |
| ec-inflate inflate_zlib | b | 7,671,183 | 2,130 | clean | ca5230f2 |
| ec-image bmp | b | 529,226 (3,608 s) | 147 | clean | ca5230f2 |
| ec-image decode | c | - (aborted, timeout) | - | OPEN, see below | ca5230f2 |
| ec-image decode_animation | c | 302,461 | 84 | clean, 1 h (1 slow unit, no timeout) | ca5230f2 |
| ec-image decode_with_limits | a | 348,865 | 96 | clean | ca5230f2 |
| ec-image gif | a | 991,260 | 275 | clean | ca5230f2 |
| ec-image info | a | 87,832,699 | 24,392 | clean | ca5230f2 |
| ec-image jpeg | c | 176,013 | 49 | clean | ca5230f2 |
| ec-image png | a | 8,025,242 | 2,229 | clean | ca5230f2 |
| ec-image probe | c | 18,086,663 | 5,023 | clean | ca5230f2 |
| ec-image tiff | a | 3,055,842 | 848 | clean | ca5230f2 |
| ec-image webp | a | 412,377 | 115 | clean | ca5230f2 |
| ec-subs ir | a | 124,529 | 35 | clean | ca5230f2 |
| ec-subs markup | b | 124,283 | 35 | clean | ca5230f2 |
| ec-subs srt | a | 226,033 | 63 | clean | ca5230f2 |
| ec-subs webvtt | a | 110,605 | 31 | clean | ca5230f2 |
| ec-ass events | b | 16,498,124 | 4,582 | clean | ca5230f2 |
| ec-ass overrides | a | 12,672,674 | 3,519 | clean | ca5230f2 |
| ec-ass script | c | 956,529 | 266 | clean | ca5230f2 |
| ec-ass styles | c | 4,789,272 | 1,330 | clean | ca5230f2 |
| ec-pgs compose | a | 437,498 | 121 | clean | ca5230f2 |
| ec-pgs rle | c | 374,139 | 104 | clean | ca5230f2 |
| ec-pgs segments | a | 1,088,931 | 302 | clean | ca5230f2 |

Hosts: a = host-a, b = host-b, c = host-c, local = this machine (fallback when
no server slot was free for 10 min). Each row is one 3,600 s libFuzzer run unless marked.

## Crashes found and fixed

1. ec-mp4 `demux_mp4`, arithmetic overflow (panic, `attempt to add with overflow`) at `demux.rs` `frame_rate`:
   the stts tick sum `t + count * delta` in u64. Fix: saturating add and multiply (a saturated total fails the
   existing `try_into`, so the rate is None). Replay: `ec-mp4/tests/fuzz_regressions.rs::stts_tick_sum_overflow_is_refused`,
   input `ec-mp4/tests/data/fuzz/demux_mp4-stts-tick-overflow.mp4`. Commit `64896167`. The replay passes in debug
   (`cargo test -p ec-mp4 --test fuzz_regressions`: 1 passed, EXIT 0). The 1 h rerun on the fixed code was clean.

## Open: ec-image decode (timeouts, not fixed)

Three harness timeouts (libFuzzer `-timeout=10`) on small inputs that declare huge pictures:
- BMP RLE8 `4194368 x 48` (4,950 bytes), two inputs; a third RLE8 BMP at `64 x 3014704` (4,948 bytes) timed out
  on the rerun.
- Progressive JPEG `65519 x 256` (19,599 bytes) timed out in `decode_animation` (the first run; that rerun was clean).

Measured on this lane with `cargo test --release -p ec-image --test fuzz_regressions large_declared` (server, idle):
BMP 0.67 s and 0.73 s, JPEG 1.6 s. With debug assertions and overflow checks on (the fuzz build's profile):
1.27 s, 1.34 s, 1.48 s. The fuzz timeouts happened while host c was loaded (load average 6.3 earlier, 4 CPUs).
So these are bounded linear work near the default `Limits::max_pixels` (2^28), not unbounded loops, and not panics.
But the gate asks for no hang, and a 10 s budget is exceeded under load. Closing this needs a policy decision
(for example a lower default pixel budget, or refusing RLE pictures far larger than their stream). I did not
change the decoder, so `decode` is NOT closed.
The replay inputs are committed under `ec-image/tests/data/fuzz/`, and the test
`large_declared_pictures_from_small_inputs_return` (commit `293e15e7`) asserts that the decode returns.

## Verify (final commits; run through scripts/remote-run.sh, logs in each host's ~/lanes/fuzz1hcontainers/out)

- `cargo test --release -p ec-mp4`: all result blocks ok (0 failed), EXIT 0.
- `cargo clippy -p ec-mp4 --all-targets -- -D warnings`: EXIT 0.
- `cargo test --release -p ec-image`: all result blocks ok (0 failed), EXIT 0.
- `cargo clippy -p ec-image --all-targets -- -D warnings`: EXIT 0.
- `cargo test -p ec-mp4 --test fuzz_regressions` (debug): 1 passed, EXIT 0.
- `cargo test --release -p ec-image --test fuzz_regressions large_declared -- --nocapture`: 1 passed, EXIT 0.

The other six crates were not changed, so their test and clippy gates were not re-run here.

## Notes

- `ec-ogg demux` runs at 12 exec/s with a corpus of about 200 MB and rss near 1 GB. It stays under the
  2 GB limit for the full hour, but it is below the 50 exec/s mark the video lane used as a warning. No change made.
- Crash copies from the pre-fix run are kept only in ~/.cache/wt/out (not committed).
- `scripts/remote-run.sh` was replaced in the worktree by main's version (b2bfb285) as the coordinator asked. It is
  not committed here.
- Fuzz corpus and artifact directories are git-ignored and were not committed. The untracked `Cargo.lock` files in
  the fuzz crates were not committed either.
