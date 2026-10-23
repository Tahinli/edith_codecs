# lane/fieldorder report

Commit on lane/fieldorder: "lane/fieldorder: mp4 fiel box and Matroska FlagInterlaced/FieldOrder ...". No trailers, not pushed.

## Design
- Reused `ec_core::Interlace` (Progressive | TopFieldFirst | BottomFieldFirst). "Unknown" is `Option::None` (writes nothing).
- **Did not add a field to `VideoParameters`.** `grep -rn VideoParameters ../edith/crates` finds one literal, crates/engine/src/mux.rs:389, with no `..Default::default()`; a new field would break edith's build, and a builder cannot fix a struct literal. Instead the muxers carry it as per-track state: `Mp4Muxer::set_field_order(stream, Option<Interlace>)` and `MatroskaMuxer::set_field_order(...)` (before first packet).
- **Default: every video track is `Some(Progressive)`** with no code change in edith, so edith exports get progressive automatically. Justification: simplest, and edith only muxes its own encoders' progressive output. Trade-off: a caller remuxing interlaced material must call `set_field_order` (using the new demuxer `field_order(stream)`), otherwise it is labelled progressive. Edith needs no diff.
- Demux: `Mp4Demuxer::field_order(stream)` (parses `fiel`: count 1 progressive; count 2 with detail 1/9 top-first, 6/14 bottom-first) and `MatroskaDemuxer::field_order(stream)` (FlagInterlaced 2 progressive; 1 with FieldOrder 1 / 6; else None).

## Container values
- mp4: `fiel` written after config, colr/mdcv/clli and pasp (ffmpeg order): progressive 1/0, TFF 2/1, BFF 2/6. Deviation from the task text (2/9, 2/14): ffprobe reads 9 as `tb` and 14 as `bt` (display-top-first but bottom-stored-first), so 1/6 is what ffmpeg itself writes for tt/bb. Verified by test: 2/9 gave "tb".
- mkv: FlagInterlaced (0x9A) and FieldOrder (0x9D): progressive 2/0, TFF 1/1, BFF 1/6.

## Tests
- New crates/ec-matroska/tests/field_order.rs (synthetic ffmpeg testsrc2 libx264 and libx265, demuxed with Mp4Demuxer, muxed to mp4 and mkv): default, explicit progressive, tt, bb and None; ffprobe field_order checked (progressive/tt/bb; HEVC with None is `unknown`, H.264 with None is `progressive` since ffmpeg derives it from the SPS) plus demux round trip in both containers. Interlaced sources were not generated; tt/bb are set through the setter. The ec-h265 encoder was not used (ec-matroska has no such dev-dep); libx265 sources exercise the same muxer path.
- server (host c) `EC_REQUIRE_FIXTURES=1 cargo test -p ec-mp4 -p ec-matroska -p ec-core --release`: all pass. No golden files changed; byte-exact tests passed with no update (they don't cover video sample entries, or none differ).
- server `cargo clippy --workspace --all-targets --release -- -D warnings`: clean.
- Edith: `cargo check -p engine --all-targets` on server with `--with-edith --host c` against the unchanged edith tree: Finished, builds. (Full `--workspace` fails on the server for libspa-sys/pipewire system libs in the GUI crate, unrelated.)
