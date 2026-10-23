# lane/oggreal: ec-ogg real-library witness

Test: `crates/ec-ogg/tests/real_library.rs`, `real_library_ogg_streams_match_ffmpeg`
(`#[ignore]`, local only, reads the gitignored manifest; skips cleanly when the
manifest or ffmpeg/ffprobe is missing).

## Result (local manifest, 4 public-domain / CC0 Ogg files)

| codec  | duration | result | packets | payload bytes | pts checked | granules checked |
|--------|----------|--------|---------|---------------|-------------|------------------|
| Vorbis | 90 s     | match  | 5689    | 1646697       | 392         | 393              |
| Vorbis | 148 s    | match  | 6436    | 2295637       | 547         | 548              |
| Opus   | 111 s    | match  | 5526    | 1292983       | 5526        | 111              |
| Vorbis | 106 s    | match  | 5419    | 1045211       | 21          | 22               |

Refused by name: 0. Failed: 0.

Per file, the sweep checks packet count, every packet size, the concatenated
payload bytes (ffmpeg `-c copy -f data`), the Opus header size, the Vorbis
header structure, pts, and granule ends.

## Timing rule (measured against ffmpeg)

- Vorbis: a page's granule is the sample position at the end of the last packet
  finishing on that page; ffmpeg's pts + duration equals it. No shift.
- Vorbis first packet: ffmpeg starts it at minus its own length. ec-ogg starts
  the stream at zero by its documented convention and states no Vorbis packet
  duration, so the first packet's pts is not compared (its granule is).
- Opus: granule counts 48 kHz samples including pre-skip. ffmpeg's end is
  `granule - pre_skip`; ec-ogg's pts is ffmpeg's pts + pre_skip.

## Header block

- Opus: the `OpusHead` size equals ffprobe's `extradata_size` exactly.
- Vorbis: ffmpeg's stated extradata size differs from ec-ogg's on two of the
  four files. ec-ogg keeps the comment header verbatim (the remux path relies on
  this for bit-identical output). ffprobe does not expose the extradata bytes,
  so the Vorbis block is checked structurally (Xiph lacing, three packets, magic,
  30-byte identification header) and the size difference is reported, not failed.

## Decode side

Demux only. The decoded PCM is not compared in this test.

## Bugs

None found in ec-ogg. The first sweep reported two differences, both examined
and classed as convention or representation, not demux errors:
the Vorbis first-packet pts (convention above) and the Vorbis extradata size
(verbatim comment header). No source change was made to ec-ogg.

## Diff summary

- `crates/ec-ogg/tests/real_library.rs`: new ignored sweep test.
- `lanes/oggreal.report.md`: this report.
- Local only, not in the repo: four `ogg` rows appended to the gitignored
  `fixtures/real-library-manifest.tsv` of the primary checkout (backup taken
  first).
