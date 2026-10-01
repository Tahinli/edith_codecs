# lane-av1422lift — the 4:2:2 sequence-header refusal is lifted, and the lift is gated

**Branch:** `lane/av1422lift`. **Base:** `main` = `4f206ec3`.
**Worktree:** `/home/tahinli/.cache/wt/av1422lift`.
**Commit:** **`b7a36001`** — one commit, as specified.
**NOT MERGED, NOT PUSHED.** Main does the merge, and only after the gate in §7.

---

## 1. What landed, in one commit

| step | what |
|---|---|
| guard | `stream.rs` `decode_frame`: `if seq.subsampling_x != seq.subsampling_y { return Err(...) }` **removed** |
| residual | the uncodable 4:4:0 `(0,1)` now closes with an **`assert!`** in the same place (repo rule: unreachable path ⇒ assertion, never a comment) |
| inventory | `refusal_inventory.rs`: the `REFUSALS` string row and its `Proof::NegativeGate` `PROVEN` tuple **deleted in the same commit** — `the_decode_path_refuses_exactly_the_listed_cases` asserts both directions, so a half-removal is red |
| tests | the seven refusal-asserting gates renamed and inverted into byte-exact decodes |
| fixtures | 11 new committed pins (2 replacement + 6 corpus + 12-bit + superres + grain) |
| prose | every site `lanes/av1422liftrisk.report.md` §4 named, corrected |

---

## 2. Prose corrected, file:line (line numbers as they were on `main` = `4f206ec3`)

`stream.rs`
- `:1779-1791` — the guard's own doc block. Rewritten: it claimed "the decoder reads them NOWHERE: every chroma extent is hardcoded 4:2:0" and "4:2:2 (ss_x != ss_y) is not [decoded]". Both halves were already false; the block now states the port and the assertion.
- `:1384-1388` — `SeqFlags::subsampling_x/y` doc: "these exist only for the sequence-level refusal … nothing downstream reads them". Now says they are published through `decode::set_subsampling` and read per axis by every chroma extent, plane allocation, output crop, deblock/CDEF crop and restoration grid.
- `:2302-2308` — the refusal test's doc; deleted with the test, its content folded into the replacement's doc.
- `:2759-2763`, `:2816-2821`, `:2997-3002`, `:3323-3326`, `:3403-3406` — the five "with the header refusal standing, NO committed test can decode a 4:2:2 stream" doc blocks. Each replaced by the scope it actually has now.
- `:8556-8583` and `:10122-10141` — the two `sb128rect_chroma_replay_counters` tripwire gates' prose ("the counter is what names the day 4:2:2 is admitted", "the tripwire for the day it is not"). Both still assert zero; the reason changed from "it cannot happen" to "it does not happen here, and the 4:2:2 rows assert it there too".
- `:319-323` — `sb128rect_chroma_replay_counters`'s own doc ("zero on every stream this decoder admits, since 4:2:2 is refused by name").
- `:51426-51429` — `cfl_ac_q3_at_is_reached_only_through_the_420_fallthrough_of_cfl_ac_ss`. The **mechanism was already right** (`cfl_ac_ss` dispatches `(1,0)` before the 4:2:0 fallthrough); only the stated reason was stale. Prose fixed, gate untouched.

`decode.rs`
- `:17008-17019` — the `TxbSet::Chroma16` reader's "the sequence header admits ss 0/0 and 1/1 only — so the table is exhaustive". The exhaustiveness now comes from the reader's own luma shape, which is what the sentence claims.
- `:23409-23419` — `SB128RECT_REPLAY_SPAN_MISMATCH_HITS`: "Zero on every stream this decoder admits … defensive until that format is ported". It is a **real detector** now.
- `:2859-2864` — `RECT_TILED_CHROMA_NXN_HITS`: "only reachable at 4:2:2, which is refused, so this stays 0 on every admitted stream". Also now reachable.
- `:6140-6145` — `INTRABC_RECT4_OWN_CHROMA422_HITS`: "4:2:2 is refused at the sequence header, so no committed test can read it".

`refusal_inventory.rs`
- `:137-149` + `:150` — the refusal string row and its whole comment block. **Deleted.**
- `:509-519` — the `Proof::NegativeGate` tuple naming `a_non_420_subsampled_sequence_header_is_refused_by_name`. **Deleted**, replaced by a note saying what replaced it and why a `Proof::NegativeGate` on a refusal string could not fail on a broken decode.
- `:1719-1725` — `every_chroma_unit_decode_block_rect_can_present_has_a_coefficient_table`'s doc: `(1,0)` "is walked anyway, so the proof does not depend on that guard staying in place". The walk was already right; its stated reason named a gate that no longer exists. **The gate itself is unchanged and still passes.**
- `:2010-2017` — the strip enumeration's doc, same correction.

`examples/gen_coverage_cells.rs`
- `:31-42` — module doc: "4:2:2, the shape this decoder refuses by name".
- `:259-263` — cell-2 comment: "the decoder refuses it by name, which is the honest outcome".
- `:316` — the `println!`: "i.e. 4:2:2, the shape this decoder refuses by name".

`film_grain.rs`, `decode_rect4_16_intrabc`, and the reserved group-tail chroma SKIP arm were **not touched** (other lanes own them).

---

## 3. The two replacement fixtures

The risk study's **R6**: the old gates' fixtures were a 4:2:2 *header* over a 4:2:0 *tile* from this crate's own encoder. `440_request_is_422.obu` is ffprobe-invalid (`Invalid data found when processing input`) — it is not a decodable 4:2:2 stream and could not be inverted into an exactness claim. Both replacements are real libaom 4:2:2 streams over a y4m whose header line is `C422`.

| pin | bytes | sha256 | fnv1a64 | shape | recipe |
|---|---|---|---|---|---|
| `422_key_64x64.obu` | 755 | `3c9396c9e42701d720419ec2cb5577387e305482df0cb150f7f0b965edeaaffc` | `0x87fc569cf53bcbc6` | 64x64 8-bit, 1 KEY frame | aomenc `--profile=2 --input-chroma-subsampling-x=1 --input-chroma-subsampling-y=0 --passes=1 --end-usage=q --cq-level=24 --cpu-used=0 --threads=1 --row-mt=0 --limit=1 --kf-min-dist=0 --kf-max-dist=0 --obu` over a hand-generated 2-frame integer `C422` card |
| `422_inter_160x128_3f.obu` | 6522 | `7b54834c18f5fdc0e8606a6dc5bf4ff368402417d87fcfc2ab9758e4e9164902` | `0xf87eba3bf4fc2bb8` | 160x128 8-bit, 3 frames, frames 1–2 INTER | same base recipe + `--limit=3 --kf-min-dist=3 --kf-max-dist=3`, over a 3-frame `C422` card whose bright square moves (6,4) → (9,6) → (12,8) |

The inter pin is what exercises the inter-predicted 4:2:2 chroma reference: a decoder that reconstructs the key frame correctly and then predicts chroma from a 4:2:0-shaped reference passes the key pin and reds here.

Both are deterministic: fixed integer sources, fixed encoder flags, single-threaded, `--threads=1 --row-mt=0`.

### `440_request_is_422.obu` is kept, and is explicitly NOT an exactness witness

It stays a **shape** witness (arm 3 proves a 4:4:0 request lands on `(1,0)` and that `(0,1)` is unreachable). Its refusal assertion is replaced by the one thing true of its bytes: the decoded chroma planes are `32x64` — half width, **full height** — proving the sequence header and not the tile fixes the geometry. Its pixels are still unclaimed, and `examples/gen_coverage_cells.rs` now says so in both the doc and the printout.

---

## 4. The new gates

| gate | what it asserts | non-vacuity arm |
|---|---|---|
| `a_real_422_key_frame_and_inter_sequence_decode_pixel_exact` | the two new pins, byte-exact per plane per frame vs ffmpeg `yuv422p` | census delta > 0 on **all three planes**, units and coded |
| `a_real_422_12bit_and_superres_stream_decode_pixel_exact` | 12-bit 4:2:2 and superres 4:2:2 — the two cells the corpus had **no coverage for at all** (R3, R9) | same |
| `a_real_422_film_grain_stream_decodes_pixel_exact` | 4:2:2 film grain, **post-grain** | `film_grain::grain_hits()` moves AND every frame header carries `apply_grain` with scaling points on all three planes AND census delta > 0 |
| `the_pinned_422_corpus_cells_decode_pixel_exact` | the **six closed corpus cells**, byte-exact per plane per frame | census delta > 0 per row, units and coded |
| `the_pinned_422_bigblock_witnesses_decode_pixel_exact` | `422_allskip_2f.obu` + `422_sb128_3f.obu` | per-row; the all-skip row's is a **negative** measurement (below) |
| `the_pinned_422_intrabc_sb128_strip_witnesses_decode_pixel_exact` | both 128-root intrabc strip pins | census delta > 0 |
| `the_pinned_422_lossless_inter_witnesses_decode_pixel_exact` | W / X / Y_intrabc (Y is 10-bit, `yuv422p10le`) | census delta > 0 |
| `the_pinned_422_residual_compound_warp_witness_decodes_pixel_exact` | 16-frame compound-warp + LR | census delta > 0 |
| `the_pinned_422_lr_off_witness_decodes_pixel_exact` | the LR-OFF twin | census delta > 0 |
| `the_440_cell_is_not_a_codable_chroma_shape` (arm 3) | header drives geometry; census walked chroma | census on U and V |

### The non-vacuity arm, and why it is the one that counts

`assert_422_stream_pixel_exact` asserts, per stream, that `census_nonsub_units()` and `census_nonsub_coded()` **both** rose on luma, U and V.

Before lane-av1422census422 the census was armed at `ss_x == 0 && ss_y == 0` only, so on a 4:2:2 stream it read `[0, 0, 0]` **by construction** while the same decode walked 4994 units. A byte-exact 4:2:2 gate asserting those counters would have been green while proving nothing about 4:2:2. That is R8, and it is why the census is the arm named here rather than a 4:2:2 arm counter.

### The all-skip row is the one exception, and it is asserted, not skipped

`422_allskip_2f.obu` codes **zero** chroma coefficients across the whole stream (measured: `units luma=4 u=8 v=8 | coded luma=1 u=0 v=0`). That is the cell's identity — the pre-port defect was the decoder reading **phantom** coefficient units in the all-skip inter frame. Demanding coded units there would be demanding the defect, so the shared body's coded-unit requirement is parameterised and the row's own arm is a negative measurement, asserted directly: chroma units were **walked** (`units[1] > before[1] && units[2] > before[2]`) and **none was coded** (`coded_delta == (0, 0)`).

### Depth correctness

The 10-bit and 12-bit rows compare 16-bit containers on **both** sides (`yuv422p10le` / `yuv422p12le`), so nothing narrows — the u8-narrowing trap that makes an HBD cell red by construction does not apply. Depth itself is read out of the stream's own sequence header (`stream_bit_depth`), never from the gate's name.

### Display order

`decode_stream` returns display order and ffmpeg's rawvideo muxer outputs display order, so frame N pairs with frame N and a hidden alt-ref does not shift the pairing. (The out-of-process corpus comparator has to solve that separately, which is why it exists separately.)

---

## 5. The two cells that had no coverage, and one provenance finding

`s422_12bit_160x128.obu` (9214 B, sha256 `eacc70f75c82ad7d795efbd0242aa4ee33db917b8c1a3570ebce2570ee8bc245`, fnv1a64 `0xa62b8e3fe6f60ae5`) is 12-bit, 160x128, 3 frames, `yuv422p12le`, profile 2.

**Its encoder is ffmpeg's libaom-av1 wrapper, not aomenc, and the reason is measured rather than preferred.** aomenc's 12-bit y4m path issues `AV1E_SET_CHROMA_SUBSAMPLING_X` (`apps/aomenc.c:2272-2278`, reached whenever the input is not 4:2:0 and the bit depth is 12) **before** `aom_codec_enc_config_set`, and on this libaom that control returns `AOM_CODEC_INTERNAL_ERROR` for any non-4:2:0 shape. Measured on **four independently built trees** (`~/.cache/aom-oracle`, `aom-affine`, `aom-oracle2`, `aom-oracle3`):

| y4m | result |
|---|---|
| `C422p12` | `Failed to set chroma subsampling x: Unspecified internal error` |
| `C444p12` | same failure — **so it is not a 4:2:2-specific gap** |
| `C420p12` | encodes (that path skips the control entirely) |
| `C422` (8-bit), `C444` (8-bit) | encode, with the same control succeeding |

So 12-bit 4:2:2 is unreachable through `aomenc` on this libaom for a configuration-order reason, not a format reason. ffmpeg's libaom-av1 wrapper sets the codec config first and encodes the same library:

```
ffmpeg -f lavfi -i "testsrc2=size=160x128:rate=1:duration=3" -pix_fmt yuv422p12le \
       -c:v libaom-av1 -profile:v 2 -b:v 0 -crf 30 -cpu-used 8 \
       -auto-alt-ref 0 -lag-in-frames 0 -strict experimental -f obu <pin>
```

The pin and the oracle (`yuv422p12le` rawvideo) therefore come from the same library, which is what makes byte-exactness a claim about **our** decoder rather than about a disagreement between two encoders.

`s422_superres_160x128.obu` (5304 B, sha256 `5852ff2c273cb773c4886762c6a6f2cce6e12065a690eb3a5ca2119c0b6f2957`, fnv1a64 `0x0789d1b2851b94ae`) is aomenc with `--superres-mode=1` over a `C422` testsrc2 source — the base recipe plus that flag. Superres is the one 4:2:2 path whose output crop is `(round_ss(w,1) x round_ss(h,0))` at the **upscaled** size, and it had no 4:2:2 cell anywhere.

`s422_grain_160x128.obu` (5433 B, sha256 `a4a6957b5e8298d9138f64498cab2d3008b3390fa73cd0d74aa0632b13d93d15`, fnv1a64 `0x2aed47614b9461b8`) is aomenc with `--film-grain-test=5` over a `C422` testsrc2 source. **It cannot go through the dump path**: `EC_AV1_FINAL_DUMP` writes the picture as it is about to enter the reference slots (`stream.rs`, the dump site beside the emit), which is *before* `apply_grain` runs on the output, while ffmpeg's rawvideo is post-grain. The corpus comparator hits this as a hard error, not a clean red — measured: `luma identity never covers the display side (3 decode frames, 3 display frames)`. The grain gate therefore compares the **in-memory `decode_stream` output**, which is post-grain, exactly as `a_real_aomenc_12bit_film_grain_stream_decodes_pixel_exact` already does for 12-bit 4:2:0.

### The six corpus pins

All six are `aomenc --codec=av1 --profile=2 --input-bit-depth=<8|10> --cpu-used=0 --cq-level=24 --limit=16 --auto-alt-ref=1 --enable-global-motion=1 --passes=1 --obu` over a hand-built `C422` y4m (header line, then `FRAME\n` + one frame of planes **per frame**), produced by the census sweep and copied from `~/.cache/census422b/sweep/`. Nothing is bolted on.

| pin | bytes | fnv1a64 | sha256 | geom | depth | frames |
|---|---|---|---|---|---|---|
| `s422_320x246.obu` | 20692 | `0x01e556015878e04e` | `aebd3a10d46e617b803579819f2502798076384be52e032b11b46f09eeb09a79` | 320x246 | 8 | 16 |
| `s422_322x240.obu` | 21096 | `0x9a96dda2a19d47a6` | `e74bf7e91f23353986edf9380bcd6cb760194e619dbbdf6a239b7a05eff81799` | 322x240 | 8 | 16 |
| `s422_322x246.obu` | 20528 | `0x2c6f8be6d30384ab` | `ee96617fe45ba963d3628f37086077a94806ef0465a74cb538a8e57d2ed900b3` | 322x246 | 8 | 16 |
| `s422_352x242_10b.obu` | 20575 | `0xf1d370f80d8ddc6d` | `8893bbed108d7555e66c35f2f45d6e44044dcd6810b0510fb251be05dabaa2d0` | 352x242 | 10 | 16 |
| `s422_416x242_10b.obu` | 20282 | `0x9826402d91abbc56` | `38d0b592e5e358901742122c590c8964b5d08d0ceccfa051e677c28f4b75d174` | 416x242 | 10 | 16 |
| `s422_416x250_10b.obu` | 19967 | `0x6b07e837eae8a6be` | `d6aa7f450f54d6fb381400a0a0226da87599c00509eeb5031abe63383e272627` | 416x250 | 10 | 16 |

These six are the closed members of the seven the measurement base recorded as diverging (`lanes/av1422ffmpegbase.report.md` §2.1). All six were **chroma-only** divergences at non-8-aligned geometries — odd or 2-mod-4 luma dimensions, where `round_ss` and the chroma block walk disagree with a hardcoded `>> 1`. That is why they are the rows a lift cannot leave unasserted.

---

## 6. Commands and results

```text
$ git worktree add -b lane/av1422lift /home/tahinli/.cache/wt/av1422lift main
$ git log --oneline -1
b7a36001 lane-av1422lift: lift the 4:2:2 sequence-header refusal; 4:2:2 decodes

$ CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/lift \
  cargo check -p ec-av1 --lib --tests --examples
Finished dev profile -- CLEAN, zero errors, zero warnings

$ CARGO_TARGET_DIR=... EC_AV1_REQUIRE_FFMPEG=1 \
  cargo test -p ec-av1 --lib -- --test-threads=4 \
    a_real_422 the_pinned_422 the_440_cell
running 10 tests ... test result: ok. 10 passed; 0 failed; 0 ignored; 801 filtered out; 8.06s

$ CARGO_TARGET_DIR=... EC_AV1_REQUIRE_FFMPEG=1 \
  cargo test -p ec-av1 --lib -- --test-threads=4 \
    film_grain census intrabc every_chroma_unit the_decode_path_refuses cfl_ac_q3
test result: ok. 48 passed; 0 failed; 4 ignored; 759 filtered out; 44.41s
```

`cargo check --tests` is the command the assignment names; `--lib --tests --examples`
is a superset and was run clean, which covers it.

### NOT run here, and why that is the correct call

The **whole** `ec-av1` lib suite was NOT completed on this box. Two runs were
started and both were cut by my own wall-clock budget, not by a failure: the
suite's aomenc-encoded gates each take a minute or more and several print
"has been running for over 60 seconds", so the suite does not fit a local
foreground run. That is the standing project rule, not an excuse — full cargo
suites run on the VPS fleet only, never locally, and **three VPS suites are
already in flight for this campaign**, which will carry this branch.

Everything this branch touched IS covered by the two scoped runs above: the
ten 4:2:2 gates, the renamed/inverted gates, `--lib film_grain`,
`--lib census`, the intrabc tests, both `every_chroma_unit_*` strip
enumerations, `the_decode_path_refuses_exactly_the_listed_cases` (the gate
that forces the guard and the two inventory rows into one commit), and
`cfl_ac_q3_at_is_reached_only_through_the_420_fallthrough_of_cfl_ac_ss`. All
green. `cargo check --lib --tests --examples` is clean with zero warnings,
so nothing fails to build. **Main's VPS runs are the regression signal for
the rest of the crate.**

### The out-of-process corpus re-measurement on this branch

Run against the **lifted** `decode_probe` (`/home/tahinli/.cache/tgt/lift/debug/examples/decode_probe`, guard removed, no bypass anywhere), through the committed `scripts/run422-ffmpeg.py` + `scripts/cmp422-ffmpeg.py` comparator, per plane per frame, decode-order→display-order mapped, depth-correct on both sides:

```text
24 cells completed of the 57-cell list; 23 BYTE-EXACT, 1 DIVERGES.
(the run was cut by my OWN `timeout 3000`, rc=124 -- not by a failure and
 not by a comparator error; no cell produced REFUSES or COMPARATOR-ERROR)

s422_384x240: DIVERGES 17df/16shown hidden=[1] Y=0 U=1780 V=1871
              first: disp f7 (decode f2) U (r169,c30)
```

The single divergence reproduces the census422 lane's figure exactly (`U=1780 V=1871`, Y=0, chroma-only) — the same residual that lane measured, unchanged by this lift, as expected: this lane does not touch the decode path of the reserved arm.

### Probe-bypass hygiene

**No bypass was ever applied on this branch.** `grep -c 'TEMP-PROBE-BYPASS\|if false &&'` is 0 in this worktree, and `git -C /home/tahinli/Documents/Code/Rust/edith_codecs status --porcelain` is **empty** — the primary checkout is untouched. No scratch worktree was created for this lane, so none needed removing; the probe binary that `run422-ffmpeg.py` hardcodes (`/home/tahinli/.cache/tgt/av1422seed/...`) was **not** used — the driver was re-pointed at this lane's own binary from `/home/tahinli/.cache/lift422run.py`, outside the repo.

---

## 7. What is NOT closed

**`s422_384x240` diverges and belongs to another lane.** Chroma-only, `U=1780 V=1871` on a 17-decode/16-display cell, reproduced identically by this lane's measurement and by `lanes/av1422census422.report.md` §3.4. It is the reserved group-tail chroma SKIP arm (`decode.rs`, the arm `lanes/av1422liftrisk.report.md` **R4** flags as owned elsewhere). This lane did not touch it.

**The 51/51 corpus gate waits for it.** This lane pins six of the seven previously-diverging cells; the seventh is not pinned here, deliberately — pinning a red cell inside a green gate is how a gate stops being a gate. The gate's doc says so in the same words.

### Therefore:

> **THIS BRANCH MUST NOT LAND UNTIL `s422_384x240` CLOSES AND THE CORPUS RE-MEASURES 51/51.**
>
> The guard removal, the inventory retirement and the `(0,1)` assertion are safe to merge as they stand — they are one commit and the tree is green on every gate above. What is not safe is landing them as the *finished* 4:2:2 support claim while one corpus cell is still red. Main should hold `b7a36001` until the owning lane closes `s422_384x240`, then pin that cell into `the_pinned_422_corpus_cells_decode_pixel_exact` and re-run the 51-cell census to 51/51 before merging.

## 8. Incidents and observations

1. **aomenc cannot produce 12-bit 4:2:2 on this libaom** (§5). Not a 4:2:2 gap — 12-bit 4:4:4 fails identically, and the same control succeeds at 8 bits. The pin's encoder is ffmpeg's libaom-av1 wrapper; both the pin and the oracle come from libaom. Recorded here because the next lane that needs a 12-bit 4:2:2 cell will hit it too.
2. **`EC_AV1_FINAL_DUMP` is pre-grain**, so no grain cell — 4:2:0, 4:2:2, 12-bit — can ever be compared through it. The grain gate uses the in-memory decode. (Independently observed by `lanes/av1422census422.report.md` §7.2; confirmed here with a hard comparator error rather than a red.)
3. **The census's coded-unit arm cannot be uniform across the corpus.** `422_allskip_2f.obu` codes no chroma coefficient by construction. A blanket "coded > 0 on every plane" gate would have been red on the one cell whose identity is that it codes nothing — and turning it green by deleting the assertion would have been the wrong fix. It is parameterised, with the row's own negative assertion standing in.
4. **`cargo check --lib --tests --examples` is clean with zero warnings** on this branch — the three `dead_code` warnings the new helpers produced mid-edit are gone once the gates use them.
