#!/bin/bash
B=$HOME/.cache/cargo-target-tplwin/release/deps/ec_av1-8fef0196ab10338d
export TMPDIR=$HOME/.cache/tw-tmp
for p in 0 3 6; do
  for t in tile_bytes_do_not_depend_on_the_thread_count the_facade_codes_the_same_bytes_as_encode_sequence; do
    echo "=== preset $p $t"
    EC_AV1_SPEED=$p EC_COMP_MISMATCH=1 $B --include-ignored --test-threads 1 "$t" 2>&1 | grep -E "EC_COMP_MISMATCH|test result|^test .* \.\.\."
  done
done
