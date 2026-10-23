# Lane chapters: report

Base: `main` at `00054596`. Branch `lane/chapters`.

## Goal

Exports carry chapters given as `(start_ns: u64, title: String)`. Two halves: the mp4 writer in
`ec-mp4` (this repo), and the edith side (`lane-chapters` in `~/.cache/wt/edith-chapters`, commit
`07b8c8a`), which hands the list to the mp4 and Matroska writers.

## Changes in this repo

- `crates/ec-mp4/src/mux.rs`
  - `Mp4Muxer::set_chapters(&[(u64, String)]) -> Result<()>`. Sorts by start (stable). Writes the `chpl`
    only for 1 to 255 chapters, since its count is one byte. Past 255 no error is raised: the chapter
    track carries all of them.
  - `moov/udta/chpl` (1 to 255 chapters only): version 1, start times in 100 ns units, one-byte title length. Titles are cut
    to 255 bytes on a character boundary.
  - QuickTime chapter track: a disabled `text` handler track, one `text` sample per chapter, sample
    bytes in the `mdat` (16-bit length plus UTF-8 title), `co64` offset, `mdhd` language `und`,
    timescale 1000 (ms). Each chapter runs to the next start; the last runs to the film's end.
  - Reference: `tref/chap` in the first video track, else the first audio track, else track 0.
  - A chapter that starts after 0 gets an empty `edts` edit, so the track starts at the first chapter.
  - Unit tests: `chpl` byte layout, `tref/chap` target, no chapter boxes when the list is empty,
    the 255 limit, title clipping on character boundaries.
- `crates/ec-mp4/tests/chapters.rs` (new): ffprobe parity. `ffprobe -show_chapters` must list the
  same starts (to the ms) and titles as given, including a non-zero first chapter; a file with no
  chapters lists none; the demuxer still opens a chaptered file and reads all its audio packets.

## Verify

Run on host c with `scripts/remote-run.sh chapters --host c -- ...`:

- `cargo test --release -p ec-mp4`: all pass. Unit 31, `chapters` 4, others as before. 0 failures.
- `cargo clippy -p ec-mp4 --all-targets -- -D warnings`: clean.
- `cargo fmt -p ec-mp4 -- --check`: my hunks are formatted. The remaining diffs are in `demux.rs`
  and an import order in `mux.rs`'s test module, all from before this lane.
- Line limit: `mux.rs` is 1,468 lines, under 5,000.

Observed ffprobe 8.1.3 output for a three-chapter file: starts 0, 5000, 12345 ms, the titles as
given, and one entry per chapter (the `chpl` and the chapter track do not double up).

## Left

- The peer that owns `project.rs` fills `ExportSettings::chapters`. Nothing in this lane sets it.
- `edith` `export.rs` is 9,636 lines. That is over the 5,000-line rule, and it was already that long
  before this lane. Not split here.
