# lane-av1-llinter2: 4:4:4 lossless inter side — sibling panic ported (r1)

## Charter

The lossless INTER panic named by `av1llband2` (`reconstruct_mc_rect`,
decode.rs:2589, `read_inter_rect_chroma`'s branch) is the same panic
sibling lane `av1llinter` fixed in 84acbd09 + 13a262a6. Port that fix;
never author a second fix for the same defect. 4:2:0 lossless stays
identical; the key-frame gate stays green. Work in
`edith_codecs-av1llinter2` @ 6da581a8.

## Same-panic proof (measured, before any edit)

Repro on the sibling's pinned bytes `dodge2.obu` (37658 B, sha256
`5a070fa351a3f2109de855234f2b3d0a872970f3e7f658db21f65c12eeb135b4` —
byte-identical to the sibling's committed
`crates/ec-av1/fixtures/ll444_minp64_inter.obu`):

- RELEASE build: `panicked at crates/ec-av1/src/decode.rs:28454:32:
  range end index 24 out of range for slice of length 16` in `exec_mc` —
  line 28454 is `reconstruct_mc_rect`'s coded-arm `dst..dst + w` index
  (fn at 28413), inlined into `exec_mc`. The sibling's parent-tree panic
  was the identical message and numbers at their decode.rs:28336, same
  fn, same inline frame.
- DEBUG build: the underlying unit defect surfaces first — the lossless
  WHT assert `(8, 8) != (4, 4)` at `TxParams::run` (decode.rs:2566), via
  `decode_inter_block8 -> read_inter_plane -> push_mc_rect_tx` — the
  sibling report's exact chain. The band2 report's "decode.rs:2589"
  citation is this same worker (`strided[row * self.stride..][..self.w]`,
  the release-form panic of the same 8-wide lossless unit); the
  "reconstruct_mc_rect" name is the release-build site proven above.

⇒ same defect, same root cause the sibling named: the 8x8 inter leaf's
chroma plane block read as ONE TX_8X8 unit where lossless codes TX_4X4
on every plane (`av1_get_tx_size`). 84acbd09 IS the writer's fix.
Ported it; no second fix invented.

## Port (faithful cherry-pick of 84acbd09, three deliberate deltas)

`git cherry-pick -n 84acbd09`:

1. `decode_rect_split` conflicts (fix 4) resolved to THIS tree's version:
   lane-av1-llband2 already ported that exact fix (01a34bc8) with the
   `RECT_SPLIT_LOSSLESS_CHROMA444_HITS` non-vacuity counter; semantics
   identical (`4 << ss` spans, span-carrying `ll_chroma_units`).
2. **fix 5 EXCLUDED**: its three hunks are inside
   `decode_rect4_16_strip` (the `cu_mi` divisor and both
   `record_mi_chroma` spans), a function owned by a live lane — reverted
   to HEAD byte-for-byte. The strip fn still carries the 4:2:0-hardcoded
   walk at 4:4:4 lossless: deferred(fix-now in the owning lane, same
   span derivation as fixes 2-4).
3. The sibling's lane report file dropped; this lane writes its own.

Applied verbatim: fix 1 (`decode_inter_block8`'s single-ref AND compound
chroma arms route `chroma_side == 8 && lossless && !mono` through the
new `leaf8_inter_chroma_lossless` helper wrapping the per-unit
`read_inter_chroma_lossless` walk, with the tail `record_mi`'s chroma
half saved/restored — `saved_chroma_ctx`, the `saved_luma_ctx` pattern;
both `record_mi` tails patched), fix 2 (`read_inter_chroma_lossless`:
`cu_mi` divisor `4 >> ss`, record spans `4 << ss`), fix 3
(`decode_inter_block`'s `mu_chroma_units` lossless replay: extents
`>> ss`, mi-steps and spans `cu << ss`). All byte-identical at 4:2:0
(`4 << 1 = 8`). Off-limits files untouched: `decode_rect4_16_strip`,
`restoration.rs`, intra write-back. Net diff vs HEAD: 9 hunks, earliest
at decode.rs:28734 — nothing before `read_inter_chroma_lossless`.

Gate port: `git cherry-pick -n 13a262a6` (fixture
`fixtures/ll444_minp64_inter.obu` 37658 B, the `ffmpeg_decode_sequence_444`
helper, gate `a_lossless_444_min_partition64_inter_stream_decodes_pixel_exact`
with the `chroma_split_tx_hits` non-vacuity assert and FNV/len pins).
stream.rs conflict = both lanes appended tests at the same spot; resolved
by keeping BOTH (band2's key-frame gate first, then the helper + inter
gate).

## Verification (measured, this tree @ HEAD+2)

- `a_lossless_444_min_partition64_inter_stream_decodes_pixel_exact`:
  **PASS** — all 6 dodge2 frames pixel-exact through BOTH the oracle
  `aomdec --rawvideo` (oracle at `~/.cache/aom-oracle/build`, arm
  executed, not skipped) and ffmpeg `yuv444p`; counter moved (non-vacuity).
- Key-frame gate stays green: `a_real_aomenc_lossless_444_key_frame_
  decodes_sample_exact` PASS.
- 4:2:0 unchanged: `a_lossless_libaom_key_frame_decodes_sample_exact`,
  `a_lossless_libaom_inter_frame_decodes_sample_exact` — both PASS
  (4 passed, 0 failed in the battery run).
- `decode_probe dodge2.obu`: `OK: 6 frames decoded`, no panic, debug AND
  release.
- `cargo check -p ec-av1 --all-targets`: 0 warnings, 0 errors.

## Same-class suspects, deferred (spoken, not hidden)

- `decode_rect4_16_strip`'s lossless chroma walk still hardcodes the
  4:2:0 span (`oy / 2`, `record_mi_chroma(.., 8, 8, ..)` at decode.rs
  ~14631/14653/14675): excluded per lane ownership.
  deferred(fix-now in the owning lane; shape = fixes 2-4's `4 << ss`).
- Default-partition `ll444.obu` no longer panics but stops at a NAMED
  non-panic gap: `EC_RECTCHROMA_GAP luma=64x32 tx=4x4 chroma=64x32`
  (decode.rs:10126 — the `(64, 32)` chroma plane block has no TxbSet row
  at the rect64 lossless route). The sibling's gate deliberately does not
  claim this stream either. deferred(fix-now in a lane chartering the
  rect64 chroma lossless route; distinct defect, not this panic).
