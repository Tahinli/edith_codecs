# lane hwfuzz: display-free H.264, VP9 and AV1 parameter builders, one fuzz target per builder, 1 h runs

Base: `main` at `55d7a6c5`. Branch `lane/hwfuzz`. Goal: DONE.md gate G2 (one cargo-fuzz target per untrusted
parse/decode entry point, 1 CPU-hour each, no panic) for the four VA-API parameter builders in `ec-hw`. The H.264, VP9
and AV1 builders had no target; the HEVC `build_parameters` target was the only one.

## 1. Changes

- `crates/ec-hw/src/dec/h264.rs`, `dpb264.rs`: the H.264 builders read plain values, not `Picture`/`Current`. New
  `RefView` (surface id, frame_num, LongTermFrameIdx, POC, mark) and `CurrentView`. `RefList::push` is `pub(crate)`.
  Behaviour-neutral: the same fields reach the same VA structs, and `slice_parameters` reads the same DPB entries.
- `crates/ec-hw/src/dec/vp9.rs`: `picture_parameters` is a free fn over the reference slot ids (`slot_ids`) instead of a
  method on the decoder.
- `crates/ec-hw/src/dec/av1.rs`: `picture_parameters` is a free fn over the slot ids; `tile_parameters` is the
  per-tile slice buffer, shared by `decode_frame` and the fuzz target.
- `crates/ec-hw/src/dec/mod.rs`: `units()` (a run of u32-LE-length-prefixed units, so one persistent parser sees several
  chunks) and re-exports `h264_/vp9_/av1_fuzz_build_parameters`.
- `crates/ec-hw/fuzz/`: three new targets `h264_build_parameters`, `vp9_build_parameters`, `av1_build_parameters`,
  plus `[[bin]]` entries. Each feeds its input through the crate's real parser (`ec-h264-syntax`, `ec-vp9-syntax`,
  `ec-av1-syntax`) into the builders. VP9 and AV1 take a run of units, one `Vp9Parser`/`Av1Parser` across them.
  Seeds are in `crates/ec-hw/fuzz/corpus/` (gitignored, not committed):
  - `h264_build_parameters`: 6 ffmpeg `libx264` Annex B streams (`keyint`, `bframes`, `weightp`, `slices`, baseline,
    `yuv420p10le`/high10, interlaced).
  - `build_parameters` (HEVC): 3 ffmpeg `libx265` streams (`wpp`, `tiles=2x2`, 10-bit).
  - `vp9_build_parameters`: 3 `libvpx-vp9` IVF streams (altref superframes, error-resilient + tiles, profile 2),
    converted to length-prefixed units.
  - `av1_build_parameters`: 3 `libaom-av1` IVF streams (lag, error-resilient, 10-bit), converted the same way.
- **Crash fix** `crates/ec-h264-syntax/src/sps.rs` `parse_scaling_list`: `delta_scale` was added to
  `last_scale + 256` unchecked, which overflowed (panic in a debug or fuzzing build). `delta_scale` outside -128..=127
  is now `Error::corrupt` (7.4.2.1.1). In-range values are unchanged.
- **Regression test** `crates/ec-h264-syntax/tests/fuzz_regressions.rs` with the minimized 16-byte fixture
  `crates/ec-h264-syntax/tests/data/fuzz/pps-scaling-delta-overflow.bin` (a PPS NAL, fuzzer-made bytes). It fails on the
  old code in a debug build (`sps.rs:86:26` overflow, the same site the fuzzer hit) and passes after the fix.

## 2. Fuzz runs

Hosts a, b, c (4 cores each), cargo-fuzz 0.13.2 with nightly, `-max_total_time=3600 -jobs=2 -workers=2
-max_len=131072 -timeout=10 -rss_limit_mb=1536`. Each run was detached with `nohup` under a host slot lock, logs on the
host in `~/lanes/hwfuzz/`. "Runs" is the sum of libFuzzer's `Done N runs` lines (one per worker).

Final runs (the committed tree, and the H.264 run after the fix):

| Target | Host | Start → end (UTC) | Duration | Runs | Crashes | Fixed |
|---|---|---|---|---|---|---|
| `build_parameters` (HEVC) | a | 01:30:45 → 02:30:47 | 1 h | 4,804,573 | 0 | n/a |
| `h264_build_parameters` | b | 03:36:43 → 04:36:45 | 1 h | 3,673,398 | 0 | n/a |
| `vp9_build_parameters` | c | 03:36:55 → 04:36:57 | 1 h | 45,942,805 | 0 | n/a |
| `av1_build_parameters` | c | 02:32:45 → 03:32:47 | 1 h | 14,049,291 | 0 | n/a |

Earlier runs, kept for the record:

| Target | Host | Window (UTC) | Runs | Crashes | Note |
|---|---|---|---|---|---|
| `h264_build_parameters` | b | 01:29:40 → 02:29:41 | 3,636,586 | 0 | Pre-fix tree; the H.264 builder code is unchanged from the final tree, only the SPS fix differs. |
| `h264_build_parameters` | b | 02:32:39 → 02:46:39 | n/a | **1** | Found the `delta_scale` overflow (below). The `-jobs` supervisor kept fuzzing with the pre-fix binary until 03:32, so its slot blocked the post-fix restart until that hour ended. |
| `vp9_build_parameters` | c | 01:29:45 → 02:29:47 | 107.2 M | 0 | Superseded: one chunk per input, so a fresh parser each time; the builder ran on 3 frames. Not counted. |
| `av1_build_parameters` | c | 01:29:45 → 02:29:47 | 93.5 M | 0 | Superseded, same reason. Not counted. |

The VP9 final run was delayed: its start on c hit a busy slot and failed silently. I checked the START line and restarted it.

**Coverage check (local, not committed):** the seeds reach each builder: H.264 6 files, 290 slices built; HEVC
3 files, 90 slices; VP9 30+30+20 streams, 80 frames; AV1 3 streams, 71 temporal units. The per-unit input format was
introduced for VP9 and AV1 because the first version reached the builder only 3 times per target.

## 3. Crashes

| # | Target | Input | Site | Cause | Fix | Regression | Attempts |
|---|---|---|---|---|---|---|---|
| 1 | `h264_build_parameters` | `crash-c9872ae0…` (63 B), minimized to 16 B | `ec-h264-syntax/src/sps.rs:86` (`parse_scaling_list`) | `last_scale + delta_scale + 256` overflows for an out-of-range `se(v)`; the PPS path reaches the same function | refuse `delta_scale` outside -128..=127 as `Error::corrupt` | `ec-h264-syntax/tests/fuzz_regressions.rs`, fixture `tests/data/fuzz/pps-scaling-delta-overflow.bin` | 1 (fixed on the first try, the 1 h post-fix run found nothing) |

The minimized input is `00 00 01 68 e9 b1 6d 6d 00 00 00 00 ff ff ff 20`. The 9 `minimized-from-*` files produced by
`cargo fuzz tmin` are not separate crashes. I found no other crash in the 1 h runs.

## 4. Verify

| Command | Result |
|---|---|
| `cargo test --release -p ec-hw` (local, radeonsi GPU tests included) | pass, exit 0. lib 19, GPU suites 41 + 1 + 3 + 2 + 1 passed |
| `cargo clippy -p ec-hw --all-targets -- -D warnings` (local) | exit 0 |
| `cargo test --release -p ec-h264-syntax` | pass, exit 0. 14 + 1 + the new regression test |
| `cargo clippy -p ec-h264-syntax --all-targets -- -D warnings` | exit 0 |
| `cargo test --release -p ec-h264` (software decoder, uses the same SPS/PPS parser) | pass, exit 0 |
| `cargo +nightly fuzz build` on a, b, c (all four targets) | ok; links with `-L ~/lib-va` (see below) |
| Regression test pre-fix: `cargo test -p ec-h264-syntax --test fuzz_regressions` (debug) | panics at `sps.rs:86:26` |
| Regression test post-fix: same command (debug) and `--release` | pass |

`rustfmt --check` on the new code is clean. `sps.rs` has a rustfmt diff at line 529 that is already on `main`.

## 5. Environment notes

- The servers have libva runtime only (`/usr/lib64/libva.so.2`, Fedora), no dev symlink, so `ld` cannot find `-lva` for
  the fuzz link. Fix used on all hosts, outside the repo: `~/lib-va/libva.so` and `libva-drm.so` symlinks, plus a `cc`
  wrapper at `~/lib-va/bin/cc` that adds `-L$HOME/lib-va`. The fuzz code never calls libva, so the link is the only
  use. The repo is unchanged.
- The server slot lock is per host; a `flock -n` on a busy slot fails silently. Verify the START line of every run.

## 6. What is left

- The H.264, VP9 and AV1 fuzz targets have 1 h each, and HEVC has 1 h. Gate G2's "one target and 1 CPU-hour per parse
  entry point" is met for these four builders. The syntax crates (`ec-h264-syntax`, `ec-vp9-syntax`, `ec-av1-syntax`)
  have their own fuzz suites, not run in this lane.
- The corpora on the servers hold libFuzzer's grown corpus and are not committed (gitignored by `crates/ec-hw/fuzz/.gitignore`).
