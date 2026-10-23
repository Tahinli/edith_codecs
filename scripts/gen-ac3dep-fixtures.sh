#!/usr/bin/env bash
# gen-ac3dep-fixtures.sh: E-AC-3 dependent-substream fixtures for crates/ec-ac3 (root fixtures/audio).
#
# ffmpeg's E-AC-3 encoder stops at 5.1, so the 7.1 fixtures are spliced: an ffmpeg 5.1 encode
# as the independent substream, and an ffmpeg stereo encode whose frames become dependent
# substreams (scripts/splice-eac3-dependent.py). Outputs:
#   ac3dep-src-5.1.eac3       independent source (ffmpeg decodes it: the 5.1 reference)
#   ac3dep-src-2.0.eac3       dependent source (ffmpeg decodes it: the 2.0 reference)
#   ac3dep-7.1-48000.eac3     5.1 + dependent Lrs/Rrs (chanmap 0x0200) -> 8 channels, "7.1": BL/BR appended
#   ac3dep-7.1wide-48000.eac3 5.1 + dependent Lc/Rc    (chanmap 0x0400) -> 8 channels, "7.1(wide-side)"
#   ac3dep-repl-48000.eac3    5.1 + dependent Ls/Rs    (chanmap 0x1800) -> 6 channels, "5.1(side)": the
#                             dependent replaces the core's Ls/Rs (ffmpeg's placement rule, see dependent.rs)
# Chanmaps are Table E2.5 as a 16-bit value, bit 15 first (see crates/ec-ac3/src/dependent.rs).
# The ac3dep-* names keep these out of decode_matrix's ac3-/eac3- sweep, which decodes one
# syncframe at a time and so cannot read packets that carry a dependent substream.
#
# Usage: scripts/gen-ac3dep-fixtures.sh [--force]
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "$0")/.." && pwd)
OUT="$ROOT/fixtures/audio"
TMP=${TMPDIR:-$HOME/.cache}/ac3dep-gen
FORCE=0
[ "${1:-}" = "--force" ] && FORCE=1
command -v ffmpeg >/dev/null || { echo "gen-ac3dep: ffmpeg not found" >&2; exit 2; }
mkdir -p "$OUT" "$TMP"

# sine_inputs <freq...>: one lavfi sine input per frequency (3 s at 48 kHz), one argument per line.
sine_inputs() {
    local f
    for f in "$@"; do
        printf '%s\n' -f lavfi -i "sine=frequency=${f}:sample_rate=48000:duration=3"
    done
}

if [ "$FORCE" -eq 1 ] || [ ! -s "$OUT/ac3dep-src-5.1.eac3" ]; then
    # 5.1 order is FL FR FC LFE SL SR (ffmpeg labels AC-3 3/2 as side). The side pair (1100, 1500 Hz) is
    # distinct from the dependent stereo pair (880, 1320 Hz), so a placement error shows up in the comparison.
    mapfile -t A < <(sine_inputs 220 440 660 55 1100 1500)
    ffmpeg -nostdin -y -v error "${A[@]}" \
        -filter_complex "[0:a][1:a][2:a][3:a][4:a][5:a]amerge=inputs=6,channelmap=channel_layout=5.1" \
        -c:a eac3 -b:a 640k -f eac3 "$OUT/ac3dep-src-5.1.eac3"
fi
if [ "$FORCE" -eq 1 ] || [ ! -s "$OUT/ac3dep-src-2.0.eac3" ]; then
    mapfile -t B < <(sine_inputs 880 1320)
    ffmpeg -nostdin -y -v error "${B[@]}" \
        -filter_complex "[0:a][1:a]amerge=inputs=2,channelmap=channel_layout=stereo" \
        -c:a eac3 -b:a 192k -f eac3 "$OUT/ac3dep-src-2.0.eac3"
fi
CONVSNR="$ROOT/scripts/ac3dep-src-2.0.convsnr.txt"
python3 -I "$ROOT/scripts/splice-eac3-dependent.py" "$OUT/ac3dep-src-5.1.eac3" \
    "$OUT/ac3dep-src-2.0.eac3" 0200 "$OUT/ac3dep-7.1-48000.eac3" "$CONVSNR"
python3 -I "$ROOT/scripts/splice-eac3-dependent.py" "$OUT/ac3dep-src-5.1.eac3" \
    "$OUT/ac3dep-src-2.0.eac3" 0400 "$OUT/ac3dep-7.1wide-48000.eac3" "$CONVSNR"
python3 -I "$ROOT/scripts/splice-eac3-dependent.py" "$OUT/ac3dep-src-5.1.eac3" \
    "$OUT/ac3dep-src-2.0.eac3" 1800 "$OUT/ac3dep-repl-48000.eac3" "$CONVSNR"
rm -rf -- "$TMP"
echo "gen-ac3dep: wrote ac3dep-{src-5.1,src-2.0,7.1,7.1wide,repl} under $OUT"
