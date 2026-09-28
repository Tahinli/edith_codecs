# lane-av1cmpaudit — ours-vs-oracle compare honesty sweep

## The class

An ours-vs-oracle compare that walks `got.iter().zip(want)` **without first asserting
equal lengths** reports EXACT for any defect outside the compared prefix: `zip` stops at
the shorter operand, so the walk is silently truncated and the `filter(|(a, b)| a != b)
.count() == 0` form returns a PASS. Measured in this repo before the sweep: a comparator
hardcoded to a 128x96 geometry zipped 36864 samples against a 98304-byte oracle dump and
declared a 256x128 witness byte-exact while the divergence sat at x>=108.

A frame-count assert (`ours.len() == sources.len()`) does **not** protect a plane compare.
The canonical SAFE shape already in the tree is `crates/ec-av1/src/stream.rs`'s
`decode_all_frames_vs_oracle` (~8129-8145): assert the frame counts, assert the byte
lengths, then walk.

Second class, same inventory: **decode-order vs display-order indexing.** The instrumented
aomdec's `EC_AV1_FINAL_DUMP` writes one file per DECODED frame in DECODE order (7 files
for a 6-picture encode); `ffmpeg -f rawvideo` emits DISPLAY order (6). Indexing one
against the other fakes a divergence on a byte-exact stream.

## Fix rule used everywhere

A length assert is inserted **after** the existing diff/panic assert, on the same buffers.
That keeps every change behaviour-preserving: a real sample divergence still fails for the
same reason with the same message, and only the previously-silent "our side is short and
the compared prefix matches" case becomes a new loud failure. Frame-count asserts that
guard a loop over frames go **before** the loop, because with unequal counts the loop body
compares a mis-paired set and there is no "same reason" failure to preserve.

## Inventory

106 ours-vs-oracle compare sites. SAFE = a length assert (or a whole-slice `==`/`!=`
compare, which is length-aware) on the SAME buffers in the same block. UNSAFE = the walk
with no such assert. N/A = a zip that is not an ours-vs-oracle compare (production DSP,
iterator plumbing, ours-vs-ours). No site is SKIPPED-by-env: every gate is fixture
presence or an ffmpeg/aomdec binary check, and every one is exercisable in a normal run.

| crate / file | sites | SAFE | UNSAFE | N/A |
|---|---|---|---|---|
| `crates/ec-av1/src/stream.rs` | 44 | 13 | 26 | 5 |
| `crates/ec-av1/src/encoder.rs` | 17 | 4 | 13 | 0 |
| `crates/ec-av1/src/encode.rs` | 22 | 2 | 20 | 0 |
| `crates/ec-h264/tests/conformance.rs` | 11 | 2 | 8 | 1 |
| `crates/ec-vp9/tests/*.rs` | 4 | 3 | 1 | 0 |
| `crates/ec-h265/tests/conformance.rs` | 8 | 8 | 0 | 0 |
| `crates/ec-alac/tests/*.rs` | 2 | 2 | 0 | 0 |
| `crates/ec-flac/tests/*.rs` | 2 | 2 | 0 | 0 |
| `tools/oracle/src/main.rs` | 1 | 1 | 0 | 0 |
| `crates/ec-aac/tests/*.rs`, `crates/ec-ac3/tests/*.rs` | 13 | 1 | 0 | 12 |
| **total** | **124** | **38** | **68** | **18** |

`scripts/**` holds no ours-vs-oracle byte compare: `lr-sgr-pin-harness.c` and
`superres-pin-harness.c` are oracle-side instrumentation the instrumented aomdec calls,
`instrument-aom-oracle.sh` only patches oracle sources, and the `aac-tables/*.py` scripts
probe the reference decoder with an error-string oracle, not a sample compare.

### The UNSAFE sites, by shape

**`filter(|(a, b)| a != b).count() == 0` is the ONLY pixel check** — a short plane gives
0 and the gate passes. `stream.rs` 3522, 6181, 6355, 6616, 6742, 6919, 7083, 7421, 7554,
7623, 6590, 6715, 6891, 7058; `encode.rs` 17386, 17471, 17560, 17630, 17702, 17781, 17848,
17938, 18029, 18090.

**`position(|(x, y)| x != y)` diff reporter with only a frame-count assert** —
`stream.rs` 5971, 6076, 7857, 12932, 33237, 34820, 41790, 41903, 42014, 28107;
`encoder.rs` 3545, 4280, 4350, 4405, 4455, 4521, 4586, 4761, 4873, 4958, 5075, 5232, 5794;
`ec-h264/tests/conformance.rs` 272, 314, 328, 1081, 1301, 2011, 2025;
`ec-vp9/tests/intraonly_exact.rs` 83.

**A helper that returns `None` for "exact" with no length guard** —
`encode.rs::first_difference` (16565). Contrast `first_plane_mismatch` (19161), which
checks length after the zip and so cannot yield a false `None`; that is the model.

**A one-sided length check** — `ec-h264/tests/conformance.rs::round_trip` (688): only
`if ours.len() > expected.len()` was rejected, so a SHORT frame zipped over
`&expected[..ours.len()]` and returned 0. Four call sites print "bit-exact" on `Ok(0)`.

**A three-way `.min()` that truncates silently** — `stream.rs:29369`
(`frame_count.min(frames.len()).min(ffmpeg_frames.len())`), the only
decode-order/display-order-shaped site in the crate.

**Frame pairing with no count assert on either side** — `encode.rs` 15617, 15622 (and the
nine `encoded.frames` vs `ffmpeg_decode_sequence` loops at 17396/17481/17565/17640/17712/
17786/17853/17943/18034 plus 17282).

### Deliberately left alone

- `ec-h264/tests/conformance.rs:157` `fn first_diff` — the primitive itself. Its two
  streamed call sites (1646, 1769) are already SAFE via `reference.resize(frame.len(), 0)`
  + `read_exact`; the seven loose call sites are fixed instead.
- `ec-h264/tests/conformance.rs:1313` — ours-vs-ours (decode order vs display order), an
  anti-vacuity claim (`moved > 0`), not an oracle compare.
- `stream.rs:10704-10718` `mismatched` — a PIXEL mismatch here stays report-only by design
  (the gate deliberately tolerates a pinned second defect). The PLANE SIZE now asserts
  loudly: with a size mismatch every number the ladder prints is meaningless, and that is
  not the defect the gate tolerates.
- `stream.rs:9298, 11248` — `assert_eq!(frames.len(), ffmpeg_frames.len(), ...)` was
  tautological (`ffmpeg_decode_sequence` was handed `frames.len()` as its budget). Deleted
  and replaced by a comment naming the tautology; the helper's own byte-length assert is
  the real guard.
- `ec-aac`, `ec-ac3` — Pearson/lag-correlation and RMS thresholds. A correlation claim is
  not an exactness claim, and shortening an operand changes a threshold result, it does
  not fake a PASS. Their one genuine exactness claim
  (`ec-aac/tests/oracle.rs:617`) is a full `assert_eq!(ours, theirs, ...)`.
- The ~250 remaining ffmpeg pairings in `stream.rs` — SAFE-DISP: `decode_stream` returns
  shown frames in display order and ffmpeg rawvideo is display order, and
  `ffmpeg_decode_sequence` hard-asserts `stdout.len() == frame_bytes * frames`, so passing
  our own `frames.len()` is still a real count assertion against ffmpeg's measured output.

### Truthfulness fixes (no logic change)

- `stream.rs:29006` claimed `// EVERY decode-order frame compared, never just the shown
  ones (class [[gate-blind-to-hidden-frames]])` directly above a loop over
  `decode_stream`'s output, which is the shown/display frames. The gate is exactly the
  class it claimed to cover. Rewritten to say what it covers and that hidden frames are
  NOT covered.
- `stream.rs:7415` — `"{NAME}: decode-order frame count"` on a shown-frame count. Message
  corrected; the assert itself is unchanged.
- `crates/ec-av1/examples/dump_yuv.rs:39` — writes `<out>.f{i}.yuv` where `i` is a DISPLAY
  index, wearing the `.f{N}` name that means DECODE index everywhere else in the crate.
  A comment now says so and points at `decode_all_frames_vs_oracle`.

### Stale-dump hazard

Already handled at the one in-tree site that reads dump files:
`decode_all_frames_vs_oracle` `remove_dir_all`s its scratch dir (~8087) before writing
both sides' `aom.f{i}` and `ours.f{i}`, so no `ours.f*` can survive from a previous run
next to a fresh `aom.f*`. The three other decode-order dump rungs
(`EC_AV1_DECODE_ORDER_DUMP`, `EC_AV1_PREFILT_DUMP`) have no in-repo consumer — the whole
hazard there lives in the manual procedure, not in a gate. Nothing to fix.

## Proofs

Every fix is a pure insertion, so the red proof is a counterfactual on the SAME code: drop
our side's last sample, run the gate. Red = the new length assert fires. Then revert only
the fix (scoped `git checkout -- <file>`) and re-run the identical mutated code: the gate
PASSES. That pass is the false PASS the fix kills.

**`crates/ec-av1/src/stream.rs`** — `stream::tests::a_skipped_lossless_intrabc_rect_strip_zeroes_its_entropy_bands`
(0.64s, not `#[ignore]`d). Mutation in `decode.rs:34785`: `probe_plane.pop()` on the
key-frame luma.
- red: `panicked at crates/ec-av1/src/stream.rs:7768: assertion 'left == right' failed:
  a_skipped_lossless_intrabc_rect_strip_zeroes_its_entropy_bands: plane 0: ours has 77439
  samples, ffmpeg's has 77440 -- a short plane would zip-truncate the compare above and
  hide a chroma-span defect` / `left: 77439 right: 77440` / `FAILED. 0 passed; 1 failed`.
  The pre-existing `bad == 0` assert did NOT fire — that is the hole.
- false pass: same mutation, `stream.rs` reverted → `a_skipped_lossless_intrabc_rect_strip_zeroes_its_entropy_bands:
  full-frame exact, 1 band resets, 3 skipped intrabc chroma predictions` / `ok`.

**`crates/ec-av1/src/encoder.rs`** — `encoder::tests::a_static_clip_codes_64x64_roots_and_decodes_sample_exact`
(site 4280). Mutation: `g.pop()` on the luma collect.
- red: `panicked at crates/ec-av1/src/encoder.rs:4296: assertion 'left == right' failed:
  frame 0: luma is 32767 samples, ffmpeg's is 32768` / `FAILED. 0 passed; 1 failed; 702
  filtered out`.
- restored: `ok`.

**`crates/ec-av1/src/encode.rs`** — `encode::tests::every_mode_decodes_to_what_the_encoder_predicted`
(the `first_difference` helper fix). Mutation: `let ours = &ours[..ours.len() - 1];`.
- red: `panicked at crates/ec-av1/src/encode.rs:16620: 128x96 mode 0, luma: no sample in the
  shared prefix differs, but ours is 12287 samples and theirs is 12288` / `FAILED`.
- restored: `ok`.

**`crates/ec-vp9/tests/intraonly_exact.rs`** — `intra_only_frame_and_its_successors_match_ffmpeg`.
Mutation: `got.pop()`.
- red: `panicked at crates/ec-vp9/tests/intraonly_exact.rs:84: frame 0: our planes are
  320x240 I420 / left: 115199 / right: 115200` / `FAILED. 0 passed; 1 failed`.
- false pass: same mutation, fix reverted → `ok ... 1 passed` — a 115199-vs-115200
  truncation reported EXACT for all 10 shown frames.
- restored: `ok`.

**`crates/ec-h264/tests/conformance.rs`** — all seven sites proven red and all seven
proven to pass without the fix (each mutation truncated our frame or the oracle's by one
sample; the reverted runs printed `bit-exact` / `12 frames bit-exact` / `ok`).
- `jvt_scaling_matrices_decode_bit_exact` / `round_trip`: red `conformance.rs:865: cqm-jvt:
  176x144 decoded to 38015 bytes, ffmpeg gave 38016`; false pass `cqm=jvt scaling matrices
  bit-exact`.
- `jvt_cavlc_first_idr_bit_exact` (two arms): red `conformance.rs:332: AUD_MW_E: frame is
  38015 samples (176x144), the reference frame is 38016`; false pass `PASS table (26
  bit-exact, 9 refused by name)`.
- `jvt_full_sequence_bit_exact`: red `conformance.rs:1118: AUD_MW_E: frame 1 is 38015
  samples, 38016 expected`; false pass `ok`.
- `p_only_gop_matches_ffmpeg_every_frame` / `compare_sequence`: red `conformance.rs:282:
  frame 1 is 38016 samples, ffmpeg gave 38015`; false pass `cavlc-p: 12 frames bit-exact` /
  `cabac-p: 12 frames bit-exact`.
- `output_is_display_order_and_the_reorder_is_real`: red `conformance.rs:1340: display-order
  frame 1 is 38016 samples, ffmpeg gave 38015`. Note a mid-sequence truncation of OUR
  display frames is already caught pre-fix by the unrelated ours-vs-ours membership check
  at 1313, so the isolation mutation truncates the ORACLE frame.
- `packet_entry_surface_reorders_and_carries_timestamps` (two sites): red
  `conformance.rs:2062`; false pass `ok`.

### Live green, after restore

```
cargo test -p ec-av1 --release --lib -- stream::tests::a_skipped_lossless_intrabc_rect_strip_zeroes_its_entropy_bands \
  encoder::tests::a_static_clip_codes_64x64_roots_and_decodes_sample_exact \
  encode::tests::every_mode_decodes_to_what_the_encoder_predicted
  test result: ok. 3 passed; 0 failed; 0 ignored; 700 filtered out; finished in 3.56s
cargo test -p ec-vp9 --test intraonly_exact --test inter_pixels_exact
  intra_only_frame_and_its_successors_match_ffmpeg ... ok   (1 passed)
  inter_gop_matches_ffmpeg / inter_gop_with_tile_columns_matches_ffmpeg / corpus_1080p_60fps_matches_ffmpeg_past_frame_72 ... ok (3 passed)
cargo test -p ec-h264 --test conformance
  jvt_scaling_matrices_decode_bit_exact ok 0.17s; p_only_gop_matches_ffmpeg_every_frame ok
  0.31s; output_is_display_order_and_the_reorder_is_real ok 0.23s;
  packet_entry_surface_reorders_and_carries_timestamps ok 0.22s;
  jvt_cavlc_first_idr_bit_exact ok 1.68s; jvt_full_sequence_bit_exact ok 5.77s
cargo check -p ec-av1 --lib --tests            -> Finished, no errors, no warnings
cargo check -p ec-h264 -p ec-vp9 --tests       -> Finished (5 pre-existing ec-vp9 lib warnings)
```

Not executed live: the ~30 UNSAFE sites inside `#[ignore]`d ec-av1 gates (they need an
explicit `--ignored` run and aomenc-driven sweeps) and the `EC_AV1_PIN` scratch diagnostic
at `stream.rs:29369`. They are fixed by the identical insertion; the red proof is carried
by a non-`#[ignore]`d sibling in the same file exercising the same assert shape.

## Notes for the parent

- The repo's cargo target runner (`scripts/memguard-runner.sh`, wired in
  `.cargo/config.toml`) fails in this container with `Unit run-p...scope was already loaded
  or has a fragment file`. Pre-existing, unrelated to the diff; `EC_NOMEMGUARD=1` is the
  runner's own documented escape hatch and was used for every run above.
- A scratch `worktree/fixtures -> main checkout fixtures` symlink (untracked, gitignored)
  was needed to make the JVT-vector tests run in the worktree. It is not part of the diff.
- Nothing is pushed. The main checkout at `/home/tahinli/Documents/Code/Rust/edith_codecs`
  is clean (`git status --porcelain` empty).

## Reviewing this commit

`core.hooksPath` points at `~/.omp/agent/hooks/git`, whose `pre-commit` runs
`format-staged.py` over the staged tree. That hook re-flowed `encode.rs` and `encoder.rs`
wholesale at commit time (the reflow is line-BREAK churn, so `git diff -w` does not hide
it: the semantic diff is the ~68 `assert_eq!` insertions, the `first_difference` rewrite,
 the three comment/message corrections and the `examples/dump_yuv.rs` note). Per skill
 `edith-format-churn-commit-hook` the committed, hook-formatted version is the canonical
 tree and is NOT reverted. The hook ran rustfmt with this workspace's edition: the
 committed tree still compiles clean
 (`cargo check -p ec-av1 --lib --tests` -> Finished, no errors, no warnings) and the three
 named green tests above still pass on the committed blobs
 (`test result: ok. 3 passed; 0 failed; 0 ignored; 700 filtered out`).
