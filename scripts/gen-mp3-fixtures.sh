#!/usr/bin/env bash
# MP3 fixtures beyond the family matrix in gen-fixtures.sh: every Layer III
# sampling frequency (MPEG-1, MPEG-2 LSF and MPEG-2.5), both channel modes,
# CBR and VBR, on material that forces block switching and a wide spread of
# Huffman tables.
#
# The family matrix only covers 44.1/48 kHz stereo and mono at one bitrate,
# which leaves the LSF side info, the 8 kHz band layout and the short-block
# path unexercised — every one of those is a separate table in the decoder.
#
# Output: fixtures/audio/mp3ext-<mode>-<rate>-<setting>.mp3 (gitignored).
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
FIXTURES="$here/../fixtures"
mkdir -p "$FIXTURES/audio"

src="$FIXTURES/audio/.mp3src.wav"
if [ ! -f "$src" ]; then
    # A tone with a glide, broadband noise and a click every half second: the
    # click is what makes an encoder switch to short blocks, and the noise is
    # what makes it reach for the escape-coded Huffman tables.
    left="0.35*sin(2*PI*440*t+3*sin(2*PI*1.5*t))+0.12*(random(0)-0.5)+0.7*lt(mod(t\,0.5)\,0.002)"
    right="0.30*sin(2*PI*443*t)+0.12*(random(1)-0.5)+0.7*lt(mod(t\,0.5)\,0.002)"
    ffmpeg -v error -y -f lavfi -i "aevalsrc='${left}'|'${right}':d=3:s=48000" \
        -c:a pcm_s16le "$src"
fi

# The same material as PCM, mono and stereo, for the encoder's quality table:
# a tone fixture cannot tell two encoders apart, and this one can.
for rate in 44100 48000; do
    for mode in mono stereo; do
        channels=1
        [ "$mode" = stereo ] && channels=2
        ffmpeg -v error -y -i "$src" -ar "$rate" -ac "$channels" -c:a pcm_s16le \
            "$FIXTURES/audio/mp3src-${mode}-${rate}.wav"
    done
done

RATES=(48000 44100 32000 24000 22050 16000 12000 11025 8000)
made=0
for rate in "${RATES[@]}"; do
    for mode in mono stereo; do
        channels=1
        [ "$mode" = stereo ] && channels=2
        for setting in cbr128 cbr320 vbr2; do
            case "$setting" in
            cbr128) opts=(-b:a 128k) ;;
            cbr320) opts=(-b:a 320k) ;;
            vbr2) opts=(-q:a 2) ;;
            esac
            # MPEG-2/2.5 rates cap out well below 320 kbit/s.
            if [ "$setting" = cbr320 ] && [ "$rate" -lt 32000 ]; then
                continue
            fi
            out="$FIXTURES/audio/mp3ext-${mode}-${rate}-${setting}.mp3"
            ffmpeg -v error -y -i "$src" -ar "$rate" -ac "$channels" \
                -c:a libmp3lame "${opts[@]}" "$out"
            made=$((made + 1))
        done
    done
done
# One simple-stereo file: LAME picks joint stereo by default, so the plain
# left/right path would otherwise never be decoded.
ffmpeg -v error -y -i "$src" -ar 44100 -ac 2 -c:a libmp3lame -b:a 192k \
    -joint_stereo 0 "$FIXTURES/audio/mp3ext-simplestereo-44100-cbr192.mp3"
made=$((made + 1))

echo "wrote $made MP3 fixtures to $FIXTURES/audio"
