# lane-rectres — non-skip residual on Rect128/Ab128 pieces

Base: `3b4673fe` (lane-rectres worktree). Disposition: **stays-off**
(inert + exact — the RD takes 0 rect/AB residual fires per frame on every
gate clip). `EC_AV1_RECTRES` keeps its default (off): OFF is today's
behavior bit for bit — the search never flips a rect/AB piece to non-skip,
so the writer's lifted refusal is never reached. Pins untouched at
8291 / 33227 (no default change, so no pin run, no lib suite). Never
merged, never pushed.

## 1. What shipped (commit `26a9b79e`)

- `write_inter_block_128_rect` (tile.rs): the skip-only refusal is lifted.
  A residual piece codes one TX_64X64 luma unit plus a TX_32X32 chroma pair
  per 64x64 mu chunk (two chunks for a 128x64/64x128 half, one for a 64x64
  square), var-tx depth 0; under `TxMode::Select` a half codes two depth-0
  `txfm_partition` flags (ctx rect at `w.max(h)=128`), a square one (via
  `write_tx_syntax_inter`). A half's chroma unit takes the +3
  `get_txb_ctx` offset; a square's takes none. Luma `txb_skip_ctx`: the
  neighbour magnitude table on a half, fixed 0 on a square (the transform
  IS the whole plane block — `read_inter_plane`'s
  `luma_skip_ctx.unwrap_or(0)`, the 64 root's own writer rule). Tail =
  `record_planes_rect(luma=false)` off the assembled chroma grids, then the
  per-chunk chroma re-stamp on halves only (`mu_chroma` mirror).
- `search_rect_residual` (encode.rs): prices the non-skip outcome against
  the skip arm the search just committed (per-chunk
  `code_from_prediction` trials, `unskip` skip-symbol delta, packed 64-wide
  level grids), commits + flips `block` on a win; wired into
  `search_root_128_rect` and `search_root_128_ab` behind
  `rectres()` (`EC_AV1_RECTRES`, default OFF, test force
  `force_b128_rectres`).
- `cdef_unit_owner` (tile.rs) extends past Whole128: a non-skip Rect128
  half collapses its 2 units onto its origin; an Ab128 half likewise (its
  64x64 squares are 1 unit each). Skip-only pieces never touch the map —
  they code no literal and the decoder's per-piece `cdef_transmitted`
  reset never fires. This is the lane-edge128 bug class, priced for the
  ONE index the syntax carries.
- Census: the native arm's standing line now prints "rect/AB pieces with a
  residual N (search) / M (writer)" and zeroes both counters per clip.
- A defect found and fixed during witnessing (would have been the lane's
  own bug class): the first draft priced the 64x64 square's luma
  all-zero flag off the neighbour table; the decoder fixes it at 0 for a
  whole-block transform, and the desync surfaced as an empty-slot
  reference refusal mid-stream (encode.rs witness fixture), not a pixel
  miss. Fixed before any gate measured anything.

## 2. Witnesses (all `--ignored --exact`, standalone, all PASS)

- `a_rect128_half_with_a_real_residual_decodes_exact_through_both_decoders`
  (smpte pan 1280x768, 8 frames, q=60, HORZ forced + residual forced):
  search won 840, writer coded 840; `coded > 0` guard; whole stream
  sample-exact through `decode_stream` AND ffmpeg, all planes/frames.
  EVIDENCE: ~/.cache/rectres/wit1.log | forced HORZ + rectres, q=60, 8
  frames | "search won 840, writer coded 840", ok, both decoders exact.
  LOADAVG 118.48/98.99/57.92 before, 118.48/98.99/57.92 after (sibling
  lane storm; 3.6s run).
- `an_ab128_piece_with_a_real_residual_decodes_exact_through_both_decoders`
  (testsrc2 1280x768, 12 frames, q=60, HORZ_A forced + residual forced):
  search won 4620, writer coded 5760 (writer counts include the filter
  search's winning re-code); exercises the two-chunk half AND both
  one-chunk squares; byte-exact through both decoders.
  EVIDENCE: ~/.cache/rectres/wit2.log | forced HORZ_A + rectres, q=60, 12
  frames | "search won 4620, writer coded 5760", ok. LOADAVG
  107.04/97.41/58.06 before, 104.55/100.35/63.37 after.
- `a_rect128_residual_half_under_a_per_unit_cdef_list_decodes_exact`
  (testsrc2, 12 frames, q=90, HORZ forced + residual forced): writer coded
  2760 non-skip halves; 62 64x64 CDEF units took another unit's literal
  (owner-map collapse firing on rect pieces); byte-exact through both
  decoders. EVIDENCE: ~/.cache/rectres/wit3.log | --ignored --exact run |
  "writer coded 2760; 64x64 CDEF units taking another unit's literal: 62",
  ok. LOADAVG 7.90/7.85/9.32 before, 9.39/8.16/9.37 after.

## 3. BD measurement (VPS-2, per Main's recipe)

The worktree has no `fixtures/`, so all four gates ran on VPS-2
(tCloud@2.28.124.204, 4 vCPU, MemoryMax=5G units, ffmpeg PATH shim). The
sanity gate PASSED before drawing any conclusion: the control run
reproduces the standing table (b128res.report §2/§3) to the digit —
long-GOP film A +20.9/-9.0 and film B +70.4/-2.2, and all five native
rows identical (±0.0 per cell, requirement was ±0.2).

### 3a. Long-GOP BD (deciding; 48 pictures, gop=48, both films)

Control = default (off), arm = `EC_AV1_RECTRES=1`, same HEAD. BD-rate,
lower better:

| row | off vs libaom | off vs rav1e | on vs libaom | on vs rav1e |
|---|---|---|---|---|
| film A (1080p) 1920x768 | +20.9% | -9.0% | +20.9% | -9.0% |
| film B (2160p HDR) 1920x1024 | +70.4% | -2.2% | +70.5% | -2.1% |

Wall ours: film A 1394.1s -> 1417.6s (+1.7%), film B 1076.6s -> 1118.3s
(+3.9%).

EVIDENCE: ~/.cache/rectres/longgop-control.log | VPS-2 gate, MemoryMax=5G,
ffmpeg shim PATH | film A +20.9%/-9.0%, film B +70.4%/-2.2%; three-way
exactness assertion held at every clip x q x frame. LOADAVG 0.15/0.17/0.18
before, 1.08/1.09/1.09 after.
EVIDENCE: ~/.cache/rectres/longgop-arm.log | VPS-2 gate, EC_AV1_RECTRES=1,
MemoryMax=5G | film A +20.9%/-9.0%, film B +70.5%/-2.1%. LOADAVG
0.14/0.23/0.58 before, 1.05/1.08/1.08 after.

### 3b. 12-frame native guard (all five rows)

| row | off vs libaom | off vs rav1e | on vs libaom | on vs rav1e | wall ours off->on |
|---|---|---|---|---|---|
| bars 1080p | -3.3% | -19.0% | -3.4% | -19.0% | 424.8s -> 435.7s |
| bars 2160p | +8.4% | -13.9% | +8.5% | -13.9% | 365.0s -> 372.8s |
| film A | +17.9% | -6.3% | +17.8% | -6.4% | 369.4s -> 376.9s |
| film B | +22.6% | -3.8% | +22.5% | -3.9% | 311.9s -> 318.4s |
| screen capture | +14.4% | -33.2% | +14.4% | -33.2% | 246.4s -> 249.2s |

EVIDENCE: ~/.cache/rectres/native-control.log | VPS-2 gate, five rows,
MemoryMax=5G | table above, "off" columns; matches the standing table to
the digit. LOADAVG 0.09/0.20/0.56 before, 1.14/1.15/1.09 after.
EVIDENCE: ~/.cache/rectres/native-arm.log | VPS-2 gate, five rows,
EC_AV1_RECTRES=1 | table above, "on" columns. LOADAVG 0.08/0.42/0.77
before, 1.07/1.10/1.09 after.

### 3c. Fire census (arm runs; per clip, over the four-q ladder)

| clip | rect/AB residual fires (search / writer) |
|---|---|
| film A long-GOP (192 frames) | 0 / 0 |
| film B long-GOP (192 frames) | 0 / 0 |
| bars 1080p (48 frames) | 0 / 0 |
| bars 2160p (48 frames) | 0 / 0 |
| film A 12f (48 frames) | 0 / 0 |
| film B 12f (48 frames) | 0 / 0 |
| screen capture (48 frames) | 0 / 0 |

ZERO fires everywhere under the knob: unlike the parked whole-128 arm
(0.4-1.9 writer fires/frame), the RD never once finds a non-skip rect/AB
piece worth taking on real content. The arm is not merely flat — it never
fires. The wall deltas above (+1-4%) are the trial cost paid on every
skip-winning rect/AB piece for a win that never comes.

## 4. Keep rule — NOT met; disposition

One film row >=0.5 BD down on a column: best delta anywhere is -0.1.
Other film row flat within +/-0.3: yes. Screen not worse by 0.3: yes.
Wall <= +15%: yes. The deciding column fails and the arm is inert
(0 fires) — the charter's stays-off branch, now on measurement rather
than on a missing fixture. Default stays OFF; no pins, no suite (default
did not flip). `fix-now | deferred(<unblock>) | accepted`: nothing
deferred — the measurement asked by the charter is complete; the arm
itself stays parked behind `EC_AV1_RECTRES`, witnessed exact if a future
cost model ever makes it fire.

## 5. Procedure notes (VPS-2 gates)

Three setup snags hit and solved, for the next lane running gates there:
`systemd-run --user` units start in $HOME, not the caller's cwd — put the
`cd` inside the unit (`--pipe bash -lc 'cd ... && cargo ...'`); the
rsync'd worktree's `.git` FILE (a worktree pointer, not covered by
`--exclude /.git/`) must be `rm -f .git && git init -q .` on the VPS or
the repo's memguard test-runner (`git rev-parse --show-toplevel`) refuses
every test binary; and a failed unit needs
`systemctl --user reset-failed <unit>` before the name is reusable.
`pkill -f` with the unit name in the pattern kills your own ssh session.

## 6. Non-goals respected

Main checkout untouched, never pushed/merged, no repo-wide formatters (a
scoped rustfmt reflow of the two edited files was fully reverted by
restoring HEAD bytes and replaying the semantic edits — the committed diff
is semantic-only, 643+/39-), media files read-only, one gate at a time
(both locally and on VPS-2), logs under ~/.cache/rectres/ and
/home/tCloud/gates/logs/.
