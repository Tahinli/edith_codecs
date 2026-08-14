#!/usr/bin/env python3
"""Emit crates/ec-aac/src/tables.rs from the probed tables."""

import json
import sys

SPEC = {  # cb: (dim, lav, unsigned, esc)
    1: (4, 1, False, False),
    2: (4, 1, False, False),
    3: (4, 2, True, False),
    4: (4, 2, True, False),
    5: (2, 4, False, False),
    6: (2, 4, False, False),
    7: (2, 7, True, False),
    8: (2, 7, True, False),
    9: (2, 12, True, False),
    10: (2, 12, True, False),
    11: (2, 16, True, True),
}


def index_of(t, dim, lav, unsigned):
    span = lav + 1 if unsigned else 2 * lav + 1
    off = 0 if unsigned else lav
    idx = 0
    for v in t:
        idx = idx * span + (v + off)
    return idx


def emit_codebook(cb):
    dim, lav, unsigned, esc = SPEC[cb]
    d = json.load(open(f"cb{cb}.json"))
    span = lav + 1 if unsigned else 2 * lav + 1
    n = span ** dim
    table = [None] * n
    for bits, tup in d.items():
        i = index_of(tuple(tup), dim, lav, unsigned)
        assert table[i] is None, (cb, tup)
        table[i] = (len(bits), int(bits, 2))
    missing = [i for i, v in enumerate(table) if v is None]
    assert not missing, (cb, missing[:5])
    body = ", ".join(f"({l},{c})" for l, c in table)
    return f"/// Codebook {cb}: dim {dim}, lav {lav}, " \
           f"{'unsigned' if unsigned else 'signed'}{', escape' if esc else ''}.\n" \
           f"static HCB{cb}: [(u8, u32); {n}] = [{body}];\n"


def main():
    out = []
    out.append('''//! Normative AAC tables (ISO/IEC 14496-3 §4.6.2, §4.6.9, tbl 4.140).
//!
//! Every number here was derived by black-box probing of a reference decoder,
//! not copied from an implementation: `scripts/derive-aac-tables.py` synthesises
//! access units whose spectral payload is a chosen bit string, decodes them, and
//! reads the quantised spectrum back through the inverse MDCT.  Each codebook
//! came out with a Kraft sum of exactly 1 and the entry count the standard
//! prescribes, which is what makes the derivation self-checking.
''')
    for cb in range(1, 12):
        out.append(emit_codebook(cb))

    sf = json.load(open("sfb.json"))
    tab = [None] * 121
    for bits, v in sf.items():
        tab[v] = (len(bits), int(bits, 2))
    assert all(t is not None for t in tab)
    out.append("/// Scalefactor codebook: index is the coded value, delta = index - 60.\n"
               "static HCB_SF: [(u8, u32); 121] = ["
               + ", ".join(f"({l},{c})" for l, c in tab) + "];\n")

    lng = json.load(open("swb.json"))
    sht = json.load(open("swb_short.json"))
    for name, src, cap in (("LONG", lng, "long"), ("SHORT", sht, None)):
        rows = []
        for i in range(12):
            e = src[str(i)]["long"] if cap else src[str(i)]
            rows.append("&[0, " + ", ".join(str(v) for v in e) + "]")
        out.append(f"/// Scalefactor-band offsets for {'1024' if cap else '128'}-line "
                   f"windows, by samplingFrequencyIndex.\n"
                   f"pub static SWB_{name}: [&[u16]; 12] = [\n    "
                   + ",\n    ".join(rows) + ",\n];\n")

    out.append('''
/// One spectral codebook's shape.
#[derive(Clone, Copy, Debug)]
pub struct Codebook {
    /// Coefficients per Huffman codeword (2 or 4).
    pub dim: u8,
    /// Largest absolute value the codebook can carry.
    pub lav: u8,
    /// True when magnitudes are coded and each non-zero one takes a sign bit.
    pub unsigned: bool,
    /// True for codebook 11, whose magnitude 16 opens an escape sequence.
    pub esc: bool,
    /// `(length, code)` by tuple index, most-significant bit first.
    pub codes: &'static [(u8, u32)],
}

/// The eleven spectral codebooks, indexed by `sect_cb - 1`.
pub static CODEBOOKS: [Codebook; 11] = [
''')
    for cb in range(1, 12):
        dim, lav, unsigned, esc = SPEC[cb]
        out.append(f"    Codebook {{ dim: {dim}, lav: {lav}, unsigned: {str(unsigned).lower()}, "
                   f"esc: {str(esc).lower()}, codes: &HCB{cb} }},\n")
    out.append("];\n\n")
    out.append('''/// The scalefactor codebook, as a `Codebook`-shaped table of `(length, code)`.
pub static SCALEFACTOR_CODES: &[(u8, u32)] = &HCB_SF;

/// Sampling frequencies by `samplingFrequencyIndex` (ISO 14496-3 tbl 1.18).
pub static SAMPLE_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];
''')
    src = "".join(out)
    open(sys.argv[1], "w").write(src)
    print(f"wrote {sys.argv[1]}, {len(src)} bytes")


main()
