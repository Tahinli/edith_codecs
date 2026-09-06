#!/bin/bash
cd /home/tahinli/Documents/Code/Rust/edith_codecs-av1tpl2 || exit 1
export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1tpl2
EC_COMP_MISMATCH=1 timeout 3000 cargo test -p ec-av1 --release > lanes/tpl2-suite.log 2>&1
echo "RC=$?" >> lanes/tpl2-suite.log
