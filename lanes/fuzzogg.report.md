# fuzzogg: cargo-fuzz targets for ec-ogg

Goal: DONE.md shared gate 2, at least one cargo-fuzz target per untrusted entry point in
ec-ogg (page parsing, demux and packet reassembly, mapping and header parsing for
Opus/Vorbis/FLAC-in-Ogg).
Base sha: 78ec1905 (main at worktree creation).

## What changed
- `crates/ec-ogg/fuzz/`: standalone cargo-fuzz crate (`ec-ogg-fuzz`, own `[workspace]`,
  path deps on ec-ogg and ec-core). Same layout as `crates/ec-aac/fuzz/`. Root `Cargo.toml`
  already excludes `crates/*/fuzz`, so nothing changed there.
  - `page_parse`: `PageHeader::parse_fixed` on the whole input, then the segment table it
    announces, `body_len`, `stored_crc`, and `xiph_unlace`.
  - `demux`: the first 8 bytes pick a seek tick; the rest has each page's CRC recomputed in
    place (fuzz-only fixup, `fix_crcs`, in the target, not the library). Then `OggDemuxer::open`,
    drains every packet of every stream, and seeks each stream under all three `SeekMode`s,
    draining after each seek.
  - `demux_raw`: the input unchanged, so the CRC rejection path and resync scanning are covered.
  - `mapping`: `Mapping::identify` on the input, every accessor (`codec`, `time_base`,
    `sample_rate`, `layout`, `header_packets`, `packet_duration`), and `opus_packet_samples`.
  - `roundtrip`: input bytes become Opus packets (u16-BE length prefixes, zero-length dropped),
    muxed with `OggMuxer` then demuxed. The packet count and bytes must match exactly.
  - `seed.sh`: every `fixtures/audio/*.ogg|*.opus` and `fixtures/vectors/vorbis-xiph/*.ogg`
    (34 files) is a seed for page_parse, demux, demux_raw; the first 256 bytes of each go to
    mapping; the Opus packets of each go to roundtrip (34 each).
  - `.gitignore`: target, corpus, artifacts, coverage.
- `crates/ec-ogg/src/demux.rs`: two overflow fixes found by reading the code, not by fuzzing
  (see below).
- `crates/ec-ogg/tests/ogg.rs`: two regression tests, `granule_at_i64_floor_does_not_overflow_the_duration`
  and `packet_position_near_i64_max_saturates_instead_of_overflowing`.

## Bugs found and fixed
No crash, timeout or OOM came out of the fuzzer. Reading `demux.rs` showed two unchecked
`i64` operations that panic in debug builds (cargo-fuzz builds with overflow checks) on
input-dependent values:
1. `measure_durations`: `granule - start` with a last-page granule near `i64::MIN` and a
   non-zero Opus pre-skip. Fix: `checked_sub`; an unrepresentable duration becomes `None`.
2. `consume`: `pos += d` with a page granule near `i64::MAX`. Fix: `checked_add`; on overflow
   the packet position is marked unknown, like an unknown duration, and the next page granule
   re-anchors it. Packets keep being delivered.

Both chose "unknown" over `Error::corrupt`: the Ogg rule is that damage costs the packets
inside the damaged part, not the whole file. Regression tests fail under `cargo test` (debug,
panic `attempt to subtract/add with overflow`) before the fix and pass after it.
The fuzz smoke runs of `demux` and `roundtrip` were repeated after the fix.

## Verify commands and results
All cargo commands ran through `scripts/remote-run.sh fuzzogg`. Host b was busy for most
runs, so several fell back to host a or local. This is fine per the lane brief.
- `cargo +nightly fuzz build --fuzz-dir crates/ec-ogg/fuzz`: exit 0, all five targets built
  (ran local after both hosts stayed busy).
- Smoke, 120 s each, `-max_total_time=120 -rss_limit_mb=3000 -timeout=10`, `--host b`:

| target      | exit | executions | crash/timeout/OOM |
|-------------|------|-----------:|-------------------|
| page_parse  | 0    | 20,090,399 | none              |
| demux (pre-fix) | 0 | 3,025   | none              |
| demux (post-fix) | 0 | 3,775  | none              |
| demux_raw   | 0    | 7,290      | none              |
| mapping     | 0    | 58,210,106 | none              |
| roundtrip (pre-fix) | 0 | 210,209 | none         |
| roundtrip (post-fix) | 0 | 213,929 | none        |

- `cargo clippy -p ec-ogg --all-targets -- -D warnings`: exit 0, clean.
- `cargo test -p ec-ogg` (debug): exit 0. 11 unit, 7 integration, 0 doc; includes the two
  new regression tests. Before the fix, both regression tests panicked.
- `cargo test -p ec-ogg --release`: exit 0. Same counts: 11 unit, 7 integration.

## Left open
- The gate asks for 1 CPU-hour per target. Only 120 s smoke runs were done, so the gate is
  not met.
- `demux` is slow (about 30 exec/s): every input does a seek under three modes, each with a
  full drain, plus `measure_durations`. A longer run will get fewer exec per hour than the other
  targets; that is expected, not a finding.
- `fixtures/vectors/opus-rfc6716` holds only `.bit` vectors, no `.ogg`, so the seed script
  takes none from there. The brief asked for them; they do not exist in the fixture set.
- The corpus is seeded from fixtures only. No minimised corpus or coverage measured.
- `crates/ec-ogg/fuzz/Cargo.lock` was generated locally and is not in this commit.
- `roundtrip` covers Opus packets only. Vorbis and FLAC-in-Ogg packets do not go through a
  mux-then-demux roundtrip.
