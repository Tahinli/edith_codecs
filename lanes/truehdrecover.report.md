# lane truehdrecover: a refused ec-truehd access unit must not poison later units

Base: `main` at `f43403af`, branch `lane/truehdrecover`. Hosts: a (cargo-fuzz and nightly). Every cargo and fuzz run went through `scripts/remote-run.sh truehdrecover --host a`, except the seed generation (`crates/ec-truehd/fuzz/seed.sh`, ffmpeg, local).

## Goal and cause

The `huff_lsbs > 24` guard from `fuzzre-c` stopped the i32 overflow, but a refused access unit (AU) still left state behind: `read_restart_header` replaces the substream, `read_decoding_params` stores codebook, `huff_lsbs`, quant step, matrices and FIR/IIR coefficients before the rest of the unit is parsed, and `filter_channel` advances the filter history on earlier blocks. Every later AU then decoded from that state.

## Test first (`crates/ec-truehd/tests/refused_au_recovery.rs`)

- For each committed fixture (`truehd-s16-2ch`, `s16-51`, `s24-2ch`, `s24-51`), take the AU at `len/3` and XOR every byte of it with 0xFF, one at a time.
- Keep the corruptions the decoder refuses (checked on the prefix).
- Oracle: the same stream with that AU removed. Every later AU's output (or refusal) must equal the oracle's output for the same AU.
- On `main`: FAILED. 189 of 233 refused corruptions poisoned later units. First: `truehd-s16-2ch.mkv` byte 45 of AU 400 refused (invalid Huffman code); AU 401 then refused with the same error, where the oracle decodes it.

## Fix (`crates/ec-truehd/src/decode.rs`)

1. `Core::decode` snapshots the substreams and stream format (`access_unit_size`, `access_unit_size_pow2`, `sample_rate`, `ch8_assignment`) before each AU, and restores them when the AU returns `Err`. The scratch buffers are rebuilt if the restored unit size differs. This makes a refused AU a no-op.
2. `read_decoding_params` validates `huff_lsbs <= 24` before storing codebook or `huff_lsbs`.
3. The `recompute_sign_offset` guard stays as defence in depth.
4. Major sync and restart behaviour are unchanged. Clean streams do not change: the existing bit-exact tests still pass, and the restart header keeps its FIR/IIR history as before.

Cost: one `Vec<Substream>` clone per AU (small: two or fewer substreams, a few hundred bytes each). Not benchmarked.

## Verify

| Command | Where | Result |
|---|---|---|
| `cargo test --release -p ec-truehd --test refused_au_recovery` (before the fix) | a | FAILED, 189 of 233 poisoned |
| same (after the fix) | a | ok; 233 refused corruptions, 158,367 later units compared, all equal |
| `cargo test --release -p ec-truehd` | a | exit 0; all suites pass (18, 2, 2, 1, 1, 3, 1 and doc-tests) |
| `cargo clippy -p ec-truehd --all-targets -- -D warnings` | a | exit 0 |
| `wc -l crates/ec-truehd/src/decode.rs` | local | 1064 lines (limit 5000) |

## Fuzz coverage (`-runs=0`, host a)

The corpus from `seed.sh` is 4 streams (ffmpeg truehd and mlp, mono/stereo/2.1/5.1), not the 6008-seed corpus behind the earlier 3087 and 939 figures, which is not on this host. The regression input was added as a second corpus dir, because libFuzzer takes positional args as directories.

| Tree | Input | cov | ft |
|---|---|---|---|
| `f43403af` (pre-fix) | seeds (4) | 578 | 860 |
| fixed | seeds (4) | 624 | 913 |
| fixed | seeds (4) + `recompute-sign-offset-overflow.bin` | 664 | 1228 |

The pre-fix binary panics on the regression input (the known overflow), so its figure is seeds only. On this seed set the fix does not lower coverage. The 3087 to 939 drop from `fuzzre-c` needs the larger corpus to reproduce, which this lane does not have.

## Fuzz run: 1 CPU-hour decode_stream (host a)

| Target | Runs (wall s) | CPU s (user + sys) | Crashes | EXIT | Final DONE (cov / ft) | Tree |
|---|---|---|---|---|---|---|
| ec-truehd `decode_stream` (seeds + regression dir, `-max_total_time=3600 -rss_limit_mb=1536 -timeout=30`) | 720,584 (3600 s) | 3537.0 + 39.7 = 3576.7 | 0 (no artifacts) | 0 | cov 962 / ft 5136 | fixed (this commit) |

Log on host a: `~/tmp/truehdrecover-decode_stream.log` (START 09:02:11Z, END 10:02:12Z). Coverage rose from 664 at replay to 962 over the hour, with no panic or timeout.

## Commit

Branch `lane/truehdrecover`, from `main` at `f43403af`. Changes: `crates/ec-truehd/src/decode.rs`, `crates/ec-truehd/tests/refused_au_recovery.rs`, `lanes/truehdrecover.report.md`. No push.

## Left

- The fix is verified on four fixtures and one corruption per byte of one AU. A longer corpus run (the 6008-seed corpus from `fuzzre-c`) would give the comparable coverage number.
- The snapshot clone per AU has not been measured.
