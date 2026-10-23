# lane hevcrext (edith)
Commit: one, on lane-hevcrext (local, not pushed): "feat(engine): HEVC range extensions on the software seat".
Files: engine/src/decode/hevc.rs (carried(), generalised crop via export::chroma_420, neutral gray chroma, 12->10-bit wide scaling, PSNR test over 9 shapes),
engine/src/demux.rs (parse_hvcc opens 4:0:0/4:4:4/12-bit/mixed depth; profile >=5 and >12-bit refused by name),
engine/src/decode.rs (hw_decodes: profile != 1,2,3,9 routed to software seat, message updated),
tests: engine/tests/hevc_rext.rs (new: open/probe chroma+depth, order, seeks == linear, export for 8 shapes), hevc_sw.rs, hevc.rs, vp9.rs (refused case now a profile-5 patched hvcC), mcp media.rs (probe reports chroma/bit_depth).
PSNR vs ffmpeg: 422 54 dB, 444 50 dB, gray exact luma (inf), gray12->10 65 dB.
Gate: clippy clean. Workspace tests 3684 passed / 5 failed on first run: hevc.rs and vp9.rs (2) tests used 4:4:4 as the refused case (fixed, re-run passing); 2 known hw_midgop failures. Not re-run in full after the fix.
Refusals kept: HEVC profile >=5 (high throughput, SCC etc.), >12-bit, plus ec-h265's own (extended precision, bypass alignment).
