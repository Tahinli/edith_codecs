# lane/unwritten-dep — the hg_* rows are not decoder defects; the instrument that lied is `dump_stage16`

**Outcome in one line: neither `hg_rect64_intra16x4_witness.obu` nor
`hg_arf_witness.obu` output depends on unwritten plane content; the census was
never blind — `dump_stage16` was, because it hardcoded a 4:2:0 chroma extent and
on a 4:2:2 frame wrote half of each chroma plane into no dump at all, which
fabricated the "identical pre-deblock, differs post-deblock" attribution; with
per-axis extents the 4:2:2 witness shows 224 unwritten samples pre-deblock
(exactly lane/av1unwritten's own number), so the difference is present BEFORE any
filter runs. The gate's non-vacuity control is now synthetic and survives
`main`'s refusal of the only fixture that had one.**

Branch `lane/unwritten-dep`. Base `8d6998d7`, **rebased by merging
`main` = `e45cc748` locally** (commit `c62eeda9`) — the merge proof the
acceptance asked for is this lane's own tree; no rebase onto a different base,
the merge keeps `8d6998d7` as the branch point.

> **r3 correction, on top of r2.** r2 named the census as the blind instrument and
> attributed the 4:2:2 witness's divergence to deblock. Both were wrong, and r2's
> own numbers refuted them: its `EC_AV1_DEBUG_SKIP_DEBLOCK` result did not remove
> the difference, and 32 surviving sentinel samples cannot explain 1986
> differing bytes. The cause is r2's own unexamined instrument.
> `dump_stage16` (`decode.rs`) cropped chroma as `(fw/2, fh/2)` — a 4:2:0
> assumption — so on a 64×64 **4:2:2** frame (ss_x=1, ss_y=0) it wrote 32×32
> chroma where the frame carries 32×64. **2048 chroma samples appeared in no
> stage dump at all**, and they are exactly the region where the difference
> lives. r2 read the dump's blind spot as a pipeline property. With per-axis
> extents the pre-deblock dump is where the difference *starts*.

## 1. Provenance

Built in this lane's worktree with its own target dir; the binary's embedded
paths name only this worktree.

```
$ CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/uwdep-merge cargo build -p ec-av1 --example decode_probe
$ strings .../decode_probe | grep -o 'wt/[a-zA-Z0-9_-]*' | sort -u
wt/unwrittendep
```

Two live traps, both of which produce a *silent* wrong reading: this lane's first
target dir was deleted underneath it mid-run (hence `uwdep-k9` → `uwdep-merge`),
and `/tmp` is a 16 GB tmpfs that filled during the first sweep and produced
short/zero-length dump files. Every retained run asserts the exact expected
frame size; the 10-bit fixtures' frame is 18 524 160 bytes.

Fixtures: `hg_arf_witness.obu` 32 126 B sha256 `7f3b060da5aa9c53…`,
`hg_rect64_intra16x4_witness.obu` 23 472 B sha256 `c9e721088766163b…`.

## 2. Reproduction

Hash shape `cat dir/frame.f*` (lexicographic), 3 plain + 2 sentinel runs:

| fixture | plain | sentinel | campaign plain | campaign sentinel |
|---|---|---|---|---|
| `hg_rect64_intra16x4_witness` (34 frames) | `a26438168c` ×3 | `a26438168c` ×2 | `a26438168c` | `0ee1edf403` |
| `hg_arf_witness` (40 frames) | `8d7a43bc5f` ×3 | `8d7a43bc5f` ×2 | `8d7a43bc5f` | `97ef82c0ce` |

**Both campaign PLAIN values are reproduced exactly by this build; neither
SENTINEL value is.** The decode output is byte-identical plain vs sentinel, and
under every other ambient content available: glibc `MALLOC_PERTURB_` ∈ {0, 1, 85,
170, 254}, `EC_AV1_THREADS` ∈ {1, 2, 4, 8}, and in-process poisoning of the
allocator's free list with 16 MiB dirty-then-freed blocks.

The in-process test is the strongest of these because there is no filesystem in
the measurement at all, and it has teeth — on the 4:2:2 witness it reports
differences under 4 of 4 poison patterns (667–715 differing U samples,
673–690 differing V, 0 in luma), while reporting **0 differing frames under 4
of 4 patterns on both hg fixtures**.

## 3. `97ef82c0ce` — consistent with a truncated dump, not proven to be one

Sweeping (frames-complete, tail-truncated) shapes against the campaign's value:

```
$ python3 /tmp/unwritten/partial.py hg_arf_witness
  MATCH: frames 0..4 + f5 truncated to 13320192 bytes, lex-hashed
```

The campaign's value is reproduced **bit-for-bit** by a byte stream that is frames
0–4 complete plus 13 320 192 bytes of frame 5 (71.8% of one frame), frames 6–39
absent. `EC_AV1_FINAL_DUMP` does `let _ = f.write_all(&buf)`, so a full filesystem
leaves a prefix of the true stream and nothing reports it.

**This pins the byte content and the shape, not the mechanism.** An unlived
sentinel, a timeout-killed process, and a full filesystem all leave the same
bytes. The wording is therefore *consistent with* a truncated dump, and the
converse — that the decode is innocent — rests on §2's invariance, not on this
match. Corroborating but likewise non-conclusive: `RLIMIT_FSIZE` at a 4 MiB cap
gives `788d3be08ae5112e` for both fills vs `5350cd5569d226eb` uncapped; a reused
dump directory leaves a longer fixture's tail (`9ca5c4d7af`); hashing the
`sha256sum` listing embeds the directory path.

`0ee1edf403` is **not** reproduced by any shape swept (every numeric prefix;
every prefix + page-truncated tail at 4 KiB and 512 B; prefixes from either
fixture, since rect64 is a 34-frame prefix of arf's 40 with byte-identical frames
0–33; hole shapes; zero-length-frame shapes; stale cross-fixture mixes both
orders; the listing shape). It remains an **open instrument-hygiene question**,
not a decode question — the decode is invariant (§2).

## 4. The instrument defect, and the corrected measurement

**Class: `diagnostic-dump-assumes-one-subsampling`** — a debug/instrumental dump
that derives a plane's extent from an assumed subsampling rather than the
sequence's, so part of the plane is absent from the measurement and conclusions
drawn from that measurement are about the dump. Same per-axis family as the
decoder's own `round_ss(dim, ss)` rule.

`decode.rs`, `dump_stage16`, cropped chroma as `(fw.div_ceil(2), fh.div_ceil(2))`
— the 4:2:0 assumption on both axes. The oracle's rung 1 uses libaom's
`uv_crop_width` / `uv_crop_height`, which are per-axis. Fixed to
`(round_ss(fw, ss_x), round_ss(fh, ss_y))`, with all six call sites passing
`ss_x(fctx)` / `ss_y(fctx)`.

**Corrected stage bisect on `440_request_is_422`** (8-bit, 64×64, 4:2:2 header
over a 4:2:0 tile; `main` = `e45cc748` refuses it, so this is measured on the
branch point `8d6998d7`):

| stage dump | before the fix | after the fix |
|---|---|---|
| `EC_AV1_PREFILT_DUMP16` (pre-deblock) | 12 288 B, **IDENTICAL** | 16 384 B, **DIFFERS (2158 B)** |
| `EC_AV1_POSTDEBLOCK_DUMP16` | 12 288 B, DIFFERS (26 B) | 16 384 B, DIFFERS (2017 B) |
| `EC_AV1_POSTCDEF_DUMP16` | 12 288 B, DIFFERS (26 B) | 16 384 B, DIFFERS (1948 B) |
| `EC_AV1_DECODE_ORDER_DUMP` | 8 192 B, DIFFERS (1985 B) | 8 192 B, DIFFERS (1917 B) |
| `EC_AV1_FINAL_DUMP` | 8 192 B, DIFFERS (1986 B) | 8 192 B, DIFFERS (1986 B) |

12 288 B = 6144 samples = 4096 luma + 2×1024: only chroma rows 0–31. The frame the
caller receives is 8192 samples. **The 2048 missing chroma samples are the
region r2's conclusion was drawn from.**

**Coordinates, from the corrected pre-deblock dump** (8192 samples, u16, whole
plane, `0xDEAD` counted):

* unwritten samples, sentinel run: **U 112, V 112, Y 0** — U and V, 28 whole
  4×4 groups each, spanning rows 36–63 (12 rows carrying 8 samples + 4 rows
  carrying 4 = 96 + 16 = 112). The column span runs 0–15 but is NOT filled:
  every row has cols 0–3 and 12–15. **224 total, exactly lane/av1unwritten's own
  census number** (`lanes/av1unwritten.report.md:152-156`).
* plain vs sentinel differing samples, pre-deblock: **U 1004 of 2048, V 1004 of
  2048, Y 0**, rows 32–63 — the hole plus everything predicted from it.

So the corrected conclusion is the opposite of r2's: the difference is **already
present before deblock**; no filter creates it. r2's "Deblock creates it" was an
artefact of the dump it could not see past.

**And the census was never blind.** It runs at pre-deblock and it reports the
hole — 224 samples, in whole 4×4 chroma blocks. What could not see it was the
stage dump. On the hg fixtures both report 0.

## 5. Displayed vs hidden, per fixture

`hg_arf_witness`: 40 decode-order frames — 19 shown, 21 hidden alt-refs, plus 18
`show_existing_frame` headers. `hg_rect64_intra16x4_witness`: 34 — 17 shown, 17
hidden, plus 16 `show_existing_frame`. (From `EC_PROBE_HDR=1`; the split is over
non-`show_existing` headers.)

**No unwritten sample reaches a DISPLAYED frame on either fixture — or a hidden
one.** Every one of the 34/40 frames hashes identically under plain, sentinel,
five ambient fills, four thread counts and in-process poisoning, and both the
pre-deblock census and the new output-point census report 0. The hidden frames
were measured specifically because they are the route by which a hole would reach
a later displayed frame through the reference bank; they are clean.

The one fixture where a hole does reach output is the 4:2:2 witness, whose single
frame is **SHOWN** (`Key show=true`), with 224 unwritten chroma samples. Its
cause is the invalid subsize `main` now refuses; that lane owns it and is
untouched here.

## 6. The fix and the gate

`decode.rs`: `census_unwritten_final` + `take_final_unwritten_samples` — a census
over the three planes at the point they are stored into the reference slots, with
`dump_stage16`'s chroma extent made per-axis.

`stream.rs`: the census is called beside the `EC_AV1_FINAL_DUMP` site.

**SCOPE, stated not implied:** that point is **before `apply_grain`**, so the
claim is over the **pre-grain** planes only. Those are also the bytes the
reference bank stores and every later frame predicts from, which is the
dependency that matters. Whether `apply_grain` can itself introduce a hole is
**not measured here** -- the census never sees the planes `apply_grain`
allocates -- so "the caller's final grained picture has no hole" is **not
claimed**, and no sentence asserts that grain cannot introduce one.

Gate `the_frame_the_caller_receives_carries_no_unwritten_plane_sample`:

1. **non-vacuity first, and SYNTHETIC**: the census is handed planes carrying
   exactly 7 sentinel samples and must report exactly 7. No fixture is involved,
   so it cannot be affected by `main` refusing `440_request_is_422` — which is
   what r2's fixture-carried control would have done on merge;
2. **0** on `hg_arf_witness`, `hg_rect64_intra16x4_witness`, `gm_small_side_witness`,
   `troy_sb128_inter_witness` — the real 10-bit streams with hidden alt-refs.

The fill is env-gated and this crate forbids `set_var`, so the measurement runs
in a child process the parent invokes by name; the child asserts
`EC_AV1_PLANE_SENTINEL` is set, so a non-live fill fails rather than reporting a
meaningless zero.

**Merge proof — green on a tree containing both lanes.** `main` = `e45cc748`
merged into this branch (`c62eeda9`), then:

```
$ cargo test -p ec-av1 --lib -- the_frame_the_caller_receives_carries_no_unwritten_plane_sample
test result: ok. 1 passed; 0 failed; 0 ignored; 816 filtered out; finished in 71.59s
```

The proof that this tree really carries `av1unwritten`'s content is behavioural,
not a test count (a filtered-test count is a count of names, and a merge that
dropped hunks would print the same number):

```
$ decode_probe crates/ec-av1/fixtures/440_request_is_422.obu
REFUSED: unsupported: AV1 tile (a block size 4x8, 8x16 or 16x4 (or 8x4 at 4:4:0) has no
chroma plane block at this frame's subsampling mode ...
```

**0 frames, refused** — only reachable with the refusal present. On the branch
point `8d6998d7` the same fixture decodes to `OK: 1 frames decoded`. That is the
merge proof. (It is also why the fixture-carried control had to go: the census
child could not run at all here.)

### Mutations

| mutation | result |
|---|---|
| `census_unwritten_final` neutered (`if true { return; }`) | **RED** at the synthetic control: "the census reported 0 for planes carrying exactly 7 sentinel samples … (class: census-silently-blind)" |
| **the call at `stream.rs` DELETED** (the whole line replaced by a `let _ =`) | **RED**: "the census scanned 0 samples on hg_arf_witness -- the call at the output point is not running, so the 0 below proves nothing (class: census-never-ran). Deleting the call site reproduces this." |
| one sentinel sample forced into a film frame's output (`p.y[0] = 0xDEAD`) | RED on the fixture: "hg_arf_witness hands the caller N samples the tile walk never wrote" |

The second row was **false as stated in r3** and is the substantive fix here: r3's
control called `census_unwritten_final` directly, so deleting the pipeline call
left every assertion passing and the gate pinned nothing about the decode. The
gate now reads `take_final_census_scanned()` alongside
`take_final_unwritten_samples()` — the same pairing the merged pre-deblock census
already documents ("a gate that only asserts `take_unwritten_samples() == 0`
passes on a decode that never scanned anything, so it reads THIS too"). The
fixture arms require `scanned > 0`, the synthetic arm requires `scanned == 40`
(16 + 16 + 8), so "scanned and found nothing" is now distinguishable from
"never scanned".

## 7. Commands

```bash
cd /home/tahinli/.cache/wt/unwrittendep
git merge e45cc748                       # merge proof tree
CARGO_TARGET_DIR=... cargo build -p ec-av1 --example decode_probe
strings .../decode_probe | grep -o 'wt/[a-z]*'          # provenance

python3 /tmp/unwritten/coords.py         # corrected coordinates (§4)
bash    /tmp/unwritten/determinism.sh "440_request_is_422 hg_rect64_intra16x4_witness hg_arf_witness" 5
python3 /tmp/unwritten/partial.py hg_arf_witness       # the 97ef82c0ce shape
cargo test -p ec-av1 --lib -- the_frame_the_caller_receives_carries_no_unwritten_plane_sample
```

## 8. Disposition

* **`hg_arf_witness` — closed, not a decoder defect.** Output invariant under
  every ambient content tested (§2); the campaign's divergence is *consistent
  with* a truncated dump (§3), and the decode is innocent on the invariance
  evidence, not on the hash match.
* **`hg_rect64_intra16x4_witness` — closed, not a decoder defect; its campaign
  sentinel value `0ee1edf403` remains an OPEN instrument-hygiene question** (§3).
* **`440_request_is_422` — measured and gated, not fixed here.** 224 unwritten
  chroma samples, pre-deblock, whole 4×4 blocks; cause is the invalid subsize
  `main` refuses (lane/av1unwritten, untouched).
* **The `dump_stage16` per-axis extent is fixed** (§4) — the class is
  `diagnostic-dump-assumes-one-subsampling`.

## 9. What this lane did not do

* **The unswept family, now swept**: r2 did not run-to-run content
  nondeterminism with uninitialised planes — the family
  `lanes/av1unwritten.report.md:118-124` records as three different plain hashes
  of 440 from three identical invocations. A sentinel run structurally cannot
  detect it (it pins content to one value), and r2's in-process test could not
  (one process, one heap). N separate processes are required; the result is in
  §10. **I had not swept it in r2 and said nothing about it — that was a gap, not
  a negative result.**
* The dump sites still swallow their write errors, so a short dump remains
  possible — the artefact behind §3. Making a partial write loud is an
  instrument change touching every dump in the crate; named, not done.
* No pixel claim is made or unmade for any fixture.
* Untouched per instructions: `film_grain.rs`, the reserved 4:2:0 group-tail
  chroma SKIP arm, `decode_rect4_16_intrabc`, and `lane/av1unwritten`'s content
  (read-only).

## 10. The run-to-run nondeterminism family, swept

N separate PROCESSES (not one process -- an in-process test cannot see this, it
has one heap), uninitialised planes, per-run decode-order concatenation hashed,
exact frame size asserted:

```
=== 440_request_is_422            frames=0   (main = e45cc748 REFUSES it; not sweepable on the merge tree)
=== hg_rect64_intra16x4_witness
5350cd5569d226eb frames=34 sizes=[18524160]
5350cd5569d226eb frames=34 sizes=[18524160]
5350cd5569d226eb frames=34 sizes=[18524160]
5350cd5569d226eb frames=34 sizes=[18524160]
=== hg_arf_witness
dca5b3d9d2d56f30 frames=40 sizes=[18524160]
dca5b3d9d2d56f30 frames=40 sizes=[18524160]
dca5b3d9d2d56f30 frames=40 sizes=[18524160]
dca5b3d9d2d56f30 frames=40 sizes=[18524160]
```

**Negative for both hg fixtures: 4/4 identical, correct size, no spread.** This
is the family a sentinel run structurally cannot detect (it pins content to one
value), and it is independent of the sentinel evidence in section 2.

`440_request_is_422` **could not be swept on the merge tree** -- `main` = `e45cc748`
refuses it, which is the refusal working as intended. On the branch point
`8d6998d7` I measured 3 identical plain hashes (`a761118c8dd5c8d0`), i.e. I did
**not** reproduce the three-distinct-hashes observation at
`lanes/av1unwritten.report.md:118-124`. That is consistent rather than
contradictory: the witness's output depends on ambient memory, and what the
allocator hands back is machine- and run-dependent, so a fixed value on one host
and a varying one on another are the same defect. **Stated as measured: 3/3
identical here, and no claim about reproducing their observation.**

## 11. What the per-axis extent change invalidated in published work

`round_ss(dim, 1)` is bit-identical to `dim.div_ceil(2)`, so **every 4:2:0 dump is
byte-for-byte unchanged** by the `dump_stage16` fix. The change only affects 4:2:2
and 4:4:0 frames.

21 lane files reference the `*_DUMP16` dumps. Of those, exactly one,
`lanes/av1422luma.report.md`, uses non-4:2:0 fixtures (`s422_416x250_10b`). Its
stage table (line 77 onward) is a **Y-only** conclusion — every row reads
"frame 0 Y wrong vs ffmpeg", and the reported numbers are luma sample counts and
a luma first-differing coordinate — so that conclusion **survives** the extent
change unchanged. What the change invalidates is only that report's **label**:
"depth-correct `*_DUMP16`" is no longer accurate for the **chroma half** of those
files, which the 4:2:0 crop had truncated to the top half-height. No published
number moves.

`lanes/unwritten-dep.report.md` (this file) is the only other DUMP16 reference
touching a non-4:2:0 frame, and its numbers are the corrected ones above.

## 12. OPEN, filed: the `unsafe { v.set_len(n) }` in `fresh_plane` is not fully justified

**This is the highest-risk item this lane found, and it is NOT closed here.** It
is filed rather than fixed because closing it means either restoring a ~3% frame-
head cost or building a reads-before-write instrument, and both are decisions
that belong to a lane chartered for them. The comments in `decode.rs` now say
this, so a reviewer can no longer approve the `unsafe` on a withdrawn proof.

**The retracted claim.** `fresh_plane`'s doc asserted the SB-padded surface "is
written sample-for-sample by the tile walk's reconstruction before anything reads
it", and cited the sentinel gate as proof ("proven by the sentinel gate above").
**The same file retracts it**, at `chroma_plane_block_codable`'s "What led here":
on `440_request_is_422`, five runs with uninitialised planes give **two different
output hashes**, five sentinel runs give one, and the pre-deblock census finds
**112 unwritten samples per chroma plane, 224 total**
(`lanes/av1unwritten.report.md:126-128`). Output that varies with the buffer's
initial content means an uninitialised sample was **read**.

**What the gate actually proves — the weaker claim.** The sentinel census counts
at two points, pre-deblock and the output point, so it measures **survival, not
reads**:

* PROVEN: no sample the tile walk never wrote **survives into the frame the
  caller receives**, on the corpus measured.
* NOT PROVEN: "every sample is written before it is read". A sample read as a
  prediction or filter neighbour and overwritten a moment later is invisible to
  both counts. **No such read has been observed, and no instrument in this crate
  measures for one.**
* On `main` = `e45cc748` the one path where a read was **demonstrated** is now
  unreachable — lane/av1unwritten refuses the subsize before descending.

**Why this is UB-class and not a style issue.** `Vec::set_len` over
uninitialised memory is not itself UB: `u16` has no invalid bit patterns and no
`Drop`, so the vector validly *owns* `n` slots. The UB is in **reading** an
uninitialised `u16`. `set_len` is therefore sound only under the "written before
read" premise, and that premise is the one the measurement withdrew.

**Two closures, neither taken here:**

1. **Zero the allocation** — restore `vec![0u16; n]`. Sound unconditionally, at
   the ~3% of frame-thread cycles at 4K that lane-picalloc removed. This is the
   safe default and the honest cost of the optimisation.
2. **Instrument reads-before-write** — a per-sample read/write map, so the premise
   can be proved rather than assumed. That is a real instrument, not a comment,
   and it is the only route that keeps the optimisation.

**Reachability evidence** (what makes this fileable rather than theoretical): a
demonstrated read on `440_request_is_422` at `8d6998d7` — 224 unwritten samples,
output varying with initial content, two distinct plain hashes across five runs
— and that path is refused on `main`, so the corpus contains no *currently
reachable* instance. The claim that needs proving is universal ("every sample is
written before it is read"), and one refused witness cannot establish it for the
other 126 fixtures.

## 13. Does this lane's gate supply the reader `take_unwritten_samples` lacked? — NO

Checked, and the answer is the unflattering one. `take_unwritten_samples` and
`take_census_scanned` (lane/av1unwritten's pre-deblock census API) still have
**no committed reader** on `main` = `e45cc748`: the only references to them
anywhere in `crates/ec-av1/src` are in their own doc comments. `grep` over
`stream.rs` and the rest finds no call site.

My gate reads only its OWN pair, `take_final_unwritten_samples` /
`take_final_census_scanned`. So the r4 pairing is a **template** for how a census
gate should be written (count AND scanned, so "scanned and found nothing" is
distinguishable from "never scanned") — it is **not** a committed gate over the
pre-deblock census, and this report does not claim one. Closing that gap means
either adding the pre-deblock pair to an existing gate's assertions or giving it
its own; that is a decision for the lane that owns the instrument, and it is named
here rather than assumed done.
