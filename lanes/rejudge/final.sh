#!/bin/bash
cd /home/tahinli/Documents/Code/Rust/edith_codecs-av1rejudge
export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1rejudge
L=lanes/rejudge/final; mkdir -p $L
run(){ n=$1; shift; echo "=== $n $(date +%T)" >> $L/summary.txt; "$@" > $L/$n.log 2>&1; echo "RC=$?" >> $L/summary.txt; }
run scoped env EC_COMP_MISMATCH=1 cargo test -p ec-av1 --release --lib -- encode:: tile::
run threadcount env EC_COMP_MISMATCH=1 cargo test -p ec-av1 --release --lib -- --include-ignored tile_bytes_do_not_depend_on_the_thread_count --nocapture
run ownsdecoder env EC_COMP_MISMATCH=1 cargo test -p ec-av1 --release --lib -- --include-ignored every_frame_of_a_sequence_decodes_through_our_own_decoder --nocapture
run native_tiles22 env EC_COMP_MISMATCH=1 EC_AV1_TILES=2:2 cargo test -p ec-av1 --release --lib -- --ignored bd_rate_screen_native --nocapture
run native_final env EC_COMP_MISMATCH=1 cargo test -p ec-av1 --release --lib -- --ignored bd_rate_screen_native --nocapture
run small_final env EC_COMP_MISMATCH=1 cargo test -p ec-av1 --release --lib -- --ignored bd_rate_vs_libaom_and_rav1e --nocapture
echo ALLDONE >> $L/summary.txt
