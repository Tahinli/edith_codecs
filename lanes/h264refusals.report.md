# Lane h264refusals: out-of-scope H.264 refusals, named and tested

## Goal

DONE.md "ec-h264 decode" requires that every out-of-scope stream refuses with
`Error::Unsupported` naming the feature, that a test asserts it, and that every
JVT progressive 8-bit 4:2:0 stream in `scripts/fetch-vectors.sh` decodes bit-exact.

Base: `bdc74c55` (branch `lane/h264refusals`, worktree `~/.cache/wt/h264refusals`).

## Changes

1. `crates/ec-h264/src/decoder.rs`
   - ASO was not refused. A picture's slices arriving with decreasing
     `first_mb_in_slice` decoded into garbage (a late slice 0 was taken as a new
     picture). Added picture identity (`PicKey`: clause 7.4.1.2.4 first-slice fields)
     and `cur_last_first_mb`. A slice of the same picture whose `first_mb` is below the
     previous slice's is refused as `arbitrary slice order (ASO)`. The
     `first_mb == 0` picture boundary is unchanged.
   - `decode_slice_rbsp` now parses the slice header before the boundary check.
   - `check_supported`: `separate_colour_plane` is checked before `chroma_format_idc`
     (idc 3 with the flag set was reported as `chroma format`). Chroma refusals name
     `4:2:2 chroma`, `4:4:4 chroma`, or `monochrome (4:0:0) chroma`. The bit-depth
     refusal is `bit depth above 8 (High10)`. The interlaced refusal is
     `interlaced coding (PAFF field pictures and MBAFF)`. The `why` texts are unchanged,
     so the existing conformance refusal checks still hold.
2. `crates/ec-h264/tests/refusals.rs` (new). Synthetic streams built with `BitWriter`
   (I_PCM slices, no ffmpeg or fixtures needed), one test per feature, plus a
   fixture-gated test that maps each refused JVT vector to its feature.

No fixture was added. `fixtures/` is untouched.

## Behavior before and after

| Feature | Stream (synthetic unless noted) | Before | After |
|---|---|---|---|
| Interlaced, PAFF | SPS frame_mbs_only 0, field_pic 1 | refused, "interlaced coding", untested synthetically | refused, named, tested |
| Interlaced, MBAFF | SPS frame_mbs_only 0, field_pic 0 | refused, "interlaced coding", untested synthetically | refused, named, tested |
| Interlaced (real) | CAMACI3, CAMASL3, CVFI1, CVFI2, CVMAQP2, CVMAQP3, FI1, MR9_BT_B | refused "interlaced coding" (not asserted per vector) | refused, asserted per vector |
| FMO | PPS num_slice_groups 2 | refused "FMO slice groups" | same, tested |
| FMO (real) | FM2_SVA_C | refused | same, asserted |
| ASO | IDR slices MBs 50..98 then 0..49 | **decoded without refusal (garbage)** | refused "arbitrary slice order (ASO)" |
| SP | slice_type 3 | refused "SP and SI slices" (tested in conformance.rs) | same, also in refusals.rs |
| SI | slice_type 4 | refused "SP and SI slices", untested | same, tested |
| Data partitioning | NAL type 2 | refused "slice data partitioning", untested | same, tested |
| High10 | SPS profile 110, bit depth 10 | refused as "bit depth" | refused "bit depth above 8 (High10)", tested |
| 4:2:2 | SPS profile 122, chroma_format_idc 2 | refused as "chroma format" | refused "4:2:2 chroma", tested |
| 4:4:4 | SPS profile 244, chroma_format_idc 3 | refused as "chroma format" | refused "4:4:4 chroma", tested |
| Separate colour planes | SPS profile 244, idc 3 + separate flag | **refused as "chroma format" (wrong name)** | refused "separate colour planes", tested |

## Verify

All run on a server host (chosen by remote-run.sh), via `scripts/remote-run.sh h264refusals --host auto`.

- `cargo test --release -p ec-h264 --test refusals`: 11 passed, 0 failed.
  Before the fix this file failed 5 of 11 (ASO decoded; high10, 4:2:2, 4:4:4 and
  separate-planes names wrong).
- `cargo test --release -p ec-h264` (all targets, nocapture): unit 81 passed;
  conformance 22 passed, 1 ignored (`real_library_streams_match_ffmpeg`, workstation
  only); refusals 11 passed; other integration binaries passed; 0 failed. Log on host
  c at `~/h264refusals-final.log`.
- `cargo clippy -p ec-h264 --all-targets -- -D warnings`: exit 0, no warnings.
- `jvt_full_sequence_bit_exact` before and after: 26 bit-exact, 9 refused by name, 0 failed.

## JVT progressive streams (fetch-vectors.sh h264-jvt set, 35 streams)

All 26 progressive streams are bit-exact on every frame against the reference YUV
(frame count matches):

| Stream | Frames |
|---|---|
| AUD_MW_E | 100/100 |
| BA1_Sony_D | 17/17 |
| BA3_SVA_C | 33/33 |
| BA_MW_D | 100/100 |
| BANM_MW_D | 100/100 |
| BASQP1_Sony_C | 4/4 |
| CABA1_SVA_B | 17/17 |
| CABA2_SVA_B | 17/17 |
| CABA3_SVA_B | 33/33 |
| CACQP3_Sony_D | 50/50 |
| CANL1_SVA_B | 17/17 |
| CANL2_SVA_B | 17/17 |
| CANL3_SVA_B | 17/17 |
| CANL4_SVA_B | 33/33 |
| CI_MW_D | 100/100 |
| CVPCMNL2_SVA_C | 2/2 |
| MIDR_MW_D | 100/100 |
| MR1_BT_A | 62/62 |
| NL1_Sony_D | 17/17 |
| NL3_SVA_E | 33/33 |
| NRF_MW_E | 100/100 |
| SL1_SVA_B | 33/33 |
| SVA_BA2_D | 17/17 |
| SVA_CL1_E | 50/50 |
| SVA_FM1_E | 17/17 |
| SVA_NL2_E | 17/17 |

Refused by name (9): the 8 interlaced vectors above and FM2_SVA_C (FMO).

## Gaps left

- The h264-jvt set has no real SP, SI, data-partitioning, ASO, High10, 4:2:2 or 4:4:4
  stream, so those refusals are proven only on synthetic streams. The synthetic streams
  carry the exact syntax element that fires each refusal.
- The older `refusals_name_a_feature_the_stream_really_uses` (conformance.rs) needs
  ffmpeg and skips without it. `refusals.rs` needs neither.
- The interlaced and High10 decisions remain open (DONE.md D1). The refusals are the
  current answer, not a decode.
- The ASO check uses the picture identity fields. Redundant slices
  (`redundant_pic_cnt` > 0) are not separately handled by this change.
- `scripts/remote-run.sh` in this worktree has uncommitted edits that this lane did not
  make (they appeared during the session). They are left unstaged and not in the commit.
