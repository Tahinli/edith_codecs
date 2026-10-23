# lane-av1export report

## Finding
The gap was already closed on edith main: `Pictures::software` routes `Codec::Av1` to `Av1Stream` (export.rs, commit 901a03f0), which runs `ec-av1` `decode_stream_with` on a worker thread into a bounded channel (2 deep), one sync-sample run at a time (bounded by GOP), seeks to the sync sample at or before the in point, drops shown pictures before it, supports `wide` (10-bit) and mono/4:2:2/4:4:4 via chroma_420. The `needs_plugin` text in `SwDecoder::open` was unreachable dead code. Design (a) was already in place; no ec-av1 change, codecs repo untouched (branch lane/av1pull has no commits; nothing to merge there).

## edith commit (branch lane-av1export), one commit
export.rs hunks (1): `SwDecoder::open` -- the unreachable AV1 refusal branch replaced by a one-line "has no export seat" error.
demux.rs: removed `Codec::needs_plugin` (always None now, only caller gone) and reworded two doc comments.
hw.rs: doc comment no longer cites needs_plugin.
New crates/engine/tests/av1_export.rs (ffmpeg libaom/svtav1 generated, 256x144, GOP 20, in point 27, 24 frames kept, plugin off via VE_SW, Mp4 H.264 software export):
- mp4 and mkv x 8 and 10 bit: frame count, first picture PSNR >= 38 dB vs the engine's decode of the source frame (clearly nearer than neighbours +-1 by >3 dB), later frames exact, engine vs ffmpeg decode of that frame >= 30 dB, ffprobe A/V durations agree.
- two AV1 clips back to back (mkv 8-bit + mp4 10-bit) in order.
- cancel mid-export leaves no file and no .part.
Note: threshold is 38 not 40 dB: the export's H.264 re-encode floors PSNR at ~39.3 dB regardless of bitrate; neighbour margin carries the exactness claim. Tests skip (vacuous pass) if ffmpeg lacks an AV1 encoder.

## Gates (host c, final commit)
clippy -D warnings clean; full test run: only the 2 known hw_midgop_seek failures.

## Merge order
Only edith lane-av1export; no codecs dependency.
