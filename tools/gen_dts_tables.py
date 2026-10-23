#!/usr/bin/env python3
"""Generate crates/ec-dts/src/tables_data.rs from ETSI TS 102 114.

Source: ETSI TS 102 114 V1.6.1 (2019-08), "DTS Coherent Acoustics; Core and
Extensions with Additional Profiles", free from
https://www.etsi.org/deliver/etsi_ts/102100_102199/102114/01.06.01_60/ts_102114v010601p.pdf

Usage:
    pdftotext -layout ts_102114v010601p.pdf ts102114.txt
    python3 -I tools/gen_dts_tables.py ts102114.txt \
        crates/ec-dts/src/tables_data.rs crates/ec-dts/tests/spec_ts102114_tables.txt

The spec text (decimal commas, "1 926" style thousands separators) is parsed
here; nothing is read from any other decoder. Two files are written:

* tables_data.rs: the Rust statics, after the conversions listed below.
* spec_ts102114_tables.txt: the raw numbers exactly as the spec prints them
  (one line per table, with clause references). tests/spec_tables.rs applies
  the same conversions independently in Rust and asserts equality with the
  statics, so the committed numbers are checked against the committed text.

Clauses used (all normative):
  Table 5-26      Number of quantization levels per ABITS      -> QUANT_LEVELS
  Table 5-27      Scale factor adjustment index (ADJ)          -> SCALE_FACTOR_ADJ
  D.1.1, D.1.2    6-bit / 7-bit scale factor quantization      -> SCALE_FACTOR_QUANT6/7
  D.2.1, D.2.2    Lossy / lossless quantization step sizes     -> LOSSY_QUANT, LOSSLESS_QUANT
  D.5.x           Huffman code books (A3 .. G129, SA129..SE129,
                  A12..E12, A4..D4)                            -> HUFF_SRC
  D.8 (col. 5)    128x LFE interpolation FIR                   -> LFE_FIR_128
  D.9             1024 tap FIR for X96 synthesis QMF           -> FIR_64
  D.11            Downmix scale factor look-up (DmixTable)     -> DMIXTABLE

Conversions (implementation layout choices, not spec content):
  * invalid entries -> 0; tables are padded to their power-of-two length with 0.
  * QUANT_LEVELS: ABITS 0 -> 1 (spec: 0, never used as a divisor here);
    "33 or 32", "65 or 64", "129 or 128" -> the power of two.
  * HUFF_SRC: codes sorted by left-aligned code value (the spec codes are
    canonical), stored as (symbol, length) with symbol = level + offset where
    offset is 1,2,3,4,6,8,12,16,32,64 for the 3..129 level books, ABITS-1 for
    the 12 level books, level for the 4 level books, level+64 for SA..SE129.
    Books are laid out in the order consumed by huff.rs.
  * LFE_FIR_128: spec column (512 symmetric taps) read in the polyphase order
    k = 4q+p -> index [q, 128+q, 255-q, 127-q][p].
  * FIR_64: round-half-up(coef * 2^29), sign negated unless (index/32) % 4 == 0
    (QMF cosine modulation sign), index 0-based (spec "Coef #" is 1-based).
  * DMIXTABLE: a leading 0 then the 241 DmixTable entries.
"""
import re
import sys
from decimal import Decimal, ROUND_HALF_UP


def D(s):
    return Decimal(s.replace(',', '.'))


def main():
    spec_path, out_rs, out_spec = sys.argv[1:4]
    with open(spec_path, encoding='utf-8') as f:
        L = f.read().split('\n')
    assert any('V1.6.1 (2019-08)' in l for l in L[:3000]), 'spec is not V1.6.1'

    def find(pat, start=0):
        for i in range(start, len(L)):
            if re.match(pat, L[i]):
                return i
        raise SystemExit('not found: ' + pat)

    # ---- section boundaries (annex D, after the table of contents)
    a_d = find(r'^Annex D \(normative\):\s*$', 1000)
    s = {}
    for key, pat in [('d11', r'^D\.1\.1 '), ('d12', r'^D\.1\.2 '), ('d2', r'^D\.2 '),
                     ('d21', r'^D\.2\.1 '), ('d22', r'^D\.2\.2 '), ('d3', r'^D\.3 '),
                     ('d4', r'^D\.4 '), ('d5', r'^D\.5 '), ('d6', r'^D\.6 '),
                     ('d7', r'^D\.7 '), ('d8', r'^D\.8 '), ('d9', r'^D\.9 '),
                     ('d10', r'^D\.10 '), ('d11x', r'^D\.11 ')]:
        s[key] = find(pat, a_d)

    raw = []  # (name, clause, [strings])

    # ---- Table 5-26 (quantization levels)
    t526 = find(r'^\s*Table 5-26: Selection of Quantization Levels', 1000)
    ql = {}
    for l in L[t526:t526 + 40]:
        m = re.match(r'^\s+(\d+)\s+(\d[\d ]*?)(?:\s+or\s+(\d+))?\s{2,}', l)
        if m and int(m.group(1)) <= 26 and m.group(2).replace(' ', '').isdigit():
            ql[int(m.group(1))] = (m.group(3) or m.group(2)).replace(' ', '')
    assert sorted(ql) == list(range(27)), sorted(ql)
    raw.append(('QUANT_LEVELS', 'Table 5-26', [ql[i] for i in range(27)]))

    # ---- Table 5-27 (ADJ)
    t527 = find(r'^\s*Table 5-27: Scale Factor Adjustment Index', 1000)
    adj = []
    for l in L[t527:t527 + 12]:
        m = re.match(r'^\s+([0-3])\s+(\d,\d+)\s*$', l)
        if m:
            adj.append(m.group(2).replace(',', '.'))
    assert len(adj) == 4
    raw.append(('SCALE_FACTOR_ADJ', 'Table 5-27', adj))

    # ---- D.1.1 / D.1.2 scale factor quantization (index, level, dB triples)
    def triples(a, b, n):
        d = {}
        for l in L[a:b]:
            t = l.split()
            if len(t) % 3 or not t or not t[0].isdigit():
                continue
            for k in range(0, len(t), 3):
                if t[k].isdigit() and (t[k + 1].isdigit() or t[k + 1] == 'invalid'):
                    d[int(t[k])] = t[k + 1]
        assert sorted(d) == list(range(n)), (len(d), n)
        return [d[i] for i in range(n)]
    raw.append(('SCALE_FACTOR_QUANT6', 'D.1.1', triples(s['d11'], s['d12'], 64)))
    raw.append(('SCALE_FACTOR_QUANT7', 'D.1.2', triples(s['d12'], s['d2'], 128)))

    # ---- D.2.x step sizes (index, step x 2^22, nominal)
    def steps(a, b):
        d = {}
        for l in L[a:b]:
            m = re.match(r'^\s+(\d+)\s+(\d+|invalid)\s+(\S+)\s*$', l)
            if m:
                d[int(m.group(1))] = m.group(2)
        assert sorted(d) == list(range(32)), len(d)
        return [d[i] for i in range(32)]
    raw.append(('LOSSY_QUANT', 'D.2.1', steps(s['d21'], s['d22'])))
    raw.append(('LOSSLESS_QUANT', 'D.2.2', steps(s['d22'], s['d3'])))

    # ---- D.5 Huffman code books
    books = {}   # table name -> [(level, length, code)]
    names = []   # in document order
    n_cols = 0
    cur = []
    fresh = False
    for l in L[s['d5']:s['d6']]:
        titles = re.findall(r'Table ([A-Z]{1,2}\d+)\b', l)
        if titles:
            # titles on consecutive lines (A33 / B33 C33) form one block
            cur = cur + titles if fresh else titles
            fresh = True
            n_cols = len(cur)
            for t in titles:
                assert t not in books
                books[t] = []
                names.append(t)
            continue
        t = l.split()
        if not t or not all(re.match(r'^-?\d+$', x) for x in t):
            continue
        row = []
        k = 0
        while k < len(t):
            lv, ln, cd = t[k], t[k + 1], t[k + 2]
            k += 3
            while k < len(t) and re.match(r'^\d{3}$', t[k]):
                cd += t[k]
                k += 1
            row.append((int(lv), int(ln), int(cd)))
        # a ragged first row (table A33 starts one line before B33 and C33)
        # (and its tail: the first column has ended, the others have one row left)
        fresh = False
        if n_cols == 1:   # one table printed in several column segments
            books[cur[0]].extend(row)
            continue
        assert len(row) == n_cols or (len(row) < n_cols and cur[0] == 'A33'), (l, n_cols, cur)
        fresh = False
        cols = cur[:len(row)] if not books[cur[0]] else cur[n_cols - len(row):]
        for tname, e in zip(cols, row):
            books[tname].append(e)
    # The printed Table D65 (D.5.11) stops at level +-30: the rows for +-31 and
    # +-32 are missing from the standard's text. They are the only symbols
    # absent, so their codes are the unassigned part of the code space, which
    # must be exactly four 15-bit words; following the printed pairs (the
    # positive level has the code one above its negative, and a larger |level|
    # has the lower code) they are -32, 32, -31, 31 in ascending order.
    e = books['D65']
    assert sorted(lv for lv, _, _ in e) == [x for x in range(-30, 31)], 'D65 layout changed'
    used = set()
    for _, ln, cd in e:
        used.update(range(cd << (15 - ln), (cd + 1) << (15 - ln)))
    free = [c for c in range(1 << 15) if c not in used]
    assert free == [28848, 28849, 28850, 28851], free
    for lv, c in zip([-32, 32, -31, 31], free):
        e.append((lv, 15, c))
    # Table D129 (D.5.12) prints level -37 as "37" (sign lost), so +37 appears
    # twice; as in every other printed pair the negative level has the lower code.
    e = books['D129']
    dup = sorted((x for x in e if x[0] == 37), key=lambda x: x[2])
    assert len(dup) == 2 and dup[0][1] == dup[1][1] and dup[1][2] == dup[0][2] + 1, dup
    e[e.index(dup[0])] = (-37, dup[0][1], dup[0][2])
    for tname in names:
        e = books[tname]
        kraft = sum(Decimal(2) ** -ln for _, ln, _ in e)
        assert kraft == 1, (tname, kraft)
        assert len(set(lv for lv, _, _ in e)) == len(e), (tname, len(e), sorted(lv for lv, _, _ in e))

    def book(tname, nlev, offset):
        e = books[tname]
        assert len(e) == nlev, (tname, len(e), nlev)
        return [(lv + offset, ln, cd) for lv, ln, cd in e]

    order = []  # (table name, symbol offset) in huff.rs consumption order
    for nlev, groups, off in [(3, 'A', 1), (5, 'ABC', 2), (7, 'ABC', 3), (9, 'ABC', 4),
                              (13, 'ABC', 6), (17, 'ABCDEFG', 8), (25, 'ABCDEFG', 12),
                              (33, 'ABCDEFG', 16), (65, 'ABCDEFG', 32), (129, 'ABCDEFG', 64)]:
        for g in groups:
            order.append((g + str(nlev), nlev, off))
    for g in 'ABCDE':
        order.append((g + '12', 12, 0))   # level column is ABITS 1..12 -> symbol ABITS-1
    for g in 'ABCDE':
        order.append(('S' + g + '129', 129, 64))
    for g in 'ABCD':
        order.append((g + '4', 4, 0))
    assert sorted(n for n, _, _ in order) == sorted(names), set(names) ^ set(n for n, _, _ in order)
    huff = []
    huff_raw = []
    for tname, nlev, off in order:
        if nlev == 12:
            e = [(lv - 1, ln, cd) for lv, ln, cd in books[tname]]
            assert len(e) == 12
        else:
            e = book(tname, nlev, off)
        e.sort(key=lambda x: x[2] << (32 - x[1]))
        huff.extend((sym, ln) for sym, ln, _ in e)
        huff_raw.append((tname, [(lv, ln, cd) for lv, ln, cd in books[tname]]))
        if tname == 'D65':
            huff_raw[-1] = (tname + ' [rows +-31,+-32 completed, see generator]', huff_raw[-1][1])
    assert len(huff) == 2709, len(huff)

    # ---- D.8 128x LFE FIR (last column) and D.9 FIR_64
    lfe = []
    for l in L[s['d8']:s['d9']]:
        t = l.split()
        if len(t) == 5 and t[0].isdigit() and 'e' in t[1]:
            lfe.append(t[4].replace(',', '.'))
    assert len(lfe) == 512
    raw.append(('LFE_FIR_128', 'D.8 (128 x Interpolation column)', lfe))

    fir = {}
    for l in L[s['d9']:s['d10']]:
        for m in re.finditer(r'(?<![\d,])(\d{1,3}(?: \d{3})?)\s+(-?\d,\d+e[-+]?\d+)', l):
            fir[int(m.group(1).replace(' ', ''))] = m.group(2).replace(',', '.')
    assert sorted(fir) == list(range(1, 1025))
    raw.append(('FIR_64', 'D.9', [fir[i] for i in range(1, 1025)]))

    # ---- D.11 downmix look-up
    dm = {}
    for l in L[s['d11x']:]:
        m = re.match(r'^\s+(\d+)\s+(-?[\d,]+)\s+([\d,]+)\s+(\d+)\s+(\d+|N/A)\s+(\d+|N/A)\s*$', l)
        if m:
            dm[int(m.group(1))] = m.group(4)
    assert sorted(dm) == list(range(241)), len(dm)
    raw.append(('DMIXTABLE', 'D.11 (DmixTable column)', [dm[i] for i in range(241)]))

    # ---- write the raw spec numbers
    with open(out_spec, 'w') as f:
        f.write('# Numbers exactly as printed in ETSI TS 102 114 V1.6.1 (2019-08), Annex D / clause 5.\n')
        f.write('# Written by tools/gen_dts_tables.py; decimal commas written as points.\n')
        f.write('# table NAME | clause | values...   (invalid entries kept as the word "invalid")\n')
        f.write('# huff TABLE | clause | level:length:code ...   (code = value of the code word)\n')
        for name, cl, vals in raw:
            f.write('table %s | %s | %s\n' % (name, cl, ' '.join(vals)))
        for tname, e in huff_raw:
            tn, _, note = tname.partition(' ')
            f.write('huff %s | D.5%s | %s\n' % (tn, ' ' + note if note else '',
                                              ' '.join('%d:%d:%d' % x for x in e)))

    # ---- convert and write the Rust file
    out = []

    def emit(rname, ty, vals, doc, per=12):
        out.append('/// %s' % doc)
        out.append('pub static %s: [%s; %d] = [' % (rname, ty, len(vals)))
        for k in range(0, len(vals), per):
            out.append('    ' + ', '.join(str(v) for v in vals[k:k + per]) + ',')
        out.append('];')
        out.append('')

    def ints(vals, n):
        v = [0 if x == 'invalid' else int(x.replace(' ', '')) for x in vals]
        return v + [0] * (n - len(v))
    R = {n: v for n, _, v in raw}
    out.append('//! Constant tables for DTS Coherent Acoustics (ETSI TS 102 114).')
    out.append('//!')
    out.append('//! GENERATED by tools/gen_dts_tables.py from the numeric tables of')
    out.append('//! ETSI TS 102 114 V1.6.1 (2019-08); the clause of each table is named on')
    out.append('//! it. The raw printed numbers are committed in')
    out.append('//! tests/spec_ts102114_tables.txt and tests/spec_tables.rs checks every')
    out.append('//! table here against them. Do not edit by hand.')
    out.append('//!')
    out.append('//! The tables the standard does not print numerically (ADPCM and')
    out.append('//! high-frequency VQ codebooks, which it says are too large to include) or')
    out.append('//! prints at a precision that does not reproduce the fixed-point values')
    out.append('//! used here live in `tables_residual.rs` and are re-exported below.')
    out.append('')
    out.append('pub use crate::tables_residual::*;')
    out.append('')
    emit('SCALE_FACTOR_QUANT6', 'u32', ints(R['SCALE_FACTOR_QUANT6'], 64),
         '6-bit scale factor quantization levels, ETSI TS 102 114 D.1.1 (63 invalid = 0).')
    emit('SCALE_FACTOR_QUANT7', 'u32', ints(R['SCALE_FACTOR_QUANT7'], 128),
         '7-bit scale factor quantization levels, ETSI TS 102 114 D.1.2 (125..127 invalid = 0).')
    emit('SCALE_FACTOR_ADJ', 'u32', [int(Decimal(a) * 2 ** 22) for a in adj],
         'Scale factor adjustment values x 2^22, ETSI TS 102 114 Table 5-27.')
    emit('QUANT_LEVELS', 'u32', [1] + [int(x) for x in R['QUANT_LEVELS'][1:]] + [0] * 5,
         'Quantization levels by ABITS, ETSI TS 102 114 Table 5-26 (ABITS 0 = 1, 8..10 the power of two).')
    emit('LOSSY_QUANT', 'u32', ints(R['LOSSY_QUANT'], 32),
         'Lossy quantizer step size x 2^22, ETSI TS 102 114 D.2.1.')
    emit('LOSSLESS_QUANT', 'u32', ints(R['LOSSLESS_QUANT'], 32),
         'Lossless quantizer step size x 2^22, ETSI TS 102 114 D.2.2.')
    f64 = [int((Decimal(x) * 2 ** 29).to_integral_value(ROUND_HALF_UP)) * (1 if (i // 32) % 4 == 0 else -1)
           for i, x in enumerate(R['FIR_64'])]
    emit('FIR_64', 'i32', f64,
         '64-band QMF window for X96, x 2^29 with the cosine modulation sign, ETSI TS 102 114 D.9.', 8)
    perm = [[q, 128 + q, 255 - q, 127 - q][p] for q in range(64) for p in range(4)]
    out.append('/// LFE interpolation FIR for the 128x decimation, ETSI TS 102 114 D.8 (128 x column),')
    out.append('/// in polyphase order.')
    out.append('pub static LFE_FIR_128: [f32; 256] = [')
    for k in range(0, 256, 4):
        out.append('    ' + ', '.join(lfe[perm[j]] for j in range(k, k + 4)) + ',')
    out.append('];')
    out.append('')
    emit('DMIXTABLE', 'u16', [0] + [int(dm[i]) for i in range(241)],
         'Downmix scale factors, ETSI TS 102 114 D.11 (DmixTable, preceded by a 0 entry).')
    out.append('/// Huffman code (symbol, length) pairs in canonical code order, from ETSI TS 102 114')
    out.append('/// D.5: quantizer index books (10 books x group), bit allocation (5 x 12),')
    out.append('/// scale factors (5 x 129), transition mode (4 x 4).')
    out.append('pub static HUFF_SRC: [(u8, u8); %d] = [' % len(huff))
    for k in range(0, len(huff), 8):
        out.append('    ' + ', '.join('(%d, %d)' % p for p in huff[k:k + 8]) + ',')
    out.append('];')
    with open(out_rs, 'w') as f:
        f.write('\n'.join(out) + '\n')


main()
