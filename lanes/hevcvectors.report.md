# lane hevcvectors: full JCT-VC HEVC_v1 conformance set

## Goal
`scripts/fetch-vectors.sh hevc-jctvc` fetched only the 32 hand-picked streams.
Fetch the full HEVC_v1 conformance set (Main, Main10, Main Still Picture).

## Base
`6120482d` (main)

## What changed
- `scripts/fetch-vectors.sh`: the `HEVC_STREAMS` array now lists all 147 zips from
  `https://www.itu.int/wftp3/av-arch/jctvc-site/bitstream_exchange/draft_conformance/HEVC_v1/`
  (flat directory, no RExt/SCC/MV-HEVC/SHVC subdirs). The stale "feature-covering subset"
  comment is replaced. The HEVC_v1 names AMVP/MVCLIP/MVDL1ZERO/MVEDGE/TMVP are Main-profile
  streams, not MV-HEVC.
- `scripts/vectors.sha256`: 115 new `hevc-jctvc/...` checksum lines (147 total for the set).
- `fixtures-manifest/hevc-jctvc.tsv`: one row per stream with profile and md5 presence.

## Verify
Command (run from the main checkout, output into the shared fixtures dir):
`EC_FIXTURES=/home/user/Documents/Code/Rust/edith_codecs/fixtures bash ~/.cache/wt/hevcvectors/scripts/fetch-vectors.sh hevc-jctvc`

Result:
```
OK      hevc-jctvc     files=703 bytes=553874851
downloaded=115 already-present=32 checksum-mismatch=0 sets-ok=1 sets-failed=0
rc=0
```
- Streams before: 32 zips. Streams after: 147 zips, all extracted (147 directories).
- Failed downloads: none. Checksum mismatches: none.
- Syntax: `bash -n scripts/fetch-vectors.sh` passes.
- No cargo build was run, per the lane brief.

## Manifest summary (`fixtures-manifest/hevc-jctvc.tsv`)
- Profile from the name or readme: Main10 11, Main 6, MainStill 1 (IPRED_B_Nokia_3, readme says still picture), `?` 129.
  The profile of the remaining 129 streams is not stated in the name or readme; resolving it needs SPS parsing (`general_profile_idc`), which is out of scope here.
- md5: all 147 streams have at least one md5 file (`*md5*`, which includes `*_md5.txt`).
  Streams with both a `.bit.md5` and a `.yuv.md5` (AMP_D/E/F, CIP_A/C, ...) count as "yes".

## Left
- Profile for the 129 `?` streams (SPS-based classification).
- Whether the fixtures are linked into the worktree (`scripts/link-fixtures.sh`) is not needed; the vectors are in the shared main fixtures dir.
