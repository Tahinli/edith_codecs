# lane hevcdecapi: stateful HEVC decoder shell (step 1 of 14)

## Goal
Picture management of H.265 in `crates/ec-h265/src/dec/`: parameter sets, POC (8.3.1),
RPS and marking with missing-picture generation (8.3.2/8.3.3), RPL with list modification
(8.3.4), random-access skipping, DPB bumping (C.5.2). Intra slices keep pixel-decoding through
the existing CTU path. Inter slices are a named stub.

## Base
`131d3a0c` (main, merged into the lane before the final run).

## What changed
- `dec/mod.rs`: `Decoder` (`new`, `push_nal`, `decode_annex_b`, `flush`, `next_frame`,
  `set_pixel_decode`, parameter-set and RPL getters), `NalOutcome`. Refusals by name: 4:2:0 only,
  bit depth above 10, separate colour planes, SPS/PPS RExt/SCC/multilayer extension flags.
  Layer > 0 and types 62/63 are skipped.
- `dec/poc.rs`, `dec/rps.rs` (RPS, marking, generation, `build_ref_lists`), `dec/dpb.rs`
  (buffer, bumping, `Frame`), `dec/inter.rs` (`decode_inter_slice` stub, returns
  `Error::unsupported("HEVC decode: inter prediction", "not implemented yet")` in pixel mode).
- `decode.rs`: `decode_i_slice` is `pub`; `decode_idr_au` is now a thin wrapper over `Decoder`.
- `ec-h265-syntax`: SPS `max_latency_increase_plus1` and `extension_flags` (SPS and PPS) parsed.
  **Bug fix:** `ShortTermRefPicSet::parse` read `delta_idx_minus1` for SPS sets (7.3.7 reads it
  only at `stRpsIdx == num_short_term_ref_pic_sets`). This refused 55 conformance streams.
  The parser now takes `num_sets`. The md5 `chunks_exact` lint is allowed with a reason.
- `tests/dec_shell.rs`: 12 unit tests (POC, RPS marking, long-term matching, generation, bumping,
  latency, reference lists, layer skip) and the JCT-VC sweep. The sweep honours
  `EC_H265_JCTVC_DIR` and `EC_H265_POC_TABLE`.

## Verify (host auto; final commit)
- `cargo test -p ec-h265-syntax --release`: 14 + 2 + 0 passed, 0 failed.
- `cargo test -p ec-h265 --release`: lib 30, conformance 15, dec_shell 13, decode 5, encode 4,
  perf 3, rdoq_cache_hash 3, all passed, 0 failed.
- `cargo clippy -p ec-h265 -p ec-h265-syntax --all-targets -- -D warnings`: **FAILS**, not clean.
  The errors come from code this lane did not change, under the server toolchain (rust 1.98):
  `decode.rs` (too_many_arguments x3: reconstruct_luma, reconstruct_chroma, decode_single_tu),
  `residual.rs` (f64 casts x6, too_many_arguments, collapsible_if, identical_blocks,
  late_init), `transform.rs:71` (manual is_multiple_of). Fixing them touches RDOQ numerics, so
  they are left as a blocker. My own lint in `dec/mod.rs` is fixed.

## POC/output order vs ffprobe (147 streams)
Method: our decoder runs with pixel decode off and reports (POC, access-unit index) per output
frame. ffprobe gives `frame=pkt_pos` in display order, mapped to packet index via
`packet=pos`. PASS means the AU-index sequence matches exactly. Full table:
`lanes/hevcdecapi.poc-table.tsv`.

Result: **138 PASS, 4 FAIL, 5 REFUSED** (of 147).

| stream | ours | ffprobe | result |
|---|---|---|---|
| NoOutPrior_A_Qualcomm_1 | 43 | 40 | FAIL (order prefix matches; count differs) |
| RAP_B_Bossen_2 | 83 | 80 | FAIL (count differs) |
| TSUNEQBD_A_MAIN10_Technicolor_2 | 8 | 0 | FAIL (ffmpeg decodes 0 frames: unequal luma/chroma bit depth) |
| VPSSPSPPS_A_MainConcept_1 | 6 | 2 | FAIL (all POCs 0) |
| PS_B_VIDYO_3 | - | - | REFUSED: SPS range/multilayer/3D/SCC extension |
| SLIST_A_Sony_5, SLIST_B_Sony_9, str | - | - | REFUSED: SPS scaling lists |
| SLIST_C_Sony_4 | - | - | REFUSED: PPS scaling lists |

The FAIL causes are not diagnosed. NoOutPrior and RAP_B point at no_output_of_prior_pics and
EOS/CRA handling. VPSSPSPPS_A needs a look at its picture structure.

## Not done in this lane
- Pixel-exact check of intra-only streams against ffmpeg was not run. Only the encoder
  round-trip tests in `decode.rs` cover pixels. Intra streams with SAO will probably not match.
- The four FAIL streams above are not fixed.
- Clippy is not clean (see Verify).

## Commit trailer
The Co-Authored-By line uses Claude Haiku 5.5, as the attribution instruction for this session
says. ORCHESTRATION.md names Opus 5.5.
