# lane-av1oddluma — the odd-dimension regime: NARROWED, NOT FIXED

Base `5b27ed79`, branch `lane-av1oddluma`. **No code change.**

## Verdict, plainly

I did not fix `320x236` and `322x248`. I did establish they are **not** the
`tx_type` class I just fixed, that the divergence is **entropic** (not
reconstruction), that it forks at **coefficient read 6 of frame 0**, and exactly
what still has to be read to close it. "Narrowed, not fixed" is the honest
outcome here.

## 1. Red-before + control

Live comparator against the oracle's real `aomdec --rawvideo` bytes:

| cell | Y | U | V | exact frames |
|---|---|---|---|---|
| `320x236` | 234349 | 56957 | 52981 | 0/16 |
| `322x248` | 36286 | 13359 | 13001 | 0/16 |

**Control (`EC_FLIP`, one bit of the real oracle buffer):**
* `320x236`, byte 0 → `Y=234350` (**+1 Y**)
* `322x248`, byte 0 → `Y=36287` (**+1 Y**)

Per-frame counts are flat from frame 0 — **the KEYFRAME is already wrong** in both
(`320x236` f0: Y 11198 U 2416 V 2409; `322x248` f0: Y 2170 U 814 V 775). So no
propagation, no motion compensation, no loop filter: this is keyframe parse or
keyframe reconstruction.

Correction to the framing I was given: 234349 is **not** near whole-frame. Luma is
74240 samples/frame, so `320x236` f0 is 11198/74240 ≈ **15%**, `322x248` f0 is
2170/79360 ≈ **2.7%**. Not a whole-frame fork.

## 2. Entropic, and where

`EC_ECDUMP_IN` on both sides, paired by `(value, range)` (our `bit` field runs a
constant +15 against the oracle's and is not comparable; that offset holds for
reads 0–5 and then breaks, which is itself the signal).

Reads 0–5 are **identical**. Read 6:

| | side/tx | mi | ctx | value | range |
|---|---|---|---|---|---|
| ours | 4 | (not printed) | 2 | 44513 | 48640 |
| oracle | TX_8X4 (`tx=6`) | plane 0 (0,4) bc=0 br=0 | 1 | 40060 | 51808 |

Bit positions at read 6: ours 1010, oracle 683 (against a +15 convention offset at
read 5), i.e. **312 extra bits consumed inside one unit**. So the fork is inside
the unit that starts at read 6 — the first `TX_8X4` luma unit of the 8×8 block at
mi (0,4) — and it is a **parse** divergence, not a reconstruction one.

## 3. What is NOT wrong (measured, so nobody re-checks it)

The seven units before the fork are byte-for-byte identical, including everything
that would be the usual suspect:

* **Block walk order** matches: the 16×16 block at mi(0,0) → its U → its V → the
  next block at mi(0,4).
* **`all_zero`**: 1,1,1,0,0,0 on both.
* **`eob`**: 3, 50, 50 on both (`TRACE eob value=3/50/50` vs
  `tag=eob eob=3/50/50`).
* **Coefficient positions**: oracle `c=49→pos=31, c=48→23, c=47→30, c=46→37,
  c=45→44, c=44→51, c=43→58, c=42→57, c=41→50, c=40→43, c=39→36, c=38→29`;
  ours `59,58,51,44,37,30,23,15,22,29,36,43,50,57`. **The oracle's positions
  transposed (`(p % 8) * 8 + p / 8`) are exactly ours.** Our `default_scan_gen(8)`
  is the documented correct row-major transcription of libaom's column-major
  `default_scan_8x8` (`decode.rs:7644`: *"transposed from libaom's column-major
  buffer into this decoder's row-major `pos = row * side + col`"*). **The scan and
  the placement are right.**
* **Contexts and levels**: `c=48 ctx=22`, `c=46 ctx=21`, `c=42 ctx=21`,
  `c=38 ctx=22`, `c=49 level=1`, `c=47 level=1` — identical on both sides.

So it is **not** a transposed scan, **not** a wrong CDF row, **not** a wrong
`eob`, **not** a block-order fork.

## 4. The one open measurement, named

Reads 0–5 agree and the `base` levels for the read-5 (V-plane 8×8) unit agree, but
the `EC_ECDUMP_IN` states diverge by read 6. Two candidate sites, in order:

1. **The sign / Golomb tail of the V-plane 8×8 unit** (`tag=sign` /
   `tag=post_golomb` on the oracle, `TRACE dc_sign` / `TRACE golomb` on ours).
   Both read `eob=50`; an `eob` that high with mostly-zero bases means the tail
   carries almost all the bits, and 312 bits of disagreement is tail-shaped.
   **Deciding measurement:** run `EC_TRACE_COEFF` (oracle) and `EC_AV1_TRACE`
   (ours) and diff the `sign`/`post_golomb` lines for that unit. If the first
   differing `c` is a Golomb-bits read, the bug is in the **b**-plane Golomb
   context, not in any scan or table.
2. **A unit-count difference.** We read **3138** `EC_ECDUMP_IN` units against the
   oracle's **4095** for the same 16 frames. Since frame 0 already forks at read 6,
   this is a *consequence*, not a cause — but once (1) is ruled out, the next
   check is which unit we skip or add.

The oracle rung that settles it, once the tail is diffed:
`EC_COEFF_STEP tag=post_golomb c=<k> level=<n> rng=<r>` versus our
`TRACE golomb` at the same scan index. `EC_COEFF_STEP tag=sign c=<k> sign=<s>
dcctx=<d>` versus our `TRACE dc_sign ctx=<d> value=<s>` — our `dcctx` convention
also carries an offset (7 for chroma, per `read_plane`'s own comment), so compare
the *sign values* first and the ctx second.

## 5. Sweep — and a trigger rule I must NOT state

The 27-cell grid (widths 320/322/324 × heights 224…256) still has **25/27 exact**
after the `tx_type` fix, with `320x236` and `322x248` the two failures. An
extended grid (heights 260/264/268/272, widths 318/326/330) was encoded this
session; see `not_done` for why I am not quoting its numbers.

**The obvious rule — "a dimension that is not a multiple of 8" — is FALSE and I am
not proposing it.** 324 is 4 mod 8 and `324x240` is exact; 236 is 4 mod 8 and
`320x236` fails; 322 is 2 mod 8 and `322x232` is exact. There is no consistent
geometric split in the data I have. The honest statement is that the trigger is
**content**: a unit whose sign/Golomb tail is reached. Both failures are large
(eob=50 in a chroma unit is unusual), which fits — sparse content triggers it,
exactly as the `tx_type` bug did.

## 6. `not_done`

* **Not fixed.** No decoder change. The tree is byte-identical to `5b27ed79`.
* **The extended grid's numbers are not quoted.** The encodes completed but I ran
  out of budget to sweep them; quoting unmeasured cells would be exactly the
  sin this wave has been about. Whoever picks this up should run them first — the
  fixtures are `/tmp/h232fix/o{318,326,330}x{232,236,240,248}.obu` and
  `/tmp/h232fix/o{320,322,324}x{260,264,268,272}.obu` with the recipe in
  `lanes/av1422h232.report.md`.
* **The trigger rule is not established**, only bounded (2 of 27 cells, both
  content-shaped). §5 says why the geometric hypothesis fails.
* **Not the `tx_type` bug.** Named separately in
  `lanes/av1422h232.report.md` §3 and measured apart here: this regime carries
  **luma** errors, which a chroma `tx_type` change cannot produce.
* **4:2:2 not re-measured** (needs `EC_AV1_ALLOW_422_PROBE`, not committable).
* **Full suite not run** (lane rules: scoped only).
