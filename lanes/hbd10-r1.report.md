# lane-hbd10 round 1 report

Branch `lane-hbd10`, worktree `edith_codecs-hbd10`, off main `3808cf8` (no rebase
needed -- branch was already at main's HEAD, `git log --oneline -1 main` = 3808cf8).

## Refusals lifted (2)

Both removed from `crates/ec-av1/src/refusal_inventory.rs` and from their
`stream.rs` guard sites, each with a real-aomenc 10-bit gate in the same commit:

- `"a bit depth other than 8 with film grain applied (film_grain.rs's LUT and blend are hardcoded 8-bit)"`
- `"a bit depth other than 8 with use_superres set (superres.rs's upscale_row is hardcoded 8-bit)"`

## What changed

- `crates/ec-av1/src/film_grain.rs:180` -- `GRAIN_MIN`/`GRAIN_MAX` consts replaced by
  `GRAIN_BIT_DEPTH` (thread-local, set once per `apply_grain`) + `grain_range()`:
  `grain_center = 128 << (bit_depth - 8)`, mirroring libaom's file statics
  (`grain_synthesis.c:1043-1045`). All 10 clamp sites (luma/chroma AR filter,
  ver/hor overlap blending) now read the runtime range.
- `crates/ec-av1/src/film_grain.rs:230,265` -- `gauss_sec_shift` was hardcoded
  `4 + grain_scale_shift` (i.e. `12 - 8 + ...`); now `12 - bit_depth + grain_scale_shift`
  (`grain_synthesis.c:468,503`).
- `crates/ec-av1/src/film_grain.rs:~296` -- new `scale_lut()`, a verbatim port of
  `scale_LUT` (`grain_synthesis.c:616-626`): the scaling LUT stays 256 entries at
  every bit depth, so the index is `>> (bit_depth-8)` and the two neighbouring
  entries are interpolated with the dropped low bits (`x == 255` short-circuits).
  Used at all three blend sites (Y, Cb, Cr) in `add_noise_to_block`.
- `crates/ec-av1/src/film_grain.rs` `add_noise_to_block` -- `cb_offset`/`cr_offset`
  now `(offset << (bd-8)) - (1 << bit_depth)` (was `- 256`); the scaling-index clamp
  is `(256 << (bd-8)) - 1` (was `255`); `clip_to_restricted_range` legal ranges and
  the unrestricted max are `<< (bd-8)` (`grain_synthesis.c:752-795`).
- `crates/ec-av1/src/film_grain.rs:551` -- new `GRAIN_HITS` counter + `grain_hits()`
  (class `gate-blind-to-feature`), incremented once per grained picture.
- `crates/ec-av1/src/superres.rs` -- `upscale_row`/`upscale_plane`/`upscale_picture`
  take `&[u16]`/`bit_depth` instead of narrowing the (already `u16`) `Picture` planes
  to `u8` and back; the only bit-depth-dependent step in spec 7.16 is the output
  clamp, now `clamp(0, (1 << bit_depth) - 1)` (`clip_pixel_highbd`). Filter table,
  `Round2`, step/x0 and the real-margin edge padding are unchanged. The three
  libaom-pinned unit tests were retyped to `u16` and pass `bit_depth = 8`, so the
  8-bit pins still hold byte-for-byte.
- `crates/ec-av1/src/stream.rs` -- both `bit_depth != 8` refusal blocks deleted;
  `apply_grain` (2 call sites) and `upscale_picture` (2 call sites) now receive the
  sequence header's real `bit_depth`.
- `crates/ec-av1/src/gate_coverage.rs` -- unchanged, deliberately: it derives its set
  from `--enable-*` flags only, and neither new gate enables or disables a tool that
  was not already covered. `every_gate_disabling_a_tool_is_a_listed_coverage_hole`
  stays green with the two new gates in the derived gate set.

## Gates added (both hard-asserting, neither can SKIP on a mismatch)

`crates/ec-av1/src/stream.rs`:

1. `a_real_aomenc_10bit_film_grain_stream_decodes_pixel_exact` --
   `aomenc --bit-depth=10 --input-bit-depth=10 --film-grain-test=1` over a
   `yuv420p10le` lavfi gradients source, pixel-exact vs `ffmpeg_decode_sequence_10bit`
   (ffmpeg synthesizes grain by default; no grain-disable flag exists anywhere in
   this file). Hard-asserts the parsed `bit_depth == 10`, `apply_grain == true`,
   `num_y_points > 0` (an empty scaling LUT would blend a zero delta and make the
   gate vacuous), and `grain_hits()` moved.
2. `a_real_aomenc_10bit_superres_stream_decodes_pixel_exact` --
   `aomenc --bit-depth=10 --superres-mode=1 --superres-denominator=12
   --superres-kf-denominator=12`, same tool-disable envelope as the existing 8-bit
   superres gate. Hard-asserts `bit_depth == 10` and `superres_hits()` moved.

Shared helpers `encode_10bit_gradients` / `parsed_bit_depth` factored out next to them.

EVIDENCE: /tmp/claude-1000/.../tasks (test stdout) | `EC_AV1_REQUIRE_AOMENC=1 cargo test -j3 -p ec-av1 --lib 10bit_ -- --nocapture` | `test result: ok. 3 passed; 0 failed` -- `grain_hits=1`, `superres_hits=1`, all three planes exact

### Sensitivity check (the gates are not vacuous)

Temporarily passed a literal `8u32` instead of the real `bit_depth` at both call
sites (mutation reverted before commit):
- film-grain gate: panics in `film_grain.rs` `scale_lut` (a 10-bit sample indexes
  past the 256-entry LUT) -- proves the grain path really runs over 10-bit samples.
- superres gate: `assertion left == right failed: luma vs ffmpeg`, ours saturated at
  `255` where ffmpeg has 10-bit values -- proves the clamp is the thing under test.

EVIDENCE: /home/tahinli/.claude/projects/-home-tahinli-Documents-Code-Rust-edith-codecs/b6d8a07f-96a4-4bbb-b378-af9ae25cf7c9/tool-results/b1odbi79e.txt | forced `bit_depth = 8` at both call sites, ran the two gates | both FAILED (LUT overrun / luma saturated at 255), both pass again with the mutation reverted

## His films: does either actually use grain or superres?

Extracted 2s (`-t 2 -c:v copy -an -f obu`, bounded) of each and counted header
flags with a throwaway `examples/hbd10_headers.rs` (deleted after the run; the
0.4s extract the charter names is only 1.8 KB / a handful of OBUs at 2160p, too
short to be representative, so the window was widened to 2s).

| film | seq bit_depth | film_grain_params_present | enable_superres | frames | apply_grain | num_y_points>0 | use_superres |
|---|---|---|---|---|---|---|---|
| Hunger Games (Ballad, 2160p HDR10) | 10 | true | **false** | 50 | **25** | 25 | 0 |
| Troy (Director's Cut, 1080p) | 10 | true | false | 49 | 0 | 0 | 0 |

**Hunger Games really does carry film grain**: half its frames in the first 2s set
`apply_grain` with a non-empty luma scaling LUT, at 10 bit. That refusal was a real
blocker-in-waiting for his library, and it is gone. **Neither film uses superres**
(`enable_superres = false` in both sequence headers, `use_superres = 0` on every
frame) -- the superres refusal is lifted and gated, but it is not on his films' path.

`decode_probe` on those extracts (this branch): both films still stop earlier than
grain/superres, at partition coverage -- unchanged by this lane, as expected:
- Hunger Games: `REFUSED: unsupported: AV1 tile (a partition below 8x8 (this decoder codes no leaf smaller than 8x8))`
- Troy: `REFUSED: unsupported: AV1 tile (a 32x32 partition type this decoder does not code (value=4))`

EVIDENCE: scratchpad hunger2.obu / troy2.obu | `ffmpeg -t 2 -c:v copy -f obu` then `cargo run -p ec-av1 --example decode_probe` | Hunger Games apply_grain 25/50 frames, use_superres 0/50; both films' first refusal is the pre-existing partition gap

## Test totals

`EC_AV1_REQUIRE_AOMENC=1 nice -n 10 cargo test -j3 -p ec-av1 --lib`
-> **269 passed, 0 failed, 23 ignored, 0 filtered out, 334.60s.**

EVIDENCE: /tmp/claude-1000/-home-tahinli-Documents-Code-Rust-edith-codecs/b6d8a07f-96a4-4bbb-b378-af9ae25cf7c9/tasks/bsumuvd86.output | `EC_AV1_REQUIRE_AOMENC=1 cargo test -j3 -p ec-av1 --lib` | test result: ok. 269 passed; 0 failed; 23 ignored; 334.60s

## Residue

- accepted: 12-bit is code-generic (every formula is `bit_depth`-parameterised, not
  `10`-specialised) but ungated -- no 12-bit gate was written and none of his files
  is 12-bit. A 12-bit stream would decode, not refuse; that is a claim no test backs.
- accepted: grain at 4:2:2/4:4:4 stays out of scope -- `film_grain.rs` is 4:2:0 by
  construction (documented at the top of the module), unchanged by this lane.
- deferred: chroma-only grain params (`num_y_points == 0`, `chroma_scaling_from_luma`)
  at 10 bit are exercised by neither gate -- `--film-grain-test=1` sets luma points --
  what unblocks it: a `--film-grain-table` fixture that pins a chroma-only parameter
  set.
