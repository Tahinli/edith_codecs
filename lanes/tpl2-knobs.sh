#!/bin/bash
cd /home/tahinli/Documents/Code/Rust/edith_codecs-av1tpl2 || exit 1
export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1tpl2
L=lanes/tpl2-knobs.log
for knob in NONE EC_AV1_OBMC EC_AV1_TX32_DEPTH EC_AV1_COMP_VARTX; do
  echo "=== prop map + $knob ===" >> $L
  timeout 900 env EC_COMP_MISMATCH=1 EC_AV1_TPL_PROP=1 EC_AV1_TPL=0.5 EC_AV1_TPL_D=8 EC_AV1_TPL_P=0.5 ${knob}=1 \
    cargo test -p ec-av1 --release --lib -- --ignored bd_rate_screen_native --nocapture >> $L 2>&1
  echo "RC=$? $knob" >> $L
done
echo KNOBSDONE >> $L
