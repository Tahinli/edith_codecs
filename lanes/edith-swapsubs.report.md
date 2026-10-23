# lane-swapsubs: subtitles off oxideav onto ec-subs / ec-ass / ec-pgs

Base: codecs-integration 10325ab. Branch lane-swapsubs, commit 8bd572a.

## What changed
- crates/engine/Cargo.toml: added ec-subs, ec-ass, ec-pgs path deps; removed oxideav-subtitle, oxideav-ass, oxideav-sub-image. oxideav-core kept (export.rs and oxideav-ogg use it).
- Cargo.lock: drops oxideav-subtitle, oxideav-ass, oxideav-sub-image, oxideav-otf/pixfmt/png/raster/scribe/ttf, compcol; adds the three ec-* crates.
- crates/engine/src/subtitle.rs: srt_body, ass_cues, parse_file now call ec_subs / ec_ass; CueImage::rgba uses ec_pgs::PgsDecoder (push the display set, take the first frame). The segment walk that cuts at the first shown END is unchanged.
- crates/engine/src/subtitle/parity.rs (cfg(test)): renders every fixture through the live path and compares to tests/data/subtitle_parity.txt.
- crates/engine/tests/data/subtitle_parity.txt: snapshot written from the oxideav path before removal (regenerate with EC_WRITE_PARITY_SNAPSHOT=1 and --nocapture).

## Mapping (what each call became)
- oxideav_subtitle::srt::parse / webvtt::parse -> ec_subs::srt::parse / webvtt::parse
- oxideav_ass::parse -> ec_ass::parse
- oxideav_subtitle::ir::plain_text -> ec_subs::plain_text (Cue.text stays a String; edith keeps no Segment runs, as before)
- oxideav_sub_image::pgs::make_decoder + send_packet/receive_frame -> ec_pgs::PgsDecoder::new + push + take_frame
- SEG_PCS / SEG_END constants -> ec_pgs::SEG_PCS / SEG_END
- Segment::Raw (ASS override tags, incl. drawings): ec-subs keeps them Raw and plain_text drops them. No edith-side code needed.

No ec-* API gaps were found. No ec-* crate was changed.

## Parity
Fixtures: tests/data/test_subs.srt, test_subs.ass; assets/test_subs.mkv, test_subs.mks, test_subs_bcp47.mkv; inline VTT, ASS (override tags), SRT (markup); synthetic PGS mkv (paint + erase).
Result: all identical except one intended difference, found by the parity run: an ASS drawing `{\p1}m 0 0 l 10 10{\p0}words`. The oxideav path returned "m 0 0 l 10 10words"; ec-ass keeps the drawing as Raw and plain_text drops it, giving "words". The new behaviour is correct (shapes are not words). It is pinned by a separate test (an_ass_drawing_is_shapes_and_not_words) and kept out of the snapshot.

## Verify (all via remote-run, edith-swapsubs)
- cargo test -p engine --release --lib subtitle::  : 7 passed
- cargo test -p engine --release --lib subtitle::parity : 2 passed (before snapshot, parity ran oxideav vs ec-* and was equal apart from the drawing case)
- cargo test -p engine --release --test subtitles : 22 passed, 0 failed
- cargo test -p engine --release --no-fail-fast : 1 failed in lib (tonemap::tests::perf_4k_and_1080p, known load-flaky), 1 failed in tests/playback (import_refuses_what_does_not_match, known). Every other target ok, including subtitles.
- cargo clippy -p engine --all-targets : exit 0. No warnings in subtitle.rs or parity.rs. The 3 warnings in tests/subtitles.rs predate this lane (file untouched).
- cargo tree --offline -p engine -i oxideav-subtitle / -i oxideav-ass / -i oxideav-sub-image : no matching packages. -i oxideav-core still resolves (export, ogg).

## Left
- oxideav-core and oxideav-ogg stay for the swapogg lane.
- Real-library PGS (hevc_4k_pgs) test skipped on the server, as usual; it was not run locally.
