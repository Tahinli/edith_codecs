# lane/hevcwpp: wavefront (WPP) row decode in ec-h265

## Goal
Streams with `entropy_coding_sync_enabled_flag` decode CTB rows in parallel. Row N+1
starts once row N has two CTBs done, with the CABAC context sync (9.3.2.2). Output must
stay bit-exact.

## Base
- Base: `main` at `322837a2` (includes the `lock_ref_planes` self-deadlock fix in
  `dec/inter.rs`; that file is unchanged from main on this lane).
- Lane commits (on top of main):
  - `096461bf` lane/hevcwpp: WIP wavefront rows (the work left by the previous agent)
  - `47436c02` Merge branch 'main' into lane/hevcwpp (one conflict in `enc_p.rs`: took main's
    `predict_rect` and added the `origin` field that the WIP's `PlaneMut` needs)
  - `4b914830` wavefront prefill from the row's bottom rows; publish only complete rows; band clear origin
  - `f556b834` clippy fixes in wavefront parse and motion band

## What changed
- `dec/cu_syntax/wpp.rs` (new module, from the WIP): the row-parallel segment parse
  (`parse_segment`), the per-row band state (`Board`, `enter`/`leave`), and the merge of
  row bands in row order.
- Bugs fixed on this lane:
  1. `Board::prefill` read the bottom rows of row `ry` from `ry << ctb_log2` (its top),
     so a segment starting a row at CTB row 1 underflowed (`y0 - 1` at `y0 = 0`) and the
     row worker panicked. It now mirrors `leave`: bottom rows, origins, and the band unit row.
  2. `parse_segment` published each merged row (`SliceMotion::row_done` and the row
     filter `rows` callback) even when the segment ends inside that row. The next segment
     finishes the row serially, so the collocated motion for the row was published
     incomplete. Later pictures using that picture as the collocated picture then took
     different temporal candidates, and output differed. Now only complete rows
     (`stats.ctus == cols`) are published.
  3. `MotionField::clear` indexed without subtracting `origin4` (wrong for band fields).
     Encoder-only today; fixed for consistency with `store`.
  4. Clippy: a collapsible `if`, `is_multiple_of`, `as_chunks` (were the 3 `-D warnings` errors).

Diagnosis note: the failures were deterministic once the band path ran with one worker
(`44/48` exact on `WPP_A_ericsson_MAIN_2`, first mismatch POC 9). The thread-count
dependence came from when the band path runs (only with spare budget threads). The earlier
"race" reading was wrong; `main` passes the same stream 5/5 at 4 threads.

## Verify
All on host a (`scripts/remote-run.sh hevcwpp --host a -- ...`, `EC_H265_REF_YUV_DIR=$HOME/refyuv/hevcinter`):

| command | result |
|---|---|
| `cargo test --release -p ec-h265 --test conformance_intra -- --nocapture`, `EC_H265_THREADS=1` | `total: 9203 pictures checked, 9203 exact, 147/147 streams accepted` |
| same, `EC_H265_THREADS=2` | same line |
| same, `EC_H265_THREADS=4` | same line |
| same, `EC_H265_THREADS=6` | same line |
| same, `EC_H265_THREADS` unset (default) | same line |
| `cargo test --release -p ec-h265 -p ec-h265-syntax` | pass (18 `test result: ok` lines, 0 failed) |
| `cargo clippy -p ec-h265 --all-targets -- -D warnings` | clean (exit 0) |

Thread setting: `EC_H265_THREADS` (read in `dec/mod.rs::configured_threads`, capped at 6).

Bench: `cargo run --release -p ec-h265 --example decode_bench -- <file>` (best of 3 inside the
example; 120 frames each). Streams: `~/.cache/hevcperf-fixtures/hevc1080_moving.hevc` and
`hevc4k_moving.hevc` (x265, `wpp` on in the SEI options; copied to host a). Milliseconds per
stream run:

| stream | threads | WPP lane (f556b834) | main (322837a2) |
|---|---|---|---|
| 1080p | 1 | 5474.9 | 5430.1 |
| 1080p | 4 | 2311.1 | 2155.7 |
| 1080p | 6 | 1844.1 | 1735.9 |
| 4K | 1 | 24229.4 | 22138.7 |
| 4K | 4 | 7737.5 | 7820.2 |
| 4K | 6 | 6187.3 | 6541.3 |

Checksums agree across all runs: 1080p `2bfe18f415b7408d`, 4K `6d7084bb34f40906`.

## What is left
- **No measured speedup from the rows.** Row workers only take threads the `Budget` has free
  after counting one slot per in-flight picture (`runner.rs`, `PictureSlot`). With frame
  threading active, that likely leaves few or no extra threads on these streams, so the row
  path seldom runs in parallel (not measured directly). The lane is 6 to 7% slower than main
  on 1080p at 4 and 6 threads, about 9% slower at 1 thread on 4K, and faster at 6 threads on
  4K. Run-to-run noise was not measured. To get the gain, the budget needs a policy that lets
  row workers use threads held by pictures waiting on references. Not done here.
- The row-start `prefill` path (a segment starting a row mid-picture) is covered only by
  the conformance streams that use it; no separate synthetic test was added.
