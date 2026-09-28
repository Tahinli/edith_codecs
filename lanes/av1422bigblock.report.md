# lane-av1-422bigblock — the two witnessed 4:2:2 big-block defects

Branch `lane-av1-422bigblock`, HEAD `213c1eb0` (contains the block-half port).
Target dir `$HOME/.cache/cargo-target-av1422bigblock` throughout. No push.

## Verdict

Both witnessed defects are fixed and both witnesses are pixel-exact vs aomdec.
The panic the ticket predicted for defect 1 no longer reproduces at this HEAD —
the block-half port had already fixed the all-skip inter frame; re-verified, not
assumed. Defect 2 needed three fixes, two of them found below the historical
panic line. The 4:2:0/4:4:4 behavior is unchanged (targeted gates green, t422
frame 0 byte-exact at all four stages). Defect 3 (Main's IRC addition, the
s2.obu crash) is fixed as a crash; s2's remaining divergence is a pre-existing
sub-8 chroma class, fingerprinted for its owning lane. Committed code still
refuses 4:2:2 at the sequence header; the `EC_AV1_ALLOW_422_PROBE` bypass was
applied and reverted per probe run and is **not** in the commit.

## Fixtures (pinned, `git add -f`; `fixtures/` is gitignored)

| fixture | bytes | sha256 | fnv1a64 |
|---|---|---|---|
| `422_allskip_2f.obu` | 62 | `ba4932c14f95971b5f25321b04abce80c9fdd5de4b1fe6fffcdad82aaea87ce1` | `0x7e4d69d3c728c55f` |
| `422_sb128_3f.obu` | 12543 | `0b851799d99cb4d141a7d042138a3999aad7e0fb24d69c670bca1d38c286dce9` | `0x5171e0aab8000da7` |

Provenance: the witness hunt's artifacts under `~/.cache/av1422sb128wit/`
(`422_allskip_2f.obu` = `b422_wit2f.obu`, `422_sb128_3f.obu` = `b422b_3f.obu`;
oracle refs `b422_wit.ref.yuv`). NOTE: the ticket said `422_sb128_3f.obu` is
10175 bytes; the live file is 12543 bytes and its sha256 matches the ticket —
the byte count in the charter is stale, the hash is authoritative.

Gate: `the_pinned_422_bigblock_witnesses_are_present_and_refuse_by_name`
(stream.rs) asserts both byte pins and the refusal-by-name contract.
`decode_stream` still refuses 4:2:2 unconditionally in committed code, so a
committed gate cannot decode these streams; the decode-completes + counters +
pixel-exact assertions were measured through the probe build and are
reproducible from the recipe below. The counters over the sb128 witness:
`chroma422_square: 32`; over s2: `chroma422_square: 68, chroma422_rect: 222,
chroma422_sub8: 24, chroma422_chunk: 40`, `part128 none=3`.

## Reproduce (patch-run-restore, local only)

```
sed 's/if seq.subsampling_x != seq.subsampling_y {/if seq.subsampling_x != seq.subsampling_y \&\& std::env::var_os("EC_AV1_ALLOW_422_PROBE").is_none() {/' -i crates/ec-av1/src/stream.rs
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1422bigblock cargo build -p ec-av1 --example decode_probe --features gate-counters
EC_AV1_ALLOW_422_PROBE=1 EC_PROBE_OUT=/tmp/ours.raw \
  $HOME/.cache/cargo-target-av1422bigblock/debug/examples/decode_probe crates/ec-av1/fixtures/422_sb128_3f.obu
$HOME/.cache/aom-oracle/build/aomdec --rawvideo -o /tmp/oracle.raw crates/ec-av1/fixtures/422_sb128_3f.obu
cmp /tmp/ours.raw /tmp/oracle.raw   # identical
git checkout -- crates/ec-av1/src/stream.rs
```

## Defect 1 — all-skip 422 inter desync: NOT REPRODUCING at HEAD (fixed upstream)

At `213c1eb0` the witness decodes clean: `OK: 2 frames decoded, 128x128`, no
panic, no phantom reads. Evidence:

- Entropy: our `EC_TRACE_COEFF` pairs rng-for-rng with the instrumented
  oracle's — frame 0 codes 20 coefficient units (one nonzero 32x32 luma unit +
  4 stacked-chroma per 32x32 quadrant), identical rng sequence on both sides;
  frame 1 codes **zero** units on both sides (all-skip, as the oracle).
- Pixels: the shown frame is byte-identical to `aomdec --rawvideo`.

The ticket's "~20 phantom units + reconstruct_mc_rect side=64 w=64 h=128 panic"
is the pre-blockhalf state; the per-axis plane-block model fixed both the
phantom reads and the geometry. No code change needed here. The full hunt
stream `b422_wit.obu` (6 frames) also decodes OK at HEAD.

## Defect 2 — 128-root square-cut chroma chunking: three stacked causes

The historical `range end 4128 > 4096` (decode.rs:34279) no longer reproduces;
at HEAD the same block panicked deeper, in `push_mc_rect_tx`'s stride-densify
(`range end index 32 out of range for slice of length 0`). The f2 128x128
PARTITION_NONE non-skip inter block carries all three causes; the oracle codes
16 chroma units (8 per plane, 2 stacked TX_32X32 per 64x64 mu chunk).

1. **Square-cut chunking** (`decode_inter_block`, both the inter-luma and the
   intra-in-inter walk): `chunk_chroma = chroma_side * 64 / side` cut each
   chunk's per-axis `(64>>ss_x) x (64>>ss_y)` chroma into a
   `chroma_side`-square 64x64 — twice the units the oracle codes at 4:2:2.
   Fixed per-axis (`chunk_cw/chunk_ch`, `units_w/units_h`), unit luma spans
   `(32<<ss_x, 32<<ss_y)` via `around_mi_rect`/per-axis
   `record_mi_chroma`, and the assembled grid placement per-axis.
2. **Prediction stride**: `cu_pred = src.offset((cr*chunk + ur*cu_tx) *
   chroma_side + ...)` indexed the prediction buffer at the square
   `chroma_side` (128) while the 4:2:2 buffer is `chroma_stride`-strided (the
   plane block's own width, 64). Chunk (0,0)'s tall unit read buffer row 64 as
   row 32, and chunk (1,0) offset to `len` exactly — the empty slice. Fixed to
   `* chroma_stride`; the assembled grid stays at the square `chroma_side`
   stride the block-tail `mu_chroma_units` re-stamp already indexes per-axis
   (verified unit-for-unit against the walk).
3. **Reference plane resolution** (`RefPix::planes`): a `Picture` carries no
   subsampling field, and the two-way guess (luma-sized = 4:4:4, otherwise
   quarter = 4:2:0) resolved every 4:2:2 reference 64x64. The frame's own
   pixels were right (the crop is ss-aware) — only inter prediction off the
   slot was wrong, so the bottom mu-chunk row predicted from a clamped row-63
   edge (±1–2 diffs over chroma rows 64..127, 3.3–3.6k samples/plane). Fixed
   with a three-way length inference (`w*h` / `halfw*h` / `halfw*halfh`,
   `halfw = (w+1)/2`, `halfh = (h+1)/2` — the three lengths are pairwise
   distinct for every w,h ≥ 1). DISCLOSED 4:2:0 edge: at ODD reference
   dimensions the half-width uses ceil (`(w+1)/2`, matching the picture
   crop's `round_ss`) where the old guess used floor (`w/2`) — ceil is the
   correct stored-crop shape and the change is visible only at odd w/h, which
   no committed fixture exercises.

After the fixes: `422_sb128_3f.obu` and the full `b422_wit.obu` decode
pixel-exact vs aomdec; every coefficient unit of every frame pairs
rng-for-rng with the oracle (29/29, 32/32, 32/32).

Class sweep of the same square-cut model:
- `decode_intrabc_128rect` has the same `cside * 64 / side` square chunk walk —
  **deferred** (same class, unwitnessed: no 4:2:2 intrabc 128-rect witness
  exists; unblock = one aomenc `--enable-intrabc` profile-2 128-SB hunt).
- The 444 branch of the inter walk degenerates to identical values (ss 0/0 ⇒
  per-axis == square, `chroma_stride == chroma_side`), verified by gate.

## Defect 3 (Main's IRC) — s2.obu crash: crash fixed, residual named

`/tmp/sa422/s2.obu` (recipe: `lanes/av1422stripanchor.report.md` "Observed, not
chased"; 420 control `b420_wit.obu`) panicked at `decode.rs:3709` at HEAD — the
same densify site, fed by defect 2's cause 3 (quarter-height reference) and the
intra 128-rect twin of the square-cut walk:

- `decode_block_128rect`'s chroma walk was chunk-square (`cn = 32/chroma_tx`,
  `cu_y = cpy + ch_row*32`, clamp strides 8/8) with a `debug_assert!` that no
  4:2:2 stream can satisfy. Fixed per-axis exactly like the inter walk
  (`tu_reach_rect`, `around_mi_rect`, per-axis record spans), in the SAME
  expression shape the already-reviewed 444 fix landed on lane-av1-444sb
  (`349b3918`): `cn = ((64 >> ss_x)/chroma_tx).max(1)` for the width axis
  plus the added `cn_h = ((64 >> ss_y)/chroma_tx).max(1)` for the height
  axis, per-axis chunk offsets `(64 >> ss)` and clamp strides `(16 >> ss)` —
  at ss(0,0) the code reduces line-for-line to `349b3918`'s form, so the
  merge with that chain is a semantic union. DISCLOSED 4:4:4 behavior change:
  the parent's walk was provably broken at ss(0,0) (one unit per chunk instead
  of four; the stale assert caught it) and this rewrite, like `349b3918`,
  codes the four units — NOT byte-identical to parent at 4:4:4, by design,
  and pinned by `349b3918`'s own witness on the merged tree (see the identity
  section). DISCLOSED 4:2:0 identity: at ss(1,1) every new expression reduces
  to the old constant (32, offsets 32, strides 8, one unit).

Result: s2.obu decodes all 16 frames, no panic (`OK: 16 frames decoded,
256x144`). The crash the charter assigned is closed. **Deferred (named for its
owning lane):** s2's frame 0 diverges from aomdec (~45% of samples, maxabs
~180, Y first diff at sample 126) with an entropy desync: the oracle codes
1089 coefficient units in frame 0, ours 541, first divergence at unit 38 — a
plane-2 **TX_4X8** chroma unit at **mi(12,24)** (oracle `all_zero=0
rng=48745`, ours `rng=41584`). Class: sub-8/odd-strip 4:2:2 chroma under the
128-rect intra path (the `ss_size_lookup` BLOCK_INVALID family), pre-existing
— the base tree panics at frame 0's assert and never produced pixels. At base
the stream could not be measured at all; the pre-fix re-run log is
`/tmp/sa422/re-run-av1422ctxgather.log`. Fingerprint: `/tmp/sa422/s2f0.obu`,
unit 38, mi(12,24), TX_4X8, plane 2.

## 4:2:0/4:4:4 identity + t422

- `a_real_aomenc_10bit_inter_sequence_decodes_pixel_exact` — green.
- `a_real_aomenc_stream_with_two_tile_rows_decodes_through_decode_stream`
  (live aomenc, 420, inter, tiles) — green.
- `a_lossless_sb128_rect_intra_block_decodes_sample_exact` — green. LABEL
  CORRECTION (review finding 2): this stream is 4:2:0 (seq_profile 0 in the
  fixture bytes), NOT 4:4:4 as this report first claimed; it is a 4:2:0
  lossless sb128 gate and proves nothing about 4:4:4.
- 4:4:4 evidence, done properly: `444_sb128rect_lr_witness.obu` (sha256
  `27825e14…`, the lane-av1-444sb witness, 192x160 profile-1, 3 frames) was
  decoded on THIS tree (`OK: 3 frames decoded`) and its output is
  BYTE-IDENTICAL to the parent commit's output on the same stream — measured
  by stash-and-rebuild, all three frames — so the rewrite is proven
  output-neutral on it. The stream itself still diverges from aomdec on this
  ancestry, identically before and after the fix: this branch predates the
  444 chain's chroma-context fixes (`9e77c16b` ss-aware chroma-above skip,
  `76f8c3fe`), the desync starts at frame-0 Y sample 128 (the first
  128-column superblock) and the first 128-rect block never fires downstream
  of it, so `decode_block_128rect` codes no samples on this stream on this
  ancestry. `349b3918`'s own gate
  (`a_444_sb128_root_rect_stream_with_restoration_decodes_pixel_exact`,
  frames 0/1 byte-exact vs aomdec AND ffmpeg) is the merged-tree pin: at
  ss(0,0) this lane's walk reduces verbatim to that reviewed form.
- `a_128_root_block_with_a_real_residual_decodes_exact_through_both_decoders`
  and `a_128_root_compound_block_decodes_exact_through_both_decoders` (solo,
  they pin the process-global superblock size) — green.
- t422 frame 0 (`/tmp/i422/t422_1f.obu`) byte-exact vs the instrumented oracle
  at PREFILT, POSTDEBLOCK, POSTCDEF and FINAL on Y, U and V; t422 16-frame
  stream decodes with the same frame count as the parent (15 shown of 16 frame
  OBUs, unchanged at HEAD — the 16th is an `OBU_TILE_LIST` quirk, not a
  regression).

`cargo check -p ec-av1`: 0 warnings. Committed on the branch, no push.
