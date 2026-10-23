#!/usr/bin/env bash
# fetch-vectors.sh — download third-party conformance vectors into fixtures/vectors/.
#
# Vectors are NOT redistributed from this repo (fixtures/ is gitignored); only this
# script and scripts/vectors.sha256 are committed. Idempotent, checksum-verified,
# and graceful: a dead URL or offline network reports the set as FAILED and the
# script continues with the remaining sets.
#
# Usage: scripts/fetch-vectors.sh [set ...]   (default: all sets)
#        sets: h264-jvt hevc-jctvc hevc-rext opus-rfc6716 opus-rfc8251 flac-xiph vorbis-xiph vp9-libvpx
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
    FM1_BT_B.zip FM1_FT_E.zip BA1_FT_C.zip
)

# Interlaced streams: field pictures (PAFF), MBAFF, field reference marking.
# A directory of their own, so the walk-everything progressive tests of
# ec-h264 (first IDR == first output frame) do not pick up streams whose
# first IDR is not the first picture in display order (CVPA1_TOSHIBA_B).
JVT_INTERLACED=(
    CABREF3_Sand_D.zip CAFI1_SVA_C.zip CAMA1_Sony_C.zip CAMA1_TOSHIBA_B.zip
    CAMA3_Sand_E.zip CAMANL1_TOSHIBA_B.zip CAMANL2_TOSHIBA_B.zip
    CAMANL3_Sand_E.zip CAMP_MOT_MBAFF_L30.zip CAMP_MOT_MBAFF_L31.zip
    CAPAMA3_Sand_F.zip CAPCM1_Sand_E.zip CVFI1_Sony_D.zip CVMA1_Sony_D.zip
    CVMA1_TOSHIBA_B.zip CVMANL1_TOSHIBA_B.zip CVMANL2_TOSHIBA_B.zip
    CVMAPAQP3_Sony_E.zip CVNLFI1_Sony_C.zip CVNLFI2_Sony_H.zip
    CVPA1_TOSHIBA_B.zip CVSE3_Sony_H.zip CVSEFDFT3_Sony_E.zip MR6_BT_B.zip
    MR7_BT_B.zip MR8_BT_B.zip Sharp_MP_Field_1_B.zip Sharp_MP_Field_3_B.zip
    Sharp_MP_PAFF_1r2.zip cama1_vtc_c.zip cama2_vtc_b.zip cama3_vtc_b.zip
)

# High profile field pictures (8x8 transform with the field scans), in the
# FRExt directory next to AVCv1.
JVT_FREXT_BASE=https://www.itu.int/wftp3/av-arch/jvt-site/draft_conformance/FRExt
JVT_FREXT_INTERLACED=(HCAFF1_HHI_B.zip)

# High 4:2:2 and High 4:2:2 Intra (Sony): frame, field and MBAFF, CAVLC and
# CABAC, 8 and 10 bits. A directory of their own: the output is 4:2:2 and the
# 4:2:0 walk-everything tests must not pick the streams up. (Hi422FR5 is not
# published.)
JVT_FREXT_HI422=(
    Hi422FR1_SONY_A.zip Hi422FR2_SONY_A.zip Hi422FR3_SONY_A.zip
    Hi422FR4_SONY_A.zip Hi422FR6_SONY_A.zip Hi422FR7_SONY_A.zip
    Hi422FR8_SONY_A.zip Hi422FR9_SONY_A.zip Hi422FR10_SONY_A.zip
    Hi422FR11_SONY_A.zip Hi422FR12_SONY_A.zip Hi422FR13_SONY_A.zip
    Hi422FR14_SONY_A.zip Hi422FR15_SONY_A.zip Hi422FREXT16_SONY_A.zip
    Hi422FREXT17_SONY_A.zip Hi422FREXT18_SONY_A.zip Hi422FREXT19_SONY_A.zip
)

# Professional profiles, High 4:4:4 Predictive / High 4:4:4 Intra / CAVLC 4:4:4
# Intra (Mitsubishi, SejongUniv, and the 2008 JVT .bits streams). A directory of
# their own: the output is 4:4:4 and the 4:2:0 walk-everything tests must not
# pick the streams up.
JVT_PRO_BASE=https://www.itu.int/wftp3/av-arch/jvt-site/draft_conformance/Professional_profiles
JVT_PRO_HI444=(
    PPCV444I1_2008.bits PPCV444I2_2008.bits PPCV444I3_2008.bits
    PPCV444I4_Mitsubishi_A.zip PPCV444I5_Mitsubishi_A.zip
    PPCV444I6_Mitsubishi_A.zip PPCV444I7_SejongUniv_A.zip
    PPH444I1_2008.bits PPH444I2_2008.bits PPH444I3_2008.bits
    PPH444I4_Mitsubishi_A.zip PPH444I5_Mitsubishi_A.zip
    PPH444I6_Mitsubishi_A.zip PPH444I7_SejongUniv_A.zip
    PPH444P1_2008.bits PPH444P2_2008.bits PPH444P3_2008.bits
    PPH444P4_2008.bits PPH444P5_2008.bits PPH444P6_Mitsubishi_A.zip
    PPH444P7_Mitsubishi_A.zip PPH444P8_Mitsubishi_A.zip
    PPH444P9_Mitsubishi_A.zip PPH444P10_SejongUniv_A.zip
)

# ----------------------------------------------------------- hevc-jctvc -----
# JCT-VC HEVC_v1 conformance suite: the full flat listing of HEVC_v1/ (147 streams,
# Main / Main10 / Main Still Picture). RExt, SCC, MV-HEVC and SHVC live in other dirs
# and are deliberately not fetched. Names are sorted so diffs stay reviewable.
HEVC_BASE=https://www.itu.int/wftp3/av-arch/jctvc-site/bitstream_exchange/draft_conformance/HEVC_v1
HEVC_STREAMS=(
    AMP_A_Samsung_7.zip AMP_B_Samsung_7.zip AMP_D_Hisilicon_3.zip
    AMP_E_Hisilicon_3.zip AMP_F_Hisilicon_3.zip AMVP_A_MTK_4.zip
    AMVP_B_MTK_4.zip AMVP_C_Samsung_7.zip BUMPING_A_ericsson_1.zip
    CAINIT_A_SHARP_4.zip CAINIT_B_SHARP_4.zip CAINIT_C_SHARP_3.zip
    CAINIT_D_SHARP_3.zip CAINIT_E_SHARP_3.zip CAINIT_F_SHARP_3.zip
    CAINIT_G_SHARP_3.zip CAINIT_H_SHARP_3.zip CIP_A_Panasonic_3.zip
    cip_B_NEC_3.zip CIP_C_Panasonic_2.zip CONFWIN_A_Sony_1.zip
    DBLK_A_MAIN10_VIXS_4.zip DBLK_A_SONY_3.zip DBLK_B_SONY_3.zip
    DBLK_C_SONY_3.zip DBLK_D_VIXS_2.zip DBLK_E_VIXS_2.zip DBLK_F_VIXS_2.zip
    DBLK_G_VIXS_2.zip DELTAQP_A_BRCM_4.zip DELTAQP_B_SONY_3.zip
    DELTAQP_C_SONY_3.zip DSLICE_A_HHI_5.zip DSLICE_B_HHI_5.zip
    DSLICE_C_HHI_5.zip ENTP_A_QUALCOMM_1.zip ENTP_B_Qualcomm_1.zip
    ENTP_C_Qualcomm_1.zip EXT_A_ericsson_4.zip FILLER_A_Sony_1.zip
    HRD_A_Fujitsu_3.zip INITQP_A_Sony_1.zip INITQP_B_Main10_Sony_1.zip
    ipcm_A_NEC_3.zip ipcm_B_NEC_3.zip ipcm_C_NEC_3.zip ipcm_D_NEC_3.zip
    ipcm_E_NEC_2.zip IPRED_A_docomo_2.zip IPRED_B_Nokia_3.zip
    IPRED_C_Mitsubishi_3.zip LS_A_Orange_2.zip LS_B_Orange_4.zip
    LTRPSPS_A_Qualcomm_1.zip MAXBINS_A_TI_5.zip MAXBINS_B_TI_5.zip
    MAXBINS_C_TI_5.zip MERGE_A_TI_3.zip MERGE_B_TI_3.zip MERGE_C_TI_3.zip
    MERGE_D_TI_3.zip MERGE_E_TI_3.zip MERGE_F_MTK_4.zip MERGE_G_HHI_4.zip
    MVCLIP_A_qualcomm_3.zip MVDL1ZERO_A_docomo_4.zip MVEDGE_A_qualcomm_3.zip
    NoOutPrior_A_Qualcomm_1.zip NoOutPrior_B_Qualcomm_1.zip
    NUT_A_ericsson_5.zip OPFLAG_A_Qualcomm_1.zip OPFLAG_B_Qualcomm_1.zip
    OPFLAG_C_Qualcomm_1.zip PICSIZE_A_Bossen_1.zip PICSIZE_B_Bossen_1.zip
    PICSIZE_C_Bossen_1.zip PICSIZE_D_Bossen_1.zip PMERGE_A_TI_3.zip
    PMERGE_B_TI_3.zip PMERGE_C_TI_3.zip PMERGE_D_TI_3.zip PMERGE_E_TI_3.zip
    POC_A_Bossen_3.zip PPS_A_qualcomm_7.zip PS_B_VIDYO_3.zip
    RAP_A_docomo_6.zip RAP_B_Bossen_2.zip RPLM_A_qualcomm_4.zip
    RPLM_B_qualcomm_4.zip RPS_A_docomo_5.zip RPS_B_qualcomm_5.zip
    RPS_C_ericsson_5.zip RPS_D_ericsson_6.zip RPS_E_qualcomm_5.zip
    RPS_F_docomo_2.zip RQT_A_HHI_4.zip RQT_B_HHI_4.zip RQT_C_HHI_4.zip
    RQT_D_HHI_4.zip RQT_E_HHI_4.zip RQT_F_HHI_4.zip RQT_G_HHI_4.zip
    SAO_A_MediaTek_4.zip SAO_B_MediaTek_5.zip SAO_C_Samsung_5.zip
    SAODBLK_A_MainConcept_4.zip SAODBLK_B_MainConcept_4.zip
    SAO_D_Samsung_5.zip SAO_E_Canon_4.zip SAO_F_Canon_3.zip SAO_G_Canon_3.zip
    SAO_H_Parabola_1.zip SDH_A_Orange_4.zip SLICES_A_Rovi_3.zip
    SLIST_A_Sony_5.zip SLIST_B_Sony_9.zip SLIST_C_Sony_4.zip
    SLIST_D_Sony_9.zip SLPPLP_A_VIDYO_2.zip STRUCT_A_Samsung_7.zip
    STRUCT_B_Samsung_7.zip TILES_A_Cisco_2.zip TILES_B_Cisco_1.zip
    TMVP_A_MS_3.zip TSCL_A_VIDYO_5.zip TSCL_B_VIDYO_4.zip TSKIP_A_MS_3.zip
    TSUNEQBD_A_MAIN10_Technicolor_2.zip TUSIZE_A_Samsung_1.zip
    VPSID_A_VIDYO_2.zip VPSSPSPPS_A_MainConcept_1.zip
    WP_A_MAIN10_Toshiba_3.zip WP_A_Toshiba_3.zip WP_B_Toshiba_3.zip
    WP_MAIN10_B_Toshiba_3.zip WPP_A_ericsson_MAIN10_2.zip
    WPP_A_ericsson_MAIN_2.zip WPP_B_ericsson_MAIN10_2.zip
    WPP_B_ericsson_MAIN_2.zip WPP_C_ericsson_MAIN10_2.zip
    WPP_C_ericsson_MAIN_2.zip WPP_D_ericsson_MAIN10_2.zip
    WPP_D_ericsson_MAIN_2.zip WPP_E_ericsson_MAIN10_2.zip
    WPP_E_ericsson_MAIN_2.zip WPP_F_ericsson_MAIN10_2.zip
    WPP_F_ericsson_MAIN_2.zip
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

# ---------------------------------------------------------- vp9-libvpx -----
# libvpx VP9 decoder test vectors (test/test_vectors.cc, profile 0 and profile 2 only;
# profile 1/3 are frozen in DONE.md). Each vector has a per-frame .md5 of the decoded
# planes (libvpx test/md5_helper.h). The list is the webmproject.org test_data set.
VP9_LIBVPX_BASE=https://storage.googleapis.com/downloads.webmproject.org/test_data/libvpx
VP9_LIBVPX_FILES=(
    vp90-2-00-quantizer-00.webm     vp90-2-00-quantizer-00.webm.md5     vp90-2-00-quantizer-01.webm
    vp90-2-00-quantizer-01.webm.md5     vp90-2-00-quantizer-02.webm     vp90-2-00-quantizer-02.webm.md5
    vp90-2-00-quantizer-03.webm     vp90-2-00-quantizer-03.webm.md5     vp90-2-00-quantizer-04.webm
    vp90-2-00-quantizer-04.webm.md5     vp90-2-00-quantizer-05.webm     vp90-2-00-quantizer-05.webm.md5
    vp90-2-00-quantizer-06.webm     vp90-2-00-quantizer-06.webm.md5     vp90-2-00-quantizer-07.webm
    vp90-2-00-quantizer-07.webm.md5     vp90-2-00-quantizer-08.webm     vp90-2-00-quantizer-08.webm.md5
    vp90-2-00-quantizer-09.webm     vp90-2-00-quantizer-09.webm.md5     vp90-2-00-quantizer-10.webm
    vp90-2-00-quantizer-10.webm.md5     vp90-2-00-quantizer-11.webm     vp90-2-00-quantizer-11.webm.md5
    vp90-2-00-quantizer-12.webm     vp90-2-00-quantizer-12.webm.md5     vp90-2-00-quantizer-13.webm
    vp90-2-00-quantizer-13.webm.md5     vp90-2-00-quantizer-14.webm     vp90-2-00-quantizer-14.webm.md5
    vp90-2-00-quantizer-15.webm     vp90-2-00-quantizer-15.webm.md5     vp90-2-00-quantizer-16.webm
    vp90-2-00-quantizer-16.webm.md5     vp90-2-00-quantizer-17.webm     vp90-2-00-quantizer-17.webm.md5
    vp90-2-00-quantizer-18.webm     vp90-2-00-quantizer-18.webm.md5     vp90-2-00-quantizer-19.webm
    vp90-2-00-quantizer-19.webm.md5     vp90-2-00-quantizer-20.webm     vp90-2-00-quantizer-20.webm.md5
    vp90-2-00-quantizer-21.webm     vp90-2-00-quantizer-21.webm.md5     vp90-2-00-quantizer-22.webm
    vp90-2-00-quantizer-22.webm.md5     vp90-2-00-quantizer-23.webm     vp90-2-00-quantizer-23.webm.md5
    vp90-2-00-quantizer-24.webm     vp90-2-00-quantizer-24.webm.md5     vp90-2-00-quantizer-25.webm
    vp90-2-00-quantizer-25.webm.md5     vp90-2-00-quantizer-26.webm     vp90-2-00-quantizer-26.webm.md5
    vp90-2-00-quantizer-27.webm     vp90-2-00-quantizer-27.webm.md5     vp90-2-00-quantizer-28.webm
    vp90-2-00-quantizer-28.webm.md5     vp90-2-00-quantizer-29.webm     vp90-2-00-quantizer-29.webm.md5
    vp90-2-00-quantizer-30.webm     vp90-2-00-quantizer-30.webm.md5     vp90-2-00-quantizer-31.webm
    vp90-2-00-quantizer-31.webm.md5     vp90-2-00-quantizer-32.webm     vp90-2-00-quantizer-32.webm.md5
    vp90-2-00-quantizer-33.webm     vp90-2-00-quantizer-33.webm.md5     vp90-2-00-quantizer-34.webm
    vp90-2-00-quantizer-34.webm.md5     vp90-2-00-quantizer-35.webm     vp90-2-00-quantizer-35.webm.md5
    vp90-2-00-quantizer-36.webm     vp90-2-00-quantizer-36.webm.md5     vp90-2-00-quantizer-37.webm
    vp90-2-00-quantizer-37.webm.md5     vp90-2-00-quantizer-38.webm     vp90-2-00-quantizer-38.webm.md5
    vp90-2-00-quantizer-39.webm     vp90-2-00-quantizer-39.webm.md5     vp90-2-00-quantizer-40.webm
    vp90-2-00-quantizer-40.webm.md5     vp90-2-00-quantizer-41.webm     vp90-2-00-quantizer-41.webm.md5
    vp90-2-00-quantizer-42.webm     vp90-2-00-quantizer-42.webm.md5     vp90-2-00-quantizer-43.webm
    vp90-2-00-quantizer-43.webm.md5     vp90-2-00-quantizer-44.webm     vp90-2-00-quantizer-44.webm.md5
    vp90-2-00-quantizer-45.webm     vp90-2-00-quantizer-45.webm.md5     vp90-2-00-quantizer-46.webm
    vp90-2-00-quantizer-46.webm.md5     vp90-2-00-quantizer-47.webm     vp90-2-00-quantizer-47.webm.md5
    vp90-2-00-quantizer-48.webm     vp90-2-00-quantizer-48.webm.md5     vp90-2-00-quantizer-49.webm
    vp90-2-00-quantizer-49.webm.md5     vp90-2-00-quantizer-50.webm     vp90-2-00-quantizer-50.webm.md5
    vp90-2-00-quantizer-51.webm     vp90-2-00-quantizer-51.webm.md5     vp90-2-00-quantizer-52.webm
    vp90-2-00-quantizer-52.webm.md5     vp90-2-00-quantizer-53.webm     vp90-2-00-quantizer-53.webm.md5
    vp90-2-00-quantizer-54.webm     vp90-2-00-quantizer-54.webm.md5     vp90-2-00-quantizer-55.webm
    vp90-2-00-quantizer-55.webm.md5     vp90-2-00-quantizer-56.webm     vp90-2-00-quantizer-56.webm.md5
    vp90-2-00-quantizer-57.webm     vp90-2-00-quantizer-57.webm.md5     vp90-2-00-quantizer-58.webm
    vp90-2-00-quantizer-58.webm.md5     vp90-2-00-quantizer-59.webm     vp90-2-00-quantizer-59.webm.md5
    vp90-2-00-quantizer-60.webm     vp90-2-00-quantizer-60.webm.md5     vp90-2-00-quantizer-61.webm
    vp90-2-00-quantizer-61.webm.md5     vp90-2-00-quantizer-62.webm     vp90-2-00-quantizer-62.webm.md5
    vp90-2-00-quantizer-63.webm     vp90-2-00-quantizer-63.webm.md5     vp90-2-01-sharpness-1.webm
    vp90-2-01-sharpness-1.webm.md5     vp90-2-01-sharpness-2.webm     vp90-2-01-sharpness-2.webm.md5
    vp90-2-01-sharpness-3.webm     vp90-2-01-sharpness-3.webm.md5     vp90-2-01-sharpness-4.webm
    vp90-2-01-sharpness-4.webm.md5     vp90-2-01-sharpness-5.webm     vp90-2-01-sharpness-5.webm.md5
    vp90-2-01-sharpness-6.webm     vp90-2-01-sharpness-6.webm.md5     vp90-2-01-sharpness-7.webm
    vp90-2-01-sharpness-7.webm.md5     vp90-2-02-size-08x08.webm     vp90-2-02-size-08x08.webm.md5
    vp90-2-02-size-08x10.webm     vp90-2-02-size-08x10.webm.md5     vp90-2-02-size-08x16.webm
    vp90-2-02-size-08x16.webm.md5     vp90-2-02-size-08x18.webm     vp90-2-02-size-08x18.webm.md5
    vp90-2-02-size-08x32.webm     vp90-2-02-size-08x32.webm.md5     vp90-2-02-size-08x34.webm
    vp90-2-02-size-08x34.webm.md5     vp90-2-02-size-08x64.webm     vp90-2-02-size-08x64.webm.md5
    vp90-2-02-size-08x66.webm     vp90-2-02-size-08x66.webm.md5     vp90-2-02-size-10x08.webm
    vp90-2-02-size-10x08.webm.md5     vp90-2-02-size-10x10.webm     vp90-2-02-size-10x10.webm.md5
    vp90-2-02-size-10x16.webm     vp90-2-02-size-10x16.webm.md5     vp90-2-02-size-10x18.webm
    vp90-2-02-size-10x18.webm.md5     vp90-2-02-size-10x32.webm     vp90-2-02-size-10x32.webm.md5
    vp90-2-02-size-10x34.webm     vp90-2-02-size-10x34.webm.md5     vp90-2-02-size-10x64.webm
    vp90-2-02-size-10x64.webm.md5     vp90-2-02-size-10x66.webm     vp90-2-02-size-10x66.webm.md5
    vp90-2-02-size-130x132.webm     vp90-2-02-size-130x132.webm.md5     vp90-2-02-size-132x130.webm
    vp90-2-02-size-132x130.webm.md5     vp90-2-02-size-132x132.webm     vp90-2-02-size-132x132.webm.md5
    vp90-2-02-size-16x08.webm     vp90-2-02-size-16x08.webm.md5     vp90-2-02-size-16x10.webm
    vp90-2-02-size-16x10.webm.md5     vp90-2-02-size-16x16.webm     vp90-2-02-size-16x16.webm.md5
    vp90-2-02-size-16x18.webm     vp90-2-02-size-16x18.webm.md5     vp90-2-02-size-16x32.webm
    vp90-2-02-size-16x32.webm.md5     vp90-2-02-size-16x34.webm     vp90-2-02-size-16x34.webm.md5
    vp90-2-02-size-16x64.webm     vp90-2-02-size-16x64.webm.md5     vp90-2-02-size-16x66.webm
    vp90-2-02-size-16x66.webm.md5     vp90-2-02-size-178x180.webm     vp90-2-02-size-178x180.webm.md5
    vp90-2-02-size-180x178.webm     vp90-2-02-size-180x178.webm.md5     vp90-2-02-size-180x180.webm
    vp90-2-02-size-180x180.webm.md5     vp90-2-02-size-18x08.webm     vp90-2-02-size-18x08.webm.md5
    vp90-2-02-size-18x10.webm     vp90-2-02-size-18x10.webm.md5     vp90-2-02-size-18x16.webm
    vp90-2-02-size-18x16.webm.md5     vp90-2-02-size-18x18.webm     vp90-2-02-size-18x18.webm.md5
    vp90-2-02-size-18x32.webm     vp90-2-02-size-18x32.webm.md5     vp90-2-02-size-18x34.webm
    vp90-2-02-size-18x34.webm.md5     vp90-2-02-size-18x64.webm     vp90-2-02-size-18x64.webm.md5
    vp90-2-02-size-18x66.webm     vp90-2-02-size-18x66.webm.md5     vp90-2-02-size-32x08.webm
    vp90-2-02-size-32x08.webm.md5     vp90-2-02-size-32x10.webm     vp90-2-02-size-32x10.webm.md5
    vp90-2-02-size-32x16.webm     vp90-2-02-size-32x16.webm.md5     vp90-2-02-size-32x18.webm
    vp90-2-02-size-32x18.webm.md5     vp90-2-02-size-32x32.webm     vp90-2-02-size-32x32.webm.md5
    vp90-2-02-size-32x34.webm     vp90-2-02-size-32x34.webm.md5     vp90-2-02-size-32x64.webm
    vp90-2-02-size-32x64.webm.md5     vp90-2-02-size-32x66.webm     vp90-2-02-size-32x66.webm.md5
    vp90-2-02-size-34x08.webm     vp90-2-02-size-34x08.webm.md5     vp90-2-02-size-34x10.webm
    vp90-2-02-size-34x10.webm.md5     vp90-2-02-size-34x16.webm     vp90-2-02-size-34x16.webm.md5
    vp90-2-02-size-34x18.webm     vp90-2-02-size-34x18.webm.md5     vp90-2-02-size-34x32.webm
    vp90-2-02-size-34x32.webm.md5     vp90-2-02-size-34x34.webm     vp90-2-02-size-34x34.webm.md5
    vp90-2-02-size-34x64.webm     vp90-2-02-size-34x64.webm.md5     vp90-2-02-size-34x66.webm
    vp90-2-02-size-34x66.webm.md5     vp90-2-02-size-64x08.webm     vp90-2-02-size-64x08.webm.md5
    vp90-2-02-size-64x10.webm     vp90-2-02-size-64x10.webm.md5     vp90-2-02-size-64x16.webm
    vp90-2-02-size-64x16.webm.md5     vp90-2-02-size-64x18.webm     vp90-2-02-size-64x18.webm.md5
    vp90-2-02-size-64x32.webm     vp90-2-02-size-64x32.webm.md5     vp90-2-02-size-64x34.webm
    vp90-2-02-size-64x34.webm.md5     vp90-2-02-size-64x64.webm     vp90-2-02-size-64x64.webm.md5
    vp90-2-02-size-64x66.webm     vp90-2-02-size-64x66.webm.md5     vp90-2-02-size-66x08.webm
    vp90-2-02-size-66x08.webm.md5     vp90-2-02-size-66x10.webm     vp90-2-02-size-66x10.webm.md5
    vp90-2-02-size-66x16.webm     vp90-2-02-size-66x16.webm.md5     vp90-2-02-size-66x18.webm
    vp90-2-02-size-66x18.webm.md5     vp90-2-02-size-66x32.webm     vp90-2-02-size-66x32.webm.md5
    vp90-2-02-size-66x34.webm     vp90-2-02-size-66x34.webm.md5     vp90-2-02-size-66x64.webm
    vp90-2-02-size-66x64.webm.md5     vp90-2-02-size-66x66.webm     vp90-2-02-size-66x66.webm.md5
    vp90-2-02-size-lf-1920x1080.webm     vp90-2-02-size-lf-1920x1080.webm.md5     vp90-2-03-deltaq.webm
    vp90-2-03-deltaq.webm.md5     vp90-2-03-size-196x196.webm     vp90-2-03-size-196x196.webm.md5
    vp90-2-03-size-196x198.webm     vp90-2-03-size-196x198.webm.md5     vp90-2-03-size-196x200.webm
    vp90-2-03-size-196x200.webm.md5     vp90-2-03-size-196x202.webm     vp90-2-03-size-196x202.webm.md5
    vp90-2-03-size-196x208.webm     vp90-2-03-size-196x208.webm.md5     vp90-2-03-size-196x210.webm
    vp90-2-03-size-196x210.webm.md5     vp90-2-03-size-196x224.webm     vp90-2-03-size-196x224.webm.md5
    vp90-2-03-size-196x226.webm     vp90-2-03-size-196x226.webm.md5     vp90-2-03-size-198x196.webm
    vp90-2-03-size-198x196.webm.md5     vp90-2-03-size-198x198.webm     vp90-2-03-size-198x198.webm.md5
    vp90-2-03-size-198x200.webm     vp90-2-03-size-198x200.webm.md5     vp90-2-03-size-198x202.webm
    vp90-2-03-size-198x202.webm.md5     vp90-2-03-size-198x208.webm     vp90-2-03-size-198x208.webm.md5
    vp90-2-03-size-198x210.webm     vp90-2-03-size-198x210.webm.md5     vp90-2-03-size-198x224.webm
    vp90-2-03-size-198x224.webm.md5     vp90-2-03-size-198x226.webm     vp90-2-03-size-198x226.webm.md5
    vp90-2-03-size-200x196.webm     vp90-2-03-size-200x196.webm.md5     vp90-2-03-size-200x198.webm
    vp90-2-03-size-200x198.webm.md5     vp90-2-03-size-200x200.webm     vp90-2-03-size-200x200.webm.md5
    vp90-2-03-size-200x202.webm     vp90-2-03-size-200x202.webm.md5     vp90-2-03-size-200x208.webm
    vp90-2-03-size-200x208.webm.md5     vp90-2-03-size-200x210.webm     vp90-2-03-size-200x210.webm.md5
    vp90-2-03-size-200x224.webm     vp90-2-03-size-200x224.webm.md5     vp90-2-03-size-200x226.webm
    vp90-2-03-size-200x226.webm.md5     vp90-2-03-size-202x196.webm     vp90-2-03-size-202x196.webm.md5
    vp90-2-03-size-202x198.webm     vp90-2-03-size-202x198.webm.md5     vp90-2-03-size-202x200.webm
    vp90-2-03-size-202x200.webm.md5     vp90-2-03-size-202x202.webm     vp90-2-03-size-202x202.webm.md5
    vp90-2-03-size-202x208.webm     vp90-2-03-size-202x208.webm.md5     vp90-2-03-size-202x210.webm
    vp90-2-03-size-202x210.webm.md5     vp90-2-03-size-202x224.webm     vp90-2-03-size-202x224.webm.md5
    vp90-2-03-size-202x226.webm     vp90-2-03-size-202x226.webm.md5     vp90-2-03-size-208x196.webm
    vp90-2-03-size-208x196.webm.md5     vp90-2-03-size-208x198.webm     vp90-2-03-size-208x198.webm.md5
    vp90-2-03-size-208x200.webm     vp90-2-03-size-208x200.webm.md5     vp90-2-03-size-208x202.webm
    vp90-2-03-size-208x202.webm.md5     vp90-2-03-size-208x208.webm     vp90-2-03-size-208x208.webm.md5
    vp90-2-03-size-208x210.webm     vp90-2-03-size-208x210.webm.md5     vp90-2-03-size-208x224.webm
    vp90-2-03-size-208x224.webm.md5     vp90-2-03-size-208x226.webm     vp90-2-03-size-208x226.webm.md5
    vp90-2-03-size-210x196.webm     vp90-2-03-size-210x196.webm.md5     vp90-2-03-size-210x198.webm
    vp90-2-03-size-210x198.webm.md5     vp90-2-03-size-210x200.webm     vp90-2-03-size-210x200.webm.md5
    vp90-2-03-size-210x202.webm     vp90-2-03-size-210x202.webm.md5     vp90-2-03-size-210x208.webm
    vp90-2-03-size-210x208.webm.md5     vp90-2-03-size-210x210.webm     vp90-2-03-size-210x210.webm.md5
    vp90-2-03-size-210x224.webm     vp90-2-03-size-210x224.webm.md5     vp90-2-03-size-210x226.webm
    vp90-2-03-size-210x226.webm.md5     vp90-2-03-size-224x196.webm     vp90-2-03-size-224x196.webm.md5
    vp90-2-03-size-224x198.webm     vp90-2-03-size-224x198.webm.md5     vp90-2-03-size-224x200.webm
    vp90-2-03-size-224x200.webm.md5     vp90-2-03-size-224x202.webm     vp90-2-03-size-224x202.webm.md5
    vp90-2-03-size-224x208.webm     vp90-2-03-size-224x208.webm.md5     vp90-2-03-size-224x210.webm
    vp90-2-03-size-224x210.webm.md5     vp90-2-03-size-224x224.webm     vp90-2-03-size-224x224.webm.md5
    vp90-2-03-size-224x226.webm     vp90-2-03-size-224x226.webm.md5     vp90-2-03-size-226x196.webm
    vp90-2-03-size-226x196.webm.md5     vp90-2-03-size-226x198.webm     vp90-2-03-size-226x198.webm.md5
    vp90-2-03-size-226x200.webm     vp90-2-03-size-226x200.webm.md5     vp90-2-03-size-226x202.webm
    vp90-2-03-size-226x202.webm.md5     vp90-2-03-size-226x208.webm     vp90-2-03-size-226x208.webm.md5
    vp90-2-03-size-226x210.webm     vp90-2-03-size-226x210.webm.md5     vp90-2-03-size-226x224.webm
    vp90-2-03-size-226x224.webm.md5     vp90-2-03-size-226x226.webm     vp90-2-03-size-226x226.webm.md5
    vp90-2-03-size-352x288.webm     vp90-2-03-size-352x288.webm.md5     vp90-2-05-resize.ivf
    vp90-2-05-resize.ivf.md5     vp90-2-06-bilinear.webm     vp90-2-06-bilinear.webm.md5
    vp90-2-07-frame_parallel.webm     vp90-2-07-frame_parallel.webm.md5     vp90-2-08-tile_1x2_frame_parallel.webm
    vp90-2-08-tile_1x2_frame_parallel.webm.md5     vp90-2-08-tile_1x2.webm     vp90-2-08-tile_1x2.webm.md5
    vp90-2-08-tile_1x4_frame_parallel.webm     vp90-2-08-tile_1x4_frame_parallel.webm.md5     vp90-2-08-tile_1x4.webm
    vp90-2-08-tile_1x4.webm.md5     vp90-2-08-tile_1x8_frame_parallel.webm     vp90-2-08-tile_1x8_frame_parallel.webm.md5
    vp90-2-08-tile_1x8.webm     vp90-2-08-tile_1x8.webm.md5     vp90-2-08-tile-4x1.webm
    vp90-2-08-tile-4x1.webm.md5     vp90-2-08-tile-4x4.webm     vp90-2-08-tile-4x4.webm.md5
    vp90-2-09-aq2.webm     vp90-2-09-aq2.webm.md5     vp90-2-09-lf_deltas.webm
    vp90-2-09-lf_deltas.webm.md5     vp90-2-09-subpixel-00.ivf     vp90-2-09-subpixel-00.ivf.md5
    vp90-2-10-show-existing-frame2.webm     vp90-2-10-show-existing-frame2.webm.md5     vp90-2-10-show-existing-frame.webm
    vp90-2-10-show-existing-frame.webm.md5     vp90-2-11-size-351x287.webm     vp90-2-11-size-351x287.webm.md5
    vp90-2-11-size-351x288.webm     vp90-2-11-size-351x288.webm.md5     vp90-2-11-size-352x287.webm
    vp90-2-11-size-352x287.webm.md5     vp90-2-12-droppable_1.ivf     vp90-2-12-droppable_1.ivf.md5
    vp90-2-12-droppable_2.ivf     vp90-2-12-droppable_2.ivf.md5     vp90-2-12-droppable_3.ivf
    vp90-2-12-droppable_3.ivf.md5     vp90-2-13-largescaling.webm     vp90-2-13-largescaling.webm.md5
    vp90-2-14-resize-10frames-fp-tiles-1-2-4-8.webm     vp90-2-14-resize-10frames-fp-tiles-1-2-4-8.webm.md5     vp90-2-14-resize-10frames-fp-tiles-1-2.webm
    vp90-2-14-resize-10frames-fp-tiles-1-2.webm.md5     vp90-2-14-resize-10frames-fp-tiles-1-4.webm     vp90-2-14-resize-10frames-fp-tiles-1-4.webm.md5
    vp90-2-14-resize-10frames-fp-tiles-1-8.webm     vp90-2-14-resize-10frames-fp-tiles-1-8.webm.md5     vp90-2-14-resize-10frames-fp-tiles-2-1.webm
    vp90-2-14-resize-10frames-fp-tiles-2-1.webm.md5     vp90-2-14-resize-10frames-fp-tiles-2-4.webm     vp90-2-14-resize-10frames-fp-tiles-2-4.webm.md5
    vp90-2-14-resize-10frames-fp-tiles-2-8.webm     vp90-2-14-resize-10frames-fp-tiles-2-8.webm.md5     vp90-2-14-resize-10frames-fp-tiles-4-1.webm
    vp90-2-14-resize-10frames-fp-tiles-4-1.webm.md5     vp90-2-14-resize-10frames-fp-tiles-4-2.webm     vp90-2-14-resize-10frames-fp-tiles-4-2.webm.md5
    vp90-2-14-resize-10frames-fp-tiles-4-8.webm     vp90-2-14-resize-10frames-fp-tiles-4-8.webm.md5     vp90-2-14-resize-10frames-fp-tiles-8-1.webm
    vp90-2-14-resize-10frames-fp-tiles-8-1.webm.md5     vp90-2-14-resize-10frames-fp-tiles-8-2.webm     vp90-2-14-resize-10frames-fp-tiles-8-2.webm.md5
    vp90-2-14-resize-10frames-fp-tiles-8-4-2-1.webm     vp90-2-14-resize-10frames-fp-tiles-8-4-2-1.webm.md5     vp90-2-14-resize-10frames-fp-tiles-8-4.webm
    vp90-2-14-resize-10frames-fp-tiles-8-4.webm.md5     vp90-2-14-resize-fp-tiles-1-16.webm     vp90-2-14-resize-fp-tiles-1-16.webm.md5
    vp90-2-14-resize-fp-tiles-1-2-4-8-16.webm     vp90-2-14-resize-fp-tiles-1-2-4-8-16.webm.md5     vp90-2-14-resize-fp-tiles-1-2.webm
    vp90-2-14-resize-fp-tiles-1-2.webm.md5     vp90-2-14-resize-fp-tiles-1-4.webm     vp90-2-14-resize-fp-tiles-1-4.webm.md5
    vp90-2-14-resize-fp-tiles-16-1.webm     vp90-2-14-resize-fp-tiles-16-1.webm.md5     vp90-2-14-resize-fp-tiles-16-2.webm
    vp90-2-14-resize-fp-tiles-16-2.webm.md5     vp90-2-14-resize-fp-tiles-16-4.webm     vp90-2-14-resize-fp-tiles-16-4.webm.md5
    vp90-2-14-resize-fp-tiles-16-8-4-2-1.webm     vp90-2-14-resize-fp-tiles-16-8-4-2-1.webm.md5     vp90-2-14-resize-fp-tiles-16-8.webm
    vp90-2-14-resize-fp-tiles-16-8.webm.md5     vp90-2-14-resize-fp-tiles-1-8.webm     vp90-2-14-resize-fp-tiles-1-8.webm.md5
    vp90-2-14-resize-fp-tiles-2-16.webm     vp90-2-14-resize-fp-tiles-2-16.webm.md5     vp90-2-14-resize-fp-tiles-2-1.webm
    vp90-2-14-resize-fp-tiles-2-1.webm.md5     vp90-2-14-resize-fp-tiles-2-4.webm     vp90-2-14-resize-fp-tiles-2-4.webm.md5
    vp90-2-14-resize-fp-tiles-2-8.webm     vp90-2-14-resize-fp-tiles-2-8.webm.md5     vp90-2-14-resize-fp-tiles-4-16.webm
    vp90-2-14-resize-fp-tiles-4-16.webm.md5     vp90-2-14-resize-fp-tiles-4-1.webm     vp90-2-14-resize-fp-tiles-4-1.webm.md5
    vp90-2-14-resize-fp-tiles-4-2.webm     vp90-2-14-resize-fp-tiles-4-2.webm.md5     vp90-2-14-resize-fp-tiles-4-8.webm
    vp90-2-14-resize-fp-tiles-4-8.webm.md5     vp90-2-14-resize-fp-tiles-8-16.webm     vp90-2-14-resize-fp-tiles-8-16.webm.md5
    vp90-2-14-resize-fp-tiles-8-1.webm     vp90-2-14-resize-fp-tiles-8-1.webm.md5     vp90-2-14-resize-fp-tiles-8-2.webm
    vp90-2-14-resize-fp-tiles-8-2.webm.md5     vp90-2-14-resize-fp-tiles-8-4.webm     vp90-2-14-resize-fp-tiles-8-4.webm.md5
    vp90-2-15-segkey_adpq.webm     vp90-2-15-segkey_adpq.webm.md5     vp90-2-15-segkey.webm
    vp90-2-15-segkey.webm.md5     vp90-2-16-intra-only.webm     vp90-2-16-intra-only.webm.md5
    vp90-2-17-show-existing-frame.webm     vp90-2-17-show-existing-frame.webm.md5     vp90-2-18-resize.ivf
    vp90-2-18-resize.ivf.md5     vp90-2-19-skip-01.webm     vp90-2-19-skip-01.webm.md5
    vp90-2-19-skip-02.webm     vp90-2-19-skip-02.webm.md5     vp90-2-19-skip.webm
    vp90-2-19-skip.webm.md5     vp90-2-20-big_superframe-01.webm     vp90-2-20-big_superframe-01.webm.md5
    vp90-2-20-big_superframe-02.webm     vp90-2-20-big_superframe-02.webm.md5     vp90-2-21-resize_inter_1280x720_5_1-2.webm
    vp90-2-21-resize_inter_1280x720_5_1-2.webm.md5     vp90-2-21-resize_inter_1280x720_5_3-4.webm     vp90-2-21-resize_inter_1280x720_5_3-4.webm.md5
    vp90-2-21-resize_inter_1280x720_7_1-2.webm     vp90-2-21-resize_inter_1280x720_7_1-2.webm.md5     vp90-2-21-resize_inter_1280x720_7_3-4.webm
    vp90-2-21-resize_inter_1280x720_7_3-4.webm.md5     vp90-2-21-resize_inter_1920x1080_5_1-2.webm     vp90-2-21-resize_inter_1920x1080_5_1-2.webm.md5
    vp90-2-21-resize_inter_1920x1080_5_3-4.webm     vp90-2-21-resize_inter_1920x1080_5_3-4.webm.md5     vp90-2-21-resize_inter_1920x1080_7_1-2.webm
    vp90-2-21-resize_inter_1920x1080_7_1-2.webm.md5     vp90-2-21-resize_inter_1920x1080_7_3-4.webm     vp90-2-21-resize_inter_1920x1080_7_3-4.webm.md5
    vp90-2-21-resize_inter_320x180_5_1-2.webm     vp90-2-21-resize_inter_320x180_5_1-2.webm.md5     vp90-2-21-resize_inter_320x180_5_3-4.webm
    vp90-2-21-resize_inter_320x180_5_3-4.webm.md5     vp90-2-21-resize_inter_320x180_7_1-2.webm     vp90-2-21-resize_inter_320x180_7_1-2.webm.md5
    vp90-2-21-resize_inter_320x180_7_3-4.webm     vp90-2-21-resize_inter_320x180_7_3-4.webm.md5     vp90-2-21-resize_inter_320x240_5_1-2.webm
    vp90-2-21-resize_inter_320x240_5_1-2.webm.md5     vp90-2-21-resize_inter_320x240_5_3-4.webm     vp90-2-21-resize_inter_320x240_5_3-4.webm.md5
    vp90-2-21-resize_inter_320x240_7_1-2.webm     vp90-2-21-resize_inter_320x240_7_1-2.webm.md5     vp90-2-21-resize_inter_320x240_7_3-4.webm
    vp90-2-21-resize_inter_320x240_7_3-4.webm.md5     vp90-2-21-resize_inter_640x360_5_1-2.webm     vp90-2-21-resize_inter_640x360_5_1-2.webm.md5
    vp90-2-21-resize_inter_640x360_5_3-4.webm     vp90-2-21-resize_inter_640x360_5_3-4.webm.md5     vp90-2-21-resize_inter_640x360_7_1-2.webm
    vp90-2-21-resize_inter_640x360_7_1-2.webm.md5     vp90-2-21-resize_inter_640x360_7_3-4.webm     vp90-2-21-resize_inter_640x360_7_3-4.webm.md5
    vp90-2-21-resize_inter_640x480_5_1-2.webm     vp90-2-21-resize_inter_640x480_5_1-2.webm.md5     vp90-2-21-resize_inter_640x480_5_3-4.webm
    vp90-2-21-resize_inter_640x480_5_3-4.webm.md5     vp90-2-21-resize_inter_640x480_7_1-2.webm     vp90-2-21-resize_inter_640x480_7_1-2.webm.md5
    vp90-2-21-resize_inter_640x480_7_3-4.webm     vp90-2-21-resize_inter_640x480_7_3-4.webm.md5     vp90-2-22-svc_1280x720_1.webm
    vp90-2-22-svc_1280x720_1.webm.md5     vp90-2-22-svc_1280x720_3.ivf     vp90-2-22-svc_1280x720_3.ivf.md5
    vp92-2-20-10bit-yuv420.webm     vp92-2-20-10bit-yuv420.webm.md5     vp92-2-20-12bit-yuv420.webm
    vp92-2-20-12bit-yuv420.webm.md5
)

# ------------------------------------------------------------ hevc-rext -----
# JCT-VC RExt conformance suite (draft_conformance/RExt/): 4:0:0, 4:2:2 and 4:4:4 at 8 to 16 bits
# with the range extension tools. The SEI picture hashes inside the streams are the oracle.
HEVC_REXT_BASE=https://www.itu.int/wftp3/av-arch/jctvc-site/bitstream_exchange/draft_conformance/RExt
HEVC_REXT_STREAMS=(
    ADJUST_IPRED_ANGLE_A_RExt_Mitsubishi_2.zip Bitdepth_A_RExt_Sony_1.zip
    Bitdepth_B_RExt_Sony_1.zip CCP_10bit_RExt_QCOM_1.zip
    CCP_12bit_RExt_QCOM_1.zip CCP_8bit_RExt_QCOM_1.zip
    ExplicitRdpcm_A_BBC_1.zip ExplicitRdpcm_B_BBC_2.zip
    EXTPREC_HIGHTHROUGHPUT_444_16_INTRA_10BIT_RExt_Sony_1.zip EXTPREC_HIGHTHROUGHPUT_444_16_INTRA_12BIT_RExt_Sony_1.zip
    EXTPREC_HIGHTHROUGHPUT_444_16_INTRA_16BIT_RExt_Sony_1.zip EXTPREC_HIGHTHROUGHPUT_444_16_INTRA_8BIT_RExt_Sony_1.zip
    EXTPREC_MAIN_444_16_INTRA_10BIT_RExt_Sony_1.zip EXTPREC_MAIN_444_16_INTRA_12BIT_RExt_Sony_1.zip
    EXTPREC_MAIN_444_16_INTRA_16BIT_RExt_Sony_1.zip EXTPREC_MAIN_444_16_INTRA_8BIT_RExt_Sony_1.zip
    GENERAL_10b_420_RExt_Sony_1.zip GENERAL_10b_422_RExt_Sony_1.zip
    GENERAL_10b_444_RExt_Sony_2.zip GENERAL_12b_400_RExt_Sony_1.zip
    GENERAL_12b_420_RExt_Sony_1.zip GENERAL_12b_422_RExt_Sony_1.zip
    GENERAL_12b_444_RExt_Sony_2.zip GENERAL_16b_400_RExt_Sony_1.zip
    GENERAL_16b_444_highThroughput_RExt_Sony_2.zip GENERAL_16b_444_RExt_Sony_2.zip
    GENERAL_8b_400_RExt_Sony_1.zip GENERAL_8b_420_RExt_Sony_1.zip
    GENERAL_8b_444_RExt_Sony_2.zip IPCM_A_RExt_NEC_2.zip
    IPCM_B_RExt_NEC.zip Main_422_10_A_RExt_Sony_2.zip
    Main_422_10_B_RExt_Sony_2.zip PERSIST_RPARAM_A_RExt_Sony_3.zip
    QMATRIX_A_RExt_Sony_1.zip SAO_A_RExt_MediaTek_1.zip
    TSCTX_10bit_I_RExt_SHARP_1.zip TSCTX_10bit_RExt_SHARP_1.zip
    TSCTX_12bit_I_RExt_SHARP_1.zip TSCTX_12bit_RExt_SHARP_1.zip
    TSCTX_8bit_I_RExt_SHARP_1.zip TSCTX_8bit_RExt_SHARP_1.zip
    WAVETILES_RExt_Sony_2.zip WPP_AND_TILE_10Bit422Test_HIGH_TP_444_10BIT_RExt_Apple_2.zip
    WPP_AND_TILE_AND_CABAC_BYPASS_ALIGN_0_HIGH_TP_444_14BIT_RExt_Apple_2.zip WPP_AND_TILE_AND_CABAC_BYPASS_ALIGN_1_HIGH_TP_444_14BIT_RExt_Apple_2.zip
    WPP_AND_TILE_AND_CABAC_EXT_PREC_1_HIGH_TP_444_14BIT_RExt_Apple_2.zip WPP_AND_TILE_HIGH_TP_444_8BIT_RExt_Apple_2.zip
    WPP_HIGH_TP_444_8BIT_RExt_Apple_2.zip
)

SETS=("$@")
[ "${#SETS[@]}" -eq 0 ] && SETS=(h264-jvt hevc-jctvc opus-rfc6716 opus-rfc8251 flac-xiph vorbis-xiph vp9-libvpx)

for s in "${SETS[@]}"; do
    case $s in
        h264-jvt)
            run_set "$s" fetch_list "$VEC/h264-jvt" "$JVT_BASE" "${JVT_STREAMS[@]}"
            extract_zips "$VEC/h264-jvt"
            run_set "$s-interlaced" fetch_list "$VEC/h264-jvt-interlaced" "$JVT_BASE" "${JVT_INTERLACED[@]}"
            run_set "$s-interlaced-frext" fetch_list "$VEC/h264-jvt-interlaced" "$JVT_FREXT_BASE" "${JVT_FREXT_INTERLACED[@]}"
            extract_zips "$VEC/h264-jvt-interlaced"
            run_set "$s-hi422" fetch_list "$VEC/h264-jvt-hi422" "$JVT_FREXT_BASE" "${JVT_FREXT_HI422[@]}"
            extract_zips "$VEC/h264-jvt-hi422"
            run_set "$s-hi444" fetch_list "$VEC/h264-jvt-hi444" "$JVT_PRO_BASE" "${JVT_PRO_HI444[@]}"
            extract_zips "$VEC/h264-jvt-hi444"
            ;;
        hevc-jctvc)
            run_set "$s" fetch_list "$VEC/hevc-jctvc" "$HEVC_BASE" "${HEVC_STREAMS[@]}"
            extract_zips "$VEC/hevc-jctvc"
            ;;
        hevc-rext)
            run_set "$s" fetch_list "$VEC/hevc-rext" "$HEVC_REXT_BASE" "${HEVC_REXT_STREAMS[@]}"
            extract_zips "$VEC/hevc-rext"
            ;;
        opus-rfc6716)
            # RFC 6716 / 8251 decoder test vectors (opus_compare reference set).
            run_set "$s" fetch_archive "$VEC/opus-rfc6716" \
                https://opus-codec.org/testvectors/opus_testvectors.tar.gz \
                opus_testvectors.tar.gz
            ;;
        opus-rfc8251)
            # RFC 8251 updated Opus vectors (opus_newvectors). The archive is the IETF
            # slides tarball; RFC 8251 Section 11 lists the SHA-1 of each .bit.
            run_set "$s" fetch_archive "$VEC/opus-rfc8251" \
                https://www.ietf.org/proceedings/98/slides/materials-98-codec-opus-newvectors-00.tar.gz \
                materials-98-codec-opus-newvectors-00.tar.gz
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
        vp9-libvpx)
            run_set "$s" fetch_list "$VEC/vp9-libvpx" "$VP9_LIBVPX_BASE" "${VP9_LIBVPX_FILES[@]}"
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
