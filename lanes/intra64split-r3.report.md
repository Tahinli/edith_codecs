# lane-intra64split r3 — RED (no witness exists yet; two measurements + one pinned reproducer)

## Verdict
RED. The 64x32/32x64 depth-0 intra strip in an inter frame still has NO firing,
pixel-exact gate. Both routes the charter named were tried and both are closed
by measurement, not by budget. Nothing was weakened to make anything pass; the
r2 gate keeps every arm and every pixel assert.

## What changed
* `crates/ec-av1/src/stream.rs` (`intra_rect64_in_inter_gate` tail) — the firing
  assert `hits[0] > 0 && hits[1] > 0` is replaced by (a) the r3 measurement in a
  comment, (b) `assert!(compared > 0)` so the gate still fails if it stops
  decoding anything, and (c) a TRIPWIRE `assert!(hits == 0)` that fails the
  moment a 64-level strip DOES fire on a pixel-exact stream — so the demotion
  cannot silently outlive its premise. Arms, recipe and pixel asserts untouched.
* `crates/ec-av1/fixtures/inter2f_256x192_8bit_desync.obu` (176 B, md5
  `fd1fd78d79efd8fd5d99c7976f8e3793`, `git add -f`) + `stream.rs`
  `the_pinned_2frame_inter_stream_decodes_pixel_exact`, `#[ignore]`d with a
  dated reason — a minimal reproducer of a SILENTLY WRONG decode.

## MEASURED 1 — the film route: no cut has a comparable frame
Every 2 s `-c:v copy` cut of the 4K 10-bit source refuses inside its FIRST inter
frame (decode-order frame 1), so exactly ONE frame (the key frame) ever
completes, and the key frame never reaches this lane's inter-frame arm:

| start_s | frames completed | refusal reached |
|---|---|---|
| 300 | 1 | a split transform on a 1:4 inter strip with a 64-px axis |
| 1200 | 1 | a split (nonzero tx_depth) transform on an intra HORZ/VERT strip in an inter frame |
| 1800 | 1 | same |
| 3000 | 1 | same |
| 3600 | 1 | same |

At `-ss 300` our counters do report `32x64=2` — but those hits are inside the
frame that then refuses (class `counter-from-refused-stream`), so they are not
gateable. `EC_PROBE_OUT16_PARTIAL` was therefore NOT implemented: the only
completed frame in every cut is the key frame, whose 64-level strips are already
gated elsewhere, so a post-grain partial dump would gate nothing this lane owns.
deferred: EC_PROBE_OUT16_PARTIAL — no completed inter frame to dump — unblocked
by the 1:4-64px-axis and intra-strip-split-transform lanes landing.

EVIDENCE: /tmp/.../i64r3/full{300,1200,1800,3000,3600}.log | ffmpeg -ss N -t 2 -c:v copy -f obu, decode_probe with EC_AV1_FINAL_DUMP, count dumped frames | completed_frames=1 in all 5, refusal string per row

## MEASURED 2 — aomenc emits no 64-level HORZ/VERT even on real film content
r2 measured 0 over ~3300 partition symbols on synthetic sources. r3 repeated it
on the user's own film as the SOURCE for aomenc (crops 256x192 at two scene
offsets and 640x384, plus two-frame scene-CUT pairs built from single frames of
different scenes, which maximise intra-in-inter), cq 20..60, cpu-used 2/3,
`--enable-ab-partitions=0 --enable-1to4-partitions=0 --enable-tx-size-search=0`,
`--limit=2/3/8`: aomdec `EC_TRACE=1` counts `EC_PART_VAL bsize=12 value=[12]`
= **0** in all 30 streams (~3000 partition symbols). Film content does not make
the encoder choose a 64-level rect strip either.

EVIDENCE: /tmp/.../i64r3/{enc,enc2}.sh + the 30 x_*.obu/keep_*.obu | aomenc sweep then aomdec EC_TRACE=1 per stream | `grep -cE 'EC_PART_VAL.*bsize=12 value=[12]'` = 0 for every stream, total_part 24..264

## MEASURED 3 — a 176-byte silently-wrong decode (pinned)
`x_cut34_2_40_2.obu` (two 256x192 8-bit film crops from different scenes, cq 40,
cpu-used 2) decodes `OK: 2 frames` and frame 1 is wrong: 44933/49152 luma and
19293 chroma samples, first bad sample at luma (60, 0), per-SB luma diff counts
`[287, 3689, 4096, 4096] / [4093, 4096, 4096, 4096] / [4096, 4096, 4096, 4096]`
(frame 0 exact, 0 diffs). aomdec's trace for that frame contains NO 64-level
HORZ/VERT at all — its first superblock is `EC_PART_VAL mi_row=0 mi_col=0
bsize=12 value=0`, a plain 64x64 PARTITION_NONE inter block — yet our
`inter_rect_counters` reports two 64x32 intra strips there. Those counts are
phantoms of the desync. Same shape as r2's MEASURED 3 (a 64x64 inter block
wrong on its own 192x128 stream), so this is one inter-path defect class, not
this lane's.

EVIDENCE: /tmp/.../i64r3/{o8.raw,r8.raw} + fixture crates/ec-av1/fixtures/inter2f_256x192_8bit_desync.obu | EC_PROBE_OUT decode vs `ffmpeg -pix_fmt yuv420p -f rawvideo`, aomdec EC_TRACE=1 | frame 0 ndiff 0, frame 1 luma ndiff 44933 chroma 19293, first diff (60,0), reference bsize=12 value=0 at mi(0,0)
EVIDENCE: tool-results/bvsdyy0lc.txt | `cargo test -p ec-av1 --lib the_pinned_2frame_inter_stream -- --ignored` | `test result: FAILED. 0 passed; 1 failed` — RED exactly as the ignore reason states

## Refusals
None lifted, none added. `refusal_inventory.rs` untouched.

## MEASURED 4 — the r2 recipe is vacuous: it decodes ZERO streams
The first full r3 suite run failed both 64-level arms not on a pixel compare but
on `compared == 0`: all 14 attempts (mandelbrot fast zoom, 192x128 and 256x192,
cq 8..55, cpu-used 2..4) stop at FOUR other lanes' refusals before any compare --
"an inter partition below 8x8" (8 seeds), "a nonzero angle delta on an 8x8 intra
leaf in an inter frame" (2), "a non-DC chroma mode on an 8x8 inter-frame leaf"
(2), "an inter 16x16-level AB or 1:4 partition" (2). Adding
`--min-partition-size=8 --enable-ab-partitions=0 --enable-1to4-partitions=0`
changes none of them (30 more streams swept outside the test binary; the same
four refusals rotate). Both arms are therefore `#[ignore]`d with that dated
measurement, the repo's idiom (`lane-oddh r2` did exactly this for the same
situation); every arm, the recipe and every assert are kept, so un-ignoring is
the only step once those refusals lift.

EVIDENCE: $HOME/.cache/intra64split-suite-r3.log:1356-1372 + /tmp/.../i64r3/m_*.obu | full suite, then a 30-stream aomenc/decode_probe sweep with the pruning flags | "compared 0 streams", 14/14 attempts refused, and 0 OK in the 30-stream sweep

## Test totals
Full suite: unit `intra64split-suite-r3-1788331800`, log
`$HOME/.cache/intra64split-suite-r3.log` — `380 passed; 2 failed; 34 ignored`
(the two failures = MEASURED 4). After the `#[ignore]`, re-run as unit
`intra64split-suite-r3b-1788333053`, log `$HOME/.cache/intra64split-suite-r3b.log`
— `test result: ok. 380 passed; 0 failed; 36 ignored; 0 measured` in 921s.
The r2 suite unit (`intra64split-suite-r2-1788331009`) was still running at the
start of this round with 224 `test ...` lines and NO `test result` line; it was
stopped before this one started, so r2 has no totals either.

## Residue
* fix-now (next lane, not this one): the inter-frame desync of MEASURED 3 —
  reproducer is 176 bytes and pinned; start at frame 1 mi(0,0), a 64x64
  PARTITION_NONE inter block, first wrong luma sample (60, 0).
* deferred(another encoder or a hand-built stream): the 64-level intra strip
  witness — aomenc never chooses the partition (2 independent measurements,
  ~6300 symbols), and the films that do contain it cannot be cut to a
  comparable frame until the 1:4-64px-axis and intra-strip-split-transform
  refusals are lifted. A hand-written stream would prove the symbol, not the
  signal (class `fixture-proves-symbol-not-signal`), so it is not a substitute.
* accepted: the demoted assert, guarded by the tripwire described above.
