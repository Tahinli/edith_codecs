# lane-sbpart — superblock- and 32x32-level partition types

Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-sbpart`, branch
`lane-sbpart`, off main 53f5358.

## Why this lane exists — measured, not guessed
Five default-settings aomenc streams were run through
`cargo run -p ec-av1 --example decode_probe -- <stream.obu>` on 2026-08-30.
Where each stopped:
- testsrc2 192x128 `--cpu-used=4` -> palette (Y)            [lane-palette]
- testsrc2 192x128 `--cpu-used=0` -> **a superblock-level partition type**
- smptebars 320x240 `--cpu-used=4` -> delta_q/delta_lf      [lane-realworld]
- flat gray 128x128 `--cpu-used=4` -> **an inter superblock-level partition**
- mandelbrot 256x192 `--cpu-used=4` -> delta_q/delta_lf     [lane-realworld]

Two of the five are yours, and they are the same family. Note that these
refusals used to claim "a partition type this encoder never writes" — the
measurement above disproved that, and main reworded them (2be815f).

## The four refusals you own, verbatim
- `"a superblock-level partition type other than NONE or SPLIT (this decoder's
   intra tile path codes only those two at 64x64)"` (decode.rs, intra tile path)
- `"an inter SB-level partition type other than SPLIT (this decoder's inter tile
   path only recurses a superblock as SPLIT)"` (decode.rs, inter tile path)
- `"a 32x32 partition type this decoder does not code (value={part32})"`
- `"an INTER 32x32 partition type this decoder does not code (value={part32})"`

## What is already there
The decoder already codes rectangular partitions BELOW the superblock level —
`decode_block_rect`, HORZ/VERT strips, HORZ_A/VERT_A/VERT_B arms, and
rectangular inverse transforms for all 14 sizes are landed and gated. What is
missing is the same alphabet at the 64x64 and 32x32 levels, on both the intra
and inter tile paths. Read how the existing sub-16x16 arms do it before writing
anything; this is mostly extending an existing recursion, not new theory.

Spec 5.11.4 `decode_partition`, and libaom `av1/decoder/decodeframe.c`
`decode_partition` — which is one function handling every level, so the
structure you want is visible there in one place.

## Staging — COMMIT AFTER EVERY GREEN STEP
1. Intra path, 64x64: HORZ and VERT. Gate with a real
   `--cpu-used=0` stream (that is the recipe that produces them). COMMIT.
2. Intra path, 32x32: the same, plus whatever `part32` values the gate turns up
   — the refusal prints the value, so run it and read it. COMMIT.
3. Inter path, both levels. COMMIT.
4. HORZ_A / HORZ_B / VERT_A / VERT_B at these levels, then HORZ_4 / VERT_4.
   COMMIT each.

## Gate rules
`EC_AV1_REQUIRE_AOMENC=1` on every run (a missing oracle must FAIL, not SKIP);
`-t <seconds>` on every ffmpeg generate; fixtures through the existing
`gradients_source(seed, w, h, tail)` helper (ffmpeg's `gradients` ignores its own
seed); aomenc `--threads=1 --row-mt=0 --sb-size=64` (this decoder hardcodes 64px
superblocks; aomenc's default 128 lands in a dead-ended gap). Firing counts are
HARD asserts via thread-local `Cell<usize>` counters like the existing `*_HITS`
(thread-local, NOT atomics). Main FAILS the suite if a gate turns a decode error
into a printed SKIP — write the gate as an attempt loop that requires at least
one decode. A gate that cannot prove its partition type fired is vacuous.

## Method
CLASS `compare-range-not-tell`: compare the msac RANGE against the oracle after
each element, never `tell()`. Oracle at `~/.cache/aom-oracle`, rungs `EC_TRACE=1`
(partitions — exactly your subject), `EC_TRACE_COEFF=1`, `EC_TRACE_MODE=1`,
`EC_AV1_PREFILT_DUMP`, `EC_AV1_POSTDEBLOCK_DUMP`, `EC_AV1_PREFILT_WIDE_DUMP`,
`EC_TRACE_PALETTE=1`. Add rungs via `scripts/instrument-aom-oracle.sh` +
`scripts/build-aom-oracle.sh` in the existing shape if you need one.
CLASS `equal-range-means-unread`: reference range unchanged where ours moves =
we read a symbol it never wrote; theirs moves and ours does not = we skipped one.

## Hard rules
`export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbpart`; foreground builds
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms (the suite
runs ~3 min). Sibling worktrees (edith_codecs, -realworld, -lr, -superres,
-tiles, -palette) have live agents — never build in or edit them, and do not
create additional worktrees. Baseline 243 passed / 0 failed. Never push, never
merge, never touch `main`. Refuse-by-name rather than desync, and never write a
refusal claiming an encoder cannot emit a case — this lane exists because three
such claims were false. 75-turn cap, does not reset: COMMIT AT EVERY GREEN STEP,
and do wide signature changes as ONE scripted sweep plus `cargo check`, not N
hand edits — three lanes have hit the cap mid-sweep. Report every refusal string
you remove, verbatim, for `refusal_inventory.rs`. End with
`lanes/sbpart.report.md`, VERDICT on line 1.
