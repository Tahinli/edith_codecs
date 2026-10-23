#!/usr/bin/env bash
# aac-encode-score.sh: encode reference clips with ffmpeg's native AAC encoder and with ec-aac,
# decode both with ffmpeg, and print objective metrics (scripts/aac_score.py).
#
# Usage:
#   scripts/aac-encode-score.sh clips <dir> <src>...   # make 10 s, 48 kHz stereo f32le clips
#   scripts/aac-encode-score.sh encode <dir>           # encode + decode every clip (runs on a server)
#   scripts/aac-encode-score.sh score <dir>            # print the metrics table (local, numpy)
#
# Clip files are <dir>/clips/<name>.f32le (stereo) and <dir>/clips/<name>-51.f32le (5.1).
# Outputs go to <dir>/out/. Configs: stereo 64, 128 and 256 kbps; 5.1 at 384 kbps.
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
cmd=${1:?usage: aac-encode-score.sh clips|encode|score <dir> ...}
dir=${2:?missing <dir>}
mkdir -p "$dir/clips" "$dir/out"

case $cmd in
clips)
  shift 2
  i=0
  for src in "$@"; do
    i=$((i + 1))
    name=clip$i
    ffmpeg -v error -y -ss 30 -t 10 -i "$src" -ac 2 -ar 48000 -f f32le "$dir/clips/$name.f32le"
    # Synthetic 5.1 from the stereo clip: front L/R, centre, LFE from the mix, rears from L/R.
    ffmpeg -v error -y -f f32le -ar 48000 -ac 2 -i "$dir/clips/$name.f32le" \
      -af "pan=5.1|FL=c0|FR=c1|FC=0.5*c0+0.5*c1|LFE=0.1*c0+0.1*c1|BL=0.3*c0|BR=0.3*c1" \
      -ac 6 -ar 48000 -f f32le "$dir/clips/$name-51.f32le"
  done
  ;;
encode)
  # Runs on the server through remote-run; needs ffmpeg and cargo, not numpy.
  cd "$here"
  cargo build --release -q -p ec-aac --example aacenc_file
  bin=$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/release/examples/aacenc_file
  for clip in "$dir"/clips/*.f32le; do
    base=$(basename "$clip" .f32le)
    case $base in *-51) ch=6; configs="384" ;; *) ch=2; configs="64 128 256" ;; esac
    for k in $configs; do
      tag=$base-$k
      ffmpeg -v error -y -f f32le -ar 48000 -ac $ch -i "$clip" -c:a aac -b:a "${k}k" -f adts "$dir/out/$tag.ff.aac"
      "$bin" "$clip" $ch 48000 "$k" "$dir/out/$tag.ec.aac"
      for enc in ff ec; do
        ffmpeg -v error -y -i "$dir/out/$tag.$enc.aac" -f f32le -ar 48000 -ac $ch "$dir/out/$tag.$enc.dec.f32le"
      done
      echo "encoded $tag"
    done
  done
  ;;
score)
  for clip in "$dir"/clips/*.f32le; do
    base=$(basename "$clip" .f32le)
    case $base in *-51) ch=6; configs="384" ;; *) ch=2; configs="64 128 256" ;; esac
    for k in $configs; do
      tag=$base-$k
      echo "== $tag"
      python3 "$here/scripts/aac_score.py" "$clip" $ch \
        "ffmpeg=$dir/out/$tag.ff.dec.f32le:$dir/out/$tag.ff.aac" \
        "ec-aac=$dir/out/$tag.ec.dec.f32le:$dir/out/$tag.ec.aac"
    done
  done
  ;;
*)
  echo "unknown command: $cmd" >&2
  exit 2
  ;;
esac
