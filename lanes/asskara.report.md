# lane/asskara: ASS karaoke override tags

## Finding: ec-ass only parses
It does not render. Nothing to compare against `ffmpeg -vf ass=`, so the parsed model now carries what a renderer needs, and the parse is tested.

## Done
- `ec_subs::Segment::Karaoke` is now `{ cs, start_cs, mode, secondary, children }` (breaking for exhaustive matches).
  - `start_cs`: syllable start in cs from the cue start. It accumulates across `\k` tags, including empty ones, as libass does. `\kt` sets it absolutely for the next `\k`.
  - `mode`: new `ec_subs::KaraokeMode`. `Fill` is `\k` (secondary to primary switch at start), `Sweep` is `\kf` / `\K` (left-to-right), `Outline` is `\ko` (outline-only switch).
  - `secondary`: `\2c` override; `None` means the style's secondary.
  - A style block inside one syllable repeats the same `(start_cs, cs)` on the split runs; group by that pair.
- `SubtitleStyle::secondary_color` (RGBA) is parsed from `SecondaryColour`.
- Fixes: `\K` was dropped to Raw, `\kt` and `\2c` were unknown, and `\r` did not reset overrides. `\rName` is reset and also kept as Raw `{\rName}`.

## Audit (parse level; all kept verbatim as `Segment::Raw`, text unaffected, tested)
`\t` with accel, `\fad`, `\fade`, `\clip`, `\iclip` (rect and vector), `\org`, `\frx/y/z`, `\fax/fay`, `\be`, `\blur`, `\q`, `\fsp`, `\bord`, `\shad`.
`\an`, `\a`, `\pos` and the start of `\move` fill `CuePosition`; the full `\move` stays as Raw. `\p` becomes Raw drawing text (existing).
Not modelled structurally (cheap only for a renderer): these Raw strings still need parsing by the renderer.

## What edith needs (text.rs, not touched)
Per frame at time t (cs since cue start = (t - cue.start_us)/10000), for each `Segment::Karaoke` (group runs by `(start_cs, cs)`):
- Secondary colour: `secondary` else `style.secondary_color`; primary is the enclosing `Color` or `style.primary_color`.
- `Fill`: t < start_cs -> secondary, else primary.
- `Sweep`: t <= start -> secondary; t >= start+cs -> primary; between, the syllable's glyph box is split at x = left + width*(t-start)/cs, left part primary, right part secondary.
- `Outline`: fill is primary always; the outline is drawn only from start_cs (libass `\ko` hides the outline before it).
- Text outside any Karaoke segment is plain primary.
Match the pattern with `..` or add the new fields where `Segment::Karaoke` is built or destructured.

## Gates
`cargo clippy --workspace --all-targets -- -D warnings` clean; `EC_REQUIRE_FIXTURES=1 cargo test -p ec-ass --release` passes (new tests: karaoke modes/timing/colours/`\kt`/`\r`, and the override audit). `cargo test -p ec-subs` shows `a_real_pgs_stream_decodes_to_plausible_bitmaps` failing in real_library.rs; PGS, unrelated to this change (not checked on main).
