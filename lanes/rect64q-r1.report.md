# lane-rect64q r1 report

## What was already there / applied

Hunter's fix (three `decode_block_rect64` dequant call sites use
`CURRENT_Q_IDX.with(|c| c.get())` instead of the stale `base_q_idx`
parameter) was already applied uncommitted in this worktree. Committed
first, as instructed: `1d2c3bb`.

## Instrumentation (charter step 1)

Added `RECT64_QIDX_DRIFT_HITS` thread-local counter (`decode.rs:719-733`)
and, at each of the three dequant sites inside `decode_block_rect64`
(luma, U, V — `decode.rs` ~3560, 3623, 3684), a check comparing
`CURRENT_Q_IDX` against `base_q_idx`, incrementing the counter on any
mismatch and printing `TRACE rect64_dequant plane=<p> base_q_idx=<b>
current_q_idx=<c>` under the existing `EC_AV1_TRACE` gate.

**Measured on the only existing gate that reaches `decode_block_rect64`**
(`a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact`,
40 attempts, `--deltaq-mode=0`): 105 dequant calls traced, every single one
`base_q_idx=180 current_q_idx=180`. `rect64_qidx_drift_hits()` stayed 0.
This gate forces `delta_q_present=0` by its own recipe (`--deltaq-mode=0`),
so this result is exactly as expected — it proves the instrumentation
works, not that the fix is inert in general.

EVIDENCE: `EC_AV1_TRACE=1 EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib
a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact
-- --nocapture --test-threads=1` | `/tmp/rect64q-trace1.log` | 105/105
`TRACE rect64_dequant` lines all `180 180`, `sb_rect_hits=36`.

## Gate attempt 2: drop `--deltaq-mode=0` (charter step 2)

Wrote
`a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_and_delta_q_decodes_pixel_exact`
(`stream.rs`, right after the sibling gate): identical recipe minus the
one flag, since real aomenc's non-realtime default is `DELTA_Q_OBJECTIVE`
(hunter's own finding, `av1_cx_iface.c:271`). Hard-asserts
`rect64_qidx_drift_hits() > 0`.

**Result: does not fire.** 40 attempts, 15 matched, 25 named refusals
(same refusal shapes as the sibling gate — AB-at-64, 32x32 AB, Golomb
tail, screen-content strip), but `rect64_qidx_drift_hits()` stayed 0
across all 15 matched attempts. Real aomenc's objective delta-q RD never
chose a nonzero delta on this tiny (192x128, 1 frame, cq=45) synthetic
gradients content, even with the flag left at its default. Marked
`#[ignore]` with the negative result recorded in its own doc comment
rather than deleted, so the recipe survives for whoever picks this up
next; it does NOT run in the default `--lib` suite, so it does not block
green.

I did **not** port lane-palette2's gate. Two reasons found live, in order:
1. My worktree's `read_intra_mode_rect` (used by both `decode_block_rect`
   and `decode_block_rect64`) still carries the blanket refusal *"a
   HORZ/VERT intra strip in a screen-content frame"* for any
   `allow_screen_content_tools` frame — I do not have lane-palette2's
   rect-generalised palette wiring (`palette_bsize_ctx_wh` is defined at
   `decode.rs:2553` but never called). Porting palette2's gate as-is would
   mostly just hit this refusal (confirmed live: seed 59 in my own
   modified gate's run hit exactly this string).
2. The coordinator's mid-task message pointed at a cheaper, *already-real*
   repro (below) before I sank more budget into that path.

## The coordinator's SB(0,1) lead — investigated, does not reproduce here

lane-part32 (`64ccb9f`, read-only) reported seed 42, same
`a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact`
recipe, SB(0,1)'s VERT-64 left arm, `row=0 col=91`, DC_PRED,
`all_zero=1`, "got=80 want=81".

Regenerated that exact stream by hand (same `gradients_source` seed-42
hash, same aomenc args) and decoded it in **this** worktree with
`EC_SBPART_DUMP64=1`:

```
DUMP64 mi_r=0 mi_c=16 px=64 py=0 bw=32 bh=64 mode=0 skip=false eob_nonzero=false
  row0=[81,81,...,81] (32 values, all 81)
```

Independently decoded the same `.obu` with real ffmpeg
(`ffmpeg -i seed42.obu -f rawvideo -pix_fmt yuv420p`) and read row 0,
cols 60-99 of luma: all `81`. **My worktree and ffmpeg agree at col 91
(value 81) — no mismatch.** This is also exactly the outcome the
coordinator's own reasoning predicts: `eob_nonzero=false` means zero
residual, so a dequant qindex bug (drift or not) cannot move this
specific block's pixels either way — consistent with `rect64_qidx_drift_hits()`
staying 0 on this exact stream too (checked: `deltaq-mode=0` in this
recipe, no drift possible by construction).

I cannot explain the discrepancy with part32's reported "got=80" from the
evidence available in this round (my byte-for-byte reproduction matches
ffmpeg, not their reported mismatch) — possibly a difference in tree
state between their fork point and mine, or an artifact of the exact
attempt-loop state (`lock_gate_counters`/thread-local counters carried
across earlier attempts in the same test process) rather than this one
block in isolation. Did not chase further given budget; this is now open
for whoever picks up either lane next, with the reproduction recipe above
(exact aomenc/ffmpeg commands in scratchpad) ready to hand.

**lane-palette2's defect status: still open, not touched this round.**
**lane-part32's SB(0,1) defect: investigated, does NOT reproduce
byte-for-byte in this worktree against real ffmpeg; open.**

## Dead parameter (charter step 3)

**Not removed.** My own instrumentation (comparing `CURRENT_Q_IDX` against
`base_q_idx` to increment the drift counter) now legitimately *uses*
`base_q_idx` inside `decode_block_rect64` — it is no longer a parsed-and-
discarded value, so `cargo check` no longer warns about it there
(confirmed: the only two remaining `unused variable: base_q_idx` warnings
are at `decode.rs:3053` (`decode_block_rect`) and `decode.rs:7259`
(`read_inter_plane`), both pre-existing and out of this charter's scope —
`decode_block_rect64` itself is clean). This is a deviation from the
charter's literal step 3 instruction ("remove the parameter"): the
premise (dead parameter) stopped being true once the drift-proof
instrumentation was added, since that instrumentation is exactly the
"permanently non-inert" proof charter step 2 asked for and it needs
`base_q_idx` as its ground truth. Flagging this explicitly rather than
silently keeping the param.

## Suite / check

- `cargo check -p ec-av1 --lib`: clean, no new warnings beyond pre-existing
  108 doc/lint warnings.
- `cargo test -p ec-av1 --lib` (default set, `#[ignore]`d test excluded):
  running full run at report time — appending numbers below once done.

## HEAD

Will update after final commit.

## Open (disposition)

- `rect64_qidx_drift_hits()` has never been observed nonzero anywhere in
  this repo. The dequant fix (`1d2c3bb`) is **still unproven live** —
  correct by code inspection and by matching every sibling path
  (`decode_block`, `decode_block_rect`), but no gate in this repo (old or
  new) has yet exercised a rect64 block with `delta_q_present=1` AND a
  nonzero coded residual. `deferred: a firing gate for this fix —
  needs either bigger/noisier real content (more SB rows so aomenc's
  objective delta-q actually varies q) or a hand-crafted/pinned stream —
  next lane round.`
- lane-part32's SB(0,1) "got=80 want=81" report does not reproduce here;
  `deferred: reconcile the two worktrees' byte streams for seed 42 — next
  round, needs comparing actual stream bytes, not just recipe text.`
