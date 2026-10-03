# lane/av1graincensus — the unwritten-sample census was blind to `apply_grain`

W1-2 of `lanes/refusal-plan.report.md` (`lane/refusalplan`). Base `5ba7a123`.

`census_unwritten_final` is called in `stream.rs` **before** `apply_grain`.
`film_grain::uninit_plane` allocates the grained destination as *uninitialised*
memory — `Vec::with_capacity` + `set_len`, no fill — with correctness resting on
a band row partition rather than on the type system. `lanes/unwritten-dep.report.md`
§6 and §8 both say, in their own words, that "the caller's final grained picture
carries no unwritten plane sample" is **not claimed**, because the census never
sees that buffer.

**This lane measures it.** Post-grain census at `apply_grain`'s return, its own
counter pair, two mutations proven red, and — because the census turned out to be
a strictly weaker instrument than the gap suggested — a second `unsafe` in the
same function brought under the gate that can actually see it (§5).

---

## 1. What was wrong, precisely

Two claims in the tree, both about the same buffer, neither true together:

* `stream.rs`'s comment on the pre-grain call said the census "covers the whole
  span from the tile walk to the caller's frame". Every filter (deblock, CDEF,
  LR, superres) writes **in place** into the buffer that call scans, so that part
  holds — but `apply_grain` runs *after* it and **allocates a new picture**. No
  filter does that. So the comment's scope was one stage too wide.
* `film_grain::uninit_plane`'s SAFETY comment says the proof that no uninitialised
  sample is read is "the band row partition at that call site, not the type
  system … the byte-exact 1-vs-4-thread gate is what checks it". No gate with a
  grain fixture existed to check it (§5).

Neither sentence was false on its own terms; together they left the only picture
between the pre-grain census and the caller unmeasured and the only other
`set_len` allocation uncovered.

---

## 2. The change

### 2.1 `decode.rs` — a second census, its own counters

`census_sentinels` is now the shared `(scanned, unwritten)` body, so
`census_unwritten_final` and the new `census_unwritten_grained` cannot drift into
disagreeing about what counts as unwritten. It returns `None` when the sentinel
fill is not live, so neither caller can publish a meaningless zero.

```rust
static GRAINED_UNWRITTEN_SAMPLES: AtomicUsize;
static GRAINED_CENSUS_SCANNED:    AtomicUsize;
pub fn take_grained_unwritten_samples() -> usize;
pub fn take_grained_census_scanned()  -> usize;
pub(crate) fn census_unwritten_grained(y: &[u16], u: &[u16], v: &[u16]);
```

The `scanned`/`found` **pairing** is the same one the pre-deblock and pre-grain
censuses already use: `unwritten == 0` is also what a build with the call deleted
reports, so a gate that reads only the count passes vacuously. `scanned > 0` is
reachable only by `apply_grain` actually running.

`plane_poison` and `plane_sentinel_on` became `pub(crate)` so `film_grain` fills
and counts the **same value under the same predicate** — the disagreement that
would make the count meaningless is now impossible by construction.

### 2.2 `film_grain.rs` — poison under the census arm, census at the return

```rust
fn uninit_plane(n: usize) -> Vec<u16> {
    let mut v: Vec<u16> = Vec::with_capacity(n);
    unsafe { v.set_len(n) };
    if crate::decode::plane_sentinel_on() {      // NEW — census arm only
        v.fill(crate::decode::plane_poison());
    }
    v
}
```

Counting `== plane_poison()` over ambient garbage is **not a census**: a hole
survives with probability ~1/65536 rather than always, so the count would be ~0
whether or not the band copy covered every row. The fill is what turns "the band
copy wrote every sample" from an assumption into a measurement.

With the sentinel off — every shipped build, every byte-exact gate, every ffmpeg
comparison — `uninit_plane` is still `with_capacity` + `set_len`. **The corner-cut
it documents is untouched where it is actually paid for.**

The census is called at **both** returns of `apply_grain`, so "the picture
`apply_grain` hands back is censused in its final form" is a property of the
function rather than of one branch.

### 2.3 Why one call site and not four

`stream.rs` synthesizes grain at four places (threaded decode, threaded
`show_existing_frame`, inline decode, inline `show_existing_frame`) and
`apply_grain` is the only one of them. One census inside `apply_grain` covers all
four; four census calls in `stream.rs` would cover the same pictures and drift.

---

## 3. SCOPE, stated not implied

* The new census covers **the picture the caller receives on a grained frame, in
  its final form, after the noise**. That is its whole claim.
* It is **not** a claim that the pre-grain census already covered this. That
  census never sees this buffer. `lanes/unwritten-dep.report.md` §6/§8 were
  right, and the pre-grain gate's own doc comment now says so and points here.
* It is **not** a claim about a frame whose grain step does not run — for a
  non-grained frame the caller receives the very bytes `census_unwritten_final`
  scanned, and `EC_AV1_NO_GRAIN` makes the grained call not run at all.
* It is **not** a claim that grain cannot introduce a wrong VALUE (§5 is where
  that question lives).

**Out of scope, untouched:** the decode path, the reserved 4:2:0 group-tail
chroma SKIP arm, `decode_rect4_16_intrabc`, and the ffmpeg oracle helpers
(`assert_420_oracle_stream` and its callers).

---

## 4. The gate

`stream::tests::the_grained_picture_the_caller_receives_carries_no_unwritten_plane_sample`

```text
$ cargo test -p ec-av1 --lib -- the_grained_picture_the_caller_receives_carries_no_unwritten_plane_sample
test stream::tests::the_grained_picture_the_caller_receives_carries_no_unwritten_plane_sample ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 837 filtered out; finished in 26.98s
```

Measured on the census child (`EC_AV1_PLANE_SENTINEL=1`, one process per
fixture), provenance-checked binary — only this worktree's paths appear in it:

```text
fixture                     UWDEP_CENSUS (pre-grain)     GRAINCENSUS (post-grain)
s422_grain_160x128          0  frames=3  scanned=122880  0  frames=3  scanned=122880
grain_cdef_lr_128x128       0  frames=5  scanned=147456  0  frames=5  scanned=122880
hg_arf_witness              0  frames=37 scanned=370483200  0  frames=37 scanned=111144960
synthetic                   7  scanned=40                5  scanned=40
```

Two things in that table are load-bearing:

* **`scanned` is non-zero on every arm.** It is the only number that
  distinguishes "scanned and found nothing" from "never scanned", and it is
  reachable only by `apply_grain` having run. It is simultaneously the
  proof-that-grain-fired and the deletion control.
* **The synthetic control counts 5, not 7.** The pre-grain arm's synthetic uses
  7 on the same planes. A gate that read the wrong counter for the grained arm
  would see 7 and go red. The two arms cannot be confused for one another.

The three fixtures are chosen for coverage, not for size: `s422_grain_160x128` is
4:2:2 (the per-axis chroma geometry), `grain_cdef_lr_128x128` is 4:2:0 running
CDEF and LR as well, and `hg_arf_witness` is the real 10-bit 3840x1608 film with
hidden alt-refs whose height is **not** a multiple of the 32-pixel band row — the
shape where the row partition has a tail.

Note `grain_cdef_lr_128x128`'s post-grain `scanned` (122880) is *smaller* than its
pre-grain `scanned` (147456): grain is synthesised for shown output only, and
hidden frames never reach the grained return. That asymmetry is the expected
result, not a gap in the census.

---

## 5. Mutations

| mutation | result |
|---|---|
| **A — the `census_unwritten_grained` call at `apply_grain`'s return DELETED** | **RED**: "the grained census scanned 0 samples on s422_grain_160x128 — the call at `apply_grain`'s return is not running (or grain never fired), so the 0 below proves nothing (class: census-never-ran)" |
| **B — one sentinel sample forced into the returned grained picture** (`out.y[0] = plane_poison()` before the census) | **RED**: "s422_grain_160x128 hands the caller a grained picture with **3** samples the grain destination's band copy never wrote" — exactly 3, because that fixture has 3 grained frames and the mutation fires once per frame |

### 5.1 The measurement that changed the lane's conclusion

I neutered `copy_clean_rows` — the grain band's clean-row copy, i.e. the code
whose correctness the `unsafe` rests on — leaving a destination written **only**
by the noise kernels, and re-ran the census:

```text
s422_grain_160x128    GRAINCENSUS 0 frames=3 scanned=122880
grain_cdef_lr_128x128 GRAINCENSUS 0 frames=5 scanned=122880
```

**Still 0.** The census is not lying and is not broken: the noise kernels overwrite
every destination sample they read, so no poison survives to the return. "0
unwritten samples in the caller's picture" is a *true* statement about a buffer
that was in fact filled with garbage.

Which means the class **`census-of-unwritten-samples-cannot-see-a-read-of-
uninitialised-memory`** is real, and this lane's census does not cover it. The
gate that does is `the_frame_does_not_depend_on_the_plane_buffers_initial_content`
— the `0xDEAD`-vs-`0xBEEF` arm, which asks whether the *value* the buffer held
can reach the output. **Its fixture list had no grain fixture at all**, so
`uninit_plane`'s `unsafe` — the crate's only other `Vec::with_capacity` +
`set_len` — had no coverage there.

**Class sweep, same lane:** added `s422_grain_160x128` to that gate's `FIXTURES`
(the smallest grain fixture; the arm grows ~0.05 s), and proved it bites by
re-running mutation D — `copy_clean_rows` neutered again:

| mutation | result |
|---|---|
| **D — `copy_clean_rows` neutered, allocdet gate WITH the grain fixture** | **RED**: "s422_grain_160x128 decoded to a DIFFERENT picture depending on what the plane buffer held before the decode wrote it — sentinel-off-uninit gave `a1cafe984ea68d83`, the baseline arm gave `dfe426a3b9597432`" |
| **D′ — same mutation, unwritten-sample census** | **GREEN — correctly so** (§5.1). Two instruments, two different questions, and only one of them was pointed at this `unsafe`. |

Before this lane the same mutation was invisible to both.

---

## 6. Byte-exactness and the scoped sweep

Film grain stays byte-exact against ffmpeg — `EC_AV1_REQUIRE_FFMPEG=1`, so no arm
can skip:

```text
$ EC_AV1_REQUIRE_FFMPEG=1 cargo test -p ec-av1 --lib -- <grain + census gates>
test stream::tests::a_real_422_film_grain_stream_decodes_pixel_exact ... ok
test stream::tests::a_real_aomenc_10bit_film_grain_stream_decodes_pixel_exact ... ok
test stream::tests::a_real_aomenc_12bit_film_grain_stream_decodes_pixel_exact ... ok
test stream::tests::a_real_aomenc_stream_with_film_grain_decodes_pixel_exact ... ok
test stream::tests::real_aomenc_film_grain_streams_decode_pixel_exact ... ok
test stream::tests::the_frame_does_not_depend_on_the_plane_buffers_initial_content ... ok
test stream::tests::the_frame_the_caller_receives_carries_no_unwritten_plane_sample ... ok
test stream::tests::the_grained_picture_the_caller_receives_carries_no_unwritten_plane_sample ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 830 filtered out; finished in 111.39s
```

Every `uninit_plane` allocation in that run is provably unfilled:
`EC_AV1_PLANE_SENTINEL` is unset, `plane_sentinel_on()` is false, and each
allocation is the byte-identical `with_capacity` + `set_len` it was before.

The full `ec-av1` suite is a VPS job (§8); this lane ran only scoped tests
locally, as the shared context requires.

---

## 7. What this lane did not do

* **It did not fix anything.** Every measured count was already 0. The defect was
  in the *instrument*, not the decoder — the same class as
  `lanes/unwritten-dep.report.md` §4's `dump_stage16`.
* **It did not make the census able to see a read of uninitialised memory.**
  §5.1 says plainly that it cannot, and that the initial-content gate is the
  instrument for that. §5 adds the grain coverage there; it does not pretend the
  census grew a new sense.
* **It did not touch the reserved 4:2:0 group-tail chroma SKIP arm**, which is
  reserved for its owning lane, nor `decode_rect4_16_intrabc`, nor the ffmpeg
  oracle helpers.

## 8. Open, filed

* **Full-suite attribution is still owed.** The five grain gates, both census
  gates and the initial-content gate are green locally; the whole `ec-av1` suite
  is a VPS job and was not run by this lane. Whoever merges this should read the
  VPS suite result for these eight test names.

## 9. Commands

```bash
cd /home/tahinli/.cache/wt/av1graincensus
export CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1graincensus

cargo test -p ec-av1 --lib --no-run
EC_AV1_REQUIRE_FFMPEG=1 cargo test -p ec-av1 --lib -- <grain + census gates>

# the census child by hand, one process per fixture
B=$(ls -t $CARGO_TARGET_DIR/debug/deps/ec_av1-* | grep -v '\.d$' | head -1)
EC_AV1_PLANE_SENTINEL=1 UWDEP_CENSUS_FIXTURE=s422_grain_160x128 \
  $B --exact stream::tests::uwdep_child_unwritten_census --nocapture

# mutations A, B, C, D: edit film_grain.rs, rebuild, run the gate named in §5
```
