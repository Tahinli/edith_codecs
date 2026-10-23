# lane fuzzre-d: DONE.md G2 fuzz re-run on the final tree (ec-ass, ec-pgs, ec-matroska)

Base: `main` at `bb0823cc`, branch `lane/fuzzre-d`. Targets re-run because their parse/decode sources changed after
their last 1 CPU-hour run: ec-ass `overrides` and `events` (override splitter, 38b01504); ec-pgs `compose` and
`segments` (lazy display sets with a bounded queue, 2babc38b); ec-matroska `demux_packets`, `seek` and `open_tracks`
(unknown-length cluster handling, 33230f6c).

Hosts: a = host-a, b = host-b, c = host-c (4 cores each, cargo-fuzz 0.13.2, nightly 1.101.0).
Each run is `cargo +nightly fuzz run <target> -- -max_total_time=3600 -rss_limit_mb=1536 -timeout=30`, the same flags as
fuzzre-c. Seeds come from `crates/<crate>/fuzz/seed.sh` (fixtures in `fixtures/subs`, `fixtures/audio`, `fixtures/video`).
Each target was started with nohup on its host through `scripts/remote-run.sh fuzzre-d --host <x>`. At most 3 jobs ran per
host. Logs and CPU times are on each host in `~/tmp/fuzzre-d/<crate>-<target>.log` and `.time`, with the resume logs
suffixed `-r1`.

CPU seconds come from bash `time` (user, sys, wall) around `cargo fuzz run`. That covers the libFuzzer child, since cargo
waits for it. Each target was short of 3600 CPU-s after its 3601 s wall run (by 24 to 90 s), so each was resumed for 120 s
from the same corpus, as the instructions say. Both runs are counted below.

## Results

| Target | Host | Runs (1 h + resume) | CPU s user+sys (1 h + resume = total) | Crashes | EXIT (1 h, resume) | Final cov / ft (resume DONE) | Earlier cov / ft |
|---|---|---|---|---|---|---|---|
| ec-ass `overrides` | a | 13,585,345 + 520,918 = 14,106,263 | 3575.7 + 120.5 = 3696.2 | 0 | 0, 0 | cov 1874 / ft 11856 | not recorded |
| ec-ass `events` | b | 16,424,045 + 563,368 = 16,987,413 | 3574.8 + 120.5 = 3695.3 | 0 | 0, 0 | cov 1880 / ft 11936 | not recorded |
| ec-pgs `compose` | a | 460,981 + 12,488 = 473,469 | 3567.8 + 120.5 = 3688.3 | 0 | 0, 0 | cov 643 / ft 2976 | not recorded |
| ec-pgs `segments` | b | 1,229,815 + 48,544 = 1,278,359 | 3570.1 + 120.1 = 3690.2 | 0 | 0, 0 | cov 562 / ft 2048 | not recorded |
| ec-matroska `demux_packets` | a | 6,794,848 + 282,741 = 7,077,589 | 3572.9 + 120.7 = 3693.6 | 0 | 0, 0 | cov 1377 / ft 6080 | not recorded |
| ec-matroska `seek` | b | 11,996,727 + 427,664 = 12,424,391 | 3572.7 + 120.4 = 3693.1 | 0 | 0, 0 | cov 1619 / ft 6784 | not recorded |
| ec-matroska `open_tracks` | c | 31,529,663 + 1,219,945 = 32,749,608 | 3509.7 + 120.0 = 3629.7 | 0 | 0, 0 | cov 1220 / ft 5523 | not recorded |

Notes on the table:
- "Earlier cov / ft" is not recorded. No lane report or host log from an earlier run of these seven targets has a
  cov/ft figure. The earlier 1 h runs (`fuzzmkv.report.md`, `fuzzsubs.report.md`, `fuzz1hcontainers.report.md`) recorded
  only executions and exit codes. So the coverage check below compares against nothing; it can say only that
  nothing looks wrong.
- First 1 h run cov / ft, for reference: ass `overrides` 1874 / 11854, ass `events` 1880 / 11929, pgs `compose` 647 / 2999,
  pgs `segments` 566 / 2058, matroska `demux_packets` 1377 / 6069, `seek` 1616 / 6775, `open_tracks` 1220 / 5521.
  The resume changed these by at most 4 cov and 11 ft.
- Earlier exec/s figures (from `fuzzmkv.report.md`, 1 h runs on an earlier tree): `demux_packets` 2365, `seek` 3478,
  `open_tracks` 4546. This run: `demux_packets` 1886, `seek` 3331, `open_tracks` 8755 (exec/s from the 1 h `Done` lines).
  Speed is in the same range as the earlier runs. `demux_packets` is a little lower.

## Coverage check

- The seeds are the same as the earlier runs. ass `events` and `overrides` each got 324 Dialogue rows and `styles` 6 rows
  (`seed.sh` for ec-ass). pgs `segments` got 64 KiB of `pgs-1080p.sup`. pgs `compose` got only 4 KiB of the same file,
  and that is mostly rejects, so its cov is low by design (see the `seed.sh` comment). matroska got 18 `open_tracks`,
  18 `demux_packets` and 54 `seek` seeds, built from `fixtures/audio` and `fixtures/video` (the `seed.sh` output on each host).
- Per-target counts are in the table. Nothing is "far below" a figure this lane can cite, because the earlier figure
  is not on record. The pgs `compose` cov of 643 is low; this comes from the seed (a 4 KiB reject-heavy prefix), not from
  the run being short.
- The first `open_tracks` launch on host c ran for about one minute before its corpus was seeded. I killed it
  (the kill also took the stale `cargo fuzz build` process), removed its log, seeded the corpus and started over.
  Only the clean run is counted.

## Observations (not crashes)

- `ec-pgs compose` and `segments` spend most of their CPU in the kernel: sys 2168 s and 2441 s of about 3570 s, against user
  1400 s and 1129 s. `ec-matroska demux_packets` is also high (sys 1664 s against user 1908 s). This is an allocation
  pattern (large buffers, mmap and page faults) and is not a timeout. The gate needs no fix. It is a cost worth a look
  if these targets get longer runs.
- Host c was at load average 17 for part of the 1 h run (other lanes). Its run still finished with EXIT 0.

## Crashes and fixes

None. All seven targets exited 0 in the 1 h run and in the resume, with no `SUMMARY`, `deadly signal`, `panicked`, OOM or
timeout line in any log, and no artifact written under `fuzz/artifacts`. No code changed, so no fix and no new
regression test.

## Gates

No crate in `crates/` changed in this lane, so no clippy or test gate applies. Nothing was run. The lane commit
contains only this report.

## Commands and verify results

| Command | Where | Result |
|---|---|---|
| `scripts/link-fixtures.sh` | local | fixtures symlinked |
| `scripts/remote-run.sh fuzzre-d --host <a,b,c> -- ...` (preflight: nightly, cargo-fuzz 0.13.2, ffmpeg, fixtures) | a, b, c | all present |
| `crates/{ec-ass,ec-pgs,ec-matroska}/fuzz/seed.sh` | a, b, c | ec-ass 324 dialogue rows, 6 style rows; ec-pgs 1 seed each; ec-matroska 18/18/54 |
| `nohup setsid bash ~/tmp/fuzzre-d-run.sh <crate> <target>` (`cargo +nightly fuzz build`, then `{ time cargo +nightly fuzz run ... }`) | a: 3 jobs, b: 3 jobs, c: 1 job | 7 runs, EXIT 0 each |
| `nohup setsid bash ~/tmp/fuzzre-d-resume.sh <crate> <target>` (`-max_total_time=120`) | a: 3, b: 3, c: 1 | 7 resumes, EXIT 0 each |

The runner scripts are kept on the hosts only (in `~/tmp/`, not in the repo).

## Commit

- Branch `lane/fuzzre-d`, from `main` at `bb0823cc`. Changes: `lanes/fuzzre-d.report.md`. No code, no push.
- The fuzz corpora and artifact directories are git-ignored and not committed.
