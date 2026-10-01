# lane/unwritten-dep — the hg_* divergence is a TRUNCATED DUMP, and the census's blind spot is the loop filter

**Outcome in one line: neither `hg_rect64_intra16x4_witness.obu` nor
`hg_arf_witness.obu` output depends on unwritten plane content on `8d6998d7` —
the campaign's sentinel hash is reproduced EXACTLY by a run whose filesystem ran
out of space mid-write (`97ef82c0ce` = frames 0–4 complete plus 13 320 192 bytes
of frame 5, hashed `cat dir/frame.f*`-style); and the census's real blind spot is
named and fixed: it runs at PRE-DEBLOCK, while deblock READS neighbours to decide
an edge, so a hole can be invisible there and still change the output. A
post-filter census, taken at the exact point `EC_AV1_FINAL_DUMP` writes, reports
32 unwritten samples on the 4:2:2 witness (non-vacuity control) and 0 on all
four film fixtures.**

Branch `lane/unwritten-dep`, base `main` = `8d6998d7`. No push.

---

## 1. Provenance

The probe was built in this lane's own worktree with its own target dir, and its
embedded paths name only this worktree:

```
$ cd /home/tahinli/.cache/wt/unwrittendep
$ CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/uwdep-k9 cargo build -p ec-av1 --example decode_probe
$ strings /home/tahinli/.cache/tgt/uwdep-k9/debug/examples/decode_probe | grep -o "wt/[a-zA-Z0-9_-]*" | sort -u
wt/unwrittendep
$ git rev-parse HEAD
8d6998d7cbafa1a7a5271e0d638365890a4b4310
```

Two traps in the brief were live and are worth recording because both produce a
*silent* wrong reading:

* the first build's target dir (`/home/tahinli/.cache/tgt/unwrittendep`) was
  **deleted underneath this lane mid-run**; every binary whose path is quoted in
  this report is under `uwdep-k9`, built and used after that;
* `/tmp` is a 16 GB tmpfs that ran at 80% during the first sweep and silently
  produced **short** dump files (sizes 0, 7 553 024, 4194304 instead of
  18 524 160) — those runs are excluded below and every retained run asserts
  the exact expected frame size.

Fixtures, byte-identical to what the campaign measured:
`hg_arf_witness.obu` 32 126 bytes sha256 `7f3b060da5aa9c53…`,
`hg_rect64_intra16x4_witness.obu` 23 472 bytes sha256 `c9e721088766163b…`.

Both fixtures are 10-bit 3840×1608, so `EC_AV1_FINAL_DUMP` writes u16 LE and one
frame is exactly `(3840*1608 + 2*1920*804) * 2 = 18 524 160` bytes. Every run
below asserts that size.

## 2. Reproduction — and the result is NOT the campaign's

`EC_AV1_FINAL_DUMP` writes `<prefix>.f<N>` per decode-order frame. Hashing the
concatenation in the order a shell glob produces (`cat dir/frame.f*`, i.e.
lexicographic) is the shape that reproduces the campaign's numbers exactly:

| fixture | plain run | sentinel run | campaign plain | campaign sentinel |
|---|---|---|---|---|
| `hg_rect64_intra16x4_witness` (34 frames) | `a26438168c` ×3 | `a26438168c` ×2 | `a26438168c` | `0ee1edf403` |
| `hg_arf_witness` (40 frames) | `8d7a43bc5f` ×3 | `8d7a43bc5f` ×2 | `8d7a43bc5f` | `97ef82c0ce` |

**Both campaign PLAIN values reproduce exactly. Neither SENTINEL value does.**
Plain is stable across repeats and across five different ambient contents
(glibc `MALLOC_PERTURB_` ∈ {0, 1, 85, 170, 254} — every fresh allocation filled
with that byte), across `EC_AV1_THREADS` ∈ {1, 2, 4, 8}, and in-process with the
allocator's free list deliberately poisoned (§4). Under every one of those the
sentinel run is byte-identical to the plain run, all 34 / all 40 frames.

## 3. What the campaign's sentinel hash actually is: a truncated dump

`EC_AV1_FINAL_DUMP` ignores its write error — `let _ = f.write_all(&buf);` at
`stream.rs` (the final-dump site). A run whose filesystem fills therefore leaves
a byte stream that is a **prefix** of the true one, and nothing reports it.

Sweeping every (frames-complete, tail-truncated) shape against the campaign's
value:

```
$ python3 /tmp/unwritten/partial.py hg_arf_witness
hg_arf_witness: 40 frames (max index 39), hunting 97ef82c0ce
  MATCH: frames 0..4 + f5 truncated to 13320192 bytes, lex-hashed
  1 match(es)
```

**`97ef82c0ce` is reproduced bit-for-bit by a dump that stopped after 5 complete
frames and 13 320 192 of frame 5's 18 524 160 bytes** — 71.8% of one frame, with
frames 6–39 never written. The decode is byte-identical; only the FILESYSTEM
differs. This is the same class the brief already flagged ("a differing hash
between runs can be a timeout with no dump at all"), one size up: not an absent
dump but a **partial** one.

Corroborating mechanisms, each demonstrated with a byte-identical decode and a
different hash:

* **`RLIMIT_FSIZE`** caps a file's size; `sigXFSZ` ignored, the write fails
  cleanly past the cap. `hg_rect64` at a 4 MiB cap: `788d3be08ae5112e` for both
  plain and sentinel, versus `5350cd5569d226eb` uncapped. Same decode, different
  hash.
* **A reused dump directory** keeps the tail files of any fixture that wrote more
  frames earlier. `hg_arf_witness` into a directory `hg_head_mvclamp_witness`
  (55 frames) had written leaves 55 files and hashes `9ca5c4d7af` — a value no
  single decode produces.
* **Hashing the `sha256sum` LISTING** rather than the bytes embeds the directory
  path, so two byte-identical dumps in differently-named directories hash
  differently (`8d7a43bc5f` vs `be6ca147a7` for the same content).

`hg_rect64`'s `0ee1edf403` is **not** reproduced by any of these. Swept and ruled
out: every numeric prefix and every prefix-plus-page-truncated tail at 4 KiB and
512 B granularity, prefixes built from either fixture (rect64 is a 34-frame
prefix of arf's 40 with byte-identical frames 0–33), hole shapes (one truncated
frame among a complete set), zero-length-frame shapes, stale cross-fixture
directory mixes in both orders, and the listing shape. Whatever produced that one
value, it is not the decode: §4 shows the decode's output is invariant under
every ambient content available to test.

## 4. The instrument with the blind spot is the CENSUS, and here is exactly where

The refusal lane's `census_unwritten` scans the three planes at each frame's
**pre-deblock** point. That is a real blind spot, and it is demonstrable on the
one fixture where a hole genuinely reaches the output.

Stage-by-stage on `440_request_is_422` (8-bit 64×64, a 4:2:2 sequence header over
a 4:2:0 tile — lane/av1unwritten's witness, which main still decodes), comparing
an uninitialised run against a `PLANE_SENTINEL`-filled one:

| stage dump | bytes | plain vs sentinel |
|---|---|---|
| `EC_AV1_PREFILT_DUMP16` (pre-deblock, where the census runs) | 12 288 | **IDENTICAL** |
| `EC_AV1_POSTDEBLOCK_DUMP16` | 12 288 | DIFFERS (26 bytes) |
| `EC_AV1_POSTCDEF_DUMP16` | 12 288 | DIFFERS (26 bytes) |
| `EC_AV1_DECODE_ORDER_DUMP` | 8 192 | DIFFERS (1 985 bytes) |
| `EC_AV1_FINAL_DUMP` | 8 192 | DIFFERS (1 986 bytes) |

So the pre-deblock census sees **zero** unwritten samples and byte-identical
planes, and the dependence is nevertheless in the output. **Deblock creates it**:
it reads a neighbouring sample to decide an edge (`aom_lpf_*` masks and
thresholds), so an unwritten sample changes its neighbour's *filtered* value even
though the unwritten sample itself is overwritten a moment later. A scan taken
before the filter cannot see that, by construction.

An exact u16 census at the pre-deblock point confirms the zero rather than
inferring it: `EC_AV1_PREFILT_DUMP16` under `EC_AV1_PLANE_SENTINEL=1`, whole
file, `0xDEAD` counted per frame — 0 on every frame of all three fixtures.

**The fix (§5) is to also take the census where the output is taken.** A census
there reports 32 unwritten samples on this witness and 0 on every clean fixture.

### Displayed vs hidden, per fixture

`hg_arf_witness`: 40 decode-order frames, 19 shown, 21 hidden alt-refs, plus 18
`show_existing_frame` headers. `hg_rect64_intra16x4_witness`: 34 decode-order
frames, 17 shown, 17 hidden, plus 16 `show_existing_frame`. (Header counts from
`EC_PROBE_HDR=1`; the show/hidden split is per non-`show_existing` header.)

**Answer to the campaign's question: no unwritten sample reaches a DISPLAYED
frame on either fixture — because no unwritten sample reaches the output at all,
displayed or hidden.** Every one of the 34 / 40 frames, hidden alt-refs included,
hashes identically under plain, sentinel, and five ambient contents. The hidden
frames matter to the question precisely because they are the ones a shown-only
gate never looks at; here they are clean too, which is why the film fixtures'
existing pixel-exactness gates are not exposed by this.

The only fixture in the corpus where a hole reaches the output is the 4:2:2
witness, whose single frame is SHOWN (`Key show=true`), and whose cause is the
invalid subsize that lane/av1unwritten refuses. That lane owns the cause; this
lane touches nothing in it (see §7).

## 5. The fix and the gate

`crates/ec-av1/src/decode.rs`:

* `PLANE_SENTINEL` census **at the output point** — `census_unwritten_final(y, u, v)`
  counts, over the three planes as they are about to be stored into the reference
  slots, how many samples still hold `PLANE_SENTINEL` once every filter has run.
  Those planes are already cropped to the true extent at this point, so a flat
  count over each IS the true-extent count. Env-gated on the same flag as the
  fill (the fill and the census must agree on when they are live).
* `FINAL_UNWRITTEN_SAMPLES`, a process-global counter, and
  `take_final_unwritten_samples()` to read and clear it — process-global rather
  than thread-local because the gate reads it from a different thread than the one
  that scanned.

`crates/ec-av1/src/stream.rs`: the census is called beside the `EC_AV1_FINAL_DUMP`
site — the same point, the same planes, so the claim it supports is literally
"no sample the tile walk never wrote survives into the frame the caller receives".

The gate `the_frame_the_caller_receives_carries_no_unwritten_plane_sample`
(`crates/ec-av1/src/stream.rs`) asserts, in this order:

1. **non-vacuity first**: the census reports **> 0** on `440_request_is_422`
   (measured 32). A census that read zero everywhere would fail here, so every
   zero below means something;
2. **0** on `hg_arf_witness` (37 shown frames), `hg_rect64_intra16x4_witness` (33),
   `gm_small_side_witness` (33), `troy_sb128_inter_witness` (15) — the real film
   streams with hidden alt-refs, the only shapes where a hidden frame's hole
   could reach a later displayed frame through the reference bank.

The fill is env-gated and this crate forbids `set_var` (`envflags`), so the
measurement runs in a child process (`uwdep_child_unwritten_census`) that the
parent invokes by name; the parent asserts the child's reported count. The child
asserts `EC_AV1_PLANE_SENTINEL` is set, so a run where the fill was not live
fails rather than reporting a meaningless zero.

```
$ cargo test -p ec-av1 --lib -- the_frame_the_caller_receives_carries_no_unwritten_plane_sample
test stream::tests::the_frame_the_caller_receives_carries_no_unwritten_plane_sample ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 813 filtered out; finished in 66.06s
```

Measured census, all five fixtures, provenance-verified probe:

```
440_request_is_422            FINAL_UNWRITTEN 32   frames=1    <- control, non-zero
hg_arf_witness                FINAL_UNWRITTEN 0    frames=37
hg_rect64_intra16x4_witness   FINAL_UNWRITTEN 0    frames=33
gm_small_side_witness         FINAL_UNWRITTEN 0    frames=33
troy_sb128_inter_witness      FINAL_UNWRITTEN 0    frames=15
```

### Mutations — three, all red

| mutation | result |
|---|---|
| `census_unwritten_final` neutered (`if true { return; }`) | RED at the non-vacuity control: "the census reports 0 even on the one fixture whose output DOES depend on unwritten plane memory" |
| the `census_unwritten_final` call removed from the output point | RED at the same control |
| one sentinel sample forced into a film frame's output (`EC_AV1_UWDEP_MUT_HOLE=1`, `p.y[0] = 0xDEAD`) | RED on the fixture itself: "hg_arf_witness hands the caller 40 samples the tile walk never wrote (class: a hole the loop filter read)" |

The third is the one that matters: it shows the gate bites on a hole of **one
sample in one frame** on a stream that otherwise decodes perfectly, which is
exactly the class the pre-deblock census missed.

## 6. Commands

```bash
# provenance
cd /home/tahinli/.cache/wt/unwrittendep
CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/uwdep-k9 cargo build -p ec-av1 --example decode_probe
strings /home/tahinli/.cache/tgt/uwdep-k9/debug/examples/decode_probe | grep -o 'wt/[a-z]*'

# reproduction: 3 plain + 2 sentinel, per-frame size asserted, per-frame hash
#   (frame = 18524160 bytes; the campaign's plain values reproduce exactly)
# stage bisect that names the blind spot
python3 /tmp/unwritten/stages.py 440_request_is_422
#   EC_AV1_PREFILT_DUMP16    12288 bytes  IDENTICAL
#   EC_AV1_POSTDEBLOCK_DUMP16 12288 bytes DIFFERS (26 bytes)
#   EC_AV1_DECODE_ORDER_DUMP  8192 bytes  DIFFERS (1985 bytes)
#   EC_AV1_FINAL_DUMP         8192 bytes  DIFFERS (1986 bytes)
# the campaign's sentinel hash, reproduced as a truncated dump
python3 /tmp/unwritten/partial.py hg_arf_witness     # MATCH: 0..4 + f5 cut to 13320192 bytes
# the gate
cargo test -p ec-av1 --lib -- the_frame_the_caller_receives_carries_no_unwritten_plane_sample
```

## 7. Disposition of the campaign's two rows

* **`hg_arf_witness` — closed, not a defect.** Output is invariant under the
  sentinel, five `MALLOC_PERTURB_` values, four thread counts, and in-process
  allocator poisoning. The reported divergence is reproduced exactly as a
  **truncated dump** (`97ef82c0ce`), which the fix to the dump path must not be
  mistaken for a decode difference: the dump site still ignores its write error,
  and that is a property of the INSTRUMENT. This lane does not change the dump
  site (the brief scopes the fix to the walk / the pre-read write), and the gate
  in §5 makes the *decoder* claim checkable regardless.
* **`hg_rect64_intra16x4_witness` — closed, not a defect, cause not identified.**
  Same invariance, same gate coverage. Its campaign sentinel value
  (`0ee1edf403`) is not reproduced by any dump-artefact shape swept (§3) and
  not by the decode (§4); it is unexplained. Named as an open measurement
  question, not a defect.
* **`440_request_is_422` — not this lane's to fix.** Its 32 unwritten samples and
  the loop-filter read are measured and gated here as the non-vacuity control;
  the CAUSE is the invalid subsize lane/av1unwritten refuses, and that lane owns
  it.

Untouched, as instructed: `film_grain.rs`, the reserved 4:2:0 group-tail chroma
SKIP arm, `decode_rect4_16_intrabc`, and anything on `lane/av1unwritten` (its
`census_unwritten` and the two dump-adjacent hunks in `decode.rs` were read, not
written; this lane's changes are a new function above `fresh_plane`, one call
beside the final-dump site, and the two new tests).

## 8. What this lane did not do

* The dump sites still swallow their write errors. A short dump is the artefact
  that produced this whole question, and the honest fix is to make a partial
  write loud (`assert_eq!` on the written length, or write-then-verify). That is
  an instrument change touching every dump in the crate, beyond this lane's
  scope, and it needs its own mutation proof. **Named, not done.**
* `0ee1edf403` is unexplained (§3).
* No pixel claim is made or unmade for any fixture. The gate claims only that no
  unwritten sample reaches the frame the caller receives.
