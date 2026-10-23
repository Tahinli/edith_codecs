#!/usr/bin/env bash
# bench-rows.sh: local best-of-5 decode rows (DONE.md D7: speed is measured on the local
# 12-core machine). Rows: ec-vp8 decode, ec-vp9 decode at 1/2/4/6 tile threads, ec-ac3 5.1
# 448k decode with parity, ec-h264 decode at 1/2/4/6 cores (the decoder runs 1 thread).
# Every row starts only when the 1-minute load is below 3; otherwise it waits in 290 s
# steps for at most 1 h (12 steps). Clips come from scripts/bench-rows-clips.sh.
# Usage: scripts/bench-rows.sh   (prints markdown rows; also saved under $BENCH_CLIPS)
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
clips=${BENCH_CLIPS:-$HOME/.cache/benchrows-clips}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/.cache/tgt-benchrows}
export TMPDIR=${TMPDIR:-$HOME/.cache/tmp-benchrows}
mkdir -p "$TMPDIR"
rel=$CARGO_TARGET_DIR/release
today=$(date +%F)
res="$clips/bench-rows-$today.md"
: > "$res"

# Prints the 1-minute load once it is below 3. Returns 1 after 1 h of waiting.
gate() {
  local steps=0 l
  while :; do
    l=$(cut -d' ' -f1 /proc/loadavg)
    if awk -v l="$l" 'BEGIN { exit !(l < 3) }'; then echo "$l"; return 0; fi
    if [ "$steps" -ge 12 ]; then echo "$l"; return 1; fi
    sleep 290
    steps=$((steps + 1))
  done
}

# emit <crate> <clip> <threads> <load> <fps> <realtime> <check>
emit() {
  printf '| %s | %s | %s | local 12-core | %s | %s | %s | %s | %s |\n' \
    "$today" "$1" "$2" "$3" "$4" "$5" "$6" "$7" | tee -a "$res"
}

# ---- build (outside the gate; nothing is timed here) ----
cd "$root"
cargo build --release -q -p ec-vp9 --example decode_ivf
cargo build --release -q -p ec-ac3 --example ac3dec
cargo build --release -q --manifest-path tools/ec-bench/Cargo.toml
cp scripts/bench-rows-vp8.rs crates/ec-vp8/examples/vp8_ivf.rs
trap 'rm -f "$root/crates/ec-vp8/examples/vp8_ivf.rs"' EXIT
cargo build --release -q -p ec-vp8 --example vp8_ivf

echo "| date | crate | clip | host | threads | load 1m at start | fps (best of 5) | realtime x | check |"
echo "|---|---|---|---|---|---|---|---|---|"

# ---- ec-vp8 decode, 1080p30 libvpx clip, 1 thread (the decoder has no thread knob) ----
vp8=$clips/vp8-1080p30.ivf
if load=$(gate); then
  line=$("$rel/examples/vp8_ivf" "$vp8" 5)
  fps=$(sed -n 's/.*fps=\([0-9.]*\).*/\1/p' <<<"$line")
  raw=$TMPDIR/vp8.yuv
  "$rel/examples/vp8_ivf" "$vp8" 1 "$raw" >/dev/null
  got=$(md5sum "$raw" | cut -d' ' -f1); rm -f "$raw"
  want=$(cat "$clips/vp8-1080p30.yuv.md5")
  check="md5 $([ "$got" = "$want" ] && echo "= ffmpeg" || echo "!= ffmpeg ($got)")"
  emit ec-vp8 "1080p30 libvpx 10 s, 4 Mb/s" 1 "$load" "$fps" "$(awk -v f="$fps" 'BEGIN{printf "%.2fx", f/30}')" "$check"
else
  emit ec-vp8 "1080p30 libvpx 10 s" 1 "$load" - - "load gate timed out"
fi

# ---- ec-vp9 decode, 1080p30 profile 0, 4 tile columns, threads 1/2/4/6 ----
vp9=$clips/vp9-1080p30-tile4.ivf
want=$(cat "$clips/vp9-1080p30-tile4.yuv.md5")
for t in 1 2 4 6; do
  if load=$(gate); then
    best=0
    for _ in 1 2 3 4 5; do
      f=$("$rel/examples/decode_ivf" "$vp9" "$t" | sed -n 's/.*decode_fps=\([0-9.]*\).*/\1/p')
      best=$(awk -v a="$best" -v b="$f" 'BEGIN{print (b > a ? b : a)}')
    done
    raw=$TMPDIR/vp9.yuv
    EC_RAW_OUT=$raw "$rel/examples/decode_ivf" "$vp9" "$t" >/dev/null
    got=$(md5sum "$raw" | cut -d' ' -f1); rm -f "$raw"
    check="md5 $([ "$got" = "$want" ] && echo "= ffmpeg" || echo "!= ffmpeg ($got)")"
    emit ec-vp9 "1080p30 profile 0 libvpx-vp9 10 s, 4 tile cols" "$t" "$load" "$best" \
      "$(awk -v f="$best" 'BEGIN{printf "%.2fx", f/30}')" "$check"
  else
    emit ec-vp9 "1080p30 profile 0 tile4" "$t" "$load" - - "load gate timed out"
  fi
done

# ---- ec-ac3 decode, AC-3 5.1 448 kb/s, 60 s; speed to /dev/null, parity to a file ----
ac3=$clips/ac3-5.1-448k.ac3
if load=$(gate); then
  best=0
  for _ in 1 2 3 4 5; do
    rt=$("$rel/examples/ac3dec" "$ac3" /dev/null | awk -F'\t' '/^realtime/{print $2}')
    best=$(awk -v a="$best" -v b="$rt" 'BEGIN{print (b > a ? b : a)}')
  done
  out=$TMPDIR/ac3.f32
  errs=$("$rel/examples/ac3dec" "$ac3" "$out" | awk -F'\t' '/^errors/{print $2}')
  par=$(python3 -I "$root/scripts/bench-rows-parity.py" "$clips/ac3-5.1-448k.ref.f32" "$out" 6 | tail -1)
  rm -f "$out"
  emit ec-ac3 "AC-3 5.1 448 kb/s 60 s (5.1 layout)" 1 "$load" "-" "${best}x" \
    "decode errors $errs; parity: $par"
else
  emit ec-ac3 "AC-3 5.1 448 kb/s" 1 "$load" - - "load gate timed out"
fi

# ---- ec-h264 decode: the bench.log:145 row, from tools/ec-bench itself, pinned to 1/2/4/6
# cores. The decoder has no thread knob (ec-h264 spawns threads only in enc/), so the
# core count is the only parallelism available; 5 full ec-bench runs per core count. ----
bench=$CARGO_TARGET_DIR/release/ec-bench
for n in 1 2 4 6; do
  best=""
  loads=""
  for _ in 1 2 3 4 5; do
    if load=$(gate); then
      line=$(taskset -c "0-$((n - 1))" "$bench" | grep '^| ec-h264 | decode')
      ms=$(awk -F'|' '{gsub(/ /,"",$6); print $6}' <<<"$line")
      frames=$(awk -F'|' '{print $4}' <<<"$line" | sed -n 's/.*, \([0-9]*\) frames.*/\1/p')
      loads="$loads $load"
      if [ -z "$best" ] || awk -v a="$ms" -v b="$best" 'BEGIN{exit !(a < b)}'; then
        best=$ms; bestframes=$frames
      fi
    else
      loads="$loads gate-timeout"
    fi
  done
  fps=$(awk -v f="$bestframes" -v m="$best" 'BEGIN{printf "%.2f", f / (m / 1000)}')
  emit ec-h264 "1080p30 film 10 s, ec-bench row (wall best $best ms, $bestframes frames)" 1 \
    "$(awk '{print $1}' <<<"${loads# }")" "$fps" "$(awk -v f="$fps" 'BEGIN{printf "%.2fx", f/30}')" \
    "cores pinned $n, decoder threads 1; loads of 5 runs:$loads"
done
echo "saved: $res" >&2
