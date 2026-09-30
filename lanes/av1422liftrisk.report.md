# lane-av1422liftrisk — what breaks when the 4:2:2 sequence-header refusal is lifted

**Read-only study. No source was changed. Target tree: `main` = `3d56fb60`.**

**DELIVERABLE CAVEAT, read first.** This lane has no shell and `write` accepts only `xd://`
targets, so I could **not** create `lanes/av1422liftrisk.report.md` nor create/commit branch
`lane-av1422liftrisk`. This document is the report verbatim; the main agent should land it as
the file and commit it. Every claim below carries `path:line`. Where I am reading intent rather
than a mechanism I say so inline and mark confidence.

---

## 0. THE TREE IS NOT WHAT THE CONTEXT SAYS — finding #1, read before anything else

The context states the guard is live at `stream.rs:1803-1808`. The file on disk reads:

```
1803    // TEMP-PROBE-BYPASS: EC_AV1_ALLOW_422_PROBE -- patch, run, restore. Never
1804    // committed.
1805    if false && seq.subsampling_x != seq.subsampling_y {
1806        return Err(Error::unsupported(
1807            "AV1 decode_stream",
1808            "a chroma format of 4:2:2 (subsampling_x != subsampling_y): ...",
```

`crates/ec-av1/src/stream.rs:1803-1808`. The probe bypass the sibling report says must **never**
be committed is present in the checkout, and by its own comment it is not supposed to be here.
I have no shell, so I cannot run `git status`/`git diff` to tell whether this is an uncommitted
edit in the primary checkout or an actual commit. **Confidence: mechanism (the file text) is
certain; the git state is unverified.** Either way, every statement in
`lanes/av1422ffmpegbase.report.md` §7 ("`stream.rs:1803` still refuses 4:2:2 by name on
`affe70dc`") and in the assignment's premise ("the guard will be LIFTED") describes a tree this
checkout is not. Reconcile this before anyone lifts anything: the lift may already be half-done
in the working tree, and the `if false &&` will silently make every "it refuses by name" gate
in §3 go RED for the wrong reason.

---

## 1. Every `subsampling_x`/`subsampling_y`/`ss_x`/`ss_y` read and write, classified

### 1a. Correct today AND correct for 4:2:2 (per-axis, already ported)

| site | what it does |
|---|---|
| `ec-av1-syntax/src/sequence.rs:455-497` | the only producer. `(0,1)` is unreachable by construction: `subsampling_y` is read only when `subsampling_x == 1` (`:481-485`) |
| `crates/ec-av1/src/stream.rs:1408-1409` | `SeqFlags` copies both |
| `crates/ec-av1/src/stream.rs:1914` | `set_subsampling` publishes both into `FrameCtx` |
| `crates/ec-av1/src/decode.rs:680-693` | the two accessors, already separate |
| `crates/ec-av1/src/decode.rs:697` | `round_ss(dim, ss)` — per-axis by construction |
| `crates/ec-av1/src/decode.rs:35220-35221` | plane alloc `(width >> ss_x, height >> ss_y)`, `true_*` likewise |
| `crates/ec-av1/src/decode.rs:33392, 33399-33401, 37531-37537, 37634-37655` | every output crop uses `round_ss(w, ss_x) x round_ss(h, ss_y)` |
| `crates/ec-av1/src/decode.rs:38930-38939` | `ref_chroma_shape` has a real 4:2:2 arm (`half_w * height`) and an explicit odd-dimension 4:2:0 arm |
| `crates/ec-av1/src/decode.rs:32567-32574` | deblock chroma crop `div_ceil(1 << ss_x)` / `div_ceil(1 << ss_y)` per axis |
| `crates/ec-av1/src/decode.rs:33686` | CDEF chroma origin `(ox >> ss_x, oy >> ss_y)` |
| `crates/ec-av1/src/decode.rs:33905-33907` | CDEF chroma band rows `(j*64) >> ss_y` |
| `crates/ec-av1/src/decode.rs:28440, 33669-33671` | `CDEF_DIR_CONV422` + its dispatch, the asymmetric direction remap |
| `crates/ec-av1/src/restoration.rs:424-441` | `RestorationGrid::with_ss` per axis |
| `crates/ec-av1/src/restoration.rs:530-542` | `mi_size_x = MI_SIZE_8PX >> ss_x`, `mi_size_y = ... >> ss_y` — the split that was itself a 4:2:2 fix |
| `crates/ec-av1/src/restoration.rs:1647` | `voffset = 8 >> ss_y` |
| `crates/ec-av1-syntax/src/frame.rs:1633-1640` | `lr_uv_shift` read only at `ss (1,1)` — matches libaom |
| `crates/ec-av1-syntax/src/frame.rs:1916` | film-grain `num_cb_points = 0` rule already keyed on the pair |
| `crates/ec-av1/src/warp.rs:611-616` | `src_x << subsampling_x`, `dst_y >> subsampling_y` |
| `crates/ec-av1/src/encode.rs:90-109` | `chroma_mi_shape` already carries `(4,4,1,0) -> (2,1)`, `(8,4,1,0)`, `(16,4,1,0)` |
| `crates/ec-av1/src/cdf.rs:3173-3181` | `TXB_SKIP_CHROMA_8_BIG` / `16_BIG`, added specifically because 4:2:2 reaches `get_txb_ctx`'s `+10` arm |

### 1b. Correct today but WRONG for 4:2:2 (the actual list)

One module. `crates/ec-av1/src/film_grain.rs`:

| site | the 4:2:0 assumption |
|---|---|
| `:39` | `const CHROMA_SUBBLOCK: i32 = 16; // LUMA_SUBBLOCK >> subsampling (always 1 here)` |
| `:228-231` | `avgLuma` is a 2x2 luma quad; at 4:2:2 it must be a horizontal pair only |
| `:351` | `let c_stride = (picture.width / 2) as i32;` — right for WIDTH at 4:2:2, but the row model below is not |
| `:577` | `let n = chroma.len().min(grain.len()).min(luma.len() / 2);` — halves the luma operand |
| `:1151` | `let c_stride = width / 2;` in `apply_grain` |
| `:1206` | `let nrows = ((height / 2).max(0) as usize)...` — **half the chroma plane's rows** |
| `:1276` | `while x < width / 2` chroma block column loop |
| `:1413-1414` | `bh = (LUMA_SUBBLOCK >> 1).min(height / 2 - y)`, `bw = ... min(width / 2 - x)` |
| `:1592, 1604, 1621, 1648` | chroma copy-area extents all derived from `>> 1` and `height/2` |
| `:1889-1890` | the test-only reference: `let cw = w / 2; let ch = h / 2;` |

Consequence at 4:2:2 (`chroma plane = round_ss(w,1) x h`): `c_rows` computed from
`u.len() / c_stride` is `h`, but `nrows` is `h/2`, so the walk covers the **top half** of the
chroma plane and `copy_clean_rows` leaves the bottom half ungrained. Inside the covered half,
chroma row `y` is addressed at luma row `2y` (wrong at 4:2:2, where it should be `y`) and the
`avgLuma` quad is 2x2 where libaom uses a pair. **No panic, no refusal, no counter** — the
output is simply a different picture. `[INFERENCE on reachability]`: libaom ships
`generate_grain_uv_422`, so 4:2:2 film grain is a codable cell; whether `aomenc` will emit it
needs a measured answer (see risk list R3).

One more, in a comment rather than code: `crates/ec-av1/src/decode.rs:16991-16992` says the
`TxbSet::Chroma16` reader "the caller set is closed -- ... the sequence header admits ss 0/0 and
1/1 only -- so the table is exhaustive". That claim is about the header, and post-lift it is
false. The mechanism (`nw * nh > 1` shapes routing to `Chroma16`/tiled arms) was already
extended for 4:2:2, so the code is fine and the *sentence* becomes a lie. Check it while
editing anyway: the census found 44/51 cells byte-exact, so this table is not the problem.

### 1c. Unreachable today, becomes a live arm (needs an assertion, not a comment)

- `crates/ec-av1/src/decode.rs:23388-23398` — `SB128RECT_REPLAY_SPAN_MISMATCH_HITS`, whose own
  doc says "Zero on every stream this decoder admits: both supported chroma formats have
  `ss_x == ss_y` ... and 4:2:2 ... is refused by name at the sequence header ... so the arm is
  defensive until that format is ported." Post-lift it is a real detector. Two gates already
  read it and assert zero on 4:2:0/4:4:4 streams (`stream.rs:8577-8583`, `stream.rs:10135-10141`)
  — those two stay correct as written, but their prose ("the tripwire for the day it is not")
  expires.
- `crates/ec-av1/src/decode.rs:2841-2844` — `RECT_TILED_CHROMA_NXN_HITS`, "only reachable at
  4:2:2, which is refused, so this stays 0 on every admitted stream; kept so the match above is
  exhaustive". Post-lift it fires.
- `crates/ec-av1/src/decode.rs:23137` — the `max_units_h` 4:2:2 arm under `ss_x==1 && ss_y==0`.
- `crates/ec-av1/src/decode.rs:40953-41008` — the `chroma_side` enclosing-square /
  `chroma_stride` split. Already correct; the guard on it is the only 4:2:2 test.
- The residual uncodable shape: `crates/ec-av1-syntax/src/sequence.rs:481` never reads
  `subsampling_y` when `subsampling_x == 0`, so `(0,1)` cannot arrive. Per this repo's rule
  that closes with an **assertion**, not a comment.

### 1d. NOT a decode-path risk (encoder-only, 4:2:0 by construction)

`crates/ec-av1/src/encoder.rs:76-78` and `crates/ec-av1/src/encode.rs:2232-2234` both hardcode
`subsampling_x: 1, subsampling_y: 1`. `crates/ec-av1/src/tile.rs:417-423` explicitly documents
the 128-root CHUNKS chroma square span as "a PORT note, not a live defect" because "refused by
name at the sequence header today". `crates/ec-av1/src/tile.rs:5004-5006`
`palette_uv_side(side) = (side/2).max(4)` is writer-side; the decoder's counterpart
(`decode.rs:11754-11763` `palette_onscreen_uv`) is per-axis. A decode-only lift leaves all of
these alone, and saying so is part of the answer.

---

## 2. Tables and helpers indexed by the subsampling PAIR

**Already correct (the recent 4:2:2 work added these; do not touch):**
`chroma422_pair_plane` (`decode.rs:12333-12344`) with its `ss_size_lookup[..][1][0]` census test
(`decode.rs:13626`); `around_mi_422_chroma` (`decode.rs:10362-10419`); `cfl_ac_ss`'s 4:2:2 arm
(`decode.rs:20980-21003`); the per-axis mu-chunk chroma walk
(`decode.rs:42474, 42588, 44287, 44381`); `chroma_side` vs `chroma_stride`
(`decode.rs:40953-41008`); `ref_chroma_shape` (`decode.rs:38930`); `CDEF_DIR_CONV422`
(`decode.rs:28440`); `obmc_skip_chroma_above` taking real `ss_x`/`ss_y`
(`decode.rs:39778, 39939`); the per-axis `is_chroma_reference` arms (`decode.rs:26136, 26620,
27296, 46453-46457, 46953, 47834, 48323, 54380`); `chroma_mi_shape`'s 4:2:2 rows
(`encode.rs:94, 100, 106`); `TXB_SKIP_CHROMA_{8,16}_BIG` (`cdf.rs:3173-3181`).

**INVALID or MISSING for 4:2:2:**

1. **`film_grain.rs`'s whole geometry** (§1b). This is the one genuinely missing entry.
2. **`refusal_inventory.rs:2129-2148`** — `every_chroma_unit_a_64_axis_strip_can_present_has_a_
   coefficient_table` enumerates `for (ss_x, ss_y) in [(0,0), (1, 1)]` and asserts
   `checked == 8` with the message "the strip domain is not the four 64-axis strips x two
   supported formats". Post-lift it does not fail; it silently stops proving anything about
   4:2:2. Its sibling at `refusal_inventory.rs:1871` already walks `[(0,0), (1,0), (1,1)]`, so
   the two halves of the same file disagree about the reachable set. That asymmetry is the
   cleanest single "would become vacuous" claim in the tree.
3. **No 4:2:2 coefficient-unit census exists.** `set_census_444`
   (`decode.rs:2337-2338`, armed `stream.rs:1918`) enables the unit counters **only** at
   `ss_x == 0 && ss_y == 0`. At 4:2:2 every `census_444_units()`/`census_444_coded()` counter
   reads zero by design. So the non-vacuity requirement the sibling report §4.5 item 5 asks for
   ("the reachability counter for the arm each cell witnesses must be `> 0`") **cannot be
   written for 4:2:2** using the existing census. The 422 counter families
   (`chroma422_square/rect/sub8/chunk/pair_wide_hits`, printed by `decode_probe`) cover a
   subset of arms only. A 422 unit census is a prerequisite, not a nicety.

**Would index out of range?** I found none. The `max_txsize_rect_lookup[255]` /
BLOCK_INVALID cases the sibling report cites (`decode.rs:12393-12397`, `:13619-13623`,
`:17004-17008`) are already handled by the `chroma_422_oob` / `chroma_422_pair_plane` arms and
are guarded by the pinned oracle table at `decode.rs:13629-13640`.

---

## 3. Tests: what goes vacuous, what goes red, what would assert the wrong thing

From `lanes/av1422ffmpegbase.report.md` §4.3, confirmed against the file:

| test | `path:line` | today | post-lift |
|---|---|---|---|
| `a_non_420_subsampled_sequence_header_is_refused_by_name` | `stream.rs:2316` (string `:2318`, assert `:2336-2339`) | builds a 4:2:2 header over a **4:2:0 tile from this crate's own encoder** (`:2321`, `:2330-2334`) and asserts the refusal | **goes RED, and it cannot simply be inverted.** The fixture is a header/tile mismatch; the tile is 4:2:0 data. It must be replaced with a real 4:2:2 stream, not flipped to "decodes" |
| `the_440_cell_is_not_a_codable_chroma_shape` arm 3 | `stream.rs:2494`, assert `:2731-2734` | decodes `440_request_is_422.obu` and asserts it refuses by name | **goes RED, same reason and worse.** Per `gen_coverage_cells.rs:252-256` that pin is "a real 4:2:0 key frame's, byte for byte" behind a 4:2:2 header. Arms 1-2 (the shape enumeration, `:2600-2603`) stay valid |
| `the_pinned_422_bigblock_witnesses_are_present_and_refuse_by_name` | `stream.rs:2766`, assert `:2784-2788` | size + `fnv1a64` pin, then `err.contains(REFUSAL)` | goes RED on the refusal assert; the pins stay load-bearing |
| `the_pinned_422_intrabc_sb128_strip_witnesses_refuse_by_name` | `stream.rs:2822`, assert `:2849-2852` | same | same |
| `the_pinned_422_lossless_inter_witnesses_are_present_and_refuse_by_name` | `stream.rs:3012`, assert `:3029-3032` | same | same |
| `the_pinned_422_residual_compound_warp_witness_is_present_and_refuses_by_name` | `stream.rs:3328`, assert `:3346-3350` | same | same |
| `the_pinned_422_lr_off_witness_is_present_and_refuses_by_name` | `stream.rs:3408`, assert `:3426-3429` | same | same |

**Which pass ONLY because of the refusal.** All seven above. Each one's only behavioural
assertion is `err.contains(REFUSAL)`, which is a statement about a *refusal*, not about pixels:
they would pass unchanged on a tree where 4:2:2 decode is completely broken but still refused
at the header. They are byte-pin tests wearing decode-test clothes. Inverting them to an
ffmpeg byte-exact decode is the whole point of the lift.

**Which become VACUOUS rather than red (the quieter failure):**

- `refusal_inventory.rs:2129-2148` — the `(0,0)`/`(1,1)` strip enumeration (§2 item 2). Passes,
  proves nothing.
- `refusal_inventory.rs:854` `the_decode_path_refuses_exactly_the_listed_cases` — asserts both
  directions. If the guard goes and the `REFUSALS` row at `:150` and the `PROVEN` tuple at
  `:530-533` do not, the "listed but no longer in the decode path" direction goes red. If all
  three go in the same commit, fine; if the string is retired but the tuple is not, red.
- `refusal_inventory.rs:1732-1736` — the doc of the other enumeration names
  `a_non_420_subsampled_sequence_header_is_refused_by_name` as the thing that refuses `(1,0)`.
  After the lift the file lies in prose.
- `stream.rs:51405-51418` `cfl_ac_q3_at_is_reached_only_through_the_420_fallthrough_of_cfl_ac_ss`
  — the doc asserts "4:2:2 is refused at the sequence header", but the **mechanism still holds**:
  `cfl_ac_ss` (`decode.rs:20980`) dispatches `(1,0)` before the 4:2:0 fallthrough. Do not
  "fix" this gate; fix its prose.
- `decode.rs:5434-5439` `top_half_compound_hits` — `#[allow(dead_code)]`, doc says "NO COMMITTED
  TEST READS THIS, and none can while the 4:2:2 header refusal stands". Post-lift it is the
  natural non-vacuity anchor for the compound/warp cells and should get a reader.

**Already correct, must not be touched:** the per-axis source-scan gates
`the_422_lossless_inter_chroma_walk_sites_stay_per_axis` (`stream.rs:3058`, geometry table
`:3204-3260`, including the `assert_ne!(plane_block(16,1,0), enclosing(16,1,0))` at `:3228-3234`
that exists precisely to prove 4:2:2 is not square) and
`the_intrabc_128rect_chroma_chunk_walk_stays_per_axis` (`stream.rs:2870`, per-axis unit counts
`:2965-2967`). These are the gates that make the lift safe, and they pass on `main` today.

---

## 4. Product-surface strings that become lies

Searched the whole repo (including gitignored `.delta/` worktrees) for `4:2:2`,
"decodes 4:2:0 and 4:4:4", "not ported", "is not supported".

- `crates/ec-av1/src/stream.rs:1808` and `crates/ec-av1/src/refusal_inventory.rs:150` / `:530` —
  the refusal string itself. Removed with the guard.
- `crates/ec-av1/examples/gen_coverage_cells.rs:33-37` ("the shape this decoder refuses by
  name"), `:256`, `:309` ("4:2:2, the shape this decoder refuses by name") — a **generator's**
  user-visible message. It will print a false claim.
- `crates/ec-av1/src/stream.rs:1779-1781` and `:1791` — the guard's own doc comment ("the decoder
  hardcodes 4:2:0 everywhere", "4:2:2 (ss_x != ss_y) is not [decoded]").
- `crates/ec-av1/src/stream.rs:2302-2308` — the refusal test's doc.
- `crates/ec-av1/src/stream.rs:1384-1388` — `SeqFlags`' doc: "these exist only for the
  sequence-level refusal ... nothing downstream reads them". Post-lift false; the whole
  per-axis port invalidates it.
- `crates/ec-av1/src/stream.rs:2759-2763`, `:2816-2821`, `:2997-3002`, `:3323-3326`, `:3403-3406`
  — five doc blocks saying "with the header refusal standing, NO committed test can decode a
  4:2:2 stream".
- `crates/ec-av1/src/stream.rs:8560-8570`, `:10122-10128` — the two tripwire gates' prose.
- `crates/ec-av1/src/decode.rs:23391-23398`, `:2841-2844`, `:16991-16992`.
- `crates/ec-av1/src/refusal_inventory.rs:137-149`, `:1732-1736`, `:2007-2011`.

**No public format declaration to correct.** `crates/ec-av1/src` exposes no `VideoFrame` /
`PixelFormat` surface (grep for `PixelFormat|VideoFrame` in the crate: no matches), and
`crates/ec-core/src/frame.rs:17, 23, 99` already carries `I422` / `I210` with
`chroma_shift() -> (1, 0)` at `:99`. There is no README and no docs directory in
`crates/ec-av1`. So the "user-visible text" surface is: the refusal error, the generator's
message, and comments. There is no CLI banner and no capability table to lie. `[confidence:
high — the search was repo-wide and the crate has no such file]`

---

## 5. ORDERED LIFT RISK LIST — check these FIRST, quietest failure first

**R1. `stream.rs:1805` is already `if false &&`.** Quietest failure of all: everything below
looks like it "already works" and the guards are dead. Catch: `git diff --stat
crates/ec-av1/src/stream.rs` and `grep -n 'if false &&' crates/ec-av1/src/stream.rs`. **First
action, before any other.**

**R2. `film_grain.rs` synthesises the wrong 4:2:2 picture, silently.** Bottom half of the
chroma plane never grained; top half grained with a 4:2:0 row map and a 2x2 `avgLuma` quad. No
panic, no counter, no refusal. Catch:
`ffmpeg -f lavfi -i testsrc2=size=320x240 -pix_fmt yuv422p … | aomenc --profile=2
--film-grain-test=5 --obu` then compare our `decode_stream` output against ffmpeg's default
decode of the same bytes. If aomenc refuses to emit 4:2:2 grain, the cell is latent-only and the
site should be closed with a named assertion at `film_grain.rs:39`, not left as a comment.
Secondary check: `crate::film_grain::grain_hits()` must be `> 0` on that stream, or the gate is
the "params-without-apply" false green the 12-bit gate already guards against
(`stream.rs:16626-16629`).

**R3. 12-bit 4:2:2 is codable and has ZERO census coverage.** At profile 2 and 12 bits both
subsampling bits are coded (`sequence.rs:479-485`), so `(1,0)` at 12 bits is a legal header. The
sibling report's 51-cell corpus has 10-bit but **no 12-bit 4:2:2 cell**, and the sweep recipe
it quotes (`lanes/av1422ffmpegbase.report.md:95-98`) passes `--input-bit-depth=$d` with `d ∈
{8,10}` only. Nothing in this tree has ever decoded a 12-bit 4:2:2 stream. Catch: the same
`ffcensus.py` comparator with `-pix_fmt yuv422p12le` over a 12-bit 4:2:2 encode.

**R4. The 7 known-diverging cells.** `s422_320x246`, `s422_322x240`, `s422_322x246`,
`s422_352x242_10b`, `s422_384x240`, `s422_416x242_10b`, `s422_416x250_10b`
(`lanes/av1422ffmpegbase.report.md:157-171`). Six are chroma-only, all at non-8-aligned
geometries. The assignment says `s422_384x240` is the last one and belongs to another lane;
the other six are **not** attributed to anyone in that report (§7: "It does not attribute any
of the 7 divergences"). Whoever lifts must decide whether "lift with 6 known-red cells" is
acceptable, because the gate in §4.5 of that report is 51/51. Catch: `ffcensus.py cells_ff.json`
as written.

**R5. `440_request_is_422.obu` is a 4:2:2 header over a 4:2:0 tile.** Post-lift it does not
refuse — it decodes with wrong pixels or panics in a chroma extent. It is the one pin whose
misleading content is *inside* the committed tree. Catch: `cargo test -p ec-av1 --lib
the_440_cell_is_not_a_codable_chroma_shape` immediately after the guard change, and expect a
panic, not a clean failure.

**R6. The `440_request_is_422.obu`-shaped test constructions generally.** Same defect at
`stream.rs:2321-2334`: any test that bolts a 4:2:2 header onto this crate's 4:2:0 encoder
output cannot be inverted into an exactness gate. Enumerate them before editing — this is the
class where a careless "invert the assert" produces a green gate over garbage.

**R7. `refusal_inventory.rs:2129-2148` silently stops covering 4:2:2.** No failure, no output
change. Catch: add `(1,0)` to the loop and re-run
`cargo test -p ec-av1 --lib every_chroma_unit_a_64_axis_strip_can_present`; the `checked == 8`
assert must move to 12 and every new unit must be in `arms`.

**R8. No 4:2:2 unit census, so the non-vacuity gate is unwritable.** `set_census_444`
(`decode.rs:2337`) enables the unit counters at `ss (0,0)` only. Until a 422 census exists,
a byte-exact 4:2:2 gate can be green while exercising nothing. Catch: assert
`census_444_units()` deltas — they will read `[0,0,0]` on every 4:2:2 cell today, which is the
proof that the counter family is not the right one to gate on.

**R9. Superres + 4:2:2 has no cell either.** The sweep recipe passes no `--enable-superres`; the
superres upscale path (`decode.rs:37531-37537`, `:37581-37584`) is per-axis and looks right, but
it is unmeasured at `(1,0)`. Catch: one `--enable-superres` 4:2:2 encode through the same
comparator.

**R10. Prose that lies.** §4 above. Zero behavioural risk, but a reviewer reading
`stream.rs:1779` ("the decoder hardcodes 4:2:0 everywhere") after the lift will think the port
never happened. Catch: `grep -n '4:2:2' crates/ec-av1/src crates/ec-av1/examples` and read each
hit.

**R11. The residual `(0,1)` guard.** `sequence.rs:481` makes it unreachable; per repo rule it
closes with an assertion, not a comment. Catch: the assertion itself.

---

## 6. Confidence statement

- **Mechanism, certain** (I read the code): every `path:line` classification in §1a, §1b, §2,
  §3, §4; the film-grain row model; the census arming condition; the guard's current text.
- **Intent, not mechanism** (flagged inline): whether libaom/aomenc can actually emit a 4:2:2
  film-grain stream (R2) — I read libaom's naming, I did not run an encode.
- **Unverified**: the git state of `stream.rs:1803-1805`. This lane has no shell, so I could
  not run `git status` or `git diff`. Finding §0 is a fact about the file, not about the index.
- **Not re-derived, taken from the sibling report**: the 95-cell census and the 7 divergences
  (R4). I did not re-run it.

---

## 0.1 LANDING NOTE (Main, after this study ran)

Two things about §0 and R1, in the report's own framing of "the tree is not what the context says":

1. **The bypass patch in the working tree was real and is now gone.** `crates/ec-av1/src/stream.rs` carried the
   uncommitted `if false && seq.subsampling_x != seq.subsampling_y` plus its `// TEMP-PROBE-BYPASS` comment in the
   PRIMARY checkout, left there by a patch-run-restore measurement. Main restored it (`git checkout --
   crates/ec-av1/src/stream.rs`) and verified: the live file, `HEAD`, and every lane commit that `git log -S`
   flags all read the plain guard again (`grep -c 'if false &&' = 0`; `git log -S` hits were doc-comment mentions,
   and each flagged commit's `stream.rs` contains no bypass). **The near-miss is the finding**: nothing was
   committed, but the patch sat in the primary checkout across several merges, one `git add -A` away from
   committing a silently-admitted format.
2. **The study was landed by Main, not by the lane.** This lane is a read-only scout with no shell and no way to
   write the file, so the report text was carried in its output; Main wrote it here verbatim (only this note is
   added). Branch `lane-av1422liftrisk` therefore does not exist — the file is on `main`.

Everything below stands as written, including the classifications, which were made against the guard as the lane
found it on disk and are unaffected by the restore (the file's *content* was the plain guard's neighbour, not a
different arm).
