# lane-av1-hdr — the header/reference/segmentation refusal cluster, dispositioned

Base: `main` **0cfb9af5** (2026-09-19). Worktree `edith_codecs-av1hdr`, branch
`lane-av1-hdr`, `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1hdr` (private).
No push. Region: `stream.rs` header/reference/segmentation guards plus the
header-level `decode.rs` guards — the block-decoder regions `lane-av1-mono`
owns were NOT touched.

## TL;DR

Every entry in this cluster is now **dispositioned**, and the inventory has
**no unproven refusal left**:

- **lifted + witnessed: none.** The one liftable-looking refusal
  (`a frame mixing lossless and lossy segments`) turned out to be a
  BLOCK-decoder feature, not a header one — measured, not assumed (§2).
- **pinned with a real-stream WITNESS (new):** the mixed-lossless refusal, the
  only inventory entry that had no proving test. New gate
  `a_real_aomenc_mixed_lossless_segment_frame_is_refused_by_name` generates the
  stream, proves it really mixes, shows libaom decodes it, and pins our
  refusal by exact string.
- **proven spec-conformant (libaom errors identically):** primary-ref empty
  slot, `show_existing_frame` empty slot, reference selected with no picture,
  inter frame with no key frame before, no mode-info grid. The libaom
  counterpart is cited per entry in §3.
- **capability claim pinned with new enumeration evidence:** filter intra on a
  superblock-level HORZ/VERT strip. New test
  `a_sb_level_horz_vert_strip_admits_no_filter_intra_symbol`.
- **kept with an assessment** (real gap, no reachable witness): bit depth 12,
  reference height mismatch, `SEG_LVL_REF_FRAME/SKIP/GLOBALMV`.

Inventory move: **34 REFUSALS (34 proven) + 1 CAPABILITY_CLAIM** — before this
lane it was **34 refusals / 33 proven** (the mixed-lossless entry was the lone
unproven one). Reported by
`refusal_inventory::tests::every_proven_refusal_names_a_test_that_exists`.

## 1. Method

For each refusal: read the refusal's guard, then read **libaom's own
counterpart** in the oracle source (`~/.cache/aom-oracle/src`, the same tree
`scripts/build-aom-oracle.sh` builds). Three verdicts:

- **libaom errors identically** → our refusal is `spec-conformant`; cite the
  libaom site and keep the pin.
- **libaom accepts and we refuse** → real capability gap; try to write a
  witness stream, and if the lift is out of the guard's scope, keep it with the
  assessment.
- **proven unreachable** → keep + annotate with enumeration evidence.

Witness streams come from the real `aomenc` oracle
(`~/.cache/aom-oracle/build/aomenc`); acceptance checks use both `aomdec`
(observational) and our decoder's refusal-by-name.

## 2. The mixed-lossless refusal — pinned with a witness (the lift is a block feature)

**Refusal** (`stream.rs:1624`): `a frame mixing lossless and lossy segments`.

**Reachable.** `aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=0
--aq-mode=1 --good --cpu-use=3 --sb-size=64` codes a frame whose segments
disagree about lossless. Mechanism (`av1/encoder/aq_variance.c:80-89`): variance
AQ sets `SEG_LVL_ALT_Q` **per segment**, and its lossless clamp is guarded by
`if (base_qindex != 0)` — so with a lossless base (`cq-level=0`, `base_qindex
== 0`) a high-variance segment gets a **positive** `SEG_LVL_ALT_Q` (qindex > 0,
lossy) while a flat segment stays at qindex 0 (lossless). Confirmed live on the
generated stream (`EC_HDR=1` `EC_SEGHDR` shows segments 4..7 carrying
`SEG_LVL_ALT_Q` data 1 over a `base_q_idx=0` frame).

**libaom decodes it.** `xd->lossless[segment_id]` is per segment
(`av1/decoder/decodeframe.c:5205`, `xd->lossless[i] = qindex == 0 && …`), and
`read_tx_size` returns `TX_4X4` **before reading any symbol**
(`decodeframe.c:1170`: `if (xd->lossless[xd->mi[0]->segment_id]) return
TX_4X4;`). So a mixed frame is a first-class libaom stream; our refusal names a
**capability gap**, not a spec violation.

**Why it was NOT lifted here (measured).** The lift is per-segment lossless
threading through the BLOCK decoder, which is outside this lane's region (and
inside `lane-av1-mono`'s). Attempted and reverted; the two concrete blockers:

1. `decode::lossless` is a frame-level `bool`. Widening it to the header's
   per-segment `lossless[8]` table is necessary but not sufficient — the block
   reader also fetches it **before** the block's `segment_id` is read
   (`decode_block` computes its lossless tx tables before `read_intra_mode`,
   which is where `intra_segment_id` runs), so the table must be consulted
   after the mode read.
2. ~8 rectangular-strip `tx_depth` readers still read a tx-size symbol for a
   lossless block; libaom returns TX_4X4 without reading one. With only (1)
   applied, the mixed stream desyncs (`a Golomb tail longer than this decoder
   reads`) or returns a picture that differs from ffmpeg from byte 32.

Both are block-decoder edits (`decode_block`, `decode_block_rect`,
`decode_leaf_rect8`, `decode_rect4_16`, …) — the mono lane's files. So the
refusal stays, now carrying the witness and the lift plan in its own doc
comment (`stream.rs`, `lane-hdrlossless`).

**New gate.** `a_real_aomenc_mixed_lossless_segment_frame_is_refused_by_name`
(`stream.rs`): encodes 3 arm recipes, asserts at least one arm really carries a
mixed frame (class `gate-blind-to-feature`), asserts ffmpeg's libaom decoder
accepts it, and asserts our refusal fires by exact string. Live output:

```
arm [--end-usage=q --cq-level=0 --aq-mode=1 --good --cpu-used=3 --sb-size=64]: 1 mixed frame(s), refused by name
arm [--end-usage=q --cq-level=0 --aq-mode=1 --good --cpu-used=5 --sb-size=64]: 1 mixed frame(s), refused by name
arm [--end-usage=q --cq-level=0 --aq-mode=1 --good --cpu-used=6 --sb-size=64]: 1 mixed frame(s), refused by name
```

## 3. libaom-equivalence table (spec-conformant pins)

Each of these already had a **negative gate** proving our refusal fires by
name (they stay in `PROVEN`). This lane adds the missing half: **libaom errors
on the same stream**, so the pin is `spec-conformant`, not a capability gap.

| our refusal | libaom counterpart | verdict |
|---|---|---|
| `primary_ref_frame` at a slot with no saved CDF | `decodeframe.c:5075-5079` → `AOM_CODEC_CORRUPT_FRAME "Reference frame containing this frame's initial frame context is unavailable."` | spec-conformant |
| `show_existing_frame` naming an empty slot | `decodeframe.c:4651-4654` → `AOM_CODEC_UNSUP_BITSTREAM "Buffer does not contain a decoded frame"` | spec-conformant |
| a reference selected with no picture | `decodeframe.c:5008-5031` → `AOM_CODEC_CORRUPT_FRAME "Inter frame requests nonexistent reference"` | spec-conformant |
| an inter frame with no key frame before it | same site — libaom permits a stream to open on an intra-only frame, but a reference resolving to an unset slot errors (`decodeframe.c:5008-5016` comment + error) | spec-conformant |
| a frame with no mode-info grid | libaom derives the mi grid from the frame size (`dec_set_mb_mi`, `decoder.c:57-58` — the frame's `mi_cols`/`mi_rows` from `ALIGN_POWER_OF_TWO(width, 3) >> MI_SIZE_LOG2`; cf. `av1_get_MBs`, `alloccommon.c:32-33`). `compute_image_size`, cited earlier, is AV1-spec pseudocode and is not a libaom symbol; a 0-sized frame is unrepresentable, so no error site exists — no conformant header codes one | spec-conformant / defensive |
| a reference picture whose height does not match | `decodeframe.c:5089-5099` → libaom **scales** (`av1_setup_scale_factors_for_frame`) and only errors on an out-of-range scale (`AOM_CODEC_UNSUP_BITSTREAM "Reference frame has invalid dimensions"`) | **capability gap** (we port only width scaling) |
| a bit depth of 12 | libaom supports 8/10/12 with `CONFIG_AV1_HIGHBITDEPTH`; `read_sequence_header` accepts `twelve_bit` | **intentional gate** (see §4) |
| `SEG_LVL_REF_FRAME/SKIP/GLOBALMV` enabled | libaom **implements** them (`decodemv.c:431,437,1071,1350`) | **capability gap** (see §4) |

Non-empty negative gates backing the first five already exist in `PROVEN`
(`a_show_existing_frame_header_naming_an_empty_slot_is_refused_by_name`,
`an_inter_frame_opening_a_stream_is_refused_by_name`,
`a_selected_reference_with_an_empty_ref_frame_idx_slot_refuses_by_name`,
`an_inter_frame_naming_an_unrefreshed_primary_ref_slot_is_refused_by_name`,
`every_frame_size_a_header_can_code_has_a_mode_info_grid`).

## 4. Kept with an assessment (real gaps, no reachable witness)

### 4.1 `SEG_LVL_REF_FRAME/SKIP/GLOBALMV`

libaom implements all three (they rewrite a block's reference / skip / mode
before the symbols are read: `decodemv.c:431` `SEG_LVL_SKIP`, `:437`
`SEG_LVL_REF_FRAME`/`SEG_LVL_GLOBALMV`, `:1071` `read_ref_frames`, `:1350`
`read_is_inter_block`). Our decoder reads `segment_id` (feeding the quantizer
and loop-filter level) but has no reader for these three, so it refuses by name
rather than desyncing.

**Reachability.** `aomenc` exposes **no** flag that enables them: its only
segmentation producers are the AQ modes, which set `SEG_LVL_ALT_Q` alone
(`aq_variance.c:89`, `aq_complexity.c:118`, `aq_cyclicrefresh.c:630`), and the
mode-overriding features are set only from the **library API** —
`av1_apply_roi_map` (`encoder_utils.c:515`, realtime + ROI), `av1_apply_active_map`
(`encoder_utils.c:557`, `SEG_LVL_SKIP`), and the alt-ref source-overlap case
(`encoder_utils.c:401-413`). `aomenc --help` has no `roi-map`/`active-map`
option (checked live), so no CLI recipe reaches them; the existing PROVEN
negative gate builds the stream by hand.

**Disposition: `kept-with-assessment`** — a real pod reaches a conformant
stream through the library API, so the gap is real; no aomenc witness exists to
prove a lift, and the lift touches the inter block path (mode/reference
selection) outside this lane.

### 4.2 Bit depth 12

libaom supports 12-bit (`CONFIG_AV1_HIGHBITDEPTH`). Our gate is deliberate and
already names its cost in the refusal string: at 12-bit the rounding shifts
change — `warp.rs`'s `REDUCE_BITS_HORIZ` is a hard 3 where libaom uses
`round_0 + max(bd + FILTER_BITS - round_0 - 14, 0)` (= 5 at bd 12), MC
`round_0`/`round_1` change (`convolve.h`), the Wiener rounding bits change, and
neither CDEF nor film grain has a 12-bit path. **Assessment:** lifting cleanly
requires a rounding-shift parameterisation across warp/MC/wiener plus CDEF and
grain 12-bit LUTs, each needing its own byte-exact witness against libaom at
12-bit — a distinct lane's worth. **Disposition: `kept-with-assessment`.**

### 4.3 Reference height mismatch

libaom accepts a reference of a different size and scales both axes
(`decodeframe.c:5089-5099`); we port only **width** scaling (superres), so any
height mismatch refuses. This is stricter than libaom but is **not a
spec-conformant** guard. Reachability from aomenc: superres changes width only;
the only both-axes route is dynamic resize (`--resize-mode=3`), whose streams
currently stop earlier in this decoder (measured: the resize arms either fail to
encode or stop on the pre-existing intrabc-rect refusal), so no witness reaches
it. **Disposition: `kept-with-assessment`.**

## 5. Tail cleanup — the capability claim and the 128-root strip

- **`filter intra on a superblock-level HORZ/VERT strip`** (`decode.rs:12632`),
  the lone `CAPABILITY_CLAIM`: unreachable. The strips it protects are
  `(64, 32)` / `(32, 64)`, and `filter_intra_size_class_rect`'s table admits
  **no arm with a 64 axis** — both fall to its `_ => None` arm
  (`av1_filter_intra_allowed_bsize` caps both sides at 32). New enumeration test
  `a_sb_level_horz_vert_strip_admits_no_filter_intra_symbol` parses **every**
  arm and asserts no side exceeds 32 and that neither strip shape has its own
  arm. Kept as a named pin (repo convention: keep + annotate a
  proven-unreachable guard) with the claim comment extended.
- **`CfL, filter intra or a palette on a 128-root HORZ/VERT intra block`**
  (`decode.rs:14992`): already `PROVEN` by
  `no_128_root_half_reads_a_cfl_filter_intra_or_palette_symbol` (lane-t900 r33)
  — the call site's literal `cfl=false`, the no-128 arms of
  `filter_intra_size_class_rect`/`filter_intra_size_class`, and
  `palette_bsize_ctx_wh`'s `bw > 64 || bh > 64` bound. No change; verified it
  still holds and is registered.

## 6. Gates run

- `cargo check -p ec-av1 --all-targets` → rc 0, **0 warnings**.
- New gates:
  `a_real_aomenc_mixed_lossless_segment_frame_is_refused_by_name` → ok (3 arms,
  1 mixed frame each, refused by name).
  `a_sb_level_horz_vert_strip_admits_no_filter_intra_symbol` → ok.
- Inventory consistency (all ok): `the_decode_path_refuses_exactly_the_listed_cases`,
  `every_proven_refusal_names_a_test_that_exists` (prints
  `34 refusals + 1 capability claims, 34 proven`),
  `capability_claims_are_declared_not_scattered`,
  `gates_that_swallow_a_decode_error_are_declared`.
- Full lib suite: `cargo test -p ec-av1 --lib` to completion — **594 passed /
  0 failed / 60 ignored** with the environment sane (§7; 592/60 baseline + this
  lane's 2 gates).

## 7. Suite result

`cargo test -p ec-av1 --lib`, two completed runs on this lane's tree. **Both
runs' single failure was environmental, not a code defect**; every test passes
otherwise, and each failing gate is green on its own once the environment is
fixed.

| run | result | the one failure | cause |
|---|---|---|---|
| 1 (3142 s) | 593 passed / 1 failed / 60 ignored | `a_real_aomenc_stream_with_film_grain_decodes_pixel_exact` | worktree had no `fixtures/` symlink → missing `fixtures/golden6-mismatch.obu` |
| 2 (7161 s) | 593 passed / 1 failed / 60 ignored | `a_1080p_shaped_clip_straddles_at_every_block_size_and_decodes_sample_exact` | `/tmp` tmpfs per-user quota exhausted (`QuotaExceeded`, EDQUOT) by three concurrent ec-av1 suites on the box |

Both fixed and re-verified in isolation:
`ln -s ../edith_codecs/fixtures fixtures` → the film-grain gate `1 passed`;
freeing `/tmp` headroom → the 1080p straddle gate `1 passed; 0 failed`
(26 s). With the environment sane, the suite is **594 passed / 0 failed / 60
ignored** (592/60 main baseline + this lane's two gates). This lane adds no
behavioral decoder code — two tests and disposition comments — so no other
test can move.

Verification protocol for the box: run the ec-av1 suite with the worktree's
`fixtures/` symlink in place and with `df`/`quota` checked first; a
`QuotaExceeded`/EDQUOT panic or a missing-fixture `unwrap` is an environment
fail, not a lane regression (class `gate-skips-on-its-own-failure`, from the
other side).

## 8. Repro commands

```
# build the probe
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1hdr \
  cargo build --release -p ec-av1 --example decode_probe

# the mixed-lossless witness, by hand
ffmpeg -v error -f lavfi -i "testsrc2=size=256x192:rate=25" -t 0.16 \
       -pix_fmt yuv420p -f yuv4mpegpipe - \
 | ~/.cache/aom-oracle/build/aomenc --codec=av1 --passes=1 --threads=1 --limit=4 \
   --end-usage=q --cq-level=0 --aq-mode=1 --good --cpu-used=3 --sb-size=64 \
   --obu -o - - > mixed.obu
EC_HDR=1 decode_probe mixed.obu     # EC_SEGHDR: SEG_LVL_ALT_Q over base_q_idx=0
decode_probe mixed.obu              # REFUSED: a frame mixing lossless and lossy segments
ffmpeg -v error -f obu -i mixed.obu -f null -   # libaom accepts it (rc 0)

# the new gates
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1hdr \
  cargo test -p ec-av1 --lib -- --nocapture \
  a_real_aomenc_mixed_lossless_segment_frame_is_refused_by_name \
  a_sb_level_horz_vert_strip_admits_no_filter_intra_symbol
```

## 9. Git log

- `lane-av1-hdr` off `0cfb9af5`: the witness gate, the enumeration test, the
  disposition comments, this report.

## 10. MERGE-SIDE FOLLOW-UPS

**Merge.** `e03f7173` (`ec-av1: merge lane-av1-hdr`), parents
`c8ac9f46 5bb35ca3` — a real merge, not a fast-forward: `main` had advanced
past the base with the sibling `lane-av1-mono` (`7806fd0d`, follow-ups
`c8ac9f46`). Resolution was an **auto-merge with no conflicts**; the two sides
of `stream.rs` were disjoint (mono rewrote the `SeqFlags`/`set_mono` region and
replaced the pilot's monochrome pin with its pixel-exact witness; this lane
added comments and its own witness at the end of the test module). **Both
sides were kept**: mono's pin stays dropped and its
`a_real_libaom_monochrome_key_frame_decodes_pixel_exact` is present; this
lane's comments and `a_real_aomenc_mixed_lossless_segment_frame_is_refused_by_name`
are present.

**Lane P3 fixes (commit `5bb35ca3`, on the lane before the merge).**

- `refusal_inventory.rs`: `a_sb_level_horz_vert_strip_admits_no_filter_intra_symbol`
  silently skipped the square-delegate guard arm (`_ if bw == bh =>
  filter_intra_size_class(bw),`). The parser now recognises that arm and the
  test additionally drives `filter_intra_size_class(32)` / `filter_intra_size_class(64)`
  directly, so a widening of the delegate turns the gate red. Doc comment
  updated to say what it enumerates.
- `lanes/av1hdr.report.md` §3: `compute_image_size` is AV1-spec pseudocode, not
  a libaom v3.13.3 symbol; the mi-grid derivation is now cited as
  `dec_set_mb_mi` (`decoder.c:57-58`; cf. `av1_get_MBs`, `alloccommon.c:32-33`).
- **The reviewer's third finding was withdrawn** (author retraction, verified):
  it claimed the 12-bit warp `reduce_bits_horiz` is 4, not 5. With
  `FILTER_BITS = 7` (`aom_dsp/aom_filter.h`, the only definition),
  `intbufrange = 12 + 7 - 3 + 2 = 18 > 16` → `round_0 = 3 + 2 = 5`, and
  `warped_motion.c:310`'s assert gives `round_0 >= 5`. §4.2's "= 5 at bd 12"
  stands; **no edit made**.

**Create-list audit.** Exactly four paths: `decode.rs` (+12, comments only),
`refusal_inventory.rs` (the enumeration test), `stream.rs` (comments + the
witness), and `lanes/av1hdr.report.md` (new). No junk/duplicate file. The
`decode.rs`/`stream.rs` edits add **no decoder behavior**.

**Merged-tree gates** (`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1hdrmerge`,
worktree `fixtures/` symlink in place):

- `cargo check -p ec-av1 --all-targets` — **0 warnings**.
- `cargo test -p ec-av1 --lib refusal_inventory` — **14 passed / 0 failed /
  0 ignored** (640 filtered out).
- The new gates, run together: `1..3 passed / 0 failed` —
  `refusal_inventory::tests::a_sb_level_horz_vert_strip_admits_no_filter_intra_symbol` ok;
  `stream::tests::a_real_aomenc_mixed_lossless_segment_frame_is_refused_by_name` ok
  (each of the 3 arms carried **1 mixed frame, refused by name**);
  `stream::tests::a_real_libaom_monochrome_key_frame_decodes_pixel_exact` ok
  (4:2:0 twin and monochrome **60 frames each, byte-exact vs ffmpeg**) — mono's
  witness, re-run on the merged tree.
- **decode_probe 10-bit sha-parity** (`fixtures/hbd-r5/hunger.obu`, 3840x1608,
  72 shown frames, `EC_PROBE_OUT16`): lane `5bb35ca3` and merged `e03f7173`
  both produce `sha256
  d7214bb733b4a6ca2d946d0fcb4849691191b694164383db110a4d9183436784`
  (1,333,739,520 B of yuv420p10le); `cmp` reports IDENTICAL. The merge is
  behavior-identical on the high-bit-depth path.

**Full merged-tree suite NOT re-run**: the verifier already completed it on the
lane (594 passed / 0 failed / 60 ignored, 8083 s); the merge resolution touched
no decoder code (comments + tests only), and the 10-bit sha-parity above is the
behavior-identity proof.

**Cleanup.** Worktree `edith_codecs-av1hdr` removed (branch `lane-av1-hdr`
kept at `5bb35ca3`); private target dirs
`$HOME/.cache/cargo-target-av1hdrmerge*` and the `$HOME/.cache/tmp-av1hdrmerge`
TMPDIR removed.

**Tripwire for the next lane.** `a_real_aomenc_mixed_lossless_segment_frame_is_refused_by_name`
pins today's refusal of a frame mixing lossless and lossy segments (the
per-segment lossless / `TX_4X4` block-decoder path is not lifted here). The lane
that lands that lift MUST flip this pin to a witness, or it merges a stale
refusal pin. (`lane-av1-mono` already flipped its monochrome pin.)
