# lane/provenance report

Workspace licence: MIT OR Apache-2.0. Goal: find code or data derived from LGPL/GPL sources, re-source normative spec tables from free public specs, and list the rest for a decision.

Mentions of ffmpeg/x264/x265 as a comparison oracle in tests, docs and scripts (about 200 files) are not findings. The audit was grep-driven plus targeted reads. A port with no marker comment would not show up.

## 1. Done in this lane

| Crate | Result | Spec | Commit |
|---|---|---|---|
| ec-dts | Rewrote `tools/gen_dts_tables.py` to parse ETSI TS 102 114 V1.6.1 (free, etsi.org). Regenerated `SCALE_FACTOR_QUANT6/7` (D.1), `LOSSY_QUANT`/`LOSSLESS_QUANT` (D.2), `QUANT_LEVELS` (5-26), `SCALE_FACTOR_ADJ` (5-27), `DMIXTABLE` (D.11), `HUFF_SRC` (D.5), `LFE_FIR_128` (D.8), `FIR_64` (D.9). Test `tests/spec_tables.rs` rebuilds them from `tests/spec_ts102114_tables.txt` (raw spec numbers) and asserts equality. Values are identical to the old tables. Two spec errata (D65 missing rows, D129 "37" for -37) are handled in the generator. | TS 102 114 | 04bc58bd |
| ec-mpeg4 | New `tools/gen_h263_tables.py` and `src/h263data.rs` from ITU-T H.263 (01/2005): MCBPC intra/inter (Tables 7, 8), CBPY (12), MVD (14), inter TCOEF (16), AIC TCOEF (I.2), CBPC B (O.4), AIC DC scale (I.3), modified quant (T.1, T.2), MBA (K.2), zigzag (Fig. 14), alternate scans (I.2), format and PAR (Tables 2, 3, 5). A `spec_tests` module rebuilds them from spec strings. All values are identical to the old tables. The unused `H263_MBTYPE_B_TAB` was deleted because it matches no spec table. | H.263 | 9ed9e5cf |
| ec-dv | Only the IDCT constants W1..W7 (formula; W4 is 16383) and the standard 8x8 zigzag (T.81 Fig. A.6) were re-sourced, each with a test. Everything else in `tables.rs` stays as it was; see section 3. | T.81 / formula | 1b86cdf7 |

## 2. Audit table

kind: N = normative table in a public spec, C = non-normative code, T = non-normative table, S = tooling. "Fix" says whether the item is re-sourceable from a free spec, but it was NOT done in this lane unless section 1 says so.

### LGPL/GPL (FFmpeg, x264, x265): action needed

| File | Origin | Lic. | Kind | Status |
|---|---|---|---|---|
| ec-dts/src/tables_data.rs (spec part) | dcadata.c, dcahuff.c | LGPL | N | DONE, re-sourced |
| ec-dts/src/tables_residual.rs: `ADPCM_VB`, `HIGH_FREQ_VQ`, `JOINT_SCALE_FACTORS`, `FIR_32_PERFECT`, `FIR_32_NONPERFECT`, `LFE_FIR_64` | dcadata.c | LGPL | N in principle | NOT re-sourceable, see section 3 |
| ec-dts/src/dct.rs, math.rs, core.rs (synth filters ~1186-1380) | probable FFmpeg dcadct/dcamath/dcadsp integer path; "bit-exact vs ffmpeg", not self-declared | LGPL (suspected) | C | Needs a human review |
| ec-mpeg4/src/tables.rs (remaining) | FFmpeg n8.1 | LGPL | N in paid 14496-2 | Section 3 |
| ec-mpeg4/src/msdata.rs | msmpeg4data.c, msmpeg4_vc1_data.c | LGPL | T | No public spec. Section 3 |
| ec-mpeg4/src/dsp.rs | "ports of FFmpeg 8.1 C DSP": simple_idct, xvid_idct, qpel/hpel MC, GMC, dequant | LGPL (xvid_idct is of Xvid GPL origin) | C | Section 3 |
| ec-mpeg4/src/msmpeg4.rs | "follows msmpeg4dec.c" | LGPL | C | Section 3 |
| ec-mpeg4/src/header.rs:92-228,434,456; core.rs:378,456,476-542; frame.rs; codec.rs:79-87; bits.rs:83; h263.rs:24 | decode_user_data, quirk handling, MAX_NVOP_SIZE, get_xbits | LGPL | C | Behaviour matching, some translated. Section 3 |
| ec-mpeg4/src/mb.rs, recon.rs, vlc.rs | no claim, consume FFmpeg-derived tables | LGPL? | C | Review |
| ec-dv/src/tables.rs | dvdata.c, dvdec.c | LGPL | N in paywalled SMPTE 314M | Section 3 |
| ec-dv/src/profile.rs | dv_profile.c, dv.c placement tables | LGPL | T | Section 3 |
| ec-dv/src/video.rs | dvdec.c, "line-for-line port" | LGPL | C | Section 3 |
| ec-dv/src/idct.rs | simple_idct (ff_simple_idct_put_int16_8bit, idct248_put) | LGPL | C | Section 3 (constants done) |
| ec-dv/src/audio.rs | dv_extract_audio (libavformat/dv.c) | LGPL | C | Section 3 |
| ec-dv/src/demux.rs:16,185 | dv_probe scoring, drift resync | LGPL | C | Section 3 |
| ec-aac/src/ps_dsp.rs | aacps.c, aacpsdsp_template.c ("a port of") | LGPL | C | Section 3 |
| ec-aac/src/ps_data.rs | aacps_common.c (`ff_ps_read_data`, READ_PAR_DATA) | LGPL | C | Section 3 |
| ec-aac/src/ps_huff.rs (+ scripts/aac-tables/gen-ps-huff.py) | aacpsdata.c, machine-extracted | LGPL | N (ISO 14496-3 subpart 8, paid) | Section 3 |
| ec-aac/src/sbr_payload.rs:157,732; sbr_chain.rs:739 | FFmpeg behaviours (read_sbr_extension, PS mono copy) | LGPL | C | Low risk, behaviour only |
| ec-ac3/src/mantissa.rs:8-65 | libavutil av_lfg_init state table and the AC-3 dither formula, "checked against libavutil" | LGPL | T+C | Section 3 |
| ec-ac3/src/dependent.rs:10-26 | `ff_eac3_custom_channel_map_locations` | LGPL | T | Section 3 |
| ec-prores/src/idct.rs | `ff_prores_idct_10/12`, "line-for-line port" | LGPL | C | Section 3 |
| ec-mpeg2/src/idct.rs, ec-dnxhd/src/idct.rs | simple_idct W-constants (suspected FFmpeg) | LGPL (suspected) | C | Review |
| ec-dnxhd/src/tables_gen.rs | "generated from VC-3 code tables"; generator missing, possibly dnxhddata.c | unknown | N? (SMPTE ST 2019-1, paid) | Verify |
| ec-truehd/src/decode.rs:658-668; sync.rs:54 | mlpdec.c restart-header defaults | LGPL | C | Small, behaviour only |
| ec-mp4 demux.rs/mux.rs (many lines), ec-mpegts psvideo.rs:275 track.rs:656, ec-matroska mux.rs:403, ec-riff demux.rs:291,401 rawvideo.rs:6 | ffmpeg/mkvmerge output conventions | LGPL/GPL | C | Interop behaviour in comments, no translated source seen. `ec-mp4/mux.rs:1479` (QuickTime chan layouts) should be checked against Apple's spec |
| ec-hw/src/enc/av1.rs:6,167, headers.rs:452 | ffmpeg av1_vaapi tool subset | LGPL | C | Behaviour only |
| ec-h265 cabac.rs:13,263,582, encoder.rs:191 (`entropyBits` table) | x265/HM idiom | GPL / BSD-3 | T | Probably computed from -log2(p); confirm |
| ec-h265 enc_me.rs:13; ec-h264 enc/quant.rs:33-41, enc/mod.rs:83, enc/mb.rs; ec-h265 residual.rs:202-224 | x264/x265 quarter-pel costing, deadzone numerators "as x264 does" | GPL | C (tuning) | Idea only, no copy seen; low risk |
| ec-mp3 huffman_tables.rs, tables.rs, layer12.rs | measured from an oracle decoder via scripts/mp3-tables, scripts/mp2-tables | facts of ISO 11172-3 | N | Not a copy; ISO 11172-3 is paid. Record only |
| ec-aac tables.rs, sbr_tables.rs | black-box probing of the ffmpeg decoder, no source read | none | N | Clean-room by claim. Record only |
| ec-dts/devscripts, ec-truehd/fuzz/seed.sh, gen-*.sh, bench*.sh | ffmpeg/x264/x265 as external tools | n/a | S | Not findings |

### Permissive origin (BSD, ISC, MIT, RFC): notice only

| File | Origin | Lic. | Kind |
|---|---|---|---|
| ec-av1 cdf/mod.rs, cdf/tables_tail.rs | libaom entropymode.c, token_cdfs.h | BSD-2 + AOM patent | N (AV1 spec 9.4, free) |
| ec-av1 qm/iwt_a/b/c.rs | libaom quant_common.c | BSD-2 | N (spec) |
| ec-av1 film_grain.rs (Gaussian seq. + `add_film_grain_run`) | libaom grain_synthesis.c | BSD-2 | N + C (spec 7.18.3) |
| ec-av1 superres.rs, warp.rs, wedge.rs, mc.rs, intra.rs, restoration.rs, quant.rs, transform.rs, compound.rs | libaom filter, warped_motion, reconinter, reconintra, restoration, av1_inv_txfm1d | BSD-2 | N tables + C |
| ec-av1 decode/mod.rs scans; encode/coef.rs `HAS_TR_*`/`HAS_BL_*` | libaom scan.c, txb_common.c, reconintra.c | BSD-2 | N (the has_tr bitmaps are rule-defined in the spec) |
| ec-av1 mvstack.rs, motion_field.rs, decode/*.rs, qm/mod.rs | libaom mvref_common.c, decodemv.c, decodeframe.c, av1_loopfilter.c | BSD-2 | C (spec processes) |
| ec-av1 encode/*.rs | libaom encoder heuristics | BSD-2 | C, inspiration only |
| scripts/extract-eob-class1.py, extract-filter-intra-cdfs.py, build-aom-*.sh, instrument-aom-oracle.sh, check-aom-oracle-rungs.sh | libaom | BSD-2 | S |
| ec-vp8 tables.rs, scripts/extract_vp8_tables.py | RFC 6386 text | RFC BSD-style | N |
| ec-vp8 transform.rs:38-80 | libvpx quant_common.c | BSD-3 | N (RFC 6386 14.1) |
| ec-vp8 mc.rs, loopfilter.rs, transform.rs, modes.rs, tokens.rs, decode.rs, intra.rs | libvpx / RFC dixie | BSD-3 | N tables + C |
| ec-vp9 tables/gen_tables.rs (1497 lines) | libvpx v1.15.0, mechanical extraction | BSD-3 | N (VP9 spec) |
| ec-vp9 tables/mod.rs, intra.rs, transform.rs, loopfilter.rs, tokens.rs, mc.rs, header.rs, adapt.rs, bool.rs, modes.rs | libvpx "ported verbatim" | BSD-3 | C + some N |
| ec-vp9-syntax quant.rs | spec 8.6.1 (equals libvpx) | unknown | N, confirm |
| ec-opus analysis.rs (incl. MLP weights), celt_enc.rs, encoder.rs thresholds, silk_enc_write.rs, silk_enc.rs, range.rs | libopus 1.6 | BSD-3 | C + T (no public spec for the MLP weights) |
| ec-opus celt.rs, silk_tables.rs, silk.rs | RFC 6716 Appendix A | BSD-3 | N; silk_tables.rs lacks a provenance comment |
| ec-hw params/*.rs, ec-va caps.rs and FFI, abi-probe.c | libva headers | MIT | T (ABI layouts, enums) |
| ec-subs ir.rs:10, ec-ass lib.rs:332 | libass semantics | ISC | C (semantics only) |
| ec-hw dec/mod.rs:479 | "edith vendor/ backend/vaapi/decoder.rs" | unknown | C? Needs a human check |
| ec-h264 cabac_tables.rs, tables.rs; ec-h265 cabac.rs, transform.rs; ec-mpeg2 tables.rs; ec-ac3 tables.rs, aht_tables.rs; ec-prores tables.rs | the specs directly | spec | N, no third-party origin claimed |
| shims/* | API facades of third-party Rust crates | check upstream licences | S |

## 3. Items needing a decision (not rewritten)

### Normative tables with no free source

1. **ec-dts/src/tables_residual.rs**
   - `ADPCM_VB`, `HIGH_FREQ_VQ`: TS 102 114 D.10.1/D.10.2 say the codebooks are too large to include. There is no spec text.
   - `JOINT_SCALE_FACTORS`: D.3 prints only 6 significant digits. round(spec x 2^17) differs in 34 of 129 entries.
   - `FIR_32_PERFECT`, `FIR_32_NONPERFECT`: the permuted and scaled spec table reproduces all but 22 of 1024 entries, each off by 1, so printed precision is too low.
   - `LFE_FIR_64`: not a copy of any D.8 column.
   - Options: keep and record the LGPL origin, or derive the FIRs by filter design and accept the output change.
2. **ec-dv/src/tables.rs** (SMPTE 314M / IEC 61834, paywalled, no free source or formula found)
   - `ZIGZAG248`, `QUANT_SHIFTS`, `QUANT_OFFSET`, `NB_VLC`, `VLC_LEN`, `VLC_RUN`, `VLC_LEVEL` (409-entry run/level VLC), `IWEIGHT_88`, `IWEIGHT_248`, `IWEIGHT_1080_Y/C`, `IWEIGHT_720_Y/C`, `AUDIO_SHUFFLE525/625`.
   - `profile.rs`: `PROFILES`, `BLOCK_SIZES_*`, `NTSC_MIN`, `PAL_MIN`, `OFF`, `SHUF1-3`, `L_START*`, `SERPENT1/2`, `REMAP`.
3. **ec-mpeg4/src/tables.rs** (remaining; ISO/IEC 14496-2 is paid; no free copy was found, but not exhaustively searched)
   - `MPEG4_DCTAB_LUM`, `MPEG4_DCTAB_CHROM`, `MPEG4_INTRA_VLC/RUN/LEVEL`, `MPEG4_RL_INTRA_N/LAST`.
   - `INTER_RVLC`, `INTRA_RVLC` (+ `_RUN`, `_LEVEL`), `RVLC_RL_*`. H.263 Annex V has RVLCs only for MCBPC, MBTYPE and MODB.
   - `SPRITE_TRAJECTORY_LENS`, `MB_TYPE_B_TAB`, `MPEG4_DEFAULT_INTRA_MATRIX`, `MPEG4_DEFAULT_NON_INTRA_MATRIX`, `MPEG4_Y/C_DC_SCALE_TABLE`, `MPEG4_DC_THRESHOLD`.
4. **ec-mpeg4/src/msdata.rs** (1825 lines): MS-MPEG4/VC-1 tables from FFmpeg. No public spec exists for MS-MPEG4.
5. **ec-aac/src/ps_huff.rs**: normative in ISO/IEC 14496-3 subpart 8 (paid). It can be regenerated only if someone has the standard.

### Non-normative code translated from FFmpeg (rewrite or relicense decision)

- ec-dv: `video.rs`, `idct.rs` (apart from the constants), `audio.rs`, `demux.rs`.
- ec-mpeg4: `dsp.rs` (including the Xvid IDCT), `msmpeg4.rs`, quirk code in `header.rs`/`core.rs`.
- ec-aac: `ps_dsp.rs`, `ps_data.rs`.
- ec-prores: `idct.rs`.
- ec-ac3: `mantissa.rs` LFG table and dither (the A/52 spec leaves the PRNG to the decoder), `dependent.rs` channel map.
- Suspected, verify: ec-dts `dct.rs`/`math.rs`/`core.rs` synthesis, ec-mpeg2 and ec-dnxhd `idct.rs` (same W-constants as FFmpeg's simple_idct), ec-mpeg4 `mb.rs`/`recon.rs`/`vlc.rs`, ec-dnxhd `tables_gen.rs` (generator missing).
- Options per item: clean-room rewrite from the spec (IDCTs are defined by spec accuracy requirements, not by the simple_idct arithmetic, but bit-exact match to FFmpeg would be lost), relicense those crates as LGPL, or drop them.

### Permissive-origin notices

- libaom (BSD-2 + AOM patent), libvpx (BSD-3), libopus (BSD-3), libva (MIT): carry the licence texts in a NOTICE file. AV1, VP8 and VP9 constant tables are all re-sourceable from their free specs if you prefer, but that was not done here.
- ec-opus `analysis.rs` MLP weights are a trained model copied from libopus (BSD-3), with no public spec.
- ec-vp9 `tables/gen_tables.rs` is labelled "test-oracle provenance" but ships in the library.
- Hard-coded home paths remain in `scripts/extract_vp8_tables.py`.

## 4. Gate

Workspace `cargo clippy --workspace --all-targets -- -D warnings` clean and `EC_REQUIRE_FIXTURES=1 cargo test -p ec-dts -p ec-dv -p ec-mpeg4` green on the server (exit 0).
