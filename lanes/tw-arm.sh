#!/bin/bash
# lane-tplwin: one native-gate arm at a named speed/tpl-depth.
# usage: tw-arm.sh <name> <ENV=VAL ...>
name=$1; shift
B=$HOME/.cache/cargo-target-tplwin/release/deps/ec_av1-8fef0196ab10338d
systemd-run --user --unit tw-$name -p MemoryMax=8G --collect --working-directory=$PWD \
  bash -c "env TMPDIR=$HOME/.cache/tw-tmp $* $B --ignored --nocapture --test-threads 1 bd_rate_screen_native > lanes/tw-$name.log 2>&1; echo RC=\$? >> lanes/tw-$name.log"
