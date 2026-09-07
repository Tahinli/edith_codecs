#!/bin/bash
# lane-av1fast: one native-gate arm (bars 1080p + film A) under a named env set.
# usage: arm.sh <name> <ENV=VAL ...>
name=$1; shift
B=$HOME/.cache/cargo-target-av1fast/release/deps/ec_av1-8fef0196ab10338d
systemd-run --user --unit fa-$name -p MemoryMax=8G --collect --working-directory=$PWD \
  bash -c "env EC_AV1_NATIVE_FILM=1 $* $B --ignored --nocapture --test-threads 1 bd_rate_screen_native > lanes/ab-$name.log 2>&1; echo RC=\$? >> lanes/ab-$name.log"
