# lane-troykf r2 handoff

SUPERSEDED: the suite finished after this file was written --
`test result: ok. 379 passed; 0 failed; 33 ignored` (1055 s). Nothing is owed.
Tip `f7e0d3d` at the time of writing; see `lanes/troykf-r2.report.md`.

* Log: `$HOME/.cache/troykf-suite-r2.log`. Check with a single
  `grep -E "^test result|FAILED" $HOME/.cache/troykf-suite-r2.log`.
  At the cap: ~200 of ~412 tests logged, **zero `FAILED` lines**.
  r1's run of the same suite on the pre-merge tree reached 361 tests with zero
  FAILED before I stopped it (superseded by this run); both film-grain gates
  (`a_real_aomenc_stream_with_film_grain_decodes_pixel_exact` and its 10-bit
  sibling) were `ok` there, so the cross-lane concurrency red did not appear.
* If anything is red, it is a sibling of the two touched paths: grep
  `smooth_uv_neighbour` and `cfl_ac_q3` in `crates/ec-av1/src/decode.rs`.
* Nothing else is owed. Merge of main 85887c7 is at 4667015 (clean auto-merge,
  cdf.rs untouched, no deletions).
* Reusable instruments: `$HOME/.cache/troykf-work/one.sh <ss>` (per-seek-point
  film compare), `.../cflsweep.sh <tag> <lavfi> <cq> <depth> [cpu] [maxpart]
  [minpart]` (aomenc + `EC_TRACE_MODE=1 aomdec`, prints `cfl=` and `skipcfl=`
  with no cargo build at all — this is how 28 more recipes were swept while a
  suite held the target dir).
