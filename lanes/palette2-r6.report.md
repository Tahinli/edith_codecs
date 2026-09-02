VERDICT: RED — the broad gate the charter asked for now fires (proof the removed-refusal case
actually occurs), and it caught a real pixel mismatch. The defect is NOT r4's palette wiring: it
traces to a pre-existing, already-tracked bug in `decode_block_rect64` (ledger
[[lane-sbpart-r9]]), newly reachable now that the blanket screen-content refusal is gone. The
refusal is intentionally NOT restored — the charter's own instruction on mismatch is to bisect and
report, not revert.

## What I did
1. Confirmed on `main` (HEAD `8440024`, textually — did not build there, see below) that the
   blanket refusal `"a HORZ/VERT intra strip in a screen-content frame (palette syntax is consumed
   for square blocks only)"` is still present verbatim (`decode.rs:2857`), so this branch really is
   the only place that case can be observed decoding.
2. Added `decode::rect_screen_content_hits()` — a `thread_local` counter incremented at the exact
   site the removed refusal used to guard, inside `read_intra_mode_rect` (`decode.rs`, right after
   `allow_screen_content_tools && palette_bsize_ctx_wh(bw, bh).is_some()`), independent of whether
   the block goes on to use a palette. This is the counter the charter asked for: it fires on the
   *common* case (r5's own sweep found 39/70 attempts reached this branch without ever using a
   palette — those were invisible to r5's `palette_rect_hits`).
3. Wrote `a_real_aomenc_stream_with_rect_screen_content_decodes_pixel_exact` in `stream.rs`,
   modeled on r5's gate: same `--min/max-partition-size=64` recipe (r5 measured live that this
   dodges the 32x32-level AB-partition quirk, [[lane-sbpart-r2]]), same size/cq sweep, drives
   `decode_stream` directly, hard-asserts `rect_screen_content_hits()` moved before comparing any
   pixels.

## What the gate found
First attempt through this recipe that actually fired the counter (`smptebars=size=128x128:rate=25
cq=15`) **mismatched ffmpeg**: rows 0-63 (the frame's top two 64x64 superblocks) match exactly;
rows 64-127 (the bottom two superblocks) are wrong wholesale (~117-128/128 pixels differ per row,
uniformly, not a scattered handful).

I bisected with `EC_AV1_TRACE`:
- The 4 palette-Y/UV blocks visible in the trace (`EC_PAL`/`EC_PAL_VAL`) are all 64x64 luma /
  32x32 chroma — **square**, going through the pre-existing, already-verified square palette path
  (`decode_block`), not the new rect one. Both top SBs decode this way and both match ffmpeg.
- `TRACE_RECT64_END mi_row=16 mi_col=16 bw=32 bh=64` and `mi_row=16 mi_col=24 bw=32 bh=64` are the
  two bottom SBs — real `VERT` splits at the superblock level, going through
  `decode_block_rect64`. Neither shows an `EC_PAL` trace inside it — **this is not a palette block
  at all**, just ordinary intra reconstruction on a genuine rect strip.
- So `rect_screen_content_hits` incremented correctly (the syntax-reading branch it counts ran),
  but the corruption is downstream of it, in `decode_block_rect64`'s own reconstruction of a real
  (non-skip) VERT-split block — the counter did its job; the block it counted is where the bug
  lives, just not in the code r4 added.

This matches an existing ledger dead-end verbatim:
`dead-end|lane-sbpart r9 ... root cause is a sibling function (decode_block_rect64) ... do not
re-attempt part32 AB until decode_block_rect64 real-residual TX_32X64/64X32 truncation is fixed and
gated`. That defect was never closed. What's new this round is that it was previously
UNREACHABLE under `allow_screen_content_tools=true` (the blanket refusal short-circuited it
first, [[refusal-short-circuits-its-own-code]]) — removing that refusal exposed it for the first
time in this configuration, on a stream that doesn't even use palette.

## Why I did not restore the refusal
The charter is explicit: on mismatch, bisect and say so — restoring the refusal was not asked for
and this branch's whole point is closing "refusal removed, no gate behind it"
([[refusal-lifted-without-a-gate]]) with an honest RED, not a silent revert back to green. The gate
itself is the safety net now: it is red, hard-asserting, and will stay red until
`decode_block_rect64`'s real-residual defect is actually fixed — exactly the state the charter
wants landed.

## r4's `refusal_inventory.rs` / palette reconstruction — still unverified
Nothing this round exercised palette reconstruction through the rect path (every genuine rect-strip
decode I observed had zero palette use; every palette use I observed was square). r4's actual new
code (palette-Y/UV symbol reading and reconstruction generalized to `bw`x`bh`) is therefore
*still* neither confirmed nor refuted — same state r5 left it in. I left `refusal_inventory.rs`
untouched.

## What I did NOT do
- Did not build or run anything in `/home/tahinli/Documents/Code/Rust/edith_codecs` (main repo,
  not a worktree) — it had another agent's uncommitted staged changes when I checked; I only read
  the committed `HEAD` text to confirm the refusal's wording, never compiled there.
- Did not attempt to fix `decode_block_rect64`'s residual truncation — out of this round's charter
  (gate r4's work), and the ledger already flags it as open for whichever lane owns `sbpart`/rect64
  residuals next.
- Did not touch r5's `a_real_aomenc_stream_with_rect_palette_decodes_pixel_exact` gate; it is
  unchanged and still red for its own reason (never observes a genuine palette-on-rect attempt).

## Commands run
```
export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette2
export EC_AV1_REQUIRE_AOMENC=1
nice -n 19 cargo build -p ec-av1 --tests -j4                    # clean
nice -n 19 cargo test -p ec-av1 --lib -j4 \
  a_real_aomenc_stream_with_rect_screen_content_decodes_pixel_exact -- --nocapture
  # FAILED: assertion `left == right` ... frame 0 luma vs ffmpeg (the mismatch above)
```
(cargo test timeout was 600000ms; actual run finished in ~1-17s per invocation, well under)

## State committed
`d427e19` on `lane-palette2` (HEAD): `decode.rs` (+23, new counter),
`stream.rs` (+153, new gate). Compiles clean (`cargo build -p ec-av1 --tests`). Two red gates now
exist for this lane's unverified surface (r5's palette-on-rect, r6's broad rect-screen-content) —
both honest, neither silenced.

## Next round should
- Fix `decode_block_rect64`'s real-residual VERT/HORZ-split reconstruction (the actual root cause,
  tracked since [[lane-sbpart-r9]]) — this gate will go green once that's fixed and no sooner.
- Once green, re-run `a_real_aomenc_stream_with_rect_palette_decodes_pixel_exact` too: with
  `decode_block_rect64` fixed, more attempts may reach a genuine palette-on-rect block and finally
  give r4's actual new code its first real test.

## Budget
~55 turns spent: charter/report reading, main-HEAD text confirmation, counter + gate authoring,
two build/test cycles (first hit a broken-pipe harness bug from `--limit=3` racing piped stdin,
fixed by dropping it and matching r5's proven recipe), then bisection via `EC_AV1_TRACE` to
localize the mismatch to `decode_block_rect64`. Stopped there per budget discipline and committed.
