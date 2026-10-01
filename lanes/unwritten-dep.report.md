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

* unwritten samples, sentinel run: **U 112, V 112, Y 0** — U and V rows 36–63
  (16 distinct rows), cols 0–15, in whole 4×4 groups (row 36: cols 0–3 and
  12–15). **224 total, which is exactly lane/av1unwritten's own census number**
  (`lanes/av1unwritten.report.md:152-156`).
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
claim is over the **pre-grain** planes. That is also the claim that matters
most — those exact bytes are what the reference bank stores and what every later
frame predicts from, and grain synthesises from the pre-grain frame plus the
grain parameters without reading the plane buffer. "The caller's final grained
picture has no hole" is **not** claimed and is not asserted.

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
$ CARGO_TARGET_DIR=.../uwdep-merge cargo test -p ec-av1 --lib -- the_frame_the_caller_receives_carries_no_unwritten_plane_sample
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 816 filtered out; finished in 71.58s
```

816 filtered (vs 813 on the branch point) — the merged tree's extra tests are
present, so this is the gate running against both lanes' content, not one.

### Mutations

| mutation | result |
|---|---|
| `census_unwritten_final` neutered (`if true { return; }`) | **RED**: "the census reported 0 for planes carrying exactly 7 sentinel samples … (class: census-silently-blind)" |
| the `census_unwritten_final` call removed from the output point | RED at the same control |
| one sentinel sample forced into a film frame's output (`p.y[0] = 0xDEAD`) | RED on the fixture: "hg_arf_witness hands the caller N samples the tile walk never wrote" |

The first is the non-vacuity proof in the same run, and it no longer depends on
any fixture's decode behaviour.

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
