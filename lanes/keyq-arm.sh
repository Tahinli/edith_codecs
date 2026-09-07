#!/usr/bin/env bash
# keyq lane: one long-GOP (or 12-frame) BD arm on the prebuilt release binary.
# usage: keyq-arm.sh <unit> <log> <test> [<pyramid spec>|control]
unit=$1; log=$2; test=$3; spec=$4
bin=$HOME/.cache/cargo-target-keyq/release/deps/ec_av1-8fef0196ab10338d
env=""
[ "$spec" = control ] || env="EC_AV1_PYRAMID=$spec"
systemd-run --user --unit "$unit" -p MemoryMax=8G --collect bash -c \
  "cd /home/tahinli/Documents/Code/Rust/edith_codecs-keyq; $env $bin --ignored --nocapture --test-threads 1 --exact $test > $log 2>&1; echo RC=\$? >> $log"
