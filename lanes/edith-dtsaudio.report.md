# Lane dtsaudio report

Commits (rebased onto main fee9cb88): 1c90c146 (DTS, IMA/MS ADPCM, ima4, A-law, mu-law, AMR import and decode); b72c4db0 (DTS in transport streams; DTS and ADPCM tests follow the decoders). Head: b72c4db0.

## What is wired
- DTS (`ec_dts::DtsDecoder`, refuse_xll on) from Matroska `A_DTS*` tracks, mp4/QuickTime/3GP, MPEG-TS, AVI, WAVE, raw `.dts`. Matroska DTS goes through `ExtFileTrack::open_native`; other containers through `ExtFileTrack::open` (ec-probe reader).
- IMA/MS ADPCM, QuickTime ima4, A-law, mu-law (ec-pcm-ext) and AMR-NB/WB (ec-amr) from WAVE, AVI, mov/mp4 and 3GP, plus raw `.amr`/`.awb`, via ec-probe's `AudioDecoder`.
- Probe listing (`probe_streams`, `unsupported`), import (`Track::Ext`), segment seek with pre-roll, and `run_ext` decode path in crates/engine/src/audio.rs. `is_audio` accepts dts, dtshd, amr, awb.
- New module crates/engine/src/extaudio.rs; crates/engine/src/demux/ts.rs and avi.rs hooked to the ext decoder.

## Files changed
- Cargo.lock (ec-dts, ec-probe entries)
- crates/engine/Cargo.toml (dependency lines only: ec-dts, ec-probe)
- crates/engine/src/audio.rs, demux.rs, demux/avi.rs, demux/ts.rs, lib.rs
- crates/engine/src/extaudio.rs (new), crates/engine/tests/audio_ext.rs (new)

## engine Cargo.toml hunk
```
+# DTS core / XCH / X96 decode (`ec-dts`) and the family's audio reader and codec
+# registry (`ec-probe`: ...), seated in `extaudio`.
+ec-dts = { path = "../../../edith_codecs/crates/ec-dts" }
+ec-probe = { path = "../../../edith_codecs/crates/ec-probe" }
```

## Fixes made in this pass
- extaudio.rs: 6.1 channel permutation rewritten with `as_chunks_mut::<7>` + `rotate_right`, to satisfy clippy `chunks_exact_mut` lint.

## TrueHD in transport streams
Not wired. ec-mpegts (edith_codecs, not modifiable here) refuses every stream type it has no demux path for, including TrueHD (0x83), so no packets reach the engine. TrueHD in TS stays refused by name (test asserts it). Wiring it needs an ec-mpegts demux path.

## Known failures outside this lane
The engine crate already uses `unsafe` (ao.rs, decode.rs, hwproc.rs), so the global CLAUDE.md rule "every Rust project forbid unsafe" is not applied here: adding `#![forbid(unsafe_code)]` would not compile.

## Gates (server, host c)
- audio_ext: 17 passed, 0 failed, 0 skipped (ffmpeg has all needed encoders). Run with --nocapture confirmed no skip lines.
- engine clippy (`-p engine --all-targets -D warnings`): clean.
- workspace clippy (`--workspace --all-targets -D warnings`): clean.
- engine tests (`cargo test --release --no-fail-fast -p engine`): 1826 passed, 2 failed, 60 ignored. The 2 failures are the known server-only h264_mid_gop_seeks_match_a_sequential_decode and hevc_mid_gop_seeks_match_a_sequential_decode.
