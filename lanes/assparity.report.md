# lane assparity: ec-ass parity with ffmpeg

Base: `f3e6c7fa` (main). Branch `lane/assparity`.

## Goal

DONE.md Wave 3 replaced oxideav-ass with ec-ass. Gate G4 needs correctness against an external
decoder, and until now only the cue count was checked. This lane adds an ffmpeg parity test over
synthetic scripts, and checks ec-ass's raw override text against what edith's `Segment::Raw` path
needs.

## Changes

- `crates/ec-ass/tests/data/`: eight synthetic `.ass` scripts, hand-written (no copyrighted media):
  - `styles_tags.ass`: styles, `\b \i \u \s \pos \an \fn \fs \c` and unmodelled tags.
  - `layers_overlap.ass`: layers 0/1/2, overlapping events, same-start ties, a zero-length cue.
  - `multiline_wrap0.ass`: `\N`, `\N\N`, a trailing break, a break inside bold.
  - `multiline_wrap2.ass`: `WrapStyle: 2`, where `\n` is a break.
  - `timing_edges.ass`: `0:00:00.00`, `.99`, zero duration, `59.99`, `59:59.99`, `9:59:59.99`.
  - `reordered_format.ass`: Format lines with reordered columns in both Styles and Events.
  - `unicode_sections.ass`: UTF-8 (Latin, CJK, Greek, Cyrillic, Arabic, emoji), `;` and `!`
    comments, `Comment:` rows, `[Fonts]` and `[Graphics]` sections.
  - `karaoke_colours.ass`: `\k \kf \ko`, `\1c \3c`, `\fn \fs`, `\bord \shad \be \fad \alpha \1a`.
- `crates/ec-ass/tests/ffmpeg_parity.rs`, four tests:
  - `cues_match_ffmpeg_timing_order_and_text`: for the seven ffmpeg-demuxable fixtures, compares
    event count, start/end in ms (ffprobe `-show_packets`), and the text with tags stripped
    (ffmpeg `-c:s text -f srt`).
  - `matroska_style_blocks_rebuild_to_the_same_cues`: edith's Matroska rebuild (CodecPrivate
    header plus `ReadOrder,Layer,...` blocks, timing rebuilt as `Dialogue:`) gives the same cues.
  - `reordered_format_is_read_by_column_name`: hand-written expectations (see below).
  - `unmodelled_override_tags_survive_as_verbatim_raw`: exact `Segment::Raw` text for `\t`,
    `\blur`, `\frz`, `\fscx`, `\move`, `\clip`, `\org`, and `\p` drawings.
  - The ffmpeg test skips with a loud stderr note if `ffmpeg` or `ffprobe` is absent.
- `crates/ec-ass/src/lib.rs`: one fix. The brace-block splitter cut on every backslash, including
  those inside parentheses. In `{\t(0,500,\fs20)}` the `\fs20` was read as a font size and the Raw
  text came out as `{\t(0,500,}`. A new `tags()` helper splits only at depth 0, so the whole `\t(...)`
  is one unmodelled tag and its text is kept verbatim. The fix was found by reading the code and
  is covered by the new Raw test. I did not run the test against the old code to see it fail.

## Findings

1. **`\n` under WrapStyle 0 (not changed).** ffmpeg's text output turns `\n` into a newline in
   WrapStyle 0. ec-ass treats it as a space, following the spec's smart-wrap rule, and the existing
   unit test `tolerances_and_the_refusal` pins that. I could not check libass's behaviour offline,
   so I did not change ec-ass to match ffmpeg. The WrapStyle 0 fixture no longer contains a `\n`.
   `multiline_wrap2.ass` covers `\n` as a break. Open question: whether ffmpeg's rendering here
   matches libass.
2. **Reordered Events `Format:` (not compared).** ffmpeg's demuxer produces zero packets for a script
   whose Events Format puts Start before Layer, with no error. There is nothing to compare, so
   `reordered_format_is_read_by_column_name` checks hand-written values. ec-ass follows the Format
   line as the spec says.
3. **Event order.** ffmpeg emits events sorted by start time, ties in file order. ec-ass keeps file
   order, which `SubtitleTrack` documents. The parity test sorts ec-ass's cues the same way (stably)
   before comparing.
4. **Non-canonical fractions (not in fixtures, not changed).** ffmpeg reads `0:00:00.005` as 5
   centiseconds (50 ms). ec-ass reads it as 5 ms. The ASS spec gives two-digit centiseconds, so the
   input is non-standard. Left alone.
5. **Matroska.** ec-ass has no block-level API. The rebuild lives in edith
   (`~/.cache/wt/edith-integ/crates/engine/src/subtitle.rs` around line 593), so the test
   reproduces that rebuild and checks it against a direct parse.
6. **Comment rows.** ffmpeg does not emit `Comment:` rows as packets, and ec-ass skips them too.
   The unicode fixture covers this.

## Verify

Run on host b with `scripts/remote-run.sh assparity --host b`. ffmpeg 8.1.3 is installed there.

- `cargo test --release -p ec-ass`: all pass. Unit tests 4/4, `ffmpeg_parity` 4/4, `fuzz_regressions`
  4/4. Doc tests have none.
- `cargo clippy -p ec-ass --all-targets -- -D warnings`: exit 0.
- Line counts: `lib.rs` 592, `ffmpeg_parity.rs` 348, both under 5000.
- `rustfmt --edition 2024` was run on both changed Rust files. The gates were rerun after it.

## Left

- The `\n` / WrapStyle 0 question (finding 1) needs an answer from libass or a spec reading.
- Matroska stays a rebuild check, since ec-ass exposes no block API.
- No bench rows.
