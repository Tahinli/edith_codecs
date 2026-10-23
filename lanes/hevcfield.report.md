# lane/hevcfield report

Outcome: no commits. The HEVC bitstream cannot make ffprobe say field_order=progressive; the fix belongs in the container muxers.

Findings (ffmpeg 8-era build on this machine):
- The encoder already writes PTL progressive_source=1, interlaced_source=0, frame_only_constraint=1 (ProfileTierLevel::main/main10), plus VUI field_seq_flag=0.
- Added frame_field_info_present_flag=1 and a pic_timing SEI (pic_struct=0, source_scan_type=1) to every AU (IDR and P). trace_headers parsed it fine, but ffprobe on raw .hevc and on mp4 remux still said `unknown`. Reverted.
- libx265 raw .hevc output (same PTL/VUI as ours, and with --pic-struct=0) is also `unknown` in ffprobe. Only `ffmpeg -c:v libx265 out.mp4`/.mkv says `progressive`, because the libx265 wrapper sets codecpar field_order and the muxer writes it: mp4 `fiel` atom in the visual sample entry, Matroska FieldOrder/FlagInterlaced. Remuxing a raw x265 stream to mp4 gives `unknown` again. H.264 says progressive because ffmpeg's h264 decoder derives it from frame_mbs_only_flag; the hevc decoder/parser has no such derivation.
- So HEVC field_order=progressive is container-level only.

Recommended fix (outside this lane's crate): add a field-order value to ec_core::registry::VideoParameters (default Unknown, set Progressive by edith export) and have ec-mp4 mux.rs write a `fiel` box (field_count=1, field_detail=0) after colr/pasp in the sample entry (crates/ec-mp4/src/mux.rs ~line 1068), and ec-matroska write FieldOrder (0x9D)=... / FlagInterlaced=2 (progressive). Not done: it crosses crates and edith must set the value.
No ec-h265 changes were made; no new test (a bitstream-level ffprobe progressive test cannot pass).
