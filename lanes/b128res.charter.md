# lane-b128res — re-measure the parked 128 residual arm after edge128

GOAL: `EC_AV1_B128RES` is written, witnessed, and OFF because (1) RD took it
0-2 times/frame (inert) and (2) bars 2160p q=90 frame 11 disagreed with
ffmpeg by 8 luma samples at a 128 block's horizontal edge. lane-edge128
claimed that mismatch was CDEF search pricing four indices under a root
that carries one literal, fixed with an owner map. This lane decides
whether the residual arm can ship.

Work ONLY in this worktree (`lane-b128res`). Never touch main, never
push, never `git checkout <file>`, never repo-wide `cargo fmt`.
`export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-b128res` on every cargo
command. Logs under `~/.cache/b128res/`. TMPDIR only on gate command
lines, not on the sccache server. Gates: `systemd-run --user -p MemoryMax=14G`,
`--ignored --exact` on both BD tests (they are `#[ignore]`; without
`--ignored` they measure nothing). One gate at a time; record LOADAVG
before AND after. Pins 8291 / 33227. Keep rule: one film row ≥0.5 BD
down on a column, the other flat within ±0.3, screen not worse by 0.3,
wall ≤+15%. Sign: BD-rate, lower better.

## 1. Refute or confirm the 8-sample mismatch (do this FIRST)

Recipe: the native bars 2160p row, `EC_AV1_B128RES=1`, q=90, the frame
and rows named in `encode.rs` `b128_residual` (frame 11, luma rows
126..127, cols 1611..1616). Compare encoder reconstruction vs ffmpeg vs
`decode_stream`. Cite the command.

- If the 8 samples are STILL wrong: STOP. Do not ship. Report the
  remaining mismatch (class, path:line). Leave the default off.
- If they are gone: continue.

INVARIANT from lane-ab128: a skip-only 128-wide piece writes no
`cdef_idx`. `cdef_unit_owner` currently only special-cases `Whole128`.
The residual arm IS Whole128. Do not extend the owner map to Rect128/Ab128
in this lane unless you turn those pieces into residual carriers — that
is a different lane.

## 2. If the mismatch is gone: measure

Control = default (B128RES off) on this HEAD. Arm = `EC_AV1_B128RES=1`.

- `cargo test -p ec-av1 --release --lib -- --ignored --exact --nocapture encode::tests::bd_rate_film_long_gop`
  (deciding). Both films.
- 12-frame guard: `encode::tests::bd_rate_screen_native` with
  `EC_AV1_NATIVE_FILM=1` then `EC_AV1_NATIVE_FILM4K=1` then the screen
  arm, or however the standing table ran them — all five 12-frame rows.
- Residual fire count: `take_b128_residual_hits` / writer
  `take_sb128_residual_hits` on the long-GOP arm. If still 0-2/frame,
  it is still inert even if exact.

Keep → default ON (and drop the "filter mismatch" paragraph). Inert +
exact → leave OFF, but rewrite the comment so it no longer names an
open filter bug. Mismatch remains → OFF, report.

## 3. Witness / pins / suite (only if you change the default)

Pins at default and `EC_AV1_SPEED=6`. Existing residual witnesses
(`a_128_root_block_with_a_real_residual_decodes_exact_through_both_decoders`
and the CDEF-covered-units one) `--ignored`. Workspace check
`timeout 900 cargo check --workspace --all-targets -j4` (0 ec-av1
warnings). Do NOT run the full lib suite unless you change the default;
if you do, three lanes: `--skip stream::` / `stream:: --skip 10bit` /
`10bit`.

## DONE

Commit on `lane-b128res` + `lanes/b128res.report.md` with EVIDENCE lines
(`EVIDENCE: <artifact> | <steps> | <measurement>`), fire counts, BD
table, mismatch gone/still, disposition `ships-on | stays-off | stopped`.
Never merge or push.
