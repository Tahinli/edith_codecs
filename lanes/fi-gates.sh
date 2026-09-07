#!/bin/bash
# lane-av1fast: scoped gates + invariants at presets 0/3/6/10 (filter-intra lever).
B=$1
cd "$(dirname "$0")/.."
rm -f lanes/fi-gates.log
for s in 0 3 6 10; do
  echo "===== speed $s scoped =====" >> lanes/fi-gates.log
  env EC_AV1_SPEED=$s EC_COMP_MISMATCH=1 "$B" --test-threads 4 encode:: encoder:: tile:: decode:: >> lanes/fi-gates.log 2>&1
  echo "RC=$?" >> lanes/fi-gates.log
  echo "===== speed $s invariants =====" >> lanes/fi-gates.log
  env EC_AV1_SPEED=$s EC_COMP_MISMATCH=1 "$B" --include-ignored --test-threads 2 tile_bytes_do_not_depend_on_the_thread_count the_facade_codes_the_same_bytes_as_encode_sequence >> lanes/fi-gates.log 2>&1
  echo "RC=$?" >> lanes/fi-gates.log
done
echo "===== preset decode-exact (ignored) =====" >> lanes/fi-gates.log
env EC_COMP_MISMATCH=1 "$B" --ignored --test-threads 1 every_speed_preset_decodes_sample_exact_through_both_decoders >> lanes/fi-gates.log 2>&1
echo "RC=$?" >> lanes/fi-gates.log
touch lanes/fi-gates-done
