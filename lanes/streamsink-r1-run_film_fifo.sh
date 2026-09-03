#!/bin/bash
# lane-streamsink r1: same census as run_film.sh, but our decoder streams its
# frames into a FIFO (ec_av1::stream::decode_stream_with, one frame written the
# moment it completes) and cmp2.py reads one frame from that FIFO and one from
# ffmpeg's stdout at a time. Neither side ever holds more than a frame, so the
# segment length no longer bounds memory -- peak RSS of the decoder is ~0.44 GB
# on a 4K 10-bit window regardless of how long the window is.
# usage: run_film_fifo.sh <name> <film> <width> <height> <seg_seconds> [probe]
set -u
S="$(cd "$(dirname "$0")" && pwd)"
NAME="$1"; FILM="$2"; W="$3"; H="$4"; SEG="$5"
PROBE="${6:-$HOME/.cache/cargo-target-streamsink/release/examples/decode_probe}"
T=$HOME/.cache/fullfilm-fifo/$NAME
rm -rf "$T"; mkdir -p "$T"
STATE=$T/state.json
DUR=$(ffprobe -v error -show_entries format=duration -of default=nw=1:nk=1 "$FILM" | cut -d. -f1)
echo "START name=$NAME res=${W}x${H} depth=10 codec=av1 duration_s=$DUR seg_s=$SEG mode=fifo"
T0=$(date +%s)
s=0
while [ "$s" -lt "$DUR" ]; do
  seg=$T/seg.obu; fifo=$T/ours.fifo
  rm -f "$seg" "$fifo"
  ffmpeg -nostdin -loglevel error -ss "$s" -t "$SEG" -i "$FILM" -map 0:v:0 -c:v copy -an -f obu "$seg" -y 2>/dev/null
  if [ ! -s "$seg" ]; then echo "t=${s}s NO-SEGMENT"; s=$((s+SEG)); continue; fi
  mkfifo "$fifo"
  base=$(python3 -c 'import json,sys,os
p=sys.argv[1]
print(json.load(open(p))["compared"] if os.path.exists(p) else 0)' "$STATE")
  EC_PROBE_OUT16="$fifo" timeout 7200 "$PROBE" "$seg" > "$T/probe.log" 2>&1 &
  probe=$!
  line=$(ffmpeg -nostdin -loglevel error -i "$seg" -pix_fmt yuv420p10le -f rawvideo - 2>/dev/null \
    | python3 "$S/cmp2.py" "$fifo" "$W" "$H" "$STATE" "$base")
  wait $probe; rc=$?
  r=$(grep -E '^REFUSED|^OK|panick|rror' "$T/probe.log" | head -1)
  [ $rc -ge 128 ] && r="KILLED(rc=$rc)"
  [ -z "$r" ] && r="NO-MATCH-LINE(rc=$rc)"
  now=$(date +%s); el=$((now-T0)); [ "$el" -lt 1 ] && el=1
  tot=$(python3 -c 'import json,sys,os
p=sys.argv[1]
print(json.load(open(p))["compared"] if os.path.exists(p) else 0)' "$STATE")
  fpm=$(python3 -c "print(round($tot*60.0/$el,2))")
  printf 't=%ss\t%s\t%b\telapsed_s=%s\tfpm=%s\n' "$s" "$r" "$line" "$el" "$fpm"
  rm -f "$seg" "$fifo"
  s=$((s+SEG))
done
now=$(date +%s); el=$((now-T0)); [ "$el" -lt 1 ] && el=1
python3 -c '
import json,sys
st=json.load(open(sys.argv[1])); el=int(sys.argv[2]); name=sys.argv[3]
print("FULLFILM name=%s compared=%d differing=%d first_bad=%s first_bad_bytes=%d max_bytes=%d segments=%d elapsed_s=%d fpm=%.2f"
      % (name, st["compared"], st["differing"], st["first_bad"] if st["first_bad"] is not None else "-",
         st["first_bad_bytes"], st["max_bytes"], st["segments"], el, st["compared"]*60.0/el))' "$STATE" "$el" "$NAME"
