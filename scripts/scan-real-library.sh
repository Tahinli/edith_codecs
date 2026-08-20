#!/usr/bin/env bash
# scan-real-library.sh — ffprobe sweep of the user's real media library.
#
# READ-ONLY on the scanned directories; the only thing written is the manifest
# under fixtures/ (gitignored — regenerable). The manifest is the T3 gate input:
# every codec/container claim in this repo is checked against these files, not
# only against synthetic fixtures.
#
# Usage: scripts/scan-real-library.sh [dir ...]   (default: ~/Videos ~/Downloads)
# Env:   EC_FIXTURES=<dir>  (default <repo>/fixtures)
# Out:   fixtures/real-library-manifest.tsv
#        columns: path container vcodec width height pix_fmt bit_depth acodecs duration size

set -uo pipefail

ROOT=$(cd -- "$(dirname -- "$0")/.." && pwd)
FIXTURES=${EC_FIXTURES:-$ROOT/fixtures}
OUT=$FIXTURES/real-library-manifest.tsv

command -v ffprobe >/dev/null || { echo "scan-real-library: ffprobe not found" >&2; exit 2; }

DIRS=("$@")
[ "${#DIRS[@]}" -eq 0 ] && DIRS=("$HOME/Videos" "$HOME/Downloads" "$HOME/Music")

EXTS=(mp4 m4v mov mkv webm avi flv ts m2ts mts wmv ogv
    mp3 m4a aac flac wav ogg oga opus wma ac3 eac3 alac aif aiff)

mkdir -p -- "$FIXTURES"
tmp=$(mktemp) || exit 2
trap 'rm -f "$tmp"' EXIT

# Build the -iname predicate list once.
find_args=()
for e in "${EXTS[@]}"; do find_args+=(-iname "*.$e" -o); done
unset 'find_args[${#find_args[@]}-1]'

# field <key> <ffprobe-kv-block> — "" when absent or N/A.
field() {
    local v
    v=$(printf '%s\n' "$2" | sed -n "s/^$1=//p" | head -n1)
    [ "$v" = "N/A" ] && v=""
    printf '%s' "$v"
}

n=0
nerr=0
{
    printf 'path\tcontainer\tvcodec\twidth\theight\tpix_fmt\tbit_depth\tacodecs\tduration\tsize\n'
    while IFS= read -r -d '' f; do
        # Key=value form (not nk=1): a field ffprobe omits must not shift the others.
        fmt=$(ffprobe -v error -show_entries format=format_name,duration \
            -of default=nw=1 "$f" 2>/dev/null)
        if [ -z "$fmt" ]; then
            nerr=$((nerr + 1))
            printf '%s\tUNREADABLE\t-\t-\t-\t-\t-\t-\t-\t%s\n' "$f" "$(stat -c %s -- "$f")"
            continue
        fi
        container=$(field format_name "$fmt")
        duration=$(field duration "$fmt")
        v=$(ffprobe -v error -select_streams v:0 \
            -show_entries stream=codec_name,width,height,pix_fmt,bits_per_raw_sample \
            -of default=nw=1 "$f" 2>/dev/null)
        vcodec=$(field codec_name "$v")
        vw=$(field width "$v")
        vh=$(field height "$v")
        vpix=$(field pix_fmt "$v")
        vbits=$(field bits_per_raw_sample "$v")
        # bits_per_raw_sample is often absent/0 — fall back to the pix_fmt suffix.
        if [ -z "$vbits" ] || [ "$vbits" = "N/A" ] || [ "$vbits" = 0 ]; then
            case $vpix in
                *12le | *12be) vbits=12 ;;
                *10le | *10be) vbits=10 ;;
                "") vbits="" ;;
                *) vbits=8 ;;
            esac
        fi
        acodecs=$(ffprobe -v error -select_streams a -show_entries stream=codec_name \
            -of default=nw=1:nk=1 "$f" 2>/dev/null | paste -sd, -)
        printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
            "$f" "${container:--}" "${vcodec:--}" "${vw:--}" "${vh:--}" \
            "${vpix:--}" "${vbits:--}" "${acodecs:--}" "${duration:--}" \
            "$(stat -c %s -- "$f")"
        n=$((n + 1))
    done < <(find "${DIRS[@]}" -type f \( "${find_args[@]}" \) -print0 2>/dev/null)
} >"$tmp"

mv -- "$tmp" "$OUT"
trap - EXIT

printf '%s\n' "== $OUT =="
printf 'scanned=%s unreadable=%s\n\n' "$((n + nerr))" "$nerr"
printf 'containers:\n'
tail -n +2 "$OUT" | cut -f2 | sort | uniq -c | sort -rn
printf '\nvideo codecs:\n'
tail -n +2 "$OUT" | cut -f3 | sort | uniq -c | sort -rn
printf '\naudio codecs (per file, comma-joined):\n'
tail -n +2 "$OUT" | cut -f8 | tr ',' '\n' | sort | uniq -c | sort -rn
printf '\nbit depths:\n'
tail -n +2 "$OUT" | cut -f7 | sort | uniq -c | sort -rn
