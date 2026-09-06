#!/bin/bash
cd /home/tahinli/Documents/Code/Rust/edith_codecs-av1tpl2 || exit 1
export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1tpl2
L=lanes/tpl2-sweep.log
for d in 4 8; do
  for k in 0.25 0.5 1 2; do
    echo "=== k=$k d=$d ===" >> $L
    timeout 900 env EC_COMP_MISMATCH=1 EC_AV1_TPL=$k EC_AV1_TPL_D=$d EC_AV1_TPL_HIST=1 \
      cargo test -p ec-av1 --release --lib -- --ignored bd_rate_screen_native --nocapture >> $L 2>&1
    echo "RC=$? k=$k d=$d" >> $L
  done
done
echo SWEEPDONE >> $L
