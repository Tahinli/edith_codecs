#!/bin/bash
cd /home/tahinli/Documents/Code/Rust/edith_codecs-av1tpl2 || exit 1
export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1tpl2
L=lanes/tpl2-sweep2.log
for kd in "0.125 4" "0.125 8" "0.0625 8"; do
  set -- $kd
  echo "=== k=$1 d=$2 ===" >> $L
  timeout 900 env EC_COMP_MISMATCH=1 EC_AV1_TPL=$1 EC_AV1_TPL_D=$2 \
    cargo test -p ec-av1 --release --lib -- --ignored bd_rate_screen_native --nocapture >> $L 2>&1
  echo "RC=$? k=$1 d=$2" >> $L
done
echo SWEEPDONE >> $L
