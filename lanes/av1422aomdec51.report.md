# lane-av1422aomdec51 — the 51-cell 4:2:2 census, re-measured in decode order against aomdec

**Outcome in one line:** the missing half of `lanes/av1bookkeep.report.md` §2.4 is now
measured — the same 51 cells, the same bytes, decoded on current main `c38dd788` and
compared against the instrumented aomdec in **decode order**: **51/51 byte-exact**
(869 decode-order frames), 0 diverging, 0 comparator-error, with the liveness control
measured on the same compare path.

Branch `lane/av1422aomdec51`, base `c38dd788` (= main when this lane started; re-checked
before commit), one commit (this report only), no source change, no push, no merge.
No private build was needed beyond the read-only example binary; worktree
`/home/tahinli/.cache/wt/av1422aomdec51`, target dir `/home/tahinli/.cache/tgt-av1422aomdec51`.
Run executed on `tCloud@178.105.165.182` (idle; not on 2.28.124.204, not on 2.28.112.3).

---

## 1. The instrument, and why it is the missing half

`av1bookkeep` measured the census with `dump_yuv` + **ffmpeg** in **display** order and
stated the limit itself: "the aomdec decode-order census is a different instrument and
was not re-run". This lane ran that other instrument:

| side | decoder | dump | order |
|---|---|---|---|
| ours | `dump_yuv` example on main (`decode_stream`) | `EC_AV1_FINAL_DUMP` (env-set), depth-correct u8/u16-LE, one rung per **coded** frame (`<prefix>.f<decode_idx>`) | decode order, hidden frames included |
| oracle | instrumented `~/.cache/aom-oracle/build/aomdec` (confirmed present before the run) | rung 12 `EC_AV1_FINAL_DUMP`, same layout | decode order, hidden frames included |

* `EC_AV1_FINAL_DUMP` fires inside `decode_frame` for every **coded** frame; a
  `show_existing_frame` output re-shows a stored picture and never reaches it, so
  neither side writes a rung for it — the accounting is identical by construction, and
  hidden alt-refs ARE compared (the frames ffmpeg never emits).
* Depth: both sides pack 8-bit as u8 and 10/12-bit as u16 LE (rung 12 is the
  depth-correct one; the u8-narrowing `EC_AV1_DECODE_ORDER_DUMP` was deliberately NOT
  used — 10 of the 51 cells are 10-bit, and that pairing is the all-HBD-red class).
* Per-frame compare asserts `len(ours) == len(oracle)` BEFORE any sample is examined;
  a truncating compare is a prefix compare.
* Compare pairing is decode index to decode index throughout.

## 2. The 51 cells

The census's own selection, not a re-derivation: `~/.cache/census422b/cells_ff.json`
(95 cells) filtered by `formats.json`'s per-stream 4:2:2 sequence-header probe = **51
cells**. This is the list `av1bookkeep` §2.4 quotes.

* 51/51 paths resolved; every byte re-hashed locally and again on the remote against
  the census's recorded `sha256` before decoding. 9 cells' recorded paths pointed at the
  pruned `av1422ffmpegbase` worktree; they re-resolved to `crates/ec-av1/fixtures/` in
  the primary checkout **byte-identically** (same sha256).
* Composition: 18 fresh-sweep `s422_*` (12 × 8-bit, 6 × 10-bit), 9 COMMITTED-PIN
  fixtures, 23 probe-cache cells, 1 fresh-reproducer (`R422_320x242`); 40 × 8-bit +
  11 × 10-bit in total.

## 3. Result

| verdict | cells |
|---|---|
| EXACT | **51** |
| DIVERGING | 0 |
| COMPARATOR-ERROR | 0 |

869 decode-order frames compared in total (1–43 per cell; the 43-frame cells are the
altref structure AA/AB/AD). Every frame length-asserted; every frame byte-compared.
Per-cell frame counts and sha256 prefixes are in `results.json`
(remote `~/census422run/results.json`, mirrored in the commit).

**Is this the bookkeep 51?** Yes — same `cells_ff.json` × same `formats.json` 4:2:2
filter, same recorded shas, no cell invented, none dropped.

## 4. Liveness control (the zero is a measurement)

`census.compare_only(name)` compares the dumps on disk **without re-decoding** — the
control's path and the census's compare are the same code (`compare_dumps`). Two flips,
both in the ORACLE's dump, both re-compared and then restored and re-compared:

| control | flip | verdict | wrong bytes | plane y/uv | offset | restored |
|---|---|---|---|---|---|---|
| `s422_384x240` f0 (8-bit) | 1 sample, Y plane middle, low bit | DIVERGING | **1** | 1 / 0 | 61440 | EXACT |
| `Y_intrabc_10b` f0 (10-bit) | 1 low bit of one u16 LE sample | DIVERGING | **1** | 1 / 0 | 102400 | EXACT |

Each flip moves the count by exactly 1, lands on the exact offset injected, restores to
EXACT. The 10-bit control also proves the compare is not an 8-bit narrowing hiding a
1-LSB fork (u16 LE bytes are compared, not narrowed).

Two defects this control caught in the harness itself, before any verdict was quoted:

1. **The first "control" was false.** `run_cell` re-decodes, so it OVERWROTE the
   injected flip before comparing and reported EXACT — a control that cannot bite.
   Fixed by extracting `compare_only`/`compare_dumps`; the census verdicts and the
   control now share one compare loop.
2. **The plane split was wrong.** First control reported the Y-plane flip (offset
   61440 < 92160 = ⅔ of a 4:2:2 8-bit frame) as `wrong_uv=1`: the code assumed
   Y = ⅓ of bytes (the 4:2:0/4:4:4 habit). For 4:2:2, Y = W·H of 1.5·W·H samples =
   **⅔ of the bytes**. Fixed and re-proven by the same flip: `wrong_y=1, wrong_uv=0`
   at the injected offset.

After both fixes the full 51-cell census was re-run end to end through the corrected
path (`census2.log`; the quoted numbers are from that run), and the controls were
re-run against it.

## 5. What this does and does not establish

* Established: every one of the 51 4:2:2 cells decodes **byte-exact against the
  instrumented aomdec, in decode order, including hidden frames and all 10-bit cells**,
  on main `c38dd788`, with a comparator proven live.
* Not the same instrument as `av1bookkeep`'s 51/51 (ffmpeg, display order): the two
  agree, which is exactly what §2.4's "different instrument" caveat left unmeasured.
* Not established: cells outside the 4:2:2 51 (the census's 44 4:2:0/4:4:4 controls were
  out of scope); film-grain synthesis output (grain is applied by both decoders'
  normal path but this census does not isolate it); 12-bit (no cell in the census).
* No decoder edit: the only tree artifact is this report; `dump_yuv` was built from the
  branch as a read-only example binary.

## 6. Verification run on this tree

| check | result |
|---|---|
| worktree base | `c38dd788` = main at start (re-checked at commit time) |
| `CARGO_TARGET_DIR=… cargo build --release -p ec-av1 --example dump_yuv` | ok (one pre-existing `ec-av1` lib warning, untouched) |
| 51-cell sha verification, local + remote, before decode | 51/51 match census records |
| census (decode order vs aomdec rung 12) | 51/51 EXACT, 869 frames, 0 errors |
| liveness: 1-sample flip, 8-bit cell | count moves by exactly 1, offset lands, restore EXACT |
| liveness: 1-LSB u16 flip, 10-bit cell | count moves by exactly 1, offset lands, restore EXACT |
| `git status` at commit | only this report |

Remote artifacts (host `178.105.165.182`, `~/census422run/`): `census.py` (driver),
`census.log`/`census2.log` (run logs), `results.json` (per-cell rows), `out/<cell>/`
(both sides' per-frame dumps, kept for reproduction).
