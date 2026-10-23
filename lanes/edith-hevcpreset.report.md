# hevcpreset report

Worktree ~/.cache/wt/edith-hevcpreset, branch lane-hevcpreset, one local commit (not pushed).

- engine/src/export.rs: `ExportSettings.hevc_speed: Option<HevcSpeed>` (None = Default), new `HevcSpeed {Default, Fast, Faster}`; `cfg.speed` set in both HEVC software encoder config sites (intra-only lanes, I+P GOP). H.264 untouched.
- Test: crates/engine/tests/hevc10_export.rs `hevc_speed_presets_decode_close_to_the_default` (8 and 10 bit, Default/Fast/Faster: 25 frames each, PSNR 38.14/37.94/37.85 dB 8-bit, 38.19/38.06/38.20 dB 10-bit). export_default_bytes green.

## app hunk (default only)
crates/app/src/export_ui.rs, ~line 706, in the ExportSettings literal: `hevc_speed: None,` (compile-forced; no UI).

## mcp hunks
crates/mcp/src/tools/export.rs: `parse_hevc_speed` (default|fast|faster, hevc/hevc-mp4 only, else error); schema property `hevc_speed`; `let hevc_speed = ...` and `hevc_speed,` in the settings literal; unit asserts added to the prores parse test; `HevcSpeed` import.
crates/mcp/src/help.rs: `hevc_speed?` in the export_start signature, one doc line.

## Gate (server c, final tree)
clippy -D warnings clean. cargo test --workspace --release: passed 3739, failed 2 (the known hw_midgop pair: h264/hevc_mid_gop_seeks_match_a_sequential_decode). No Cargo.lock change committed beyond the commit (none after).
