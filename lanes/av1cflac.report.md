---
name: lane-av1cflac
base: 3b691e13 (main at lane start; main has since moved to 3aa16dd1)
branch: lane-av1cflac
worktree: /home/tahinli/.cache/wt/av1cflac
---

# lane-av1cflac — `cfl_ac_q3_at` at 4:4:4: **PATH 2, UNREACHABLE.** The
# "188 rung hits on the 4:4:4 set" premise does not reproduce (0 on all 26
# `*444*` pins), the defect is real but latent, and it is now closed with two
# committed non-vacuous gates instead of a fix.

**One line, because the whole lane turns on it:** the 4:4:4 CfL path never
reaches `cfl_ac_q3_at`; `cfl_ac_ss` gives 4:4:4 its own libaom-anchored 1x1
body, so `bw / 2, bh / 2` is correct at every reachable call site and no
behaviour change was made.

---

## 0. Verdict on the inherited premises

| premise (charter / `lanes/av1chromahalvings.report.md` r3.4, r5.0) | status | evidence |
|---|---|---|
| `cfl_ac_q3_at` has no `fctx`/`ss` and `let (cw, ch) = (bw / 2, bh / 2);` | **TRUE** | `decode.rs:19946-19957` on this branch (definition `19946`, halving `19957`) |
| At 4:4:4 it would be wrong twice — extent, and 2x2-vs-1x1 footprint | **TRUE, latent** | §2 |
| "188 rung hits on the 4:4:4 set" | **FALSE — does not reproduce** | §1: **0 hits on 26/26** `*444*` fixtures |
| "the threading through the CfL call tree is what's missing" | **MOOT** | the tree is already ss-aware: `exec_intra` passes `ss_x(fctx), ss_y(fctx)` into `cfl_ac_ss` (`decode.rs:4423`), and `cfl_ac_ss` dispatches 4:4:4/4:2:2 to their own bodies BEFORE the 4:2:0 fallthrough that owns `cfl_ac_q3_at` (`decode.rs:19898`, `19913`, `19937`, `19940`) |
| the 4:2:2 arm's `(bw >> 1, bh)` is inert at 4:4:4 (do not "fix" it) | **TRUE, and inert everywhere** | §1.3: `CFL_AC_422_HITS == 0` on all 85 pins; 4:2:2 is refused at the sequence header (`stream.rs:1752`) |
| main carries an `EC_HALVSWEEP` rung for `cfl_ac_q3_at` | **FALSE** | main `edffad9e` carries ONE rung, `EC_HALV ibc_owned` (main `decode.rs:16854`; my branch predates that merge, so the rung is absent here); `grep -c 'EC_HALV cfl_ac_q3_at'` on main = 0. The cfl rung lived only on the superseded `lane-chromahalvings-r3` (`19890`). My lane keeps exactly one copy. |

Also corrected for the ledger: the `stream.rs` hunk in
`/tmp/leak-main-010352.patch` (the moved `FRAMES` assert, the dropped
`rect_narrow_kern_hits()`) is **not mine** — I never opened `stream.rs` for
edit before that leak. The `decode.rs` hunk in it was.

---

## 1. RED-BEFORE: the 188 does not reproduce; the 4:4:4 CfL path is real and goes elsewhere

### 1.1 The rung, swept over the whole 4:4:4 set

The env-gated rung was taken from the superseded r3 branch (identical lines),
kept env-gated, and landed at `decode.rs:19953`:

```rust
pub(crate) fn cfl_ac_q3_at(
    px: usize, py: usize, bw: usize, bh: usize,
    sample: impl Fn(usize, usize) -> i32,
) -> Vec<i32> {
    hit!(CFL_AC_Q3_AT_HITS);
    if crate::envflags::env_flag!("EC_HALVSWEEP") {
        eprintln!("EC_HALV cfl_ac_q3_at px={px} py={py} bw={bw} bh={bh}");
    }
    let (cw, ch) = (bw / 2, bh / 2);
```

Commands (lane-private `CARGO_TARGET_DIR`, so no cross-worktree binary
clobber):

```text
$ cd /home/tahinli/.cache/wt/av1cflac
$ CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1cflac cargo build -p ec-av1 --example decode_probe
$ export EC_HALVSWEEP=1
$ for f in crates/ec-av1/fixtures/*444*.obu; do ... grep -c "EC_HALV cfl_ac_q3_at" ... done
```

Raw output — **rung column is 0 on every one**; the bracketed column is the
new per-arm counter, which is what the rung could not see:

```text
rung=0  444_intrabc_rect4_witness.obu           [cfl_ac: 444=72  422=0 420sq=0 420rect=0 q3_at=0]
rung=0  444_leaf8_oob.obu                       [cfl_ac: 444=72  422=0 420sq=0 420rect=0 q3_at=0]
rung=0  444_lossy_rect4_inter_witness.obu       [cfl_ac: 444=8   422=0 420sq=0 420rect=0 q3_at=0]
rung=0  444_lossy_rect4_odd_130x122.obu          [cfl_ac: 444=14  422=0 420sq=0 420rect=0 q3_at=0]
rung=0  444_lossy_rect4_wide_256x128.obu        [cfl_ac: 444=8   422=0 420sq=0 420rect=0 q3_at=0]
rung=0  444_lossy_superres_256x128_d12.obu      [cfl_ac: 444=70  422=0 420sq=0 420rect=0 q3_at=0]
rung=0  444_lossy_superres_256x128_d12_10bit.obu[cfl_ac: 444=86  422=0 420sq=0 420rect=0 q3_at=0]
rung=0  444_lossy_superres_256x128_d12_12bit.obu[cfl_ac: 444=212 422=0 420sq=0 420rect=0 q3_at=0]
rung=0  444_lossy_superres_256x128_d9.obu       [cfl_ac: 444=40  422=0 420sq=0 420rect=0 q3_at=0]
rung=0  444_lossy_superres_256x128_d9_12bit.obu [cfl_ac: 444=312 422=0 420sq=0 420rect=0 q3_at=0]
rung=0  444_lossy_superres_mode2_256x128.obu    [cfl_ac: 444=64  422=0 420sq=0 420rect=0 q3_at=0]
rung=0  444_lossy_superres_mode2_256x128_10bit.obu [cfl_ac: 444=58  422=0 420sq=0 420rect=0 q3_at=0]
rung=0  444_lossy_superres_mode2_256x128_12bit.obu [cfl_ac: 444=236 422=0 420sq=0 420rect=0 q3_at=0]
rung=0  444_quad_leaf_tx_type.obu               [cfl_ac: 444=0   422=0 420sq=0 420rect=0 q3_at=0]
rung=0  444_rect_strip_leaf_tx_type.obu         [cfl_ac: 444=0   422=0 420sq=0 420rect=0 q3_at=0]
rung=0  444_sb128rect_lr_witness.obu            [cfl_ac: 444=0   422=0 420sq=0 420rect=0 q3_at=0]
rung=0  ll444-lossless-key.obu                  [cfl_ac: 444=20  422=0 420sq=0 420rect=0 q3_at=0]
rung=0  ll444_128root_lossless.obu              [cfl_ac: 444=32  422=0 420sq=0 420rect=0 q3_at=0]
rung=0  ll444_defaultp_inter_strip.obu          [cfl_ac: 444=20  422=0 420sq=0 420rect=0 q3_at=0]
rung=0  ll444_ibc_rect.obu                      [cfl_ac: 444=216 422=0 420sq=0 420rect=0 q3_at=0]
rung=0  ll444_intrabc_rect_l2.obu               [cfl_ac: 444=10  422=0 420sq=0 420rect=0 q3_at=0]
rung=0  ll444_minp64_128root_control.obu        [cfl_ac: 444=32  422=0 420sq=0 420rect=0 q3_at=0]
rung=0  ll444_minp64_inter.obu                  [cfl_ac: 444=0   422=0 420sq=0 420rect=0 q3_at=0]
rung=0  ll444_minp8_inter.obu                   [cfl_ac: 444=0   422=0 420sq=0 420rect=0 q3_at=0]
rung=0  ll444_rect16x4_chroma_reach.obu         [cfl_ac: 444=4   422=0 420sq=0 420rect=0 q3_at=0]
rung=0  ll444_sb64_1to4_lossless.obu            [cfl_ac: 444=14  422=0 420sq=0 420rect=0 q3_at=0]

TOTAL 4:4:4-SET RUNG HITS: 0 across 26 fixtures
```

**This is the red-before, and it is a negative.** The 4:4:4 CfL path is not
missing: 1834 CfL blocks ran across those 26 streams (arm 0 total = 72+72+8+
14+8+70+86+212+40+312+64+58+236+0+0+0+20+32+20+216+10+32+0+0+4+14), every one
of them byte-exact against the oracle (§4). The inherited 188 is most likely a
4:2:0 count: the same counter on the 4:2:0 set is **33 752** across 17 fixtures
(§1.2), and the largest single 4:2:0 fixture (`gm_small_side_witness.obu`)
alone reports 11 466.

### 1.2 The same counter on the 4:2:0 set — the counter is live

```text
$ for f in crates/ec-av1/fixtures/*.obu; do case 444*) continue;; esac; ... done
420_intrabc_rect4_witness.obu          [cfl_ac: 444=0 422=0 420sq=14   420rect=16  q3_at=30]
gm_small_side_witness.obu              [cfl_ac: 444=0 422=0 420sq=6208 420rect=5258 q3_at=11466]
grain_cdef_lr_128x128.obu              [cfl_ac: 444=0 422=0 420sq=36   420rect=20  q3_at=56]
hg_arf_witness.obu                     [cfl_ac: 444=0 422=0 420sq=146  420rect=46  q3_at=192]
hg_head_mvclamp_witness.obu            [cfl_ac: 444=0 422=0 420sq=334  420rect=132  q3_at=466]
hg_intra14_witness.obu                 [cfl_ac: 444=0 422=0 420sq=3074 420rect=4074 q3_at=7148]
hg_kf900.obu                           [cfl_ac: 444=0 422=0 420sq=2522 420rect=3058 q3_at=5580]
hg_rect64_intra16x4_witness.obu        [cfl_ac: 444=0 422=0 420sq=144  420rect=46  q3_at=190]
hg_ss300_key_frame.obu                 [cfl_ac: 444=0 422=0 420sq=448  420rect=484  q3_at=932]
hg_ss600_key_frame.obu                 [cfl_ac: 444=0 422=0 420sq=460  420rect=546  q3_at=1006]
intrabc_tx4_chroma_kf.obu              [cfl_ac: 444=0 422=0 420sq=474  420rect=0   q3_at=474]
palette_screen_leaf8_witness.obu       [cfl_ac: 444=0 422=0 420sq=16   420rect=0   q3_at=16]
sbpart-pin.obu                         [cfl_ac: 444=0 422=0 420sq=8    420rect=0   q3_at=8]
superres_alltools_sb128_320x180.obu    [cfl_ac: 444=0 422=0 420sq=94   420rect=4   q3_at=98]
superres_kf_cdef_lr_64x64.obu          [cfl_ac: 444=0 422=0 420sq=24   420rect=2   q3_at=26]
troy_kf2700.obu                        [cfl_ac: 444=0 422=0 420sq=1008 420rect=1222 q3_at=2230]
troy_sb128_inter_witness.obu           [cfl_ac: 444=0 422=0 420sq=1516 420rect=2318 q3_at=3834]
                                                       4:2:0 total q3_at = 33752
```

`444` and `q3_at` are **mutually exclusive on every one of the 43 streams that
carry CfL at all.** That is the whole finding.

### 1.3 The 4:2:2 arm is inert too — the "do not fix it" note, proven not asserted

`CFL_AC_422_HITS` is 0 on all 85 pins. Two independent reasons, both
structural: `ss_x == 1 && ss_y == 0` (4:2:2) is refused by name at the
sequence header (`stream.rs:1752`), and the pair `(0, 1)` is **structurally
uncodable** — `ec-av1-syntax` reads `subsampling_y` only when
`subsampling_x == 1` (`crates/ec-av1-syntax/src/sequence.rs:481-485`). So
`cfl_ac_ss`'s residual `else` is exactly 4:2:0, by construction and not by
measurement.

---

## 2. Why the defect is latent: the dispatch, and what the reference says

`cfl_ac_ss` (`decode.rs:19889`) is the whole CfL AC call tree on the decode
side, and its arm order is the guard:

```rust
fn cfl_ac_ss(y, px, py, bw, bh, ss_x, ss_y) -> Vec<i32> {
    if ss_x == 0 && ss_y == 0 {        // 19998  4:4:4
        ... ac[row*bw+col] = y[...] << 3;  // 1x1, FULL extent
    } else if ss_x == 1 && ss_y == 0 { // 19913  4:2:2
        let (cw, ch) = (bw >> 1, bh);      // 2x1 horizontal pair, full height
    } else if bw == bh {               // 19937  4:2:0 square
        cfl_ac_q3(y, px, py, bw)           // -> cfl_ac_q3_at
    } else {                           // 19940  4:2:0 rect
        cfl_ac_q3_rect(y, px, py, bw, bh)  // -> cfl_ac_q3_at
    }
}
```

That is libaom's own dispatch, transcribed:
`cfl_subsampling_lbd` (`~/.cache/aom-oracle/src/av1/common/cfl.c:325-336`)
returns `cfl_luma_subsampling_420/422/444_lbd` on `(sub_x, sub_y)`, and the
three bodies are `cfl.c:230` (2x2 average, `<< 1`), `cfl.c:243` (horizontal
pair, `<< 2`), `cfl.c:257` (1x1, `<< 3`). `cfl_ac_ss` has all three already.
`cfl_ac_q3_at` is only libaom's 420 body, reached only when the two format
tests have already declined.

The extent arithmetic is not a near-miss either. libaom's subtract-average is
indexed by tx size with `round_offset = num_pel/2` and `num_pel_log2`
(`CFL_SUB_AVG_FN`, `cfl.h:183-221`); our `(sum + num_pel/2) >> num_pel.trailing_zeros()`
is the same identity for every power-of-two block, and the 4:2:0 arms are
byte-identical to it today (§4).

**So `bw / 2, bh / 2` and the 2x2 footprint are CORRECT at every reachable
call site.** Two independent call-site sets, both enumerated mechanically by
gate 2 below:

* decode side — `cfl_ac_q3` (`decode.rs:19876`) and `cfl_ac_q3_rect`
  (`decode.rs:11613`) have exactly one caller each, `cfl_ac_ss`, and only from
  its last two arms;
* encoder side — `encode.rs:6943`, whose every `ColorConfig` in the crate
  hardcodes `subsampling_x: 1, subsampling_y: 1` (`encode.rs:2191-2192`,
  `encoder.rs:77-78`); `grep -c "subsampling_x: 0"` over both files = 0.

---

## 3. What landed (no behaviour change, no refusal introduced)

Three files, **+220 / −35** (the −35 are my own intermediate iterations being
replaced; no foreign line is touched — `git diff -U0` hunks are all inside
`decode.rs:5981`, `19899-19953` and `stream.rs:47505-47767`):

| file | what |
|---|---|
| `crates/ec-av1/src/decode.rs` | five gate counters (`CFL_AC_444/422/420_SQ/420_RECT/Q3_AT_HITS`) + `cfl_ac_arm_hits() -> [usize; 4]` + `cfl_ac_q3_at_hits()`, one `hit!` per `cfl_ac_ss` arm and one at `cfl_ac_q3_at`'s entry, plus the env-gated rung |
| `crates/ec-av1/src/stream.rs` | gate 1 `cfl_ac_444_routes_to_its_own_1x1_luma_body_and_420_to_the_2x2_average`, gate 2 `cfl_ac_q3_at_is_reached_only_through_the_420_fallthrough_of_cfl_ac_ss`, and the helper `oracle_plane_diffs` (per-plane Y/U/V mismatch counts — `decode_all_frames_vs_oracle` names an offset but not a plane, and for a chroma defect the plane split IS the diagnosis) |
| `crates/ec-av1/examples/decode_probe.rs` | one census line so the sweep in §1 is reproducible from outside the crate |

**Gate 1 — measured, two-sided, with a pixel compare.** Two pinned fixtures,
each asserting the arm its format MUST take is `> 0` (so the `== 0` is a
routing fact, not an absence of CfL), the counter it must NOT touch, and
`0/0/0` per-plane mismatches against the instrumented `aomdec`:

* 4:4:4 `fixtures/444_lossy_superres_mode2_256x128.obu` — 16620 bytes, fnv
  `0x8cbb_36b2_5e0f_07ab`, sha256
  `c18496cf3b7db0feca9aa904024cec2115be80b5b94f87c848ac52f9b32ffd97`,
  aomenc `testsrc2=size=256x128:rate=25 -pix_fmt yuv444p --passes=1
  --end-usage=q --cq-level=20 --cpu-used=2 --superres-mode=2 --limit=4 --obu`,
  4 frames;
* 4:2:0 control `fixtures/420_intrabc_rect4_witness.obu` — 890 bytes, fnv
  `0x57ec_3da2_a63b_403a`, sha256
  `d01352af5059919cdce733abdcbae8e8240ac3c2a6190fa1bd96d3c6f945b979`, 1 frame,
  640x480.

Both are already pinned by existing gates; no fixture was added and none was
encoded on the VPS fleet (there was no witness to hunt for — the claim was
about reachability, and reachability was the thing that turned out to be
false).

**Gate 2 — the static closure, the way `lane-av1rect8x16` closed its `(4, 8)`
refusal.** Not a comment: a source enumeration that asserts (1) `cfl_ac_q3_at`
has exactly 3 call sites — 2 in `decode.rs`, 1 in `encode.rs`; (2) both
`decode.rs` wrappers have exactly one caller, inside `cfl_ac_ss`; (3) inside
`cfl_ac_ss` the 4:4:4 and 4:2:2 tests come BEFORE both wrapper calls, so the
format-blind average stays the fallthrough; (4) neither `encode.rs` nor
`encoder.rs` contains `subsampling_x: 0`, so the encoder cannot hand it a
4:4:4 block.

---

## 4. Before / after, per plane

There is no fix, so "before" and "after" are the same tree — that is the
finding, and the numbers are here to be checked rather than believed:

| fixture | format | CfL blocks | `cfl_ac_q3_at` | Y mismatches | U mismatches | V mismatches |
|---|---|---|---|---|---|---|
| `444_lossy_superres_mode2_256x128.obu` | 4:4:4 | 64 (arm 0) | **0** | 0 / 32768 | 0 / 32768 | 0 / 32768 |
| `420_intrabc_rect4_witness.obu` | 4:2:0 | 30 (arms 2+3: 14 + 16) | 30 | 0 / 307200 | 0 / 76800 | 0 / 76800 |

The 4:4:4 denominator is the upscaled 256x128 plane; the 4:2:0 denominators are
640x480 luma and 320x240 chroma. Raw gate line, green:

```text
cfl_ac_444_routes_to_its_own_1x1_luma_body_and_420_to_the_2x2_average: fixtures/444_lossy_superres_mode2_256x128.obu arm0 [64, 0, 0, 0], cfl_ac_q3_at 0, per-plane Y/U/V mismatches vs aomdec 0/0/0 of 256x128; sha256 c18496cf3b7db0feca9aa904024cec2115be80b5b94f87c848ac52f9b32ffd97
cfl_ac_444_routes_to_its_own_1x1_luma_body_and_420_to_the_2x2_average: fixtures/420_intrabc_rect4_witness.obu arm2 [0, 0, 14, 16], cfl_ac_q3_at 30, per-plane Y/U/V mismatches vs aomdec 0/0/0 of 640x480; sha256 d01352af5059919cdce733abdcbae8e8240ac3c2a6190fa1bd96d3c6f945b979
ok
```

The 4:4:4 arm is byte-exact on much heavier witnesses too, re-quoted unchanged
on this branch:

```text
a_444_lossy_superres_12bit_d9_stream_decodes_pixel_exact: 4 decode-order frame(s) (0 hidden) byte-exact vs aomdec at 12-bit (256, 128) upscaled from [(9, 228) x4] per frame, 4 upscale(s), 1956 scaled MC block(s)
a_444_lossy_rect4_strip_stream_decodes_pixel_exact_at_odd_and_wide_geometries: fixtures/444_lossy_rect4_odd_130x122.obu 4 decode-order frame(s) byte-exact vs aomdec, 12 own-extent 1:4 chroma gather(s), 28 1:4 inter strip(s)
a_444_lossy_rect4_strip_stream_decodes_pixel_exact_at_odd_and_wide_geometries: fixtures/444_lossy_rect4_wide_256x128.obu 4 decode-order frame(s) byte-exact vs aomdec, 24 own-extent 1:4 chroma gather(s), 76 1:4 inter strip(s)
a_444_lossy_superres_mode2_random_denom_stream_decodes_pixel_exact: 4 decode-order frame(s) byte-exact vs aomdec at (256, 128) upscaled from [(11, 186), (14, 146), (15, 137), (9, 228)], 2265 scaled MC block(s)
```

**4:2:0 CfL gates re-quoted unchanged** (all pass on this branch, no edit
touches their code path):

```text
a_real_libaom_cfl_stream_decodes_pixel_exact .............................. ok
a_real_film_key_frame_with_a_skipped_cfl_block_decodes_pixel_exact ...... ok  (pixel-exact, skipped_cfl=1)
the_hunger_games_ss300_key_frame_skipped_cfl_decodes_pixel_exact ........ ok  (1 skipped sub-8x8 CfL chroma block, frame pixel-exact)
```

---

## 5. Mutation proof — the gate is not a green that survives anything

Three mutations, each reverted, each with the restore verified byte-identical
(`diff <(git diff) /tmp/av1cflac-green2.patch` → identical).

**M1 — make the latent defect live (4:4:4 arm calls the 4:2:0 body).**
`decode.rs:19899-19912` replaced with
`cfl_ac_q3_at(px, py, bw, bh, |lx, ly| i32::from(y.data[ly * y.width + lx]))`.
Naive form (half extent, as the defect literally is) does not even reach an
assertion — it is a **hard crash**, which is the honest release framing
(skill `release-invariant-not-debug-assert` §2):

```text
thread 'stream::tests::cfl_ac_444_routes_to_its_own_1x1_luma_body_and_420_to_the_2x2_average' panicked at crates/ec-av1/src/decode.rs:4455:16:
index out of bounds: the len is 16 but the index is 16
test result: FAILED. 0 passed; 1 failed
```

So the "silent wrongness" framing in the inherited report is wrong twice over:
a live 4:4:4 route would **panic in the reconstruct path**, not silently
mis-colour. Extent-preserving form (`bw*2, bh*2` with a clamped reader, so
the vector length stays right) reds the routing assertion by name:

```text
panicked at crates/ec-av1/src/stream.rs:47568:17:
assertion `left == right` failed: ... fixtures/444_lossy_superres_mode2_256x128.obu is 4:4:4 and entered the
format-blind cfl_ac_q3_at 64 time(s) -- its extent is bw/2, bh/2 and its footprint is a 2x2 luma average, both wrong
at ss (0,0) (libaom cfl.c:230 vs :257). Arm deltas [64, 0, 0, 0]. Either cfl_ac_ss lost its 4:4:4 arm or a new caller
reached cfl_ac_q3_at directly; both make this site LIVE
  left: 64
 right: 0
test result: FAILED. 0 passed; 1 failed
```

**M2 — a fourth call site** (`cfl_ac_q3_at_mutation_probe` delegating to
`cfl_ac_q3_at`, `#[allow(dead_code)]`) reds gate 2's enumeration:

```text
panicked at crates/ec-av1/src/stream.rs:47681:9:
assertion `left == right` failed: ... cfl_ac_q3_at's call-site set changed -- {"decode.rs": 3, "encode.rs": 1}
against an expected 2 in decode.rs (the cfl_ac_q3 / cfl_ac_q3_rect wrappers) and 1 in encode.rs ...
  left: {"decode.rs": 3, "encode.rs": 1}
 right: {"decode.rs": 2, "encode.rs": 1}
test result: FAILED. 0 passed; 1 failed
```

**Restored green** (`cargo test -p ec-av1 --lib --no-run` clean, then):

```text
test stream::tests::cfl_ac_444_routes_to_its_own_1x1_luma_body_and_420_to_the_2x2_average ... ok
test stream::tests::cfl_ac_q3_at_is_reached_only_through_the_420_fallthrough_of_cfl_ac_ss ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 763 filtered out
```

`cargo check -p ec-av1 --all-targets` → `Finished`, 0 warnings.

---

## 6. Boundary note — which files, versus the other lane

I touched **`crates/ec-av1/src/decode.rs`, `crates/ec-av1/src/stream.rs`,
`crates/ec-av1/examples/decode_probe.rs`, and this report.** Nothing else.

I did **not** touch `decode_intrabc_owned_rect` (definition `decode.rs:16843`
on this branch, the `let (cw, ch) = (bw / 2, bh / 2);` at `16861`; on post-merge
main the same function is at `16823` with the `EC_HALV ibc_owned` rung at `16854`) — one `git diff -U0` hunk list proves it: my decode.rs hunks are
at `5981` and `19899`, `19919`, `19938`, `19941`, `19953`. That function is the
*other* half of the same latent class and it stays with its owning lane; the
difference is that one is genuinely unreachable (this report) and the other is
reachable-but-refused on a 4:4:4 rect chroma shape pending a `TxbSet`
(`lanes/av1chromahalvings.report.md` r5.2).

**Edit-path leak, for the next lane (it cost this lane two recoveries).** The
`edit` tool resolved my RELATIVE path `crates/ec-av1/src/stream.rs` against the
session cwd — the MAIN checkout — not the caller's worktree, and put a 249-line
gate into main twice. Detected by `git -C <main> status --porcelain` after
each batch, preserved at `/tmp/av1cflac-gate-leak.patch`, restored with
`git checkout --`, and re-applied inside the lane with
`git apply --3way`. Every edit in this lane after that point used an absolute
`/home/tahinli/.cache/wt/av1cflac/...` header.

---

## 7. Handed back

The latent-defect row, for whoever makes 4:4:4 rect chroma reachable (the
`ChromaRect32x64` / `ChromaRect64x32` `TxbSet`):

> **`cfl_ac_q3_at`** (`decode.rs:19946`) computes libaom's **4:2:0** CfL AC: a
> 2x2 luma average per chroma sample (`cfl_luma_subsampling_420_lbd_c`, cfl.c:230)
> over a `bw/2 x bh/2` extent, average-subtracted (`CFL_SUB_AVG_FN`, cfl.h:183).
> It has **no `fctx`/`ss`**, so at `ss (0,0)` it is wrong on **both** counts:
> chroma extent equals luma extent, and the footprint is 1x1 (`<< 3`, cfl.c:257)
> rather than 2x2. It is **unreachable at 4:4:4 today** — `cfl_ac_ss` returns
> 4:4:4 and 4:2:2 from their own bodies before the 4:2:0 fallthrough — and two
> committed gates now say so (`cfl_ac_444_routes_to_its_own_1x1_luma_body_and_420_to_the_2x2_average`,
> `cfl_ac_q3_at_is_reached_only_through_the_420_fallthrough_of_cfl_ac_ss`).
> **When a 4:4:4 rect chroma block can reach it, threading `ss` is the fix and
> the pixel gate is already there** — mutation M1 above is that state, and it
> is a panic, not a colour shift.

Not done, deliberately:

* **no `fctx`/`ss` threading.** At every reachable call site the current
  arithmetic is right; threading would be an unmeasured change to a shared
  helper on a path Main classified as unreachable, and this project does not
  take unmeasured decoder changes (same reasoning as
  `lanes/av1merge-wave3d.report.md:225`).
* **no new fixture, no VPS encode.** Nothing to hunt: reachability was the
  claim, and it was false. Both gate fixtures are already-pinned aomenc
  artifacts.
* **no 4:2:2 arm change.** `(bw >> 1, bh)` in `cfl_ac_ss` is proven inert
  (§1.3) and left exactly as found.
