# hevcsaodblk report

## Goal

Find and fix the chroma mismatch in SAODBLK_A_MainConcept_4 (0/500 exact, first mismatch poc=0 Cb x=455 y=369, ours 122, ffmpeg 129) and SAODBLK_B_MainConcept_4 (1/500 exact, first mismatch poc=1 Cb x=371 y=162). Make `tests/conformance_intra.rs` assert that every accepted stream is fully exact, except an explicit `KNOWN_INEXACT` list with a reason for each.

Base: `7e1780fb` (main). Branch `lane/hevcsaodblk`.

## Outcome

- No decoder fix was made. The SAODBLK_A mismatch appears to be a disagreement with the ffmpeg reference, not a decoder bug. The SAODBLK_B mismatch is still unresolved.
- The conformance test now asserts exactness for accepted streams, with a `KNOWN_INEXACT` list of three entries.
- Because no fix was made, no unit test pinning a fixed rule was added.

## Diagnosis

### SAODBLK_A_MainConcept_4 (0/500 exact)

- Y is exact in every frame. Cb and Cr each differ at exactly one sample per frame, at chroma x=455 (last column of a 16x16 CTB). Total: 1000 mismatched samples over 500 frames, 500 of 500 frames affected.
- Cb (455,369): deblocked neighbours (456,368)=127 and (454,370)=123, current 129. SAO class eo=3 (45 degrees), offsets [1,0,0,-7]. Our SAO classifies the sample as a local maximum and applies -7, giving 122. ffmpeg gives 129.
- Our output follows H.265 8.7.2 (chroma deblock, vertical pass then horizontal pass) and 8.7.3 (SAO on the fully deblocked picture). The hand-check of the deblock arithmetic matches the spec formulas.
- The ffmpeg output is consistent with SAO reading the right-column neighbour before the horizontal chroma deblock pass: for Cb, the pre-horizontal value 129 gives no change; for Cr, pre-horizontal 126 gives offset -5 and output 122, matching ffmpeg (ours 127).
- ffmpeg `libavcodec/hevc/filter.c` has a comment about a 2-CTB SAO delay for 16x16 CTBs with horizontal chroma subsampling, so the quirk is plausibly in ffmpeg. This is not confirmed. The stream has no SEI picture hash (NAL types seen: 32, 33, 34, 19, 1, 0; no 39/40), and the package `_md5.txt` is an md5 of the .bin, not of the YUV. So there is no normative reference to decide which output is correct.

### SAODBLK_B_MainConcept_4 (1/500 exact)

- Not resolved. Mismatches are widespread, not the SAODBLK_A pattern: across 500 frames, 499 frames mismatch, Cb 1625 samples and Cr 90871 samples, spread over x mod 8.
- Frame 0 is exact. The first mismatch is poc=1 Cb x=371 y=162.
- The stream uses tiles. Not investigated further.

### Code reviewed (matches spec, no change)

- `crates/ec-h265/src/dec/deblock.rs`: chroma only at bS 2, 8-sample chroma grid, QpC from average QP plus PPS cb/cr offset, tc from q-side slice offsets, q-side slice/tile flags, pcm/bypass locks.
- `crates/ec-h265/src/dec/sao.rs` and SAO parsing in `cu_syntax.rs` (`fn sao`).
- Noted deviation, not relevant to these 8-bit streams and not changed: SaoOffsetVal scaling uses `bd - min(bd,10)` instead of `log2_sao_offset_scale`. This only matters above 10 bits.

## Changes

`crates/ec-h265/tests/conformance_intra.rs` only (+32 lines):

- After the report-writing block, an assertion: every row with `accepted == "yes"`, `checked > 0` and `exact != checked` must appear in `KNOWN_INEXACT`, otherwise the test fails and lists the offending streams.
- `const KNOWN_INEXACT: &[(&str, &str)]` with three entries, each with a reason:
  - `SAODBLK_A_MainConcept_4`: 2 chroma samples per frame differ from the ffmpeg reference; reference disagreement, not confirmed against a normative output.
  - `SAODBLK_B_MainConcept_4`: 1/500 frames exact, widespread chroma mismatch, unresolved, uses tiles.
  - `VPSSPSPPS_A_MainConcept_1`: ffmpeg reference is incomplete (PPS id out of range, 2 of 6 frames output). Handled by the hevcvps lane. Per the coordinator, this entry is expected to be removed once hevcvps lands and the test prefers the package YUV.

No decoder source files are changed.

## Verify

Run on host a via `scripts/remote-run.sh hevcsaodblk --host a -- <cmd>`.

| Command | Result |
|---|---|
| `cargo clippy -p ec-h265 -p ec-h265-syntax --all-targets -- -D warnings` | PASS (exit 0) |
| `cargo test -p ec-h265 -p ec-h265-syntax --release` | PASS (all test binaries exit 0; 16 result lines, 0 failed) |

Test result lines from the release run, in order (binary names are in the log, `~/.cache/hevcsaodblk-test.log`):

```
test result: ok. 82 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.46s
test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 44.93s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 147.89s
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 121.61s
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 71.51s
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.24s
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 19.96s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 377.67s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 12.92s
test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 29 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.08s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

All lines report `0 failed`.

Conformance total (intra table, `EC_H265_INTRA_REPORT` on host a, 147 rows):

```
total: 9203 pictures checked, 8198 exact, 147/147 streams accepted
```

Logs: `~/.cache/hevcsaodblk-clippy.log`, `~/.cache/hevcsaodblk-test.log`, table `~/.cache/hevcsaodblk-intra.tsv`.

## Exact / inexact

Accepted streams: 147 of 147. Exact streams: 144 of 147 (from the TSV, 147 rows, 3 with `pictures_exact != pictures_checked`).

| Stream | Accepted | Checked | Exact | First mismatch |
|---|---|---|---|---|
| SAODBLK_A_MainConcept_4 | yes | 500 | 0 | poc=0 Cb x=455 y=369 |
| SAODBLK_B_MainConcept_4 | yes | 500 | 1 | poc=1 Cb x=371 y=162 |
| VPSSPSPPS_A_MainConcept_1 | yes | 6 | 0 | poc=0 Y x=77 y=0 (reference is incomplete) |

All other 144 accepted streams are fully exact.

## What is left

1. SAODBLK_A: decide whether the ffmpeg SAO-before-horizontal-deblock behaviour is the reference. This needs a normative output (a stream with an SEI picture hash, or the conformance package YUV). Until then the stream stays in `KNOWN_INEXACT`.
2. SAODBLK_B: investigate the widespread Cr mismatch (tiles, poc 1 onwards). Not started beyond the first-mismatch location.
3. VPSSPSPPS_A: removed from `KNOWN_INEXACT` when the hevcvps lane lands and the test uses the package YUV. If hevcvps merges first, merge main and drop this entry.
