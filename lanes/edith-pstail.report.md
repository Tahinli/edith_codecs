# pstail (edith-pstail, branch lane-pstail)
Final commit: e2e3d57c (on top of 83e9a850). No trailers, not pushed.
- tests/mpegps.rs: the `-f mpeg` clip must play exactly 60 frames; the joined-VOB test asserts length 4.8 s +/- 0.03, 120 frames, and seeks [5,33,59,60,66,70,80,119] equal the sequential frame; second loop and the now-unused `flash_frame_in` removed.
- demux/ps.rs: dropped the "joined VOB time approximate" known-limit note. ts.rs had no tail-frame note. The 20/24-bit LPCM read-from-the-top note is kept.
- No edith-side regressions; no ec-mpegts bug found.
Gates on host c:
- mpegps (11) and mpegts (12) tests pass.
- clippy -D warnings on engine/edith_mcp/edith --all-targets: clean (exit 0).
- Full suite: only failures are the 2 known VA-API ones (h264_/hevc_mid_gop_seeks...) plus a_trim_widened_past_the_analysis_is_stabilised_end_to_end. That one failed while another cargo suite ran on host c and passes alone on the same commit (load flake).
