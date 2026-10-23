#!/usr/bin/env python3
"""Generate crates/ec-mpeg4/src/h263data.rs from the text of ITU-T Rec. H.263 (01/2005).

Usage:
    pdftotext -layout T-REC-H.263-200501-I!!PDF-E.pdf h263.txt
    python3 -I tools/gen_h263_tables.py h263.txt crates/ec-mpeg4/src/h263data.rs
    rustfmt --edition 2021 crates/ec-mpeg4/src/h263data.rs

The PDF is freely downloadable from https://www.itu.int/rec/T-REC-H.263 and is the
only input; every table below is read from the tables/figures of that document.
"""
import re
import sys

src = open(sys.argv[1], encoding="utf-8").read().replace("−", "-").replace("–", "-")
lines = src.split("\n")


def section(start, end):
    """Lines from the first line containing `start` up to the next line containing `end`."""
    i = next(k for k, l in enumerate(lines) if start in l)
    j = next(k for k in range(i + 1, len(lines)) if end in lines[k])
    return lines[i:j]


def bits(code):
    return code.replace(" ", "")


# ---- Tables 7 and 8: MCBPC ------------------------------------------------------
def mcbpc(sec, kinds):
    rows = {}
    for l in sec:
        m = re.match(r"^\s*(\d+)\s+(\S+)\s+(?:(-|\d\d)\s+)?(\d+)\s+((?:[01]+ ?)+)\s*$", l)
        if m:
            rows[int(m.group(1))] = (m.group(2), m.group(3), int(m.group(4)), bits(m.group(5)))
    return rows


t7 = mcbpc(section("Table 7/H.263", "Table 8/H.263"), None)
t8 = mcbpc(section("Table 8/H.263 – VLC table for MCBPC (for P-pictures)".replace("–", "-"), "An extra codeword"), None)
assert sorted(t7) == list(range(9)) and sorted(t8) == list(range(25)), (sorted(t7), sorted(t8))
for rows in (t7, t8):
    for k, (_, _, n, c) in rows.items():
        assert len(c) == n
intra_code, intra_bits = [], []
for i in range(9):
    intra_code.append(int(t7[i][3], 2))
    intra_bits.append(t7[i][2])
# Layout of the P-picture table: slot = 4 * group + CBPC, where the group is a flag
# combination of the MB type: bit 0 = INTRA, bit 1 = "+Q" (DQUANT present), bit 2 = four
# vectors (spec types 0 INTER, 1 INTER+Q, 2 INTER4V, 3 INTRA, 4 INTRA+Q, 5 INTER4V+Q give
# groups 0, 2, 4, 1, 3, 6). Group 5 holds the stuffing code in its first slot only, so
# slots 21-23 are unused.
GROUP = {0: 0, 1: 2, 2: 4, 3: 1, 4: 3, 5: 6}
inter_code, inter_bits = [0] * 28, [0] * 28
for i in range(25):
    if t8[i][0] == "Stuffing":
        slot = 4 * 5
    else:
        assert t8[i][1] == format(i % 4 if i < 20 else i - 21, "02b")
        slot = 4 * GROUP[int(t8[i][0])] + int(t8[i][1], 2)
    assert inter_bits[slot] == 0
    inter_code[slot], inter_bits[slot] = int(t8[i][3], 2), t8[i][2]
mcbpc_spec = {"I": [(t7[i][0], t7[i][1], t7[i][3]) for i in range(9)],
              "P": [(t8[i][0], t8[i][1], t8[i][3]) for i in range(25)]}

# ---- Table 12: CBPY ---------------------------------------------------------------
sec = section("Table 12/H.263", "5.3.6")
cbpy_rows = {}
cur = None
for l in sec:
    m = re.match(r"^\s*(\d+)\s+(\d\d)\s+(\d\d)\s+(\d+)\s+((?:[01]+ ?)+)\s*$", l)
    if m:
        cur = (int(m.group(1)), m.group(2), m.group(3), int(m.group(4)), bits(m.group(5)))
        continue
    m = re.match(r"^\s*(\d\d)\s+(\d\d)\s*$", l)
    if m and cur:
        idx, a12, b12, n, c = cur
        intra = int(a12 + m.group(1), 2)
        inter = int(b12 + m.group(2), 2)
        assert inter == 15 - intra and intra == idx and len(c) == n
        cbpy_rows[intra] = (c, n)
        cur = None
assert sorted(cbpy_rows) == list(range(16))
cbpy = [[int(cbpy_rows[i][0], 2), cbpy_rows[i][1]] for i in range(16)]
cbpy_spec = [cbpy_rows[i][0] for i in range(16)]

# ---- Table 14: MVD ---------------------------------------------------------------
sec = section("Table 14/H.263", "5.3.8")
mvd = {}
for l in sec:
    m = re.match(r"^\s*(\d+)\s+(-?[\d.]+)\s+(?:(-?[\d.]+)\s+)?(\d+)\s+((?:[01]+ ?)+)\s*$", l)
    if m:
        c = bits(m.group(5))
        assert len(c) == int(m.group(4))
        mvd[int(m.group(1))] = (float(m.group(2)), c)
assert sorted(mvd) == list(range(64))
by_vec = {v: c for v, c in mvd.values()}
mvtab = []  # index = |vector| in half samples; the code without its trailing sign bit
mvd_spec = []
for k in range(33):
    v = k / 2
    if k == 0:
        c = by_vec[0.0]
        assert c == "1"
        mvtab.append([1, 1])
        mvd_spec.append("1")
        continue
    if k == 32:  # +-16 share one codeword (-16 is listed; +16 == -16 modulo 32)
        c = by_vec[-16.0]
        assert c.endswith("1")
        c = c[:-1]
    else:
        cp, cn = by_vec[v], by_vec[-v]
        assert cp[:-1] == cn[:-1] and cp[-1] == "0" and cn[-1] == "1"
        c = cp[:-1]
    mvtab.append([int(c, 2), len(c)])
    mvd_spec.append(c)

# ---- Tables 16 and I.2: TCOEF ----------------------------------------------------
def tcoef(start, end):
    sec = section(start, end)
    rows = {}
    esc = None
    for l in sec:
        m = re.match(r"^\s*(\d+)\s+([01])\s+(\d+)\s+(\d+)\s+(\d+)\s+((?:[01]+ ?)+)s\s*$", l)
        if m:
            c = bits(m.group(6))
            assert len(c) + 1 == int(m.group(5))
            rows[int(m.group(1))] = (int(m.group(2)), int(m.group(3)), int(m.group(4)), c)
            continue
        m = re.match(r"^\s*(\d+)\s+ESCAPE\s+(\d+)\s+((?:[01]+ ?)+)\s*$", l)
        if m:
            esc = (int(m.group(1)), bits(m.group(3)))
            assert len(esc[1]) == int(m.group(2))
    assert sorted(rows) == list(range(102)) and esc[0] == 102
    return [rows[i] for i in range(102)], esc[1]


def rl_arrays(entries, esc):
    """Entries sorted by (LAST, RUN, |LEVEL|); vlc excludes the sign bit; the escape
    code is the final entry. Returns vlc, run, level, count of LAST = 0 entries."""
    ent = sorted(entries, key=lambda e: (e[0], e[1], e[2]))
    vlc = [[int(e[3], 2), len(e[3])] for e in ent] + [[int(esc, 2), len(esc)]]
    return ent, vlc, [e[1] for e in ent], [e[2] for e in ent], sum(1 for e in ent if e[0] == 0)


t16, t16esc = tcoef("Table 16/H.263", "Table 17/H.263")
t12i, t12iesc = tcoef("Table I.2/H.263", "Depending on the value of INTRA_MODE")
inter_ent, inter_vlc, inter_run, inter_level, inter_last = rl_arrays(t16, t16esc)
aic_ent, aic_vlc, aic_run, aic_level, aic_last = rl_arrays(t12i, t12iesc)
# Table 16 is printed in this order already
assert [e for e in t16] == inter_ent
assert {e[3] for e in t16} == {e[3] for e in t12i} and t16esc == t12iesc  # I.2 re-uses the codewords

# ---- Table O.4: CBPC for B/EP pictures ------------------------------------------
sec = section("Table O.4/H.263", "O.4.4")
cbpc_b = {}
for l in sec:
    m = re.match(r"^\s*(\d)\s+(\d\d)\s+([01]+)\s+(\d)\s*$", l)
    if m:
        assert len(m.group(3)) == int(m.group(4))
        cbpc_b[int(m.group(2), 2)] = (m.group(3), int(m.group(1)))
assert sorted(cbpc_b) == [0, 1, 2, 3]
cbpc_b_tab = [[int(cbpc_b[i][0], 2), len(cbpc_b[i][0])] for i in range(4)]

# ---- Tables T.1 and T.2: Modified Quantization ---------------------------------
sec = section("Table T.1/H.263", "T.2.2")
t1 = {}
for l in sec:
    m = re.match(r"^\s*(\d+)(?:-(\d+))?\s+([+-]?\d)\s+([+-]?\d)\s*$", l)
    if m:
        lo = int(m.group(1))
        hi = int(m.group(2) or lo)
        for q in range(lo, hi + 1):
            t1[q] = (int(m.group(3)), int(m.group(4)))
assert sorted(t1) == list(range(1, 32))
# Slot 0 (QUANT = 0 does not occur) is 0. Row 0: DQUANT = 10, row 1: DQUANT = 11.
modq = [[0] + [q + t1[q][r] for q in range(1, 32)] for r in range(2)]
sec = section("Table T.2/H.263", "T.4")
t2 = {}
for l in sec:
    m = re.match(r"^\s*(\d+)-(\d+)\s+(?:QUANT_C = QUANT(?: - (\d))?|(\d+))\s*$", l)
    if m:
        for q in range(int(m.group(1)), int(m.group(2)) + 1):
            if m.group(4):
                t2[q] = int(m.group(4))
            else:
                t2[q] = q - int(m.group(3) or 0)
assert sorted(t2) == list(range(1, 32))
chroma_q = [0] + [t2[q] for q in range(1, 32)]

# ---- Table K.2: MBA field width ------------------------------------------------
sec = section("Table K.2/H.263", "K.2.6")
mba_max, mba_len = [], []
for l in sec:
    m = re.match(r"^\s*(sub-QCIF|QCIF|CIF|4CIF|16CIF|2048 .* 1152)\s+(\d+)\s+(\d+)\s+\d+\s+\d+\s*$", l)
    if m:
        mba_max.append(int(m.group(2)))
        mba_len.append(int(m.group(3)))
assert len(mba_max) == 6
# Pictures larger than the last row also use 14 bits (final padding slot).
mba_len.append(mba_len[-1])

# ---- Figure 14 and Figure I.2: scans -------------------------------------------
def grid_rows(sec, n):
    rows = []
    for l in sec:
        nums = re.findall(r"\d+", l)
        if len(nums) == n and re.match(r"^[\s\d]+$", l):
            rows.append([int(x) for x in nums])
    return rows


start14 = next(k for k, l in enumerate(lines) if l.lstrip("\x0c").startswith("6.2.3") and "Zigzag" in l and "...." not in l)
rows = grid_rows(lines[start14:start14 + 20], 8)[:8]
assert len(rows) == 8


def scan_from_grid(g):
    """g[row][col] = 1-based scan position -> table of raster positions per scan index."""
    out = [None] * 64
    for r in range(8):
        for c in range(8):
            out[g[r][c] - 1] = r * 8 + c
    assert None not in out
    return out


zigzag = scan_from_grid(rows)
startI2 = next(k for k, l in enumerate(lines) if l.lstrip("\x0c").rstrip() == "I.3     Decoding process")
rows2 = grid_rows(lines[startI2:startI2 + 20], 16)[:8]
assert len(rows2) == 8
alt_h = scan_from_grid([r[:8] for r in rows2])
alt_v = scan_from_grid([r[8:] for r in rows2])

# ---- Table 2 + PTYPE source format codes, Table 5 + Table 3: formats and PAR ----
fmt = {}
for l in section("Table 2/H.263", "For all picture formats, colour"):
    m = re.match(r"^\s*(sub-QCIF|QCIF|CIF|4CIF|16CIF)\s+(\d+)\s+(\d+)\s+\d+\s+\d+\s*$", l)
    if m:
        fmt[m.group(1)] = (int(m.group(2)), int(m.group(3)))
# PTYPE bits 6-8 (5.1.3): 001 sub-QCIF, 010 QCIF, 011 CIF, 100 4CIF, 101 16CIF.
flat = re.sub(r"\s+", " ", " ".join(lines))
assert '"001" sub-QCIF, "010" QCIF, "011" CIF, "100" 4CIF, "101" 16CIF' in flat
names = ["sub-QCIF", "QCIF", "CIF", "4CIF", "16CIF"]
h263_format = [[0, 0]] + [list(fmt[n]) for n in names] + [[0, 0], [0, 0]]
par = {}
for l in section("Table 5/H.263", "5.1.6"):
    m = re.match(r"^\s*([01]{4})\s+(\d+):(\d+)\b", l)
    if m:
        par[int(m.group(1), 2)] = (int(m.group(2)), int(m.group(3)))
assert sorted(par) == [1, 2, 3, 4, 5] and par[1] == (1, 1) and par[5] == (40, 33)
# Codes 0 (forbidden), 6-14 (reserved) and 15 (extended PAR, carried in EPAR) are (0, 1).
h263_par = [[0, 1]] + [list(par[i]) for i in range(1, 6)] + [[0, 1]] * 10

# ---- Annex I.3: INTRA dequantisation RecC = 2 * QUANT * LEVEL (also for DC) -----
assert "RecC(u,v) = 2 * QUANT * LEVEL(u,v)" in src
aic_dc_scale = [2 * q for q in range(32)]

# ---------------------------------------------------------------------------------
out = []
w = out.append


def arr2(name, ty, a, doc):
    w(f"/// {doc}")
    w(f"pub const {name}: [[{ty}; 2]; {len(a)}] = [")
    for x in a:
        w(f"    [{x[0]}, {x[1]}],")
    w("];\n")


def arr1(name, ty, a, doc):
    w(f"/// {doc}")
    w(f"pub const {name}: [{ty}; {len(a)}] = [" + ", ".join(str(x) for x in a) + "];\n")


w("""// Generated by tools/gen_h263_tables.py from the text of ITU-T Rec. H.263 (01/2005)
// (https://www.itu.int/rec/T-REC-H.263). Do not edit by hand.
//
// Every table in this file is transcribed from the Recommendation; the clause or table
// number is given in each doc comment. A `[code, bits]` pair holds a variable length
// code as its integer value and its length in bits (the sign bit of TCOEF / MVD
// codes is not included).
""")
arr1("H263_INTRA_MCBPC_CODE", "u8", intra_code, "MCBPC for I pictures, code value by spec index (Table 7/H.263; index 8 is stuffing).")
arr1("H263_INTRA_MCBPC_BITS", "u8", intra_bits, "MCBPC for I pictures, code length (Table 7/H.263).")
arr1("H263_INTER_MCBPC_CODE", "u8", inter_code, "MCBPC for P pictures, code value. Slot = 4 * group + CBPC with group = INTRA + 2 * Q + 4 * four-vectors flags of the MB type (types 0, 3, 1, 4, 2 -> groups 0..4, type 5 -> group 6), slot 20 = stuffing, slots 21-23 unused (Table 8/H.263).")
arr1("H263_INTER_MCBPC_BITS", "u8", inter_bits, "MCBPC for P pictures, code length (Table 8/H.263); 0 marks an unused slot.")
arr2("CBPC_B_TAB", "u8", cbpc_b_tab, "CBPC for B- and EP-pictures, indexed by the coded block pattern (Table O.4/H.263).")
arr2("H263_CBPY_TAB", "u8", cbpy, "CBPY, indexed by the INTRA pattern value (12,34) read as four bits (Table 12/H.263); the INTER code is that of the complemented pattern.")
arr2("MVTAB", "u8", mvtab, "MVD, indexed by the absolute vector difference in half samples 0..32 (Table 14/H.263); the sign bit follows the code, +-16 share one code.")
arr2("INTER_VLC", "u16", inter_vlc, "TCOEF VLC without the sign bit, 102 (LAST, RUN, |LEVEL|) codes sorted by LAST, RUN, |LEVEL| followed by ESCAPE (Table 16/H.263).")
arr1("INTER_LEVEL", "i8", inter_level, "|LEVEL| of the entries of INTER_VLC (Table 16/H.263).")
arr1("INTER_RUN", "i8", inter_run, "RUN of the entries of INTER_VLC (Table 16/H.263).")
arr2("INTRA_VLC_AIC", "u16", aic_vlc, "INTRA TCOEF VLC for Advanced INTRA Coding, entries sorted by LAST, RUN, |LEVEL|, then ESCAPE (Table I.2/H.263).")
arr1("INTRA_RUN_AIC", "i8", aic_run, "RUN of the entries of INTRA_VLC_AIC (Table I.2/H.263).")
arr1("INTRA_LEVEL_AIC", "i8", aic_level, "|LEVEL| of the entries of INTRA_VLC_AIC (Table I.2/H.263).")
arr1("AIC_DC_SCALE_TABLE", "u8", aic_dc_scale, "Advanced INTRA Coding reconstruction scale 2 * QUANT (Annex I.3: RecC = 2 * QUANT * LEVEL, applied to INTRADC too).")
w("/// Modified Quantization: new QUANT for DQUANT = 10 (row 0) and 11 (row 1) by prior QUANT (Table T.1/H.263); slot 0 is unused.")
w(f"pub const MODIFIED_QUANT_TAB: [[u8; 32]; 2] = [")
for r in modq:
    w("    [" + ", ".join(map(str, r)) + "],")
w("];\n")
arr1("H263_CHROMA_QSCALE_TABLE", "u8", chroma_q, "QUANT_C for chrominance in Modified Quantization mode (Table T.2/H.263); slot 0 is unused.")
arr1("MBA_MAX", "u16", mba_max, "Largest macroblock address per standard picture format, sub-QCIF to 2048x1152 (Table K.2/H.263).")
arr1("MBA_LENGTH", "u8", mba_len, "MBA field width in bits for the same formats (Table K.2/H.263); the last slot repeats the 14 bits of the largest format.")
arr1("ZIGZAG_DIRECT", "u8", zigzag, "Zigzag scan: raster position (row * 8 + column) of each scan index (Figure 14/H.263).")
arr1("ALTERNATE_HORIZONTAL_SCAN", "u8", alt_h, "Alternate-Horizontal scan (Figure I.2-a/H.263).")
arr1("ALTERNATE_VERTICAL_SCAN", "u8", alt_v, "Alternate-Vertical scan (Figure I.2-b/H.263, as in H.262).")
w("/// Source format dimensions by PTYPE source-format code 0-7 (width, height; Table 2/H.263 and 5.1.3); 0, 6 and 7 are not standard formats.")
w(f"pub const H263_FORMAT: [[u16; 2]; 8] = [")
for x in h263_format:
    w(f"    [{x[0]}, {x[1]}],")
w("];\n")
arr2("H263_PIXEL_ASPECT", "u8", h263_par, "Pixel aspect ratio (width, height) by PAR code (Table 5/H.263); forbidden, reserved and extended codes give (0, 1).")
w("// Number of entries (without ESCAPE) and number of LAST = 0 entries of the TCOEF tables.")
w(f"pub const H263_RL_INTER_N: usize = {len(inter_ent)};")
w(f"pub const H263_RL_INTER_LAST: usize = {inter_last};")
w(f"pub const RL_INTRA_AIC_N: usize = {len(aic_ent)};")
w(f"pub const RL_INTRA_AIC_LAST: usize = {aic_last};")

# --- test module with the spec's own text
w("""
#[cfg(test)]
mod spec_tests {
    //! The rows below are the printed codes of the Recommendation; the tests rebuild
    //! the packed tables from them and compare.
    use super::*;

    fn val(c: &str) -> (u32, u32) {
        (u32::from_str_radix(c, 2).unwrap(), c.len() as u32)
    }
""")
w("    /// (MB type, CBPC, code) of Table 7/H.263, indices 0-8.")
w("    const T7: [(&str, &str, &str); 9] = [")
for r in mcbpc_spec["I"]:
    w(f'        ("{r[0]}", "{r[1]}", "{r[2]}"),')
w("    ];")
w("    /// (MB type, CBPC, code) of Table 8/H.263, indices 0-24.")
w("    const T8: [(&str, &str, &str); 25] = [")
for r in mcbpc_spec["P"]:
    w(f'        ("{r[0]}", "{r[1]}", "{r[2]}"),')
w("    ];")
w("    /// Table 12/H.263 codes in index order (index = INTRA pattern).")
w("    const T12: [&str; 16] = [" + ", ".join(f'"{c}"' for c in cbpy_spec) + "];")
w("    /// Table 14/H.263 codes for positive vectors 0, 0.5, ... 15.5 without sign bit, and -16 last.")
w("    const T14: [&str; 33] = [" + ", ".join(f'"{c}"' for c in mvd_spec) + "];")
w("    /// Table 16/H.263 rows: (LAST, RUN, |LEVEL|, code without sign) by index; ESCAPE code below.")
w(f"    const T16: [(u8, i8, i8, &str); 102] = [")
for e in t16:
    w(f'        ({e[0]}, {e[1]}, {e[2]}, "{e[3]}"),')
w("    ];")
w(f'    const T16_ESC: &str = "{t16esc}";')
w("    /// Table I.2/H.263 rows in index order.")
w(f"    const TI2: [(u8, i8, i8, &str); 102] = [")
for e in t12i:
    w(f'        ({e[0]}, {e[1]}, {e[2]}, "{e[3]}"),')
w("    ];")
w("""
    #[test]
    fn mcbpc_matches_tables_7_and_8() {
        for (i, r) in T7.iter().enumerate() {
            let (c, n) = val(r.2);
            assert_eq!((H263_INTRA_MCBPC_CODE[i] as u32, H263_INTRA_MCBPC_BITS[i] as u32), (c, n));
        }
        for r in T8.iter() {
            // group = flags of the MB type (INTRA 1, +Q 2, 4V 4); stuffing is group 5.
            let group = match r.0 {
                "Stuffing" => 5,
                t => [0, 2, 4, 1, 3, 6][t.parse::<usize>().unwrap()],
            };
            let slot = 4 * group + if r.0 == "Stuffing" { 0 } else { usize::from_str_radix(r.1, 2).unwrap() };
            let (c, n) = val(r.2);
            assert_eq!(
                (H263_INTER_MCBPC_CODE[slot] as u32, H263_INTER_MCBPC_BITS[slot] as u32),
                (c, n)
            );
        }
        for slot in 21..24 {
            assert_eq!(H263_INTER_MCBPC_BITS[slot], 0);
        }
    }

    #[test]
    fn cbpy_and_mvd_match_tables_12_and_14() {
        for (i, c) in T12.iter().enumerate() {
            let (v, n) = val(c);
            assert_eq!((H263_CBPY_TAB[i][0] as u32, H263_CBPY_TAB[i][1] as u32), (v, n));
        }
        for (i, c) in T14.iter().enumerate() {
            let (v, n) = val(c);
            assert_eq!((MVTAB[i][0] as u32, MVTAB[i][1] as u32), (v, n));
        }
    }

    fn check_rl(rows: &[(u8, i8, i8, &str); 102], vlc: &[[u16; 2]], run: &[i8], level: &[i8], last: usize) {
        let mut sorted: Vec<_> = rows.to_vec();
        sorted.sort_by_key(|r| (r.0, r.1, r.2));
        assert_eq!(sorted.iter().filter(|r| r.0 == 0).count(), last);
        for (i, r) in sorted.iter().enumerate() {
            let (v, n) = val(r.3);
            assert_eq!((vlc[i][0] as u32, vlc[i][1] as u32), (v, n), "entry {i}");
            assert_eq!((run[i], level[i]), (r.1, r.2), "entry {i}");
        }
        let (v, n) = val(T16_ESC);
        assert_eq!((vlc[102][0] as u32, vlc[102][1] as u32), (v, n));
    }

    #[test]
    fn tcoef_matches_tables_16_and_i2() {
        check_rl(&T16, &INTER_VLC, &INTER_RUN, &INTER_LEVEL, H263_RL_INTER_LAST);
        check_rl(&TI2, &INTRA_VLC_AIC, &INTRA_RUN_AIC, &INTRA_LEVEL_AIC, RL_INTRA_AIC_LAST);
        // Table I.2 re-uses the codewords of Table 16 (I.3).
        let mut a: Vec<_> = T16.iter().map(|r| r.3).collect();
        let mut b: Vec<_> = TI2.iter().map(|r| r.3).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }

    #[test]
    fn small_tables_match_the_text() {
        // Table O.4: 0, 10, 111, 110 for CBPC pattern 00, 01, 10, 11.
        assert_eq!(CBPC_B_TAB, [[0, 1], [2, 2], [7, 3], [6, 3]]);
        // Annex I.3: RecC = 2 * QUANT * LEVEL.
        for q in 0..32 {
            assert_eq!(AIC_DC_SCALE_TABLE[q] as usize, 2 * q);
        }
        // Table T.2.
        for q in 1..32usize {
            let c = match q {
                1..=6 => q,
                7..=9 => q - 1,
                10..=11 => 9,
                12..=13 => 10,
                14..=15 => 11,
                16..=18 => 12,
                19..=21 => 13,
                22..=26 => 14,
                _ => 15,
            };
            assert_eq!(H263_CHROMA_QSCALE_TABLE[q] as usize, c, "QUANT {q}");
        }
        // Table T.1: change of QUANT for DQUANT = 10 / 11.
        for q in 1..32i32 {
            let (d10, d11) = match q {
                1 => (2, 1),
                2..=10 => (-1, 1),
                11..=20 => (-2, 2),
                21..=28 => (-3, 3),
                29 => (-3, 2),
                30 => (-3, 1),
                _ => (-3, -5),
            };
            assert_eq!(MODIFIED_QUANT_TAB[0][q as usize] as i32, q + d10);
            assert_eq!(MODIFIED_QUANT_TAB[1][q as usize] as i32, q + d11);
        }
        // Table K.2.
        assert_eq!(MBA_MAX, [47, 98, 395, 1583, 6335, 9215]);
        assert_eq!(MBA_LENGTH, [6, 7, 9, 11, 13, 14, 14]);
        // Table 2 (formats) and Table 5 (PAR).
        assert_eq!(H263_FORMAT[1], [128, 96]);
        assert_eq!(H263_FORMAT[5], [1408, 1152]);
        assert_eq!(&H263_PIXEL_ASPECT[1..6], &[[1, 1], [12, 11], [10, 11], [16, 11], [40, 33]]);
    }

    #[test]
    fn scans_match_figures() {
        // Figure 14: first rows of the printed grid (scan positions, 1-based).
        let fig14: [[u8; 8]; 8] = [
            [1, 2, 6, 7, 15, 16, 28, 29],
            [3, 5, 8, 14, 17, 27, 30, 43],
            [4, 9, 13, 18, 26, 31, 42, 44],
            [10, 12, 19, 25, 32, 41, 45, 54],
            [11, 20, 24, 33, 40, 46, 53, 55],
            [21, 23, 34, 39, 47, 52, 56, 61],
            [22, 35, 38, 48, 51, 57, 60, 62],
            [36, 37, 49, 50, 58, 59, 63, 64],
        ];
        for (i, &p) in ZIGZAG_DIRECT.iter().enumerate() {
            assert_eq!(fig14[p as usize / 8][p as usize % 8] as usize, i + 1);
        }
        // Figure I.2-a and I.2-b.
        let a: [[u8; 8]; 8] = [
            [1, 2, 3, 4, 11, 12, 13, 14],
            [5, 6, 9, 10, 18, 17, 16, 15],
            [7, 8, 20, 19, 27, 28, 29, 30],
            [21, 22, 25, 26, 31, 32, 33, 34],
            [23, 24, 35, 36, 43, 44, 45, 46],
            [37, 38, 41, 42, 47, 48, 49, 50],
            [39, 40, 51, 52, 57, 58, 59, 60],
            [53, 54, 55, 56, 61, 62, 63, 64],
        ];
        let b: [[u8; 8]; 8] = [
            [1, 5, 7, 21, 23, 37, 39, 53],
            [2, 6, 8, 22, 24, 38, 40, 54],
            [3, 9, 20, 25, 35, 41, 51, 55],
            [4, 10, 19, 26, 36, 42, 52, 56],
            [11, 18, 27, 31, 43, 47, 57, 61],
            [12, 17, 28, 32, 44, 48, 58, 62],
            [13, 16, 29, 33, 45, 49, 59, 63],
            [14, 15, 30, 34, 46, 50, 60, 64],
        ];
        for (i, &p) in ALTERNATE_HORIZONTAL_SCAN.iter().enumerate() {
            assert_eq!(a[p as usize / 8][p as usize % 8] as usize, i + 1);
        }
        for (i, &p) in ALTERNATE_VERTICAL_SCAN.iter().enumerate() {
            assert_eq!(b[p as usize / 8][p as usize % 8] as usize, i + 1);
        }
    }
}""")
open(sys.argv[2], "w").write("\n".join(out) + "\n")
