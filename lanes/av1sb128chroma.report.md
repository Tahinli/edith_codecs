# lane-av1sb128chroma — the ticket's `cu_tx = 64` repair is REFUTED; the arm is pinned and gated

Base: `main` `57834ee2` ("Merge lane-av1chromarect-r2 …"). Branch
`lane-av1sb128chroma`, worktree `/home/tahinli/.cache/wt/av1sb128chroma`.
Oracle `~/.cache/aom-oracle` (aom `v3.13.3-7-g9bb526a`, instrumented), read-only.

**Verdict: no source fix — there is nothing to fix.** The site named by the
ticket is *already* the repaired form, and the repair itself is a hard panic
plus a 45 900-sample pixel divergence when applied. What the ticket's premise
was right about is the *fixture* gap: this arm had no committed bytes, so I
pinned the witness, added the gate, and closed the ticket on evidence.

---

## 0. The prerequisite is on `main` — here is the commit

The ticket's stated unblock was `lane-av1444rect` `f92776ba` (the H1 fix).

```
$ git merge-base --is-ancestor f92776ba main && echo YES
YES
$ git show -s --format='%H %ad %s' f92776ba
f92776bab2442319e32d4f56844af6956e2fc18b  Mon Sep 28 21:32:43 2026 +0300
  ec-av1: lane-av1444rect -- a 1:4 inter strip's chroma context was gathered
  at the 4:2:0 pair extent at ss (0,0), forking the coder (H1)
```

Confirmed on `main`. H1 is the `decode_inter_block` `around_c` context
geometry, a **different arm** from the one under test (see §2).

---

## 1. The site, re-located

The ticket says "around 41930, re-locate with grep". Line numbers have drifted.
The `EC_TMP_SB128CHROMA` counter that report line names is `lanes/av1tilerows.report.md:192`;
grepping the crate for it finds nothing (it was removed), and its successor
counter is `INTRA_128_IN_INTER_MU_CHROMA_HITS` (decode.rs:5591, hit at 43576).

The arm is the **`side > 64` mu-chunk chroma walk of the INTRA-in-inter 128
root**, `decode.rs:43568-43772`, inside `decode_inter_block`. Its three
claimed defects are all present in the *repaired* form:

| line | ticket says it is | actually is |
|---|---|---|
| 43592 | `cu_tx = 32` is the 4:2:0 answer, must be `64` at ss (0,0) | `cu_tx = 32` — **correct at every subsampling** (§3) |
| 43599-43600 | `luma_span = cu_tx * 2` is the 4:2:0 answer | `cu_tx << ss_x(fctx)` — already ss-aware |
| 43667-43670 | the origin is per-chunk, should be `cpx + cc * 64` | `cpx + cc * chunk_chroma_w + uc * cu_tx` — per-chunk **and** per-unit |

`git log -L 43568,43620:crates/ec-av1/src/decode.rs` names the commit that put
it there: **`c0727d64` "lanes/av1txsizeaudit r5: re-re-base onto ec4e9528,
declare provenance of the foreign work"**, which is
`git merge-base --is-ancestor c0727d64 main` → **ON MAIN**. That same commit
added the live-encode gate
`a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact`
(stream.rs:37893) with the counter-argument already written out at
stream.rs:37883-37891.

---

## 2. Which arm the ticket's `20 fires` was counting — NOT this one

The ticket's evidence is a 4:4:4 lossy `--sb-size=128 --cq-level=20` stream
with "20 fires at ss=(0,0) side=128". I encoded exactly that and measured both
sets of counters on it (throwaway probe, since removed):

```
$ aomenc --codec=av1 --profile=1 --passes=1 --end-usage=q --cq-level=20 \
    --cpu-used=2 --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 \
    --limit=24 --sb-size=128 --obu -o ts444_cq20.obu testsrc2_128x96.y4m
sha256 84de42800452d69f5db8dfec7c538ef34d66b26dce7eb428c1960f80df3a103a, 31602 B, 24 frames
header: (sb128=true, ss_x=0, ss_y=0, mono=false)   -> 4:4:4 confirmed

frames oracle=24 ours=24
intra_128_in_inter=0   mu_chroma=0   chroma_split_tx=0     <-- the arm NEVER runs
per-plane wrong Y=0 U=0 V=0        per-frame wrong [0,0,... x24]
```

**Zero fires, not 20.** On clean `testsrc2` the encoder's RD never picks INTRA
over inter for a whole 128 root, so this recipe cannot reach the arm at all —
and `lanes/av1txsizeaudit`'s author already measured the same thing
("measured over 24 recipe/cell combinations, three of which fired and none of
them `testsrc2`", stream.rs:37863-37865). The 20 was a count of the *other*
`side > 64` chroma walk (the INTER twin, `chroma_split_tx_hits`), or a count
inflated under the now-removed masking fork; either way it is not this arm.
The ticket's own instruction — "re-measure rather than adopt either number" —
is what produced 0.

So the ticket's proposed evidence stream is **vacuous for the arm it names**.
That is the finding.

---

## 3. `av1_get_max_uv_txsize(BLOCK_128X128, 0, 0)` is `TX_32X32`, not `TX_64X64`

The ticket's central "known fact" is the whole basis for `cu_tx = 64`. It is
false. Read from the oracle tree:

```c
/* av1/common/blockd.h:1361-1370 */
static inline TX_SIZE av1_get_adjusted_tx_size(TX_SIZE tx_size) {
  switch (tx_size) {
    case TX_64X64:
    case TX_64X32:
    case TX_32X64: return TX_32X32;      /* <-- unconditional 64 -> 32 */
    case TX_64X16: return TX_32X16;
    case TX_16X64: return TX_16X32;
    default: return tx_size;
  }
}

/* av1/common/blockd.h:1372-1379 */
static inline TX_SIZE av1_get_max_uv_txsize(BLOCK_SIZE bsize, int subsampling_x,
                                            int subsampling_y) {
  const BLOCK_SIZE plane_bsize = get_plane_block_size(bsize, subsampling_x, subsampling_y);
  const TX_SIZE uv_tx = max_txsize_rect_lookup[plane_bsize];
  return av1_get_adjusted_tx_size(uv_tx);
}
```

`av1_get_adjusted_tx_size` takes **no subsampling argument**. At ss (0,0),
`get_plane_block_size(BLOCK_128X128, 0, 0) == BLOCK_128X128` and
`max_txsize_rect_lookup[BLOCK_128X128] == TX_64X64`, so the answer is
`adjusted(TX_64X64) == TX_32X32`. The 64→32 step fires **at ss (0,0) too**,
which is exactly the point the ticket's derivation misses.
`lanes/av1txsizeaudit.report.md` §0.1 measured the same table with a C probe
linked against the oracle's own `libaom.a`
(`DIRECT bsize=BLOCK_128X128 ss=00 … adjusted=3 (32x32)`), and §0.2 measured
it at runtime from the oracle's `EC_TRACE_COEFF` on a 4:4:4 `--sb-size=128`
stream: **chroma reads 0 units of any 64-axis shape, 49 units of `tx=3`
(TX_32X32)**.

Two independent confirmations from my own runs below: `cu_tx = 64` makes this
crate **panic on a 64-point scan table that libaom does not have**
(§5, mutation A), and the arm's own live-encode gate measured
`mu_chunk_chroma_reads = 4` on the firing witness — four TX_32X32 units, not
one TX_64X64.

---

## 4. The witness (pinned) and the counter

**Fixture** `crates/ec-av1/fixtures/444_intra_in_inter_128root_mu_chroma.obu`
— sha256 `f5999bd996fef820472dc7846e42cad3e62bd1f816fafd7f218e34b77786394a`,
3570 bytes, 3 decode-order frames, fnv1a64 `0xe455_060d_f685_86dd`.

**Recipe**, re-run and `cmp`-confirmed byte-reproducible:

```text
ffmpeg -v error -f lavfi -i \
  "gradients=size=128x256:c0=0xa1128f:c1=0xd98c48:c2=0x120601:c3=0x4abfba:\
seed=63:duration=0.12:rate=25,noise=all_seed=63:alls=6:allf=t" \
  -pix_fmt yuv444p -t 0.12 -f yuv4mpegpipe - > in.y4m

aomenc --codec=av1 --profile=1 --passes=1 --end-usage=q --cq-level=62 \
  --cpu-used=0 --threads=1 --row-mt=0 --sb-size=128 --limit=3 \
  --max-partition-size=128 --min-partition-size=64 \
  --enable-rect-partitions=0 --enable-ab-partitions=0 \
  --enable-1to4-partitions=0 --enable-palette=0 --enable-intrabc=0 \
  --deltaq-mode=0 --enable-tx-size-search=0 --obu -o out.obu in.y4m
```

`duration=0.12:rate=25` and `--limit=3` are load-bearing (the `gradients` /
`noise` patterns are time-parameterised, and at `--limit=2` the encoder stops
choosing the shape at all).

**Counter, quoted** — the arm fires on the pinned witness:

```
$ cargo test -p ec-av1 --lib -- a_444_intra_in_inter_128root_muchroma_pinned --nocapture
a_444_intra_in_inter_128root_muchroma_pinned_stream_decodes_pixel_exact:
  3 decode-order frame(s) pixel-exact (0 hidden),
  intra_128_in_inter=1 mu_chunk_chroma_reads=4
a_444_intra_in_inter_128root_muchroma_pinned_stream_decodes_pixel_exact:
  control 24 frame(s) pixel-exact (0 hidden)
test result: ok. 1 passed; 0 failed
```

`intra_128_in_inter_hits = 1` (one intra-coded 128x128 root inside an inter
frame) and `intra_128_in_inter_mu_chroma_hits = 4` (mu-chunk chroma reads:
a 4:4:4 64x64 mu chunk is FOUR TX_32X32 units per plane, and the walk
`continue`s after the first chunk's arms). The gate asserts `roots >= 1` and
`units >= 4`, so it is non-vacuous by construction. The pre-existing
live-encode gate independently reports the same `1` / `4` on the same recipe,
so the pin matches it byte-for-byte.

**Control, also pinned** —
`crates/ec-av1/fixtures/444_sb128cq20_tsrc2_control.obu`, sha256
`84de42800452d69f5db8dfec7c538ef34d66b26dce7eb428c1960f80df3a103a`, 31602 B,
24 frames, fnv1a64 `0xc542_f45f_f3a5_d494`. This is the ticket's own recipe
(§2). The gate asserts it decodes 24 frames byte-exact **and** reads
`(0, 0)` on both arm counters, which is what pins the refutation in-crate
with no encoder on the host.

---

## 5. Red / green

**GREEN, unpatched `main` `57834ee2`:** all 3 decode-order frames of the
pinned witness byte-exact vs the oracle `aomdec`; per-plane wrong
**Y 0, U 0, V 0**; 24/24 frames of the control byte-exact.

There is no red-before *of the site*, because the site is already correct.
The red is therefore measured on the **ticket's proposed change**, applied to
the site and then reverted. Two directions, both of which make the new gate
fail:

### Mutation A — the ticket's full change (`cu_tx = 64` at ss (0,0),
plus `unit_luma_w/h = cu_tx * 2`, plus `(cpx + cc * 64, cpy + cr * 64)`)

```
thread 'stream::tests::a_444_intra_in_inter_128root_muchroma_pinned_stream_decodes_pixel_exact'
panicked at crates/ec-av1/src/decode.rs:7197:5:
no default scan for a 64-point transform
test result: FAILED. 0 passed; 1 failed
```

Not a pixel divergence — a **hard panic**. libaom's `av1_scan_orders` has no
64-point chroma table (a 64-axis transform codes only its 32x32 corner;
scan.c aliases TX_64X64 to `default_scan_32x32`), so asking for one asserts.
This is the decisive single measurement: the ticket's answer does not decode.

### Mutation B — the ticket's luma-span and per-chunk-origin halves only
(`cu_tx` left at 32, so the mutation reaches the pixels instead of the assert)

```
thread '…muchroma_pinned_stream_decodes_pixel_exact' panicked at
  crates/ec-av1/src/stream.rs:9767:17:
  decode-order frame 2 of 3 (3 shown, 0 hidden) differs from the oracle at
  byte 15450 (ours 142 vs 141), 45900 bytes differ
test result: FAILED. 0 passed; 1 failed
```

Per-plane wrong samples over the 3 frames (98 304 per plane), from the
throwaway probe that measured them plane-by-plane:

| plane | wrong samples | first differing sample | ours | oracle |
|---|---|---|---|---|
| Y | 12 537 | (x=90, y=120) | 142 | 141 |
| U | 16 696 | (x=65, y=120) | 153 | 152 |
| V | 16 667 | (x=64, y=120) | 145 | 146 |

Per-frame wrong: `[0, 0, 45900]` — all of it on decode-order frame 2, and the
arm's counters still read `intra_128_in_inter=1`,
`mu_chunk_chroma_reads=4`, so the damage is on **pixels**, not on the counter
(class `gate-blind-to-feature`, avoided).

**GREEN after revert:** both mutations reverted; `git diff crates/ec-av1/src/decode.rs`
is **empty** — the decoder is byte-identical to `main`. The only source
change in this lane is the new test in `stream.rs` plus the two pinned
fixtures.

---

## 6. Gates re-run

| scope | command | result |
|---|---|---|
| the new pinned gate | `cargo test -p ec-av1 --lib -- a_444_intra_in_inter_128root_muchroma_pinned` | **1 passed / 0 failed** (witness 1 root / 4 mu-chunk chroma reads, 3/3 frames byte-exact; control 24/24 byte-exact, 0 fires) |
| the pre-existing live-encode twin | `… -- a_real_aomenc_444_intra_in_inter_128_root` | **passed**, same `1` / `4` |
| every 4:4:4 and 4:2:0 gate in the crate (44) | `cargo test -p ec-av1 --lib -- <44 names matching 444\|420>` | **44 passed / 0 failed** |
| the 128-root / lossless families (66 names) | `cargo test -p ec-av1 --lib -- <66 names matching 128\|lossless\|ll444\|mu_chunk>` | **55 passed / 0 failed / 11 ignored** — the 11 are pre-existing `#[ignore]`d probes, none of them this lane's gate |
| compile | `cargo check -p ec-av1 --all-targets` | **clean**, no warnings |

No full suite, no formatters — sibling lanes are live in this box.

---

## 7. What is left for whoever picks this up

Nothing on the source side. The arm is correct, gated, and now pinned. The one
thing this lane could not close, and hands on unchanged:

- **`decode.rs:43568`'s `around_mi` is the square-gather form.** The inter
  twins at 40904/43661 already use `around_mi_rect(unit_mi, unit_luma_w,
  unit_luma_h)`; this arm passes one `side`. With `w == h` the two are
  byte-identical, and `unit_luma_w != unit_luma_h` only at 4:2:2, which the
  sequence header refuses by name — so it is **unreachable**, not a defect.
  Left as is; `lanes/av1txsizeaudit.report.md` §1.4 names it as its own
  (already-known) row.
