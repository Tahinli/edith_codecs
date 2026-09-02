# lane-intra14 r4 handoff

TIP: see `git log --oneline -1` on branch `lane-intra14` (r4 tip carries six
merges + the witness fixture + the report).

MERGED THIS ROUND: main 1176a16, lane-intersub8 a5b9770, lane-uv8 7f372b9
(carries lane-interp3 10b801a), lane-inter16ab 2e711e1, lane-rectchroma2
48216c2, lane-sb128c fedb7fe. The non-textual resolutions are tabulated in
`lanes/intra14-r4.report.md` section 1; the one that is a real defect if
re-resolved wrongly: intersub8's `if write_w != write_h -> unsupported
COMPOUND_WEDGE` MUST be deleted, it shadows inter16ab's rect wedge codebook.

STATE OF THE GOAL: the witness exists and is pinned.
`crates/ec-av1/fixtures/hg_intra14_witness.obu` (163057 B, sha256
0eff603bf1608e47faf5e6729670c4c77cf5c674dbad3a1533ac6660151fd90e) = the film's
ss=90 segment truncated to 2 decode-order frames. Its frame 1 codes 3 intra
16x64 strips (`intra_rect4_in_inter` 16x64=3; truncated to 1 frame the counter
is 0, so attribution is pinned). The gate
`a_10bit_film_inter_frame_with_intra_1to4_strips_decodes_pixel_exact`
(crates/ec-av1/src/stream.rs, above `intra_rect4_in_inter_gate`) decodes it,
hard-asserts the counter moved and compares every plane of every frame vs
`ffmpeg_decode_sequence_10bit`.

THE ONE BLOCKER: that frame refuses with "a split intra strip whose transform
unit is 64x32 (no luma coefficient tables for that shape here)". Land
TX_64X32/TX_32X64 luma coefficient tables, delete the `#[ignore]`, done.
Verify with:
`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-intra14 EC_NOMEMGUARD=1 nice -n 10 cargo test -p ec-av1 --lib -j3 -- a_10bit_film_inter_frame_with_intra_1to4_strips_decodes_pixel_exact --include-ignored --nocapture`
(today: refuses at that string, 27.98s).

DO NOT REPEAT: the aomenc recipe hunt (r3 burned 6 x 40-attempt sweeps) and the
film sweep (r4, 27 segments at ss 0..6000: only ss=0/2/5/10 decode more than
one frame -- 33 -- and those code the shape ZERO times; every segment that
codes it stops at its first inter frame).

SUITE: 404 passed / 2 failed / 36 ignored ($HOME/.cache/intra14-suite-r4.log).
Both failures arrived with the merges and are other lanes' shapes:
`a_frame_edge_straddling_band_decodes_pixel_exact` (192x68 cq61 frame 1 Y row 0
col 64, 6859 px) and
`a_real_aomenc_10bit_inter_sequence_with_a_split_transform_intra_block_decodes_pixel_exact`
(seed 54 frame 4 Y (48,16), 2341 samples).
