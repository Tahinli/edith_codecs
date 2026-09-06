#!/bin/bash
# usage: run.sh <logdir> <name>=<envspec> ...
cd /home/tahinli/Documents/Code/Rust/edith_codecs-av1rejudge
export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1rejudge
LOGDIR=$1; shift
mkdir -p "$LOGDIR"
for spec in "$@"; do
  name=${spec%%=*}; envs=${spec#*=}
  echo "=== ARM $name env=[$envs] $(date +%T)" >> "$LOGDIR/summary.txt"
  env EC_COMP_MISMATCH=1 $envs cargo test -p ec-av1 --release --lib -- --ignored bd_rate_screen_native --nocapture > "$LOGDIR/$name.log" 2>&1
  echo "RC=$?" >> "$LOGDIR/summary.txt"
done
echo "ALLDONE" >> "$LOGDIR/summary.txt"
