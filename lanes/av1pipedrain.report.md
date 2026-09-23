# lane-av1-pipedrain — test-infra pipe-drain fix

Branch `lane-av1-pipedrain` @ main `5f5aef9a`. Worktree
`/home/tahinli/Documents/Code/Rust/edith_codecs-av1pipe`. No push.

## Defect

Every `a_real_aomenc_*` gate spawned aomenc with piped stdin/stdout/stderr and
wrote the y4m down stdin INLINE before `wait_with_output()`. When aomenc's
stdout pipe buffer (~64 KiB) filled before the last input byte was written,
the child blocked in write(2) on stdout and the test blocked in `write_all`
on stdin — both at 0% CPU until the harness timeout, or EPIPE flakes when the
child was killed. Hit four times across lanes today; the class was already
diagnosed once for the ffmpeg helpers (lane-t900 r10: 45-minute hang on a
1.1 MB fixture; lane-gaterecipe r1: one 384x256 cq-10 encode hung 40 minutes
until a hand-rolled feeder thread fixed that ONE aomenc site).

## Census (before)

`crates/ec-av1/src`, piped-stdin spawn sites (child stdin piped AND fed):

| file      | aomenc gates | ffmpeg helpers | total |
|-----------|-------------:|---------------:|------:|
| stream.rs | 139          | 4              | 143   |
| decode.rs | —            | 2              | 2     |
| encode.rs | —            | 2              | 2     |
| tile.rs   | —            | 1              | 1     |
| **all**   | **139**      | **9**          | **148** |

Of the 148: **138 were dangerous** (inline `write_all` before
`wait_with_output`, incl. compact `.unwrap()`/`.ok()` shapes and
EPIPE-tolerant `.or_else(BrokenPipe)` shapes — the latter only help when the
child EXITED, not when it is alive and blocked) and **10 were safe** but
duplicated the feeder-thread pattern (9 ffmpeg decode helpers + 1 aomenc
gate, the lane-gaterecipe r1 site). The intrabc merge-close "~150 sites, one
already feeder-thread-fixed" = these 148.

Untouched by design: 2 stream.rs aomenc spawns fed by FILE
(`stdin(Stdio::null())`, `.arg(&y4m_path)`) and every ffprobe/ffmpeg
`.output()` / `.status()` site (nothing feeds stdin, so no cycle exists).

## Fix

One shared helper, `crates/ec-av1/src/probe.rs` (`#[cfg(test)]`):

```rust
pub(crate) fn run_with_stdin(cmd: &mut Command, input: &[u8]) -> std::process::Output
```

- sets piped stdin/stdout/stderr, spawns the child;
- a writer thread owns stdin (`let _ = stdin.write_all(&payload)` — EPIPE is
  swallowed on purpose: the child's exit status and stderr, asserted by the
  caller, are the real diagnosis; aomenc refusing early can never panic the
  feeder);
- the parent runs `wait_with_output()`, which drains stdout AND stderr
  concurrently — no pipe can fill against a blocked writer, so multi-MB y4m
  inputs always meet a reader;
- joins the writer thread, returns `Output`.

Every one of the 148 sites now calls it (the ffmpeg decode helpers keep their
pixel-parsing tails; callers keep their own `assert!(out.status.success(), …
{stderr})` messages). Migration: a bounded, validating Python codemod
(transformed 136 stream.rs sites; runaway guards — bounded window, exactly
one `write_all`, no `assert!`/`decode_stream`/`Command::new` in the replaced
span — left anything non-uniform for hand edits) + 12 hand migrations
(feeder-thread site, `let out = { … }` bare-expression block, affine-oracle
`Command::new(affine_aomenc_path())` site, `.ok()`/`unwrap` compact shapes,
`let enc` binding, 9 ffmpeg helpers).

Grep proof (final tree): zero `stdin.take()`/child-stdin `write_all` outside
`probe.rs`; `wait_with_output` appears only inside the helper and at the two
file-fed spawns. Zero `stdin(Stdio::piped())` outside `probe.rs`.

Diff: 5 source files, +1065/−2492 (net −1427 lines of duplicated boilerplate
removed); whitespace-insensitive +871/−2298.

## Verification

Toolchain: aomenc oracle `~/.cache/aom-oracle/build/aomenc` + affine oracle
`~/.cache/aom-affine/build/aomenc`, system ffmpeg; release test binary;
`EC_AV1_REQUIRE_AOMENC=1 EC_AV1_REQUIRE_FFMPEG=1` (skips fail the run).

- `cargo check -p ec-av1 --all-targets`: **0 warnings**.
- Stress, previously-flaked gate
  `a_real_aomenc_inter_sequence_with_intra_16x4_strips_in_1to4_partitions_\
decodes_pixel_exact` × 20 sequential runs:
  **20/20 pass, 0 fail** (28.3 min wall; each run does the full multi-attempt
  encode sweep + pixel compares).
- Scoped battery, 50 gates through the migrated paths (intrabc unblock set
  13, rect14 16x4/4x16 set 5, loss64/lossless set 4, real-aomenc family
  25 incl. tile-rows/columns, tile-group OBUs, sb128 family, obmc, wedge,
  comp-mode, temporal-MV, cdef, filter-intra, 1D-tx rect, palette-on-rect,
  10-bit; plus 3 tile::tests ffmpeg-decode helpers):
  **50/50 green** (two initially misnamed entries re-checked by exact name,
  both pass; census sweeps among them ran their full 20–40 attempt loops).

## Notes / follow-ups

- The repo is NOT rustfmt-clean under default `rustfmt --edition 2024`
  (pristine stream.rs shows a 7776-line diff), so no wholesale fmt pass was
  applied. One honest exception: tile.rs carries ~600 lines of rustfmt reflow
  around the ffmpeg_decode helper migration (raw churn 1051 lines vs 663
  whitespace-insensitive; 388 lines differ by whitespace alone) — format-only
  churn, semantically inert, not a defect. Any future
  "just cargo fmt it" would churn ~40k-line files wholesale; decide that
  separately, never inside a content lane.
- `ec-av1-syntax` and `examples/` spawn no aomenc/ffmpeg on piped stdin;
  other ec-* crates are out of this lane's scope but share none of these
  helpers.
- The helper names the generic spawn/reap failures ("encoder failed to
  start/run"); per-gate diagnosis stays in each gate's own
  `assert!(status.success(), …)` on stderr, unchanged.
