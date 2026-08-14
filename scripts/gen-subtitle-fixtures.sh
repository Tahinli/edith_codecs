#!/usr/bin/env bash
# gen-subtitle-fixtures.sh — extract real subtitle streams out of the library.
#
# READ-ONLY on the library; writes only under fixtures/ (gitignored). Picks the
# first manifest file carrying each subtitle codec and copies that stream out
# without re-encoding, so what the tests parse is exactly the bytes a disc rip
# holds. The synthetic .srt/.vtt/.ass cases live inside the crates' own unit
# tests; what cannot be written by hand is a real PGS display set.
#
# Usage: scripts/gen-subtitle-fixtures.sh
# Env:   EC_FIXTURES=<dir>  (default <repo>/fixtures)
#        EC_PGS_SECONDS=<n> (default 900 — a whole film's worth is 30 MB)
# In:    fixtures/real-library-manifest.tsv (scripts/scan-real-library.sh)
# Out:   fixtures/subs/{pgs-1080p.sup,real.ass,real.srt}

set -uo pipefail

ROOT=$(cd -- "$(dirname -- "$0")/.." && pwd)
FIXTURES=${EC_FIXTURES:-$ROOT/fixtures}
MANIFEST=$FIXTURES/real-library-manifest.tsv
OUT=$FIXTURES/subs
SECONDS_LIMIT=${EC_PGS_SECONDS:-900}

command -v ffmpeg >/dev/null || { echo "gen-subtitle-fixtures: ffmpeg not found" >&2; exit 2; }
[ -r "$MANIFEST" ] || { echo "gen-subtitle-fixtures: run scripts/scan-real-library.sh first" >&2; exit 2; }

mkdir -p -- "$OUT"

# first_with <codec_name> — path of the first manifest file holding such a
# subtitle stream, empty when the library has none.
first_with() {
    local codec=$1 path
    while IFS=$'\t' read -r path _; do
        [ -r "$path" ] || continue
        if ffprobe -v error -select_streams s -show_entries stream=codec_name -of csv=p=0 -- "$path" 2>/dev/null |
            grep -qx "$codec"; then
            printf '%s\n' "$path"
            return 0
        fi
    done < <(tail -n +2 -- "$MANIFEST")
    return 1
}

# extract <codec_name> <output> [ffmpeg args...]
extract() {
    local codec=$1 out=$2; shift 2
    local src
    if ! src=$(first_with "$codec"); then
        echo "gen-subtitle-fixtures: no $codec stream in the library — skipping $(basename "$out")" >&2
        return 0
    fi
    # -map by codec so a dual-subtitle file gives the stream we asked for.
    local index
    index=$(ffprobe -v error -select_streams s -show_entries stream=index,codec_name -of csv=p=0 -- "$src" |
        awk -F, -v c="$codec" '$2 == c {print $1; exit}')
    if ffmpeg -v error -y "$@" -i "$src" -map "0:$index" -c copy -- "$out"; then
        printf '%s\t%s\n' "$(basename "$out")" "$src"
    else
        echo "gen-subtitle-fixtures: ffmpeg failed on $src" >&2
    fi
}

extract hdmv_pgs_subtitle "$OUT/pgs-1080p.sup" -t "$SECONDS_LIMIT"
extract ass "$OUT/real.ass"
extract subrip "$OUT/real.srt"
