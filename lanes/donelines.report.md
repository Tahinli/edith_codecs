# lane donelines: DONE.md evidence update

Base: `b4e5f018` (main). Branch `lane/donelines`. Documentation only: `DONE.md` and this report. No cargo runs.

Workspace clippy G1 is taken as clean on main, as the brief states. It was not re-run in this lane.

## Lines added or changed in DONE.md

Placement: the Done log is a flat bullet list, so the new lines go there next to the ec-truehd and ec-ac3 lines, one bullet per crate section.

| Line | Change | Evidence |
|---|---|---|
| DONE 2026-10-18 ec-h264 decode and encode | added (gateaudit section 4, date changed to 2026-10-18) | `lanes/gateaudit.report.md` section 4; `h264refusals`, `h264reallib`, `h264encrc`, `h264encspeed2` (`lanes/*.report.md`) |
| DONE 2026-10-18 ec-opus decode and encode | added (gateaudit section 4) | `lanes/gateaudit.report.md` section 4; `opusgate` (`lanes/opusgate.report.md`) |
| DONE 2026-10-18 ec-vorbis | added (gateaudit section 4) | `lanes/gateaudit.report.md` section 4; `vorbis-rate-r1`, `fuzz1haudio` |
| DONE 2026-10-18 ec-mp3 encode | added (gateaudit section 4) | `lanes/gateaudit.report.md` section 4; `mp3lame-r1` |
| DONE 2026-10-18 ec-h265 encode | added: every "Done when" criterion met | `hevcencrc`, `hevcencrcpar` (merge `1229ca2d`), `hevcencspeed4` (merge `6808f23d`), `hevcencspeed5` (merge `8fb9fb6c`), `hevcmisc` (merge `7d8711af`) |
| DONE 2026-10-18 ec-aac encode | added: every "Encode done when" criterion met | `aacencbar` (merge `5241ffbe`), `aacgates2` |
| ec-ac3 decode (DONE 2026-10-17) | note updated: the "not bit-exact by design: dither differs" statement is replaced. Correlation 1.000000 vs ffmpeg; bit-exactness is not claimed. Added the AC-3 bench row (323.2x). Date kept at 2026-10-17 | `ac3parity` (merge `a99a45ab`, `lanes/ac3parity.report.md`); `benchrows` (merge `e188b83b`, `lanes/bench.md`) |

## Criteria recorded as met (crate not DONE)

| Crate | Criterion | Evidence |
|---|---|---|
| ec-aac decode | mono, stereo, 5.1, 7.1 raw ADTS and m4a per-channel corr >= 0.999 (HE-AAC v1 not covered) | `aacdecmulti` (merge `0ad0ea98`, `lanes/aacdecmulti.report.md`) |
| ec-h265 decode | Dolby Vision profile 8.x base layer bit-exact vs ffmpeg, RPU ignored (mp4 dvcC/dvvC not covered) | `hevcmisc` (merge `7d8711af`, `lanes/hevcmisc.report.md`) |
| ec-vp8 decode | bench row: 157.30 fps, 5.24x | `benchrows` (`lanes/bench.md`) |
| ec-vp9 decode | bench row: 1080p30 profile 0, 165.13 fps, 5.50x, md5 = ffmpeg | `benchrows` (`lanes/bench.md`) |
| ec-ac3 decode | bench row: 323.2x. The parity figure in that row (0.981) predates the dither fix and was not re-run on the 60 s clip | `benchrows` (`lanes/bench.md`) |
| ec-av1 decode | 1080p30 at or above realtime with `EC_AV1_THREADS` > 1: 3.93x at 4, 4.03x at 6 | `av1bench6` (merge `5939a7de`, `lanes/av1bench6.report.md`) |

## Open, recorded in DONE.md

| Crate | Item | Status |
|---|---|---|
| ec-h265 decode | phone HEVC clips bit-exact vs ffmpeg | Blocked: no HEVC phone footage on the machine (`lanes/gateaudit.report.md` section 3) |
| ec-av1 decode | AOM conformance profile 0 at 240/242; 2 spatial-SVC vectors | Lane `av1svc` in progress; no lane report or merge on main. The 240/242 count comes from the brief, not from a report on main |
| ec-av1 decode | 1 h `decode_stream` fuzz on the final tree | Lane `av1fuzzfinal` in progress; no lane report or merge on main |

## Not changed

- The ec-h264, ec-opus, ec-vorbis, ec-mp3 and ec-aac-encode lines are as in gateaudit section 4, except for the date. The ec-h265 and ec-aac encode lines are new full-crate lines, since every criterion in their DONE.md sections is now met.
- Crate sections of DONE.md (for example "### ec-av1 decode") are unchanged. Their open criteria remain open.
- Edith-side checks (integration tests on `codecs-integration`) are not on main. The existing precedent is kept: the DONE lines cite them as on that branch.
