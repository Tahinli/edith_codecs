# lane/av1oracleguard — the 4:2:0 ffmpeg oracle helpers now REFUSE a non-4:2:0 stream

Base `main` = `b8385c93` ("Merge lane/ecimagewallclock").
Worktree `~/Documents/Code/Rust/edith_codecs-oracleguard`, own
`CARGO_TARGET_DIR=~/.cache/tgt/av1oracleguard`.

## The hazard, and why it was silent

`crates/ec-av1/src/stream.rs`'s 4:2:0 oracle helpers ask ffmpeg for a 4:2:0
pix_fmt and size chroma at `width * height / 4`:

| helper | pix_fmt | chroma span |
|---|---|---|
| `ffmpeg_decode_sequence` (`7033` call site) | `yuv420p` | `w*h/4` |
| `ffmpeg_decode_sequence_10bit` (`7125`) | `yuv420p10le` | `w*h/4` |
| `ffmpeg_decode_sequence_12bit` (`17428`) | `yuv420p12le` | `w*h/4` |

The failure mode is that **ffmpeg does not refuse a mismatched stream — it
CONVERTS**. It emits a 4:2:0 picture, so the helper's own byte-count assert
(`out.stdout.len() == frame_bytes * frames`) passes, the helper returns a `Vec<Pic>`,
and a gate built on it compares CONVERTED chroma against our REAL 4:2:2 chroma.
`lanes/av1subsizesweep.report.md:245` names exactly this: *"a future 4:2:2 gate
using them compares half the oracle's bytes and passes green."*

The correct siblings already existed: `ffmpeg_decode_sequence_422`,
`ffmpeg_decode_sequence_422_depth`, `ffmpeg_decode_sequence_444`.

## The change

One shared guard, called as the FIRST statement of each helper — before the
ffmpeg spawn, so a host without ffmpeg still proves the refusal.

- `stream_subsampling(stream, name) -> (u8, u8)` (`6527` region): the stream's
  OWN `color_config.subsampling_x/y`, read from its sequence-header OBUs with
  `Av1Parser`, using the existing `stream_bit_depth` / `assert_12bit_sequence_header`
  loop. Never inferred from the caller's `width`/`height`.
- `chroma_shape(sx, sy)`: (1,1)→4:2:0, (1,0)→4:2:2, (0,0)→4:4:4, (0,1)→4:4:0.
- `assert_420_oracle_stream(stream, helper, sibling)`: asserts `(sx,sy) == (1,1)`.
  The message names the real subsampling, the chroma shape it means, the reason
  (`CONVERTS` + `width*height/4`), and the sibling to use.

The diff is **256 insertions, 0 deletions, 0 modified lines** — every existing
line is byte-identical, so the 4:2:0 path is unchanged by construction, not by
argument.

Sample of the message actually produced:

```
ffmpeg_decode_sequence REFUSED a (1,0) stream: this helper asks ffmpeg for a
4:2:0 pix_fmt, which CONVERTS the stream's real 4:2:2 chroma down to 4:2:0
instead of failing, then sizes chroma at `width * height / 4` rather than the
4:2:2 shape. A gate built on it would compare converted planes against real
ones and pass green. Use `ffmpeg_decode_sequence_422 (4:2:2) or
ffmpeg_decode_sequence_444 (4:4:4)` for a (1,0) stream.
```

### Class sweep: the 10-bit sibling

`lanes/av1subsizesweep.report.md:245` calls the unguarded set "the 4:2:0 trio".
`ffmpeg_decode_sequence_10bit` is the identical hazard (`yuv420p10le`, same
`w*h/4` span) and got the same one-line guard. It was NOT in the original
assignment's target list; it is reported here rather than silently widened.

`probe.rs::source` (`:81`, listed at the same table row) is deliberately NOT this
class: it loads a clip and *asks* ffmpeg to convert it to `yuv420p`, then asserts
the converted shape. It is a source-card loader, not an AV1 OBU oracle. Left
untouched, deliberately.

## Non-vacuity, both directions

**Direction 1 — the comparator still bites** (`a_420_oracle_comparator_counts_a_flipped_oracle_sample`).
Pin `420_mixll_256x128_6f.obu` (22336 B, fnv1a64 `5694016005779287373`),
8-bit 4:2:0 at 256x128. Asserts the pin's own subsampling is (1,1), decodes it
with `decode_stream`, takes `ffmpeg_decode_sequence(&data, 256, 128, n)`, and
counts per-plane mismatches: **`[0, 0, 0]`**. Then flips exactly one oracle
sample (`flipped[0].u[0] ^= 1`) and re-counts: **`[0, 1, 0]`** — the U count
moved by exactly 1, Y and V unmoved. Measured output:

```
a_420_oracle_comparator_counts_a_flipped_oracle_sample: clean per-plane counts
[0, 0, 0], after one flipped oracle U sample [0, 1, 0]
```

This also IS the "4:2:0 behaviour is byte-identical" evidence: the helper still
produces a picture ours matches sample-for-sample on a real 4:2:0 pin.

**Direction 2 — the refusal fires** (two tests).

- `a_420_oracle_helper_refuses_a_422_pin`: `422_key_64x64.obu` (755 B,
  fnv1a64 `0x87fc569cf53bcbc6`), (1,0), into `ffmpeg_decode_sequence(…, 64, 64, 1)`.
- `a_420_high_bit_depth_oracle_helpers_refuse_a_422_pin`: `s422_352x242_10b.obu`
  (20575 B) into `_10bit`, and `s422_12bit_160x128.obu` (9214 B,
  fnv1a64 `0xa62b8e3fe6f60ae5`) into `_12bit`. Both (1,0).

Each asserts the pin's own subsampling first (so the test cannot pass on a pin
that stopped being 4:2:2), then runs the helper under `catch_unwind` with the
panic hook silenced, and requires the message to contain `(1,0)`, `4:2:2` AND
the sibling name. `#[should_panic(expected = …)]` was rejected: it matches one
substring, so it could not prove the sibling is named.

**Red-before (mutation)**: reducing `assert_420_oracle_stream` to
`fn assert_420_oracle_stream(_: &[u8], _: &str, _: &str) {}` turns BOTH refusal
tests red, and the failure text is the hazard itself:

```
a_420_oracle_helper_refuses_a_422_pin: ffmpeg_decode_sequence ACCEPTED a (1,0)
stream. ffmpeg converted it to 4:2:0 and the helper returned a green-looking
comparison
```

Guard restored; both green again.

## Did any existing test build on a 4:2:0 helper with a non-4:2:0 stream?

**No — none.** Reported, not adapted, as asked.

Two independent checks:

1. A static scan of every test function in `stream.rs` that calls one of the
   three guarded helpers AND references a fixture whose name contains
   `422`/`440`/`444`/`441` returns exactly two rows: this lane's own refusal
   tests. No pre-existing gate feeds a wrong-shape pin to a 4:2:0 helper.
2. A runtime check: `EC_AV1_REQUIRE_FFMPEG=1 cargo test -p ec-av1 --lib -- 422
   440 444 441 chroma_shape subsampling` → **68 passed, 0 failed** (35.9 s).
   If any of those 4:2:2/4:4:0 gates had been silently converted, the new guard
   would have turned them red there.

A third scan checked the four generically-named tests that matched a loose
`422|440|444` regex in their bodies (`a_lossless_libaom_inter_frame_decodes_sample_exact`,
`a_lossless_16x4_chroma_pair_repairs_the_measured_site`,
`a_real_aomenc_rect_strip_palette_decodes_pixel_exact`,
`a_superres_key_frame_with_cdef_and_loop_restoration_decodes_pixel_exact`) — all
four source their fixtures from `-pix_fmt yuv420p`/`yuv420p10le` y4m or from an
8-bit 4:2:0 encode. Genuine 4:2:0 inputs.

## Runs

| run | result |
|---|---|
| `cargo test -p ec-av1 --lib -- <the three new gates>` `EC_AV1_REQUIRE_FFMPEG=1` | **3 passed, 0 failed** |
| same, guard mutated to a no-op | **0 passed, 2 failed** (the two refusal tests) |
| `cargo test -p ec-av1 --lib -- 422 440 444 441 chroma_shape subsampling` | **68 passed, 0 failed** |
| `cargo test -p ec-av1 --lib -- pinned pinned_` | **24 passed, 0 failed, 2 ignored** |
| `cargo test -p ec-av1 --lib -- pixel_exact` | **257 passed, 0 failed** (924 s) |

`EC_AV1_REQUIRE_FFMPEG=1` on every one of them, so a missing ffmpeg could not
silently green a gate. No VPS suite was staged for this lane; the full `ec-av1`
suite is the main agent's call.

## Not closed, stated plainly

1. **No full-suite run.** The 4:2:0 helpers have ~180 call sites and the
   `pixel_exact` family alone is 257 tests. That family and the shape-filtered
   run are green, and both the static scan and those runs say no caller feeds a
   wrong-shape stream — but that is a scoped claim, not a whole-suite claim.
   The remaining filtered-out tests include the `aomenc`-sourced gates, which
   SKIP on this host (no `aomenc` on PATH) and need a VPS run to execute.

2. **`assert_420_oracle_stream` panics; it does not return `Result`.** That is
   deliberate — every oracle helper in this file is assert-based, and changing
   the 8-bit one to `Result` would ripple through 176 call sites. The refusal
   message names the sibling so a caller that trips it has the fix in front of
   it. A `Result`-returning variant would be a separate, larger lane.
3. **`ffmpeg_decode_sequence_444` has no depth-generic sibling.** The refusal
   message says so explicitly ("`ffmpeg_decode_sequence_444` is 8-bit only")
   rather than naming a helper that does not exist at 10/12-bit. A high-depth
   4:4:4 oracle is a real gap, not one this lane closed.
4. **The guard proves the shape, not the depth.** `assert_12bit_sequence_header`
   already covers depth for the 12-bit gates; `_10bit` has no depth assert
   (`stream_bit_depth` exists and could be applied the same way). Out of scope
   here, named so it is not lost.