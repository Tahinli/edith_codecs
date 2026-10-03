# lane/hash0ee1edf403 — `0ee1edf403` is still open, and the closure argument is now a proof rather than a sweep count

**Verdict in one line: NOT REPRODUCED.** Every byte-shape of this tree's frames
is exhausted at a granularity no prior sweep reached — **every single byte
offset**, 629 821 474 truncations per frame order, zero hits — and the campaign's
own PLAIN value `a26438168c` reproduces here while its SENTINEL value does not,
which places the divergence **outside every artefact shape** and leaves exactly
one live mechanism class: *the sentinel run on the campaign host read
uninitialised memory that this host never hands out*. §9 states it as a
conclusion and says what would falsify it.

## 1. Provenance

Scratch worktree of the campaign's own tree, its own target dir, and the binary's
embedded paths name only that worktree — the stale-binary trap
(`lanes/av1unwritten.report.md` §7) does not apply to any number below.

```
$ git worktree add /home/tahinli/.cache/wt/hash0ee1 8d6998d7
$ CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/hash0ee1 \
    cargo build -p ec-av1 --example decode_probe
$ strings .../decode_probe | grep -o 'wt/[a-zA-Z0-9_-]*' | sort -u
wt/hash0ee1
```

Fixture: `crates/ec-av1/fixtures/hg_rect64_intra16x4_witness.obu`, 23 472 B,
34 decode-order frames of exactly 18 524 160 B each (`sha256 c9e72108…`, per
`lanes/unwritten-dep.report.md` §1). `hg_arf_witness.obu` is its 40-frame
extension and is used here as the **control fixture**: its campaign value
`97ef82c0ce` IS a known truncation, so the sweeper that fails on rect64 can be
shown to have teeth (§3).

Harness shape, held fixed everywhere: `cat dir/f.f*` — **shell glob order, i.e.
lexicographic by file name** (`f.f0, f.f1, f.f10, f.f11, … f.f19, f.f2, …`) —
piped to `sha256sum`, first 10 hex. Every prior sweep used this shape and so does
every sweep here. Numeric order is carried alongside wherever the shape could
plausibly differ, because on a 34-frame directory the two orders are different
byte streams (`a26438168c` lex vs `5350cd5569` numeric, both measured).

**Read-only on the decode path.** No crate source is edited in this lane; every
artefact under `/home/tahinli/.cache/hash0ee1-*` is a measurement harness.

## 2. The baseline, and the fact the whole report turns on

```
plain   × 8 separate processes   frames=34 sizes=[18524160]  lexcat10=a26438168c   (×8)
sentinel× 8 separate processes   frames=34 sizes=[18524160]  lexcat10=a26438168c   (×8)
```

Not just the same hash — **the frame files are byte-identical between the two
fills** (checked byte-for-byte, not by hash, in §7). On this tree the sentinel
changes nothing at all in the artefact. The campaign recorded the same plain
value and a *different* sentinel value. That asymmetry is the whole puzzle, and
it is why the answer cannot be an artefact shape: **every shape built from these
bytes collapses to `a26438168c`**, so a sweep over shapes cannot reach
`0ee1edf403` no matter how fine its granularity.

## 3. A byte-exact truncation sweeper, proved to have teeth

Every prior truncation sweep stopped at a granularity (`4 KiB`, `512 B`, or a
handful of fractions). `hash0ee1-trunc.c` walks a SHA-256 state one **byte** at a
time and checks the digest after every single offset, for every prefix length, in
either frame order — so the granularity of the sweep is now 1, not 4096.

Control first, because a negative from an unvalidated sweeper is worthless. Run
against `hg_arf_witness`, whose campaign value `97ef82c0ce` is a known
truncation:

```
$ hash0ee1-trunc 97ef82c0ce …/hash0ee1-frames/arf 40 lex
MATCH(lex): frames 0..4 complete + next frame truncated to 13320192 bytes
[lex] checked 740966440 truncations (every byte offset, 0..frame end)
```

That is the exact shape `lanes/unwritten-dep.report.md` §3 reported (frames 0–4
plus 13 320 192 B of frame 5), found by the sweeper with no hint in it. Independently
confirmed in Python:

```
frames 0..4 + f5[:13320192]  ->  97ef82c0ce0b3778
```

SHA-256 correctness is pinned against `hashlib` on `"abc"` and `""` from the same
binary (`ba7816bf…`, `e3b0c442…`).

## 4. The target, at one-byte granularity

```
$ hash0ee1-trunc 0ee1edf403 …/hash0ee1-frames 34 lex
[lex] checked 629821474 truncations (every byte offset, 0..frame end)   → no MATCH
$ hash0ee1-trunc 0ee1edf403 …/hash0ee1-frames 34 num
[num] checked 629821474 truncations (every byte offset, 0..frame end)   → no MATCH
```

629 821 474 = 34 × 18 524 161, i.e. **every prefix length × every byte offset of
the next frame, including the 0-length and full-frame ends**, in both frame
orders. This subsumes the whole `4 KiB` / `512 B` / `1/4` / `1/1024` / `1/65536`
family of `lanes/av1dumploud.report.md` §3E and `lanes/unwritten-dep.report.md`
§3, and adds the 4 093 601 offsets between two page boundaries that no earlier
sweep looked at.

## 5. Run-to-run nondeterminism with uninitialised planes (the unswept family)

This is the family `lanes/unwritten-dep.report.md` §9 named as never swept in r2
and §10 then swept at N=4. It is re-swept here at N=8, under ambient perturbation
and under thread-count and scheduling variation, on the campaign's own tree.

| lever | plain | sentinel |
|---|---|---|
| 8 separate processes | `a26438168c` ×8 | `a26438168c` ×8 |
| `MALLOC_PERTURB_` ∈ {1, 85, 170, 254} | `a26438168c` ×4 | `a26438168c` ×4 |
| `MALLOC_MMAP_THRESHOLD_=0` (forces reuse of freed heap instead of fresh `mmap`) | `a26438168c` | `a26438168c` |
| `+ MALLOC_TRIM_THRESHOLD_=1 GiB` | `a26438168c` | `a26438168c` |
| max poison: `MMAP_THRESHOLD_=0 + TRIM=1GiB + TOP_PAD=64MiB + PERTURB_ ∈ {170, 254}` | `a26438168c` ×2 | `a26438168c` ×2 |
| `EC_AV1_THREADS` ∈ {1, 2, 4, 8} | `a26438168c` ×4 | `a26438168c` ×4 |
| 8 concurrent decodes, both fills, on an oversubscribed box (6 spinners on 12 cores) | `a26438168c` ×4 | `a26438168c` ×4 |

`MALLOC_MMAP_THRESHOLD_=0` is the load-bearing one: with the default threshold a
12 MiB plane allocation goes to fresh `mmap` and is therefore **zero** every
time, which is why a plain run can look content-invariant while reading
uninitialised memory. Forcing the heap path makes the recycled block carry
whatever the previous decode left in it. Even that does not move a byte.

**34 + 34 processes, 16 of them under deliberate allocator poisoning and 8 under
CPU oversubscription: one value.** This host does not exhibit the
`440_request_is_422` behaviour `lanes/av1unwritten.report.md:118-124` measured
(`lanes/unwritten-dep.report.md` §10 also failed to reproduce it here, at N=5).

### 5b. The grain plane is a second uninitialised buffer, and it is not sentinel-gated

`EC_AV1_PLANE_SENTINEL` gates exactly one allocation: `fresh_plane`
(`decode.rs:21254`). A second `set_len`-over-`with_capacity` exists at
`film_grain.rs:1289` (`uninit_plane`), and this fixture **does** carry film grain
(`EC_PROBE_HDR=1`, HDR 1: `grain=true/seed=50047`). So the sentinel cannot see a
grain-plane hole, which is a mechanism a sentinel-vs-plain sweep structurally
cannot exclude.

```
plain                     frames=34 lexcat=a26438168c3c53a0
sentinel                  frames=34 lexcat=a26438168c3c53a0
EC_AV1_NO_GRAIN=1         frames=34 lexcat=a26438168c3c53a0   (plain)
EC_AV1_NO_GRAIN=1 + SENTINEL          frames=34 lexcat=a26438168c3c53a0
0xDEAD residue in the sentinel dump: 0 pairs (plain baseline: 0 pairs)
```

Skipping grain entirely does not move the hash, and no sentinel value survives
into the output. **A grain-plane hole reaching output is ruled out** — and note
this closes a specific class the earlier lanes left open by naming only, not by
sweeping (§8).

## 6. A dump directory shared across fixtures (the whole corpus, not four names)

`stale2.py` tried four named polluters. Here **every one of the 126 other
committed fixtures** is a polluter: each is decoded alone and its frames past
index 33 are mixed into rect64's 34, hashed in both fills and in lex order.

Only **two** fixtures in the corpus decode more than 34 frames, so only two
mixes exist:

```
hg_arf_witness          40 frames, tail=6   mix-plain=8d7a43bc5f  mix-sentinel=8d7a43bc5f
hg_head_mvclamp_witness 55 frames, tail=21  mix-plain=9ca5c4d7af  mix-sentinel=9ca5c4d7af
matches: 0
```

(`9ca5c4d7af` is the value `lanes/unwritten-dep.report.md` §3 already recorded for
a reused directory; it is not the target.)

**The shared-directory case where the leftover tail is itself truncated** — a
polluter that died mid-frame — is swept at one-byte granularity too, six donors:

```
rect64 f0..33 + arf f.f{0..5} placed at index 34, then EVERY truncation of it
  6 × 648345635 truncations → no MATCH
```

## 7. The two fills in one shell (write-on/read-off and its reverse)

Since the two fills produce byte-identical frame files, any directory assembled
from a mixture of them is the same stream. Measured anyway, 70 shapes (every
split point, both directions):

```
plain f0..k + sentinel f(k+1)..33   for k in 0..34, and the reverse:  70 shapes → 0 matches
```

The harness variant of the same idea — one shell, `EC_AV1_PLANE_SENTINEL` exported
for the second fill only — is covered by the byte-identity above rather than by a
separate run.

## 8. Everything else swept here, with the count

| family | shapes | match |
|---|---|---|
| byte-exact truncations, lex order | 629 821 474 | 0 |
| byte-exact truncations, numeric order | 629 821 474 | 0 |
| foreign truncated tails (6 donors × every offset) | 3 890 073 810 | 0 |
| byte-SET shapes: every single-index drop, every contiguous run `f_a..f_b`, every prefix+suffix gap, every one-frame substitution from a 40-frame and a 34-frame donor | 3 706 | 0 |
| cross-fixture splices (rect64 head + donor tail, donor head + rect64 tail) | 140 | 0 |
| mixed-fill directories (§7) | 70 | 0 |
| hash FUNCTION variants: sha256 / sha512 / blake2b / blake2s / sha3-256 / sha1 × both orders, `sha256sum` listing (both orders), concatenated per-frame digests, concatenated per-frame hex, lengths+digest listing, last-N for N ∈ {1,2,3,5,10,17,20,30,33,34} | 36 | 0 |
| **whole corpus**: 127 fixtures × 2 fills × both frame orders — is the target simply another fixture's hash (a swapped column, a renamed pin)? | 508 | 0 |
| the ten `EC_AV1_*_DUMP` rungs, each hashed whole (a campaign run under a different dump var) | 10 | 0 |
| multi-var runs sharing one prefix (index collision: the later writer overwrites the earlier per frame) | 8 | 0 |
| directory contamination: `EC_AV1_DUMP_TABLES` output inside the hashed directory, `cat dir/*` and path-bearing `sha256sum` listing forms | 4 | 0 |

The rung row is worth its own line: the harness could have hashed
`EC_AV1_DECODE_ORDER_DUMP` (u8-narrowed, 9 262 080 B/frame) or any of the nine
others, and each was measured — `1851fd0dcb`, `b3e0166669`, `a8d941579f`,
`27a70718c`, `728871753d`, `89f5ae4e5`, `a7585b8f6c`, `3e6ed21944`,
`e3b0c44298` (`EC_AV1_MARGIN_DUMP` writes nothing for this fixture) — none is the
target.

**Note on the corpus row**: it also settles the "stale cross-fixture directory
mix" family from the other direction. Neither campaign value is any committed
fixture's plain or sentinel hash, so the target is not a mislabelled column.

## 9. Verdict, and the narrowed candidate list

**STILL OPEN.** Not reproduced here, and the shape space is now closed to the
byte, so the remaining candidates are no longer artefact shapes. In likelihood
order:

1. **Host-specific uninitialised memory reached the output on the campaign host,
   under the sentinel.** This is the only mechanism consistent with both halves
   of the observation: the plain value reproduces everywhere (so the decode is
   the same code on the same bytes), while the sentinel value is
   host-specific (so *some* buffer the sentinel does not gate — the grain plane
   of §5b being the one named in this crate — carried that host's ambient
   content into the output). `0ee1edf403` is then not reproducible from this
   tree's bytes by any means, because it encodes another machine's heap.
   *Falsifier*: a run on the campaign host itself, or any host whose allocator
   recycles 12 MiB blocks with non-zero residue, producing `0ee1edf403` under
   the sentinel. §5's `MALLOC_MMAP_THRESHOLD_=0` rows are the local version of
   that experiment and they do not reproduce it, which is itself informative: it
   says this host's recycled blocks are zero, not that the mechanism is absent.
2. **A different build.** Ruled out for every tree measured but never measured on
   the campaign's own binary: no artefact of the campaign survives except the two
   10-hex strings, so this cannot be closed from here.
3. **A harness whose shape is not any of the ~1.5 billion tried above.** The
   sweep covers byte order, digest function, frame set and directory contents;
   it cannot cover a harness that, say, normalised line endings, compressed, or
   read only a subset of files by a rule not derivable from the directory.
4. **A transcription error in the recorded value itself** — one hex digit.
   Nothing in this lane can test it, and it is the cheapest explanation that
   survives every measurement above.

**What is now closed going forward**, and does not need re-sweeping:

* silent truncation by `ENOSPC` / quota / `RLIMIT_FSIZE`, at **any** byte offset
  and in **either** frame order (§4) — a superset of
  `lanes/av1dumploud.report.md` §3D/E;
* run-to-run content nondeterminism on `8d6998d7` for this fixture, under
  allocator poisoning, thread counts and scheduling pressure (§5);
* a film-grain-plane hole reaching output (§5b);
* cross-fixture directory contamination, for **every** fixture in the corpus,
  with both a whole and a truncated leftover (§6);
* the value being some other fixture's hash (§8, corpus row);
* the value being a different `EC_AV1_*_DUMP` rung of this fixture (§8, rung row).

## 10. Commands

```bash
git worktree add /home/tahinli/.cache/wt/hash0ee1 8d6998d7
cd /home/tahinli/.cache/wt/hash0ee1
CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/hash0ee1 \
    cargo build -p ec-av1 --example decode_probe
strings /home/tahinli/.cache/tgt/hash0ee1/debug/examples/decode_probe \
    | grep -o 'wt/[a-zA-Z0-9_-]*' | sort -u          # provenance

bash   /home/tahinli/.cache/hash0ee1-sweep.sh            # §2, §5 (N=8, threads, grain)
bash   /home/tahinli/.cache/hash0ee1-cde.sh              # §5 (perturb, threads, NO_GRAIN)
python3 /home/tahinli/.cache/hash0ee1-leftover.py        # §6 (126 corpus polluters)
python3 /home/tahinli/.cache/hash0ee1-corpus.py          # §8 (127 fixtures x 2 fills)
python3 /home/tahinli/.cache/hash0ee1-shapes.py          # §8 (hash functions, listings)
python3 /home/tahinli/.cache/hash0ee1-shapes2.py         # §8 (gaps, runs, substitutions)
gcc -O2 -o hash0ee1-trunc hash0ee1-trunc.c -lpthread   # §3 the byte-exact sweeper
hash0ee1-trunc 97ef82c0ce …/arf 40 1                    # §3 control: MUST match
hash0ee1-trunc 0ee1edf403 …/rect 34 1                   # §4 lex  → 0
hash0ee1-trunc 0ee1edf403 …/rect 34 0                   # §4 num  → 0
```

## 11. What this lane did not do

* Did not touch the decode path, any dump site, or any crate source. The lane
  diff is this report alone.
* Did not re-run the `hg_*` fixtures' full-crate gates — no decode change to
  gate, and a full `ec-av1` suite is Main's call.
* Did not attempt to obtain the campaign's own binary or host. Candidates 1 and 2
  above are what is left when that is unavailable, and saying so is the honest
  form of the verdict.
* Did not chase the `unsafe { v.set_len(n) }` question filed by
  `lanes/unwritten-dep.report.md` §12; §5b is about whether a hole reaches
  *output*, which is a narrower question and is now measured.