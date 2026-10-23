# lane/h264interlace report

Branch lane/h264interlace (from main 90b45340), not pushed.

## Outcome
Stage 1 (PAFF field pictures) is done and byte-exact. Stage 2 (MBAFF) is NOT implemented: the fallback applies. MBAFF streams are still refused by name ("MBAFF (macroblock-adaptive frame/field coding)") on the first slice, and tests assert it. Stage 3 (High 10 interlaced) is untestable: all available High 10 interlaced content is MBAFF (x264 --interlaced always writes MBAFF; ffmpeg has no PAFF encoder), and the JVT 10-bit interlaced vectors are 4:2:2 (refused). The High 10 path itself is unchanged and the field code is sample-type generic, so it should work but is unverified.

## JVT interlaced pass list (name: result)
PAFF, all exact versus the reference YUV, frames flagged interlaced:
CVFI1_Sony_D, CVFI1_SVA_C, CVFI2_SVA_C, CVNLFI1_Sony_C, CVNLFI2_Sony_H, FI1_Sony_E, CAFI1_SVA_C, Sharp_MP_Field_1_B, Sharp_MP_Field_3_B, Sharp_MP_PAFF_1r2, CVPA1_TOSHIBA_B, CABREF3_Sand_D, MR6_BT_B, MR7_BT_B, MR8_BT_B, HCAFF1_HHI_B (High profile, 8x8 transform, field scans, CABAC).
MBAFF, refused by name (not mismatched): CAMA1_Sony_C, CAMA1_TOSHIBA_B, CAMA3_Sand_E, CAMANL1/2_TOSHIBA_B, CAMANL3_Sand_E, CAMP_MOT_MBAFF_L30/L31, CVMA1_Sony_D, CVMA1_TOSHIBA_B, CVMANL1/2_TOSHIBA_B, CAPAMA3_Sand_F, CVMAPAQP3_Sony_E, cama1/2/3_vtc, CAMACI3_Sony_C, CAMASL3_Sony_B, CVMAQP2/3, MR9_BT_B, FRExt2/4_Panasonic. (Fixtures not obtainable at the JVT URLs: camp_mot_fld0_full, CAPA1..3, Sharp_MP_Field_2_B.)

## What PAFF covers
Picture structure and pairing of first/second fields, unpaired fields, field POC (types 0/1/2), field DPB with per-field marking, sliding window and MMCO on fields, field ref list init and modification (8.2.4.2.5), frame_num gap fill, field MV and ref handling, chroma mv offset (+-2), field scans and CABAC field contexts, field deblocking (bS 3 on horizontal intra edges, mv vertical limit 2), colocated selection with vertMvScale, temporal direct and implicit weights with field POCs. Frames come out woven; VideoFrame.interlace carries the interlace/field order (new ec_core::frame::Interlace). The 4:2:2/4:4:4 refusals are kept.

## Tests
crates/ec-h264/tests/interlaced.rs: PAFF sweep, FRExt field vector, MBAFF-refused-by-name. refusals.rs and conformance.rs updated (frame_mbs_only refusal removed, MBAFF named). New vectors live in fixtures/vectors/h264-jvt-interlaced (separate dir because CVPA1_TOSHIBA_B's first IDR is not the first displayed frame, which the first-IDR test assumes); scripts/fetch-vectors.sh and vectors.sha256 updated. x264 --interlaced sweep was not added: it yields only MBAFF, so it can only test the refusal (already in conformance.rs).

## Gates
clippy -D warnings clean; full `cargo test -p ec-h264 --release` passes with EC_REQUIRE_FIXTURES=1 (run locally, not on the server). 8-bit progressive hashes identical to base; instruction count on a 1080p progressive clip +0.8% to +1.5% (limit 2%).

## What is left for MBAFF
Per-MB field flag with inference and CABAC ctx (70..72), pair addressing with slot rows, Table 6-4 neighbour derivation for every neighbour reader (nz counts, intra modes and samples, mv/ref/mvd prediction, cbp/skip ctx, qp), geometry swap for field MBs inside a frame, ref lists doubled for field MBs, MBAFF direct modes, and mixed-edge deblocking (left edges 8-row split, top edge double filtering). Design is settled (Table 6-4 was transcribed and cross-checked); roughly 100 neighbour-read sites in decoder.rs, mv.rs, cabac.rs and the intra gather functions need an indirection.

## Provenance
The H.264 spec text (Table 6-4, 8.4.1.2, 8.4.1.3.2 etc.) was used for all tables and rules; no other decoder's source was copied. ffmpeg was used only as a byte-exactness oracle together with the JVT reference YUVs.
