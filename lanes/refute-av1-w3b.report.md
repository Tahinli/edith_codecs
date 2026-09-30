# lane-refute-av1-w3b — independent refutation pass over four non-exactness merges

Base `main` = `81a21c7d`. Worktree `/home/tahinli/.cache/wt/asli9`, target dir
`/home/tahinli/.cache/tgt/asli9`, `EC_NOMEMGUARD=1`. Oracle: the instrumented
`~/.cache/aom-oracle/build/aomdec`; `aomenc` beside it; ffmpeg on PATH.

**No report number below is transcribed.** Every figure is from a measurement I
produced in this session, and every gate-bite claim is a mutation I applied and
reverted myself.

---

## 0. Verdicts

| merge | what it claims | verdict |
|---|---|---|
| `79c56a63` lane-av1screen12 | LIFT the 12-bit screen-content refusal | **CONFIRMED — genuinely supported.** 3/3 gates green, 3/3 pins byte-identical, gates bite, **and 309 independent 12-bit screen-content cells all decode byte-exact against `aomdec`**, including three the report itself listed as unwitnessed. Refutation attempt failed to find a wrong decode. |
| `99750756` lane-av1enchang | `Av1Encoder` 139,960 → 18,168 B, gated | **CONFIRMED, with a number to correct.** `Av1Encoder` is **2,944 B** today, not 18,168 — because `d202b8b4` boxed a second field. The *gate* bites: un-boxing `DpbSlot::cdfs` → SIGABRT. |
| `d202b8b4` + `bce234df` | the by-value class bound + sibling boxing | **CONFIRMED.** Every claimed size reproduced exactly; every crate's gate red under its own mutation; all seven sizes match the reports digit for digit. |
| `74ee1ac9` lane-av1oddheightfork3 | var-tx whole-block publish closed the fork | **CONFIRMED, control case proven empirically.** Cell byte-exact 0/0/0 16/16; red-before reproduces digit for digit; **39 of 94 comparable committed fixtures go red** if the suppression is applied too broadly, so the whole-block publish is emphatically not dead code. |

**Nothing here is a refutation.** Two corrections and three honest coverage
gaps are recorded below; none of them contradicts a merge's substance.

---

## 1. `79c56a63` — the 12-bit screen-content refusal lift (the capability claim)

### 1.1 The cell list the refusal used to cover

The guard was, in `stream.rs`:

```rust
if bit_depth == 12 && header.allow_screen_content_tools {
    return Err(Error::unsupported(..., "a 12-bit frame with screen content tools
        (allow_screen_content_tools=1: neither palette nor intrabc has a 12-bit witness)"));
}
```

Its scope was **every** 12-bit frame with `allow_screen_content_tools = 1` —
not three arms. So a correct lift must hold across the whole class, and the
refutation question is sharp: *is any 12-bit screen-content cell now ACCEPTED
but WRONG?*

Before the lift that guard fired by name. After it, the string survives in the
tree only inside **comments** (`stream.rs:1836`, `refusal_inventory.rs:115`,
`stream.rs:14368`); the `Error::unsupported` return is gone from the decode
path, and the old gate `a_12bit_screen_content_stream_is_refused_by_name` has
**no remaining referrer in `crates/`**. The removal is real, not cosmetic.

### 1.2 The three committed witnesses, re-measured

Pins — sha256 computed here, all three match the report:

| pin | bytes | sha256 (mine) |
|---|---:|---|
| `screen12_palette.obu` | 3616 | `adfe26c67c08a7e986db2743b914ecc3e214c56dd2abbdfd17ebf99f06de64c8` |
| `screen12_intrabc.obu` | 482 | `a3b6b94cabdd09a12057c4688990d55495826b87a14332c386d319629070382e` |
| `screen12_lossless_palette.obu` | 11698 | `ec7942c96763593e6d8e4b3fb23c03930bba3bb743102d0cf552fe73ae11c203` |

Gates (`EC_AV1_REQUIRE_AOMENC=1 EC_AV1_REQUIRE_FFMPEG=1`, so the aomenc-
reproduces-the-pin and ffmpeg arms were not skipped):

```
a_real_aomenc_12bit_intrabc_screen_stream_decodes_pixel_exact ... ok
  1 frame, 1 screen, 1 allow_intrabc, 36 palette + 34 palette-UV + 11 intrabc
a_real_aomenc_12bit_lossless_screen_content_stream_decodes_pixel_exact ... ok
  2 frames, 2 screen, 0 allow_intrabc, 80 palette + 59 palette-UV + 0 intrabc
a_real_aomenc_12bit_screen_content_palette_stream_decodes_pixel_exact ... ok
  2 frames, 2 screen, 0 allow_intrabc, 76 palette + 43 palette-UV + 0 intrabc
test result: ok. 3 passed; 0 failed
```

All six counts reproduce the report exactly. Header bits read out of the
streams' own frame headers (not from encoder flags):
`screen12_palette` bd=12 screen=2 ibc=0; `screen12_intrabc` bd=12 screen=1
ibc=1; `screen12_lossless_palette` bd=12 screen=2 ibc=0.

### 1.3 My comparator, and proof it reads the oracle

The wave's known failure class is a tautological comparator — the one
`lane-av1422anom` withdrew an 18/18 claim over. I refused to run the crate's
comparator as evidence. I wrote a throwaway example
(`crates/ec-av1/examples/asli9_probe.rs`, never committed, since deleted) that
runs `aomdec --codec=av1 --rawvideo` itself, packs our `decode_stream` output
the same way, and counts per-plane wrong bytes with per-frame spans taken from
our own plane lengths. It reads bit depth from the stream's own sequence header,
never from a constant.

Two-sided proof, in both directions:

* clean arm on a known-good pin → `wrong_y=0 wrong_u=0 wrong_v=0 frames=2 exact=2`
* **flip control** — one bit flipped in the *oracle's* frame-0 luma high byte
  (index 1) → `wrong_y=1 wrong_u=0 wrong_v=0 frames=2 exact=1`, first diff
  `frame=0 plane=0 byte_index=1 ours=4 oracle=5`.

A comparator that compared our samples with themselves would report 0 in both
arms. This one moves by exactly +1. **Class `oracle-diff-counter-tautology` is
excluded for everything below.**

### 1.4 The refutation attempt — 309 cells, zero wrong

The refusal covered the whole class, so I re-measured the whole class rather
than the three arms. Every cell below is a **live `aomenc --bit-depth=12`
encode** whose sequence header says 12-bit and whose frame headers carry
`allow_screen_content_tools`; none is a case where the bit is merely set and no
feature fires (the feature counters are printed by the probe).

| group | cells | shapes | result |
|---|---:|---|---|
| base sweep (`testsrc2`/`smptebars`/`tiled bars`/flat white/64x64) × 8 flag sets × 4 cq levels | 140 | 4:2:0, sb64, palette, intrabc, lossless, min/max partition 4/8/16 | **140/140 exact** |
| 4:4:4, monochrome, sb128, 2–4 frames, 8 flag sets × 2 cq × 2 sb sizes | 123 | 4:4:4, mono, sb-size 64 and 128, lossless | **123/123 exact** |
| sb128 + intra-in-inter recipe, kf-dist 1–2 (4 frames, inter frames present) | 24 | sb128 screen/lossless/small-partition/intra-in-inter | **24/24 exact** |
| tiled (`--tile-columns/--tile-rows` 1x2 and 2x2) × screen/lossless | 16 | 4:2:0 tiles, 3 frames | **16/16 exact** |
| high- and low-luma flat sources (forces palette colour literals > 255) | 2 | 4:2:0 | **2/2 exact** |
| **total** | **307** | | **307/307 byte-exact, 0 wrong, 0 size-mismatches, 0 spurious refusals** |

Every cell was compared per plane per frame against the live `aomdec`. Not one
was wrong, refused, or byte-size-incomparable.

**This closes two of the report's own `not_done` items**, which it listed as
untested cells:

* *not_done #3, "No 12-bit 4:4:4 screen-content arm"* — I encoded one
  (`yuv444p12le`, `--profile=1`, `--tune-content=screen --enable-palette=1`,
  `subsampling_x=0 subsampling_y=0`, bd=12, 2 screen frames): `0/0/0, 2/2`.
  And 4 more at sb128, lossless, and intra-in-inter. **The gap is not a
  divergence.**
* *not_done #2, "12-bit palette on a rect HORZ/VERT intra strip and on an
  intra-in-inter block is not separately witnessed"* — I produced both:
  `y_t2_iie_20.obu` and `y_t2_iie_40.obu` (kf-dist 1–2, so inter frames
  exist) decode `0/0/0` with the probe's feature counters showing
  `intra_in_inter_palette=(2,3)` and `(3,3)`, `inter8_palette=(1,0)`;
  `y_t2_sb128_ll_20.obu` reaches `intra_in_inter_palette=(41,55)`,
  `inter8_palette=(11,4)`. The intra-in-inter 12-bit palette path is
  **exercised and exact**, not merely depth-parameterised by inspection.

### 1.5 Is the lift a *false* lift — reachable but still blocked by another name?

This is the failure mode a lift introduces, so I checked it directly.

* The removed string appears **only in comments**; the guard itself is gone.
* `cargo test -p ec-av1 --lib -- refusal` and `-- gate_coverage`:
  **48 passed, 0 failed** — including `the_decode_path_refuses_exactly_the_
  listed_cases`, which is the test that would go red if a refusal fired without
  being registered, or a registered one stopped firing.
* The full 12-bit battery (`-- 12bit`): **16 passed, 0 failed**.
* The inventory's own 128-root residue notes (refusal_inventory.rs:1363-1380)
  are *not* 12-bit claims: they rest on `av1_allow_palette`'s
  `bw > 64 || bh > 64` bound, which is depth-independent. My sb128 12-bit
  screen cells (24 of them, all exact) are the empirical version of that
  argument.

**Verdict: genuinely supported.** The lifted path is exercised (non-zero
palette, palette-UV, intrabc, intra-in-inter palette and inter-8 palette
counters), correct on every cell I could produce, and no other named guard
shadows it.

### 1.6 Gates bite (mutation, revert, re-run)

`read_palette_colors_y` (`decode.rs:11417`):

```rust
-        let first = dec.literal(bd) as u16;
+        let first = (dec.literal(bd) as u16) & 0x00ff;   // 12-bit colours truncated
```

Bit-preserving, so no desync — only palette pixels wrong. All three gates
FAILED, naming the defect:

```
a_real_aomenc_12bit_screen_content_palette_stream_...  119726 bytes differ (first at 1)
a_real_aomenc_12bit_intrabc_screen_stream_...           108010 bytes differ (first at 1)
a_real_aomenc_12bit_lossless_screen_content_stream_...  118818 bytes differ (first at 1)
```

The 119,726 is the report's figure, reproduced. `git checkout --` restored
`decode.rs`; the same three gates are green again (3 passed, 0 failed).

---

## 2. `99750756` lane-av1enchang — `Av1Encoder` boxed, gated

### 2.1 The size, measured

```
$ cargo test -p ec-av1 --lib -- stack_budget --nocapture
BY-VALUE INVENTORY (21 types, budget 8192 B = 8 KiB)
   BYTES  %BUDGET  TYPE
   15232   185.9%  Cdfs
   15232   185.9%  CdfSnapshot
    2944    35.9%  Av1Encoder      <-- 0.36x of budget
    1296    15.8%  FrameCtx
      280     3.4%  Encoded
TIGHTSTACK 1048576 bytes: Av1Encoder=2944 Encoded=280 bytes, both chains returned
test result: ok. 8 passed; 0 failed
```

**Correction to record:** the merge message says "139960 → 18168 bytes", and
`Av1Encoder` **is** 2,944 bytes today. 18,168 was true when `99750756` landed
and was correct then; `d202b8b4` then boxed `carried_cdfs` and took it to
2,944. Both reports agree with the tree at their own commit. Anyone reading the
older figure as current is off by 6.2x.

The 1 MiB capability arm really constructs both nested `Result<Self>`
constructor paths and returns. `test_threads_get_the_stack_they_are_given`
measured the caller's floor from `/proc/self/task/<tid>/maps`: 2,101,248 bytes
with the repo cap lifted, 67,112,960 with it.

### 2.2 The gate bites

Mutation = a real revert of `99750756`: `DpbSlot::cdfs: Box<CdfSnapshot>` →
`CdfSnapshot` (`encoder.rs:1495`) plus its construction site (`:1981`).

```
$ cargo test -p ec-av1 --lib -- the_pyramid_constructors_fit_on_a_tight_stack
thread '<unknown>' has overflowed its stack
fatal runtime error: stack overflow, aborting
(signal: 6, SIGABRT)  exit 101
```

The size gate reds too, by name:

```
Av1Encoder is 124736 bytes, over the 8192-byte by-value stack budget (15.23x).
assertion `left == right` failed: Av1Encoder is 124736 bytes, the inventory
  records 2944    left: 2944  right: 124736
```

(124,736 = 2,944 + 7×15,232, i.e. the eight DPB slots — the arithmetic
confirms the box is load-bearing in exactly the way claimed.)

Restored via `git checkout -- crates/ec-av1/src/encoder.rs`; sha256 back to
`91c504fb1ad331201054f81a12821d901868cf17610b447a7d9ed55ee3acc3c4`; 9 passed /
0 failed on the re-run.

---

## 3. `d202b8b4` + `bce234df` — the by-value class bound and the sibling crates

### 3.1 Every claimed size, re-measured

`ec-av1` (from §2.1): `Av1Encoder` 2944, `Encoded` 280, `CdfSnapshot`/`Cdfs`
15232 (crate-private, not budgeted), 21 types, 8 tests green.

`ec-opus` (7 tests green) — reproduces the report exactly:

| type | report | mine |
|---|---:|---:|
| `Encoder` | 4,072 | **4072** (49.7% of budget) |
| `Decoder` | 1,280 | **1280** |
| `SilkStereoEncoder` | 19,744 | **19744** |
| `SilkEncoder` | 6,648 | **6648** |
| `SilkDecoder` | 6,304 | **6304** |

`ec-ac3` (8 tests green): `Ac3Decoder` **3160** (38.6%), `Core` 2864,
`Ac3Encoder` **2784** (34.0%) — all three match the report.

`ec-vp9` (7 tests green): `Decoder` **768** (9.4%), `FrameContext` 2039,
`Picture` 96, `BoolDecoder` 40 — matches.

All seven headline sizes (4072, 1280, 3160, 2784, 768, 2944, 280) reproduce
digit for digit.

### 3.2 Each crate's gate bites — mutation, revert, re-run

| crate | mutation | gate output |
|---|---|---|
| `ec-opus` | `silk_buf: Box<[u8; MAX_SILK_PACKET_BYTES]>` → inline array | 2 FAILED. `Encoder is 7896 bytes, the inventory records 4072`; box pin fires |
| `ec-ac3` | `Core::coeffs: Box<[[f32; COEFFS]; CHANNELS]>` → inline | 3 FAILED. `Ac3Decoder is 10320 bytes, over the 8192-byte by-value stack budget (1.26x)`, inventory `10320 vs 3160`, box pin fires |
| `ec-vp9` | `ctx: Box<FrameContext>` → inline (plus the 6 mechanical call-site adaptations) | 2 FAILED. `Decoder is 2800 bytes, the inventory records 768`; box pin fires |
| `ec-av1` | `Encoded::start_cdfs: Box<CdfSnapshot>` → inline | 3 FAILED. `Encoded is 15504 bytes, over the 8192-byte by-value stack budget (1.89x)`, inventory `15504 vs 280`, `the_crate_private_cdf_tables_stay_behind_a_box` fires |

**7896, 10320 and 2800 are the reports' own mutation figures**, reproduced
exactly. Every restore was `git checkout --` and every post-restore sha256
matched the pre-mutation value; all four crates green afterwards
(7/8/7 and 8 tests).

### 3.3 Public API surface

The claim is "no breaking change". I checked the mechanism rather than trusting
it: every field boxed is private or `pub(crate)` — `ec-opus::Encoder`/`Decoder`
fields are all private, `ec-vp9::Decoder`'s are all private, `ec-ac3::decode::
Core` is `pub(crate)` and `Core` itself crate-private — and no public method
signature changed. `Encoded`'s changed fields (`start_cdfs`, `next_cdfs`) and
their type `CdfSnapshot` are `pub(crate)`; the crate's other consumers are
inside `ec-av1`. **No caller in this workspace can reach a changed field.**
Bit-exactness: the boxing is layout-only (`Box::new(x)` stores the same `x`);
the `Encoded` CDF *values* are untouched. I did not re-run the encoder
bitstream families (out of budget for this pass, and the 12-bit + odd-height
decode batteries I did run are unaffected by encoder layout).

---

## 4. `74ee1ac9` lane-av1oddheightfork3 — the var-tx whole-block publish

Measured by a sibling lane in its own worktree
(`/home/tahinli/.cache/wt/av1oddfork3`, branched at `81a21c7d`), from oracle
bytes with its own throwaway comparator.

### 4.1 Provenance and the fixed cell

`crates/ec-av1/fixtures/420_oddheight_320x236.obu` — **16562 bytes, sha256
`84e4d1ab56620af1c5b78e1aa3d2d67496a492f2e127198c82dd1d68b01c6200`, fnv1a64
`521889434652397870`**. All three match the gate's pins at
`stream.rs:11909-11921`. (The report and the merge message call it
`420_oddheight_320x236_diverging.obu`; commit `8b18b94e` renamed it 11 minutes
after the merge to match its now-exact state. Same blob, name only — **not** a
defect.)

Independently of the crate's comparator: our `decode_stream` output packed and
`cmp`'d against our own `aomdec --rawvideo` run — **both 1,812,480 bytes,
byte-identical**. I reproduced the same 0/0/0, 16/16 with my own probe
(`RESULT oddheight: wrong_y=0 wrong_u=0 wrong_v=0 frames=16 exact=16`).

The report's red-before is not a citation: reverting **only** the four call
sites (`fill_lf_grid_rect_after_vartx` → `fill_lf_grid_rect`) and rebuilding
gives **Y 234349 / U 56957 / V 52981, 0/16 exact**, first diff at frame 0
offset 41216, frame 0 alone wrong (no propagation) — the report's numbers digit
for digit. The committed gate
`the_pinned_420_oddheight_320x236_witness_decodes_byte_exact` goes RED under
that same revert and green after restore.

### 4.2 The control case — the OTHER direction, proven not unfalsified

This is the part a refutation must attack: does suppressing the whole-block
publish break a case where libaom *does* perform one?

**The mechanism.** `fill_lf_grid_rect` (`decode.rs:9644`) passes
`publish_txfm_bands = true`; the band write is guarded at `:9736` by
`if publish_txfm_bands && fctx.intra_only` and issues
`txfm_partition_update_rect(self, at_mi, (tx_px, tx_h_px), (w_mi*MI, h_mi*MI))`.
Only the four var-tx bodies take the `false` route (`decode_intrabc_rect`,
`decode_intrabc_128rect`, `decode_intrabc_owned_rect`, `decode_rect4_16_intrabc`).
**18 publishing callers remain** — the standing audit list for this class.

A concrete libaom-`else`-branch caller: `decode.rs:17013` in
`decode_block_rect4` (also `:17155`, `:17290`) passes the BLOCK's own
`bw, bh` as the transform size, because that body's tx comes from
`depth_to_tx_wh`/the block size, never from a var-tx tree read — so libaom's
`parse_decode_block` takes the `else` branch and `set_txfm_ctxs` runs once over
the whole block. Same at `decode.rs:15943` (`decode_block_rect`) and `:18941`
(`decode_rect4_16_strip`).

**The empirical proof the report did not give.** Suppress the publish for
**all** callers (make the wrapper pass `false`) and re-sweep the 104 committed
fixtures against live `aomdec`: of 94 comparable, **36 go pixel-RED and 3 more
go hard decode-refusal** (a Golomb tail longer than this decoder reads — the
suppression desyncs the bitstream). Concretely RED: `420_odd65x65_key`
(4006,1028,1035), `444_lossy_rect4_inter_witness` (24329,24316,24409),
`444_sb128cq20_tsrc2_control` (293945,294031,294065), `hg_arf_witness`
(272662599,59146808,59093458), `palette_screen_leaf8_witness`
(720546,176574,182443), and 30 more. **39 of 94 depend on the whole-block
publish surviving.** The publish is emphatically not dead code, and the
fix's narrow suppression is correct.

The lane's own baseline sweep is also broader than the report's: **95 of 104
fixtures comparable, 95/95 byte-exact**, the 9 exceptions all refused by name
(6× 4:2:2, 3× named) or oracle-refused (`440_request_is_422`).

### 4.3 Coverage gap found (reported, not fixed)

The report added, at the same time, a `TX_4X4` publish to the two **lossless**
arms (`decode.rs:18187` `decode_rect4_16_intrabc`, `decode.rs:30147`
`read_block_tx_size_rect`). Reverting **only** those two publishes and
re-sweeping all 104 fixtures: **94 comparable, 94 byte-exact, ZERO red, ZERO new
refusals.** The report's own concession is confirmed: a lossless *segment*
fails libaom's var-tx condition so nothing on a fully lossless frame reads those
bands, and no committed fixture carries a lossless segment beside a lossy
neighbour that does. **The two lossless-arm publishes are correct-per-libaom
and provably harmless, but uncovered by any committed fixture.** This belongs
in `not_done`, not in the evidence. It is the one substantive gap in merge 4.

---

## 5. Corrections, and things a reader should not inherit

1. **`Av1Encoder` is 2,944 B, not 18,168 B.** Correct when `99750756` landed;
   superseded by `d202b8b4` in the same day. Do not quote 18,168 as current.
2. **The `_diverging` fixture name is stale, not the fixture.** `8b18b94e`
   renamed `420_oddheight_320x236_diverging.obu` →
   `420_oddheight_320x236.obu` post-merge, same blob (sha256 unchanged).
3. **Stale line citations in `lanes/av1oddheightfork3.report.md`.** The writer
   is cited as `decode.rs:9734`; on the lane's own tip it was 9750 and on
   today's main it is **9749**. The lossless publishes are cited `:30020`/
   `:18185`; actually **30147/18187** on main. Statements correct, line
   numbers off by 1–127 from intervening merges.
4. **Merge 4's lossless-arm publishes are uncovered by any committed fixture**
   (§4.3).
5. Merge 1's `not_done` items 2 and 3 (12-bit intra-in-inter / rect-strip
   palette; 12-bit 4:4:4 screen content) are now **measured, and exact** — not
   divergences, and the report's "untested-cell, not a known-diverging cell"
   reading is the correct one. I have not committed those cells as fixtures.

## 6. Untestable / not tested, with reasons

* **Sibling-crate bit-exactness integration families** (opus conformance, ac3
  matrices, vp9 eight exactness families): not re-run. Layout-only change with
  an unboxed-field mutation already proving the gates bite; re-running the
  audio matrices is Main's integration job, and the merge messages record
  identical counts.
* **`322x248` cell (16131 B, sha256 `9e8c9c6c…`)**: not committed to the
  fixture library, so I could not re-measure it without re-encoding. Its
  recipe is sha-verified in the report; I did not re-derive it. The committed
  320x236 sibling covers the same code path and I measured it directly.
* **`Encoded` CDF values byte-identical after boxing**: asserted by
  construction (`Box::new(x)` stores the same `x`) and by the 12-bit decode
  battery I ran; the encoder bitrate/bitstream families were not re-run.
* **The ~7 h `bitrate_target_lands_within_5_percent_over_48_frames` test**:
  skipped per the standing instruction; not a hang.

## 7. Process notes

* All work in `/home/tahinli/.cache/wt/asli9` (and the sibling's
  `/home/tahinli/.cache/wt/av1oddfork3`). **A relative-path `edit` landed in
  the PRIMARY checkout once during this pass** — `crates/ec-av1/src/encoder.rs`
  was mutated there by accident. Detected immediately, reverted with
  `git checkout --`, and the primary's sha256 returned to
  `91c504fb…` (verified identical to the worktree's baseline). Every later
  mutation used an absolute path inside the worktree. This is the
  `worktree-edit-absolute-path-leak` class and it is worth a guard.
* Primary checkout is **clean** (`git status --porcelain` empty, HEAD
  `a30aca9e`). All throwaway probes (`examples/asli9_probe.rs`,
  `examples/asli9_hdr.rs`) and the throwaway integration test are deleted.
* No fix applied, no merge reverted, no push, no rustfmt, no commit.
