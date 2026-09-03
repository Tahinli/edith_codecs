# lane-streamsink r1 -- streaming decode entry point

## What changed
- `crates/ec-av1/src/stream.rs:223` -- `decode_stream` is now a thin collecting wrapper;
  the decode loop lives in **`decode_stream_with(data, sink)`** (stream.rs:236),
  `sink: impl FnMut(&Picture, decode_idx: usize, shown: bool) -> Result<()>`.
  Each completed picture is handed to the sink and dropped; only the 8 reference
  slots (+ CDF/motion-field/segment-map slots) and the current frame stay alive.
  Hidden (`show_frame == 0`) frames are reported with `shown == false` and are not
  part of `decode_stream`'s Vec; `show_existing_frame` outputs and shown frames come
  with `shown == true` (grain applied, as before). A sink `Err` aborts the decode.
- Same file: the `EC_AV1_DUMP_TABLES` label used `pictures.len()`; it is a shown-output
  counter (`pictures_shown`) now, same numbers. `EC_AV1_FINAL_DUMP` /
  `EC_AV1_DECODE_ORDER_DUMP` already wrote one frame at a time (one frame-sized buffer).
- `crates/ec-av1/examples/decode_probe.rs:~270` -- decodes through `decode_stream_with`
  and writes each shown frame to `EC_PROBE_OUT16` (yuv420p10le) / `EC_PROBE_OUT`|argv[2]
  (8-bit) as it completes, into a file **or a FIFO**. The old path collected every
  Picture and then built a second full copy of the dump before one `fs::write` -- 2x the
  whole segment. OK/REFUSED/`wrote N bytes`/counter lines are unchanged.
- `crates/ec-av1/src/stream.rs` tests: `streaming_decode_matches_the_collecting_one`
  (same pictures, same order, one callback per shown frame, monotone decode idx, sink
  error aborts after 1 call).
- Runner: `<scratchpad>/fullfilm/run_film_fifo.sh` -- our decoder streams into a FIFO,
  `cmp2.py` (unchanged) reads one frame from the FIFO and one from ffmpeg per iteration.

## Peak RSS (4K 10-bit 3840x1608, `systemd-run --scope -p MemoryMax=6G /usr/bin/time -v`, EC_PROBE_OUT16=/dev/null)
| stream | old (collect + 2nd copy) | new (streaming) |
|---|---|---|
| 8 s window, 384 shown frames | **6,280,804 kB (6.28 GB, at the cap)** | **436,404 kB (0.44 GB)** |
| 32 s window, 960 shown frames | not run (old scales linearly; 20+ GiB) | **437,860 kB (0.44 GB)** |

EVIDENCE: ~/.cache/streamsink-tmp/h8.obu, rss32.log | `/usr/bin/time -v` on the old and new decode_probe over an 8 s and a 32 s 4K 10-bit window under a 6 GiB scope | peak RSS 6,280,804 kB -> 436,404 kB (8 s); 437,860 kB at 960 frames, i.e. flat in segment length (target <2.5 GB met, 14x under)

EVIDENCE: ~/.cache/streamsink-tmp/{old,new}.raw | both probes, EC_PROBE_OUT16, on a 48-frame 4K 10-bit cut | `cmp` identical, 889,159,680 bytes both, "OK: 48 frames decoded, 3840x1608" both

EVIDENCE: <scratchpad>/fullfilm/run_film_fifo.sh h8w | FIFO-fed streaming decode vs ffmpeg yuv420p10le, 2 segments of the 4K 10-bit stream | FULLFILM compared=584 differing=0 max_bytes=0

## Suite
SUITE: `cargo test -p ec-av1 --lib` (systemd unit, EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1, log
`$HOME/.cache/streamsink-suite.log`) -- **test result: ok. 430 passed; 0 failed; 38 ignored;
0 measured; 0 filtered out; finished in 891.27s**. Includes refusal_inventory (3),
gate_coverage (9), and the named film gates (128sb, hidden_arf, gm small-side,
mv-clamp/frame-edge) -- all ok.

## Residue
- accepted: `decode_stream` clones each shown picture into its Vec (one extra frame at
  peak on the collecting path). Gates are small; the streaming path is the one that matters.
- deferred: the whole-Vec probe path was NOT kept behind an env -- the streaming output is
  byte-identical and the printed lines unchanged, so a second path would be dead code
  (unblocks: nothing; `git revert` of this commit if a consumer ever needs it).
