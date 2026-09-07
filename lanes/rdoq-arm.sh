#!/bin/bash
# lane-rdoq: one native-gate arm (all five rows) under a named env set.
# usage: rdoq-arm.sh <name> <ENV=VAL ...>
name=$1; shift
B=$HOME/.cache/cargo-target-rdoq/release/deps/ec_av1-8fef0196ab10338d
systemd-run --user --unit rq-$name -p MemoryMax=8G --collect --working-directory=$PWD \
  bash -c "env EC_AV1_NATIVE_FILM=1 EC_AV1_NATIVE_FILM4K=1 EC_AV1_NATIVE_SCREEN=1 $* $B --ignored --nocapture --test-threads 1 bd_rate_screen_native > lanes/rdoq-$name.log 2>&1; echo RC=\$? >> lanes/rdoq-$name.log"
