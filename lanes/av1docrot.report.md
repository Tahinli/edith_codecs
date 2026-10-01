# lane/av1docrot — four comments that claimed more than their own evidence

Base: `main` = `1f4fcc84`. Comments only: no behaviour change, plus the deletion
of one `#[allow(dead_code)]` function with no callers. Scoped proof:
`cargo check -p ec-av1 --tests` clean; `cargo doc -p ec-av1 --no-deps` produces no
new broken-link warning; `cargo test -p ec-av1 --lib -- --test-threads=1 dump census 422`
green.

## Row 1 — `decode.rs` `census_unwritten` doc: coverage claim

**Was:** "at each frame's pre-deblock point … an EXACT census", stated as though
every frame is scanned.

**Now:** the coverage is named and bounded. The census runs at **two of the four**
places `apply_deblock` is entered from — the key-frame path and the
non-pipelined inter path, each immediately before that frame's deblock. The two
it does not cover are named as well: the encoder's filter search
(`replay_filters`, which runs the filters over a *captured copy*, not a live
decode) and the pipelined inter branch, where deblock has already run band by
band inside `par` before this tail is reached. The comment says a zero count
means "no hole on the paths that reach this call", not "no hole on any path".

The **exactness** claim is kept, and narrowed to what is true of the scan: exact
over the extent it scans — every sample of `true_width.min(p.width)` x
`true_height`, per axis. The claim that this differs from the older
`dump_stage`/`dump_stage16` is stated per axis (`true_width.min(p.width)` and
`round_ss`), which is what the code at the scan does.

**Evidence:** the call sites themselves — `census_unwritten` has exactly two
callers in the crate (key path, inter path), and `apply_deblock` has four
entries (`replay_filters`, key path, and the inter path's `pipelined` /
non-pipelined arms, where the pipelined arm calls `cdef_scan_counters` instead
and deblock has run inside `par` via `deblock_span`).

## Row 2 — `decode.rs` `stage_dump_idx`: dead function deleted

**Decision: deleted**, rather than repointing the doc. `stage_dump_idx()` was
`#[allow(dead_code)]` with zero callers in the crate; its doc was a duplicate
description of the mechanism that is actually live.

The doc it carried had two parts, and both now live where they are true:

* the lane-hgkf / lane-unwritten-dep byte-shape and per-axis crop description
  stays on `dump_stage16`, which is what it describes, unchanged;
* the lane-mc64 r1 "decode-order index" paragraph is rewritten to name the live
  mechanism — [`dump_stage_idx`], the per-var decode-order counter that
  `dump_stage`/`dump_stage16`/`dump_prefilter_wide` name their files from.

The rewritten paragraph also records *why* the deleted accessor could not have
been the mechanism: `PREFILT_PICTURE_IDX` is bumped **after** the dumps run (the
dump sites `fetch_add` it immediately below the `census_unwritten` call), so a
`load() - 1` accessor would have named the previous frame. The live per-var map
is what indexes the files.

**Evidence:** `grep stage_dump_idx` over the whole repo → only its own
definition, before this change. `dump_stage_idx` call sites: `dump_stage16`,
`dump_stage`, `dump_prefilter_wide`.

## Row 3 — `stream.rs` `EC_AV1_DECODE_ORDER_DUMP` comment: dangling oracle rung

**Was:** "diff this byte-for-byte against `EC_AV1_POSTFILT_DUMP.fN` from the
instrumented aomdec build". No such rung: `scripts/instrument-aom-oracle.sh`
defines `PREFILT` (1), `PREFILT_WIDE` (7), `POSTDEBLOCK` (6), `POSTCDEF` (15),
`FINAL` (12) — no `POSTFILT`.

**Now:** names `EC_AV1_POSTDEBLOCK_DUMP.fN` (rung 6), the post-deblock rung that
matches what this dump holds, with the rung's own depth caveat stated (1 byte per
sample, so byte-comparable only at 8-bit; rung 12 `EC_AV1_FINAL_DUMP` for HBD).
The rung number is cited so a future bisect can find it, and the comment 16 lines
below naming `EC_AV1_FINAL_DUMP` (rung 12) was already correct and is untouched.

**Evidence:** `scripts/instrument-aom-oracle.sh` header (rungs 1/6/7/12/15) and
the rung-6 block at :328-338, whose `DEPTH ASSUMPTION` block states the 1-byte
per-sample pairing with our own `EC_AV1_POSTDEBLOCK_DUMP`.

## Row 4 — `decode.rs` `chroma_plane_block_codable`: the "what led here" history

The historical measurement is **kept**, with three corrections layered on:

1. **The tree it was taken on.** `8d6998d7` is now named as lane/av1unwritten's
   branch point, not a bare sha that reads like `main`.
2. **The non-reproduction.** The comment now states plainly that the hash half did
   not reproduce: five plain runs on the same `8d6998d7` gave **one** hash,
   `a761118c8dd5c8d0` — citing `lanes/unwritten-dep.report.md` §10 (and §13 for
   the census's still-absent committed reader). The spread is presented as
   measured on that tree, not as a standing property, with the machine-dependence
   caveat the report itself makes.
3. **Which witness behaviour is current.** `main` **refuses** the witness. The
   comment names the test that pins that
   (`stream::tests::a_422_header_over_a_420_tile_refuses_the_subsize_libaom_calls_corrupt`,
   which asserts the decode errors with libaom's "invalid with this subsampling
   mode" wording) and says the run-to-run hashes are what the tree did *before*
   that refusal and cannot be re-measured on `main`.

The `0xAD`-not-`DE AD` narrowing detail and the 224-sample census count are
unchanged; the census count keeps its "lane-tree measurement with a lane-only
instrument" qualifier. The closing line is retargeted: facts (1) libaom refusing
the shape and (2) libaom's own encoder never emitting it are what make the
refusal correct — the measurement history is only why it was looked for.

**Evidence:** `lanes/unwritten-dep.report.md` §10 (both the `440_request_is_422`
= REFUSED line and the `a761118c8dd5c8d0` non-reproduction) and §13 (no committed
reader for `take_unwritten_samples` on `main`); the refusal test named above.

## What this lane did not touch

`decode.rs`'s allocation path (`fresh_plane`) — peer lane `av1uballoc` owns it.
Every hunk here is a doc comment, except the row-2 deletion at old
`decode.rs:21144-21149`.

## Files

* `crates/ec-av1/src/decode.rs` — rows 1, 2, 4 (comments; one dead-fn deletion)
* `crates/ec-av1/src/stream.rs` — row 3 (comment)