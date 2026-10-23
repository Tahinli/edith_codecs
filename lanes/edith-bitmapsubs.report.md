# Lane bitmapsubs: DVD (VobSub) and DVB bitmap subtitles in edith

Base: edith `cda806cc`. Commits: `0fe90740` (feature), `e99ec0ab` (clippy fix for test targets).

## What is wired

- Matroska `S_VOBSUB` and `S_DVBSUB` tracks are listed and read as bitmap tracks, like `S_HDMV/PGS`
  (`crates/engine/src/demux.rs`, `subtitle.rs` `of_matroska` / `cues_of`).
- MPEG-TS: stream type 0x06 with the DVB subtitling descriptor is a DVB track; private stream 1 substreams
  are not wired here (the `.sub` path covers VobSub). Probe name `dvb_subtitle`
  (`demux/ts.rs`, `TsSubInfo` now carries the codec and probe name).
- `.idx`/`.sub` pair imports from either file (`subtitle.rs` `vobsub_file_cues`, `ec_dvdsub::sub_file_units`).
- Each cue is a `CueImage` whose `Source` keeps the bytes of its unit: PGS display set, DVD unit with the idx
  text, or DVB display set. `CueImage::rgba()` decodes through `ec_dvdsub::DvdSubDecoder` or
  `ec_dvbsub::DvbSubDecoder` and returns the straight-alpha RGBA canvas.
- DVD cue end: the SPU end, else the block duration, else the next cue, else two seconds. DVB: the
  set's timeout, else the next set, else two seconds; a clear (empty set) produces no cue.
- Dependencies: `crates/engine/Cargo.toml` gained `ec-dvdsub` and `ec-dvbsub` path lines; `Cargo.lock`
  gained the two packages.
- Tests: `crates/engine/tests/bitmap_subs.rs` (4 tests). Fixtures are generated at test time from
  `edith_codecs/scripts/gen-bitmap-sub-fixtures.py` with ffmpeg. `EC_REQUIRE_FIXTURES=1` turns a missing
  generator into a failure; `EC_PYTHON` names an interpreter with PIL for the generator.

## Export and burn-in

PGS has no burn-in on export. `cue_on_screen` and `cues_on_screen` in `export.rs` skip picture cues, and an
export drops a bitmap track with "pictures; drawn, not written". DVD and DVB follow that PGS behaviour exactly;
this lane did not add a bitmap burn-in.

## Peer-owned files

No hunks in `project.rs`, `playback.rs`, `text.rs`, `edith.rs`, `crates/app`, `crates/mcp`, or `assets`.
The `Cargo.toml` change is only the two dependency lines, which the rules allow.

## Known gaps

- MPEG-TS private stream 1 substreams are not read as subtitles.
- The DTS-in-TS test is not ours: it fails the same way at base `cda806cc` (`dts: []`), so it is environmental.
- The `.idx`/`.sub` test does its own comparison with the first cue's offset; the Matroska and TS tests
  compare spacing after the first cue's offset because ffmpeg's muxers start each track at zero.
- `forbid(unsafe_code)`: the engine crate has no such attribute and uses `unsafe` in about 72 places (FFI and
  hardware decode), so adding it would break the build. Not changed here; no other crate in the workspace has it.

## Gates (server, final tree `e99ec0ab`)

- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
- `cargo test --release --no-fail-fast -p engine`: 1811 passed, 3 failed, 60 ignored.
  - Known server-only: `h264_mid_gop_seeks_match_a_sequential_decode`, `hevc_mid_gop_seeks_match_a_sequential_decode`.
  - `dts_and_truehd_tracks_are_refused_by_name_and_the_picture_still_opens`: fails at base `cda806cc` too.
- `cargo test --release -p engine --test bitmap_subs` with `EC_REQUIRE_FIXTURES=1`: 4 passed.
