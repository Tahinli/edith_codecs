# lane hevcpocfix: fix the four POC/output-order FAILs and the PS_B refusal

## Goal
Fix the failing streams in the `hevcdecapi` sweep (`lanes/hevcdecapi.report.md`): NoOutPrior_A,
RAP_B, VPSSPSPPS_A, TSUNEQBD_A, and PS_B_VIDYO_3 (wrongly refused).

## Base
`15931054` (main).

## What changed
- `crates/ec-h265/src/dec/mod.rs`
  - **EOS/EOB no longer drains the DPB.** NAL types 36/37 finish the open picture and set
    `after_eos`, but output nothing. Per C.5.2.2, the next IRAP decides what happens to pending
    pictures. A CRA after EOS (NoOutputOfPriorPicsFlag inferred 1) discards them. An IDR or BLA
    outputs them unless `no_output_of_prior_pics_flag` is set. `flush()` still outputs everything
    at true end of stream. The `NalOutcome::EndOfSequence` doc and the C.5.2.2 comment are updated.
  - **Extension refusal narrowed.** `check_sps` and `check_pps` refuse only when the range,
    multilayer, 3D or SCC flag bits are set (mask `0xf0`). `sps_extension_4bits` alone only says
    that extension data follows, and it is skipped.
- `crates/ec-h265/tests/dec_shell.rs`
  - New `spec_expectation(name)`: for streams where ffprobe is not the reference, it gives the
    spec's frame count and the reason. A stream whose count matches the spec gets verdict `SPEC`
    (counted separately from `PASS`), otherwise `FAIL`.
  - TSUNEQBD_A_MAIN10_Technicolor_2: spec 8 frames, ffmpeg unsupported (unequal bit depths).
  - VPSSPSPPS_A_MainConcept_1: spec 6 frames, ffmpeg outputs 2.
- `lanes/hevcdecapi.poc-table.tsv`: regenerated from the sweep (147 rows).

## Per-stream analysis
- **NoOutPrior_A_Qualcomm_1 (43 -> 40, PASS).** NAL sequence: IDR, ..., EOS, then CRA with RASL
  pictures. The old EOS handler drained the DPB and output 3 pictures that the CRA should discard.
  Now the CRA discards them and the RASL pictures are skipped (NoRaslOutputFlag=1). NoOutPrior_B
  (IDR with no_output_of_prior_pics_flag=1, no EOS) still PASSes at 47.
- **RAP_B_Bossen_2 (83 -> 80, PASS).** Same mechanism at the second EOS, followed by a CRA with
  leading pictures.
- **PS_B_VIDYO_3 (REFUSED -> PASS, 25 frames).** The SPS is Main profile (profile_idc 1). Its
  extension byte is `00000110`: range, multilayer, 3D and SCC bits are 0, and `sps_extension_4bits`
  is `0110`. That was refused because the check tested the whole byte. PPS extension byte is 0.
- **VPSSPSPPS_A_MainConcept_1 (6 vs 2, SPEC).** The conformance description says 6 frames, six IDR
  pictures each with its own VPS/SPS/PPS and resolution. Our 6 matches the spec. ffmpeg outputs 2,
  so ffmpeg is the odd one out. POCs are all 0 because every picture is an IDR; no POC or
  prevTid0Pic bug was found.
- **TSUNEQBD_A_MAIN10_Technicolor_2 (8 vs 0, SPEC).** Luma 10-bit, chroma 9-bit. The description
  says 8 frames. Picture management gives 8. Pixel decode for unequal depths is not implemented;
  the decoder refuses it in pixel mode only.

## Verify (run through `scripts/remote-run.sh hevcpocfix`)
Both hosts were busy for the whole run, so remote-run built and ran locally after its retry window.
- `cargo test -p ec-h265 -p ec-h265-syntax --release`: all pass, 0 failed.
  - ec-h265 lib 30, conformance 15, dec_shell 13, decode 5, encode 4, perf 3, rdoq_cache_hash 3.
  - ec-h265-syntax lib 14, fuzz 2.
  - Sweep: **141 PASS, 2 SPEC, 4 REFUSED** of 147 (REFUSED = SLIST_A, SLIST_B, SLIST_C, str,
    scaling lists, out of scope).
- `cargo clippy -p ec-h265 -p ec-h265-syntax --all-targets -- -D warnings`: fails with 10 errors
  in the lib, all pre-existing: `decode.rs` (3), `residual.rs` (10), `transform.rs` (1). None in
  files this lane changed. Clippy stops at the lib, so the test targets were not linted; the only
  test change is the expectation table and the new `spec_expectation` function.

## Not done
- The four SLIST/str streams (scaling lists) stay refused.
- TSUNEQBD pixel decode (unequal bit depths) is not implemented.
- Clippy errors in `decode.rs`, `residual.rs` and `transform.rs` belong to another lane.
- The sweep's "PASS count" is 141 by ffprobe match. The 2 SPEC streams are spec-correct but do not
  match ffmpeg, so they are not counted as PASS.

## Commit trailer
The Co-Authored-By line uses Claude Haiku 5.5, as the attribution instruction for this session
says. ORCHESTRATION.md names Opus 5.5.
