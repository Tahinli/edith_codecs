#!/usr/bin/env bash
# gen-fixtures.sh — deterministic ffmpeg-generated media fixtures for edith_codecs.
#
# ffmpeg is ORACLE/test-input tooling only; no ec-* crate depends on it at runtime.
# Idempotent: non-empty outputs are skipped, partial outputs are deleted on failure.
# Content is deterministic (sine tones per channel, testsrc2) so regenerating on
# another machine with the same ffmpeg yields the same bytes.
#
# Usage: scripts/gen-fixtures.sh [-f]     (-f: regenerate everything)
# Env:   EC_FIXTURES=<dir>  (default <repo>/fixtures)

set -uo pipefail

ROOT=$(cd -- "$(dirname -- "$0")/.." && pwd)
FIXTURES=${EC_FIXTURES:-$ROOT/fixtures}
DUR_A=3
DUR_V=2
FORCE=0
[ "${1:-}" = "-f" ] && FORCE=1

command -v ffmpeg >/dev/null || { echo "gen-fixtures: ffmpeg not found" >&2; exit 2; }

TMPLOG=$(mktemp) || exit 2
trap 'rm -f "$TMPLOG"' EXIT

n_made=0
n_skip=0
failed=()
unsupported=()

# gen <out> <ffmpeg args...>
gen() {
    local out=$1
    shift
    if [ "$FORCE" -eq 0 ] && [ -s "$out" ]; then
        n_skip=$((n_skip + 1))
        return 0
    fi
    mkdir -p -- "$(dirname -- "$out")"
    if ffmpeg -nostdin -y -v error "$@" "$out" 2>"$TMPLOG"; then
        n_made=$((n_made + 1))

        return 0
    fi
    rm -f -- "$out"
    failed+=("${out#"$FIXTURES"/}: $(tr '\n' ' ' <"$TMPLOG" | cut -c1-140)")
    return 1
}

# ---------------------------------------------------------------- audio -----
# Per-channel distinct frequencies: lets the oracle catch channel-order bugs
# (5.1 order FL,FR,FC,LFE,BL,BR) that identical-tone fixtures hide.
FREQS=(220 440 660 55 880 1320)

# build_audio_in <nch> <rate>  -> sets AIN[] and AMAP[]
build_audio_in() {
    local nch=$1 rate=$2 i lbl="" layout
    AIN=()
    for ((i = 0; i < nch; i++)); do
        AIN+=(-f lavfi -i "sine=frequency=${FREQS[i]}:sample_rate=${rate}:duration=${DUR_A}")
    done
    if [ "$nch" -eq 1 ]; then
        AMAP=(-map 0:a)
        return
    fi
    for ((i = 0; i < nch; i++)); do lbl="${lbl}[${i}:a]"; done
    case $nch in
        2) layout=stereo ;;
        6) layout=5.1 ;;
        *) layout=$nch ;;
    esac
    AMAP=(-filter_complex "${lbl}join=inputs=${nch}:channel_layout=${layout}[a]" -map "[a]")
}

# name|ext|encoder args
AUDIO_CODECS=(
    "wav16|wav|-c:a pcm_s16le"
    "flac|flac|-c:a flac -compression_level 5"
    "mp3|mp3|-c:a libmp3lame -b:a 192k"
    "aac-adts|aac|-c:a aac -b:a 160k"
    "aac-mp4|mp4|-c:a aac -b:a 160k"
    "ac3|ac3|-c:a ac3 -b:a 448k"
    "eac3|eac3|-c:a eac3 -b:a 448k"
    "vorbis-ogg|ogg|-c:a libvorbis -q:a 5"
    "opus-ogg|opus|-c:a libopus -b:a 160k -vbr on"
    "alac-mp4|m4a|-c:a alac"
)
LAYOUTS=("mono|1" "stereo|2" "5.1|6")
RATES=(44100 48000)

for spec in "${AUDIO_CODECS[@]}"; do
    IFS='|' read -r cname cext cargs <<<"$spec"
    read -r -a CARGS <<<"$cargs"
    for lspec in "${LAYOUTS[@]}"; do
        IFS='|' read -r lname nch <<<"$lspec"
        for rate in "${RATES[@]}"; do
            # Encoder capability gates — a skipped combo is reported, never silent.
            if [ "$cname" = "mp3" ] && [ "$nch" -gt 2 ]; then
                unsupported+=("audio/${cname}-${lname}-${rate}: libmp3lame is mono/stereo only")
                continue
            fi
            if [ "$cname" = "opus-ogg" ] && [ "$rate" != 48000 ]; then
                unsupported+=("audio/${cname}-${lname}-${rate}: libopus encodes at 48 kHz only")
                continue
            fi
            build_audio_in "$nch" "$rate"
            gen "$FIXTURES/audio/${cname}-${lname}-${rate}.${cext}" \
                "${AIN[@]}" "${AMAP[@]}" "${CARGS[@]}" -ar "$rate"
        done
    done
done

# ---------------------------------------------------------------- video -----
# name|encoder|extra args|pix_fmts (comma-separated)
VIDEO_CODECS=(
    "h264|libx264|-preset ultrafast -crf 23|yuv420p"
    "hevc|libx265|-preset ultrafast -crf 28 -x265-params log-level=error|yuv420p,yuv420p10le"
    "vp9|libvpx-vp9|-deadline realtime -cpu-used 8 -row-mt 1 -b:v 0 -crf 32|yuv420p,yuv420p10le"
    "av1|librav1e|-speed 10 -qp 100|yuv420p,yuv420p10le"
)
FPS=("23.976|24000/1001" "60|60")
CONTAINERS=(mp4 mkv)

# gen_video <codec> <encoder> <extra-args-str> <pix_fmt> <fpstag> <fpsval> <WxH> <restag> <container>
gen_video() {
    local codec=$1 enc=$2 extra=$3 pix=$4 fpstag=$5 fpsval=$6 size=$7 restag=$8 cont=$9
    local depth=8 tag=()
    case $pix in *10le) depth=10 ;; esac
    [ "$codec" = hevc ] && [ "$cont" = mp4 ] && tag=(-tag:v hvc1)
    local ex=()
    read -r -a ex <<<"$extra"
    gen "$FIXTURES/video/${codec}-${restag}-${fpstag}-${depth}bit.${cont}" \
        -f lavfi -i "testsrc2=size=${size}:rate=${fpsval}:duration=${DUR_V}" \
        -pix_fmt "$pix" -c:v "$enc" "${ex[@]}" "${tag[@]}"
}

for spec in "${VIDEO_CODECS[@]}"; do
    IFS='|' read -r vname venc vextra vpixs <<<"$spec"
    IFS=',' read -r -a PIXS <<<"$vpixs"
    for pix in "${PIXS[@]}"; do
        for fspec in "${FPS[@]}"; do
            IFS='|' read -r ftag fval <<<"$fspec"
            for cont in "${CONTAINERS[@]}"; do
                gen_video "$vname" "$venc" "$vextra" "$pix" "$ftag" "$fval" 1920x1080 1080p "$cont"
            done
        done
    done
    # One 4K sample per codec (8-bit, mp4, 23.976) — size/stride edge cases.
    gen_video "$vname" "$venc" "$vextra" yuv420p 23.976 24000/1001 3840x2160 2160p mp4
    # 1916x1080: not a multiple of the macroblock/CTU size, so the decoder must
    # honour the cropping rect / conformance window. The real library has BluRay
    # remuxes at exactly this size, and every other fixture here is CTU-aligned.
    case $vname in
        h264 | hevc) gen_video "$vname" "$venc" "$vextra" yuv420p 23.976 24000/1001 1916x1080 1916x1080 mp4 ;;
    esac
done

# -------------------------------------------------------------- summary -----
printf '\n%s\n' "== fixtures: $FIXTURES =="
printf '%-42s %10s  %s\n' FILE SIZE PROBE
total=0
while IFS= read -r f; do
    total=$((total + 1))
    sz=$(stat -c %s -- "$f")
    p=$(ffprobe -v error -show_entries stream=codec_name,channels,sample_rate,width,height,pix_fmt \
        -of default=nw=1:nk=1 "$f" 2>/dev/null | tr '\n' ' ')
    printf '%-42s %10s  %s\n' "${f#"$FIXTURES"/}" "$sz" "$p"
done < <(find "$FIXTURES/audio" "$FIXTURES/video" -type f 2>/dev/null | sort)

printf '\n%s\n' "total=$total generated=$n_made skipped-existing=$n_skip unsupported=${#unsupported[@]} failed=${#failed[@]}"
for u in "${unsupported[@]:-}"; do [ -n "$u" ] && printf 'UNSUPPORTED %s\n' "$u"; done
for e in "${failed[@]:-}"; do [ -n "$e" ] && printf 'FAILED      %s\n' "$e"; done
[ "${#failed[@]}" -eq 0 ] || exit 1
