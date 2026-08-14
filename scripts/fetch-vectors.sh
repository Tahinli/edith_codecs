#!/usr/bin/env bash
# fetch-vectors.sh — download third-party conformance vectors into fixtures/vectors/.
#
# Vectors are NOT redistributed from this repo (fixtures/ is gitignored); only this
# script and scripts/vectors.sha256 are committed. Idempotent, checksum-verified,
# and graceful: a dead URL or offline network reports the set as FAILED and the
# script continues with the remaining sets.
#
# Usage: scripts/fetch-vectors.sh [set ...]   (default: all sets)
#        sets: h264-jvt hevc-jctvc opus-rfc6716 flac-xiph vorbis-xiph
# Env:   EC_FIXTURES=<dir>  (default <repo>/fixtures)

set -uo pipefail

ROOT=$(cd -- "$(dirname -- "$0")/.." && pwd)
VEC=${EC_FIXTURES:-$ROOT/fixtures}/vectors
SUMS=$ROOT/scripts/vectors.sha256
CURL=(curl -fsSL --retry 2 --retry-delay 2 --connect-timeout 20 --max-time 900)

command -v curl >/dev/null || { echo "fetch-vectors: curl not found" >&2; exit 2; }
mkdir -p -- "$VEC"
touch -- "$SUMS"

ok_sets=()
fail_sets=()
n_new=0
n_have=0
n_bad=0

# verify_or_record <abs-file>  — checksum against scripts/vectors.sha256, or record it.
verify_or_record() {
    local f=$1 rel=${1#"$VEC"/} want have
    have=$(sha256sum -- "$f" | cut -d' ' -f1)
    want=$(awk -v k="$rel" '$2 == k { print $1 }' "$SUMS")
    if [ -z "$want" ]; then
        printf '%s  %s\n' "$have" "$rel" >>"$SUMS"
        return 0
    fi
    [ "$want" = "$have" ] && return 0
    echo "CHECKSUM MISMATCH $rel (want $want got $have)" >&2
    rm -f -- "$f"
    n_bad=$((n_bad + 1))
    return 1
}

# fetch_list <destdir> <baseurl> <name...>  — parallel download of missing names.
fetch_list() {
    local dest=$1 base=$2
    shift 2
    local name missing=() args=() rc=0
    mkdir -p -- "$dest"
    for name in "$@"; do
        if [ -s "$dest/$name" ]; then
            verify_or_record "$dest/$name" && { n_have=$((n_have + 1)); continue; }
        fi
        missing+=("$name")
    done
    if [ "${#missing[@]}" -gt 0 ]; then
        for name in "${missing[@]}"; do args+=(-O "$base/$name"); done
        "${CURL[@]}" --parallel --parallel-max 4 --output-dir "$dest" "${args[@]}" || rc=1
        for name in "${missing[@]}"; do
            if [ -s "$dest/$name" ] && verify_or_record "$dest/$name"; then
                n_new=$((n_new + 1))
            else
                rm -f -- "$dest/$name"
                rc=1
            fi
        done
    fi
    return "$rc"
}

# fetch_archive <destdir> <url> <archive-name> — download + extract once.
fetch_archive() {
    local dest=$1 url=$2 arc=$3
    mkdir -p -- "$dest"
    if [ -s "$dest/$arc" ]; then
        verify_or_record "$dest/$arc" || return 1
        n_have=$((n_have + 1))
    else
        "${CURL[@]}" -o "$dest/$arc" "$url" || { rm -f -- "$dest/$arc"; return 1; }
        verify_or_record "$dest/$arc" || return 1
        n_new=$((n_new + 1))
    fi
    [ -e "$dest/.extracted" ] && return 0
    case $arc in
        *.tar.gz | *.tgz) tar -xzf "$dest/$arc" -C "$dest" || return 1 ;;
        *.zip) unzip -qo "$dest/$arc" -d "$dest" || return 1 ;;
        *) return 0 ;;
    esac
    touch -- "$dest/.extracted"
}

# extract_zips <dir> — unpack each <name>.zip into <name>/ once (bitstream + ref yuv).
extract_zips() {
    local dir=$1 z base
    command -v unzip >/dev/null || {
        echo "fetch-vectors: unzip not found — zips left packed in $dir" >&2
        return 0
    }
    for z in "$dir"/*.zip; do
        [ -e "$z" ] || continue
        base=${z%.zip}
        [ -d "$base" ] && continue
        mkdir -p -- "$base"
        unzip -qo "$z" -d "$base" || {
            echo "fetch-vectors: unzip failed: $z" >&2
            rm -rf -- "$base"
        }
    done
}

run_set() {
    local name=$1
    shift
    if "$@"; then ok_sets+=("$name"); else fail_sets+=("$name"); fi
}

# ------------------------------------------------------------- h264-jvt -----
# JVT/ITU-T H.264.1 conformance bitstreams (the set openh264 & ffmpeg are checked
# against). 35-stream feature-covering subset: CAVLC/CABAC, MBAFF/PAFF, I_PCM,
# weighted pred, long-term refs, multiple slice groups, redundant slices.
JVT_BASE=https://www.itu.int/wftp3/av-arch/jvt-site/draft_conformance/AVCv1
JVT_STREAMS=(
    AUD_MW_E.zip BA1_Sony_D.zip BA3_SVA_C.zip BA_MW_D.zip BANM_MW_D.zip
    BASQP1_Sony_C.zip CABA1_SVA_B.zip CABA2_SVA_B.zip CABA3_SVA_B.zip
    CACQP3_Sony_D.zip CAMACI3_Sony_C.zip CAMASL3_Sony_B.zip CANL1_SVA_B.zip
    CANL2_SVA_B.zip CANL3_SVA_B.zip CANL4_SVA_B.zip CI_MW_D.zip CVFI1_SVA_C.zip
    CVFI2_SVA_C.zip CVMAQP2_Sony_G.zip CVMAQP3_Sony_D.zip CVPCMNL2_SVA_C.zip
    FI1_Sony_E.zip FM2_SVA_C.zip MIDR_MW_D.zip MR1_BT_A.zip MR9_BT_B.zip
    NL1_Sony_D.zip NL3_SVA_E.zip NRF_MW_E.zip SL1_SVA_B.zip SVA_BA2_D.zip
    SVA_CL1_E.zip SVA_FM1_E.zip SVA_NL2_E.zip
)

# ----------------------------------------------------------- hevc-jctvc -----
# JCT-VC HEVC_v1 conformance suite. NOTE: "class A/B" names the CTC *test
# sequences* (Traffic/Kimono/...), not conformance bitstreams — the redistributable
# conformance set is feature-named, so this is a feature-covering subset incl. the
# Main10 streams needed for the 10-bit/P010 HW path.
HEVC_BASE=https://www.itu.int/wftp3/av-arch/jctvc-site/bitstream_exchange/draft_conformance/HEVC_v1
HEVC_STREAMS=(
    AMP_A_Samsung_7.zip AMVP_A_MTK_4.zip BUMPING_A_ericsson_1.zip
    CAINIT_A_SHARP_4.zip CIP_A_Panasonic_3.zip CONFWIN_A_Sony_1.zip
    DBLK_A_SONY_3.zip DBLK_A_MAIN10_VIXS_4.zip DELTAQP_A_BRCM_4.zip
    DSLICE_A_HHI_5.zip ENTP_A_QUALCOMM_1.zip FILLER_A_Sony_1.zip
    INITQP_A_Sony_1.zip INITQP_B_Main10_Sony_1.zip ipcm_A_NEC_3.zip
    IPRED_A_docomo_2.zip MAXBINS_A_TI_5.zip MERGE_A_TI_3.zip
    PICSIZE_A_Bossen_1.zip POC_A_Bossen_3.zip RAP_A_docomo_6.zip
    RPS_A_docomo_5.zip RQT_A_HHI_4.zip SAO_A_MediaTek_4.zip SDH_A_Orange_4.zip
    SLICES_A_Rovi_3.zip TILES_A_Cisco_2.zip TSKIP_A_MS_3.zip
    TSUNEQBD_A_MAIN10_Technicolor_2.zip WPP_A_ericsson_MAIN_2.zip
    WPP_A_ericsson_MAIN10_2.zip WP_A_MAIN10_Toshiba_3.zip
)

# ---------------------------------------------------------- vorbis-xiph -----
# Xiph's own Vorbis I decoder test vectors (chained streams, LSP edge cases,
# 48k mono, sample-rate oddities).
VORBIS_BASE=https://people.xiph.org/~xiphmont/test-vectors/vorbis
VORBIS_FILES=(
    1.0-test.ogg 1.0.1-test.ogg 48k-mono.ogg beta3-test.ogg beta4-test.ogg
    bimS-silence.ogg chain-test1.ogg chain-test2.ogg chain-test3.ogg
    highrate-test.ogg lsp-test.ogg lsp-test2.ogg lsp-test3.ogg lsp-test4.ogg
    moog.ogg one-entry-codebook-test.ogg out-of-spec-blocksize.ogg
    rc1-test.ogg rc2-test.ogg rc3-test.ogg singlemap-test.ogg sleepzor.ogg
    test-short.ogg test-short2.ogg unused-mode-test.ogg
)

SETS=("$@")
[ "${#SETS[@]}" -eq 0 ] && SETS=(h264-jvt hevc-jctvc opus-rfc6716 flac-xiph vorbis-xiph)

for s in "${SETS[@]}"; do
    case $s in
        h264-jvt)
            run_set "$s" fetch_list "$VEC/h264-jvt" "$JVT_BASE" "${JVT_STREAMS[@]}"
            extract_zips "$VEC/h264-jvt"
            ;;
        hevc-jctvc)
            run_set "$s" fetch_list "$VEC/hevc-jctvc" "$HEVC_BASE" "${HEVC_STREAMS[@]}"
            extract_zips "$VEC/hevc-jctvc"
            ;;
        opus-rfc6716)
            # RFC 6716 / 8251 decoder test vectors (opus_compare reference set).
            run_set "$s" fetch_archive "$VEC/opus-rfc6716" \
                https://opus-codec.org/testvectors/opus_testvectors.tar.gz \
                opus_testvectors.tar.gz
            ;;
        flac-xiph)
            # IETF cellar WG FLAC test files (supersedes the old svn.xiph.org set;
            # carries the RFC 9639 subset + uncommon-but-legal stream shapes).
            run_set "$s" fetch_archive "$VEC/flac-xiph" \
                https://codeload.github.com/ietf-wg-cellar/flac-test-files/tar.gz/refs/heads/main \
                flac-test-files.tar.gz
            ;;
        vorbis-xiph)
            run_set "$s" fetch_list "$VEC/vorbis-xiph" "$VORBIS_BASE" "${VORBIS_FILES[@]}"
            ;;
        *)
            echo "unknown set: $s" >&2
            fail_sets+=("$s")
            ;;
    esac
done

printf '\n%s\n' "== vectors: $VEC =="
for s in "${ok_sets[@]:-}"; do
    [ -n "$s" ] || continue
    printf 'OK      %-14s files=%s bytes=%s\n' "$s" \
        "$(find "$VEC/$s" -type f ! -name .extracted 2>/dev/null | wc -l)" \
        "$(du -sb "$VEC/$s" 2>/dev/null | cut -f1)"
done
for s in "${fail_sets[@]:-}"; do
    [ -n "$s" ] && printf 'FAILED  %-14s (network or dead URL — rerun to resume)\n' "$s"
done
printf 'downloaded=%s already-present=%s checksum-mismatch=%s sets-ok=%s sets-failed=%s\n' \
    "$n_new" "$n_have" "$n_bad" "${#ok_sets[@]}" "${#fail_sets[@]}"
[ "${#fail_sets[@]}" -eq 0 ] && [ "$n_bad" -eq 0 ]
