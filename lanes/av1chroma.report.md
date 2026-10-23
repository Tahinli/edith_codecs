# lane av1chroma: ec-av1 4:4:4 / 4:2:2 / 12-bit status and first real defects

Base: main `00054596`. Worktree `~/.cache/wt/av1chroma`, branch `lane/av1chroma`.
**No decoder source changed.** This lane is a measurement report. Its findings
change the premise of the charter, so they are recorded here before any fix.

## Premise correction

The charter says ec-av1 refuses everything except 4:2:0 at 8/10 bit. That is no
longer true of this tree:

- The sequence-header refusal for non-4:2:0 is gone (`stream.rs`, lane
  `av1422lift`). `refusal_inventory.rs` records the 12-bit blanket refusal as
  replaced (`lane-av112bit`), and the 12-bit warp refusal is lifted
  (`decode/mod.rs:31`).
- Prior lanes already added 4:4:4 CfL, rect and palette paths, 4:2:2 chroma
  units, film grain per-axis geometry, and 12-bit `round_0 = 5` (`mc.rs`,
  `restoration.rs`).
- The only refusal that fires on a non-4:2:0 stream is the sub-8x8 plane-block
  rule (`refuse_invalid_plane_block`, `decode/block.rs:1108`), which is
  libaom's `Block size %dx%d invalid` check (`ss_size_lookup`).
- `DONE.md` (lines 77 and 113) still lists profiles 1/2 as **Frozen**. The charter
  overrides that, but the frozen note itself should be updated by the owner.

Existing server evidence: `~/gates` on host a has `444sb-suite.log` and
`repo-444sb`. Host b has `422ctxgather-suite.log`, `av1444128-suite.log`,
`lr444-suite.log` and `12sc8d-suite.log`. These are prior work and were not
re-run. `git log --all` grep for 422 finds about 100 `av1422*` lanes.
Host c has no `~/gates`.

## Method

Clips were generated locally with ffmpeg 8.x `libaom-av1` (`-cpu-used 2`,
`-lag-in-frames 0`, `-crf 25`, `-b:v 0`) from `mandelbrot` plus `noise=alls=12:allf=t`,
in all eight target formats. The variants are:

- `p_*`: CDEF and LR off, 352x288, 8 frames;
- `r_cdef_*`, `r_lr_*`, `r_fg_*`: CDEF on, LR on, or film grain (`-denoise-noise-level 30`).

Oracle: `ffmpeg -c:v libdav1d -i X.obu -f rawvideo -pix_fmt <fmt>`.
Decoder: `cargo run --release -p ec-av1 --example dump_yuv -- X.obu out`, run on host b
through `scripts/remote-run.sh av1chroma --host b`. Byte comparison used `md5` of the
concatenated frames.

Fixtures are in `/tmp/claude-1000/-home-user-Documents-Code-Rust-edith-codecs/c1cdf2b5-40e6-4004-b10f-cef858f82563/scratchpad/av1c`, and are not committed.

## Results (byte-exact vs libdav1d)

| clip | 8-bit | 10-bit | 12-bit |
|---|---|---|---|
| 4:2:0, 160x96, 6 frames | EXACT | - | EXACT |
| 4:4:4, 160x96, 6 frames | EXACT | EXACT | EXACT |
| 4:2:2, 160x96, 6 frames | EXACT | EXACT | EXACT |
| 4:2:0, 352x288, CDEF / LR / grain | EXACT | - | EXACT |
| 4:4:4, 352x288, CDEF | EXACT | EXACT | **DIFF** (V plane) |
| 4:4:4, 352x288, LR | EXACT | EXACT | **DIFF** (V plane) |
| 4:4:4, 352x288, grain | EXACT | EXACT | EXACT |
| 4:2:2, 352x288, CDEF / LR / grain | **DIFF** | EXACT | EXACT (CDEF, grain) |
| 4:2:2, 352x288, LR | **DIFF** | EXACT | **REFUSED** (see D3) |
| 4:2:2, 352x288, CDEF/LR off | **DIFF** | - | - |

The 8-bit 4:2:0 CDEF, LR and grain clips are EXACT. The 4:2:0 12-bit clips
are EXACT.

## Defect D1: 4:2:2 chroma at the right frame edge (8-bit only in these clips)

Reproduces with CDEF and LR off (`p_422.obu`), so the defect is in
reconstruction, not in a filter. Frame 3 is the first to differ. The diffs
are in the chroma columns x=168..175 of a 176-wide plane, that is luma 336..351
of a 352-wide frame. They are near rows 72..95. The luma plane is exact.

This is the same family as the open report `lanes/av1422h242map.report.md`
(commit `586b5460`, \"4:2:2 sub-8x8 chroma DC_PRED on a 4x4 chroma unit at the
frame's right edge\"). That report names the mechanism: the 4:2:2 sub-8x8
chroma leaf reads its left column four rows too high. It stopped at "report
only" and made no source change. This lane's `EC_PRED` trace is consistent with
that, but it did **not** localise the exact line. Frame-to-block mapping was
ambiguous in the trace, so the fix is still open.

Suggested next measurement: for frame 3 of `p_422`, dump the left and above
samples used by the chroma block at x=172, y=72 (4x8) and compare them with the
pixels that libaom's `av1_setup_dst_planes` reads for the same cell.

## Defect D2: 12-bit 4:4:4 V plane, off by one (pre-filter)

`p444.obu` (CDEF and LR off) is already wrong on frame 0, a key frame. Only the
V plane differs. Diffs are 274 samples in frame 0, almost all +1, in a box
x=142..155, y=160..183. The U plane and 10-bit 4:4:4 are exact. Film grain
at 12-bit 4:4:4 is also exact, which suggests the defect is in the residual or
dequant path for V, not in the filters. The likely places are the V quantizer
offsets (`DeltaQV`), the V quantizer matrix, or 4:4:4 CfL (the V prediction
uses the U AC). None has been checked yet. The `EC_PRED` trace shows DC blocks
near that cell, not CfL.

## Defect D3: 12-bit 4:2:2 LR clip refused

`r_lr_yuv422p12le.obu` is refused with `a block size 4x8, 8x16 or 16x4 ... has no
chroma plane block`. That is libaom's `Block size %dx%d invalid` check (`decodeframe.c:1456`).
dav1d decodes the same stream without error. The stream is produced by
libaom's encoder. So either libaom's encoder can emit a partition that its own
decoder rejects, or the decoder took a wrong branch earlier (a partition
symbol read out of sync). This was not resolved, and the refusal stays by name.

## Verify commands

- `scripts/remote-run.sh av1chroma --host b -- bash -c 'cargo run --release -p ec-av1 --example dump_yuv -- <obu> <out>'`
- oracle: `ffmpeg -v error -c:v libdav1d -i <obu> -f rawvideo -pix_fmt <fmt> <ref.yuv>`
- compare: `md5sum` of `<out>.f*.yuv` concatenated vs the reference

No cargo test, clippy or line-count gate was run, because no source changed.

## Not done

1. D1 and D2 are not fixed. Neither has a failing test yet.
2. Sub-features not covered: superres in 4:2:2/4:4:4, CfL 4:2:2 averaging,
   film grain 12-bit 4:2:2 at other sizes, the Argon profile 1/2 streams.
3. AOM 242/242 was not re-run, because there is no code change.
4. DONE.md still says profiles 1/2 are Frozen (lines 77 and 113).
