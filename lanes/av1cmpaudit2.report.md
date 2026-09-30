# lane-av1cmpaudit2 — audit of the class `comparator-that-never-reads-the-oracle`

Base `9d5f7bd9`. Question: which committed gates and which recent lane
reports depend on a comparison that cannot fail?

**Answer: none in the crate. One report headline was unverifiable, and is
now re-measured. The whole class is now pinned by ONE committed control.**

## 1. The class

`lanes/av1422remeasure.report.md` claimed "0 wrong on 18/18 cells, 0 of
2359296". Its per-cell counter packed OUR decoded samples into a buffer and
compared that buffer against the same samples; `aomdec`'s output was written,
length-checked, and never read. Retracted in
`lanes/av1422remeasure.RETRACTION.md`. Cells that differ from `aomdec` in
17658 samples reported zero.

The reason this class is dangerous is that it is invisible from the outside.
The helper's signature, its panic text and its return type are all exactly
what a working comparator has. "Read the source and check" is not a control,
because the retracted counter's source read fine too.

## 2. Per-helper table — the crate's own comparators

Every comparator in `crates/ec-av1/src/stream.rs` that reads an external
oracle. Line numbers are this branch's tip (`277327e3`), which adds the
control test of §6 and the thread-local oracle override. Method: located every `std::fs::read` of an oracle-written buffer in
the crate (16 `ref_raw`/`want`/`aom_dump` bindings), then read each
comparison loop. Class is LIVE only if the oracle buffer is the operand of a
content comparison, not just of a length check.

| # | comparator | location | reads oracle bytes at | classification |
|---|---|---|---|---|
| 1 | `assert_rawvideo_matches` | `stream.rs:5536` | reads `ref_raw` at 5568, compares `ours.iter().zip(&ref_raw)` at 5588 + 5595 | **LIVE** |
| 2 | `decode_all_frames_vs_oracle` | `stream.rs:10250` | reads `aom.f{i}` into `want` at 10301, `ours.f{i}` into `got` at 10302, `got.iter().zip(&want)` at 10310 | **LIVE** |
| 3 | `count_rawvideo_diffs` | `stream.rs:10345` | reads aomdec rawvideo into `want` at 10369, `ours.iter().zip(&want)` at 10386 | **LIVE** |
| 4 | `key_frame_diffs` | `stream.rs:48354` | reads `aomkf.f0` into `want` at 48384, `got.iter().zip(&want)` at 48398 | **LIVE** |
| 5 | `oracle_plane_diffs` | `stream.rs:49392` | reads `aom.f{i}` into `want` at 49440, `got[a..b].zip(&want[a..b])` at 49452 | **LIVE** |

Plus 13 gates that inline the same two-buffer comparison rather than calling a
helper — 7 rawvideo sites (`stream.rs` 7505, 8019, 8208, 9009, 9154, 9291,
9718; each does `let want = &ref_raw[…]` then `plane.iter().zip(want).filter(|&(&a, &b)| a as u8 != b)`)
and 6 final-dump sites (50415, 50551, 50674, 50783, 50935, 51054; each does
`our_dump[at..at+PLANE] == aom_dump[at..at+PLANE]`). All **LIVE**.

There is no `assert_final_dump_matches` in the crate under that or any other
name; searched by name and by role (every `std::fs::read` of an aomdec-written
path). `encode.rs`'s `ffmpeg_decode` / `ffmpeg_decode_sequence` /
`ffmpeg_decode_sequence_10bit` all spawn `ffmpeg` and take its stdout
(`encode.rs:16503`, `18587`; `stream.rs:5797`, `5888`) — **LIVE**, and their
comparators `first_difference` (`encode.rs:17906`) and `first_plane_mismatch`
(`encode.rs:20744`) zip two independent `Picture`s — **LIVE**.

Swept outside `ec-av1` too: `crates/ec-vp8`, `ec-h264`, `ec-h265`, `ec-hw`
all reach ffmpeg through a real `Command::output()` and compare the stdout;
no self-compare found.

## 3. Per-report table

| report | headline | comparator | reads oracle? | non-vacuity control | verdict |
|---|---|---|---|---|---|
| `av1chromarect-r3` | "TOTAL 0 wrong from 2359296" (§1) | a temporary probe test, **removed before commit, no path quoted** — UNKNOWN by its own admission | not inspectable | §3 mutation (`chroma_window`'s `cn_cols == 1 && cn_rows == 1` → `if true`) reproduces **79969 wrong**, and both committed gates go FAILED with the gate's own panic text | **LIVE by proxy; headline RE-DERIVED, see §4** |
| `av1dcunit` | "0 wrong … 0 of 2359296" (§2) | `~/.cache/cmp444.py`, quoted in §2, **present on disk** | YES — reads `aom.f{f}` and `ours.f{f}` separately, `o[at+s] != a[at+s]` | §5 M1 (unit readers back to block `reach`) → RED, "U … 130 samples wrong", the committed gate's own panic text | **LIVE** |
| `av1dcunit` | "f0: Y 0 / U 130 / V 30 of 65536" (§5) | "my own script", **no path quoted**; the surviving `cmp444.py` hardcodes `W = H = 512` and 3 frames, so it cannot be it | not inspectable | the committed gate covering the same witness reproduces **U = 130** under the lane's own M1 | **LIVE by proxy; V = 30 not independently reproduced** |
| `av1ibcfork` | "fully byte-exact", 0 of 307200 per plane (§7/§8) | committed gate `a_444_intrabc_rect4_witness_is_byte_exact_after_the_skip_arm_footprint` (`stream.rs:50613`): 4:2:0 arm calls `decode_all_frames_vs_oracle` at 50639, 4:4:4 arm reads `aom_dump`/`our_dump` at 50674/50675 and compares them inline at 50686/50690 | YES, both arms | feature-reach assert `hits > 0` at 50669; §8 mutation of `(bw >> ss_x, …)` → `(bw/2, …)` reds at sample 51360 (x=160, y=80) | **LIVE** |
| `av1loss444` | "EXACT 6/6, 0 wrong luma/U/V"; "0 across all 36 frame-dumps" | `cmp_final.py`, quoted verbatim in §9 as `~/.cache/loss444/cmp_final.py`, **present on disk** | YES — `sa` = oracle plane, `sb` = ours, `bad = [j for j in range(len(sa)) if sa[j] != sb[j]]`, with a length guard | §4 two bit-flip controls on the real dumps: "Y 2 / U 2 / V 2, first (110,26)" and "U 1 wrong with Y 0, V 0" | **LIVE** |
| `av1txctxband` | "37229 / 37229, zero divergence"; "99/99 exact-entropy" | `~/.cache/sb128fork/cmp444.py` (present) for pixels; `hunt5.py` (present) for the entropy ladder | YES — both scripts read their two trace/dump sets independently | mutation of the RECT reader's four lines → FAILED 0 passed; 1 failed | **LIVE** |
| `av1txctxband` | the gate's primary arm is a counter comparing two of OUR OWN models (`decode.rs:28536`, `28635`) | oracle-free by design | n/a | it is a mutation detector, not an exactness claim, and the gate additionally calls `decode_all_frames_vs_oracle` | **LIVE (not a pixel claim)** |
| `av1sb128fork` | entropy fork table | `hunt4.py`, present, reads both traces | YES | stated | **LIVE** |
| `av1sb128fork` | §1 pixel table "841/1295/1372", "56986/55974/56442" | **no comparator named anywhere in the report, and no committed gate at all** | not inspectable | none stated | **UNKNOWN — treat as unverified, see §5** |
| `av1422anom` | "17658 wrong chroma of 593920" on 320x232 | `count_rawvideo_diffs` (`stream.rs:10345`) | YES | `the_counting_oracle_diff_reports_a_real_difference` decodes a witness KNOWN to differ and asserts non-zero | **LIVE** |
| `av1422remeasure` | "0 wrong on 18/18" | the retracted throwaway counter | **NO** | none — that is why it was retracted | **TAUTOLOGICAL (already retracted)** |

## 4. Re-measured headline

`av1chromarect-r3`'s headline comparator was deleted, so I re-derived the
number with an independently written comparator that reads two separately
produced dump sets: `aomdec --codec=av1` with `EC_AV1_FINAL_DUMP` for the
oracle, and the crate's own `examples/dump_yuv.rs` for ours, on the committed
`crates/ec-av1/fixtures/r512.obu` (6948 bytes, 3 frames, 4:4:4 512x512,
262144 samples per plane).

```
f0: Y=0 U=0 V=0
f1: Y=0 U=0 V=0
f2: Y=0 U=0 V=0
TOTAL wrong: 0 of 2359296
```

**The headline survives.** `av1dcunit` §5's "V 30" was not reproduced (its
U = 130 is reproduced by the committed gate under the lane's own M1); its §2
headline is directly re-checkable and is LIVE.

## 5. Claims that must be treated as unverified

1. `av1sb128fork` §1's pixel table — "Y 841 / U 1295 / V 1372 of 65536" and
   "Y 56986 / U 55974 / V 56442" — names no comparator and commits no gate.
   **Unverified.** Its entropy-fork table is separately LIVE.
2. `av1ibcfork` §5's by-hand red-before numbers (`U 72945`, `V 64865`).
   Produced by an off-repo manual diff through `decode_probe`, which is not a
   comparator. The committed claim on the same fixture is LIVE and
   independently re-runnable; the by-hand figures are not.
3. `av1dcunit` §5's `V 30 wrong` (the U = 130 half is reproduced by the
   committed gate's own panic under mutation M1).
4. `av1chromarect-r3` §1's probe script itself — deleted, unpathed,
   unrecoverable. The NUMBER is re-derived above and the two committed gates
   are LIVE.

## 6. The control this lane landed

`stream.rs` — `every_oracle_comparator_reds_on_a_one_byte_wrong_oracle`.

`aomdec_path()` grows a thread-local override (same shape as the existing
`FINAL_DUMP_PREFIX`, and thread-local because the crate denies `unsafe_code`
and a process-global env var would leak one test's tampered oracle into every
sibling's comparison on another thread). The test points that override at a
shim which runs the REAL `aomdec` and then rotates byte 7 of the
`--rawvideo` output and of `EC_AV1_FINAL_DUMP` frame 0 — one byte, inside the
Y plane, everything else a genuine decode. On the byte-exact
`420_odd65x65_key.obu` pin:

```
arm 1, real oracle     -> all five comparators green
arm 2, one-byte-wrong  -> all five comparators red
```

All five helpers named in §2 are covered by that one test. Four gates that
reached the oracle without `lock_gate_counters()` now take it
(`a_real_aomenc_segmentation_stream_with_map_inheritance_decodes_pixel_exact`,
`the_counting_oracle_diff_reports_a_real_difference`,
`the_pinned_420_oddheight_witness_is_pinned_and_ratchets_its_reconstruction_defect`,
`sb128_r2_control_sb64`).

### Mutation proof (the control bites)

All five comparators mutated to the retracted defect — each zipping our
buffer against itself instead of the oracle:

- `assert_rawvideo_matches` 5588 `.zip(&ref_raw)` → `.zip(&ours)`
- `decode_all_frames_vs_oracle` 10310 `.zip(&want)` → `.zip(&got)`
- `count_rawvideo_diffs` 10386 `.zip(&want)` → `.zip(&ours)`
- `key_frame_diffs` 48398 `.zip(&want)` → `.zip(&got)`
- `oracle_plane_diffs` 49452 `.zip(&want[a..b])` → `.zip(&got[a..b])`

Result:

```
every_oracle_comparator_reds_on_a_one_byte_wrong_oracle: 5 of 5 oracle
comparators stayed GREEN against an oracle that is wrong by exactly one
byte -- ["assert_rawvideo_matches", "decode_all_frames_vs_oracle",
"count_rawvideo_diffs", "key_frame_diffs", "oracle_plane_diffs"]
test result: FAILED. 0 passed; 1 failed
```

**Arm 1 passed under the same mutation.** That is the point: the mutants are
invisible to all 60-odd existing pixel-exact gates and to every other
assertion in this crate. Only this control sees them. Mutations reverted;
`cargo check -p ec-av1 --all-targets` clean, no warnings.

## 7. Gates run

```
cargo test -p ec-av1 --lib -- every_oracle_comparator_reds_on_a_one_byte_wrong_oracle \
  the_counting_oracle_diff_reports_a_real_difference \
  the_pinned_420_oddheight_witness_is_pinned_and_ratchets_its_reconstruction_defect \
  the_rawvideo_helper_compares_real_samples_at_the_streams_own_bit_depth \
  a_real_aomenc_segmentation_stream_with_map_inheritance_decodes_pixel_exact
→ 5 passed; 0 failed
```

`EC_AV1_REQUIRE_AOMDEC=1 EC_AV1_REQUIRE_AOMENC=1 EC_AV1_REQUIRE_FIXTURES=1`,
oracle `~/.cache/aom-oracle/build/aomdec`.

## 8. not_done

1. `av1sb128fork` §1's pixel numbers were **not** re-derived. They are
   red-before mutation figures whose comparator was never recorded; deriving
   them means reconstructing the lane's mutation, which is not identifiable
   from the report. Left as unverified rather than guessed.
2. `av1dcunit` §5's `V 30` was not independently reproduced (U = 130 was).
3. No full `ec-av1` suite run — per the wave's rules, scoped tests only. The
   lock added to `sb128_r2_control_sb64` is inside an `#[ignore]`d
   diagnostic, so it is compile-verified but not executed.
4. The entropy-ladder comparators (`hunt4.py`, `hunt5.py`, `align.py`) were
   classified LIVE by reading, not by a control; the class audited here is the
   sample/pixel comparator, and a byte-vs-trace-field variant of the same
   control would be a separate lane.
