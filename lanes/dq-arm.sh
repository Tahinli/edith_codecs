#!/bin/bash
# lane-deltaq: one native-gate arm under a named env set.
# usage: dq-arm.sh <name> <test> <ENV=VAL ...>
name=$1; test=$2; shift 2
B=$HOME/.cache/cargo-target-deltaq/release/deps/ec_av1-8fef0196ab10338d
systemd-run --user --unit dq-$name -p MemoryMax=8G --collect --working-directory=$PWD \
  bash -c "env $* $B --ignored --nocapture --test-threads 1 $test > lanes/dq-$name.log 2>&1; echo RC=\$? >> lanes/dq-$name.log"
