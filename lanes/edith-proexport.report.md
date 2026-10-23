# lane proexport report

Branch `lane-proexport` (worktree /home/user/.cache/wt/edith-proexport), from edith main c890955a. Not pushed, no trailers.

## What the app must set (engine API, all in `engine::export`)

```rust
ExportSettings {
    format: Format::ProRes,              // NEW variant; ext() == "mov"; has_video() true
    prores_profile: ProResProfile::Hq,   // NEW field, default Hq. Proxy | Lt | Standard | Hq | P4444 | P4444Xq (ALL, name(), is_444())
    hdr: HdrOutput::default(),           // NEW field, default = no HDR
    ..
}
HdrOutput { transfer: HdrTransfer /* Sdr (default) | Pq | Hlg */,
            mastering_display: Option<MasteringDisplay>,   // MasteringDisplay::bt2020(max_nits, min_nits) / ::p3_d65(..) / raw SEI units
            max_cll: Option<u16>, max_fall: Option<u16> }  // cd/m2
```
- `Format::ProRes` and the six profiles: `ProResProfile::name()` gives "ProRes 422 HQ" etc.
- HEVC bit depth is the existing `video_bit_depth: Option<u8>` (Some(8) / Some(10) / None follows source). It already supported HEVC Main 10 (`wide_ten_bit`); only its doc said "AV1 only". HDR forces 10-bit; `Some(8)` with an HDR transfer is refused by name.
- HDR applies to `Format::Hevc` (mkv) and `Format::HevcMp4` only; any other format with `hdr.transfer != Sdr` is refused ("only HEVC is"). mastering_display / max_cll / max_fall with `Sdr` is refused ("need an HDR transfer"). HDR + `keep_source_colour` refused.
- Extension logic: unchanged, `Format::ext()`; ProRes gives "mov". A ProRes export of a timeline without picture is refused like every picture format ("would be black").
- Ignored for ProRes: `bitrate`, `seat`, `video_bit_depth`, `audio_kbps`, `subtitles`, `chapters` (the .mov carries neither; `planned_subtitles` says "none"), `intra_only` is refused as for every non-H.264/HEVC format, `exact` is never a copy.
- Compile-forced app edits (no UI): `export_ui.rs` `export_settings` literal gets the two new fields at defaults; `Format::ProRes` arms in `export_ui.rs` (`codec_word` "ProRes", `container_word` "MOV"), `ui/settings_stance.rs` (`sound_codec` PCM, `format_key` "prores"); `mcp/src/tools/export.rs` (`format_name` "prores", `plain_container` "QuickTime"). `format_from_name` / `FORMAT_NAMES` in the MCP were NOT touched: "prores" is not yet a name an agent can ask for (peer wires that).

## Engine behaviour

ProRes (`Format::ProRes`)
- Seat `Enc::ProRes` (ec-prores, software, its own threads inside one frame call), on the export worker thread like the other software encoders. Frames are one sample each, all key frames, progressive, no alpha.
- Colour tagged from the same rule as every export (709/601 by size, or the 10-bit source's SDR matrix): written in the ProRes frame header and in the `colr` box (ec-mp4 writes colr/fiel/pasp itself).
- Always 10-bit: `wide_ten_bit` returns true for ProRes, so a 10-bit source passing through the walk untouched arrives as 16-bit planes; a composed picture (grade, fade, transform, caption, tone map, layer) is 8-bit as in every other 10-bit path and is widened `x<<2|x>>6`.
- Chroma 4:2:0 -> 4:2:2 / 4:4:4 is a linear filter, not duplication (documented in `prores_out.rs`): vertical 3/4 own + 1/4 neighbour (4:2:0 "left" siting), horizontal (4444 only) even columns copied, odd columns the mean of neighbours. 4:2:2 profiles get I210, 4444/XQ get I410. No chroma detail is invented: it is a 4:2:2/4:4:4 container of 4:2:0 information. Needs even width and height.
- Audio: **16-bit little-endian PCM (`sowt`)** at the timeline's rate and channel count, mixed by the same mixer as a WAV export (`mix_pcm`, factored out of `run_audio`, so loudness target, range, EQ, speed, fades all apply), on its own thread beside the picture, written after the picture in 1 s packets. **24-bit is not offered: ec-mp4 cannot mux `in24`/`lpcm`/`fl32`/`fl64`/big-endian 24.** What ec-mp4 has: `sowt` (PcmS16Le), `twos` (PcmS16Be), `raw ` (PcmU8) in `mux.rs audio_entry` and `demux.rs` (nothing else, no `in24`, no `lpcm` v2 sample entry, no `chan`/`wave` boxes). For a s24le master a codecs lane would need: `CodecId::PcmS24Le` (and S32/F32) + `in24` (QuickTime v1/v2 sound description with bytes-per-sample fields) in the muxer, plus `chan`. Then `MovWriter::write_sound` (prores_out.rs) switches on a setting.
- Cancel: the `.part` is deleted like every export (test: 1080p HQ, cancel at 10%, no file, no .part).
- Verified with ffprobe/ffmpeg (tests below): codec prores, profile Proxy/LT/Standard/HQ/4444/XQ, pix_fmt yuv422p10le (4444/XQ: ffmpeg's decoder reports `yuv444p12le`, as the codecs report predicted), pcm_s16le 48000, frame count = 50, duration 2.00 +-0.05, A/V lag <= 1 ms (48 samples) with correlation > 0.99.
- PSNR vs source (320x240, testsrc2): luma Proxy 36.7, LT 51.1, 422 52.7, HQ 52.9, 4444 53.0 dB; all planes via a 4:2:0 view 36.5/41.7/41.8/41.8/34.8 dB (the 4444 figure is lower because ffmpeg's 4:4:4 -> 4:2:0 downscale of the upsampled chroma is not the inverse of the upsample).

HEVC 10-bit and HDR
- What is 10-bit end to end: a 10-bit source on a span that touches nothing (no grade, transform, fade, dissolve, caption, tone map, layer, rotation, non-square pixels, other matrix) is decoded at 10 bits and fed to the encoder as 16-bit planes (`Enc::encode_wide`). This already existed for AV1/HEVC; the compositor/grading path itself is 8-bit for most operations (a few have 10-bit paths: `place_picture16`, dissolves, keyed colour, burned16). Anything composed at 8 bits is widened with `x<<2|x>>6` (honest but banding-prone: 8-bit precision in a 10-bit container). This lane did not widen any compositing.
- HDR (`hdr.transfer` Pq/Hlg): output colour is BT.2020 primaries + BT.2020 NCL matrix + the transfer, limited range, 10-bit, software seat. Declared in: the HEVC VUI (video_signal_type, plus chroma_sample_loc type 2), the mp4 `colr` box (existing `set_colour`), and the Matroska `Colour` element (`HevcParams.colour`, new, only set for HDR so other mkv files are unchanged); mastering display and content light level as SEIs in the bitstream (`ec-h265` `mastering_display`/`content_light`). mp4 `mdcv`/`clli` boxes are NOT written (ec-mp4's box writer takes a `ContentLight` with a fixed BT.2020 mastering primaries set and the engine mp4 wrapper never fills it; the SEI is what ffprobe/mpv/ffmpeg read; Apple players may want the boxes). Matroska `MasteringMetadata`/MaxCLL elements are not written either (same corner cut as copy exports).
- No SDR tone map for HDR output: with an HDR transfer set the per-source `ToneMapper`s are not built. A HDR source on the SAME curve passes through untouched (verified: PQ and HLG source -> 38.2 dB against the source after decode, no tone map, no matrix remap). A source on the OTHER HDR curve is refused by name ("converting one HDR curve to the other is not done here"). An SDR source is allowed and is simply tagged and matrix-converted (709 -> 2020 YCbCr matrix only, no gamut or tone mapping): a user choice, documented on `ExportSettings::hdr`; the picture is not "made HDR". HDR output forces `CopyPlan::of` to None (the copy path would carry the source's own stream and SEIs).
- Default exports: untouched. `hdr` and `prores_profile` default to nothing; `apply_hdr` is a no-op for Sdr; the Matroska `colour` is `None`. Proven by `export_default_bytes.rs`: size+md5 of five default exports (H.264, HEVC mp4, HEVC mkv, HEVC mp4 10-bit, HEVC intra mkv, software seat, picture-only source) captured on a clean extract of main c890955a and identical on this branch.

## export.rs hunks (for rebase against lane-procodecs)
1. `Format::ProRes` variant + `has_video`, new `is_prores`, `name`, `ext`.
2. New public types before `ExportSettings`: `ProResProfile`, `HdrTransfer`, `MasteringDisplay`, `HdrOutput`.
3. Two new `ExportSettings` fields (`prores_profile`, `hdr`) at the end; `video_bit_depth` doc corrected.
4. `video_label` / `audio_label` / `audio_rate_refusal` / `export_subtitles` / `planned_subtitles_in`: ProRes arms.
5. `CopyPlan::of`: HDR returns None.
6. `run`: `hdr_colour_of` call, PCM pass spawn (`pcm_job`) and `audio_job` made `Option` (None for ProRes), HDR tone-map skip, curve-mismatch refusal, `source_sdr` = HDR colour, ProRes muxer creation after the encoder opens, `Muxer::Mov` arm at the end (PCM write + zero-pad to picture length).
7. `Muxer::Mov` variant; `write_video` ProRes branch; `HevcParams.colour` sites (2).
8. `Enc::ProRes` variant: `label`, `prores_params`, `open` dispatch, `open_prores`, `encode_wide`/`encode` arms, `drain`, unreachable arm.
9. `apply_hdr` helper, called in both HEVC seats (`open_hevc` intra lanes, `open_hevc_gop`).
10. `wide_ten_bit`: ProRes true, HDR true (8 refused).
11. `hdr_colour_of`, `transfer_name`.
12. `run_audio` split: body moved into `mix_pcm` (returns `Option<(samples, AudioMeta)>`), `run_audio` calls it; the progress writes inside are skipped for picture formats; `mix_pcm16`, `PcmPass`, `PcmJob`, `join_pcm` added.
Other files: `crates/engine/Cargo.toml` (+`ec-prores` path dep, expect a trivial conflict with procodecs), `crates/engine/src/lib.rs` (+`mod prores_out;`), `crates/engine/src/mux.rs` (`HevcParams.colour`, used in `MkvMuxer::create_hevc`), new `crates/engine/src/prores_out.rs` (upsampling, `MovWriter`, 4 unit tests), tests `prores_export.rs`, `hevc10_export.rs`, `export_default_bytes.rs`; compile-forced arms in app and mcp as above.

## Tests
- `prores_export.rs`: every profile (ffprobe codec/profile/pix_fmt/audio/frames/duration, ffmpeg decode PSNR floors, A/V lag), cancel at 1080p leaves no file.
- `hevc10_export.rs`: PQ and HLG sources x (mkv, mp4): Main 10, yuv420p10le, bt2020/bt2020nc/transfer/tv, chroma_location topleft (mp4; a Matroska reader answers for the container), mastering display and content light SEI with MaxCLL/MaxFALL values, decode PSNR 38.2 dB > 30; SDR source tagged PQ (no SEI unless asked); refusals (8-bit HDR, H.264 HDR, PQ source into HLG, stray light levels).
- `export_default_bytes.rs`: pinned hashes above.
- `prores_out` unit tests (flat chroma, filter taps, widening).

## Gates (host c, final commit)
See the end of this file (filled in after the final run).

Final commit 1e6679e5, host c: `cargo clippy -p engine -p edith_mcp -p edith --release --all-targets -- -D warnings` clean. Full `cargo test -p engine -p edith_mcp -p edith --release --no-fail-fast`: awk over "test result" lines = 3607 passed, 2 failed; the 2 are the known VA-API-only h264_/hevc_mid_gop_seeks_match_a_sequential_decode.
