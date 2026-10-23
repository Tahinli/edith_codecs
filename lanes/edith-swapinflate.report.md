# edith lane swapinflate: miniz_oxide replaced by ec-inflate

Goal (DONE.md, dependency replacement wave 1): the zlib `ContentEncoding` path
in edith no longer uses `miniz_oxide`.

- Base: edith `main` at `b7d8ed0`.
- Branch: `lane-swapinflate`, commit `56677a3`
  (`♻️ refactor(demux): inflate zlib Matroska blocks with ec-inflate`).
- Worktree `~/.cache/wt/edith-swapinflate` (removed by `--clean` afterwards).

## What changed

- `crates/engine/Cargo.toml`: `miniz_oxide = "0.8"` removed, and
  `ec-inflate = { path = "../../../edith_codecs/crates/ec-inflate" }` added.
- `crates/engine/src/demux.rs` (`Unpack::frame`, the `Zlib` arm): the call is now
  `ec_inflate::inflate_zlib(buf, LIMIT)` with the same 64 MiB `LIMIT` and the same
  `"a zlib Matroska block did not inflate: {e}"` error wrapper. `ec-inflate`'s
  `LimitExceeded` is reported through its Display text
  ("decompressed output exceeded the 67108864 byte limit"). Its errors are
  `Error`-typed values, so the wrapper converts them to the same `String` the
  caller already gets from miniz. No other edith code changed.
- `crates/engine/src/demux/zlib_tests.rs` (new, `#[cfg(test)] mod zlib_tests;`
  in demux.rs): 10 unit tests on `Unpack::Zlib.frame`.
- `Cargo.lock`: updated by cargo.

## Parity

The new tests compare the output bytes (valid streams) or the refusal (invalid
streams) with values computed in the test. Fixtures: Python `zlib` at writing
time for the empty stream, the 880-byte text, "Hello there", and a 300-byte
xorshift buffer. Larger inputs (a 70 000-byte stream spanning two stored blocks,
and the 64 MiB ceiling cases) are built in the test as stored-block zlib streams.

Cases: empty stream; short text; repetitive text; random 300 bytes; random
70 000 bytes (two blocks); exactly 64 MiB (kept); 64 MiB + 1 (refused, message names
the limit); bad zlib header; reserved block type 3; wrong Adler-32; truncation at
0, 1, 2, 12 and len-1 bytes.

Cross-check against miniz_oxide (done once, on the server, with miniz temporarily
re-added; removed before the commit). Outcome and bytes matched for every case:

| case | miniz_oxide | ec-inflate |
|---|---|---|
| empty | Ok(0 bytes) | Ok(0 bytes) |
| hello | Ok(11) | Ok(11) |
| text | Ok(880) | Ok(880) |
| random300 | Ok(300) | Ok(300) |
| stored70k | Ok(70000) | Ok(70000) |
| limit exact (64 MiB) | Ok(67108864) | Ok(67108864) |
| limit + 1 | Err HasMoreOutput | Err LimitExceeded |
| bad header | Err Failed | Err Corrupt |
| reserved block type | Err Failed | Err Corrupt |
| adler mismatch | Err Adler32Mismatch | Err Adler32Mismatch |
| truncated 0 / 2 / 12 | Err FailedCannotMakeProgress | Err Truncated |

No miniz_oxide dev-dependency was kept.

## Existing coverage

`crates/engine/tests/mkv_encodings.rs::a_zlib_subtitle_track_inflates_into_cues`
already covers a zlib-compressed Matroska subtitle track through the public API.
It passes (7/7 in that file). No new integration test was needed.

## Dependency check

`cargo tree -p engine -e normal -i miniz_oxide` (local, offline):

```
miniz_oxide v0.8.9
├── flate2 v1.1.9
│   └── png v0.18.1
│       └── image v0.25.10
│           └── engine
└── png v0.18.1
```

engine has no direct miniz_oxide dependency. It still arrives through `image`'s
`png` feature (`png` -> `flate2` -> `miniz_oxide`, plus `png` directly). Removing
that is a later wave, in `image`.

## Verify commands and results

Tests were run on the server through `remote-run` with `--with-edith` (host b, then a).
Command: `cargo test -p engine --release --no-fail-fast`.

- Lane tree (commit `56677a3`'s content): **832 passed, 10 failed, 56 ignored**
  (the lane's test count is 822 + the 10 new tests).
- Baseline, edith `main` at `b7d8ed0`, same command and server setup: **822 passed,
  10 failed**.
- The same 10 tests fail on both trees, and they are environmental: 9 in
  `video_copy.rs` and `import_refuses_what_does_not_match` in `playback.rs`.
  Every one either opens a HEVC file, which needs the VA-API plugin
  (`libengine_hw.so`) that the server does not have, or fails in the same way on
  `main`.
- New tests: `cargo test -p engine --release --lib zlib_tests`: 10/10 pass.
- `mkv_encodings`: 7/7 pass.

Setup note: `remote-run` excludes `assets/` from the sync, and the server had no edith
assets, so the first runs failed about 150 asset-backed tests. I copied
`edith/assets` to `~/lanes/swapinflate/edith/assets` on both hosts with rsync. That
is outside the remote-run sync, so `--clean` does not remove it. The lane directory is
removed by `--clean`. In one run, the wall-clock test `bitrate.rs::no_decoder_is_opened`
and several others in `bitrate.rs` failed on the lane tree. A second and a third run
passed them. I did not find a cause, so treat that as a flake to watch.

Clippy: `cargo clippy -p engine --all-targets` on the lane tree gives 160 warning and
error lines. `main` gives 160 as well. The new code adds none: nothing points at
`demux/zlib_tests.rs` or at the changed `Unpack::frame` line. The remaining warnings are
pre-existing (decode, export, demux, and the ec-av1 path dep). `cargo clippy -p engine`
is not clean on `main` either, so "no new warnings" holds, but the warnings are not
zero.

## Left for later

- `miniz_oxide` is still in the engine's transitive tree, through `image`'s `png`.
- The 10 environmental test failures need an HEVC-capable VA-API server, or a local run,
  to be confirmed as passing.
- rustfmt: `demux.rs` already fails `rustfmt --check` on `main`. The new file is
  formatted.
